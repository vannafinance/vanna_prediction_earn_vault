use super::shared::*;
use crate::constants::{SWAP_DUST_UNITS, USDC_MINT};
use crate::errors::PoolError;
use crate::events::{Redeem, Swapped};
use crate::jupiter::{self, Leg};
use crate::math::{checked_add, to_assets_down};
use crate::state::Pool;
use crate::token;
use anchor_lang::prelude::*;
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_interface::{Mint, TokenAccount};

/// A redemption's route may swap up to this fraction (per million) more than the shortfall, plus `SWAP_SLACK_UNITS`.
/// Callers size the route before the transaction lands and interest keeps accruing, so they need a little headroom;
/// more would let a caller pull the pool's funds out of Vanna for no reason and cost it a swap to put them back.
const SWAP_SLACK_PPM: u64 = 1_000;
const SWAP_SLACK_UNITS: u64 = 1_000;

#[derive(Accounts)]
#[instruction(is_yes: bool)]
pub struct RedeemOutcome<'info> {
    /// The share account's owner, or a delegate approved on it.
    pub authority: Signer<'info>,
    /// Writable because prediCt requires its `split` signer to be.
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, address = pool.share_mint(is_yes) @ PoolError::OutcomeMintMismatch)]
    pub share_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mut, token::mint = share_mint, token::token_program = token_program)]
    pub owner_shares: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        mut,
        constraint = recipient_outcome.mint == pool.outcome_mint(is_yes) @ PoolError::OutcomeMintMismatch
    )]
    pub recipient_outcome: Box<InterfaceAccount<'info, TokenAccount>>,
    pub predict: PredictAccounts<'info>,
    pub vanna: VannaAccounts<'info>,
    /// CHECK: key checked against `JUPITER_PROGRAM_ID` when a swap is needed.
    pub jupiter_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token2022>,
}

/// Burns `shares` of one side and pays out that side's outcome tokens. Pays from dangling tokens first. On a shortfall
/// it redeems USDC from Vanna, swaps it into CASH with the caller's Jupiter route (`swap_data`, accounts in
/// `remaining_accounts`; ignored when no swap is needed) and splits the CASH into fresh YES/NO pairs. The other half of
/// each pair stays in the pool as the opposite side's dangling tokens.
///
/// The swap must return at least one CASH per USDC, less `SWAP_DUST_UNITS` of fee rounding. The pool counts USDC at par,
/// so a worse price would take the difference from both sides' holders and hand it to whoever priced the route. If the
/// rounding leaves the split short of the shortfall, the redeemer is paid what it produced: at most `SWAP_DUST_UNITS`
/// under `assets`. Callers route exactly the shortfall; the last holder of a side cannot route more than the pool has.
pub fn redeem<'info>(
    ctx: Context<'info, RedeemOutcome<'info>>,
    is_yes: bool,
    shares: u64,
    swap_data: Vec<u8>,
) -> Result<()> {
    let a = &ctx.accounts;
    let pool = &a.pool;
    require!(pool.is_open(), PoolError::PoolSettled);
    require!(shares > 0, PoolError::ZeroAmount);
    a.predict.validate(pool)?;
    a.vanna.validate(pool)?;
    let pool_info = pool.to_account_info();

    // Each unit of invested USDC splits back into one token of this side, so the side's assets are its dangling tokens
    // plus the whole invested balance.
    let pool_outcome = a.predict.pool_outcome(is_yes);
    let dangling = token::amount(pool_outcome)?;
    let total_assets = checked_add(dangling, a.vanna.invested(&pool_info)?)?;
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

    if dangling < assets {
        let shortfall = assets - dangling;
        let usdc_in = jupiter::declared_in_amount(&swap_data)?;
        let slack = (shortfall as u128 * SWAP_SLACK_PPM as u128 / 1_000_000) as u64 + SWAP_SLACK_UNITS;
        require!(usdc_in >= shortfall && usdc_in - shortfall <= slack, PoolError::SwapAmountMismatch);

        a.vanna.ensure_idle_usdc(&pool_info, usdc_in, signer)?;
        let swap = jupiter::swap(
            &a.jupiter_program,
            ctx.remaining_accounts,
            swap_data,
            &pool_info,
            signer,
            Leg { account: a.vanna.pool_usdc.as_ref(), mint: USDC_MINT },
            Leg { account: a.predict.pool_cash.as_ref(), mint: pool.collateral_mint },
        )?;
        require!(swap.amount_out >= swap.amount_in.saturating_sub(SWAP_DUST_UNITS), PoolError::SwapBelowMinimum);
        emit!(Swapped {
            pool: pool.key(),
            cash_to_usdc: false,
            caller: a.authority.key(),
            amount_in: swap.amount_in,
            amount_out: swap.amount_out,
        });

        // Split the whole CASH balance, which is this pool's by construction. Anything the swap returned above the
        // shortfall becomes extra pairs, shared by both sides.
        let cash = token::amount(&a.predict.pool_cash)?;
        a.predict.set_accounts(&pool_info, &token_program).split(cash, signer)?;
    }

    // Everything the split produced is this side's to pay; only the swap's fee rounding can leave it under `assets`.
    let assets = assets.min(token::amount(pool_outcome)?);
    if assets > 0 {
        token::transfer_checked(
            &token_program,
            pool_outcome,
            a.predict.outcome_mint(is_yes),
            &a.recipient_outcome.to_account_info(),
            &pool_info,
            assets,
            signer,
        )?;
    }

    emit!(Redeem {
        pool: pool.key(),
        is_yes,
        caller: a.authority.key(),
        on_behalf: a.owner_shares.owner,
        to: a.recipient_outcome.owner,
        shares,
        assets,
    });
    Ok(())
}
