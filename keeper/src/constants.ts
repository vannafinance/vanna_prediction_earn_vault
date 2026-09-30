import { PublicKey } from "@solana/web3.js";

/** Mirrors `programs/outcome_yield_pool/src/constants.rs`. Only the pool program's own id is configurable. */
export const DEFAULT_POOL_PROGRAM_ID = new PublicKey("2h3hJSZVR6DgSGXmmf4wdBqo8z16kzMyVJ5gJWT1mskZ");
export const PREDICT_PROGRAM_ID = new PublicKey("prediCtPZCttYMvm2W3PtxmMxLmT1dtN7riU6Cxh6tM");
export const VANNA_PROGRAM_ID = new PublicKey("H37YVBG3eGtz89ofd7iDLc2PJgn12778392XeU2QJcGC");
export const JUPITER_PROGRAM_ID = new PublicKey("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
export const CASH_MINT = new PublicKey("CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH");
export const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

export const TOKEN_PROGRAM_ID = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
export const TOKEN_2022_PROGRAM_ID = new PublicKey("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
export const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
export const SYSTEM_PROGRAM_ID = new PublicKey("11111111111111111111111111111111");

export const CONFIG_SEED = Buffer.from("config");
export const BASIS_POINTS = 10_000n;
/** Fee rounding the program forgives on every swap (`SWAP_DUST_UNITS`). */
export const SWAP_DUST_UNITS = 5n;

/** `PoolError`, in declaration order from 6000. */
export const POOL_ERRORS = [
  "Unauthorized",
  "NotResolver",
  "NotPredictMarket",
  "InvalidCollateral",
  "OutcomeMintMismatch",
  "VannaAccountMismatch",
  "InvalidVannaReserve",
  "ZeroAmount",
  "ZeroShares",
  "PoolSettled",
  "PoolNotSettled",
  "MarketNotResolved",
  "UnsupportedOutcome",
  "MarketStillOpen",
  "MergeAmountMismatch",
  "MathOverflow",
  "MarketResolved",
  "SwapNotAllowed",
  "InvalidSwapAccounts",
  "InvalidSwapResult",
  "SwapAmountMismatch",
  "SwapBelowMinimum",
  "InvalidSwapPolicy",
] as const;
export type PoolErrorName = (typeof POOL_ERRORS)[number];

export function configPda(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync([CONFIG_SEED], programId)[0];
}
