//! The slice of World's `prediCt` program this pool depends on.
//!
//! World publishes no IDL for prediCt. The layout and rosters below were decoded from live mainnet transactions
//! (chainstacklabs/world-xyz-research `docs/reference.md`) and then checked against the deployed program itself: the
//! test suite runs World's real prediCt bytecode over snapshotted mainnet markets (`fixtures/world`).

use crate::constants::PREDICT_PROGRAM_ID;
use crate::errors::PoolError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

/// `sha256("account:Market")[:8]`, verified against live accounts.
pub const MARKET_DISCRIMINATOR: [u8; 8] = [0xdb, 0xbe, 0xd5, 0x37, 0x00, 0xe3, 0xc6, 0x9a];
pub const MARKET_LEN: usize = 320;

pub const MARKET_CREATOR_OFFSET: usize = 8;
pub const MARKET_COLLATERAL_OFFSET: usize = 40;
pub const MARKET_YES_MINT_OFFSET: usize = 72;
pub const MARKET_NO_MINT_OFFSET: usize = 104;
pub const MARKET_CASH_VAULT_OFFSET: usize = 136;
/// u64 slot at which `determine_outcome` ran; zero while the market is open.
pub const MARKET_DETERMINED_SLOT_OFFSET: usize = 176;
/// u16 outcome written by `determine_outcome`.
pub const MARKET_OUTCOME_OFFSET: usize = 184;

/// `determine_outcome` writes 10000 when YES wins and 0 when NO wins.
pub const OUTCOME_YES: u16 = 10_000;
pub const OUTCOME_NO: u16 = 0;

pub const SPLIT_DISCRIMINATOR: [u8; 8] = [124, 189, 27, 43, 216, 40, 147, 66];
pub const MERGE_DISCRIMINATOR: [u8; 8] = [148, 141, 236, 47, 174, 126, 69, 111];

pub struct MarketView {
    pub creator: Pubkey,
    pub collateral_mint: Pubkey,
    pub yes_mint: Pubkey,
    pub no_mint: Pubkey,
    pub cash_vault: Pubkey,
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().unwrap())
}

fn load_market_data<'a>(market: &'a AccountInfo) -> Result<std::cell::Ref<'a, &'a mut [u8]>> {
    require_keys_eq!(*market.owner, PREDICT_PROGRAM_ID, PoolError::NotPredictMarket);
    let data = market.try_borrow_data()?;
    require!(data.len() >= MARKET_LEN, PoolError::NotPredictMarket);
    require!(data[..8] == MARKET_DISCRIMINATOR, PoolError::NotPredictMarket);
    Ok(data)
}

pub fn read_market(market: &AccountInfo) -> Result<MarketView> {
    let data = load_market_data(market)?;
    Ok(MarketView {
        creator: read_pubkey(&data, MARKET_CREATOR_OFFSET),
        collateral_mint: read_pubkey(&data, MARKET_COLLATERAL_OFFSET),
        yes_mint: read_pubkey(&data, MARKET_YES_MINT_OFFSET),
        no_mint: read_pubkey(&data, MARKET_NO_MINT_OFFSET),
        cash_vault: read_pubkey(&data, MARKET_CASH_VAULT_OFFSET),
    })
}

/// The written outcome, or `None` while the market is unresolved.
pub fn read_outcome(market: &AccountInfo) -> Result<Option<u16>> {
    let data = load_market_data(market)?;
    let slot = &data[MARKET_DETERMINED_SLOT_OFFSET..MARKET_DETERMINED_SLOT_OFFSET + 8];
    if u64::from_le_bytes(slot.try_into().unwrap()) == 0 {
        return Ok(None);
    }
    let outcome = u16::from_le_bytes([data[MARKET_OUTCOME_OFFSET], data[MARKET_OUTCOME_OFFSET + 1]]);
    Ok(Some(outcome))
}

/// True once World's `close_market` has torn the account down.
pub fn is_closed(market: &AccountInfo) -> bool {
    market.lamports() == 0 || *market.owner != PREDICT_PROGRAM_ID || market.data_is_empty()
}

/// The 11-account roster shared by `split` and `merge`, with the pool PDA as the writable signer at index 0.
pub struct SetAccounts<'a, 'info> {
    pub authority: &'a AccountInfo<'info>,
    pub market: &'a AccountInfo<'info>,
    pub collateral_mint: &'a AccountInfo<'info>,
    pub yes_mint: &'a AccountInfo<'info>,
    pub no_mint: &'a AccountInfo<'info>,
    pub authority_cash: &'a AccountInfo<'info>,
    pub market_cash_vault: &'a AccountInfo<'info>,
    pub authority_yes: &'a AccountInfo<'info>,
    pub authority_no: &'a AccountInfo<'info>,
    pub token_program: &'a AccountInfo<'info>,
    pub predict_program: &'a AccountInfo<'info>,
}

impl<'a, 'info> SetAccounts<'a, 'info> {
    fn invoke(&self, discriminator: [u8; 8], amount: u64, signer_seeds: &[&[&[u8]]]) -> Result<()> {
        require_keys_eq!(self.predict_program.key(), PREDICT_PROGRAM_ID, PoolError::NotPredictMarket);

        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&discriminator);
        data.extend_from_slice(&amount.to_le_bytes());

        let ix = Instruction {
            program_id: PREDICT_PROGRAM_ID,
            accounts: vec![
                // prediCt declares its `user` signer `mut`.
                AccountMeta::new(self.authority.key(), true),
                AccountMeta::new(self.market.key(), false),
                AccountMeta::new_readonly(self.collateral_mint.key(), false),
                AccountMeta::new(self.yes_mint.key(), false),
                AccountMeta::new(self.no_mint.key(), false),
                AccountMeta::new(self.authority_cash.key(), false),
                AccountMeta::new(self.market_cash_vault.key(), false),
                AccountMeta::new(self.authority_yes.key(), false),
                AccountMeta::new(self.authority_no.key(), false),
                AccountMeta::new_readonly(self.token_program.key(), false),
                AccountMeta::new_readonly(self.token_program.key(), false),
            ],
            data,
        };

        invoke_signed(
            &ix,
            &[
                self.authority.clone(),
                self.market.clone(),
                self.collateral_mint.clone(),
                self.yes_mint.clone(),
                self.no_mint.clone(),
                self.authority_cash.clone(),
                self.market_cash_vault.clone(),
                self.authority_yes.clone(),
                self.authority_no.clone(),
                self.token_program.clone(),
                self.predict_program.clone(),
            ],
            signer_seeds,
        )
        .map_err(Into::into)
    }

    /// Locks `amount` CASH and mints `amount` YES + `amount` NO to the authority.
    pub fn split(&self, amount: u64, signer_seeds: &[&[&[u8]]]) -> Result<()> {
        self.invoke(SPLIT_DISCRIMINATOR, amount, signer_seeds)
    }

    /// Burns `amount` YES + `amount` NO from the authority and releases `amount` CASH to it.
    pub fn merge(&self, amount: u64, signer_seeds: &[&[&[u8]]]) -> Result<()> {
        self.invoke(MERGE_DISCRIMINATOR, amount, signer_seeds)
    }
}
