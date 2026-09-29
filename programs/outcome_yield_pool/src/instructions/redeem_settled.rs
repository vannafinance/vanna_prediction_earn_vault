use super::shared::*;
use crate::errors::PoolError;
use crate::events::RedeemSettled as RedeemSettledEvent;
use crate::math::{checked_add, to_assets_down};
use crate::state::{Pool, PoolStatus};
use crate::token;
use anchor_lang::prelude::*;
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_interface::{Mint, TokenAccount};

#[derive(Accounts)]
#[instruction(is_yes: bool)]
pub struct RedeemSettled<'info> {
    pub authority: Signer<'info>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, address = pool.share_mint(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, token::mint = share_mint, token::token_program = token_program)]
    pub owner_shares: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: key checked. Unchecked because World closes outcome mints after settlement; only touched when the pool
    /// still holds tokens of this side, which means the mint is still live.
    #[account(address = pool.outcome_mint(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub outcome_mint: UncheckedAccount<'info>,
    /// CHECK: the pool's own account for this side, checked by key.
    #[account(mut, address = pool.pool_outcome_account(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub pool_outcome: UncheckedAccount<'info>,
    /// CHECK: only used as a transfer destination, which Token-2022 validates against `outcome_mint`.
    #[account(mut)]
    pub recipient_outcome: UncheckedAccount<'info>,
    /// CHECK: key checked against `pool.collateral_mint`.
    #[account(address = pool.collateral_mint @ PoolError::InvalidCollateral)]
    pub collateral_mint: UncheckedAccount<'info>,
    /// CHECK: the pool's own CASH account, checked by key.
    #[account(mut, address = pool.pool_cash @ PoolError::InvalidCollateral)]
    pub pool_cash: UncheckedAccount<'info>,
    /// CHECK: only used as a transfer destination, which Token-2022 validates against `collateral_mint`.
    #[account(mut)]
    pub recipient_cash: UncheckedAccount<'info>,
    /// CHECK: only used as a transfer destination, which the token program validates against USDC.
    #[account(mut)]
    pub recipient_usdc: UncheckedAccount<'info>,
    pub vanna: VannaAccounts<'info>,
    pub token_program: Program<'info, Token2022>,
}

/// Redeems shares once the pool is settled. No swap is involved: split and merge are closed, so there is nothing to
/// convert USDC back into.
///
/// Winning side: its assets are its own tokens (each worth 1 CASH), the CASH World's operator already paid the pool
/// for tokens it redeemed on the pool's behalf, and the whole invested USDC, since each invested unit would have split
/// into one winning token. Paid out in that order: tokens, then CASH, then USDC.
///
/// Losing side: its assets are its own tokens, which World pays nothing for. Holders get them back pro rata.
pub fn redeem_settled(ctx: Context<RedeemSettled>, is_yes: bool, shares: u64) -> Result<()> {
    let a = &ctx.accounts;
    let pool = &a.pool;
    let PoolStatus::Settled { yes_won } = pool.status else {
        return err!(PoolError::PoolNotSettled);
    };
    require!(shares > 0, PoolError::ZeroAmount);
    a.vanna.validate(pool)?;
    let pool_info = pool.to_account_info();

    let won = is_yes == yes_won;
    let side_tokens = token::amount(&a.pool_outcome)?;
    let (idle_cash, invested) = if won {
        (token::amount(&a.pool_cash)?, a.vanna.invested(&pool_info)?)
    } else {
        (0, 0)
    };
    let total_assets = checked_add(checked_add(side_tokens, idle_cash)?, invested)?;
    let assets = to_assets_down(shares, total_assets, a.share_mint.supply)?;

    let token_program = a.token_program.to_account_info();
    token::burn(
        &token_program,
        &a.owner_shares.to_account_info(),
        &a.share_mint.to_account_info(),
        &a.authority,
        shares,
    )?;

    let bump = [pool.bump];
    let seeds = pool_seeds(pool, &bump);
    let signer = &[&seeds[..]];

    let tokens_out = assets.min(side_tokens);
    if tokens_out > 0 {
        token::transfer_checked(
            &token_program,
            &a.pool_outcome,
            &a.outcome_mint,
            &a.recipient_outcome,
            &pool_info,
            tokens_out,
            signer,
        )?;
    }

    // Only the winning side can be owed more than its tokens: a side's payout never exceeds its total assets.
    let cash_out = (assets - tokens_out).min(idle_cash);
    if cash_out > 0 {
        token::transfer_checked(
            &token_program,
            &a.pool_cash,
            &a.collateral_mint,
            &a.recipient_cash,
            &pool_info,
            cash_out,
            signer,
        )?;
    }

    let usdc_out = assets - tokens_out - cash_out;
    if usdc_out > 0 {
        a.vanna.ensure_idle_usdc(&pool_info, usdc_out, signer)?;
        token::transfer_checked(
            a.vanna.spl_token_program.as_ref(),
            a.vanna.pool_usdc.as_ref(),
            a.vanna.usdc_mint.as_ref(),
            &a.recipient_usdc,
            &pool_info,
            usdc_out,
            signer,
        )?;
    }

    emit!(RedeemSettledEvent {
        pool: pool.key(),
        is_yes,
        caller: a.authority.key(),
        on_behalf: a.owner_shares.owner,
        shares,
        tokens_out,
        cash_out,
        usdc_out,
    });
    Ok(())
}
