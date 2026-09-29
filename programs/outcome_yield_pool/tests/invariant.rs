//! Stateful property run over random operation sequences.
//!
//! Drives all three pools, which share Vanna's USDC reserve, with a fixed-seed PRNG: deposits, keeper merges (real
//! Jupiter swaps and Vanna supplies), redemptions (Vanna redemptions, swaps back and splits), Vanna interest from a
//! real borrower, and share transfers. After every step:
//!
//! - `share_conservation`: tracked holders own exactly each side's share supply.
//! - `yes_no_difference_conservation`: merge, split and the surplus pairs of a swap move YES and NO together, so a
//!   pool's `yes - no` token balance changes only by deposits and redemptions. Exact, no rounding.
//! - `side_backing_covers_flows`: a side's assets cover everything deposited minus everything redeemed minus what the
//!   keeper's merges lost to the swap, up to a unit of Vanna rounding per hop and `SWAP_DUST_UNITS` per redemption
//!   swap.
//! - `reserve_reconciles`: the pools' reserve shares plus the seed lender's are the share supply, the pools and the
//!   seed lender are never owed more than the reserve's lenders are, and Vanna's vault covers its books.
//! - `no_stray_funds`: no pool ever keeps CASH, and idle USDC stays at rounding dust.
//! - `all_holders_can_redeem`: at the end, once the borrower repays, every share of every side redeems.

mod common;

use common::*;
use outcome_yield_pool::constants::SWAP_DUST_UNITS;
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

#[derive(Default, Clone, Copy)]
struct Flows {
    deposited: [i128; 2],
    redeemed: [i128; 2],
    /// What the keeper's merges lost to the swap, per side (a merge costs both sides the same).
    merge_loss: i128,
    vanna_hops: i128,
    /// Redemptions that swapped: each may cost both sides up to `SWAP_DUST_UNITS` of fee rounding.
    swaps: i128,
}

fn side(is_yes: bool) -> usize {
    usize::from(!is_yes)
}

fn check(env: &Env, pools: &[Pool], flows: &[Flows], users: &[Keypair], seed_shares: u64) {
    let reserve = env.reserve_state();
    let mut pool_shares = 0;
    let mut owed = 0;
    for (p, f) in pools.iter().zip(flows) {
        for is_yes in [true, false] {
            let held: u64 = users.iter().map(|u| env.shares_of(p, &u.pubkey(), is_yes)).sum();
            assert_eq!(held, env.total_shares(p, is_yes), "share_conservation");

            let net = f.deposited[side(is_yes)] - f.redeemed[side(is_yes)] - f.merge_loss;
            let backing = env.total_assets(p, is_yes) as i128;
            let dust = 2 * f.vanna_hops + SWAP_DUST_UNITS as i128 * f.swaps;
            assert!(backing + dust >= net, "side_backing_covers_flows: backing {backing}, net {net}");
        }
        let token_diff = env.dangling(p, true) as i128 - env.dangling(p, false) as i128;
        let flow_diff = (f.deposited[0] - f.redeemed[0]) - (f.deposited[1] - f.redeemed[1]);
        assert_eq!(token_diff, flow_diff, "yes_no_difference_conservation");

        assert_eq!(env.balance(&p.pool_cash), 0, "no_stray_funds: CASH");
        assert!(env.balance(&p.pool_usdc) as i128 <= 2 * f.vanna_hops + 2, "no_stray_funds: USDC");
        pool_shares += env.vanna_shares(p);
        owed += env.vanna_value(p);
    }
    assert_eq!(pool_shares + seed_shares, reserve.share_supply, "reserve_reconciles: shares");
    assert!(owed + reserve.value_of(seed_shares) <= reserve.lender_total(), "reserve_reconciles: value");
    assert!(reserve.vault_balance >= reserve.accounted_liquidity, "reserve_reconciles: vault");
}

fn run(seed: u64, steps: usize) {
    let mut env = Env::new();
    let pools = [env.pool, env.create_market_and_pool(), env.create_market_and_pool()];
    let seed_shares = env.reserve_state().share_supply;
    let mut flows = [Flows::default(); 3];
    let users: Vec<Keypair> = (0..4).map(|_| env.user()).collect();
    let mut rng = Rng(seed);

    for _ in 0..steps {
        let pi = rng.below(3) as usize;
        let p = pools[pi];
        let user = &users[rng.below(users.len() as u64) as usize];
        let is_yes = rng.below(2) == 0;

        match rng.below(8) {
            0..=2 => {
                let amount = ONE + rng.below(50 * ONE);
                env.mint_outcome_tokens(user, &p.market, amount);
                let to = user.pubkey();
                if env.try_deposit_as(&p, user, is_yes, amount, &to).is_ok() {
                    flows[pi].deposited[side(is_yes)] += amount as i128;
                }
            }
            3 => {
                let before = env.total_assets(&p, true);
                let sets = env.complete_sets(&p);
                if env.merge(&p).is_ok() && sets > 0 {
                    // Merging moves `sets` of each side's tokens into Vanna; whatever the swap returned below par is
                    // the merge's cost, the same for both sides.
                    flows[pi].merge_loss += (before - env.total_assets(&p, true)) as i128;
                    flows[pi].vanna_hops += 1;
                }
            }
            4 | 5 => {
                let held = env.shares_of(&p, &user.pubkey(), is_yes);
                if held > 0 {
                    let shares = 1 + rng.below(held);
                    let divests = env.redemption_shortfall(&p, is_yes, shares) > 0;
                    let owner = user.pubkey();
                    if let Ok(paid) = env.try_redeem_as(&p, user, &owner, is_yes, shares, &owner) {
                        flows[pi].redeemed[side(is_yes)] += paid as i128;
                        flows[pi].vanna_hops += i128::from(divests);
                        flows[pi].swaps += i128::from(divests);
                    }
                }
            }
            6 => {
                // Interest: the borrower draws up to a third of what the reserve can lend, and time passes.
                let r = env.reserve_state();
                let free = r.accounted_liquidity.saturating_sub(r.accrued_fees);
                if rng.below(4) == 0 && r.total_borrow > 0 {
                    env.repay_all();
                } else if free > 3 * ONE {
                    env.borrow(1 + rng.below(free / 3));
                }
                env.warp(3_600 + rng.below(20 * 86_400) as i64);
            }
            _ => {
                let held = env.shares_of(&p, &user.pubkey(), is_yes);
                if held > 0 {
                    let to = users[rng.below(users.len() as u64) as usize].pubkey();
                    if to != user.pubkey() {
                        env.transfer(user, &p.share_mint(is_yes), &to, rng.below(held) + 1).unwrap();
                    }
                }
            }
        }
        check(&env, &pools, &flows, &users, seed_shares);
    }

    if env.reserve_state().total_borrow > 0 {
        env.repay_all();
    }
    for p in &pools {
        for is_yes in [true, false] {
            for u in &users {
                let held = env.shares_of(p, &u.pubkey(), is_yes);
                if held > 0 {
                    let owner = u.pubkey();
                    env.try_redeem_as(p, u, &owner, is_yes, held, &owner).expect("all_holders_can_redeem");
                }
            }
            assert_eq!(env.total_shares(p, is_yes), 0);
        }
    }
}

#[test]
fn invariants_hold_seed_1() {
    run(0x9E37_79B9_7F4A_7C15, 200);
}

#[test]
fn invariants_hold_seed_2() {
    run(0xD1B5_4A32_D192_ED03, 200);
}

#[test]
fn invariants_hold_seed_3() {
    run(0x2545_F491_4F6C_DD1D, 200);
}

#[test]
fn invariants_hold_seed_4() {
    run(0x5851_F42D_4C95_7F2D, 200);
}
