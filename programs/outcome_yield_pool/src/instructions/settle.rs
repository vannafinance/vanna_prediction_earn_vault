use crate::constants::CONFIG_SEED;
use crate::errors::PoolError;
use crate::events::PoolSettled;
use crate::predict::{self, OUTCOME_NO, OUTCOME_YES};
use crate::state::{Config, Pool, PoolStatus};
use anchor_lang::prelude::*;

#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: key checked against `pool.market`, contents parsed by `predict::read_outcome`.
    #[account(address = pool.market @ PoolError::NotPredictMarket)]
    pub market: UncheckedAccount<'info>,
}

/// Snapshots the winner from the prediCt Market account once World's operator has written it. Permissionless. World
/// closes resolved markets shortly after paying out, so this should run promptly after `determine_outcome`.
pub fn settle(ctx: Context<Settle>) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    require!(pool.is_open(), PoolError::PoolSettled);

    let outcome = predict::read_outcome(&ctx.accounts.market)?.ok_or_else(|| error!(PoolError::MarketNotResolved))?;
    let yes_won = match outcome {
        OUTCOME_YES => true,
        OUTCOME_NO => false,
        _ => return err!(PoolError::UnsupportedOutcome),
    };

    pool.status = PoolStatus::Settled { yes_won };
    emit!(PoolSettled { pool: pool.key(), yes_won, by_resolver: false });
    Ok(())
}

#[derive(Accounts)]
pub struct SettleByResolver<'info> {
    pub resolver: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = resolver @ PoolError::NotResolver)]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: key checked against `pool.market`; must already be closed.
    #[account(address = pool.market @ PoolError::NotPredictMarket)]
    pub market: UncheckedAccount<'info>,
}

/// Fallback for a pool nobody settled before World closed its market: with the Market account gone the outcome can no
/// longer be read on-chain, so the resolver records it. Refused while the Market account still exists.
pub fn settle_by_resolver(ctx: Context<SettleByResolver>, yes_won: bool) -> Result<()> {
    let pool = &mut ctx.accounts.pool;
    require!(pool.is_open(), PoolError::PoolSettled);
    require!(predict::is_closed(&ctx.accounts.market), PoolError::MarketStillOpen);

    pool.status = PoolStatus::Settled { yes_won };
    emit!(PoolSettled { pool: pool.key(), yes_won, by_resolver: true });
    Ok(())
}
