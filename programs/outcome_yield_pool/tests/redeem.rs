//! Redemptions before resolution, including the swap that turns the pool's USDC back into CASH for a split.

mod common;

use common::*;
use outcome_yield_pool::constants::SWAP_DUST_UNITS;
use outcome_yield_pool::errors::PoolError;
use solana_signer::Signer;

/// Alice 100 YES and Bob 60 NO, merged by the keeper: 40 YES dangle, ~60 CASH worth of USDC is in Vanna.
fn funded_pool() -> (Env, Pool, solana_keypair::Keypair, u64, solana_keypair::Keypair, u64) {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    let b = env.deposit(&p, &bob, false, 60 * ONE);
    (env, p, alice, a, bob, b)
}

#[test]
fn first_depositor_round_trip_is_exact() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    let shares = env.deposit_only(&p, &alice, true, 10 * ONE);
    assert_eq!(env.redeem(&p, &alice, true, shares), 10 * ONE);
    assert_eq!(env.total_shares(&p, true), 0);
}

#[test]
fn redeem_pays_from_dangling_without_touching_vanna() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let vanna_shares = env.vanna_shares(&p);
    let reserve = env.reserve_state();

    // 30% of Alice's claim fits inside the 40 dangling YES, so no route is needed at all.
    let owner = alice.pubkey();
    let expected = env.preview_redeem(&p, true, a * 3 / 10);
    assert!(expected < env.dangling(&p, true));
    let paid = env.try_redeem_with(&p, &alice, &owner, true, a * 3 / 10, &owner, Route::none()).unwrap();
    assert_eq!(paid, expected);
    assert_eq!(env.vanna_shares(&p), vanna_shares);
    assert_eq!(env.reserve_state(), reserve);
}

#[test]
fn shortfall_is_covered_by_redeeming_usdc_swapping_and_splitting() {
    let (mut env, p, alice, a, bob, b) = funded_pool();
    let owed = env.preview_redeem(&p, true, a);
    let no_before = env.total_assets(&p, false);

    assert_eq!(env.redeem(&p, &alice, true, a), owed);
    assert_eq!(env.vanna_shares(&p), 0, "the whole position was needed");
    // The split minted a NO alongside every YES Alice took, and the swap paid at least par, so Bob's side is whole.
    assert!(env.total_assets(&p, false) >= no_before);
    assert_eq!(env.balance(&p.pool_cash), 0);

    let bob_owed = env.preview_redeem(&p, false, b);
    assert_eq!(env.redeem(&p, &bob, false, b), bob_owed);
    assert!(bob_owed >= no_before - 1);
}

#[test]
fn redeeming_one_side_never_lowers_the_other_sides_assets() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 50 * ONE);
    env.deposit(&p, &bob, false, 80 * ONE);
    env.earn(5_000 * ONE, 10 * 86_400);

    let no_before = env.total_assets(&p, false);
    env.redeem(&p, &alice, true, a);
    let no_after = env.total_assets(&p, false);
    // At par or better the split hands the NO side at least what it gave up; Vanna's exit rounds by one unit.
    assert!(no_after + 1 >= no_before, "NO side moved from {no_before} to {no_after}");
}

#[test]
fn redeem_to_another_account() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, receiver) = (env.user(), env.user());
    let shares = env.deposit(&p, &alice, false, 4 * ONE);
    let owner = alice.pubkey();
    let paid = env.try_redeem_as(&p, &alice, &owner, false, shares, &receiver.pubkey()).unwrap();
    assert_eq!(paid, 4 * ONE);
    assert_eq!(env.outcome_of(&p, &receiver.pubkey(), false), 4 * ONE);
}

#[test]
fn cannot_redeem_more_shares_than_held() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    let shares = env.deposit(&p, &alice, true, ONE);
    let owner = alice.pubkey();
    assert_fails(env.try_redeem_with(&p, &alice, &owner, true, shares + 1, &owner, Route::none()));
}

#[test]
fn zero_share_redeem_is_rejected() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.deposit(&p, &alice, true, ONE);
    let owner = alice.pubkey();
    assert_pool_err(env.try_redeem_as(&p, &alice, &owner, true, 0, &owner), PoolError::ZeroAmount);
}

#[test]
fn recipient_must_hold_the_redeemed_sides_mint() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    let shares = env.deposit(&p, &alice, true, ONE);
    let owner = alice.pubkey();
    env.ensure_ata(&owner, &p.market.no_mint);
    let ix = env.redeem_ix(&p, &owner, &owner, true, shares, &owner, Route::none());
    let ix = swap_account(ix, &ata(&owner, &p.market.yes_mint), &ata(&owner, &p.market.no_mint));
    assert_pool_err(env.send_as(&alice, &[ix]), PoolError::OutcomeMintMismatch);
}

#[test]
fn many_small_redemptions_never_overpay() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let shares = env.deposit(&p, &alice, true, 10 * ONE);
    env.deposit(&p, &bob, false, 10 * ONE);
    env.earn(1_000 * ONE, 7 * 86_400);

    let owed = env.preview_redeem(&p, true, shares);
    let mut paid = 0;
    let chunk = shares / 7;
    for _ in 0..6 {
        paid += env.redeem(&p, &alice, true, chunk);
    }
    let rest = env.shares_of(&p, &alice.pubkey(), true);
    paid += env.redeem(&p, &alice, true, rest);
    // Each split may return a little more CASH than asked, which the next chunk shares in; never less than owed
    // beyond Vanna's one unit per exit.
    assert!(paid + 7 >= owed, "paid {paid} for a claim worth {owed}");
}

#[test]
fn redemption_that_needs_a_swap_without_a_route_fails() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    assert!(env.redemption_shortfall(&p, true, a) > 0);
    assert_pool_err(env.try_redeem_with(&p, &alice, &owner, true, a, &owner, Route::none()), PoolError::SwapNotAllowed);
    assert_eq!(env.shares_of(&p, &owner, true), a, "the failed redemption burned nothing");
}

#[test]
fn route_smaller_than_the_shortfall_is_refused() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    let shortfall = env.redemption_shortfall(&p, true, a);
    let route = env.route(&p, false, shortfall - 1, RouteKind::Direct);
    assert_pool_err(env.try_redeem_with(&p, &alice, &owner, true, a, &owner, route), PoolError::SwapAmountMismatch);
}

#[test]
fn route_far_larger_than_the_shortfall_is_refused() {
    // Otherwise a one-share redemption could pull the whole pool out of Vanna and cost it a swap to put it back.
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    let small = a / 2;
    let shortfall = env.redemption_shortfall(&p, true, small);
    assert!(shortfall > 0);
    let route = env.route(&p, false, shortfall + shortfall / 100, RouteKind::Direct);
    assert_pool_err(env.try_redeem_with(&p, &alice, &owner, true, small, &owner, route), PoolError::SwapAmountMismatch);
}

#[test]
fn route_slightly_above_the_shortfall_turns_the_surplus_into_pairs_for_both_sides() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    let owed = env.preview_redeem(&p, true, a / 2);
    let shortfall = env.redemption_shortfall(&p, true, a / 2);
    let (yes_before, no_before) = (env.total_assets(&p, true), env.total_assets(&p, false));
    let route = env.route(&p, false, shortfall + 500, RouteKind::Direct);
    assert_eq!(env.try_redeem_with(&p, &alice, &owner, true, a / 2, &owner, route).unwrap(), owed);
    // Converting invested USDC into pairs at par or better never lowers either side.
    assert!(env.total_assets(&p, true) + 1 >= yes_before - owed);
    assert!(env.total_assets(&p, false) + 1 >= no_before);
}

#[test]
fn redemption_through_a_shared_accounts_route_works() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    let owed = env.preview_redeem(&p, true, a);
    let route = env.redeem_route(&p, true, a, RouteKind::Shared);
    assert_eq!(env.try_redeem_with(&p, &alice, &owner, true, a, &owner, route).unwrap(), owed);
}

#[test]
fn a_route_is_ignored_when_dangling_tokens_cover_the_redemption() {
    let (mut env, p, alice, a, ..) = funded_pool();
    let owner = alice.pubkey();
    let route = env.route(&p, false, 5 * ONE, RouteKind::Direct);
    let vanna_shares = env.vanna_shares(&p);
    env.try_redeem_with(&p, &alice, &owner, true, a / 10, &owner, route).unwrap();
    assert_eq!(env.vanna_shares(&p), vanna_shares);
}

#[test]
fn a_tiny_redemption_is_not_stuck_on_the_swaps_fee_rounding() {
    // A balanced pool has nothing dangling, so even a few units of claim need a swap. The AMM loses a unit or two to
    // rounding however small the swap; the pool tolerates `SWAP_DUST_UNITS` of that, so small holders can always exit.
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 60 * ONE);
    env.deposit(&p, &bob, false, 60 * ONE);
    assert_eq!(env.dangling(&p, true), 0);

    let tiny = a / 500_000; // ~120 raw units of claim
    let owed = env.preview_redeem(&p, true, tiny);
    assert!(owed > 0 && owed < 1_000, "claim of {owed}");
    let paid = env.redeem(&p, &alice, true, tiny);
    assert!(paid <= owed && paid + SWAP_DUST_UNITS >= owed, "owed {owed}, paid {paid}");
}

#[test]
fn the_last_holder_of_a_side_redeems_everything_the_pool_invested() {
    // The last redeemer's shortfall is the whole Vanna position, so its route cannot ask for a unit more.
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 37 * ONE);
    env.deposit(&p, &bob, false, 37 * ONE);
    env.earn(2_000 * ONE, 9 * 86_400 + 1_234);
    let owed = env.preview_redeem(&p, true, a);
    let paid = env.redeem(&p, &alice, true, a);
    assert!(paid <= owed && paid + SWAP_DUST_UNITS >= owed, "owed {owed}, paid {paid}");
    assert!(env.vanna_value(&p) <= 1);
}
