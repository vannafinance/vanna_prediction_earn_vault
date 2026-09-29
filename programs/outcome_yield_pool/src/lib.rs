//! Earns yield on the idle YES/NO outcome tokens of World (world.xyz) prediction markets.
//!
//! It rests on one par identity, which World's prediCt program enforces exactly: `1 YES + 1 NO <-> 1 CASH` through
//! `split` / `merge`. Deposited YES and NO tokens are matched into
//! complete sets and merged into CASH, which is swapped into USDC through Jupiter and lent in Vanna's USDC reserve.
//! Redemptions pay dangling tokens first; on a shortfall they redeem USDC, swap it back into CASH and split it into
//! fresh pairs. USDC is counted at par with CASH, and every swap is checked against par.
//!
//! World closes a market after resolving it, so split/merge stop working. The pool therefore has a settled phase: `settle` snapshots the winner, and `redeem_settled` pays winners in tokens, CASH and
//! USDC.

#![allow(unexpected_cfgs)]

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod jupiter;
pub mod math;
pub mod predict;
pub mod state;
pub mod token;
pub mod vanna;

use anchor_lang::prelude::*;
use instructions::*;

declare_id!("2h3hJSZVR6DgSGXmmf4wdBqo8z16kzMyVJ5gJWT1mskZ");

#[program]
pub mod outcome_yield_pool {
    use super::*;

    /* ADMIN */

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        resolver: Pubkey,
        keeper: Pubkey,
        max_swap_loss_bps: u16,
    ) -> Result<()> {
        instructions::admin::initialize_config(ctx, resolver, keeper, max_swap_loss_bps)
    }

    pub fn set_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
        instructions::admin::set_admin(ctx, new_admin)
    }

    pub fn set_resolver(ctx: Context<AdminOnly>, new_resolver: Pubkey) -> Result<()> {
        instructions::admin::set_resolver(ctx, new_resolver)
    }

    pub fn set_swap_policy(ctx: Context<AdminOnly>, keeper: Pubkey, max_swap_loss_bps: u16) -> Result<()> {
        instructions::admin::set_swap_policy(ctx, keeper, max_swap_loss_bps)
    }

    pub fn allow_market(ctx: Context<AllowMarket>) -> Result<()> {
        instructions::admin::allow_market(ctx)
    }

    pub fn disallow_market(ctx: Context<DisallowMarket>) -> Result<()> {
        instructions::admin::disallow_market(ctx)
    }

    /* POOLS */

    pub fn create_pool(ctx: Context<CreatePool>) -> Result<()> {
        instructions::create_pool::create_pool(ctx)
    }

    pub fn deposit(ctx: Context<DepositOutcome>, is_yes: bool, amount: u64) -> Result<()> {
        instructions::deposit::deposit(ctx, is_yes, amount)
    }

    pub fn merge_and_deposit<'info>(ctx: Context<'info, MergeAndDeposit<'info>>, swap_data: Vec<u8>) -> Result<()> {
        instructions::merge_and_deposit::merge_and_deposit(ctx, swap_data)
    }

    pub fn redeem<'info>(
        ctx: Context<'info, RedeemOutcome<'info>>,
        is_yes: bool,
        shares: u64,
        swap_data: Vec<u8>,
    ) -> Result<()> {
        instructions::redeem::redeem(ctx, is_yes, shares, swap_data)
    }

    /* RESOLUTION */

    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        instructions::settle::settle(ctx)
    }

    pub fn settle_by_resolver(ctx: Context<SettleByResolver>, yes_won: bool) -> Result<()> {
        instructions::settle::settle_by_resolver(ctx, yes_won)
    }

    pub fn redeem_settled(ctx: Context<RedeemSettled>, is_yes: bool, shares: u64) -> Result<()> {
        instructions::redeem_settled::redeem_settled(ctx, is_yes, shares)
    }
}
