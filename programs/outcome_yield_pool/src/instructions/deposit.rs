use super::shared::*;
use crate::errors::PoolError;
use crate::events::Deposit;
use crate::math::{checked_add, to_shares_down};
use crate::predict;
use crate::state::Pool;
use crate::token;
use anchor_lang::prelude::*;
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_interface::{Mint, TokenAccount};

#[derive(Accounts)]
#[instruction(is_yes: bool)]
pub struct DepositOutcome<'info> {
    pub depositor: Signer<'info>,
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: key checked against `pool.market`, contents parsed by `predict::read_outcome`.
    #[account(address = pool.market @ PoolError::NotPredictMarket)]
    pub market: UncheckedAccount<'info>,
    #[account(address = pool.outcome_mint(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub outcome_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        mut,
        token::mint = outcome_mint,
        token::authority = depositor,
        token::token_program = token_program
    )]
    pub depositor_outcome: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: the pool's own account for this side, checked by key.
    #[account(mut, address = pool.pool_outcome_account(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub pool_outcome: UncheckedAccount<'info>,
    #[account(mut, address = pool.share_mint(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,
    /// Receives the shares; its owner is the depositor's chosen recipient.
    #[account(mut, token::mint = share_mint, token::token_program = token_program)]
    pub recipient_shares: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vanna: VannaAccounts<'info>,
    pub token_program: Program<'info, Token2022>,
}

/// Credits `amount` outcome tokens of one side and mints that side's shares. Does not merge: Solana cannot catch a
/// failed CPI, so a merge here would let a refusing swap or reserve block deposits. Append `merge_and_deposit` to the
/// same transaction to rebalance, or leave it out to defer the merge.
pub fn deposit(ctx: Context<DepositOutcome>, is_yes: bool, amount: u64) -> Result<()> {
    let a = &ctx.accounts;
    let pool = &a.pool;
    require!(pool.is_open(), PoolError::PoolSettled);
    // Open-market pricing only holds while split/merge do. Once World has determined the outcome it can pay the pool
    // CASH for its winning tokens, which this pricing does not count, so a deposit here would claim part of it.
    require!(predict::read_outcome(&a.market)?.is_none(), PoolError::MarketResolved);
    require!(amount > 0, PoolError::ZeroAmount);
    a.vanna.validate(pool)?;

    // Price against the side as it stood before these tokens arrived.
    let dangling_before = token::amount(&a.pool_outcome)?;
    let total_assets_before = checked_add(dangling_before, a.vanna.invested(&pool.to_account_info())?)?;
    let total_shares = a.share_mint.supply;

    let token_program = a.token_program.to_account_info();
    token::transfer_checked(
        &token_program,
        &a.depositor_outcome.to_account_info(),
        &a.outcome_mint.to_account_info(),
        &a.pool_outcome,
        &a.depositor,
        amount,
        &[],
    )?;
    // Credit what actually arrived, not what was requested.
    let received = token::amount(&a.pool_outcome)?.saturating_sub(dangling_before);

    let shares = to_shares_down(received, total_assets_before, total_shares)?;
    require!(shares > 0, PoolError::ZeroShares);

    let bump = [pool.bump];
    let seeds = pool_seeds(pool, &bump);
    token::mint_to(
        &token_program,
        &a.share_mint.to_account_info(),
        &a.recipient_shares.to_account_info(),
        &pool.to_account_info(),
        shares,
        &[&seeds],
    )?;

    emit!(Deposit {
        pool: pool.key(),
        is_yes,
        caller: a.depositor.key(),
        to: a.recipient_shares.owner,
        assets: received,
        shares,
    });
    Ok(())
}
