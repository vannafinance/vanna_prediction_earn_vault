use crate::constants::*;
use crate::errors::PoolError;
use crate::events::{MarketAllowed, MarketDisallowed, SwapPolicySet};
use crate::predict;
use crate::program::OutcomeYieldPool;
use crate::state::{AllowedMarket, Config};
use anchor_lang::prelude::*;

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(init, payer = admin, space = 8 + Config::INIT_SPACE, seeds = [CONFIG_SEED], bump)]
    pub config: Account<'info, Config>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ PoolError::Unauthorized)]
    pub program: Program<'info, OutcomeYieldPool>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ PoolError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,
    pub system_program: Program<'info, System>,
}

/// Only the program's upgrade authority can bootstrap the admin. A deployed program is not invocable until the next
/// slot, so without this check anyone watching the deployment could claim the config first.
pub fn initialize_config(
    ctx: Context<InitializeConfig>,
    resolver: Pubkey,
    keeper: Pubkey,
    max_swap_loss_bps: u16,
) -> Result<()> {
    require!(max_swap_loss_bps <= MAX_SWAP_LOSS_BPS_CAP, PoolError::InvalidSwapPolicy);
    ctx.accounts.config.set_inner(Config {
        admin: ctx.accounts.admin.key(),
        resolver,
        keeper,
        max_swap_loss_bps,
        bump: ctx.bumps.config,
    });
    emit!(SwapPolicySet { keeper, max_swap_loss_bps });
    Ok(())
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ PoolError::Unauthorized)]
    pub config: Account<'info, Config>,
}

pub fn set_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
    ctx.accounts.config.admin = new_admin;
    Ok(())
}

pub fn set_resolver(ctx: Context<AdminOnly>, new_resolver: Pubkey) -> Result<()> {
    ctx.accounts.config.resolver = new_resolver;
    Ok(())
}

/// Sets who may merge at a CASH -> USDC price below par, and by how much. Capped, so no configuration lets the keeper
/// lose more than `MAX_SWAP_LOSS_BPS_CAP` of a merge.
pub fn set_swap_policy(ctx: Context<AdminOnly>, keeper: Pubkey, max_swap_loss_bps: u16) -> Result<()> {
    require!(max_swap_loss_bps <= MAX_SWAP_LOSS_BPS_CAP, PoolError::InvalidSwapPolicy);
    ctx.accounts.config.keeper = keeper;
    ctx.accounts.config.max_swap_loss_bps = max_swap_loss_bps;
    emit!(SwapPolicySet { keeper, max_swap_loss_bps });
    Ok(())
}

#[derive(Accounts)]
pub struct AllowMarket<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ PoolError::Unauthorized)]
    pub config: Account<'info, Config>,
    /// CHECK: parsed and validated by `predict::read_market`.
    pub market: UncheckedAccount<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + AllowedMarket::INIT_SPACE,
        seeds = [ALLOWED_MARKET_SEED, market.key().as_ref()],
        bump
    )]
    pub allowed_market: Account<'info, AllowedMarket>,
    pub system_program: Program<'info, System>,
}

pub fn allow_market(ctx: Context<AllowMarket>) -> Result<()> {
    let view = predict::read_market(&ctx.accounts.market)?;
    require_keys_eq!(view.collateral_mint, CASH_MINT, PoolError::InvalidCollateral);

    let market = ctx.accounts.market.key();
    ctx.accounts.allowed_market.set_inner(AllowedMarket { market, bump: ctx.bumps.allowed_market });
    emit!(MarketAllowed { market });
    Ok(())
}

#[derive(Accounts)]
pub struct DisallowMarket<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump, has_one = admin @ PoolError::Unauthorized)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        close = admin,
        seeds = [ALLOWED_MARKET_SEED, allowed_market.market.as_ref()],
        bump = allowed_market.bump
    )]
    pub allowed_market: Account<'info, AllowedMarket>,
}

/// Stops new pools for the market. Pools that already exist keep serving their holders.
pub fn disallow_market(ctx: Context<DisallowMarket>) -> Result<()> {
    emit!(MarketDisallowed { market: ctx.accounts.allowed_market.market });
    Ok(())
}
