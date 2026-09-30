//! Jupiter v6 exact-input swaps between the pool's CASH and USDC accounts.
//!
//! The pool never prices a swap itself: the caller builds the route off-chain (Jupiter's API) and passes its data and
//! accounts, and the pool checks the result against par. The route's accounts reach the CPI with the pool PDA as a
//! signer, so before calling Jupiter the pool pins who signs, what is spent and where the output lands, and refuses
//! any other token account the pool owns. The checks follow Vanna's own Jupiter adapter (`adapters/jupiter.rs`).
//!
//! Allowed instructions (Jupiter's account order; the route's AMM accounts follow these):
//!
//! ```text
//! route                                   shared_accounts_route
//!   0 token_program                         0 token_program
//!   1 user_transfer_authority (pool)        1 program_authority
//!   2 user_source_token_account             2 user_transfer_authority (pool)
//!   3 user_destination_token_account        3 source_token_account
//!   4 destination_token_account  (unset)    4 program_source_token_account
//!   5 destination_mint                      5 program_destination_token_account
//!   6 platform_fee_account       (unset)    6 destination_token_account
//!   7 event_authority                       7 source_mint
//!   8 program                               8 destination_mint
//!                                           9 platform_fee_account       (unset)
//!                                          10 token_2022_program
//!                                          11 event_authority
//!                                          12 program
//! ```
//!
//! Jupiter encodes an omitted optional account as its own program id.

use crate::constants::{JUPITER_PROGRAM_ID, PREDICT_PROGRAM_ID, VANNA_PROGRAM_ID};
use crate::errors::PoolError;
use crate::token;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;

/// `sha256("global:route")[..8]`
pub const ROUTE: [u8; 8] = [229, 23, 203, 151, 122, 227, 173, 42];
/// `sha256("global:shared_accounts_route")[..8]`
pub const SHARED_ACCOUNTS_ROUTE: [u8; 8] = [193, 32, 155, 51, 65, 214, 156, 129];

/// Both instructions end with `in_amount: u64, quoted_out_amount: u64, slippage_bps: u16, platform_fee_bps: u8`.
const TAIL_LEN: usize = 19;
/// Selector plus the route plan's `Vec` length prefix.
const MIN_HEAD_LEN: usize = 12;
const ROUTE_ACCOUNT_COUNT: usize = 9;
const SHARED_ROUTE_ACCOUNT_COUNT: usize = 13;

/// Where the pool's roles sit in the route's accounts.
struct Plan {
    authority: usize,
    source: usize,
    destination: usize,
    source_mint: Option<usize>,
    destination_mint: usize,
    in_amount: u64,
}

fn plan(data: &[u8], keys: &[Pubkey]) -> Result<Plan> {
    require!(data.len() >= MIN_HEAD_LEN + TAIL_LEN, PoolError::SwapNotAllowed);
    let tail = &data[data.len() - TAIL_LEN..];
    let in_amount = u64::from_le_bytes(tail[..8].try_into().unwrap());
    require!(in_amount > 0, PoolError::ZeroAmount);
    require!(tail[TAIL_LEN - 1] == 0, PoolError::SwapNotAllowed); // platform_fee_bps

    let is_unset = |i: usize| keys.get(i) == Some(&JUPITER_PROGRAM_ID);
    match <[u8; 8]>::try_from(&data[..8]).unwrap() {
        ROUTE => {
            require!(keys.len() >= ROUTE_ACCOUNT_COUNT, PoolError::InvalidSwapAccounts);
            // The output may only land in the pool's destination, and no fee account may take a cut.
            require!(is_unset(4) || keys[4] == keys[3], PoolError::InvalidSwapAccounts);
            require!(is_unset(6), PoolError::InvalidSwapAccounts);
            Ok(Plan { authority: 1, source: 2, destination: 3, source_mint: None, destination_mint: 5, in_amount })
        }
        SHARED_ACCOUNTS_ROUTE => {
            require!(keys.len() >= SHARED_ROUTE_ACCOUNT_COUNT, PoolError::InvalidSwapAccounts);
            require!(is_unset(9), PoolError::InvalidSwapAccounts);
            Ok(Plan { authority: 2, source: 3, destination: 6, source_mint: Some(7), destination_mint: 8, in_amount })
        }
        _ => err!(PoolError::SwapNotAllowed),
    }
}

/// Whether `account` is a classic SPL or Token-2022 token account owned by `owner`.
fn is_token_account_of(account: &AccountInfo, owner: &Pubkey) -> bool {
    if *account.owner != anchor_spl::token::ID && *account.owner != anchor_spl::token_2022::ID {
        return false;
    }
    let Ok(data) = account.try_borrow_data() else {
        return false;
    };
    // 165 bytes is the base layout; an extended Token-2022 account marks byte 165 as 2 (account).
    let is_account = data.len() == 165 || (data.len() > 165 && data[165] == 2);
    is_account && data[32..64] == owner.to_bytes()
}

/// A token account the swap touched must still be a plain pool account: pool-owned, no delegate, no close authority.
fn still_plain_pool_account(account: &AccountInfo, pool: &Pubkey) -> Result<()> {
    let data = account.try_borrow_data()?;
    require!(data.len() >= 165, PoolError::InvalidSwapResult);
    require!(data[32..64] == pool.to_bytes(), PoolError::InvalidSwapResult);
    require!(data[72..76] == [0; 4] && data[129..133] == [0; 4], PoolError::InvalidSwapResult);
    Ok(())
}

pub struct SwapResult {
    /// The route's declared input, which is also exactly what left the source.
    pub amount_in: u64,
    /// What arrived in the destination.
    pub amount_out: u64,
}

/// One side of the swap: a pool token account and its mint.
pub struct Leg<'a, 'info> {
    pub account: &'a AccountInfo<'info>,
    pub mint: Pubkey,
}

/// Runs the caller's Jupiter route, spending from `source` into `destination`, signed by the pool.
#[allow(clippy::too_many_arguments)]
pub fn swap<'info>(
    jupiter_program: &AccountInfo<'info>,
    route_accounts: &[AccountInfo<'info>],
    data: Vec<u8>,
    pool: &AccountInfo<'info>,
    signer: &[&[&[u8]]],
    source: Leg<'_, 'info>,
    destination: Leg<'_, 'info>,
) -> Result<SwapResult> {
    require_keys_eq!(jupiter_program.key(), JUPITER_PROGRAM_ID, PoolError::SwapNotAllowed);
    let keys: Vec<Pubkey> = route_accounts.iter().map(|a| a.key()).collect();
    let plan = plan(&data, &keys)?;

    // Roles: the pool signs, spends from `source` and receives into `destination`, in the declared mints.
    require_keys_eq!(keys[plan.authority], pool.key(), PoolError::InvalidSwapAccounts);
    require_keys_eq!(keys[plan.source], source.account.key(), PoolError::InvalidSwapAccounts);
    require_keys_eq!(keys[plan.destination], destination.account.key(), PoolError::InvalidSwapAccounts);
    if let Some(i) = plan.source_mint {
        require_keys_eq!(keys[i], source.mint, PoolError::InvalidSwapAccounts);
    }
    require_keys_eq!(keys[plan.destination_mint], destination.mint, PoolError::InvalidSwapAccounts);

    // The pool PDA signs this CPI, so nothing else it controls may be reachable: no program that would honour its
    // signature (this one, Vanna, prediCt), no account this program owns but the pool itself, and no pool token
    // account other than the two legs. In particular its YES, NO and Vanna share accounts stay out.
    for account in route_accounts {
        let key = account.key();
        require!(
            key != crate::ID && key != VANNA_PROGRAM_ID && key != PREDICT_PROGRAM_ID,
            PoolError::InvalidSwapAccounts
        );
        if *account.owner == crate::ID {
            require_keys_eq!(key, pool.key(), PoolError::InvalidSwapAccounts);
        }
        if key != source.account.key() && key != destination.account.key() {
            require!(!is_token_account_of(account, &pool.key()), PoolError::InvalidSwapAccounts);
        }
    }

    let source_before = token::amount(source.account)?;
    let destination_before = token::amount(destination.account)?;

    // Writability passes through as the route asks: some venues (Manifest) take the user as a writable payer, and a
    // read-only pool there fails the whole swap with PrivilegeEscalation. A writable pool gives the route nothing: only
    // this program can change its data or debit its lamports, and this program is kept out of the route above.
    let metas = route_accounts
        .iter()
        .map(|a| AccountMeta { pubkey: a.key(), is_signer: a.key() == pool.key(), is_writable: a.is_writable })
        .collect();
    let mut infos = route_accounts.to_vec();
    infos.push(jupiter_program.clone());
    invoke_signed(&Instruction { program_id: JUPITER_PROGRAM_ID, accounts: metas, data }, &infos, signer)?;

    still_plain_pool_account(source.account, &pool.key())?;
    still_plain_pool_account(destination.account, &pool.key())?;
    let spent = source_before
        .checked_sub(token::amount(source.account)?)
        .ok_or_else(|| error!(PoolError::InvalidSwapResult))?;
    require!(spent == plan.in_amount, PoolError::InvalidSwapResult);
    let amount_out = token::amount(destination.account)?
        .checked_sub(destination_before)
        .ok_or_else(|| error!(PoolError::InvalidSwapResult))?;
    Ok(SwapResult { amount_in: plan.in_amount, amount_out })
}

/// Parses only the route's declared input, so a caller can size what it hands the swap before running it.
pub fn declared_in_amount(data: &[u8]) -> Result<u64> {
    require!(data.len() >= MIN_HEAD_LEN + TAIL_LEN, PoolError::SwapNotAllowed);
    let tail = &data[data.len() - TAIL_LEN..];
    Ok(u64::from_le_bytes(tail[..8].try_into().unwrap()))
}
