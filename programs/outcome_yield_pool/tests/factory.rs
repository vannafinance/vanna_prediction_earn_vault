//! Config, market allowlist, swap policy and pool creation.

mod common;

use common::*;
use outcome_yield_pool::constants::{CASH_MINT, MAX_SWAP_LOSS_BPS_CAP, USDC_MINT, VANNA_PROGRAM_ID};
use outcome_yield_pool::errors::PoolError;
use outcome_yield_pool::state::Config;
use solana_signer::Signer;

fn config(env: &Env) -> Config {
    let a = env.svm.get_account(&Env::config_pda()).unwrap();
    anchor_lang::AccountDeserialize::try_deserialize(&mut a.data.as_slice()).unwrap()
}

#[test]
fn create_pool_records_market_and_vanna_identity() {
    let env = Env::new();
    let p = env.pool;
    let s = env.pool_state(&p);

    assert_eq!(s.market, p.market.market);
    assert_eq!(s.vanna_reserve, vanna::reserve(&USDC_MINT));
    assert_eq!(s.vanna_protocol_config, vanna::protocol_config());
    assert_eq!(s.vanna_asset_config, vanna::asset_config(&USDC_MINT));
    assert_eq!(s.vanna_liquidity_vault, vanna::liquidity_vault(&USDC_MINT));
    assert_eq!(s.vanna_share_mint, vanna::share_mint(&USDC_MINT));
    assert_eq!(s.collateral_mint, CASH_MINT);
    assert_eq!(s.yes_mint, p.market.yes_mint);
    assert_eq!(s.no_mint, p.market.no_mint);
    assert_eq!(s.market_cash_vault, p.market.cash_vault);
    assert_eq!(s.pool_cash, p.pool_cash);
    assert_eq!(s.pool_usdc, p.pool_usdc);
    assert_eq!(s.pool_vanna_shares, p.pool_vanna_shares);
    assert_eq!(s.status, PoolStatus::Open);
}

#[test]
fn pool_token_accounts_are_owned_by_the_pool_under_the_right_programs() {
    let env = Env::new();
    let p = env.pool;
    for (account, program) in [
        (p.pool_cash, TOKEN_2022),
        (p.pool_yes, TOKEN_2022),
        (p.pool_no, TOKEN_2022),
        (p.pool_usdc, SPL_TOKEN),
        (p.pool_vanna_shares, SPL_TOKEN),
    ] {
        let a = env.svm.get_account(&account).unwrap();
        assert_eq!(a.owner, program, "{account}");
        assert_eq!(env.token_account_info(&account).1, p.pool, "{account} is not the pool's");
    }
}

#[test]
fn share_mints_carry_three_more_decimals_than_cash() {
    let env = Env::new();
    assert_eq!(env.decimals(&env.pool.yes_share_mint), CASH_DECIMALS + 3);
    assert_eq!(env.decimals(&env.pool.no_share_mint), CASH_DECIMALS + 3);
    assert_eq!(env.total_shares(&env.pool, true), 0);
}

#[test]
fn pool_address_is_a_pure_function_of_market_and_reserve() {
    let env = Env::new();
    assert_eq!(env.pool.pool, Env::pool_pda(&env.pool.market.market));
}

#[test]
fn same_market_cannot_get_a_second_pool() {
    let mut env = Env::new();
    let market = env.pool.market;
    assert_fails(env.create_pool(&market));
}

#[test]
fn market_must_be_allowed_before_a_pool_can_be_created() {
    let mut env = Env::new();
    let market = env.create_market();
    assert_fails(env.create_pool(&market));

    env.allow_market(&market).unwrap();
    env.create_pool(&market).unwrap();
}

#[test]
fn only_admin_can_allow_a_market() {
    let mut env = Env::new();
    let market = env.create_market();
    let stranger = env.user();
    assert_pool_err(env.allow_market_as(&stranger, &market.market), PoolError::Unauthorized);
}

#[test]
fn allow_market_rejects_an_account_predict_does_not_own() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    assert_pool_err(env.allow_market_as(&admin, &vanna::reserve(&USDC_MINT)), PoolError::NotPredictMarket);
}

#[test]
fn disallowing_a_market_stops_new_pools_but_not_existing_ones() {
    let mut env = Env::new();
    let p = env.pool;
    let admin = env.admin.insecure_clone();
    env.disallow_market_as(&admin, &p.market.market).unwrap();
    // The pool exists, so recreating it fails either way; what matters is the existing pool keeps working.
    let alice = env.user();
    assert!(env.deposit(&p, &alice, true, 10 * ONE) > 0);

    let market = env.create_market();
    env.allow_market(&market).unwrap();
    env.disallow_market_as(&admin, &market.market).unwrap();
    assert_fails(env.create_pool(&market));
}

#[test]
fn admin_can_hand_over_and_old_admin_loses_rights() {
    let mut env = Env::new();
    let old = env.admin.insecure_clone();
    let new = env.user();
    env.set_admin_as(&old, &new.pubkey()).unwrap();

    let market = env.create_market();
    assert_pool_err(env.allow_market_as(&old, &market.market), PoolError::Unauthorized);
    env.allow_market_as(&new, &market.market).unwrap();
}

#[test]
fn only_admin_can_change_the_resolver() {
    let mut env = Env::new();
    let stranger = env.user();
    let key = stranger.pubkey();
    assert_pool_err(env.set_resolver_as(&stranger, &key), PoolError::Unauthorized);
}

#[test]
fn config_can_only_be_initialized_once() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let ix = Env::initialize_config_ix(&admin.pubkey(), &admin.pubkey(), &admin.pubkey(), 0);
    assert_fails(env.send_as(&admin, &[ix]));
}

#[test]
fn only_the_upgrade_authority_can_initialize_the_config() {
    // A program is not invocable in the slot it is deployed in, so the first caller cannot be trusted to be the
    // deployer. Anyone else claiming the config is refused.
    let mut env = Env::bare();
    let squatter = env.user();
    let k = squatter.pubkey();
    let ix = Env::initialize_config_ix(&k, &k, &k, 0);
    assert_pool_err(env.send_as(&squatter, &[ix]), PoolError::Unauthorized);

    env.initialize_config();
}

#[test]
fn config_records_the_keeper_and_its_loss_allowance() {
    let env = Env::new();
    let c = config(&env);
    assert_eq!(c.keeper, env.keeper.pubkey());
    assert_eq!(c.max_swap_loss_bps, KEEPER_LOSS_BPS);
    assert_eq!(c.swap_loss_bps(&env.keeper.pubkey()), KEEPER_LOSS_BPS);
    assert_eq!(c.swap_loss_bps(&env.admin.pubkey()), 0, "nobody but the keeper may lose anything");
}

#[test]
fn swap_loss_allowance_is_capped_at_initialization() {
    let mut env = Env::bare();
    let admin = env.admin.insecure_clone();
    let a = admin.pubkey();
    let ix = Env::initialize_config_ix(&a, &a, &a, MAX_SWAP_LOSS_BPS_CAP + 1);
    assert_pool_err(env.send_as(&admin, &[ix]), PoolError::InvalidSwapPolicy);
    let ix = Env::initialize_config_ix(&a, &a, &a, MAX_SWAP_LOSS_BPS_CAP);
    env.send_as(&admin, &[ix]).unwrap();
}

#[test]
fn only_admin_sets_the_swap_policy_and_within_the_cap() {
    let mut env = Env::new();
    let admin = env.admin.insecure_clone();
    let stranger = env.user();
    let s = stranger.pubkey();
    assert_pool_err(env.set_swap_policy_as(&stranger, &s, 5), PoolError::Unauthorized);
    assert_pool_err(env.set_swap_policy_as(&admin, &s, MAX_SWAP_LOSS_BPS_CAP + 1), PoolError::InvalidSwapPolicy);

    env.set_swap_policy_as(&admin, &s, 5).unwrap();
    let c = config(&env);
    assert_eq!((c.keeper, c.max_swap_loss_bps), (s, 5));
}

#[test]
fn a_reserve_other_than_vannas_usdc_reserve_is_rejected() {
    let mut env = Env::new();
    let market = env.create_market();
    env.allow_market(&market).unwrap();
    let payer = env.user();

    // Vanna-owned, right layout, wrong account: the reserve of another mint.
    let admin = env.vanna_admin.insecure_clone();
    let collateral = env.collateral_mint;
    env.send_as(&admin, &[vanna::initialize_reserve(&admin.pubkey(), &collateral)]).unwrap();
    let (ix, _) = env.create_pool_ix_over(&payer.pubkey(), &market, &vanna::reserve(&collateral));
    assert_pool_err(env.send_as(&payer, &[ix]), PoolError::InvalidVannaReserve);

    // Vanna-owned but not a reserve at all.
    let (ix, _) = env.create_pool_ix_over(&payer.pubkey(), &market, &vanna::protocol_config());
    assert_pool_err(env.send_as(&payer, &[ix]), PoolError::InvalidVannaReserve);

    // Anything else.
    let (ix, _) = env.create_pool_ix_over(&payer.pubkey(), &market, &market.market);
    assert_pool_err(env.send_as(&payer, &[ix]), PoolError::InvalidVannaReserve);
}

#[test]
fn a_vanna_share_mint_other_than_the_reserves_is_rejected() {
    let mut env = Env::new();
    let market = env.create_market();
    env.allow_market(&market).unwrap();
    let payer = env.user();
    let (ix, pool) = env.create_pool_ix(&payer.pubkey(), &market);
    let collateral = env.collateral_mint;
    let ix = swap_account(ix, &vanna::share_mint(&USDC_MINT), &collateral);
    let ix = swap_account(ix, &pool.pool_vanna_shares, &spl_ata(&pool.pool, &collateral));
    assert_pool_err(env.send_as(&payer, &[ix]), PoolError::InvalidVannaReserve);
}

#[test]
fn a_program_other_than_vannas_cannot_pose_as_the_reserve() {
    let mut env = Env::new();
    let market = env.create_market();
    env.allow_market(&market).unwrap();
    // A byte-identical copy of the reserve, owned by some other program, at the reserve's own address.
    let reserve = vanna::reserve(&USDC_MINT);
    let mut forged = env.svm.get_account(&reserve).unwrap();
    forged.owner = outcome_yield_pool::ID;
    env.svm.set_account(reserve, forged).unwrap();
    assert_pool_err(env.create_pool(&market), PoolError::InvalidVannaReserve);
    assert_ne!(VANNA_PROGRAM_ID, outcome_yield_pool::ID);
}

#[test]
fn wrong_outcome_mint_is_rejected() {
    let mut env = Env::new();
    let other_market = env.create_market();
    let market = env.create_market();
    env.allow_market(&market).unwrap();
    let payer = env.user();
    let (ix, pool) = env.create_pool_ix(&payer.pubkey(), &market);
    let ix = swap_account(ix, &market.yes_mint, &other_market.yes_mint);
    let ix = swap_account(ix, &pool.pool_yes, &ata(&pool.pool, &other_market.yes_mint));
    assert_pool_err(env.send_as(&payer, &[ix]), PoolError::OutcomeMintMismatch);
}
