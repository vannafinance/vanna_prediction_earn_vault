// Building, simulating and sending `merge_and_deposit`.

import {
  AccountMeta,
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  JUPITER_PROGRAM_ID,
  PREDICT_PROGRAM_ID,
  SYSTEM_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  USDC_MINT,
  VANNA_PROGRAM_ID,
  configPda,
} from "./constants.ts";
import { encodeMergeData, type PoolState } from "./codec.ts";
import type { Route } from "./jupiter.ts";

const MAX_COMPUTE_UNITS = 1_400_000;

const w = (pubkey: PublicKey): AccountMeta => ({ pubkey, isSigner: false, isWritable: true });
const r = (pubkey: PublicKey): AccountMeta => ({ pubkey, isSigner: false, isWritable: false });

/** `MergeAndDeposit`'s accounts after the caller, in IDL order: config, pool, prediCt group, Vanna group, programs. */
function fixedAccounts(programId: PublicKey, pool: PoolState): AccountMeta[] {
  return [
    r(configPda(programId)),
    w(pool.address),
    w(pool.market),
    r(pool.collateralMint),
    w(pool.yesMint),
    w(pool.noMint),
    w(pool.marketCashVault),
    w(pool.poolCash),
    w(pool.poolYes),
    w(pool.poolNo),
    r(PREDICT_PROGRAM_ID),
    r(VANNA_PROGRAM_ID),
    r(pool.vannaProtocolConfig),
    r(pool.vannaAssetConfig),
    w(pool.vannaReserve),
    r(USDC_MINT),
    w(pool.vannaLiquidityVault),
    w(pool.vannaShareMint),
    w(pool.poolUsdc),
    w(pool.poolVannaShares),
    r(TOKEN_PROGRAM_ID),
    r(ASSOCIATED_TOKEN_PROGRAM_ID),
    r(SYSTEM_PROGRAM_ID),
    r(JUPITER_PROGRAM_ID),
    r(TOKEN_2022_PROGRAM_ID),
  ];
}

/** What the pool's lookup table must hold: every fixed account of its merges. */
export function lookupAddresses(programId: PublicKey, pool: PoolState): PublicKey[] {
  return fixedAccounts(programId, pool).map((m) => m.pubkey);
}

export function mergeIx(programId: PublicKey, caller: PublicKey, pool: PoolState, route: Route): TransactionInstruction {
  return new TransactionInstruction({
    programId,
    keys: [{ pubkey: caller, isSigner: true, isWritable: false }, ...fixedAccounts(programId, pool), ...route.accounts],
    data: encodeMergeData(route.data),
  });
}

export interface Built {
  tx: VersionedTransaction;
  blockhash: string;
  lastValidBlockHeight: number;
}

export async function build(
  connection: Connection,
  payer: Keypair,
  ix: TransactionInstruction,
  tables: AddressLookupTableAccount[],
  computeUnits: number,
  microLamports: number,
): Promise<Built> {
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash("confirmed");
  const message = new TransactionMessage({
    payerKey: payer.publicKey,
    recentBlockhash: blockhash,
    instructions: [
      ComputeBudgetProgram.setComputeUnitLimit({ units: computeUnits }),
      ComputeBudgetProgram.setComputeUnitPrice({ microLamports }),
      ix,
    ],
  }).compileToV0Message(tables);
  const tx = new VersionedTransaction(message);
  tx.sign([payer]);
  // Serializing throws past the 1232-byte packet limit; surface it here rather than at send.
  const size = tx.serialize().length;
  if (size > 1232) throw new Error(`transaction is ${size} bytes`);
  return { tx, blockhash, lastValidBlockHeight };
}

export interface Simulation {
  err: unknown;
  logs: string[];
  unitsConsumed: number;
}

export async function simulate(connection: Connection, tx: VersionedTransaction): Promise<Simulation> {
  const { value } = await connection.simulateTransaction(tx, { sigVerify: false, commitment: "confirmed" });
  return { err: value.err, logs: value.logs ?? [], unitsConsumed: value.unitsConsumed ?? 0 };
}

/** Compute units to request after a simulation used `used`: a margin for state that moves before landing. */
export function computeBudget(used: number): number {
  return Math.min(MAX_COMPUTE_UNITS, Math.ceil(used * 1.2) + 20_000);
}

export { MAX_COMPUTE_UNITS };

export async function sendAndConfirm(connection: Connection, built: Built): Promise<string> {
  const signature = await connection.sendRawTransaction(built.tx.serialize(), { skipPreflight: true, maxRetries: 5 });
  const { value } = await connection.confirmTransaction(
    { signature, blockhash: built.blockhash, lastValidBlockHeight: built.lastValidBlockHeight },
    "confirmed",
  );
  if (value.err) {
    const failed = await connection.getTransaction(signature, { maxSupportedTransactionVersion: 0, commitment: "confirmed" });
    throw new LandedFailure(signature, value.err, failed?.meta?.logMessages ?? []);
  }
  return signature;
}

export class LandedFailure extends Error {
  constructor(
    readonly signature: string,
    readonly err: unknown,
    readonly logs: string[],
  ) {
    super(`${signature} failed: ${JSON.stringify(err)}`);
  }
}
