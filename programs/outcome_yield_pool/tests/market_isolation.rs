//! Pools never reach each other's funds, even though every pool lends into the same Vanna reserve.

mod common;

use common::*;
use outcome_yield_pool::errors::PoolError;
use solana_signer::Signer;

#[test]
fn deposits_in_one_market_do_not_move_another() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&a, &alice, true, 10 * ONE);
    env.deposit(&a, &bob, false, 10 * ONE);

    assert_eq!(env.total_assets(&b, true), 0);
    assert_eq!(env.total_assets(&b, false), 0);
    assert_eq!(env.vanna_shares(&b), 0);
}

#[test]
fn shared_reserve_interest_is_split_by_each_pools_own_shares() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let users: Vec<_> = (0..4).map(|_| env.user()).collect();
    env.deposit(&a, &users[0], true, 30 * ONE);
    env.deposit(&a, &users[1], false, 30 * ONE);
    env.deposit(&b, &users[2], true, 10 * ONE);
    env.deposit(&b, &users[3], false, 10 * ONE);
    let (a0, b0) = (env.invested(&a), env.invested(&b));
    env.earn(5_000 * ONE, 60 * 86_400);

    let (a_gain, b_gain) = (env.invested(&a) - a0, env.invested(&b) - b0);
    assert!(a_gain > 0 && b_gain > 0);
    let expected = b_gain as f64 * env.vanna_shares(&a) as f64 / env.vanna_shares(&b) as f64;
    assert!((a_gain as f64 - expected).abs() <= 3.0, "A gained {a_gain}, pro rata says {expected}");
}

#[test]
fn a_pool_cannot_be_driven_with_another_pools_accounts() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let (alice, bob, carol) = (env.user(), env.user(), env.user());
    let shares = env.deposit(&a, &alice, true, 10 * ONE);
    env.deposit(&a, &carol, false, 10 * ONE);
    env.deposit(&b, &bob, true, 10 * ONE);

    let owner = alice.pubkey();
    for (from, to, err) in [
        (a.pool_yes, b.pool_yes, PoolError::OutcomeMintMismatch),
        (a.pool_cash, b.pool_cash, PoolError::OutcomeMintMismatch),
        (a.pool_usdc, b.pool_usdc, PoolError::VannaAccountMismatch),
        (a.pool_vanna_shares, b.pool_vanna_shares, PoolError::VannaAccountMismatch),
    ] {
        let route = env.redeem_route(&a, true, shares, RouteKind::Direct);
        let ix = env.redeem_ix(&a, &owner, &owner, true, shares, &owner, route);
        assert_pool_err(env.send_as(&alice, &[swap_account(ix, &from, &to)]), err);
    }
}

#[test]
fn merge_cannot_be_pointed_at_another_markets_tokens() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&b, &alice, true, ONE);
    env.deposit_only(&b, &bob, false, ONE);

    let keeper = env.keeper.insecure_clone();
    let route = env.route(&a, true, ONE, RouteKind::Direct);
    let ix = env.merge_ix(&a, &keeper.pubkey(), route);
    let ix = swap_account(ix, &a.market.market, &b.market.market);
    assert_pool_err(env.send_as(&keeper, &[ix]), PoolError::NotPredictMarket);
}

#[test]
fn another_pools_shares_cannot_be_redeemed_here() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let alice = env.user();
    let shares = env.deposit(&b, &alice, true, ONE);
    let owner = alice.pubkey();
    env.ensure_ata(&owner, &a.market.yes_mint);
    let ix = env.redeem_ix(&a, &owner, &owner, true, shares, &owner, Route::none());
    let ix = swap_account(ix, &a.yes_share_mint, &b.yes_share_mint);
    let ix = swap_account(ix, &ata(&owner, &a.yes_share_mint), &ata(&owner, &b.yes_share_mint));
    assert_pool_err(env.send_as(&alice, &[ix]), PoolError::OutcomeMintMismatch);
}

#[test]
fn a_route_cannot_spend_or_sign_for_another_pool() {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let users: Vec<_> = (0..4).map(|_| env.user()).collect();
    env.deposit(&b, &users[0], true, 10 * ONE);
    env.deposit(&b, &users[1], false, 10 * ONE);
    env.deposit_only(&a, &users[2], true, 5 * ONE);
    env.deposit_only(&a, &users[3], false, 5 * ONE);

    let keeper = env.keeper.insecure_clone();
    let b_value = env.invested(&b);
    for (from, to) in [(a.pool_cash, b.pool_cash), (a.pool_usdc, b.pool_usdc), (a.pool, b.pool)] {
        let route = env.route(&a, true, 5 * ONE, RouteKind::Direct).swap_account(&from, &to);
        assert_pool_err(env.merge_with(&a, &keeper, route), PoolError::InvalidSwapAccounts);
    }
    assert_eq!(env.invested(&b), b_value);
}
