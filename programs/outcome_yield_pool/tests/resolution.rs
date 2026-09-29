//! Resolution. World closes split/merge once `determine_outcome` runs and later tears the market down, so the pool
//! settles explicitly and pays winners out of its tokens, CASH and Vanna USDC without converting anything.

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use outcome_yield_pool::errors::PoolError;
use solana_keypair::Keypair;
use solana_signer::Signer;

const YES: u16 = 10_000;
const NO: u16 = 0;

struct Funded {
    env: Env,
    p: Pool,
    alice: Keypair,
    a: u64,
    bob: Keypair,
    b: u64,
    /// The pool's Vanna position after interest: what the winning side is owed on top of its tokens.
    invested: u64,
}

/// Alice 100 YES and Bob 60 NO into the default pool, then 30 days of Vanna interest. Both keep the other leg of
/// their split.
fn funded_pool() -> Funded {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    let b = env.deposit(&p, &bob, false, 60 * ONE);
    env.earn(5_000 * ONE, 30 * 86_400);
    let invested = env.invested(&p);
    assert!(invested > 60 * ONE, "interest outgrew the swap cost: {invested}");
    Funded { env, p, alice, a, bob, b, invested }
}

/// What World's operator does after resolving: pays every winning holder in CASH and burns every losing balance,
/// including the market's real mainnet holders.
fn wind_down(env: &mut Env, p: &Pool, holders: &[Pubkey], yes_won: bool) {
    env.wind_down(&p.market, yes_won, holders);
}

#[test]
fn settle_before_resolution_fails() {
    let mut f = funded_pool();
    assert_pool_err(f.env.settle(&f.p), PoolError::MarketNotResolved);
}

#[test]
fn settle_snapshots_the_winner() {
    let mut f = funded_pool();
    f.env.determine_outcome(&f.p.market, YES).unwrap();
    f.env.settle(&f.p).unwrap();
    assert_eq!(f.env.pool_state(&f.p).status, PoolStatus::Settled { yes_won: true });
    assert_pool_err(f.env.settle(&f.p), PoolError::PoolSettled);
}

#[test]
fn non_binary_outcome_is_refused() {
    let mut f = funded_pool();
    f.env.determine_outcome(&f.p.market, 5_000).unwrap();
    assert_pool_err(f.env.settle(&f.p), PoolError::UnsupportedOutcome);
}

#[test]
fn yes_win_pays_yes_holders_their_tokens_plus_the_invested_usdc() {
    let Funded { mut env, p, alice, a, bob, b, invested } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();
    env.settle(&p).unwrap();

    let (tokens, cash, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!(tokens, 40 * ONE, "the dangling YES come back as tokens");
    assert_eq!(cash, 0);
    assert!(usdc.abs_diff(invested) <= 2, "the invested USDC and its interest come back as USDC: {usdc} of {invested}");

    // Every invested USDC backed a winning token, so the losing side is left with nothing.
    assert_eq!(env.redeem_settled(&p, &bob, false, b), (0, 0, 0));
    assert_eq!(env.total_shares(&p, false), 0);
    assert!(env.vanna_value(&p) <= 1);
}

#[test]
fn no_win_pays_no_holders_and_leaves_yes_with_its_worthless_tokens() {
    let Funded { mut env, p, alice, a, bob, b, invested } = funded_pool();
    env.determine_outcome(&p.market, NO).unwrap();
    env.settle(&p).unwrap();

    let (tokens, cash, usdc) = env.redeem_settled(&p, &bob, false, b);
    assert_eq!((tokens, cash), (0, 0));
    assert!(usdc.abs_diff(invested) <= 2, "bob got {usdc} of {invested}");

    let (tokens, cash, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!(tokens, 40 * ONE, "losing holders get their own tokens back");
    assert_eq!((cash, usdc), (0, 0));
}

#[test]
fn operator_payout_to_the_pool_becomes_cash_for_winners() {
    let Funded { mut env, p, alice, a, invested, .. } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();
    // World redeems the pool's 40 winning YES for 40 CASH on the pool's behalf.
    env.redeem_for_user(&p.market, &p.market.yes_mint, &p.pool).unwrap();
    assert_eq!(env.balance(&p.pool_cash), 40 * ONE);
    env.settle(&p).unwrap();

    let (tokens, cash, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!((tokens, cash), (0, 40 * ONE));
    assert!(usdc.abs_diff(invested) <= 2, "alice got {usdc} of {invested}");
}

#[test]
fn winners_split_pro_rata_across_tokens_cash_and_usdc() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, carol, bob) = (env.user(), env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 30 * ONE);
    let c = env.deposit(&p, &carol, true, 70 * ONE);
    env.deposit(&p, &bob, false, 60 * ONE);
    env.earn(5_000 * ONE, 30 * 86_400);
    let total = env.total_assets(&p, true);
    env.determine_outcome(&p.market, YES).unwrap();
    env.settle(&p).unwrap();

    let (at, ac, au) = env.redeem_settled(&p, &alice, true, a);
    let (ct, cc, cu) = env.redeem_settled(&p, &carol, true, c);
    let (alice_total, carol_total) = (at + ac + au, ct + cc + cu);
    assert!(alice_total.abs_diff(total * 3 / 10) <= 2, "alice got {alice_total} of {total}");
    assert!(carol_total.abs_diff(total * 7 / 10) <= 3, "carol got {carol_total} of {total}");
    assert!(alice_total + carol_total <= total);
}

#[test]
fn market_closed_before_anyone_settled_falls_back_to_the_resolver() {
    let Funded { mut env, p, alice, a, bob, invested, .. } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();
    wind_down(&mut env, &p, &[alice.pubkey(), bob.pubkey(), p.pool], true);
    env.close_market(&p.market).unwrap();
    assert!(!env.account_exists(&p.market.market));
    assert!(!env.account_exists(&p.market.yes_mint));

    assert_fails(env.settle(&p));
    let admin = env.admin.insecure_clone();
    let ix = env.settle_by_resolver_ix(&p, &admin.pubkey(), true);
    env.send_as(&admin, &[ix]).unwrap();

    // The pool's 40 YES were paid out as CASH before the mint closed; the rest is USDC in Vanna.
    let (tokens, cash, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!((tokens, cash), (0, 40 * ONE));
    assert!(usdc.abs_diff(invested) <= 2, "alice got {usdc} of {invested}");
}

#[test]
fn resolver_cannot_settle_while_the_market_account_exists() {
    let mut f = funded_pool();
    f.env.determine_outcome(&f.p.market, YES).unwrap();
    let admin = f.env.admin.insecure_clone();
    let ix = f.env.settle_by_resolver_ix(&f.p, &admin.pubkey(), false);
    assert_pool_err(f.env.send_as(&admin, &[ix]), PoolError::MarketStillOpen);
}

#[test]
fn only_the_resolver_can_use_the_fallback() {
    let Funded { mut env, p, alice, bob, .. } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();
    wind_down(&mut env, &p, &[alice.pubkey(), bob.pubkey(), p.pool], true);
    env.close_market(&p.market).unwrap();

    let stranger = env.user();
    let ix = env.settle_by_resolver_ix(&p, &stranger.pubkey(), false);
    assert_pool_err(env.send_as(&stranger, &[ix]), PoolError::NotResolver);
}

#[test]
fn settled_pool_refuses_open_market_actions() {
    let Funded { mut env, p, alice, a, .. } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();
    env.settle(&p).unwrap();

    let owner = alice.pubkey();
    assert_pool_err(env.try_redeem_as(&p, &alice, &owner, true, a, &owner), PoolError::PoolSettled);
    assert_pool_err(env.merge(&p), PoolError::PoolSettled);
    // Alice still holds the NO leg of her original split.
    assert_pool_err(env.try_deposit_as(&p, &alice, false, ONE, &owner), PoolError::PoolSettled);
}

#[test]
fn settled_redemption_requires_settlement() {
    let Funded { mut env, p, alice, a, .. } = funded_pool();
    let owner = alice.pubkey();
    assert_pool_err(env.try_redeem_settled_as(&p, &alice, &owner, true, a, &owner), PoolError::PoolNotSettled);
}

#[test]
fn between_resolution_and_settlement_only_dangling_redemptions_work() {
    let Funded { mut env, p, alice, a, bob, b, .. } = funded_pool();
    env.determine_outcome(&p.market, YES).unwrap();

    // A quarter of Alice's claim fits in the 40 dangling YES. Bob's NO side has no dangling tokens at all, so paying
    // him needs prediCt's split, which is closed once the market resolves.
    let small = a / 4;
    let expected = env.preview_redeem(&p, true, small);
    assert!(expected < env.dangling(&p, true));
    assert_eq!(env.redeem(&p, &alice, true, small), expected);

    let owner = bob.pubkey();
    assert_fails(env.try_redeem_as(&p, &bob, &owner, false, b, &owner));
    assert_eq!(env.shares_of(&p, &owner, false), b);
}

#[test]
fn deposit_is_refused_once_the_market_is_determined() {
    // Between `determine_outcome` and `settle`, World can pay the pool CASH for its winning tokens. Open-market pricing
    // does not count that CASH, so a deposit then would buy a share of it at a discount and take it from the winners.
    let Funded { mut env, p, alice, a, invested, .. } = funded_pool();
    let mallory = env.user();
    env.mint_outcome_tokens(&mallory, &p.market, 60 * ONE);
    env.determine_outcome(&p.market, YES).unwrap();
    env.redeem_for_user(&p.market, &p.market.yes_mint, &p.pool).unwrap();

    let to = mallory.pubkey();
    assert_pool_err(env.try_deposit_as(&p, &mallory, true, 60 * ONE, &to), PoolError::MarketResolved);

    env.settle(&p).unwrap();
    let (tokens, cash, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!((tokens, cash), (0, 40 * ONE));
    assert!(usdc.abs_diff(invested) <= 2);
}

#[test]
fn merging_stops_once_the_market_is_determined() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 5 * ONE);
    env.deposit_only(&p, &bob, false, 5 * ONE);
    env.determine_outcome(&p.market, NO).unwrap();
    // prediCt's `MarketAlreadyDetermined`; the pool rolls back whole and both sides still dangle.
    assert_custom(env.merge(&p), 6000);
    assert_eq!(env.complete_sets(&p), 5 * ONE);
}

#[test]
fn winners_wait_for_vanna_liquidity_when_borrowers_hold_it() {
    let Funded { mut env, p, alice, a, invested, .. } = funded_pool();
    // Borrowers draw nearly everything the reserve holds.
    let r = env.reserve_state();
    env.borrow(r.accounted_liquidity - r.accrued_fees - ONE);
    env.determine_outcome(&p.market, YES).unwrap();
    env.settle(&p).unwrap();

    let owner = alice.pubkey();
    assert_fails(env.try_redeem_settled_as(&p, &alice, &owner, true, a, &owner));
    assert_eq!(env.shares_of(&p, &owner, true), a, "nothing was burned");

    env.repay_all();
    let (tokens, _, usdc) = env.redeem_settled(&p, &alice, true, a);
    assert_eq!(tokens, 40 * ONE);
    assert!(usdc >= invested - 2, "alice got {usdc} of at least {invested}");
}

#[test]
fn the_losing_side_gets_no_interest() {
    let Funded { mut env, p, bob, b, .. } = funded_pool();
    env.warp(60 * 86_400);
    env.determine_outcome(&p.market, YES).unwrap();
    env.settle(&p).unwrap();
    assert_eq!(env.redeem_settled(&p, &bob, false, b), (0, 0, 0));
}
