// Byte-level views of the pool program: discriminators, account layouts, events in logs, errors. No Anchor client: the
// layouts are small and fixed, and reading them directly keeps the keeper independent of IDL tooling versions.

import { createHash } from "node:crypto";
import { PublicKey } from "@solana/web3.js";
import { POOL_ERRORS, type PoolErrorName } from "./constants.ts";

function discriminator(preimage: string): Buffer {
  return createHash("sha256").update(preimage).digest().subarray(0, 8);
}

export const DISCRIMINATORS = {
  mergeAndDeposit: discriminator("global:merge_and_deposit"),
  pool: discriminator("account:Pool"),
  config: discriminator("account:Config"),
  depositEvent: discriminator("event:Deposit"),
  redeemEvent: discriminator("event:Redeem"),
  mergeEvent: discriminator("event:MergeAndDeposit"),
  swappedEvent: discriminator("event:Swapped"),
};

/** `Pool`'s seventeen addresses, in account order. */
const POOL_FIELDS = [
  "market",
  "vannaReserve",
  "vannaProtocolConfig",
  "vannaAssetConfig",
  "vannaLiquidityVault",
  "vannaShareMint",
  "collateralMint",
  "yesMint",
  "noMint",
  "marketCashVault",
  "yesShareMint",
  "noShareMint",
  "poolCash",
  "poolYes",
  "poolNo",
  "poolUsdc",
  "poolVannaShares",
] as const;

export type PoolState = { [K in (typeof POOL_FIELDS)[number]]: PublicKey } & {
  address: PublicKey;
  /** `PoolStatus::Open`. A settled pool never merges again. */
  open: boolean;
};

export function decodePool(address: PublicKey, data: Buffer): PoolState {
  if (!data.subarray(0, 8).equals(DISCRIMINATORS.pool)) throw new Error(`${address.toBase58()} is not a Pool`);
  const pool = { address } as PoolState;
  POOL_FIELDS.forEach((field, i) => {
    pool[field] = new PublicKey(data.subarray(8 + 32 * i, 40 + 32 * i));
  });
  pool.open = data[8 + 32 * POOL_FIELDS.length] === 0;
  return pool;
}

export interface ConfigState {
  admin: PublicKey;
  keeper: PublicKey;
  maxSwapLossBps: number;
}

export function decodeConfig(data: Buffer): ConfigState {
  if (!data.subarray(0, 8).equals(DISCRIMINATORS.config)) throw new Error("not the pool Config");
  return {
    admin: new PublicKey(data.subarray(8, 40)),
    keeper: new PublicKey(data.subarray(72, 104)),
    maxSwapLossBps: data.readUInt16LE(104),
  };
}

/** A classic SPL or Token-2022 account's balance. */
export function tokenAmount(data: Buffer): bigint {
  return data.readBigUInt64LE(64);
}

/** prediCt `Market`: a non-zero determined slot means World has written the outcome, after which merges are refused. */
export function marketDetermined(data: Buffer): boolean {
  return data.readBigUInt64LE(176) !== 0n;
}

/** The pool program's `emit!` payloads in a transaction's logs, skipping any another program printed. */
export function programEvents(logs: readonly string[], programId: PublicKey): Buffer[] {
  const id = programId.toBase58();
  const stack: string[] = [];
  const events: Buffer[] = [];
  for (const line of logs) {
    const invoke = /^Program (\S+) invoke \[\d+\]$/.exec(line);
    if (invoke) {
      stack.push(invoke[1]);
    } else if (/^Program \S+ (success|failed)/.test(line)) {
      stack.pop();
    } else if (line.startsWith("Program data: ") && stack[stack.length - 1] === id) {
      events.push(Buffer.from(line.slice("Program data: ".length), "base64"));
    }
  }
  return events;
}

/**
 * Pools whose dangling YES/NO balances an event changed: `Deposit` adds one side, a `Redeem` that splits leaves the
 * other side behind. Both events start with the pool's address.
 */
export function touchedPools(logs: readonly string[], programId: PublicKey): PublicKey[] {
  const pools = new Map<string, PublicKey>();
  for (const event of programEvents(logs, programId)) {
    const tag = event.subarray(0, 8);
    if (tag.equals(DISCRIMINATORS.depositEvent) || tag.equals(DISCRIMINATORS.redeemEvent)) {
      const pool = new PublicKey(event.subarray(8, 40));
      pools.set(pool.toBase58(), pool);
    }
  }
  return [...pools.values()];
}

/** `merge_and_deposit(swap_data: Vec<u8>)`. */
export function encodeMergeData(swapData: Buffer): Buffer {
  const length = Buffer.alloc(4);
  length.writeUInt32LE(swapData.length);
  return Buffer.concat([DISCRIMINATORS.mergeAndDeposit, length, swapData]);
}

export interface Failure {
  /** The innermost program that failed. */
  program: string;
  message: string;
  /** Set when the pool program itself refused. */
  poolError?: PoolErrorName;
}

/** Explains a failed transaction from its logs: the first `failed` line is the innermost failing program. */
export function explainFailure(logs: readonly string[] | null | undefined, programId: PublicKey): Failure | undefined {
  for (const line of logs ?? []) {
    const m = /^Program (\S+) failed: (.+)$/.exec(line);
    if (!m) continue;
    const failure: Failure = { program: m[1], message: m[2] };
    const custom = /custom program error: (0x[0-9a-f]+)/.exec(m[2]);
    if (custom && m[1] === programId.toBase58()) {
      failure.poolError = POOL_ERRORS[Number.parseInt(custom[1], 16) - 6000];
    }
    return failure;
  }
  return undefined;
}
