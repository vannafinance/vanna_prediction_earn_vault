//! Deposits, merges and share pricing.

mod common;

use common::*;
use outcome_yield_pool::constants::USDC_MINT;
use outcome_yield_pool::errors::PoolError;
use solana_signer::Signer;

/// USDC a merge of `sets` CASH yields at the live whirlpool, measured on a copy of the chain.
fn merge_output(env: &mut Env, sets: u64) -> u64 {
    env.quote(true, sets)
}

#[test]
fn first_deposit_mints_virtual_shares_per_token() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    let shares = env.deposit_only(&p, &alice, true, 100 * ONE);

    assert_eq!(shares, 100 * ONE * V_SHARES);
    assert_eq!(env.dangling(&p, true), 100 * ONE);
    assert_eq!(env.outcome_of(&p, &alice.pubkey(), true), 0);
    assert_eq!(env.shares_of(&p, &alice.pubkey(), true), shares);
}

#[test]
fn deposit_does_not_merge_on_its_own() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 10 * ONE);
    env.deposit_only(&p, &bob, false, 10 * ONE);

    assert_eq!(env.dangling(&p, true), 10 * ONE);
    assert_eq!(env.dangling(&p, false), 10 * ONE);
    assert_eq!(env.invested(&p), 0);
    assert_eq!(env.vanna_shares(&p), 0);
}

#[test]
fn merge_lends_exactly_the_swapped_usdc_and_leaves_the_surplus() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 100 * ONE);
    env.deposit_only(&p, &bob, false, 60 * ONE);
    let expected = merge_output(&mut env, 60 * ONE);
    let reserve_before = env.reserve_state();
    env.merge(&p).unwrap();

    assert_eq!(env.dangling(&p, true), 40 * ONE);
    assert_eq!(env.dangling(&p, false), 0);
    assert_eq!(env.balance(&p.pool_cash), 0, "all merged CASH is swapped");
    assert_eq!(env.balance(&p.pool_usdc), 0, "all swapped USDC is lent");
    let reserve = env.reserve_state();
    assert_eq!(reserve.accounted_liquidity - reserve_before.accounted_liquidity, expected);
    // Vanna mints shares 1:1 while no interest has accrued.
    assert_eq!(env.vanna_shares(&p), expected);
    assert_eq!(env.invested(&p), expected);
    // Merging moves value between buckets; the swap is the only cost, and it stays within the keeper's allowance.
    let cost = 60 * ONE - expected;
    assert!(cost <= 60 * ONE * KEEPER_LOSS_BPS as u64 / 10_000, "swap cost {cost}");
    assert_eq!(env.total_assets(&p, true), 100 * ONE - cost);
    assert_eq!(env.total_assets(&p, false), 60 * ONE - cost);
}

#[test]
fn deposit_and_merge_in_one_transaction() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit_only(&p, &alice, true, 5 * ONE);
    env.mint_outcome_tokens(&bob, &p.market, 5 * ONE);

    let keeper = env.keeper.insecure_clone();
    let route = env.route(&p, true, 5 * ONE, RouteKind::Direct);
    let ixs = [
        env.create_ata_ix(&bob.pubkey(), &p.no_share_mint),
        env.deposit_ix(&p, &bob.pubkey(), false, 5 * ONE, &bob.pubkey()),
        env.merge_ix(&p, &keeper.pubkey(), route),
    ];
    let admin = env.admin.insecure_clone();
    env.send(&admin, &ixs, &[&keeper, &bob]).unwrap();
    assert_eq!(env.complete_sets(&p), 0);
    assert!(env.invested(&p) > 0);
}

#[test]
fn merge_with_nothing_to_match_is_a_no_op() {
    let mut env = Env::new();
    let p = env.pool;
    env.merge(&p).unwrap();
    let alice = env.user();
    env.deposit(&p, &alice, true, 3 * ONE);
    assert_eq!(env.invested(&p), 0);
    assert_eq!(env.dangling(&p, true), 3 * ONE);
}

#[test]
fn deposit_is_priced_against_assets_before_it_arrived() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, carol) = (env.user(), env.user(), env.user());
    env.deposit(&p, &alice, true, 100 * ONE);
    env.deposit(&p, &bob, false, 100 * ONE);
    env.earn(5_000 * ONE, 30 * 86_400);

    let expected = env.preview_deposit(&p, true, 50 * ONE);
    let shares = env.deposit_only(&p, &carol, true, 50 * ONE);
    assert_eq!(shares, expected);
    assert!(shares < 50 * ONE * V_SHARES, "a later depositor pays for accrued interest");
}

#[test]
fn deposit_can_credit_another_account() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, receiver) = (env.user(), env.user());
    env.mint_outcome_tokens(&alice, &p.market, 7 * ONE);
    let shares = env.try_deposit_as(&p, &alice, false, 7 * ONE, &receiver.pubkey()).unwrap();

    assert_eq!(env.shares_of(&p, &receiver.pubkey(), false), shares);
    assert_eq!(env.shares_of(&p, &alice.pubkey(), false), 0);
}

#[test]
fn zero_deposit_is_rejected() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &p.market, ONE);
    assert_pool_err(env.try_deposit_as(&p, &alice, true, 0, &alice.pubkey()), PoolError::ZeroAmount);
}

#[test]
fn deposit_cannot_exceed_balance() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &p.market, ONE);
    assert_fails(env.try_deposit_as(&p, &alice, true, ONE + 1, &alice.pubkey()));
}

#[test]
fn depositing_the_other_sides_mint_is_rejected() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &p.market, ONE);
    env.ensure_ata(&alice.pubkey(), &p.yes_share_mint);
    // Claim YES but hand over the NO mint.
    let ix = env.deposit_ix(&p, &alice.pubkey(), true, ONE, &alice.pubkey());
    let ix = swap_account(ix, &p.market.yes_mint, &p.market.no_mint);
    assert_pool_err(env.send_as(&alice, &[ix]), PoolError::OutcomeMintMismatch);
}

#[test]
fn another_markets_tokens_cannot_be_deposited() {
    let mut env = Env::new();
    let p = env.pool;
    let other = env.create_market();
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &other, ONE);
    env.ensure_ata(&alice.pubkey(), &p.yes_share_mint);
    let ix = env.deposit_ix(&p, &alice.pubkey(), true, ONE, &alice.pubkey());
    let ix = swap_account(ix, &p.market.yes_mint, &other.yes_mint);
    let ix = swap_account(ix, &ata(&alice.pubkey(), &p.market.yes_mint), &ata(&alice.pubkey(), &other.yes_mint));
    assert_pool_err(env.send_as(&alice, &[ix]), PoolError::OutcomeMintMismatch);
}

#[test]
fn shares_for_one_side_cannot_be_minted_into_the_other() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &p.market, ONE);
    env.ensure_ata(&alice.pubkey(), &p.no_share_mint);
    let ix = env.deposit_ix(&p, &alice.pubkey(), true, ONE, &alice.pubkey());
    let ix = swap_account(ix, &p.yes_share_mint, &p.no_share_mint);
    let ix = swap_account(ix, &ata(&alice.pubkey(), &p.yes_share_mint), &ata(&alice.pubkey(), &p.no_share_mint));
    assert_pool_err(env.send_as(&alice, &[ix]), PoolError::OutcomeMintMismatch);
}

#[test]
fn deposit_cannot_be_priced_against_another_reserve_position() {
    // Pricing reads the pool's USDC and Vanna share accounts; pointing it at a richer account would mint cheap shares.
    let mut env = Env::new();
    let p = env.pool;
    let rich = env.create_market_and_pool();
    let (alice, bob, carol) = (env.user(), env.user(), env.user());
    env.deposit(&rich, &alice, true, 50 * ONE);
    env.deposit(&rich, &bob, false, 50 * ONE);

    env.mint_outcome_tokens(&carol, &p.market, ONE);
    env.ensure_ata(&carol.pubkey(), &p.yes_share_mint);
    for (from, to) in [(p.pool_vanna_shares, rich.pool_vanna_shares), (p.pool_usdc, rich.pool_usdc)] {
        let ix = env.deposit_ix(&p, &carol.pubkey(), true, ONE, &carol.pubkey());
        assert_pool_err(env.send_as(&carol, &[swap_account(ix, &from, &to)]), PoolError::VannaAccountMismatch);
    }
}

#[test]
fn donated_tokens_accrue_to_existing_holders() {
    // Solana has no receive hook, so a plain transfer into the pool is a donation to that side.
    let mut env = Env::new();
    let p = env.pool;
    let (alice, donor) = (env.user(), env.user());
    let shares = env.deposit_only(&p, &alice, true, 10 * ONE);
    env.mint_outcome_tokens(&donor, &p.market, 5 * ONE);
    env.transfer(&donor, &p.market.yes_mint, &p.pool, 5 * ONE).unwrap();

    let paid = env.redeem(&p, &alice, true, shares);
    assert!(paid > 10 * ONE && paid <= 15 * ONE);
}

#[test]
fn donated_usdc_accrues_to_both_sides_and_is_lent_on_the_next_merge() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob, donor) = (env.user(), env.user(), env.user());
    env.deposit(&p, &alice, true, 10 * ONE);
    env.deposit(&p, &bob, false, 10 * ONE);
    let (yes, no) = (env.total_assets(&p, true), env.total_assets(&p, false));

    env.mint_usdc(&donor.pubkey(), 2 * ONE);
    env.transfer_usdc(&donor, &p.pool_usdc, 2 * ONE).unwrap();
    assert_eq!(env.total_assets(&p, true), yes + 2 * ONE);
    assert_eq!(env.total_assets(&p, false), no + 2 * ONE);

    // The next merge supplies it to Vanna along with the swap's output.
    let carol = env.user();
    env.deposit_only(&p, &carol, true, ONE);
    let dave = env.user();
    env.deposit_only(&p, &dave, false, ONE);
    env.merge(&p).unwrap();
    assert_eq!(env.balance(&p.pool_usdc), 0);
    assert_eq!(env.reserve_state().vault_balance, env.reserve_state().accounted_liquidity);
    assert_eq!(USDC_MINT, env.token_account_info(&p.pool_usdc).0);
}
