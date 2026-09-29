//! Three World markets, three pools, one Vanna USDC reserve: the end-to-end scenario. Every pool lends into the same
//! reserve, so this is where a collision of accounts, addresses or money between pools would show.
//!
//! Throughout: pool A, B and C are the three live World markets in the snapshot, the Vanna reserve also holds a seed
//! lender's USDC, and a Vanna borrower pays the interest.

mod common;

use std::collections::HashSet;

use anchor_lang::prelude::Pubkey;
use common::*;
use outcome_yield_pool::constants::{CASH_MINT, USDC_MINT};
use outcome_yield_pool::errors::PoolError;
use solana_keypair::Keypair;
use solana_signer::Signer;

const DAY: i64 = 86_400;
const YES: u16 = 10_000;
const NO: u16 = 0;

/// All three live markets allowlisted, each with its pool. `pools[0]` is the env's default pool.
fn three_pools() -> (Env, [Pool; 3]) {
    let mut env = Env::new();
    let a = env.pool;
    let b = env.create_market_and_pool();
    let c = env.create_market_and_pool();
    (env, [a, b, c])
}

/// One holder's position in one pool.
struct Holder {
    user: Keypair,
    is_yes: bool,
    deposited: u64,
    shares: u64,
}

/// Deposits `yes` and `no` (whole tokens) from fresh users into `p`, then the keeper merges.
fn fund(env: &mut Env, p: &Pool, yes: &[u64], no: &[u64]) -> Vec<Holder> {
    let mut holders = Vec::new();
    for (is_yes, amounts) in [(true, yes), (false, no)] {
        for &amount in amounts {
            let user = env.user();
            let shares = env.deposit_only(p, &user, is_yes, amount * ONE);
            holders.push(Holder { user, is_yes, deposited: amount * ONE, shares });
        }
    }
    env.merge(p).unwrap();
    holders
}

/// The seed lender's reserve shares: everything the pools do not hold.
fn seed_shares(env: &Env, pools: &[Pool]) -> u64 {
    env.reserve_state().share_supply - pools.iter().map(|p| env.vanna_shares(p)).sum::<u64>()
}

#[test]
fn three_markets_get_three_pools_with_disjoint_accounts() {
    let (env, pools) = three_pools();
    let mut seen = HashSet::new();
    let mut markets = HashSet::new();
    for p in &pools {
        assert!(markets.insert(p.market.market), "two pools on one market");
        assert_eq!(p.pool, Env::pool_pda(&p.market.market));
        for k in [p.pool, p.yes_share_mint, p.no_share_mint, p.pool_cash, p.pool_yes, p.pool_no, p.pool_usdc, p.pool_vanna_shares] {
            assert!(seen.insert(k), "{k} belongs to more than one pool");
        }
        for account in p.token_accounts() {
            assert_eq!(env.token_account_info(&account).1, p.pool, "{account} is not owned by its pool");
        }

        let s = env.pool_state(p);
        assert_eq!(
            [s.pool_cash, s.pool_yes, s.pool_no, s.pool_usdc, s.pool_vanna_shares, s.yes_share_mint, s.no_share_mint],
            [p.pool_cash, p.pool_yes, p.pool_no, p.pool_usdc, p.pool_vanna_shares, p.yes_share_mint, p.no_share_mint]
        );
        // What the pools share is Vanna's side only: one reserve, one share mint, one liquidity vault.
        assert_eq!(s.vanna_reserve, vanna::reserve(&USDC_MINT));
        assert_eq!(s.vanna_share_mint, vanna::share_mint(&USDC_MINT));
        assert_eq!(s.vanna_liquidity_vault, vanna::liquidity_vault(&USDC_MINT));
        // Every pool's share mints answer only to that pool.
        for mint in [p.yes_share_mint, p.no_share_mint] {
            let data = env.svm.get_account(&mint).unwrap().data;
            assert_eq!(&data[4..36], p.pool.as_ref(), "{mint}'s mint authority");
        }
    }
}

#[test]
fn each_pool_lends_its_own_usdc_into_the_one_reserve() {
    let (mut env, pools) = three_pools();
    let start = env.reserve_state();
    let mut supplied = Vec::new();
    for (p, (yes, no)) in pools.iter().zip([(100, 100), (250, 80), (40, 300)]) {
        let (u, v) = (env.user(), env.user());
        env.deposit_only(p, &u, true, yes * ONE);
        env.deposit_only(p, &v, false, no * ONE);
        let expected = env.quote(true, env.complete_sets(p));
        let before = env.reserve_state();
        env.merge(p).unwrap();
        let lent = env.reserve_state().accounted_liquidity - before.accounted_liquidity;
        assert_eq!(lent, expected, "{} lent the swap's whole output", p.market.title);
        // No interest has accrued yet, so the reserve mints one share per USDC.
        assert_eq!(env.vanna_shares(p), lent);
        supplied.push(lent);
    }

    let r = env.reserve_state();
    assert_eq!(r.share_supply, start.share_supply + pools.iter().map(|p| env.vanna_shares(p)).sum::<u64>());
    assert_eq!(r.accounted_liquidity, start.accounted_liquidity + supplied.iter().sum::<u64>());
    assert_eq!(r.vault_balance, r.accounted_liquidity);
    for p in &pools {
        assert_eq!((env.balance(&p.pool_cash), env.balance(&p.pool_usdc)), (0, 0));
    }
}

#[test]
fn interest_is_shared_pro_rata_by_reserve_shares() {
    let (mut env, pools) = three_pools();
    for (p, (yes, no)) in pools.iter().zip([(100, 100), (250, 80), (40, 300)]) {
        fund(&mut env, p, &[yes], &[no]);
    }
    let seed = seed_shares(&env, &pools);
    let r0 = env.reserve_state();
    let before: Vec<u64> = pools.iter().map(|p| env.vanna_value(p)).collect();
    let seed_before = r0.value_of(seed);

    env.earn(8_000 * ONE, 45 * DAY);
    let r1 = env.reserve_state();
    let owed_to_lenders = r1.lender_total() - r0.lender_total();
    let gains: Vec<u64> = pools.iter().zip(&before).map(|(p, b)| env.vanna_value(p) - b).collect();
    let seed_gain = r1.value_of(seed) - seed_before;

    // Everyone in the reserve earns the same rate on the same share price.
    let seed_rate = seed_gain as f64 / seed_before as f64;
    for ((p, gain), b) in pools.iter().zip(&gains).zip(&before) {
        let rate = *gain as f64 / *b as f64;
        assert!((rate - seed_rate).abs() < 1e-6, "{} earned {rate}, the seed lender {seed_rate}", p.market.title);
        assert_eq!(env.vanna_value(p), r1.value_of(env.vanna_shares(p)));
    }
    // Nobody is credited interest another lender is owed: the parts add up to the whole, less rounding.
    let credited: u64 = gains.iter().sum::<u64>() + seed_gain;
    assert!(credited <= owed_to_lenders && owed_to_lenders - credited <= 4, "{credited} of {owed_to_lenders}");
}

#[test]
fn share_to_asset_and_asset_to_share_per_pool_and_side() {
    let (mut env, pools) = three_pools();
    for (p, (yes, no)) in pools.iter().zip([(100, 100), (250, 80), (40, 300)]) {
        fund(&mut env, p, &[yes], &[no]);
    }
    env.earn(8_000 * ONE, 30 * DAY);

    for p in &pools {
        for is_yes in [true, false] {
            // Asset -> share: a deposit mints exactly what the side's own totals price it at.
            let totals = (env.total_assets(p, is_yes), env.total_shares(p, is_yes));
            let expected = env.preview_deposit(p, is_yes, 10 * ONE);
            let expected_formula =
                (10 * ONE as u128 * (totals.1 as u128 + V_SHARES as u128) / (totals.0 as u128 + V_ASSETS as u128)) as u64;
            assert_eq!(expected, expected_formula);
            let user = env.user();
            let minted = env.deposit_only(p, &user, is_yes, 10 * ONE);
            assert_eq!(minted, expected, "{} {is_yes}", p.market.title);

            // Share -> asset: redeeming pays exactly what the side's totals say, and never more than was deposited.
            let owed = env.preview_redeem(p, is_yes, minted);
            let paid = env.redeem(p, &user, is_yes, minted);
            assert_eq!(paid, owed, "{} {is_yes}", p.market.title);
            assert!(paid <= 10 * ONE && paid + 2 >= 10 * ONE, "{} {is_yes}: deposited 10, got {paid}", p.market.title);
        }
    }

    // Each pool prices its sides by its own utilization, so the three indexes differ.
    let prices: Vec<(u64, u64)> = pools
        .iter()
        .map(|p| (env.preview_redeem(p, true, ONE * V_SHARES), env.preview_redeem(p, false, ONE * V_SHARES)))
        .collect();
    assert_ne!(prices[0], prices[1]);
    assert_ne!(prices[1], prices[2]);
    // Pool B is YES-heavy: its NO side is fully invested and earns more per share than its YES side.
    assert!(prices[1].1 > prices[1].0);
    // Pool C is NO-heavy: the other way round.
    assert!(prices[2].0 > prices[2].1);
}

#[test]
fn activity_in_one_pool_never_moves_another_pools_backing() {
    let (mut env, pools) = three_pools();
    let mut a_holders = Vec::new();
    for (i, (p, (yes, no))) in pools.iter().zip([(100, 100), (250, 80), (40, 300)]).enumerate() {
        let h = fund(&mut env, p, &[yes], &[no]);
        if i == 0 {
            a_holders = h;
        }
    }
    env.earn(8_000 * ONE, 30 * DAY);

    let snapshot = |env: &Env| -> Vec<[u64; 6]> {
        pools[1..]
            .iter()
            .map(|p| {
                [
                    env.vanna_shares(p),
                    env.dangling(p, true),
                    env.dangling(p, false),
                    env.balance(&p.pool_usdc),
                    env.total_assets(p, true),
                    env.total_assets(p, false),
                ]
            })
            .collect()
    };
    let before = snapshot(&env);

    // Everything pool A can do while the clock stands still: deposits, a merge, redemptions that redeem from Vanna
    // and swap, and a USDC donation.
    let a = pools[0];
    fund(&mut env, &a, &[33], &[77]);
    for h in &a_holders {
        let owner = h.user.pubkey();
        env.try_redeem_as(&a, &h.user, &owner, h.is_yes, h.shares / 2, &owner).unwrap();
    }
    let donor = env.user();
    env.mint_usdc(&donor.pubkey(), 3 * ONE);
    env.transfer_usdc(&donor, &a.pool_usdc, 3 * ONE).unwrap();

    let after = snapshot(&env);
    for (i, (b, x)) in before.iter().zip(&after).enumerate() {
        let title = pools[i + 1].market.title;
        assert_eq!(b[..4], x[..4], "{title}: its shares, tokens or USDC moved");
        // Vanna rounds each supply and redemption in favour of the remaining lenders, so another pool's value can only
        // gain dust.
        for side in 4..6 {
            assert!(x[side] >= b[side] && x[side] - b[side] <= 3, "{title}: {} -> {}", b[side], x[side]);
        }
    }
}

#[test]
fn the_same_user_in_three_pools_holds_three_separate_positions() {
    let (mut env, pools) = three_pools();
    let alice = env.user();
    let mut shares = Vec::new();
    for p in &pools {
        shares.push(env.deposit_only(p, &alice, true, 10 * ONE));
        let bob = env.user();
        env.deposit_only(p, &bob, false, 10 * ONE);
        env.merge(p).unwrap();
    }
    let accounts: HashSet<Pubkey> = pools.iter().map(|p| ata(&alice.pubkey(), &p.yes_share_mint)).collect();
    assert_eq!(accounts.len(), 3);

    env.redeem(&pools[1], &alice, true, shares[1]);
    assert_eq!(env.shares_of(&pools[0], &alice.pubkey(), true), shares[0]);
    assert_eq!(env.shares_of(&pools[1], &alice.pubkey(), true), 0);
    assert_eq!(env.shares_of(&pools[2], &alice.pubkey(), true), shares[2]);
}

#[test]
fn one_market_resolving_leaves_the_others_untouched() {
    let (mut env, pools) = three_pools();
    let mut holders = Vec::new();
    for (p, (yes, no)) in pools.iter().zip([(100, 60), (50, 50), (20, 90)]) {
        holders.push(fund(&mut env, p, &[yes], &[no]));
    }
    env.earn(8_000 * ONE, 30 * DAY);
    let others = |env: &Env| -> Vec<(u64, u64, u64)> {
        pools[1..].iter().map(|p| (env.vanna_shares(p), env.dangling(p, true), env.dangling(p, false))).collect()
    };
    let before = others(&env);

    // Market A resolves YES and its winners take their USDC out of the shared reserve. Alice holds A tokens from
    // before, to try depositing afterwards.
    let a = pools[0];
    let alice = env.user();
    env.mint_outcome_tokens(&alice, &a.market, ONE);
    env.determine_outcome(&a.market, YES).unwrap();
    env.settle(&a).unwrap();
    for h in &holders[0] {
        let (tokens, cash, usdc) = env.redeem_settled(&a, &h.user, h.is_yes, h.shares);
        if h.is_yes {
            assert!(tokens + usdc > h.deposited - h.deposited * KEEPER_LOSS_BPS as u64 / 10_000);
        } else {
            assert_eq!((cash, usdc), (0, 0), "the losing side is owed nothing of the reserve");
        }
    }
    assert_eq!(others(&env), before, "B and C kept every share and token");

    // B and C carry on as open markets.
    for (p, hs) in pools[1..].iter().zip(&holders[1..]) {
        assert_eq!(env.pool_state(p).status, PoolStatus::Open);
        fund(&mut env, p, &[5], &[5]);
        let h = &hs[0];
        let owner = h.user.pubkey();
        env.try_redeem_as(p, &h.user, &owner, h.is_yes, h.shares, &owner).unwrap();
    }
    assert_pool_err(env.try_deposit_as(&a, &alice, false, ONE, &alice.pubkey()), PoolError::PoolSettled);
}

#[test]
fn full_lifecycle_three_markets_three_outcomes_everyone_exits() {
    let (mut env, pools) = three_pools();
    let [a, b, c] = pools;
    let mut holders = vec![
        fund(&mut env, &a, &[70, 30], &[50, 40]),
        fund(&mut env, &b, &[120], &[20, 60, 15]),
        fund(&mut env, &c, &[10, 10], &[200]),
    ];
    let seed = seed_shares(&env, &pools);

    // Interest, then some coming and going while the markets are open.
    env.earn(6_000 * ONE, 20 * DAY);
    {
        let h = &mut holders[0][0];
        let owner = h.user.pubkey();
        let half = h.shares / 2;
        env.try_redeem_as(&a, &h.user, &owner, true, half, &owner).unwrap();
        h.shares -= half;
    }
    {
        let h = &mut holders[1][2];
        let owner = h.user.pubkey();
        env.try_redeem_as(&b, &h.user, &owner, false, h.shares, &owner).unwrap();
        h.shares = 0;
    }
    holders[2].extend(fund(&mut env, &c, &[50], &[]));
    env.warp(25 * DAY);
    env.repay_all();

    // A: YES wins. B: NO wins, and World pays the pool for its winning tokens before anyone redeems. C: YES wins, and
    // World winds down and closes the market before anyone settles, so the resolver does.
    env.determine_outcome(&a.market, YES).unwrap();
    env.settle(&a).unwrap();
    env.determine_outcome(&b.market, NO).unwrap();
    if env.balance(&b.pool_no) > 0 {
        env.redeem_for_user(&b.market, &b.market.no_mint, &b.pool).unwrap();
    }
    env.settle(&b).unwrap();
    env.determine_outcome(&c.market, YES).unwrap();
    let c_wallets: Vec<Pubkey> = holders[2].iter().map(|h| h.user.pubkey()).chain([c.pool]).collect();
    env.wind_down(&c.market, true, &c_wallets);
    env.close_market(&c.market).unwrap();
    let admin = env.admin.insecure_clone();
    let ix = env.settle_by_resolver_ix(&c, &admin.pubkey(), true);
    env.send_as(&admin, &[ix]).unwrap();

    for ((p, hs), yes_won) in pools.iter().zip(&holders).zip([true, false, true]) {
        let owed = env.total_assets(p, yes_won) + env.balance(&p.pool_cash);
        let mut paid = 0;
        for h in hs.iter().filter(|h| h.shares > 0) {
            let (tokens, cash, usdc) = env.redeem_settled(p, &h.user, h.is_yes, h.shares);
            if h.is_yes == yes_won {
                paid += tokens + cash + usdc;
            } else {
                assert_eq!((cash, usdc), (0, 0), "{}: a loser was paid", p.market.title);
            }
        }
        // Winners took the whole winning side, less a unit of rounding per holder.
        assert!(paid <= owed && owed - paid <= hs.len() as u64 + 2, "{}: paid {paid} of {owed}", p.market.title);
        assert_eq!(env.total_shares(p, true) + env.total_shares(p, false), 0, "{}", p.market.title);
        assert!(env.vanna_value(p) <= 3 && env.balance(&p.pool_usdc) <= 3, "{} kept more than dust", p.market.title);
    }

    // With every pool out, the seed lender still redeems exactly its own share of the reserve.
    let r = env.reserve_state();
    let seed_value = r.value_of(seed);
    let pools_left: u64 = pools.iter().map(|p| env.vanna_value(p)).sum();
    assert!(r.lender_total() - seed_value - pools_left <= 4, "the reserve owes someone unaccounted");
    assert!(r.vault_balance >= r.accounted_liquidity);
}

#[test]
fn cross_pool_substitution_is_rejected_in_every_instruction() {
    let (mut env, pools) = three_pools();
    let [a, b, _] = pools;
    let ha = fund(&mut env, &a, &[40], &[30]);
    fund(&mut env, &b, &[25], &[45]);
    env.earn(5_000 * ONE, 10 * DAY);
    let b_before: Vec<u64> = b.token_accounts().iter().map(|k| env.balance(k)).collect();
    let yes = &ha[0];
    let owner = yes.user.pubkey();

    // deposit
    let carol = env.user();
    env.mint_outcome_tokens(&carol, &a.market, ONE);
    env.ensure_ata(&carol.pubkey(), &a.yes_share_mint);
    for (from, to) in [
        (a.pool_yes, b.pool_yes),
        (a.market.market, b.market.market),
        (a.pool_usdc, b.pool_usdc),
        (a.pool_vanna_shares, b.pool_vanna_shares),
    ] {
        let ix = env.deposit_ix(&a, &carol.pubkey(), true, ONE, &carol.pubkey());
        assert_fails(env.send_as(&carol, &[swap_account(ix, &from, &to)]));
    }

    // merge
    let dave = env.user();
    env.deposit_only(&a, &dave, false, 5 * ONE);
    let keeper = env.keeper.insecure_clone();
    for (from, to) in [
        (a.market.market, b.market.market),
        (a.pool_cash, b.pool_cash),
        (a.pool_yes, b.pool_yes),
        (a.pool_no, b.pool_no),
        (a.pool_usdc, b.pool_usdc),
        (a.pool_vanna_shares, b.pool_vanna_shares),
    ] {
        let route = env.merge_route(&a, RouteKind::Direct);
        let ix = env.merge_ix(&a, &keeper.pubkey(), route);
        assert_fails(env.send_as(&keeper, &[swap_account(ix, &from, &to)]));
    }

    // redeem, with the swap it needs
    for (from, to) in [
        (a.pool_yes, b.pool_yes),
        (a.pool_no, b.pool_no),
        (a.pool_cash, b.pool_cash),
        (a.pool_usdc, b.pool_usdc),
        (a.pool_vanna_shares, b.pool_vanna_shares),
        (a.market.market, b.market.market),
    ] {
        let route = env.redeem_route(&a, true, yes.shares, RouteKind::Direct);
        let ix = env.redeem_ix(&a, &owner, &owner, true, yes.shares, &owner, route);
        assert_fails(env.send_as(&yes.user, &[swap_account(ix, &from, &to)]));
    }

    // redeem_settled
    for p in [&a, &b] {
        env.determine_outcome(&p.market, YES).unwrap();
        env.settle(p).unwrap();
    }
    let b_settled: Vec<u64> = b.token_accounts().iter().map(|k| env.balance(k)).collect();
    env.ensure_spl_ata(&owner, &USDC_MINT);
    env.ensure_ata(&owner, &CASH_MINT);
    for (from, to) in [
        (a.pool_yes, b.pool_yes),
        (a.pool_cash, b.pool_cash),
        (a.pool_usdc, b.pool_usdc),
        (a.pool_vanna_shares, b.pool_vanna_shares),
    ] {
        let ix = env.redeem_settled_ix(&a, &owner, &owner, true, yes.shares, &owner);
        assert_fails(env.send_as(&yes.user, &[swap_account(ix, &from, &to)]));
    }
    assert_eq!(b.token_accounts().iter().map(|k| env.balance(k)).collect::<Vec<_>>(), b_settled);
    assert_eq!(b_before.len(), b_settled.len());

    // And the honest calls still go through.
    let (tokens, _, usdc) = env.redeem_settled(&a, &yes.user, true, yes.shares);
    assert!(tokens + usdc >= yes.deposited - yes.deposited * KEEPER_LOSS_BPS as u64 / 10_000);
}
