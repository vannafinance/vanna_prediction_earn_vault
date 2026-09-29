//! Shared fixture for every integration suite.
//!
//! Everything the pool calls runs as real bytecode over mainnet state:
//!
//! - World's prediCt and three live markets with all their holders (`fixtures/world`);
//! - Jupiter v6, Orca Whirlpools and the CASH/USDC whirlpool (`fixtures/swap`);
//! - Vanna's lending program, built from its source at `VANNA_PROGRAM_ID` (`fixtures/vanna`). Vanna is not deployed
//!   yet, so its state is set up here the way its admin would: the protocol, a reserve over the real USDC mint, and a
//!   collateral asset for the borrower whose interest is the pools' yield. A seed lender supplies first, as the
//!   reserve would have lenders other than pools.
//!
//! Not real: CASH, USDC and collateral balances are written into token accounts (only Bridge and Circle mint CASH and
//! USDC), Pyth price accounts are written directly (as Vanna's own suite does), and World's operator signs with a
//! placeholder signature, which is why signature verification is off.
//!
//! Time only moves through [`Env::warp`], which also refreshes the Vanna reserve, so between warps the reserve's stored
//! state is exactly what the pool program sees. Every accounting helper derives its value from raw chain state the way
//! an off-chain consumer would.

#![allow(dead_code, unused_imports)]

use std::str::FromStr;

use anchor_lang::prelude::{Clock, Pubkey};
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use base64::Engine as _;
use litesvm::types::{FailedTransactionMetadata, TransactionResult};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, AddressLookupTableAccount, VersionedMessage};
use solana_signer::Signer as _;
use solana_transaction::versioned::VersionedTransaction;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token_2022_interface::instruction as token_ix;

use outcome_yield_pool::constants::*;
use outcome_yield_pool::errors::PoolError;
use outcome_yield_pool::math::{VIRTUAL_ASSETS, VIRTUAL_SHARES};
use outcome_yield_pool::predict::{MERGE_DISCRIMINATOR, SPLIT_DISCRIMINATOR};

pub use outcome_yield_pool::state::{Pool as PoolState, PoolStatus};

pub const TOKEN_2022: Pubkey = spl_token_2022_interface::ID;
pub const SPL_TOKEN: Pubkey = anchor_lang::prelude::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const ATA_PROGRAM: Pubkey = spl_associated_token_account_interface::program::ID;
pub const SYSTEM_PROGRAM: Pubkey = anchor_lang::solana_program::system_program::ID;
pub const WHIRLPOOL_PROGRAM: Pubkey = anchor_lang::prelude::pubkey!("whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc");
const COMPUTE_BUDGET: Pubkey = anchor_lang::prelude::pubkey!("ComputeBudget111111111111111111111111111111");
const LOOKUP_TABLE_PROGRAM: Pubkey = anchor_lang::prelude::pubkey!("AddressLookupTab1e1111111111111111111111111");
pub const PYTH_RECEIVER: Pubkey = anchor_lang::prelude::pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");

/// prediCt's Anchor event-CPI authority, `["__event_authority"]`.
pub const EVENT_AUTHORITY: Pubkey = anchor_lang::prelude::pubkey!("3szuQmavzLtzPitk9LbuUtcRd6W3299f7zNGNKD5vK82");
const DETERMINE_OUTCOME: [u8; 8] = [0x18, 0x71, 0x16, 0x66, 0x31, 0x95, 0x6d, 0x52];
const REDEEM_OUTCOME_FOR_USER: [u8; 8] = [0x00, 0x11, 0xa7, 0x62, 0xe9, 0x1c, 0x6b, 0x34];
const BURN_WORTHLESS_OUTCOME: [u8; 8] = [0xb0, 0x80, 0xce, 0x01, 0x6e, 0x20, 0x5a, 0x2d];
const CLOSE_MARKET: [u8; 8] = [0x58, 0x9a, 0xf8, 0xba, 0x30, 0x0e, 0x7b, 0xf4];
/// Byte offset of the payout wallet in prediCt's 113-byte `UserSettings` account.
const USER_SETTINGS_PAYOUT_OFFSET: usize = 41;

pub const CASH_DECIMALS: u8 = 6;
/// One whole CASH, USDC or outcome token.
pub const ONE: u64 = 1_000_000;

pub const V_SHARES: u64 = VIRTUAL_SHARES as u64;
pub const V_ASSETS: u64 = VIRTUAL_ASSETS as u64;

/// The keeper's CASH -> USDC loss allowance in the default config. The live whirlpool costs ~2.3 bps.
pub const KEEPER_LOSS_BPS: u16 = 10;

/// USDC a lender other than the pools supplies to Vanna before any pool exists.
pub const SEED_LENDER_USDC: u64 = 10_000 * ONE;

/// Anchor custom error code of a pool error.
pub fn pool_err(e: PoolError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

/* VANNA */

pub mod vanna {
    use super::*;

    pub const PROGRAM: Pubkey = VANNA_PROGRAM_ID;
    pub const USDC_FEED: [u8; 32] = [1u8; 32];
    pub const COLLATERAL_FEED: [u8; 32] = [2u8; 32];
    /// $1.00 and $200.00 at exponent -8.
    pub const USDC_PRICE: i64 = 100_000_000;
    pub const COLLATERAL_PRICE: i64 = 20_000_000_000;
    pub const COLLATERAL_DECIMALS: u8 = 9;
    /// Vanna's own test curve: ~17.5% borrow APR at 50% utilization.
    pub const RATE_CURVE: [u64; 3] = [100_000_000_000_000_000, 300_000_000_000_000_000, 3_500_000_000_000_000_000];
    /// 10% of borrow interest goes to Vanna's treasury.
    pub const RESERVE_FACTOR_BPS: u16 = 1_000;

    /// `ReserveStatus` and `OperatingMode` values.
    pub const ACTIVE: u8 = 0;
    pub const SUPPLY_ONLY: u8 = 1;
    pub const REPAY_ONLY: u8 = 2;
    pub const FROZEN: u8 = 3;
    pub const NORMAL: u8 = 0;
    pub const WITHDRAW_ONLY: u8 = 2;
    pub const HALTED: u8 = 3;

    const INITIALIZE_PROTOCOL: [u8; 8] = [188, 233, 252, 106, 134, 146, 202, 91];
    const ADMIN_REGISTER_ASSET: [u8; 8] = [102, 222, 185, 83, 217, 237, 73, 58];
    const ADMIN_INITIALIZE_RESERVE: [u8; 8] = [112, 120, 94, 208, 244, 120, 140, 116];
    const ADMIN_UPDATE_RESERVE_CONFIG: [u8; 8] = [16, 184, 69, 228, 32, 7, 42, 112];
    const ADMIN_SET_OPERATING_MODE: [u8; 8] = [200, 95, 61, 164, 49, 74, 164, 155];
    const PUBLIC_REFRESH_RESERVE: [u8; 8] = [212, 53, 162, 184, 171, 222, 142, 156];
    const LENDER_SUPPLY: [u8; 8] = [52, 247, 77, 250, 74, 122, 100, 33];
    const LENDER_REDEEM: [u8; 8] = [22, 48, 40, 70, 164, 48, 75, 224];
    const USER_CREATE_MARGIN: [u8; 8] = [105, 23, 202, 134, 147, 166, 130, 205];
    const USER_DEPOSIT_COLLATERAL: [u8; 8] = [238, 206, 161, 246, 218, 26, 100, 157];
    const USER_OPEN_DEBT_POSITION: [u8; 8] = [144, 183, 201, 223, 74, 100, 20, 205];
    const USER_BORROW: [u8; 8] = [125, 34, 221, 55, 252, 190, 132, 154];
    const PUBLIC_REPAY_FROM_WALLET: [u8; 8] = [136, 29, 241, 215, 137, 62, 170, 22];
    pub const PRICE_UPDATE_V2: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];

    fn pda(seeds: &[&[u8]]) -> Pubkey {
        Pubkey::find_program_address(seeds, &PROGRAM).0
    }

    pub fn protocol_config() -> Pubkey {
        pda(&[b"protocol"])
    }

    pub fn asset_config(mint: &Pubkey) -> Pubkey {
        pda(&[b"asset", mint.as_ref()])
    }

    pub fn reserve(mint: &Pubkey) -> Pubkey {
        pda(&[b"reserve", mint.as_ref()])
    }

    pub fn share_mint(mint: &Pubkey) -> Pubkey {
        pda(&[b"share_mint", mint.as_ref()])
    }

    pub fn liquidity_vault(mint: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(&reserve(mint), mint, &SPL_TOKEN)
    }

    pub fn margin(authority: &Pubkey) -> Pubkey {
        pda(&[b"margin", authority.as_ref()])
    }

    pub fn debt_position(margin: &Pubkey, reserve: &Pubkey) -> Pubkey {
        pda(&[b"debt", margin.as_ref(), reserve.as_ref()])
    }

    fn ix(discriminator: [u8; 8], accounts: Vec<AccountMeta>, args: &[u8]) -> Instruction {
        Instruction { program_id: PROGRAM, accounts, data: [&discriminator[..], args].concat() }
    }

    fn w(k: Pubkey) -> AccountMeta {
        AccountMeta::new(k, false)
    }

    fn r(k: Pubkey) -> AccountMeta {
        AccountMeta::new_readonly(k, false)
    }

    fn s(k: Pubkey, writable: bool) -> AccountMeta {
        AccountMeta { pubkey: k, is_signer: true, is_writable: writable }
    }

    pub fn initialize_protocol(admin: &Pubkey, treasury: &Pubkey) -> Instruction {
        let args = [treasury.as_ref(), &[8u8]].concat();
        ix(INITIALIZE_PROTOCOL, vec![s(*admin, true), s(*admin, false), w(protocol_config()), r(SYSTEM_PROGRAM)], &args)
    }

    pub fn register_asset(admin: &Pubkey, mint: &Pubkey, feed: [u8; 32], collateral: bool, borrow: bool) -> Instruction {
        let mut args = feed.to_vec();
        args.extend_from_slice(&0u64.to_le_bytes()); // max_collateral_per_margin: uncapped
        for bps in [8_000u16, 8_500, 500, 1_000] {
            args.extend_from_slice(&bps.to_le_bytes()); // ltv, liquidation threshold, bonus, max confidence
        }
        args.extend_from_slice(&3_600u32.to_le_bytes()); // max_price_age_secs
        args.extend_from_slice(&[u8::from(collateral), u8::from(borrow)]);
        let accounts = vec![
            s(*admin, false),
            s(*admin, true),
            w(protocol_config()),
            r(*mint),
            w(asset_config(mint)),
            r(SPL_TOKEN),
            r(SYSTEM_PROGRAM),
        ];
        ix(ADMIN_REGISTER_ASSET, accounts, &args)
    }

    fn reserve_args(supply_cap: u64, status: u8) -> Vec<u8> {
        let mut args: Vec<u8> = RATE_CURVE.iter().flat_map(|c| c.to_le_bytes()).collect();
        args.extend_from_slice(&RESERVE_FACTOR_BPS.to_le_bytes());
        args.extend_from_slice(&supply_cap.to_le_bytes());
        args.extend_from_slice(&0u64.to_le_bytes()); // borrow_cap: uncapped
        args.push(status);
        args
    }

    pub fn initialize_reserve(admin: &Pubkey, mint: &Pubkey) -> Instruction {
        let accounts = vec![
            s(*admin, false),
            s(*admin, true),
            r(protocol_config()),
            w(asset_config(mint)),
            r(*mint),
            w(reserve(mint)),
            w(liquidity_vault(mint)),
            w(share_mint(mint)),
            r(SPL_TOKEN),
            r(SPL_TOKEN),
            r(ATA_PROGRAM),
            r(SYSTEM_PROGRAM),
        ];
        ix(ADMIN_INITIALIZE_RESERVE, accounts, &reserve_args(0, ACTIVE))
    }

    pub fn update_reserve(admin: &Pubkey, mint: &Pubkey, supply_cap: u64, status: u8) -> Instruction {
        let accounts = vec![s(*admin, false), r(protocol_config()), r(*mint), w(reserve(mint))];
        ix(ADMIN_UPDATE_RESERVE_CONFIG, accounts, &reserve_args(supply_cap, status))
    }

    pub fn set_operating_mode(admin: &Pubkey, mode: u8) -> Instruction {
        ix(ADMIN_SET_OPERATING_MODE, vec![s(*admin, false), w(protocol_config())], &[mode])
    }

    pub fn refresh_reserve(mint: &Pubkey) -> Instruction {
        ix(PUBLIC_REFRESH_RESERVE, vec![w(reserve(mint))], &[])
    }

    fn lender_accounts(lender: &Pubkey, mint: &Pubkey, supply: bool) -> Vec<AccountMeta> {
        let mut accounts = vec![
            s(*lender, supply),
            r(protocol_config()),
            r(asset_config(mint)),
            w(reserve(mint)),
            r(*mint),
            w(get_associated_token_address_with_program_id(lender, mint, &SPL_TOKEN)),
            w(liquidity_vault(mint)),
            w(share_mint(mint)),
            w(get_associated_token_address_with_program_id(lender, &share_mint(mint), &SPL_TOKEN)),
            r(SPL_TOKEN),
            r(SPL_TOKEN),
        ];
        if supply {
            accounts.extend([r(ATA_PROGRAM), r(SYSTEM_PROGRAM)]);
        }
        accounts
    }

    pub fn lender_supply(lender: &Pubkey, mint: &Pubkey, assets: u64) -> Instruction {
        let args = [assets.to_le_bytes(), 1u64.to_le_bytes()].concat();
        ix(LENDER_SUPPLY, lender_accounts(lender, mint, true), &args)
    }

    pub fn lender_redeem(lender: &Pubkey, mint: &Pubkey, shares: u64) -> Instruction {
        let args = [shares.to_le_bytes(), 1u64.to_le_bytes()].concat();
        ix(LENDER_REDEEM, lender_accounts(lender, mint, false), &args)
    }

    pub fn create_margin(authority: &Pubkey) -> Instruction {
        let accounts = vec![s(*authority, false), s(*authority, true), w(margin(authority)), r(SYSTEM_PROGRAM)];
        ix(USER_CREATE_MARGIN, accounts, &[])
    }

    pub fn deposit_collateral(authority: &Pubkey, mint: &Pubkey, amount: u64) -> Instruction {
        let m = margin(authority);
        let accounts = vec![
            s(*authority, true),
            r(protocol_config()),
            w(m),
            r(asset_config(mint)),
            r(*mint),
            w(get_associated_token_address_with_program_id(authority, mint, &SPL_TOKEN)),
            w(get_associated_token_address_with_program_id(&m, mint, &SPL_TOKEN)),
            r(SPL_TOKEN),
            r(ATA_PROGRAM),
            r(SYSTEM_PROGRAM),
        ];
        ix(USER_DEPOSIT_COLLATERAL, accounts, &amount.to_le_bytes())
    }

    pub fn open_debt_position(authority: &Pubkey, mint: &Pubkey) -> Instruction {
        let m = margin(authority);
        let accounts = vec![
            s(*authority, false),
            s(*authority, true),
            w(m),
            r(asset_config(mint)),
            w(reserve(mint)),
            w(debt_position(&m, &reserve(mint))),
            r(SYSTEM_PROGRAM),
        ];
        ix(USER_OPEN_DEBT_POSITION, accounts, &[])
    }

    #[allow(clippy::too_many_arguments)]
    pub fn borrow(
        authority: &Pubkey,
        mint: &Pubkey,
        price: &Pubkey,
        collateral_mint: &Pubkey,
        collateral_price: &Pubkey,
        assets: u64,
    ) -> Instruction {
        let m = margin(authority);
        let mut accounts = vec![
            s(*authority, true),
            r(protocol_config()),
            w(m),
            r(asset_config(mint)),
            w(reserve(mint)),
            w(debt_position(&m, &reserve(mint))),
            r(*price),
            r(*mint),
            w(liquidity_vault(mint)),
            w(get_associated_token_address_with_program_id(&m, mint, &SPL_TOKEN)),
            r(SPL_TOKEN),
            r(ATA_PROGRAM),
            r(SYSTEM_PROGRAM),
        ];
        // Health-scan group for the one collateral: asset config, margin vault, price.
        accounts.extend([
            r(asset_config(collateral_mint)),
            r(get_associated_token_address_with_program_id(&m, collateral_mint, &SPL_TOKEN)),
            r(*collateral_price),
        ]);
        let args = [&assets.to_le_bytes()[..], &u128::MAX.to_le_bytes()[..]].concat();
        ix(USER_BORROW, accounts, &args)
    }

    pub fn repay_all_from_wallet(payer: &Pubkey, borrower: &Pubkey, mint: &Pubkey) -> Instruction {
        let m = margin(borrower);
        let accounts = vec![
            s(*payer, true),
            w(m),
            r(asset_config(mint)),
            w(reserve(mint)),
            w(debt_position(&m, &reserve(mint))),
            r(*mint),
            w(get_associated_token_address_with_program_id(payer, mint, &SPL_TOKEN)),
            w(liquidity_vault(mint)),
            r(SPL_TOKEN),
        ];
        let args = [&u64::MAX.to_le_bytes()[..], &[1u8]].concat();
        ix(PUBLIC_REPAY_FROM_WALLET, accounts, &args)
    }

    /// A Pyth-receiver `PriceUpdateV2`, fully verified, published at `publish_time`.
    pub fn price_account(feed: [u8; 32], price: i64, publish_time: i64) -> Account {
        let mut data = PRICE_UPDATE_V2.to_vec();
        data.extend_from_slice(&[7u8; 32]); // write_authority
        data.push(1); // VerificationLevel::Full
        data.extend_from_slice(&feed);
        data.extend_from_slice(&price.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // conf
        data.extend_from_slice(&(-8i32).to_le_bytes());
        data.extend_from_slice(&publish_time.to_le_bytes());
        data.extend_from_slice(&(publish_time - 1).to_le_bytes());
        data.extend_from_slice(&price.to_le_bytes()); // ema_price
        data.extend_from_slice(&0u64.to_le_bytes()); // ema_conf
        data.extend_from_slice(&1u64.to_le_bytes()); // posted_slot
        data.resize(134, 0);
        Account { lamports: 10_000_000, data, owner: PYTH_RECEIVER, executable: false, rent_epoch: u64::MAX }
    }

    /// A Vanna reserve's accounting, read from its account.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ReserveState {
        pub accounted_liquidity: u64,
        pub total_borrow: u64,
        pub accrued_fees: u64,
        pub last_update: i64,
        pub status: u8,
        pub share_supply: u64,
        pub vault_balance: u64,
    }

    impl ReserveState {
        /// `lender_total_assets`: what all lenders together are owed.
        pub fn lender_total(&self) -> u64 {
            self.accounted_liquidity + self.total_borrow - self.accrued_fees
        }

        /// `supply_shares_to_assets_down`.
        pub fn value_of(&self, shares: u64) -> u64 {
            if self.share_supply == 0 {
                return 0;
            }
            (shares as u128 * self.lender_total() as u128 / self.share_supply as u128) as u64
        }
    }
}

/* FIXTURE TYPES */

/// One of the three live World markets in the snapshot.
#[derive(Clone, Copy, Debug)]
pub struct Market {
    pub title: &'static str,
    pub market: Pubkey,
    pub yes_mint: Pubkey,
    pub no_mint: Pubkey,
    pub cash_vault: Pubkey,
}

/// A deployed pool plus everything needed to drive it. The program only stores the pool; this bundles the rest.
#[derive(Clone, Copy, Debug)]
pub struct Pool {
    pub pool: Pubkey,
    pub market: Market,
    pub yes_share_mint: Pubkey,
    pub no_share_mint: Pubkey,
    pub pool_cash: Pubkey,
    pub pool_yes: Pubkey,
    pub pool_no: Pubkey,
    pub pool_usdc: Pubkey,
    pub pool_vanna_shares: Pubkey,
}

impl Pool {
    pub fn outcome_mint(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.market.yes_mint
        } else {
            self.market.no_mint
        }
    }

    pub fn share_mint(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.yes_share_mint
        } else {
            self.no_share_mint
        }
    }

    pub fn pool_outcome(&self, is_yes: bool) -> Pubkey {
        if is_yes {
            self.pool_yes
        } else {
            self.pool_no
        }
    }

    /// Every token account the pool owns.
    pub fn token_accounts(&self) -> [Pubkey; 5] {
        [self.pool_cash, self.pool_yes, self.pool_no, self.pool_usdc, self.pool_vanna_shares]
    }
}

/// A token account of the classic SPL program (USDC, Vanna shares, collateral).
pub fn spl_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, &SPL_TOKEN)
}

/// A Token-2022 account (CASH, outcome tokens, pool shares).
pub fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(owner, mint, &TOKEN_2022)
}

/// A Jupiter instruction's data and accounts, as a client would get them from Jupiter's API.
#[derive(Clone, Debug)]
pub struct Route {
    pub data: Vec<u8>,
    pub accounts: Vec<AccountMeta>,
}

impl Route {
    /// No route: for merges with nothing to match and redemptions dangling tokens cover.
    pub fn none() -> Self {
        Route { data: Vec::new(), accounts: Vec::new() }
    }

    /// Replaces every occurrence of `from` in the route's accounts.
    pub fn swap_account(mut self, from: &Pubkey, to: &Pubkey) -> Self {
        for meta in self.accounts.iter_mut().filter(|m| m.pubkey == *from) {
            meta.pubkey = *to;
        }
        self
    }

    pub fn set_account(mut self, index: usize, key: Pubkey) -> Self {
        self.accounts[index].pubkey = key;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouteKind {
    /// Jupiter's `route`: the user's accounts trade directly with the whirlpool.
    Direct,
    /// Jupiter's `shared_accounts_route`: Jupiter's program authority trades on the user's behalf.
    Shared,
}

/// One snapshotted direction of the CASH/USDC whirlpool route.
#[derive(Clone)]
struct RouteTemplate {
    fixed: Vec<Pubkey>,
    hop: Vec<(Pubkey, bool)>,
    shared_id: u8,
}

struct SwapFixture {
    placeholder: Pubkey,
    /// [direct, shared] for CASH -> USDC, then for USDC -> CASH.
    cash_to_usdc: [RouteTemplate; 2],
    usdc_to_cash: [RouteTemplate; 2],
}

/// The snapshot, as loaded into a chain.
pub struct World {
    /// The three allowlisted live markets.
    pub markets: Vec<Market>,
    /// Every real holder's YES/NO token account per live market, as of the snapshot.
    pub holders: Vec<Vec<Pubkey>>,
    /// Markets World had already resolved, but not yet closed, at snapshot time, with their outcome.
    pub resolved: Vec<(Market, u16)>,
}

pub struct Env {
    pub svm: LiteSVM,
    /// Pool config admin, resolver, upgrade authority and fee payer.
    pub admin: Keypair,
    /// The config's keeper: merges within `KEEPER_LOSS_BPS` of par.
    pub keeper: Keypair,
    /// Vanna's protocol admin.
    pub vanna_admin: Keypair,
    /// A Vanna borrower whose interest is the reserve's yield.
    pub borrower: Keypair,
    pub collateral_mint: Pubkey,
    pub usdc_price: Pubkey,
    pub collateral_price: Pubkey,
    pub pool: Pool,
    pub world: World,
    /// The live markets, in the order `create_market` hands them out.
    pub markets: Vec<Market>,
    next_market: usize,
    swaps: SwapFixture,
    lookup_table: Pubkey,
}

fn program_path(name: &str) -> String {
    format!("{}/../../target/deploy/{name}.so", env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path(name: &str) -> String {
    format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn read_json(name: &str) -> serde_json::Value {
    let path = fixture_path(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("{path}: {e}. Run scripts/fetch_world_fixtures.py, fetch_swap_fixtures.py and build_vanna_fixture.sh.")
    });
    serde_json::from_str(&text).unwrap()
}

fn key(v: &serde_json::Value) -> Pubkey {
    Pubkey::from_str(v.as_str().unwrap()).unwrap()
}

/// Loads every snapshotted mainnet account in `dir` exactly as it was on mainnet.
fn load_accounts(svm: &mut LiteSVM, dir: &str) {
    for entry in std::fs::read_dir(fixture_path(dir)).unwrap_or_else(|_| panic!("fixtures/{dir} missing")) {
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap();
        let data = base64::engine::general_purpose::STANDARD.decode(doc["data"].as_str().unwrap()).unwrap();
        let account = Account {
            lamports: doc["lamports"].as_u64().unwrap(),
            data,
            owner: key(&doc["owner"]),
            executable: doc["executable"].as_bool().unwrap(),
            rent_epoch: u64::MAX,
        };
        svm.set_account(key(&doc["pubkey"]), account).unwrap();
    }
}

fn parse_market(m: &serde_json::Value) -> Market {
    Market {
        title: Box::leak(m["title"].as_str().unwrap().to_owned().into_boxed_str()),
        market: key(&m["market"]),
        yes_mint: key(&m["yesMint"]),
        no_mint: key(&m["noMint"]),
        cash_vault: key(&m["cashVault"]),
    }
}

fn load_world(svm: &mut LiteSVM) -> World {
    svm.add_program_from_file(PREDICT_PROGRAM_ID, fixture_path("world/predict.so")).unwrap();
    load_accounts(svm, "world/accounts");
    let snapshot = read_json("world/markets.json");
    let live = snapshot["markets"].as_array().unwrap();
    World {
        markets: live.iter().map(parse_market).collect(),
        holders: live.iter().map(|m| m["holders"].as_array().unwrap().iter().map(key).collect()).collect(),
        resolved: snapshot["resolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (parse_market(m), m["outcome"].as_u64().unwrap() as u16))
            .collect(),
    }
}

fn load_swaps(svm: &mut LiteSVM) -> SwapFixture {
    svm.add_program_from_file(JUPITER_PROGRAM_ID, fixture_path("swap/jupiter.so")).unwrap();
    svm.add_program_from_file(WHIRLPOOL_PROGRAM, fixture_path("swap/whirlpool.so")).unwrap();
    load_accounts(svm, "swap/accounts");
    let doc = read_json("swap/routes.json");
    let template = |direction: &str, shared: bool| {
        let r = doc["routes"][direction]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["shared"].as_bool() == Some(shared) && h["size"].as_u64() == Some(1_000_000_000))
            .expect("route missing from fixtures/swap/routes.json");
        let data = hex(r["data"].as_str().unwrap());
        RouteTemplate {
            fixed: r["fixed"].as_array().unwrap().iter().map(key).collect(),
            hop: r["hop"].as_array().unwrap().iter().map(|h| (key(&h["pubkey"]), h["writable"].as_bool().unwrap())).collect(),
            shared_id: if shared { data[8] } else { 0 },
        }
    };
    SwapFixture {
        placeholder: key(&doc["placeholderUser"]),
        cash_to_usdc: [template("cash_to_usdc", false), template("cash_to_usdc", true)],
        usdc_to_cash: [template("usdc_to_cash", false), template("usdc_to_cash", true)],
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// The latest of the two snapshots: the whirlpool refuses a clock older than its last update, and prediCt accepts one
/// a day past its own snapshot.
fn snapshot_clock() -> (u64, i64) {
    let world = read_json("world/markets.json");
    let swap = read_json("swap/routes.json");
    let slot = world["slot"].as_u64().unwrap().max(swap["slot"].as_u64().unwrap());
    let time = world["unixTimestamp"].as_i64().unwrap().max(swap["unixTimestamp"].as_i64().unwrap());
    (slot, time)
}

fn funded(svm: &mut LiteSVM) -> Keypair {
    let kp = Keypair::new();
    svm.airdrop(&kp.pubkey(), 1_000_000_000_000).unwrap();
    kp
}

fn compute_limit_ix() -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&1_400_000u32.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET, accounts: vec![], data }
}

/// The upgradeable loader's ProgramData account for `program`.
pub fn program_data_pda(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[program.as_ref()], &anchor_lang::solana_program::bpf_loader_upgradeable::ID).0
}

/// LiteSVM deploys with no upgrade authority. Sets one, as a real deployment would have.
fn set_upgrade_authority(svm: &mut LiteSVM, program: &Pubkey, authority: &Pubkey) {
    // bincode `ProgramData { slot: u64, upgrade_authority_address: Option<Pubkey> }` after a u32 variant tag.
    let address = program_data_pda(program);
    let mut account = svm.get_account(&address).expect("program data missing");
    account.data[12] = 1;
    account.data[13..45].copy_from_slice(authority.as_ref());
    svm.set_account(address, account).unwrap();
}

/// An address lookup table holding `addresses`, active since slot 0.
fn lookup_table_account(addresses: &[Pubkey]) -> Account {
    let mut data = vec![0u8; 56];
    data[..4].copy_from_slice(&1u32.to_le_bytes()); // ProgramState::LookupTable
    data[4..12].copy_from_slice(&u64::MAX.to_le_bytes()); // deactivation_slot: never
    for a in addresses {
        data.extend_from_slice(a.as_ref());
    }
    Account { lamports: 1_000_000_000, data, owner: LOOKUP_TABLE_PROGRAM, executable: false, rent_epoch: u64::MAX }
}

impl Env {
    /// A fresh chain with the pool config and one default market and pool.
    pub fn new() -> Self {
        let mut env = Self::bare();
        env.initialize_config();
        let market = env.create_market();
        env.allow_market(&market).unwrap();
        env.pool = env.create_pool(&market).unwrap();
        env
    }

    /// Every program, World's markets and a seeded Vanna USDC reserve; no pool config, allowlist or pools.
    pub fn bare() -> Self {
        let mut env = Self::without_seed_lender();
        let seed = env.user();
        env.vanna_supply(&seed, SEED_LENDER_USDC).unwrap();
        env
    }

    /// Like [`Env::bare`], but nobody has supplied to Vanna yet, so the first pool to merge is the reserve's first
    /// lender.
    pub fn without_seed_lender() -> Self {
        let mut svm = LiteSVM::new().with_sigverify(false);
        svm.add_program_from_file(outcome_yield_pool::ID, program_path("outcome_yield_pool")).unwrap();
        svm.add_program_from_file(VANNA_PROGRAM_ID, fixture_path("vanna/vanna_lending.so")).unwrap();
        let world = load_world(&mut svm);
        let swaps = load_swaps(&mut svm);
        let (slot, time) = snapshot_clock();
        let mut clock: Clock = svm.get_sysvar();
        clock.slot = slot;
        clock.unix_timestamp = time;
        svm.set_sysvar(&clock);

        let markets = world.markets.clone();
        svm.airdrop(&WORLD_OPERATOR, 100_000_000_000).unwrap();
        let admin = funded(&mut svm);
        let keeper = funded(&mut svm);
        let vanna_admin = funded(&mut svm);
        let borrower = funded(&mut svm);
        set_upgrade_authority(&mut svm, &outcome_yield_pool::ID, &admin.pubkey());

        let blank = Market {
            title: "",
            market: Pubkey::default(),
            yes_mint: Pubkey::default(),
            no_mint: Pubkey::default(),
            cash_vault: Pubkey::default(),
        };
        let placeholder = Pool {
            pool: Pubkey::default(),
            market: blank,
            yes_share_mint: Pubkey::default(),
            no_share_mint: Pubkey::default(),
            pool_cash: Pubkey::default(),
            pool_yes: Pubkey::default(),
            pool_no: Pubkey::default(),
            pool_usdc: Pubkey::default(),
            pool_vanna_shares: Pubkey::default(),
        };

        let mut env = Env {
            svm,
            admin,
            keeper,
            vanna_admin,
            borrower,
            collateral_mint: Pubkey::new_unique(),
            usdc_price: Pubkey::new_unique(),
            collateral_price: Pubkey::new_unique(),
            pool: placeholder,
            world,
            markets,
            next_market: 0,
            swaps,
            lookup_table: Pubkey::new_unique(),
        };
        env.setup_vanna();
        env
    }

    /// What Vanna's admin does to open a USDC reserve, plus a collateral asset the test borrower pledges.
    fn setup_vanna(&mut self) {
        let admin = self.vanna_admin.insecure_clone();
        let a = admin.pubkey();
        self.write_mint(&self.collateral_mint.clone(), vanna::COLLATERAL_DECIMALS);
        self.set_prices();
        let ixs = [
            vanna::initialize_protocol(&a, &Pubkey::new_unique()),
            vanna::register_asset(&a, &USDC_MINT, vanna::USDC_FEED, true, true),
            vanna::initialize_reserve(&a, &USDC_MINT),
            vanna::register_asset(&a, &self.collateral_mint, vanna::COLLATERAL_FEED, true, false),
        ];
        self.send_as(&admin, &ixs).expect("Vanna setup");
    }

    fn write_mint(&mut self, mint: &Pubkey, decimals: u8) {
        // spl-token `Mint`: no mint authority (balances are written directly), supply 0, initialized.
        let mut data = vec![0u8; 82];
        data[44] = decimals;
        data[45] = 1;
        let account = Account { lamports: 10_000_000, data, owner: SPL_TOKEN, executable: false, rent_epoch: u64::MAX };
        self.svm.set_account(*mint, account).unwrap();
    }

    fn set_prices(&mut self) {
        let now = self.now();
        let usdc = vanna::price_account(vanna::USDC_FEED, vanna::USDC_PRICE, now);
        let collateral = vanna::price_account(vanna::COLLATERAL_FEED, vanna::COLLATERAL_PRICE, now);
        self.svm.set_account(self.usdc_price, usdc).unwrap();
        self.svm.set_account(self.collateral_price, collateral).unwrap();
    }

    pub fn user(&mut self) -> Keypair {
        funded(&mut self.svm)
    }

    pub fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    /// Moves the clock forward, then refreshes Vanna's reserve and the Pyth prices, so the reserve's stored state is
    /// what the pool program sees until the next warp.
    pub fn warp(&mut self, seconds: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp += seconds;
        clock.slot += (seconds as u64).saturating_mul(5) / 2 + 1;
        self.svm.set_sysvar(&clock);
        self.set_prices();
        self.refresh_reserve();
    }

    pub fn refresh_reserve(&mut self) {
        let payer = self.admin.insecure_clone();
        self.send_as(&payer, &[vanna::refresh_reserve(&USDC_MINT)]).unwrap();
    }

    /* TRANSACTIONS */

    /// Sends a v0 transaction whose non-signer accounts all come from a lookup table: a merge or a redemption with
    /// its Jupiter route names more accounts than a legacy transaction can hold.
    pub fn send(&mut self, payer: &Keypair, ixs: &[Instruction], extra_signers: &[&Keypair]) -> TransactionResult {
        let mut signers: Vec<&Keypair> = vec![payer];
        for s in extra_signers {
            if !signers.iter().any(|x| x.pubkey() == s.pubkey()) {
                signers.push(s);
            }
        }
        let signer_keys: Vec<Pubkey> = signers.iter().map(|s| s.pubkey()).collect();
        let tx = |msg| VersionedTransaction::try_new(VersionedMessage::V0(msg), &signers).unwrap();
        let msg = self.compile(&payer.pubkey(), ixs, &signer_keys);
        self.svm.send_transaction(tx(msg))
    }

    fn compile(&mut self, payer: &Pubkey, ixs: &[Instruction], signers: &[Pubkey]) -> v0::Message {
        self.svm.expire_blockhash();
        let mut all = vec![compute_limit_ix()];
        all.extend_from_slice(ixs);
        let mut addresses: Vec<Pubkey> = Vec::new();
        for ix in &all {
            for k in ix.accounts.iter().map(|m| m.pubkey).chain([ix.program_id]) {
                if !signers.contains(&k) && k != *payer && !addresses.contains(&k) {
                    addresses.push(k);
                }
            }
        }
        self.svm.set_account(self.lookup_table, lookup_table_account(&addresses)).unwrap();
        let table = AddressLookupTableAccount { key: self.lookup_table, addresses };
        v0::Message::try_compile(payer, &all, &[table], self.svm.latest_blockhash()).expect("compile")
    }

    /// Sends as `signer`, who also pays.
    pub fn send_as(&mut self, signer: &Keypair, ixs: &[Instruction]) -> TransactionResult {
        self.send(signer, ixs, &[])
    }

    /// Sends as World's operator key, which pays and signs. Its signature is a placeholder: the chain runs without
    /// signature verification, so this exercises exactly the instructions World's operator sends.
    pub fn send_as_operator(&mut self, ixs: &[Instruction]) -> TransactionResult {
        let msg = self.compile(&WORLD_OPERATOR, ixs, &[WORLD_OPERATOR]);
        let signatures = vec![Default::default(); msg.header.num_required_signatures as usize];
        self.svm.send_transaction(VersionedTransaction { signatures, message: VersionedMessage::V0(msg) })
    }

    /* RAW STATE */

    pub fn balance(&self, token_account: &Pubkey) -> u64 {
        match self.svm.get_account(token_account) {
            Some(a) if a.data.len() >= 72 => u64::from_le_bytes(a.data[64..72].try_into().unwrap()),
            _ => 0,
        }
    }

    pub fn supply(&self, mint: &Pubkey) -> u64 {
        let a = self.svm.get_account(mint).expect("mint missing");
        u64::from_le_bytes(a.data[36..44].try_into().unwrap())
    }

    pub fn account_exists(&self, key: &Pubkey) -> bool {
        self.svm.get_account(key).is_some_and(|a| a.lamports > 0)
    }

    pub fn pool_state(&self, pool: &Pool) -> PoolState {
        let a = self.svm.get_account(&pool.pool).expect("pool missing");
        anchor_lang::AccountDeserialize::try_deserialize(&mut a.data.as_slice()).unwrap()
    }

    pub fn reserve_state(&self) -> vanna::ReserveState {
        let d = self.svm.get_account(&vanna::reserve(&USDC_MINT)).expect("reserve missing").data;
        let u64_at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
        vanna::ReserveState {
            accounted_liquidity: u64_at(152),
            total_borrow: u64_at(160),
            accrued_fees: u64_at(200),
            last_update: u64_at(208) as i64,
            status: d[242],
            share_supply: self.supply(&vanna::share_mint(&USDC_MINT)),
            vault_balance: self.balance(&vanna::liquidity_vault(&USDC_MINT)),
        }
    }

    /* DERIVED ACCOUNTING */

    /// Outcome tokens of one side the pool holds but has not merged.
    pub fn dangling(&self, pool: &Pool, is_yes: bool) -> u64 {
        self.balance(&pool.pool_outcome(is_yes))
    }

    /// The pool's Vanna reserve shares.
    pub fn vanna_shares(&self, pool: &Pool) -> u64 {
        self.balance(&pool.pool_vanna_shares)
    }

    /// USDC the pool's reserve shares redeem for now.
    pub fn vanna_value(&self, pool: &Pool) -> u64 {
        self.reserve_state().value_of(self.vanna_shares(pool))
    }

    /// The pool's invested balance: idle USDC plus its reserve position, USDC at par with CASH.
    pub fn invested(&self, pool: &Pool) -> u64 {
        self.balance(&pool.pool_usdc) + self.vanna_value(pool)
    }

    pub fn total_assets(&self, pool: &Pool, is_yes: bool) -> u64 {
        self.dangling(pool, is_yes) + self.invested(pool)
    }

    pub fn total_shares(&self, pool: &Pool, is_yes: bool) -> u64 {
        self.supply(&pool.share_mint(is_yes))
    }

    pub fn shares_of(&self, pool: &Pool, owner: &Pubkey, is_yes: bool) -> u64 {
        self.balance(&ata(owner, &pool.share_mint(is_yes)))
    }

    pub fn outcome_of(&self, pool: &Pool, owner: &Pubkey, is_yes: bool) -> u64 {
        self.balance(&ata(owner, &pool.outcome_mint(is_yes)))
    }

    pub fn cash_of(&self, owner: &Pubkey) -> u64 {
        self.balance(&ata(owner, &CASH_MINT))
    }

    pub fn usdc_of(&self, owner: &Pubkey) -> u64 {
        self.balance(&spl_ata(owner, &USDC_MINT))
    }

    /// Shares a deposit of `assets` would mint right now.
    pub fn preview_deposit(&self, pool: &Pool, is_yes: bool, assets: u64) -> u64 {
        let total = self.total_assets(pool, is_yes) as u128;
        let supply = self.total_shares(pool, is_yes) as u128;
        (assets as u128 * (supply + VIRTUAL_SHARES) / (total + VIRTUAL_ASSETS)) as u64
    }

    /// Outcome tokens redeeming `shares` would pay right now.
    pub fn preview_redeem(&self, pool: &Pool, is_yes: bool, shares: u64) -> u64 {
        let total = self.total_assets(pool, is_yes) as u128;
        let supply = self.total_shares(pool, is_yes) as u128;
        (shares as u128 * (total + VIRTUAL_ASSETS) / (supply + VIRTUAL_SHARES)) as u64
    }

    /// USDC a redemption of `shares` must convert back into CASH: the part dangling tokens do not cover.
    pub fn redemption_shortfall(&self, pool: &Pool, is_yes: bool, shares: u64) -> u64 {
        self.preview_redeem(pool, is_yes, shares).saturating_sub(self.dangling(pool, is_yes))
    }

    /* TOKEN HELPERS */

    pub fn create_ata_ix(&self, owner: &Pubkey, mint: &Pubkey) -> Instruction {
        create_associated_token_account_idempotent(&self.admin.pubkey(), owner, mint, &TOKEN_2022)
    }

    pub fn create_spl_ata_ix(&self, owner: &Pubkey, mint: &Pubkey) -> Instruction {
        create_associated_token_account_idempotent(&self.admin.pubkey(), owner, mint, &SPL_TOKEN)
    }

    pub fn ensure_ata(&mut self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let ix = self.create_ata_ix(owner, mint);
        let admin = self.admin.insecure_clone();
        self.send(&admin, &[ix], &[]).unwrap();
        ata(owner, mint)
    }

    pub fn ensure_spl_ata(&mut self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let ix = self.create_spl_ata_ix(owner, mint);
        let admin = self.admin.insecure_clone();
        self.send(&admin, &[ix], &[]).unwrap();
        spl_ata(owner, mint)
    }

    /// Adds `amount` to a token account and to its mint's supply, as a mint by the issuer would.
    pub fn credit(&mut self, token_account: &Pubkey, mint: &Pubkey, amount: u64) {
        let add = |svm: &mut LiteSVM, address: &Pubkey, offset: usize| {
            let mut a = svm.get_account(address).unwrap();
            let v = u64::from_le_bytes(a.data[offset..offset + 8].try_into().unwrap()) + amount;
            a.data[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
            svm.set_account(*address, a).unwrap();
        };
        add(&mut self.svm, token_account, 64);
        add(&mut self.svm, mint, 36);
    }

    /// Gives `owner` `amount` CASH in their real Token-2022 CASH account. Only Bridge can mint CASH.
    pub fn mint_cash(&mut self, owner: &Pubkey, amount: u64) -> Pubkey {
        let account = self.ensure_ata(owner, &CASH_MINT);
        self.credit(&account, &CASH_MINT, amount);
        account
    }

    /// Gives `owner` `amount` USDC. Only Circle can mint USDC.
    pub fn mint_usdc(&mut self, owner: &Pubkey, amount: u64) -> Pubkey {
        let account = self.ensure_spl_ata(owner, &USDC_MINT);
        self.credit(&account, &USDC_MINT, amount);
        account
    }

    pub fn transfer(&mut self, owner: &Keypair, mint: &Pubkey, to_owner: &Pubkey, amount: u64) -> TransactionResult {
        let to = self.ensure_ata(to_owner, mint);
        let ix = token_ix::transfer_checked(
            &TOKEN_2022,
            &ata(&owner.pubkey(), mint),
            mint,
            &to,
            &owner.pubkey(),
            &[],
            amount,
            self.decimals(mint),
        )
        .unwrap();
        self.send_as(owner, &[ix])
    }

    /// Transfers USDC (a classic SPL mint) from `owner` straight into `to`.
    pub fn transfer_usdc(&mut self, owner: &Keypair, to: &Pubkey, amount: u64) -> TransactionResult {
        let ix = token_ix::transfer_checked(
            &SPL_TOKEN,
            &spl_ata(&owner.pubkey(), &USDC_MINT),
            &USDC_MINT,
            to,
            &owner.pubkey(),
            &[],
            amount,
            6,
        )
        .unwrap();
        self.send_as(owner, &[ix])
    }

    pub fn approve(&mut self, owner: &Keypair, mint: &Pubkey, delegate: &Pubkey, amount: u64) -> TransactionResult {
        let ix = token_ix::approve(&TOKEN_2022, &ata(&owner.pubkey(), mint), delegate, &owner.pubkey(), &[], amount)
            .unwrap();
        self.send_as(owner, &[ix])
    }

    pub fn decimals(&self, mint: &Pubkey) -> u8 {
        self.svm.get_account(mint).unwrap().data[44]
    }

    /* VANNA */

    /// `lender` supplies `amount` USDC to Vanna directly, as any lender other than a pool.
    pub fn vanna_supply(&mut self, lender: &Keypair, amount: u64) -> TransactionResult {
        self.mint_usdc(&lender.pubkey(), amount);
        self.send_as(lender, &[vanna::lender_supply(&lender.pubkey(), &USDC_MINT, amount)])
    }

    /// The test borrower takes `amount` USDC out of the reserve against ample collateral. Its interest is the yield.
    pub fn borrow(&mut self, amount: u64) {
        let b = self.borrower.insecure_clone();
        let bk = b.pubkey();
        if !self.account_exists(&vanna::margin(&bk)) {
            let collateral = 1_000_000 * 10u64.pow(vanna::COLLATERAL_DECIMALS as u32); // $200m
            let wallet = self.ensure_spl_ata(&bk, &self.collateral_mint.clone());
            self.credit(&wallet, &self.collateral_mint.clone(), collateral);
            let ixs = [
                vanna::create_margin(&bk),
                vanna::deposit_collateral(&bk, &self.collateral_mint, collateral),
                vanna::open_debt_position(&bk, &USDC_MINT),
            ];
            self.send_as(&b, &ixs).expect("borrower setup");
        }
        let ix = vanna::borrow(&bk, &USDC_MINT, &self.usdc_price, &self.collateral_mint, &self.collateral_price, amount);
        self.send_as(&b, &[ix]).expect("borrow");
    }

    /// The borrower repays everything it owes, principal and interest, from its wallet.
    pub fn repay_all(&mut self) {
        let b = self.borrower.insecure_clone();
        let debt = self.reserve_state().total_borrow;
        self.mint_usdc(&b.pubkey(), debt + ONE);
        self.send_as(&b, &[vanna::repay_all_from_wallet(&b.pubkey(), &b.pubkey(), &USDC_MINT)]).expect("repay");
    }

    /// Interest for the pools: the borrower draws `borrowed` USDC and time passes by `seconds`.
    pub fn earn(&mut self, borrowed: u64, seconds: i64) {
        self.borrow(borrowed);
        self.warp(seconds);
    }

    pub fn set_reserve(&mut self, supply_cap: u64, status: u8) {
        let admin = self.vanna_admin.insecure_clone();
        self.send_as(&admin, &[vanna::update_reserve(&admin.pubkey(), &USDC_MINT, supply_cap, status)]).unwrap();
    }

    pub fn set_vanna_mode(&mut self, mode: u8) {
        let admin = self.vanna_admin.insecure_clone();
        self.send_as(&admin, &[vanna::set_operating_mode(&admin.pubkey(), mode)]).unwrap();
    }

    /* JUPITER */

    /// A Jupiter route through the CASH/USDC whirlpool for `pool`, swapping `amount` in.
    pub fn route(&self, pool: &Pool, cash_to_usdc: bool, amount: u64, kind: RouteKind) -> Route {
        let (source, destination) =
            if cash_to_usdc { (pool.pool_cash, pool.pool_usdc) } else { (pool.pool_usdc, pool.pool_cash) };
        self.route_between(&pool.pool, &source, &destination, cash_to_usdc, amount, kind)
    }

    /// A route for any authority and token accounts, e.g. a plain wallet swapping directly on Jupiter.
    pub fn route_between(
        &self,
        authority: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
        cash_to_usdc: bool,
        amount: u64,
        kind: RouteKind,
    ) -> Route {
        let templates = if cash_to_usdc { &self.swaps.cash_to_usdc } else { &self.swaps.usdc_to_cash };
        let t = &templates[usize::from(kind == RouteKind::Shared)];
        let (authority_slot, source_slot, destination_slot) = match kind {
            RouteKind::Direct => (1, 2, 3),
            RouteKind::Shared => (2, 3, 6),
        };
        let (user_source, user_destination) = (t.fixed[source_slot], t.fixed[destination_slot]);
        let substitute = |k: Pubkey| {
            if k == self.swaps.placeholder {
                *authority
            } else if k == user_source {
                *source
            } else if k == user_destination {
                *destination
            } else {
                k
            }
        };
        let mut accounts: Vec<AccountMeta> = t
            .fixed
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let writable = i == source_slot
                    || i == destination_slot
                    || (kind == RouteKind::Shared && (i == 4 || i == 5));
                AccountMeta { pubkey: substitute(*k), is_signer: false, is_writable: writable }
            })
            .collect();
        accounts[authority_slot].pubkey = *authority;
        accounts.extend(t.hop.iter().map(|(k, w)| AccountMeta { pubkey: substitute(*k), is_signer: false, is_writable: *w }));

        let mut data = match kind {
            RouteKind::Direct => outcome_yield_pool::jupiter::ROUTE.to_vec(),
            RouteKind::Shared => [&outcome_yield_pool::jupiter::SHARED_ACCOUNTS_ROUTE[..], &[t.shared_id]].concat(),
        };
        data.extend_from_slice(&1u32.to_le_bytes()); // one route-plan step
        // `Swap::WhirlpoolSwapV2 { a_to_b, remaining_accounts_info: None }`, 100% of input 0 into output 1. CASH is the
        // whirlpool's mint A.
        data.extend_from_slice(&[0x2f, u8::from(cash_to_usdc), 0, 100, 0, 1]);
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes()); // quoted_out_amount: the pool enforces its own floor
        data.extend_from_slice(&0u16.to_le_bytes()); // slippage_bps
        data.push(0); // platform_fee_bps
        Route { data, accounts }
    }

    /// What the live whirlpool pays for `amount` right now, measured by a throwaway wallet's swap.
    pub fn quote(&mut self, cash_to_usdc: bool, amount: u64) -> u64 {
        let trader = self.user();
        let t = trader.pubkey();
        let cash = self.ensure_ata(&t, &CASH_MINT);
        let usdc = self.ensure_spl_ata(&t, &USDC_MINT);
        let (source, destination) = if cash_to_usdc { (cash, usdc) } else { (usdc, cash) };
        let mint = if cash_to_usdc { CASH_MINT } else { USDC_MINT };
        self.credit(&source, &mint, amount);
        let route = self.route_between(&t, &source, &destination, cash_to_usdc, amount, RouteKind::Direct);
        let mut accounts = route.accounts;
        for m in accounts.iter_mut().filter(|m| m.pubkey == t) {
            m.is_signer = true;
        }
        // Measure on a copy of the chain, so quoting leaves no trace.
        let before = self.balance(&destination);
        let snapshot = self.svm.clone();
        self.send_as(&trader, &[Instruction { program_id: JUPITER_PROGRAM_ID, accounts, data: route.data }])
            .expect("quote swap");
        let out = self.balance(&destination) - before;
        self.svm = snapshot;
        out
    }

    /* WORLD (live prediCt) */

    /// The next of the three live World markets. Only World's operator can create markets, so tests draw from the
    /// snapshot instead.
    pub fn create_market(&mut self) -> Market {
        let market = *self.markets.get(self.next_market).expect("the snapshot holds only three live markets");
        self.next_market += 1;
        market
    }

    /// Mints `amount` CASH to `user` and splits it at prediCt, leaving the user `amount` YES and `amount` NO.
    pub fn mint_outcome_tokens(&mut self, user: &Keypair, market: &Market, amount: u64) {
        let owner = user.pubkey();
        let cash = self.mint_cash(&owner, amount);
        let yes = self.ensure_ata(&owner, &market.yes_mint);
        let no = self.ensure_ata(&owner, &market.no_mint);
        let ix = Self::set_ix(true, &owner, market, cash, yes, no, amount);
        self.send_as(user, &[ix]).unwrap();
    }

    pub fn set_ix(
        split: bool,
        authority: &Pubkey,
        market: &Market,
        cash: Pubkey,
        yes: Pubkey,
        no: Pubkey,
        amount: u64,
    ) -> Instruction {
        let accounts = vec![
            AccountMeta::new(*authority, true),
            AccountMeta::new(market.market, false),
            AccountMeta::new_readonly(CASH_MINT, false),
            AccountMeta::new(market.yes_mint, false),
            AccountMeta::new(market.no_mint, false),
            AccountMeta::new(cash, false),
            AccountMeta::new(market.cash_vault, false),
            AccountMeta::new(yes, false),
            AccountMeta::new(no, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
        ];
        let discriminator = if split { SPLIT_DISCRIMINATOR } else { MERGE_DISCRIMINATOR };
        predict_ix(discriminator, accounts, &amount.to_le_bytes())
    }

    /// World's operator writes the result: `10000` when YES wins, `0` when NO wins.
    pub fn determine_outcome(&mut self, market: &Market, outcome: u16) -> TransactionResult {
        let accounts = vec![
            AccountMeta::new(WORLD_OPERATOR, true),
            AccountMeta::new(market.market, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PREDICT_PROGRAM_ID, false),
        ];
        self.send_as_operator(&[predict_ix(DETERMINE_OUTCOME, accounts, &outcome.to_le_bytes())])
    }

    pub fn user_settings_pda(user: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[b"user_settings", user.as_ref()], &PREDICT_PROGRAM_ID).0
    }

    /// Where prediCt sends `holder`'s winnings: the payout wallet in their `UserSettings`, or the holder itself.
    pub fn payout_wallet(&self, holder: &Pubkey) -> Pubkey {
        match self.svm.get_account(&Self::user_settings_pda(holder)) {
            Some(a) if a.owner == PREDICT_PROGRAM_ID && a.data.len() >= USER_SETTINGS_PAYOUT_OFFSET + 32 => {
                Pubkey::new_from_array(
                    a.data[USER_SETTINGS_PAYOUT_OFFSET..USER_SETTINGS_PAYOUT_OFFSET + 32].try_into().unwrap(),
                )
            }
            _ => *holder,
        }
    }

    /// (mint, owner) of a token account.
    pub fn token_account_info(&self, token_account: &Pubkey) -> (Pubkey, Pubkey) {
        let data = self.svm.get_account(token_account).expect("token account missing").data;
        (Pubkey::new_from_array(data[..32].try_into().unwrap()), Pubkey::new_from_array(data[32..64].try_into().unwrap()))
    }

    /// World's operator redeems `holder`'s whole winning balance for CASH, paid to the holder's payout wallet.
    pub fn redeem_for_user(&mut self, market: &Market, winning_mint: &Pubkey, holder: &Pubkey) -> TransactionResult {
        self.redeem_token_account(market, &ata(holder, winning_mint))
    }

    /// `redeem_outcome_for_user` on any winning token account.
    pub fn redeem_token_account(&mut self, market: &Market, token_account: &Pubkey) -> TransactionResult {
        let (winning_mint, holder) = self.token_account_info(token_account);
        let payout = self.payout_wallet(&holder);
        let accounts = vec![
            AccountMeta::new(WORLD_OPERATOR, true),
            AccountMeta::new(holder, false),
            AccountMeta::new(market.market, false),
            AccountMeta::new_readonly(CASH_MINT, false),
            AccountMeta::new(winning_mint, false),
            AccountMeta::new(*token_account, false),
            AccountMeta::new_readonly(payout, false),
            AccountMeta::new(ata(&payout, &CASH_MINT), false),
            AccountMeta::new(market.cash_vault, false),
            AccountMeta::new_readonly(Self::user_settings_pda(&holder), false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(ATA_PROGRAM, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PREDICT_PROGRAM_ID, false),
        ];
        self.send_as_operator(&[predict_ix(REDEEM_OUTCOME_FOR_USER, accounts, &[])])
    }

    /// Burns `holder`'s whole losing balance after resolution. prediCt requires no signer for this.
    pub fn burn_worthless(&mut self, market: &Market, losing_mint: &Pubkey, holder: &Pubkey) -> TransactionResult {
        self.burn_token_account(market, &ata(holder, losing_mint))
    }

    /// `burn_worthless_outcome` on any losing token account.
    pub fn burn_token_account(&mut self, market: &Market, token_account: &Pubkey) -> TransactionResult {
        let (losing_mint, holder) = self.token_account_info(token_account);
        let accounts = vec![
            AccountMeta::new(market.market, false),
            AccountMeta::new(losing_mint, false),
            AccountMeta::new(*token_account, false),
            AccountMeta::new(holder, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PREDICT_PROGRAM_ID, false),
        ];
        self.send_as_operator(&[predict_ix(BURN_WORTHLESS_OUTCOME, accounts, &[])])
    }

    /// What World's operator does between `determine_outcome` and `close_market`: pays out every winning balance and
    /// burns every losing one, for the market's real holders from the snapshot plus `wallets` (test users, pools).
    pub fn wind_down(&mut self, market: &Market, yes_won: bool, wallets: &[Pubkey]) {
        let index = self.markets.iter().position(|m| m.market == market.market).expect("not a live market");
        let (win, lose) = if yes_won { (market.yes_mint, market.no_mint) } else { (market.no_mint, market.yes_mint) };
        let mut accounts = self.world.holders[index].clone();
        for w in wallets {
            accounts.extend([ata(w, &win), ata(w, &lose)]);
        }
        accounts.sort();
        accounts.dedup();
        for account in accounts {
            if self.balance(&account) == 0 {
                continue;
            }
            let (mint, owner) = self.token_account_info(&account);
            if mint == win {
                // prediCt funds a missing payout CASH account only from the closed outcome account's rent, which
                // does not cover it, so the operator creates it first when needed.
                let payout = self.payout_wallet(&owner);
                if !self.account_exists(&ata(&payout, &CASH_MINT)) {
                    let ix = create_associated_token_account_idempotent(&WORLD_OPERATOR, &payout, &CASH_MINT, &TOKEN_2022);
                    self.send_as_operator(&[ix]).unwrap();
                }
                self.redeem_token_account(market, &account).unwrap();
            } else {
                self.burn_token_account(market, &account).unwrap();
            }
        }
    }

    /// World's operator deletes the market: both outcome mints, the CASH vault and the Market account.
    pub fn close_market(&mut self, market: &Market) -> TransactionResult {
        let operator_cash = self.ensure_ata(&WORLD_OPERATOR, &CASH_MINT);
        let accounts = vec![
            AccountMeta::new(market.market, false),
            AccountMeta::new(WORLD_OPERATOR, true),
            AccountMeta::new_readonly(CASH_MINT, false),
            AccountMeta::new(operator_cash, false),
            AccountMeta::new(market.yes_mint, false),
            AccountMeta::new(market.no_mint, false),
            AccountMeta::new(market.cash_vault, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(TOKEN_2022, false),
            AccountMeta::new_readonly(EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PREDICT_PROGRAM_ID, false),
        ];
        self.send_as_operator(&[predict_ix(CLOSE_MARKET, accounts, &[])])
    }

    /* POOL PROGRAM: ADMIN */

    pub fn config_pda() -> Pubkey {
        Pubkey::find_program_address(&[CONFIG_SEED], &outcome_yield_pool::ID).0
    }

    pub fn allowed_market_pda(market: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[ALLOWED_MARKET_SEED, market.as_ref()], &outcome_yield_pool::ID).0
    }

    pub fn pool_pda(market: &Pubkey) -> Pubkey {
        Self::pool_pda_over(market, &vanna::reserve(&USDC_MINT))
    }

    pub fn pool_pda_over(market: &Pubkey, reserve: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[POOL_SEED, market.as_ref(), reserve.as_ref()], &outcome_yield_pool::ID).0
    }

    pub fn initialize_config_ix(admin: &Pubkey, resolver: &Pubkey, keeper: &Pubkey, loss_bps: u16) -> Instruction {
        pool_ix(
            outcome_yield_pool::accounts::InitializeConfig {
                admin: *admin,
                config: Self::config_pda(),
                program: outcome_yield_pool::ID,
                program_data: program_data_pda(&outcome_yield_pool::ID),
                system_program: SYSTEM_PROGRAM,
            },
            outcome_yield_pool::instruction::InitializeConfig {
                resolver: *resolver,
                keeper: *keeper,
                max_swap_loss_bps: loss_bps,
            },
        )
    }

    pub fn initialize_config(&mut self) {
        let admin = self.admin.insecure_clone();
        let ix = Self::initialize_config_ix(&admin.pubkey(), &admin.pubkey(), &self.keeper.pubkey(), KEEPER_LOSS_BPS);
        self.send_as(&admin, &[ix]).unwrap();
    }

    pub fn allow_market_as(&mut self, admin: &Keypair, market: &Pubkey) -> TransactionResult {
        let ix = pool_ix(
            outcome_yield_pool::accounts::AllowMarket {
                admin: admin.pubkey(),
                config: Self::config_pda(),
                market: *market,
                allowed_market: Self::allowed_market_pda(market),
                system_program: SYSTEM_PROGRAM,
            },
            outcome_yield_pool::instruction::AllowMarket {},
        );
        self.send_as(admin, &[ix])
    }

    pub fn allow_market(&mut self, market: &Market) -> TransactionResult {
        let admin = self.admin.insecure_clone();
        self.allow_market_as(&admin, &market.market)
    }

    pub fn disallow_market_as(&mut self, admin: &Keypair, market: &Pubkey) -> TransactionResult {
        let ix = pool_ix(
            outcome_yield_pool::accounts::DisallowMarket {
                admin: admin.pubkey(),
                config: Self::config_pda(),
                allowed_market: Self::allowed_market_pda(market),
            },
            outcome_yield_pool::instruction::DisallowMarket {},
        );
        self.send_as(admin, &[ix])
    }

    pub fn set_resolver_as(&mut self, admin: &Keypair, resolver: &Pubkey) -> TransactionResult {
        let ix = pool_ix(
            outcome_yield_pool::accounts::AdminOnly { admin: admin.pubkey(), config: Self::config_pda() },
            outcome_yield_pool::instruction::SetResolver { new_resolver: *resolver },
        );
        self.send_as(admin, &[ix])
    }

    pub fn set_admin_as(&mut self, admin: &Keypair, new_admin: &Pubkey) -> TransactionResult {
        let ix = pool_ix(
            outcome_yield_pool::accounts::AdminOnly { admin: admin.pubkey(), config: Self::config_pda() },
            outcome_yield_pool::instruction::SetAdmin { new_admin: *new_admin },
        );
        self.send_as(admin, &[ix])
    }

    pub fn set_swap_policy_as(&mut self, admin: &Keypair, keeper: &Pubkey, loss_bps: u16) -> TransactionResult {
        let ix = pool_ix(
            outcome_yield_pool::accounts::AdminOnly { admin: admin.pubkey(), config: Self::config_pda() },
            outcome_yield_pool::instruction::SetSwapPolicy { keeper: *keeper, max_swap_loss_bps: loss_bps },
        );
        self.send_as(admin, &[ix])
    }

    /* POOL PROGRAM: POOLS */

    pub fn create_pool_ix(&self, payer: &Pubkey, market: &Market) -> (Instruction, Pool) {
        self.create_pool_ix_over(payer, market, &vanna::reserve(&USDC_MINT))
    }

    /// `create_pool` naming `reserve` as the pool's Vanna reserve, with the pool derived from it.
    pub fn create_pool_ix_over(&self, payer: &Pubkey, market: &Market, reserve: &Pubkey) -> (Instruction, Pool) {
        let pool = Self::pool_pda_over(&market.market, reserve);
        let yes_share_mint =
            Pubkey::find_program_address(&[YES_SHARES_SEED, pool.as_ref()], &outcome_yield_pool::ID).0;
        let no_share_mint = Pubkey::find_program_address(&[NO_SHARES_SEED, pool.as_ref()], &outcome_yield_pool::ID).0;
        let vanna_share_mint = vanna::share_mint(&USDC_MINT);
        let p = Pool {
            pool,
            market: *market,
            yes_share_mint,
            no_share_mint,
            pool_cash: ata(&pool, &CASH_MINT),
            pool_yes: ata(&pool, &market.yes_mint),
            pool_no: ata(&pool, &market.no_mint),
            pool_usdc: spl_ata(&pool, &USDC_MINT),
            pool_vanna_shares: spl_ata(&pool, &vanna_share_mint),
        };
        let ix = pool_ix(
            outcome_yield_pool::accounts::CreatePool {
                payer: *payer,
                allowed_market: Self::allowed_market_pda(&market.market),
                market: market.market,
                vanna_reserve: *reserve,
                usdc_mint: USDC_MINT,
                vanna_share_mint,
                collateral_mint: CASH_MINT,
                yes_mint: market.yes_mint,
                no_mint: market.no_mint,
                pool,
                yes_share_mint,
                no_share_mint,
                pool_cash: p.pool_cash,
                pool_yes: p.pool_yes,
                pool_no: p.pool_no,
                pool_usdc: p.pool_usdc,
                pool_vanna_shares: p.pool_vanna_shares,
                token_program: TOKEN_2022,
                spl_token_program: SPL_TOKEN,
                associated_token_program: ATA_PROGRAM,
                system_program: SYSTEM_PROGRAM,
            },
            outcome_yield_pool::instruction::CreatePool {},
        );
        (ix, p)
    }

    pub fn create_pool(&mut self, market: &Market) -> Result<Pool, FailedTransactionMetadata> {
        let payer = self.user();
        let (ix, pool) = self.create_pool_ix(&payer.pubkey(), market);
        self.send_as(&payer, &[ix])?;
        Ok(pool)
    }

    /// Creates a fresh allowed market and a pool for it.
    pub fn create_market_and_pool(&mut self) -> Pool {
        let market = self.create_market();
        self.allow_market(&market).unwrap();
        self.create_pool(&market).unwrap()
    }

    pub fn vanna_metas(pool: &Pool) -> outcome_yield_pool::accounts::VannaAccounts {
        outcome_yield_pool::accounts::VannaAccounts {
            vanna_program: VANNA_PROGRAM_ID,
            vanna_protocol_config: vanna::protocol_config(),
            vanna_asset_config: vanna::asset_config(&USDC_MINT),
            vanna_reserve: vanna::reserve(&USDC_MINT),
            usdc_mint: USDC_MINT,
            vanna_liquidity_vault: vanna::liquidity_vault(&USDC_MINT),
            vanna_share_mint: vanna::share_mint(&USDC_MINT),
            pool_usdc: pool.pool_usdc,
            pool_vanna_shares: pool.pool_vanna_shares,
            spl_token_program: SPL_TOKEN,
            associated_token_program: ATA_PROGRAM,
            system_program: SYSTEM_PROGRAM,
        }
    }

    fn predict_metas(pool: &Pool) -> outcome_yield_pool::accounts::PredictAccounts {
        outcome_yield_pool::accounts::PredictAccounts {
            market: pool.market.market,
            collateral_mint: CASH_MINT,
            yes_mint: pool.market.yes_mint,
            no_mint: pool.market.no_mint,
            market_cash_vault: pool.market.cash_vault,
            pool_cash: pool.pool_cash,
            pool_yes: pool.pool_yes,
            pool_no: pool.pool_no,
            predict_program: PREDICT_PROGRAM_ID,
        }
    }

    pub fn deposit_ix(&self, pool: &Pool, depositor: &Pubkey, is_yes: bool, amount: u64, to: &Pubkey) -> Instruction {
        pool_ix(
            outcome_yield_pool::accounts::DepositOutcome {
                depositor: *depositor,
                pool: pool.pool,
                market: pool.market.market,
                outcome_mint: pool.outcome_mint(is_yes),
                depositor_outcome: ata(depositor, &pool.outcome_mint(is_yes)),
                pool_outcome: pool.pool_outcome(is_yes),
                share_mint: pool.share_mint(is_yes),
                recipient_shares: ata(to, &pool.share_mint(is_yes)),
                vanna: Self::vanna_metas(pool),
                token_program: TOKEN_2022,
            },
            outcome_yield_pool::instruction::Deposit { is_yes, amount },
        )
    }

    /// Complete sets a merge would match right now.
    pub fn complete_sets(&self, pool: &Pool) -> u64 {
        self.dangling(pool, true).min(self.dangling(pool, false))
    }

    pub fn merge_ix(&self, pool: &Pool, caller: &Pubkey, route: Route) -> Instruction {
        let mut ix = pool_ix(
            outcome_yield_pool::accounts::MergeAndDeposit {
                caller: *caller,
                config: Self::config_pda(),
                pool: pool.pool,
                predict: Self::predict_metas(pool),
                vanna: Self::vanna_metas(pool),
                jupiter_program: JUPITER_PROGRAM_ID,
                token_program: TOKEN_2022,
            },
            outcome_yield_pool::instruction::MergeAndDeposit { swap_data: route.data },
        );
        ix.accounts.extend(route.accounts);
        ix
    }

    /// The route a merge needs now: exactly the matched CASH, into USDC.
    pub fn merge_route(&self, pool: &Pool, kind: RouteKind) -> Route {
        match self.complete_sets(pool) {
            0 => Route::none(),
            sets => self.route(pool, true, sets, kind),
        }
    }

    pub fn merge_as(&mut self, pool: &Pool, caller: &Keypair) -> TransactionResult {
        let route = self.merge_route(pool, RouteKind::Direct);
        self.merge_with(pool, caller, route)
    }

    pub fn merge_with(&mut self, pool: &Pool, caller: &Keypair, route: Route) -> TransactionResult {
        let ix = self.merge_ix(pool, &caller.pubkey(), route);
        self.send_as(caller, &[ix])
    }

    /// The keeper merges, as it would in production while CASH trades just under USDC.
    pub fn merge(&mut self, pool: &Pool) -> TransactionResult {
        let keeper = self.keeper.insecure_clone();
        self.merge_as(pool, &keeper)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn redeem_ix(
        &self,
        pool: &Pool,
        authority: &Pubkey,
        owner: &Pubkey,
        is_yes: bool,
        shares: u64,
        to: &Pubkey,
        route: Route,
    ) -> Instruction {
        let mut ix = pool_ix(
            outcome_yield_pool::accounts::RedeemOutcome {
                authority: *authority,
                pool: pool.pool,
                share_mint: pool.share_mint(is_yes),
                owner_shares: ata(owner, &pool.share_mint(is_yes)),
                recipient_outcome: ata(to, &pool.outcome_mint(is_yes)),
                predict: Self::predict_metas(pool),
                vanna: Self::vanna_metas(pool),
                jupiter_program: JUPITER_PROGRAM_ID,
                token_program: TOKEN_2022,
            },
            outcome_yield_pool::instruction::Redeem { is_yes, shares, swap_data: route.data },
        );
        ix.accounts.extend(route.accounts);
        ix
    }

    /// The route a redemption of `shares` needs now: exactly its shortfall in USDC, into CASH.
    pub fn redeem_route(&self, pool: &Pool, is_yes: bool, shares: u64, kind: RouteKind) -> Route {
        match self.redemption_shortfall(pool, is_yes, shares) {
            0 => Route::none(),
            shortfall => self.route(pool, false, shortfall, kind),
        }
    }

    /* SETTLEMENT */

    pub fn settle_ix(&self, pool: &Pool) -> Instruction {
        pool_ix(
            outcome_yield_pool::accounts::Settle { pool: pool.pool, market: pool.market.market },
            outcome_yield_pool::instruction::Settle {},
        )
    }

    pub fn settle_by_resolver_ix(&self, pool: &Pool, resolver: &Pubkey, yes_won: bool) -> Instruction {
        pool_ix(
            outcome_yield_pool::accounts::SettleByResolver {
                resolver: *resolver,
                config: Self::config_pda(),
                pool: pool.pool,
                market: pool.market.market,
            },
            outcome_yield_pool::instruction::SettleByResolver { yes_won },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn redeem_settled_ix(
        &self,
        pool: &Pool,
        authority: &Pubkey,
        owner: &Pubkey,
        is_yes: bool,
        shares: u64,
        to: &Pubkey,
    ) -> Instruction {
        pool_ix(
            outcome_yield_pool::accounts::RedeemSettled {
                authority: *authority,
                pool: pool.pool,
                share_mint: pool.share_mint(is_yes),
                owner_shares: ata(owner, &pool.share_mint(is_yes)),
                outcome_mint: pool.outcome_mint(is_yes),
                pool_outcome: pool.pool_outcome(is_yes),
                recipient_outcome: ata(to, &pool.outcome_mint(is_yes)),
                collateral_mint: CASH_MINT,
                pool_cash: pool.pool_cash,
                recipient_cash: ata(to, &CASH_MINT),
                recipient_usdc: spl_ata(to, &USDC_MINT),
                vanna: Self::vanna_metas(pool),
                token_program: TOKEN_2022,
            },
            outcome_yield_pool::instruction::RedeemSettled { is_yes, shares },
        )
    }

    /* HIGH-LEVEL FLOWS */

    /// `user` deposits `amount` already-held outcome tokens, crediting `to`. Returns the shares minted to `to`.
    pub fn try_deposit_as(
        &mut self,
        pool: &Pool,
        user: &Keypair,
        is_yes: bool,
        amount: u64,
        to: &Pubkey,
    ) -> Result<u64, FailedTransactionMetadata> {
        let before = self.shares_of(pool, to, is_yes);
        let ixs = [
            self.create_ata_ix(to, &pool.share_mint(is_yes)),
            self.deposit_ix(pool, &user.pubkey(), is_yes, amount, to),
        ];
        let admin = self.admin.insecure_clone();
        self.send(&admin, &ixs, &[user])?;
        Ok(self.shares_of(pool, to, is_yes) - before)
    }

    /// Mints `amount` of both sides to `user` and deposits one side to `user`, without merging.
    pub fn deposit_only(&mut self, pool: &Pool, user: &Keypair, is_yes: bool, amount: u64) -> u64 {
        self.mint_outcome_tokens(user, &pool.market, amount);
        self.try_deposit_as(pool, user, is_yes, amount, &user.pubkey()).unwrap()
    }

    /// A deposit as users experience it: credits the deposit, then the keeper rebalances.
    pub fn deposit(&mut self, pool: &Pool, user: &Keypair, is_yes: bool, amount: u64) -> u64 {
        let shares = self.deposit_only(pool, user, is_yes, amount);
        self.merge(pool).unwrap();
        shares
    }

    /// `authority` redeems `shares` from `owner`'s account, paying `to`, with the route the redemption needs.
    /// Returns the outcome tokens `to` received.
    pub fn try_redeem_as(
        &mut self,
        pool: &Pool,
        authority: &Keypair,
        owner: &Pubkey,
        is_yes: bool,
        shares: u64,
        to: &Pubkey,
    ) -> Result<u64, FailedTransactionMetadata> {
        let route = self.redeem_route(pool, is_yes, shares, RouteKind::Direct);
        self.try_redeem_with(pool, authority, owner, is_yes, shares, to, route)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_redeem_with(
        &mut self,
        pool: &Pool,
        authority: &Keypair,
        owner: &Pubkey,
        is_yes: bool,
        shares: u64,
        to: &Pubkey,
        route: Route,
    ) -> Result<u64, FailedTransactionMetadata> {
        let before = self.outcome_of(pool, to, is_yes);
        let ixs = [
            self.create_ata_ix(to, &pool.outcome_mint(is_yes)),
            self.redeem_ix(pool, &authority.pubkey(), owner, is_yes, shares, to, route),
        ];
        let admin = self.admin.insecure_clone();
        self.send(&admin, &ixs, &[authority])?;
        Ok(self.outcome_of(pool, to, is_yes) - before)
    }

    pub fn redeem(&mut self, pool: &Pool, user: &Keypair, is_yes: bool, shares: u64) -> u64 {
        let owner = user.pubkey();
        self.try_redeem_as(pool, user, &owner, is_yes, shares, &owner).unwrap()
    }

    pub fn settle(&mut self, pool: &Pool) -> TransactionResult {
        let caller = self.user();
        let ix = self.settle_ix(pool);
        self.send_as(&caller, &[ix])
    }

    /// Returns `(outcome tokens, CASH, USDC)` received by `to`.
    pub fn try_redeem_settled_as(
        &mut self,
        pool: &Pool,
        authority: &Keypair,
        owner: &Pubkey,
        is_yes: bool,
        shares: u64,
        to: &Pubkey,
    ) -> Result<(u64, u64, u64), FailedTransactionMetadata> {
        let mut ixs = vec![self.create_ata_ix(to, &CASH_MINT), self.create_spl_ata_ix(to, &USDC_MINT)];
        if self.account_exists(&pool.outcome_mint(is_yes)) {
            ixs.push(self.create_ata_ix(to, &pool.outcome_mint(is_yes)));
        }
        let before = (self.outcome_of(pool, to, is_yes), self.cash_of(to), self.usdc_of(to));
        ixs.push(self.redeem_settled_ix(pool, &authority.pubkey(), owner, is_yes, shares, to));
        let admin = self.admin.insecure_clone();
        self.send(&admin, &ixs, &[authority])?;
        Ok((
            self.outcome_of(pool, to, is_yes) - before.0,
            self.cash_of(to) - before.1,
            self.usdc_of(to) - before.2,
        ))
    }

    pub fn redeem_settled(&mut self, pool: &Pool, user: &Keypair, is_yes: bool, shares: u64) -> (u64, u64, u64) {
        let owner = user.pubkey();
        self.try_redeem_settled_as(pool, user, &owner, is_yes, shares, &owner).unwrap()
    }
}


pub fn predict_ix(discriminator: [u8; 8], accounts: Vec<AccountMeta>, args: &[u8]) -> Instruction {
    Instruction { program_id: PREDICT_PROGRAM_ID, accounts, data: [&discriminator[..], args].concat() }
}

pub fn pool_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id: outcome_yield_pool::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

/// Replaces one account meta, for tests that pass a wrong account on purpose.
pub fn swap_account(mut ix: Instruction, from: &Pubkey, to: &Pubkey) -> Instruction {
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == *from {
            *meta = AccountMeta { pubkey: *to, ..meta.clone() };
        }
    }
    ix
}

/// Asserts the transaction failed with the given Anchor custom error.
#[track_caller]
pub fn assert_custom<T: std::fmt::Debug>(res: Result<T, FailedTransactionMetadata>, code: u32) {
    use solana_instruction_error::InstructionError;
    use solana_transaction_error::TransactionError;
    match res {
        Ok(v) => panic!("expected custom error {code}, transaction succeeded with {v:?}"),
        Err(e) => match e.err {
            TransactionError::InstructionError(_, InstructionError::Custom(c)) if c == code => {}
            other => panic!("expected custom error {code}, got {other:?}\nlogs: {:#?}", e.meta.logs),
        },
    }
}

#[track_caller]
pub fn assert_pool_err<T: std::fmt::Debug>(res: Result<T, FailedTransactionMetadata>, e: PoolError) {
    assert_custom(res, pool_err(e));
}

#[track_caller]
pub fn assert_fails<T: std::fmt::Debug>(res: Result<T, FailedTransactionMetadata>) {
    assert!(res.is_err(), "expected failure, transaction succeeded with {res:?}");
}
