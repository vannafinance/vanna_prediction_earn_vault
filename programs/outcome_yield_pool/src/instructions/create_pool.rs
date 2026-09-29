use crate::constants::*;
use crate::errors::PoolError;
use crate::events::PoolCreated;
use crate::predict;
use crate::state::{AllowedMarket, Pool, PoolStatus};
use crate::vanna;
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::Token;
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_interface::{Mint, TokenAccount};

#[derive(Accounts)]
pub struct CreatePool<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [ALLOWED_MARKET_SEED, market.key().as_ref()], bump = allowed_market.bump)]
    pub allowed_market: Box<Account<'info, AllowedMarket>>,
    /// CHECK: parsed and validated by `predict::read_market`.
    pub market: UncheckedAccount<'info>,
    /// CHECK: must be Vanna's USDC reserve; parsed and validated by `vanna::read_reserve`.
    pub vanna_reserve: UncheckedAccount<'info>,
    #[account(address = USDC_MINT @ PoolError::InvalidVannaReserve, mint::token_program = spl_token_program)]
    pub usdc_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = spl_token_program)]
    pub vanna_share_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = token_program)]
    pub collateral_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = token_program)]
    pub yes_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(mint::token_program = token_program)]
    pub no_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        space = 8 + Pool::INIT_SPACE,
        seeds = [POOL_SEED, market.key().as_ref(), vanna_reserve.key().as_ref()],
        bump
    )]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        init,
        payer = payer,
        seeds = [YES_SHARES_SEED, pool.key().as_ref()],
        bump,
        mint::decimals = collateral_mint.decimals + SHARE_DECIMALS_OFFSET,
        mint::authority = pool,
        mint::token_program = token_program
    )]
    pub yes_share_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        seeds = [NO_SHARES_SEED, pool.key().as_ref()],
        bump,
        mint::decimals = collateral_mint.decimals + SHARE_DECIMALS_OFFSET,
        mint::authority = pool,
        mint::token_program = token_program
    )]
    pub no_share_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = collateral_mint,
        associated_token::authority = pool,
        associated_token::token_program = token_program
    )]
    pub pool_cash: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = yes_mint,
        associated_token::authority = pool,
        associated_token::token_program = token_program
    )]
    pub pool_yes: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = no_mint,
        associated_token::authority = pool,
        associated_token::token_program = token_program
    )]
    pub pool_no: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(
        init,
        payer = payer,
        associated_token::mint = usdc_mint,
        associated_token::authority = pool,
        associated_token::token_program = spl_token_program
    )]
    pub pool_usdc: Box<InterfaceAccount<'info, TokenAccount>>,
    /// Created here so Vanna's `lender_supply` never has to create it with the pool as payer.
    #[account(
        init,
        payer = payer,
        associated_token::mint = vanna_share_mint,
        associated_token::authority = pool,
        associated_token::token_program = spl_token_program
    )]
    pub pool_vanna_shares: Box<InterfaceAccount<'info, TokenAccount>>,
    pub token_program: Program<'info, Token2022>,
    pub spl_token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Creates the pool for a World market over Vanna's USDC reserve. Permissionless for any allowed market.
pub fn create_pool(ctx: Context<CreatePool>) -> Result<()> {
    let a = &ctx.accounts;

    let market = predict::read_market(&a.market)?;
    require_keys_eq!(market.collateral_mint, CASH_MINT, PoolError::InvalidCollateral);
    require_keys_eq!(a.collateral_mint.key(), market.collateral_mint, PoolError::InvalidCollateral);
    require_keys_eq!(a.yes_mint.key(), market.yes_mint, PoolError::OutcomeMintMismatch);
    require_keys_eq!(a.no_mint.key(), market.no_mint, PoolError::OutcomeMintMismatch);

    // The yield vault is fixed: Vanna's reserve for USDC, which Vanna derives from the mint. It is read once, here,
    // and every later instruction checks the accounts against what is stored.
    let (expected_reserve, _) =
        Pubkey::find_program_address(&[vanna::RESERVE_SEED, USDC_MINT.as_ref()], &VANNA_PROGRAM_ID);
    require_keys_eq!(a.vanna_reserve.key(), expected_reserve, PoolError::InvalidVannaReserve);
    let reserve = vanna::read_reserve(&a.vanna_reserve)?;
    require_keys_eq!(reserve.underlying_mint, USDC_MINT, PoolError::InvalidVannaReserve);
    require_keys_eq!(a.vanna_share_mint.key(), reserve.share_mint, PoolError::InvalidVannaReserve);
    let (protocol_config, _) = Pubkey::find_program_address(&[vanna::PROTOCOL_SEED], &VANNA_PROGRAM_ID);

    let pool_key = a.pool.key();
    let pool = Pool {
        market: a.market.key(),
        vanna_reserve: a.vanna_reserve.key(),
        vanna_protocol_config: protocol_config,
        vanna_asset_config: reserve.asset_config,
        vanna_liquidity_vault: reserve.liquidity_vault,
        vanna_share_mint: reserve.share_mint,
        collateral_mint: market.collateral_mint,
        yes_mint: market.yes_mint,
        no_mint: market.no_mint,
        market_cash_vault: market.cash_vault,
        yes_share_mint: a.yes_share_mint.key(),
        no_share_mint: a.no_share_mint.key(),
        pool_cash: a.pool_cash.key(),
        pool_yes: a.pool_yes.key(),
        pool_no: a.pool_no.key(),
        pool_usdc: a.pool_usdc.key(),
        pool_vanna_shares: a.pool_vanna_shares.key(),
        status: PoolStatus::Open,
        bump: ctx.bumps.pool,
    };

    emit!(PoolCreated {
        pool: pool_key,
        market: pool.market,
        vanna_reserve: pool.vanna_reserve,
        yes_share_mint: pool.yes_share_mint,
        no_share_mint: pool.no_share_mint,
    });
    ctx.accounts.pool.set_inner(pool);
    Ok(())
}
