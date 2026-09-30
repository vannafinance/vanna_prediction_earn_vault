//! Full-lifecycle property run: random open-market activity over three pools sharing Vanna's reserve, then every market
//! resolves and every holder exits. World's operator pays the pool out before `settle`, after it, only after the
//! holders have left, or closes the market so the resolver settles; which one is random. Checks:
//!
//! - `wind_down_is_neutral`: World paying the pool CASH for its winning tokens (and burning its losing ones) leaves the
//!   winning side's settled assets unchanged.
//! - `early_exit_is_neutral`: a `redeem` between `determine_outcome` and `settle` never lowers what a winning share is
//!   worth.
//! - `merges_stop`: once World has written the outcome, a merge with complete sets fails.
//! - `payout_is_exact`: every settled redemption pays exactly `floor(shares * (T + 1) / (S + 1000))` of the side's
//!   settled assets `T`: winners in tokens, then CASH, then USDC; losers in their own tokens only.
//! - `payout_is_pro_rata`: each holder's total equals its share of the side as it stood at settlement, within rounding.
//!   Every redemption rounds down, so the units it leaves behind go to whoever redeems later.
//! - `side_pays_what_it_holds`: a side's holders together receive at most its settled assets, and all of it but what
//!   the 1000 virtual shares are worth (`ceil(1000 * (T + 1) / (S + 1000))`, about one raw unit per unit of share
//!   price) and a few units of Vanna rounding.
//! - `nothing_is_stranded`: once every share of a side is redeemed, the pool keeps no more than that.
//! - `reserve_reconciles`: Vanna's books add up after the pools withdraw.
//!
//! The fixed seeds must between them reach every path above (`Coverage`), so the checks are known to have run.
//! `cargo test -p outcome_yield_pool --test lifecycle -- --ignored` runs `LIFECYCLE_SEEDS` more (default 64).

mod common;

use common::*;
use outcome_yield_pool::predict::{OUTCOME_NO, OUTCOME_YES};
use solana_keypair::Keypair;
use solana_signer::Signer;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// How often each path was taken, so a seed set that never reaches one fails instead of passing vacuously.
#[derive(Default, Debug)]
struct Coverage {
    holders_exit_before_payout: u32,
    settled_before_payout: u32,
    paid_out_before_settle: u32,
    resolver_settled: u32,
    early_exits: u32,
    winner_token_payouts: u32,
    winner_cash_payouts: u32,
    winner_usdc_payouts: u32,
    loser_token_payouts: u32,
}

impl Coverage {
    fn add(&mut self, o: &Coverage) {
        self.holders_exit_before_payout += o.holders_exit_before_payout;
        self.settled_before_payout += o.settled_before_payout;
        self.paid_out_before_settle += o.paid_out_before_settle;
        self.resolver_settled += o.resolver_settled;
        self.early_exits += o.early_exits;
        self.winner_token_payouts += o.winner_token_payouts;
        self.winner_cash_payouts += o.winner_cash_payouts;
        self.winner_usdc_payouts += o.winner_usdc_payouts;
        self.loser_token_payouts += o.loser_token_payouts;
    }

    fn assert_complete(&self) {
        let reached = [
            self.holders_exit_before_payout,
            self.settled_before_payout,
            self.paid_out_before_settle,
            self.resolver_settled,
            self.early_exits,
            self.winner_token_payouts,
            self.winner_cash_payouts,
            self.winner_usdc_payouts,
            self.loser_token_payouts,
        ];
        assert!(reached.iter().all(|&n| n > 0), "a path was never taken: {self:?}");
    }
}

/// What `redeem_settled` prices a side against: winners own their tokens, the pool's CASH and the invested balance;
/// losers own their tokens.
fn settled_total(env: &Env, p: &Pool, is_yes: bool, yes_won: bool) -> u64 {
    let tokens = env.dangling(p, is_yes);
    if is_yes == yes_won {
        tokens + env.balance(&p.pool_cash) + env.invested(p)
    } else {
        tokens
    }
}

/// `(T + 1) / (S + 1000)` as a comparable fraction.
fn share_price(env: &Env, p: &Pool, is_yes: bool, yes_won: bool) -> (u128, u128) {
    (settled_total(env, p, is_yes, yes_won) as u128 + 1, env.total_shares(p, is_yes) as u128 + 1_000)
}

fn open_market(env: &mut Env, pools: &[Pool], users: &[Keypair], rng: &mut Rng, steps: usize) {
    for _ in 0..steps {
        let p = pools[rng.below(pools.len() as u64) as usize];
        let user = &users[rng.below(users.len() as u64) as usize];
        let is_yes = rng.below(2) == 0;
        match rng.below(7) {
            0..=2 => {
                let amount = ONE + rng.below(40 * ONE);
                env.mint_outcome_tokens(user, &p.market, amount);
                let to = user.pubkey();
                let _ = env.try_deposit_as(&p, user, is_yes, amount, &to);
            }
            3 => {
                let _ = env.merge(&p);
            }
            4 => {
                let held = env.shares_of(&p, &user.pubkey(), is_yes);
                if held > 0 {
                    let owner = user.pubkey();
                    let _ = env.try_redeem_as(&p, user, &owner, is_yes, 1 + rng.below(held), &owner);
                }
            }
            5 => {
                let r = env.reserve_state();
                let free = r.accounted_liquidity.saturating_sub(r.accrued_fees);
                if free > 3 * ONE {
                    env.borrow(1 + rng.below(free / 3));
                }
                env.warp(3_600 + rng.below(10 * 86_400) as i64);
            }
            _ => {
                let held = env.shares_of(&p, &user.pubkey(), is_yes);
                let to = users[rng.below(users.len() as u64) as usize].pubkey();
                if held > 0 && to != user.pubkey() {
                    env.transfer(user, &p.share_mint(is_yes), &to, 1 + rng.below(held)).unwrap();
                }
            }
        }
    }
}

fn resolve(env: &mut Env, p: &Pool, users: &[Keypair], rng: &mut Rng, cov: &mut Coverage) -> bool {
    let yes_won = rng.below(2) == 0;
    env.determine_outcome(&p.market, if yes_won { OUTCOME_YES } else { OUTCOME_NO }).unwrap();
    if env.complete_sets(p) > 0 {
        assert!(env.merge(p).is_err(), "merges_stop");
    }

    // Someone exits through the open-market path before anyone settles. Covered by dangling tokens it may succeed;
    // needing a split it fails, since World refuses splits now. Either way winners must not lose by it.
    let user = &users[rng.below(users.len() as u64) as usize];
    let side = rng.below(2) == 0;
    let held = env.shares_of(p, &user.pubkey(), side);
    if held > 0 {
        let (t0, s0) = share_price(env, p, yes_won, yes_won);
        let owner = user.pubkey();
        if env.try_redeem_as(p, user, &owner, side, 1 + rng.below(held), &owner).is_ok_and(|paid| paid > 0) {
            cov.early_exits += 1;
        }
        let (t1, s1) = share_price(env, p, yes_won, yes_won);
        assert!(t1 * s0 >= t0 * s1, "early_exit_is_neutral: {t0}/{s0} -> {t1}/{s1}");
    }

    let wallets: Vec<_> = users.iter().map(|u| u.pubkey()).chain([p.pool]).collect();
    let wind_down = |env: &mut Env| {
        let before = settled_total(env, p, yes_won, yes_won);
        env.wind_down(&p.market, yes_won, &wallets);
        assert_eq!(settled_total(env, p, yes_won, yes_won), before, "wind_down_is_neutral");
        assert_eq!(env.dangling(p, !yes_won), 0, "World burns the pool's losing tokens");
    };
    match rng.below(4) {
        // World pays out later: holders exit while the pool still holds the outcome tokens.
        0 => {
            env.settle(p).unwrap();
            cov.holders_exit_before_payout += 1;
        }
        1 => {
            env.settle(p).unwrap();
            wind_down(env);
            cov.settled_before_payout += 1;
        }
        2 => {
            wind_down(env);
            env.settle(p).unwrap();
            cov.paid_out_before_settle += 1;
        }
        _ => {
            wind_down(env);
            cov.resolver_settled += 1;
            env.close_market(&p.market).unwrap();
            let resolver = env.admin.insecure_clone();
            let ix = env.settle_by_resolver_ix(p, &resolver.pubkey(), yes_won);
            env.send_as(&resolver, &[ix]).unwrap();
        }
    }
    yes_won
}

#[allow(clippy::too_many_arguments)]
fn exit_side(
    env: &mut Env,
    p: &Pool,
    users: &[Keypair],
    rng: &mut Rng,
    cov: &mut Coverage,
    is_yes: bool,
    yes_won: bool,
) {
    let won = is_yes == yes_won;
    let supply = env.total_shares(p, is_yes);
    let total = settled_total(env, p, is_yes, yes_won);
    // What the virtual shares' slice of the side is worth: it stays in the pool when the last holder leaves.
    let virtual_slice = (1_000 * (total as u128 + 1)).div_ceil(supply as u128 + 1_000) as u64;
    let dust = virtual_slice + 4;
    let mut paid_side = 0;
    let mut order: Vec<usize> = (0..users.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    for i in order {
        let user = &users[i];
        let held = env.shares_of(p, &user.pubkey(), is_yes);
        if held == 0 {
            continue;
        }
        let first = if held > 1 && rng.below(2) == 0 { 1 + rng.below(held - 1) } else { held };
        let mut paid = 0;
        for chunk in [first, held - first] {
            if chunk == 0 {
                continue;
            }
            let (t, s) = (settled_total(env, p, is_yes, yes_won) as u128, env.total_shares(p, is_yes) as u128);
            let expected = (chunk as u128 * (t + 1) / (s + 1_000)) as u64;
            let (tokens, cash, usdc) = env.redeem_settled(p, user, is_yes, chunk);
            assert_eq!(tokens + cash + usdc, expected, "payout_is_exact");
            if won {
                cov.winner_token_payouts += u32::from(tokens > 0);
                cov.winner_cash_payouts += u32::from(cash > 0);
                cov.winner_usdc_payouts += u32::from(usdc > 0);
            } else {
                assert_eq!(cash + usdc, 0, "losers are paid only in their own tokens");
                cov.loser_token_payouts += u32::from(tokens > 0);
            }
            paid += tokens + cash + usdc;
        }
        let fair = (held as u128 * total as u128 / supply as u128) as u64;
        assert!(paid.abs_diff(fair) <= dust, "payout_is_pro_rata: paid {paid}, fair {fair}, dust {dust}");
        paid_side += paid;
    }
    assert_eq!(env.total_shares(p, is_yes), 0);
    assert!(paid_side <= total, "side_pays_what_it_holds: paid {paid_side} of {total}");
    assert!(total - paid_side <= dust, "side_pays_what_it_holds: paid {paid_side} of {total}, dust {dust}");
    let left = settled_total(env, p, is_yes, yes_won);
    assert!(left <= dust, "nothing_is_stranded: {left} left of {total}, dust {dust}");
}

fn run(seed: u64) -> Coverage {
    let mut cov = Coverage::default();
    let mut env = Env::new();
    let pools = [env.pool, env.create_market_and_pool(), env.create_market_and_pool()];
    let users: Vec<Keypair> = (0..5).map(|_| env.user()).collect();
    let seed_shares = env.reserve_state().share_supply;
    let mut rng = Rng(seed);

    open_market(&mut env, &pools, &users, &mut rng, 120);
    let outcomes: Vec<bool> = pools.iter().map(|p| resolve(&mut env, p, &users, &mut rng, &mut cov)).collect();

    // Interest keeps accruing to the winners after settlement, until the borrower repays and frees the liquidity.
    env.warp(7 * 86_400);
    if env.reserve_state().total_borrow > 0 {
        env.repay_all();
    }
    for (p, &yes_won) in pools.iter().zip(&outcomes) {
        for is_yes in [yes_won, !yes_won] {
            exit_side(&mut env, p, &users, &mut rng, &mut cov, is_yes, yes_won);
        }
    }

    let reserve = env.reserve_state();
    let pool_shares: u64 = pools.iter().map(|p| env.vanna_shares(p)).sum();
    assert_eq!(pool_shares + seed_shares, reserve.share_supply, "reserve_reconciles: shares");
    assert!(reserve.value_of(seed_shares) <= reserve.lender_total(), "reserve_reconciles: value");
    assert!(reserve.vault_balance >= reserve.accounted_liquidity, "reserve_reconciles: vault");
    cov
}

const SEEDS: [u64; 6] = [
    0x9E37_79B9_7F4A_7C15,
    0xD1B5_4A32_D192_ED03,
    0x2545_F491_4F6C_DD1D,
    0x5851_F42D_4C95_7F2D,
    0xA076_1D64_78BD_642F,
    0xE703_7ED1_A0B4_28DB,
];

#[test]
fn lifecycle_holds() {
    let mut cov = Coverage::default();
    for seed in SEEDS {
        cov.add(&run(seed));
    }
    cov.assert_complete();
}

#[test]
#[ignore = "long: set LIFECYCLE_SEEDS and run with --ignored"]
fn lifecycle_holds_many_seeds() {
    let count: u64 = std::env::var("LIFECYCLE_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(64);
    let mut seeds = Rng(0x0123_4567_89AB_CDEF);
    let mut cov = Coverage::default();
    for _ in 0..count {
        cov.add(&run(seeds.next()));
    }
    cov.assert_complete();
}
