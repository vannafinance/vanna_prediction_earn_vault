//! The slice of Vanna's lending program this pool depends on: its USDC reserve, every pool's yield vault.
//!
//! Vanna runs one lending reserve per mint. Lenders supply the mint for reserve shares (a classic SPL mint) and redeem
//! shares for it; a share's value grows as borrowers pay interest. The account layout and the three instructions
//! below mirror `Protocol_V1_Solana/programs/vanna_lending`, and the test suite runs that program's bytecode
//! (`fixtures/vanna`), so any drift fails the tests.

use crate::constants::VANNA_PROGRAM_ID;
use crate::errors::PoolError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, invoke_signed};

pub const PROTOCOL_SEED: &[u8] = b"protocol";
pub const RESERVE_SEED: &[u8] = b"reserve";

/// `sha256("account:Reserve")[:8]`.
pub const RESERVE_DISCRIMINATOR: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];
/// `sha256("global:<name>")[:8]`.
pub const LENDER_SUPPLY_DISCRIMINATOR: [u8; 8] = [52, 247, 77, 250, 74, 122, 100, 33];
pub const LENDER_REDEEM_DISCRIMINATOR: [u8; 8] = [22, 48, 40, 70, 164, 48, 75, 224];
pub const REFRESH_RESERVE_DISCRIMINATOR: [u8; 8] = [212, 53, 162, 184, 171, 222, 142, 156];

/// `Reserve`: discriminator, four pubkeys, then its u64/u128 accounting fields, Borsh-packed.
const RESERVE_LEN: usize = 372;
const ASSET_CONFIG_OFFSET: usize = 8;
const UNDERLYING_MINT_OFFSET: usize = 40;
const LIQUIDITY_VAULT_OFFSET: usize = 72;
const SHARE_MINT_OFFSET: usize = 104;
const ACCOUNTED_LIQUIDITY_OFFSET: usize = 152;
const TOTAL_BORROW_OFFSET: usize = 160;
const ACCRUED_FEES_OFFSET: usize = 200;

pub struct ReserveView {
    pub asset_config: Pubkey,
    pub underlying_mint: Pubkey,
    pub liquidity_vault: Pubkey,
    pub share_mint: Pubkey,
    /// Assets owed to lenders: `liquidity + borrows - protocol fees`, as Vanna's `lender_total_assets`.
    pub lender_total_assets: u64,
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

pub fn read_reserve(reserve: &AccountInfo) -> Result<ReserveView> {
    require_keys_eq!(*reserve.owner, VANNA_PROGRAM_ID, PoolError::InvalidVannaReserve);
    let data = reserve.try_borrow_data()?;
    require!(data.len() >= RESERVE_LEN && data[..8] == RESERVE_DISCRIMINATOR, PoolError::InvalidVannaReserve);
    let lender_total_assets = (read_u64(&data, ACCOUNTED_LIQUIDITY_OFFSET) as u128)
        .checked_add(read_u64(&data, TOTAL_BORROW_OFFSET) as u128)
        .and_then(|v| v.checked_sub(read_u64(&data, ACCRUED_FEES_OFFSET) as u128))
        .and_then(|v| u64::try_from(v).ok())
        .ok_or_else(|| error!(PoolError::MathOverflow))?;
    Ok(ReserveView {
        asset_config: read_pubkey(&data, ASSET_CONFIG_OFFSET),
        underlying_mint: read_pubkey(&data, UNDERLYING_MINT_OFFSET),
        liquidity_vault: read_pubkey(&data, LIQUIDITY_VAULT_OFFSET),
        share_mint: read_pubkey(&data, SHARE_MINT_OFFSET),
        lender_total_assets,
    })
}

fn mul_div(a: u64, b: u64, c: u64, round_up: bool) -> Result<u64> {
    require!(c != 0, PoolError::MathOverflow);
    let product = (a as u128) * (b as u128);
    let q = product / c as u128 + u128::from(round_up && !product.is_multiple_of(c as u128));
    u64::try_from(q).map_err(|_| error!(PoolError::MathOverflow))
}

/// What redeeming `shares` pays now: Vanna's `supply_shares_to_assets_down`.
pub fn shares_to_assets_down(shares: u64, share_supply: u64, lender_total_assets: u64) -> Result<u64> {
    if share_supply == 0 {
        return Ok(0);
    }
    mul_div(shares, lender_total_assets, share_supply, false)
}

/// The fewest shares whose redemption pays at least `assets`. Never more than a position worth `assets` holds,
/// since `floor(held * total / supply) >= assets` implies `ceil(assets * supply / total) <= held`.
pub fn assets_to_shares_up(assets: u64, share_supply: u64, lender_total_assets: u64) -> Result<u64> {
    mul_div(assets, share_supply, lender_total_assets, true)
}

/// The accounts every Vanna lender instruction takes, with the pool PDA as the lender.
pub struct Vanna<'a, 'info> {
    pub program: &'a AccountInfo<'info>,
    pub protocol_config: &'a AccountInfo<'info>,
    pub asset_config: &'a AccountInfo<'info>,
    pub reserve: &'a AccountInfo<'info>,
    pub usdc_mint: &'a AccountInfo<'info>,
    pub liquidity_vault: &'a AccountInfo<'info>,
    pub share_mint: &'a AccountInfo<'info>,
    pub lender: &'a AccountInfo<'info>,
    pub lender_usdc: &'a AccountInfo<'info>,
    pub lender_shares: &'a AccountInfo<'info>,
    pub token_program: &'a AccountInfo<'info>,
    pub associated_token_program: &'a AccountInfo<'info>,
    pub system_program: &'a AccountInfo<'info>,
}

impl<'a, 'info> Vanna<'a, 'info> {
    /// Accrues interest to now, so the reserve read afterwards is what a redemption would be priced at.
    pub fn refresh(&self) -> Result<()> {
        let ix = Instruction {
            program_id: VANNA_PROGRAM_ID,
            accounts: vec![AccountMeta::new(self.reserve.key(), false)],
            data: REFRESH_RESERVE_DISCRIMINATOR.to_vec(),
        };
        invoke(&ix, &[self.reserve.clone(), self.program.clone()]).map_err(Into::into)
    }

    fn lender_ix(&self, discriminator: [u8; 8], amount: u64, min_out: u64, supply: bool) -> Instruction {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&discriminator);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&min_out.to_le_bytes());
        let mut accounts = vec![
            // `lender_supply` pays for the share account with `init_if_needed`, so it declares the lender `mut`. The
            // pool's share account exists from `create_pool`, so nothing is ever charged.
            AccountMeta { pubkey: self.lender.key(), is_signer: true, is_writable: supply },
            AccountMeta::new_readonly(self.protocol_config.key(), false),
            AccountMeta::new_readonly(self.asset_config.key(), false),
            AccountMeta::new(self.reserve.key(), false),
            AccountMeta::new_readonly(self.usdc_mint.key(), false),
            AccountMeta::new(self.lender_usdc.key(), false),
            AccountMeta::new(self.liquidity_vault.key(), false),
            AccountMeta::new(self.share_mint.key(), false),
            AccountMeta::new(self.lender_shares.key(), false),
            AccountMeta::new_readonly(self.token_program.key(), false),
            AccountMeta::new_readonly(self.token_program.key(), false),
        ];
        if supply {
            accounts.push(AccountMeta::new_readonly(self.associated_token_program.key(), false));
            accounts.push(AccountMeta::new_readonly(self.system_program.key(), false));
        }
        Instruction { program_id: VANNA_PROGRAM_ID, accounts, data }
    }

    fn infos(&self) -> Vec<AccountInfo<'info>> {
        vec![
            self.lender.clone(),
            self.protocol_config.clone(),
            self.asset_config.clone(),
            self.reserve.clone(),
            self.usdc_mint.clone(),
            self.lender_usdc.clone(),
            self.liquidity_vault.clone(),
            self.share_mint.clone(),
            self.lender_shares.clone(),
            self.token_program.clone(),
            self.associated_token_program.clone(),
            self.system_program.clone(),
            self.program.clone(),
        ]
    }

    /// Supplies `assets` USDC from the pool for reserve shares.
    pub fn supply(&self, assets: u64, signer: &[&[&[u8]]]) -> Result<()> {
        let ix = self.lender_ix(LENDER_SUPPLY_DISCRIMINATOR, assets, 1, true);
        invoke_signed(&ix, &self.infos(), signer).map_err(Into::into)
    }

    /// Redeems `shares` for at least `min_assets_out` USDC into the pool's USDC account.
    pub fn redeem(&self, shares: u64, min_assets_out: u64, signer: &[&[&[u8]]]) -> Result<()> {
        let ix = self.lender_ix(LENDER_REDEEM_DISCRIMINATOR, shares, min_assets_out, false);
        invoke_signed(&ix, &self.infos(), signer).map_err(Into::into)
    }
}
