//! The Jupiter route is caller-supplied and runs with the pool PDA as a signer, so every way a route could reach past
//! the swap is refused, and the price is checked against par. Also the ways Vanna can refuse the pool.

mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use common::*;
use outcome_yield_pool::constants::{CASH_MINT, JUPITER_PROGRAM_ID, PREDICT_PROGRAM_ID, USDC_MINT, VANNA_PROGRAM_ID};
use outcome_yield_pool::errors::PoolError;
use solana_keypair::Keypair;
use solana_signer::Signer;

const SETS: u64 = 10 * ONE;

/// A pool holding `SETS` complete sets, ready for the keeper to merge.
fn ready_to_merge() -> (Env, Pool, Keypair) {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, SETS);
    env.deposit_only(&p, &bob, false, SETS);
    let keeper = env.keeper.insecure_clone();
    (env, p, keeper)
}

fn merge_with(env: &mut Env, p: &Pool, keeper: &Keypair, route: Route) -> litesvm::types::TransactionResult {
    let res = env.merge_with(p, keeper, route);
    if res.is_err() {
        assert_eq!(env.complete_sets(p), SETS, "a refused merge must roll back whole");
    }
    res
}

/// A wallet with its own CASH and USDC accounts, to stand in for somebody else's funds.
fn outsider(env: &mut Env) -> (Keypair, Pubkey, Pubkey) {
    let o = env.user();
    let cash = env.mint_cash(&o.pubkey(), 5 * ONE);
    let usdc = env.mint_usdc(&o.pubkey(), 5 * ONE);
    (o, cash, usdc)
}

#[test]
fn a_well_formed_route_merges() {
    let (mut env, p, keeper) = ready_to_merge();
    let route = env.route(&p, true, SETS, RouteKind::Direct);
    merge_with(&mut env, &p, &keeper, route).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
}

#[test]
fn a_shared_accounts_route_merges() {
    let (mut env, p, keeper) = ready_to_merge();
    let route = env.route(&p, true, SETS, RouteKind::Shared);
    merge_with(&mut env, &p, &keeper, route).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
}

#[test]
fn the_route_must_swap_exactly_the_merged_cash() {
    let (mut env, p, keeper) = ready_to_merge();
    let route = env.route(&p, true, SETS - 1, RouteKind::Direct);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::SwapAmountMismatch);

    // CASH already idle in the pool is not the merge's to swap.
    let donor = env.user();
    env.mint_cash(&donor.pubkey(), ONE);
    env.transfer(&donor, &CASH_MINT, &p.pool, ONE).unwrap();
    let route = env.route(&p, true, SETS + ONE, RouteKind::Direct);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::SwapAmountMismatch);
}

#[test]
fn the_route_must_spend_from_the_pools_cash() {
    let (mut env, p, keeper) = ready_to_merge();
    let (_, their_cash, _) = outsider(&mut env);
    let route = env.route(&p, true, SETS, RouteKind::Direct).swap_account(&p.pool_cash, &their_cash);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
}

#[test]
fn the_route_must_pay_into_the_pools_usdc() {
    let (mut env, p, keeper) = ready_to_merge();
    let (_, _, their_usdc) = outsider(&mut env);
    for kind in [RouteKind::Direct, RouteKind::Shared] {
        let route = env.route(&p, true, SETS, kind).swap_account(&p.pool_usdc, &their_usdc);
        assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    }
}

#[test]
fn the_pool_must_be_the_routes_authority() {
    let (mut env, p, keeper) = ready_to_merge();
    let route = env.route(&p, true, SETS, RouteKind::Direct).set_account(1, keeper.pubkey());
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    let route = env.route(&p, true, SETS, RouteKind::Shared).set_account(2, keeper.pubkey());
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
}

#[test]
fn the_route_cannot_name_any_other_pool_token_account() {
    // The pool signs the CPI, so its YES, NO and Vanna share accounts must be unreachable from the route.
    let (mut env, p, keeper) = ready_to_merge();
    let last = env.route(&p, true, SETS, RouteKind::Direct).accounts.len() - 1;
    for protected in [p.pool_yes, p.pool_no, p.pool_vanna_shares] {
        let route = env.route(&p, true, SETS, RouteKind::Direct).set_account(last, protected);
        assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    }
}

#[test]
fn the_route_cannot_name_programs_that_honour_the_pools_signature() {
    let (mut env, p, keeper) = ready_to_merge();
    let last = env.route(&p, true, SETS, RouteKind::Direct).accounts.len() - 1;
    for program in [outcome_yield_pool::ID, VANNA_PROGRAM_ID, PREDICT_PROGRAM_ID] {
        let route = env.route(&p, true, SETS, RouteKind::Direct).set_account(last, program);
        assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    }
}

#[test]
fn the_route_cannot_name_other_accounts_this_program_owns() {
    let (mut env, p, keeper) = ready_to_merge();
    let other = env.create_market_and_pool();
    let last = env.route(&p, true, SETS, RouteKind::Direct).accounts.len() - 1;
    for owned in [Env::config_pda(), other.pool, Env::allowed_market_pda(&p.market.market)] {
        let route = env.route(&p, true, SETS, RouteKind::Direct).set_account(last, owned);
        assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    }
}

#[test]
fn a_platform_fee_is_refused() {
    let (mut env, p, keeper) = ready_to_merge();
    let mut route = env.route(&p, true, SETS, RouteKind::Direct);
    *route.data.last_mut().unwrap() = 50;
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::SwapNotAllowed);
}

#[test]
fn a_fee_account_or_an_output_redirect_is_refused() {
    let (mut env, p, keeper) = ready_to_merge();
    let (_, _, their_usdc) = outsider(&mut env);
    // `route`: 4 is the redirect destination, 6 the platform fee account. `shared_accounts_route`: 9 is the fee account.
    for (kind, slot) in [(RouteKind::Direct, 4), (RouteKind::Direct, 6), (RouteKind::Shared, 9)] {
        let route = env.route(&p, true, SETS, kind).set_account(slot, their_usdc);
        assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    }
}

#[test]
fn the_mints_named_by_the_route_must_be_the_swaps() {
    let (mut env, p, keeper) = ready_to_merge();
    // `route` names the destination mint at 5; `shared_accounts_route` names source and destination at 7 and 8.
    let route = env.route(&p, true, SETS, RouteKind::Direct).set_account(5, CASH_MINT);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
    let route = env.route(&p, true, SETS, RouteKind::Shared).set_account(7, USDC_MINT);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::InvalidSwapAccounts);
}

#[test]
fn only_jupiters_exact_input_routes_are_accepted() {
    let (mut env, p, keeper) = ready_to_merge();
    // `sha256("global:exact_out_route")[..8]`
    let mut route = env.route(&p, true, SETS, RouteKind::Direct);
    route.data[..8].copy_from_slice(&[208, 51, 239, 151, 123, 43, 237, 92]);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::SwapNotAllowed);

    let mut route = env.route(&p, true, SETS, RouteKind::Direct);
    route.data.truncate(20);
    assert_pool_err(merge_with(&mut env, &p, &keeper, route), PoolError::SwapNotAllowed);
}

#[test]
fn the_swap_program_must_be_jupiter() {
    let (mut env, p, keeper) = ready_to_merge();
    let route = env.route(&p, true, SETS, RouteKind::Direct);
    let ix = env.merge_ix(&p, &keeper.pubkey(), route);
    let ix = swap_account(ix, &JUPITER_PROGRAM_ID, &WHIRLPOOL_PROGRAM);
    assert_pool_err(env.send_as(&keeper, &[ix]), PoolError::SwapNotAllowed);
}

#[test]
fn a_sandwiched_redemption_fails_instead_of_paying_below_par() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    env.deposit(&p, &bob, false, 100 * ONE);

    // A whale buys CASH first, so the redemption's USDC -> CASH leg would land below par.
    let whale = env.user();
    let w = whale.pubkey();
    let usdc = env.mint_usdc(&w, 1_000_000 * ONE);
    let cash = env.ensure_ata(&w, &CASH_MINT);
    let route = env.route_between(&w, &usdc, &cash, false, 1_000_000 * ONE, RouteKind::Direct);
    let mut accounts = route.accounts;
    for m in accounts.iter_mut().filter(|m| m.pubkey == w) {
        m.is_signer = true;
    }
    env.send_as(&whale, &[Instruction { program_id: JUPITER_PROGRAM_ID, accounts, data: route.data }]).unwrap();
    assert!(env.quote(false, 50 * ONE) < 50 * ONE, "the whale pushed USDC -> CASH below par");

    let owner = alice.pubkey();
    assert_pool_err(env.try_redeem_as(&p, &alice, &owner, true, a, &owner), PoolError::SwapBelowMinimum);
    assert_eq!(env.shares_of(&p, &owner, true), a);
}

/* VANNA REFUSING THE POOL */

#[test]
fn a_pool_with_no_vanna_position_never_calls_vanna() {
    let mut env = Env::new();
    let p = env.pool;
    env.set_vanna_mode(vanna::HALTED);
    env.set_reserve(0, vanna::FROZEN);
    let alice = env.user();
    let shares = env.deposit_only(&p, &alice, true, 5 * ONE);
    assert_eq!(env.redeem(&p, &alice, true, shares), 5 * ONE);
}

#[test]
fn a_frozen_reserve_blocks_divesting_redemptions_but_not_dangling_ones() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    env.deposit(&p, &bob, false, 60 * ONE);
    env.set_reserve(0, vanna::FROZEN);

    let owner = alice.pubkey();
    assert_fails(env.try_redeem_as(&p, &alice, &owner, true, a, &owner));
    // Pricing still works (refreshing a frozen reserve is allowed), and the 40 dangling YES still pay out.
    let paid = env.redeem(&p, &alice, true, a / 4);
    assert!(paid > 25 * ONE - ONE && paid <= 25 * ONE);
}

#[test]
fn halted_vanna_blocks_divesting_redemptions_until_it_resumes() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 60 * ONE);
    let b = env.deposit(&p, &bob, false, 60 * ONE);
    env.set_vanna_mode(vanna::HALTED);
    let owner = bob.pubkey();
    assert_fails(env.try_redeem_as(&p, &bob, &owner, false, b, &owner));

    env.set_vanna_mode(vanna::WITHDRAW_ONLY);
    assert!(env.redeem(&p, &bob, false, b) > 60 * ONE - 60 * ONE * KEEPER_LOSS_BPS as u64 / 10_000);
}

#[test]
fn vanna_exit_rounding_costs_at_most_one_unit_per_hop() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 10 * ONE);
    let b = env.deposit(&p, &bob, false, 10 * ONE);
    env.earn(3_333 * ONE, 13 * 86_400 + 777);

    let owed = env.preview_redeem(&p, true, a) + env.preview_redeem(&p, false, b);
    let paid = env.redeem(&p, &alice, true, a) + env.redeem(&p, &bob, false, b);
    // The USDC -> CASH leg pays at or above par, so any shortfall is Vanna's share rounding.
    assert!(paid + 2 >= owed, "owed {owed}, paid {paid}");
}
