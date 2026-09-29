//! A merge that cannot complete is deferred instead of blocking deposits. The deferral is the caller leaving
//! `merge_and_deposit` out, or its transaction failing whole, since a failed CPI cannot be caught. A merge can fail at Vanna (supply paused, capped) or at the swap (price below what the caller
//! may accept).

mod common;

use common::*;
use outcome_yield_pool::errors::PoolError;
use solana_signer::Signer;

fn assert_untouched(env: &Env, p: &Pool, yes: u64, no: u64) {
    assert_eq!(env.dangling(p, true), yes);
    assert_eq!(env.dangling(p, false), no);
    assert_eq!(env.balance(&p.pool_cash), 0);
    assert_eq!(env.balance(&p.pool_usdc), 0);
    assert_eq!(env.vanna_shares(p), 0);
}

#[test]
fn a_reserve_refusing_supply_fails_the_merge_but_not_the_deposit() {
    for status in [vanna::REPAY_ONLY, vanna::FROZEN] {
        let mut env = Env::new();
        let p = env.pool;
        env.set_reserve(0, status);
        let (alice, bob) = (env.user(), env.user());

        assert!(env.deposit_only(&p, &alice, true, 10 * ONE) > 0);
        assert!(env.deposit_only(&p, &bob, false, 6 * ONE) > 0);
        assert_fails(env.merge(&p));
        // The failed merge rolled back whole, prediCt merge and swap included: both sides still dangle.
        assert_untouched(&env, &p, 10 * ONE, 6 * ONE);
    }
}

#[test]
fn vanna_paused_for_supply_defers_merges() {
    for mode in [vanna::WITHDRAW_ONLY, vanna::HALTED] {
        let mut env = Env::new();
        let p = env.pool;
        env.set_vanna_mode(mode);
        let (alice, bob) = (env.user(), env.user());
        env.deposit_only(&p, &alice, true, 4 * ONE);
        env.deposit_only(&p, &bob, false, 4 * ONE);
        assert_fails(env.merge(&p));
        assert_untouched(&env, &p, 4 * ONE, 4 * ONE);
    }
}

#[test]
fn a_full_supply_cap_defers_merges() {
    let mut env = Env::new();
    let p = env.pool;
    env.set_reserve(SEED_LENDER_USDC + ONE, vanna::ACTIVE);
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 5 * ONE);
    env.deposit_only(&p, &bob, false, 5 * ONE);
    assert_fails(env.merge(&p));
    assert_untouched(&env, &p, 5 * ONE, 5 * ONE);

    env.set_reserve(0, vanna::ACTIVE);
    env.merge(&p).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
}

#[test]
fn both_sides_dangling_still_redeem() {
    let mut env = Env::new();
    let p = env.pool;
    env.set_reserve(0, vanna::FROZEN);
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit_only(&p, &alice, true, 10 * ONE);
    let b = env.deposit_only(&p, &bob, false, 6 * ONE);

    assert_eq!(env.redeem(&p, &alice, true, a), 10 * ONE);
    assert_eq!(env.redeem(&p, &bob, false, b), 6 * ONE);
}

#[test]
fn deferred_merge_goes_through_once_vanna_recovers() {
    let mut env = Env::new();
    let p = env.pool;
    env.set_reserve(0, vanna::FROZEN);
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 10 * ONE);
    env.deposit_only(&p, &bob, false, 6 * ONE);
    assert_fails(env.merge(&p));

    env.set_reserve(0, vanna::ACTIVE);
    env.merge(&p).unwrap();
    assert!(env.invested(&p) > 0);
    assert_eq!(env.dangling(&p, true), 4 * ONE);
    assert_eq!(env.dangling(&p, false), 0);
}

#[test]
fn deposits_while_deferred_are_priced_like_any_other() {
    let mut env = Env::new();
    let p = env.pool;
    env.set_reserve(0, vanna::FROZEN);
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit_only(&p, &alice, true, 10 * ONE);
    let b = env.deposit_only(&p, &bob, true, 10 * ONE);
    assert_eq!(a, b);
}

#[test]
fn only_the_keeper_merges_below_par_and_the_deposit_waits_for_it() {
    // CASH trades just under USDC on the live whirlpool, so a par-only merge cannot go through.
    let mut env = Env::new();
    let p = env.pool;
    assert!(env.quote(true, 6 * ONE) < 6 * ONE, "the snapshot has CASH below par");
    let (alice, bob, stranger) = (env.user(), env.user(), env.user());
    env.deposit_only(&p, &alice, true, 10 * ONE);
    env.deposit_only(&p, &bob, false, 6 * ONE);

    assert_pool_err(env.merge_as(&p, &stranger), PoolError::SwapBelowMinimum);
    assert_untouched(&env, &p, 10 * ONE, 6 * ONE);

    env.merge(&p).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
}

#[test]
fn a_keeper_allowance_too_tight_for_the_market_defers_the_merge() {
    let mut env = Env::new();
    let p = env.pool;
    let admin = env.admin.insecure_clone();
    let keeper = env.keeper.pubkey();
    env.set_swap_policy_as(&admin, &keeper, 0).unwrap();
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 3 * ONE);
    env.deposit_only(&p, &bob, false, 3 * ONE);
    assert_pool_err(env.merge(&p), PoolError::SwapBelowMinimum);

    env.set_swap_policy_as(&admin, &keeper, KEEPER_LOSS_BPS).unwrap();
    env.merge(&p).unwrap();
}

#[test]
fn the_keepers_floor_is_exact_to_the_basis_point() {
    // The live whirlpool charges a fee on every CASH sale, so a merge always costs a few basis points. The keeper's
    // allowance must cover that cost; one basis point less refuses the merge.
    let mut env = Env::new();
    let p = env.pool;
    let sets = 6 * ONE;
    let out = env.quote(true, sets);
    let cost_bps = ((sets - out) * 10_000).div_ceil(sets) as u16;
    assert!((1..=KEEPER_LOSS_BPS).contains(&cost_bps), "whirlpool cost {cost_bps} bps");

    let admin = env.admin.insecure_clone();
    let keeper = env.keeper.pubkey();
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, sets);
    env.deposit_only(&p, &bob, false, sets);
    env.set_swap_policy_as(&admin, &keeper, cost_bps - 1).unwrap();
    assert_pool_err(env.merge(&p), PoolError::SwapBelowMinimum);
    env.set_swap_policy_as(&admin, &keeper, cost_bps).unwrap();
    env.merge(&p).unwrap();
    assert_eq!(env.invested(&p), out);
}

#[test]
fn a_dust_merge_is_not_stuck_on_the_swaps_fee_rounding() {
    // The swap loses a unit or two to rounding even on a handful of CASH; within `SWAP_DUST_UNITS` anyone may merge.
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, stranger) = (env.user(), env.user(), env.user());
    env.deposit_only(&p, &alice, true, 500);
    env.deposit_only(&p, &bob, false, 500);
    env.merge_as(&p, &stranger).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
    let out = env.invested(&p);
    assert!(out < 500 && out + outcome_yield_pool::constants::SWAP_DUST_UNITS >= 500, "500 CASH -> {out} USDC");
}
