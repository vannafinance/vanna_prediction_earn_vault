// Jupiter's swap API, and the checks the pool program makes on a route (`src/jupiter.rs`), run before sending so a
// route the pool would refuse costs nothing.

import { AccountMeta, PublicKey } from "@solana/web3.js";
import { PREDICT_PROGRAM_ID, JUPITER_PROGRAM_ID, USDC_MINT, VANNA_PROGRAM_ID } from "./constants.ts";
import type { PoolState } from "./codec.ts";

const ROUTE = Buffer.from([229, 23, 203, 151, 122, 227, 173, 42]);
const SHARED_ACCOUNTS_ROUTE = Buffer.from([193, 32, 155, 51, 65, 214, 156, 129]);
/** `in_amount: u64, quoted_out_amount: u64, slippage_bps: u16, platform_fee_bps: u8`. */
const TAIL_LEN = 19;
const CREATE_IDEMPOTENT = 1;
const ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

export interface JupiterOptions {
  api: string;
  apiKey?: string;
  slippageBps: number;
  maxAccounts: number;
  onlyDirectRoutes: boolean;
}

export interface Quote {
  inAmount: string;
  outAmount: string;
  routePlan: { percent: number; swapInfo: { label: string; ammKey: string } }[];
  [key: string]: unknown;
}

interface ApiInstruction {
  programId: string;
  accounts: { pubkey: string; isSigner: boolean; isWritable: boolean }[];
  data: string;
}

interface SwapInstructions {
  swapInstruction: ApiInstruction;
  setupInstructions?: ApiInstruction[];
  addressLookupTableAddresses?: string[];
}

/** A route the pool will accept: its data, its accounts as `remaining_accounts`, and Jupiter's lookup tables. */
export interface Route {
  data: Buffer;
  accounts: AccountMeta[];
  lookupTables: PublicKey[];
  /** Jupiter's labels for the venues the route trades on, e.g. `Whirlpool`, `Manifest`. */
  venues: string[];
}

async function call<T>(opts: JupiterOptions, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (opts.apiKey) headers["x-api-key"] = opts.apiKey;
  const res = await fetch(opts.api + path, {
    method: body === undefined ? "GET" : "POST",
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(15_000),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`Jupiter ${path.split("?")[0]} ${res.status}: ${text.slice(0, 300)}`);
  const json = JSON.parse(text);
  if (json.error) throw new Error(`Jupiter ${path.split("?")[0]}: ${json.error}`);
  return json as T;
}

export function quote(
  opts: JupiterOptions,
  inputMint: PublicKey,
  outputMint: PublicKey,
  amount: bigint,
  excludeDexes: string[] = [],
): Promise<Quote> {
  const params = new URLSearchParams({
    inputMint: inputMint.toBase58(),
    outputMint: outputMint.toBase58(),
    amount: amount.toString(),
    slippageBps: String(opts.slippageBps),
    maxAccounts: String(opts.maxAccounts),
    onlyDirectRoutes: String(opts.onlyDirectRoutes),
    // The pool refuses any fee account, so none may be asked for.
    platformFeeBps: "0",
  });
  if (excludeDexes.length > 0) params.set("excludeDexes", excludeDexes.join(","));
  return call<Quote>(opts, `/quote?${params}`);
}

/**
 * The swap for `quote` with the pool PDA as the user, checked the way the program will check it. `route` (not
 * `shared_accounts_route`) keeps the accounts to the pool's own CASH and USDC accounts.
 */
export async function poolRoute(
  opts: JupiterOptions,
  q: Quote,
  programId: PublicKey,
  pool: PoolState,
  amount: bigint,
): Promise<Route> {
  const reply = await call<SwapInstructions>(opts, "/swap-instructions", {
    quoteResponse: q,
    userPublicKey: pool.address.toBase58(),
    wrapAndUnwrapSol: false,
    useSharedAccounts: false,
    skipUserAccountsRpcCalls: true,
    dynamicComputeUnitLimit: false,
  });
  checkSetup(reply.setupInstructions ?? [], pool);
  const ix = reply.swapInstruction;
  const data = Buffer.from(ix.data, "base64");
  const keys = ix.accounts.map((a) => new PublicKey(a.pubkey));
  checkRoute(ix.programId, data, keys, programId, pool, amount);
  return {
    data,
    // The pool PDA can't sign the transaction: the program signs for it inside the CPI. Writability passes through,
    // since some venues need the user writable.
    accounts: ix.accounts.map((a) => ({ pubkey: new PublicKey(a.pubkey), isSigner: false, isWritable: a.isWritable })),
    lookupTables: (reply.addressLookupTableAddresses ?? []).map((k) => new PublicKey(k)),
    venues: [...new Set(q.routePlan.map((s) => s.swapInfo.label))],
  };
}

/**
 * The keeper never sends Jupiter's setup instructions: they would need the pool PDA to pay. The only ones a pool route
 * may come with create the pool's own CASH/USDC accounts, which `create_pool` already made.
 */
function checkSetup(setup: ApiInstruction[], pool: PoolState): void {
  const own = new Set([pool.poolCash.toBase58(), pool.poolUsdc.toBase58()]);
  for (const ix of setup) {
    const data = Buffer.from(ix.data, "base64");
    const createsOwn = ix.programId === ATA_PROGRAM && data[0] === CREATE_IDEMPOTENT && own.has(ix.accounts[1]?.pubkey);
    if (!createsOwn) throw new RouteRejected(`route needs a setup instruction for ${ix.programId}`);
  }
}

export class RouteRejected extends Error {}

/** The program's `jupiter::plan` and role checks. */
export function checkRoute(
  jupiter: string,
  data: Buffer,
  keys: PublicKey[],
  programId: PublicKey,
  pool: PoolState,
  amount: bigint,
): void {
  const reject = (why: string) => {
    throw new RouteRejected(why);
  };
  if (jupiter !== JUPITER_PROGRAM_ID.toBase58()) reject(`not a Jupiter v6 instruction: ${jupiter}`);
  if (data.length < 12 + TAIL_LEN) reject("route data too short");
  const tail = data.subarray(data.length - TAIL_LEN);
  if (tail.readBigUInt64LE(0) !== amount) reject(`route spends ${tail.readBigUInt64LE(0)}, pool has ${amount}`);
  if (tail[TAIL_LEN - 1] !== 0) reject("route carries a platform fee");

  const unset = (i: number) => keys[i]?.equals(JUPITER_PROGRAM_ID);
  let roles: { authority: number; source: number; destination: number; sourceMint?: number; destinationMint: number };
  const selector = data.subarray(0, 8);
  if (selector.equals(ROUTE)) {
    if (keys.length < 9) reject("route has too few accounts");
    if (!(unset(4) || keys[4].equals(keys[3]))) reject("route redirects its output");
    if (!unset(6)) reject("route names a fee account");
    roles = { authority: 1, source: 2, destination: 3, destinationMint: 5 };
  } else if (selector.equals(SHARED_ACCOUNTS_ROUTE)) {
    if (keys.length < 13) reject("route has too few accounts");
    if (!unset(9)) reject("route names a fee account");
    roles = { authority: 2, source: 3, destination: 6, sourceMint: 7, destinationMint: 8 };
  } else {
    return reject(`unsupported Jupiter instruction ${selector.toString("hex")}`);
  }

  if (!keys[roles.authority].equals(pool.address)) reject("route is not signed by the pool");
  if (!keys[roles.source].equals(pool.poolCash)) reject("route does not spend the pool's CASH");
  if (!keys[roles.destination].equals(pool.poolUsdc)) reject("route does not pay the pool's USDC account");
  if (roles.sourceMint !== undefined && !keys[roles.sourceMint].equals(pool.collateralMint)) reject("wrong input mint");
  if (!keys[roles.destinationMint].equals(USDC_MINT)) reject("wrong output mint");

  const forbidden = [programId, VANNA_PROGRAM_ID, PREDICT_PROGRAM_ID, pool.poolYes, pool.poolNo, pool.poolVannaShares];
  for (const key of keys) {
    if (forbidden.some((f) => f.equals(key))) reject(`route touches ${key.toBase58()}`);
  }
}
