use super::shared::*;
use crate::constants::{BASIS_POINTS, CONFIG_SEED, SWAP_DUST_UNITS, USDC_MINT};
use crate::errors::PoolError;
use crate::events::{MergeAndDeposit as MergeAndDepositEvent, Swapped};
use crate::jupiter::{self, Leg};
use crate::predict;
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

/// `MergeAndDeposit` for a market inside a World Group: the same accounts plus the Group, which owns the CASH vault
/// every outcome market of the Group shares.
#[derive(Accounts)]
pub struct MergeAndDepositInGroup<'info> {
    pub base: MergeAndDeposit<'info>,
    /// CHECK: a World `Group` account (owner and discriminator checked) that owns `market_cash_vault`.
    pub group: UncheckedAccount<'info>,
}

/// Merges every complete set the pool holds into CASH through prediCt, swaps exactly that CASH into USDC with the
/// caller's Jupiter route (`swap_data`, route accounts in `remaining_accounts`) and supplies all the pool's USDC to
/// Vanna. A no-op when there is no complete set.
///
/// The pool counts USDC at par, so the swap must return at least one USDC per CASH, less `SWAP_DUST_UNITS` of fee
/// rounding. Only the config's keeper may accept less, by at most `max_swap_loss_bps`: anyone else could route the swap
/// through a venue they price and keep the difference, at the expense of both sides.
///
/// Markets whose CASH vault belongs to a World Group use [`merge_and_deposit_in_group`] instead.
pub fn merge_and_deposit<'info>(ctx: Context<'info, MergeAndDeposit<'info>>, swap_data: Vec<u8>) -> Result<()> {
    merge_and_deposit_core(&ctx.accounts, ctx.remaining_accounts, swap_data, None)
}

/// [`merge_and_deposit`] for a market inside a World Group: prediCt's Group `merge` releases the CASH from the Group's
/// shared vault. Everything after the merge (par check, swap, Vanna supply) is identical.
pub fn merge_and_deposit_in_group<'info>(
    ctx: Context<'info, MergeAndDepositInGroup<'info>>,
    swap_data: Vec<u8>,
) -> Result<()> {
    let group = ctx.accounts.group.to_account_info();
    merge_and_deposit_core(&ctx.accounts.base, ctx.remaining_accounts, swap_data, Some(&group))
}

fn merge_and_deposit_core<'info>(
    a: &MergeAndDeposit<'info>,
    remaining_accounts: &'info [AccountInfo<'info>],
    swap_data: Vec<u8>,
    group: Option<&AccountInfo<'info>>,
) -> Result<()> {
    let pool = &a.pool;
    require!(pool.is_open(), PoolError::PoolSettled);
    a.predict.validate(pool)?;
    a.vanna.validate(pool)?;
    // Who signs for the CASH vault decides which prediCt merge applies: the market itself, or its World Group.
    let vault_owner = predict::token_account_owner(&a.predict.market_cash_vault)?;
    match group {
        Some(group) => {
            predict::require_group(group)?;
            require_keys_eq!(vault_owner, group.key(), PoolError::GroupMismatch);
        }
        None => require_keys_eq!(vault_owner, a.predict.market.key(), PoolError::MarketInGroup),
    }

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
    let set = a.predict.set_accounts(&pool_info, &token_program);
    match group {
        Some(group) => set.merge_in_group(group, complete_sets, signer)?,
        None => set.merge(complete_sets, signer)?,
    }
    let cash_after = token::amount(&a.predict.pool_cash)?;
    require!(cash_after.checked_sub(cash_before) == Some(complete_sets), PoolError::MergeAmountMismatch);

    // Swap exactly the merged CASH, so CASH that was already idle is not swept in.
    let swap = jupiter::swap(
        &a.jupiter_program,
        remaining_accounts,
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
