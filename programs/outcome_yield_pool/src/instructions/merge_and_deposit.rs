use super::shared::*;
use crate::constants::{BASIS_POINTS, CONFIG_SEED, SWAP_DUST_UNITS, USDC_MINT};
use crate::errors::PoolError;
use crate::events::{MergeAndDeposit as MergeAndDepositEvent, Swapped};
use crate::jupiter::{self, Leg};
use crate::state::{Config, Pool};
use crate::token;
use anchor_lang::prelude::*;
use anchor_spl::token_2022::Token2022;

#[derive(Accounts)]
pub struct MergeAndDeposit<'info> {
    /// Anyone. The config's keeper alone may accept a CASH -> USDC price below par.
    pub caller: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, Config>>,
    /// Writable because prediCt requires its `merge` signer to be, and so does Vanna's `lender_supply`.
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    pub predict: PredictAccounts<'info>,
    pub vanna: VannaAccounts<'info>,
    /// CHECK: key checked against `JUPITER_PROGRAM_ID`.
    pub jupiter_program: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token2022>,
}

/// Merges every complete set the pool holds into CASH through prediCt, swaps exactly that CASH into USDC with the
/// caller's Jupiter route (`swap_data`, route accounts in `remaining_accounts`) and supplies all the pool's USDC to
/// Vanna. A no-op when there is no complete set.
///
/// The pool counts USDC at par, so the swap must return at least one USDC per CASH, less `SWAP_DUST_UNITS` of fee
/// rounding. Only the config's keeper may accept less, by at most `max_swap_loss_bps`: anyone else could route the swap
/// through a venue they price and keep the difference, at the expense of both sides.
pub fn merge_and_deposit<'info>(ctx: Context<'info, MergeAndDeposit<'info>>, swap_data: Vec<u8>) -> Result<()> {
    let a = &ctx.accounts;
    let pool = &a.pool;
    require!(pool.is_open(), PoolError::PoolSettled);
    a.predict.validate(pool)?;
    a.vanna.validate(pool)?;

    let yes_balance = token::amount(&a.predict.pool_yes)?;
    let no_balance = token::amount(&a.predict.pool_no)?;
    let complete_sets = yes_balance.min(no_balance);
    if complete_sets == 0 {
        return Ok(());
    }

    let bump = [pool.bump];
    let seeds = pool_seeds(pool, &bump);
    let signer = &[&seeds[..]];
    let pool_info = pool.to_account_info();
    let token_program = a.token_program.to_account_info();

    let cash_before = token::amount(&a.predict.pool_cash)?;
    a.predict.set_accounts(&pool_info, &token_program).merge(complete_sets, signer)?;
    let cash_after = token::amount(&a.predict.pool_cash)?;
    require!(cash_after.checked_sub(cash_before) == Some(complete_sets), PoolError::MergeAmountMismatch);

    // Swap exactly the merged CASH, so CASH that was already idle is not swept in.
    let swap = jupiter::swap(
        &a.jupiter_program,
        ctx.remaining_accounts,
        swap_data,
        &pool_info,
        signer,
        Leg { account: a.predict.pool_cash.as_ref(), mint: pool.collateral_mint },
        Leg { account: a.vanna.pool_usdc.as_ref(), mint: USDC_MINT },
    )?;
    require!(swap.amount_in == complete_sets, PoolError::SwapAmountMismatch);
    let loss_bps = a.config.swap_loss_bps(&a.caller.key()) as u128;
    let min_out = ((complete_sets as u128 * (BASIS_POINTS as u128 - loss_bps) / BASIS_POINTS as u128) as u64)
        .saturating_sub(SWAP_DUST_UNITS);
    require!(swap.amount_out >= min_out, PoolError::SwapBelowMinimum);
    emit!(Swapped {
        pool: pool.key(),
        cash_to_usdc: true,
        caller: a.caller.key(),
        amount_in: swap.amount_in,
        amount_out: swap.amount_out,
    });

    // Supply every USDC the pool holds, including dust left idle by earlier redemptions. The share count Vanna mints
    // is not checked: it rounds against the pool by under one share. See README § Accepted risks.
    let usdc_supplied = token::amount(a.vanna.pool_usdc.as_ref())?;
    a.vanna.vanna(&pool_info).supply(usdc_supplied, signer)?;

    emit!(MergeAndDepositEvent { pool: pool.key(), complete_sets, usdc_supplied });
    Ok(())
}
