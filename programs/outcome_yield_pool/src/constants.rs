use anchor_lang::prelude::*;

/// World's market program. Mints and burns YES/NO complete sets against CASH (`split` / `merge`).
pub const PREDICT_PROGRAM_ID: Pubkey = pubkey!("prediCtPZCttYMvm2W3PtxmMxLmT1dtN7riU6Cxh6tM");

/// The only collateral World markets use: Bridge's CASH stablecoin, a Token-2022 mint.
pub const CASH_MINT: Pubkey = pubkey!("CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH");

/// World's operator key. It creates, resolves, pays out and closes every market. Informational only: the pool never
/// requires its signature, but it is the key whose `determine_outcome` the pool reads at settlement.
pub const WORLD_OPERATOR: Pubkey = pubkey!("DDucv2DeUsTsg1rfAcWAnUSUVpqfdHEzxX66ARB2JYVg");

/// Vanna's lending program. Its USDC reserve is every pool's yield vault.
pub const VANNA_PROGRAM_ID: Pubkey = pubkey!("H37YVBG3eGtz89ofd7iDLc2PJgn12778392XeU2QJcGC");

/// Circle's USDC, a classic SPL mint: the asset of Vanna's USDC reserve.
pub const USDC_MINT: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

/// Jupiter v6. The only venue the pool swaps CASH and USDC through.
pub const JUPITER_PROGRAM_ID: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

pub const CONFIG_SEED: &[u8] = b"config";
pub const ALLOWED_MARKET_SEED: &[u8] = b"allowed_market";
pub const POOL_SEED: &[u8] = b"pool";
pub const YES_SHARES_SEED: &[u8] = b"yes_shares";
pub const NO_SHARES_SEED: &[u8] = b"no_shares";

/// Share mints carry this many more decimals than CASH. Equals log10 of `VIRTUAL_SHARES`, so one outcome token
/// deposited into an empty side mints exactly one whole share.
pub const SHARE_DECIMALS_OFFSET: u8 = 3;

pub const BASIS_POINTS: u64 = 10_000;

/// Ceiling on the loss the keeper may accept on a CASH -> USDC swap, whatever the admin configures.
pub const MAX_SWAP_LOSS_BPS_CAP: u16 = 100;

/// Raw units any swap may fall short of its floor by. AMMs round their fee up and their output down, which costs a
/// unit or two however small the swap, so without this a swap of a few units could never reach par and a small
/// redemption would be stuck. Five units (0.000005 CASH) is far below a transaction fee, so no caller gains by
/// routing swaps to collect it.
pub const SWAP_DUST_UNITS: u64 = 5;
