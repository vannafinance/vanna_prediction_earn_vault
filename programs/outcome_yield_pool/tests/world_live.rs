//! The pool's assumptions about the programs it calls, checked against their real bytecode: World's deployed prediCt
//! and its three live markets (plus two markets World had already resolved on mainnet), the live CASH/USDC route
//! through Jupiter and Orca, and Vanna's lending program.

mod common;

use anchor_lang::prelude::{AccountMeta, Clock};
use common::*;
use outcome_yield_pool::constants::{CASH_MINT, JUPITER_PROGRAM_ID, PREDICT_PROGRAM_ID, USDC_MINT};
use outcome_yield_pool::predict::{MARKET_DETERMINED_SLOT_OFFSET, MARKET_OUTCOME_OFFSET};
use solana_signer::Signer;

const YES: u16 = 10_000;
const NO: u16 = 0;
const CREATE_USER_SETTINGS_IDEMPOTENT: [u8; 8] = [0x8e, 0x64, 0x3f, 0xfe, 0xcb, 0x32, 0x43, 0x61];
const UPDATE_USER_SETTINGS_V2: [u8; 8] = [0x1c, 0xcb, 0xdc, 0x67, 0x77, 0x98, 0x5d, 0x72];

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
}

#[test]
fn live_split_and_merge_are_fee_free_at_par_on_every_market() {
    let mut env = Env::bare();
    for m in env.markets.clone() {
        let alice = env.user();
        let owner = alice.pubkey();
        let (vault_before, yes_supply_before) = (env.balance(&m.cash_vault), env.supply(&m.yes_mint));

        env.mint_outcome_tokens(&alice, &m, 25 * ONE);
        assert_eq!(env.balance(&ata(&owner, &m.yes_mint)), 25 * ONE, "{}", m.title);
        assert_eq!(env.balance(&ata(&owner, &m.no_mint)), 25 * ONE, "{}", m.title);
        assert_eq!(env.cash_of(&owner), 0, "{}", m.title);
        assert_eq!(env.balance(&m.cash_vault), vault_before + 25 * ONE, "{}", m.title);
        assert_eq!(env.supply(&m.yes_mint), yes_supply_before + 25 * ONE, "{}", m.title);

        let ix = Env::set_ix(
            false,
            &owner,
            &m,
            ata(&owner, &CASH_MINT),
            ata(&owner, &m.yes_mint),
            ata(&owner, &m.no_mint),
            10 * ONE,
        );
        env.send_as(&alice, &[ix]).unwrap();
        assert_eq!(env.cash_of(&owner), 10 * ONE, "{}", m.title);
        assert_eq!(env.balance(&m.cash_vault), vault_before + 15 * ONE, "{}", m.title);
    }
}

#[test]
fn determine_outcome_writes_the_slot_and_outcome_the_pool_reads() {
    for (outcome, yes_won) in [(YES, true), (NO, false)] {
        let mut env = Env::new();
        let p = env.pool;
        let before = env.svm.get_account(&p.market.market).unwrap().data;
        assert_eq!(u64_at(&before, MARKET_DETERMINED_SLOT_OFFSET), 0, "open markets carry no determination slot");

        env.determine_outcome(&p.market, outcome).unwrap();
        let after = env.svm.get_account(&p.market.market).unwrap().data;
        let clock: Clock = env.svm.get_sysvar();
        assert_eq!(u64_at(&after, MARKET_DETERMINED_SLOT_OFFSET), clock.slot);
        assert_eq!(u16_at(&after, MARKET_OUTCOME_OFFSET), outcome);

        env.settle(&p).unwrap();
        assert_eq!(env.pool_state(&p).status, PoolStatus::Settled { yes_won });
    }
}

#[test]
fn live_split_is_closed_once_the_market_is_determined() {
    let mut env = Env::bare();
    let m = env.markets[0];
    env.determine_outcome(&m, YES).unwrap();

    let alice = env.user();
    let owner = alice.pubkey();
    let cash = env.mint_cash(&owner, ONE);
    let yes = env.ensure_ata(&owner, &m.yes_mint);
    let no = env.ensure_ata(&owner, &m.no_mint);
    let ix = Env::set_ix(true, &owner, &m, cash, yes, no, ONE);
    // prediCt's `MarketAlreadyDetermined`.
    assert_custom(env.send_as(&alice, &[ix]), 6000);
}

#[test]
fn markets_resolved_on_mainnet_settle_the_pool() {
    let mut env = Env::bare();
    env.initialize_config();
    for (market, outcome) in env.world.resolved.clone() {
        env.allow_market(&market).unwrap();
        let p = env.create_pool(&market).unwrap();
        env.settle(&p).unwrap();
        assert_eq!(env.pool_state(&p).status, PoolStatus::Settled { yes_won: outcome == YES }, "{}", market.market);
    }
}

/// prediCt closes a holder's outcome account only to fund a missing payout CASH account. The pool's CASH account
/// exists from `create_pool`, so World just burns the pool's winning tokens and pays the pool's own CASH account.
#[test]
fn operator_payout_burns_the_pools_winning_tokens_into_its_cash_account() {
    let mut env = Env::new();
    let p = env.pool;
    let alice = env.user();
    env.deposit_only(&p, &alice, true, 40 * ONE);
    env.determine_outcome(&p.market, YES).unwrap();

    assert_eq!(env.payout_wallet(&p.pool), p.pool, "nothing redirects the pool's payout");
    env.redeem_for_user(&p.market, &p.market.yes_mint, &p.pool).unwrap();
    assert!(env.account_exists(&p.pool_yes));
    assert_eq!(env.balance(&p.pool_yes), 0);
    assert_eq!(env.balance(&p.pool_cash), 40 * ONE);

    env.settle(&p).unwrap();
    let shares = env.shares_of(&p, &alice.pubkey(), true);
    assert_eq!(env.redeem_settled(&p, &alice, true, shares), (0, 40 * ONE, 0));
}

#[test]
fn nobody_can_set_up_payout_redirection_for_the_pool() {
    let mut env = Env::new();
    let p = env.pool;
    let attacker = env.user();
    let settings = Env::user_settings_pda(&p.pool);

    let create = predict_ix(
        CREATE_USER_SETTINGS_IDEMPOTENT,
        vec![
            AccountMeta::new(p.pool, false),
            AccountMeta::new(attacker.pubkey(), true),
            AccountMeta::new(settings, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        &[],
    );
    assert_fails(env.send_as(&attacker, &[create]));

    let mut args = vec![0u8, 1u8];
    args.extend_from_slice(attacker.pubkey().as_ref());
    let update = predict_ix(
        UPDATE_USER_SETTINGS_V2,
        vec![AccountMeta::new(p.pool, false), AccountMeta::new(settings, false)],
        &args,
    );
    assert_fails(env.send_as(&attacker, &[update]));

    assert!(!env.account_exists(&settings));
    assert_eq!(env.payout_wallet(&p.pool), p.pool);
}

#[test]
fn a_real_wind_down_pays_every_holder_and_lets_world_close_the_market() {
    let mut env = Env::new();
    let p = env.pool;
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 30 * ONE);
    env.deposit(&p, &bob, false, 20 * ONE);
    env.determine_outcome(&p.market, NO).unwrap();
    env.settle(&p).unwrap();

    let vault_cash = env.balance(&p.market.cash_vault);
    let no_supply = env.supply(&p.market.no_mint);
    assert_eq!(vault_cash, no_supply, "the market vault backs every winning token 1:1");

    env.wind_down(&p.market, false, &[alice.pubkey(), bob.pubkey(), p.pool]);
    assert_eq!(env.supply(&p.market.yes_mint), 0);
    assert_eq!(env.supply(&p.market.no_mint), 0);
    assert_eq!(env.balance(&p.market.cash_vault), 0);

    env.close_market(&p.market).unwrap();
    for gone in [p.market.market, p.market.yes_mint, p.market.no_mint, p.market.cash_vault] {
        assert!(!env.account_exists(&gone), "{gone} survived close_market");
    }
    assert!(env.svm.get_account(&p.market.market).is_none_or(|a| a.owner != PREDICT_PROGRAM_ID || a.data.is_empty()));

    // Bob's NO shares still pay out after World deleted everything: his side's value sits in Vanna as USDC.
    let invested = env.invested(&p);
    let shares = env.shares_of(&p, &bob.pubkey(), false);
    let (tokens, cash, usdc) = env.redeem_settled(&p, &bob, false, shares);
    assert_eq!((tokens, cash), (0, 0));
    assert!(usdc.abs_diff(invested) <= 1 && usdc > 20 * ONE - 20 * ONE * KEEPER_LOSS_BPS as u64 / 10_000, "bob got {usdc}");
}

#[test]
fn live_whirlpool_swaps_cash_and_usdc_both_ways_near_par() {
    let mut env = Env::bare();
    let sell = env.quote(true, 1_000 * ONE);
    let buy = env.quote(false, 1_000 * ONE);
    // A stable pair: selling CASH pays its fee, buying CASH with USDC lands at or above par. The pool's merges rely on
    // the first staying inside the keeper's allowance and its redemptions on the second staying at par or better.
    assert!(sell < 1_000 * ONE && 1_000 * ONE - sell <= 1_000 * ONE * KEEPER_LOSS_BPS as u64 / 10_000, "sold for {sell}");
    assert!(buy >= 1_000 * ONE, "bought {buy}");
}

#[test]
fn jupiter_and_the_whirlpool_program_are_the_snapshotted_ones() {
    let env = Env::bare();
    for program in [JUPITER_PROGRAM_ID, WHIRLPOOL_PROGRAM, PREDICT_PROGRAM_ID, vanna::PROGRAM] {
        assert!(env.svm.get_account(&program).is_some_and(|a| a.executable), "{program} missing");
    }
}

#[test]
fn vanna_reserve_layout_matches_what_the_pool_reads() {
    // The pool values its position from raw Reserve bytes. A lender's actual redemption must pay exactly that value.
    let mut env = Env::bare();
    env.earn(3_000 * ONE, 20 * 86_400);
    let lender = env.user();
    env.vanna_supply(&lender, 500 * ONE).unwrap();
    env.warp(86_400);
    let share_account = spl_ata(&lender.pubkey(), &vanna::share_mint(&USDC_MINT));
    let shares = env.balance(&share_account);
    let expected = env.reserve_state().value_of(shares);
    let before = env.usdc_of(&lender.pubkey());
    env.send_as(&lender, &[vanna::lender_redeem(&lender.pubkey(), &USDC_MINT, shares)]).unwrap();
    assert_eq!(env.usdc_of(&lender.pubkey()) - before, expected);
    assert!(expected > 500 * ONE, "interest accrued: {expected}");
}

#[test]
fn a_fresh_reserve_mints_its_first_shares_one_to_one() {
    // With no seed lender the first pool to merge is Vanna's first lender, which must clear its minimum first deposit.
    let mut env = Env::without_seed_lender();
    env.initialize_config();
    let p = env.create_market_and_pool();
    let (alice, bob) = (env.user(), env.user());
    env.deposit(&p, &alice, true, 2 * ONE);
    env.deposit(&p, &bob, false, 2 * ONE);
    assert_eq!(env.reserve_state().share_supply, env.vanna_shares(&p));
    assert_eq!(env.vanna_value(&p), env.vanna_shares(&p));
}
