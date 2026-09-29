//! Who may redeem whose shares. Shares are plain Token-2022 balances, so ownership, delegation and transfers are the
//! token program's.

mod common;

use common::*;
use solana_signer::Signer;

#[test]
fn stranger_cannot_redeem_someone_elses_shares() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, mallory) = (env.user(), env.user());
    let shares = env.deposit(&p, &alice, true, 5 * ONE);
    let owner = alice.pubkey();
    let thief = mallory.pubkey();
    assert_fails(env.try_redeem_as(&p, &mallory, &owner, true, shares, &thief));
    assert_eq!(env.shares_of(&p, &owner, true), shares);
}

#[test]
fn approved_delegate_can_redeem_up_to_its_allowance() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, operator) = (env.user(), env.user());
    let shares = env.deposit(&p, &alice, false, 10 * ONE);
    env.approve(&alice, &p.no_share_mint, &operator.pubkey(), shares / 2).unwrap();

    let owner = alice.pubkey();
    let to = operator.pubkey();
    let paid = env.try_redeem_as(&p, &operator, &owner, false, shares / 2, &to).unwrap();
    assert_eq!(paid, 5 * ONE);
    assert_eq!(env.shares_of(&p, &owner, false), shares - shares / 2);

    assert_fails(env.try_redeem_as(&p, &operator, &owner, false, 1, &to));
}

#[test]
fn allowance_on_one_side_does_not_cover_the_other() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, operator) = (env.user(), env.user());
    env.deposit(&p, &alice, true, ONE);
    let no_shares = env.deposit(&p, &alice, false, ONE);
    env.approve(&alice, &p.yes_share_mint, &operator.pubkey(), u64::MAX).unwrap();

    let owner = alice.pubkey();
    let to = operator.pubkey();
    assert_fails(env.try_redeem_as(&p, &operator, &owner, false, no_shares, &to));
}

#[test]
fn transferred_shares_are_redeemable_by_the_new_holder() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    let shares = env.deposit(&p, &alice, true, 8 * ONE);
    env.transfer(&alice, &p.yes_share_mint, &bob.pubkey(), shares / 4).unwrap();

    assert_eq!(env.redeem(&p, &bob, true, shares / 4), 2 * ONE);
    let owner = alice.pubkey();
    assert_fails(env.try_redeem_as(&p, &alice, &owner, true, shares, &owner));
    assert_eq!(env.redeem(&p, &alice, true, shares - shares / 4), 6 * ONE);
}

#[test]
fn share_supply_equals_the_sum_of_holder_balances() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, carol) = (env.user(), env.user(), env.user());
    env.deposit(&p, &alice, true, 3 * ONE);
    env.deposit(&p, &bob, true, 4 * ONE);
    env.transfer(&bob, &p.yes_share_mint, &carol.pubkey(), 1_000).unwrap();

    let sum: u64 =
        [&alice, &bob, &carol].iter().map(|u| env.shares_of(&p, &u.pubkey(), true)).sum();
    assert_eq!(sum, env.total_shares(&p, true));
}
