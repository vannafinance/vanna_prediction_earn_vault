//! How Vanna's interest reaches each side. The yield is real: a borrower draws USDC from Vanna's reserve and pays
//! interest as time passes.

mod common;

use common::*;
use solana_signer::Signer;

const DAY: i64 = 86_400;

#[test]
fn worked_example_alice_yes_bob_no() {
    // Alice 100 YES, Bob 60 NO. 60 pairs merge into ~60 USDC in Vanna, which earns interest.
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    let b = env.deposit(&p, &bob, false, 60 * ONE);
    let invested = env.invested(&p);
    env.earn(5_000 * ONE, 30 * DAY);
    let interest = env.invested(&p) - invested;
    assert!(interest > 0);

    // Each side claims its dangling tokens plus the whole invested balance, interest included.
    assert_eq!(env.total_assets(&p, true), 40 * ONE + invested + interest);
    assert_eq!(env.total_assets(&p, false), invested + interest);

    let alice_paid = env.redeem(&p, &alice, true, a);
    let bob_paid = env.redeem(&p, &bob, false, b);
    assert!(alice_paid + 2 >= 40 * ONE + invested + interest, "alice got {alice_paid}");
    assert!(bob_paid + 2 >= invested + interest, "bob got {bob_paid}");
}

#[test]
fn both_sides_earn_the_same_interest_so_the_scarce_side_earns_the_higher_rate() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 200 * ONE);
    env.deposit(&p, &bob, false, 100 * ONE);
    let (yes, no) = (env.total_assets(&p, true), env.total_assets(&p, false));
    env.earn(5_000 * ONE, 90 * DAY);

    let yes_gain = env.total_assets(&p, true) - yes;
    let no_gain = env.total_assets(&p, false) - no;
    assert_eq!(yes_gain, no_gain, "one invested balance backs both sides");
    let yes_rate = yes_gain as f64 / yes as f64;
    let no_rate = no_gain as f64 / no as f64;
    // YES is half utilized (100 of its 200 matched), so it earns half NO's rate.
    assert!((yes_rate / no_rate - 0.5).abs() < 0.001, "yes {yes_rate} no {no_rate}");
}

#[test]
fn balanced_market_pays_both_sides_the_reserves_rate() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 50 * ONE);
    env.deposit(&p, &bob, false, 50 * ONE);
    let before = env.total_assets(&p, true);
    assert_eq!(before, env.total_assets(&p, false));
    env.earn(5_000 * ONE, 60 * DAY);
    assert!(env.total_assets(&p, true) > before);
    assert_eq!(env.total_assets(&p, true), env.total_assets(&p, false));
}

#[test]
fn one_sided_pool_invests_nothing_and_earns_nothing() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.deposit(&p, &alice, true, 50 * ONE);
    env.earn(5_000 * ONE, 60 * DAY);
    assert_eq!(env.invested(&p), 0);
    assert_eq!(env.total_assets(&p, true), 50 * ONE);
}

#[test]
fn late_depositor_does_not_capture_earlier_interest() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, carol) = (env.user(), env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 100 * ONE);
    env.deposit(&p, &bob, false, 100 * ONE);
    env.earn(5_000 * ONE, 60 * DAY);
    let alice_value = env.preview_redeem(&p, true, a);

    let c = env.deposit_only(&p, &carol, true, 100 * ONE);
    let carol_paid = env.redeem(&p, &carol, true, c);
    assert!((100 * ONE - 2..=100 * ONE).contains(&carol_paid), "carol got {carol_paid}");
    assert!(env.preview_redeem(&p, true, a) + 2 >= alice_value);
}

#[test]
fn yield_is_paid_in_outcome_tokens() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let a = env.deposit(&p, &alice, true, 10 * ONE);
    env.deposit(&p, &bob, false, 10 * ONE);
    env.earn(5_000 * ONE, 30 * DAY);

    let paid = env.redeem(&p, &alice, true, a);
    assert!(paid > 10 * ONE - 10 * ONE * KEEPER_LOSS_BPS as u64 / 10_000);
    assert_eq!(env.cash_of(&alice.pubkey()), 0);
    assert_eq!(env.usdc_of(&alice.pubkey()), 0);
}

#[test]
fn a_pool_earns_exactly_what_any_vanna_lender_earns() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, lender) = (env.user(), env.user(), env.user());
    env.deposit(&p, &alice, true, 100 * ONE);
    env.deposit(&p, &bob, false, 100 * ONE);
    env.vanna_supply(&lender, 100 * ONE).unwrap();
    let lender_shares = env.balance(&spl_ata(&lender.pubkey(), &vanna::share_mint(&outcome_yield_pool::constants::USDC_MINT)));
    let (pool_before, lender_before) = (env.vanna_value(&p), env.reserve_state().value_of(lender_shares));
    env.earn(5_000 * ONE, 45 * DAY);
    let pool_rate = (env.vanna_value(&p) - pool_before) as f64 / pool_before as f64;
    let lender_rate = (env.reserve_state().value_of(lender_shares) - lender_before) as f64 / lender_before as f64;
    assert!((pool_rate - lender_rate).abs() < 1e-6, "pool {pool_rate} vs lender {lender_rate}");
}

#[test]
fn interest_keeps_accruing_while_the_market_is_open() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 20 * ONE);
    env.deposit(&p, &bob, false, 20 * ONE);
    env.borrow(5_000 * ONE);
    let mut last = env.invested(&p);
    for _ in 0..5 {
        env.warp(7 * DAY);
        let now = env.invested(&p);
        assert!(now > last, "invested did not grow: {last} -> {now}");
        last = now;
    }
}
