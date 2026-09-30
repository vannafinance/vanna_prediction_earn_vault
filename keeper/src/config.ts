import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Keypair, PublicKey } from "@solana/web3.js";
import { DEFAULT_POOL_PROGRAM_ID } from "./constants.ts";
import type { JupiterOptions } from "./jupiter.ts";

export interface KeeperConfig {
  rpcUrl: string;
  /** Defaults to the RPC URL's WebSocket endpoint, as web3.js derives it. */
  wsUrl?: string;
  keypairPath: string;
  programId: PublicKey;
  jupiter: JupiterOptions;
  /** Smallest complete-set count worth a merge, in raw CASH units. */
  minMerge: bigint;
  /** Margin the quote must clear above the program's floor, so price drift before landing doesn't waste a send. */
  headroomBps: bigint;
  /** How long after an event to wait, so a burst of deposits merges once. */
  debounceMs: number;
  /** Full scan of every pool, which catches anything the event stream missed. */
  sweepMs: number;
  /** Wait before re-quoting a pool whose price was below the floor. */
  priceRetryMs: number;
  priorityMicroLamports: number;
  stateFile: string;
  /** Simulate only, never send. */
  dryRun: boolean;
}

function number(env: NodeJS.ProcessEnv, name: string, fallback: number): number {
  const raw = env[name];
  if (raw === undefined || raw === "") return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value) || value < 0) throw new Error(`${name} must be a non-negative number, got "${raw}"`);
  return value;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): KeeperConfig {
  return {
    rpcUrl: env.RPC_URL ?? "http://127.0.0.1:8899",
    wsUrl: env.WS_URL || undefined,
    keypairPath: env.KEEPER_KEYPAIR ?? path.join(os.homedir(), ".config", "solana", "id.json"),
    programId: env.POOL_PROGRAM_ID ? new PublicKey(env.POOL_PROGRAM_ID) : DEFAULT_POOL_PROGRAM_ID,
    jupiter: {
      api: (env.JUPITER_API ?? "https://lite-api.jup.ag/swap/v1").replace(/\/$/, ""),
      apiKey: env.JUPITER_API_KEY || undefined,
      slippageBps: number(env, "SLIPPAGE_BPS", 20),
      maxAccounts: number(env, "MAX_ACCOUNTS", 32),
      onlyDirectRoutes: (env.ONLY_DIRECT_ROUTES ?? "true") !== "false",
    },
    minMerge: BigInt(env.MIN_MERGE ?? "1000000"),
    headroomBps: BigInt(env.HEADROOM_BPS ?? "1"),
    debounceMs: number(env, "DEBOUNCE_MS", 3_000),
    sweepMs: number(env, "SWEEP_MS", 300_000),
    priceRetryMs: number(env, "PRICE_RETRY_MS", 60_000),
    priorityMicroLamports: number(env, "PRIORITY_MICROLAMPORTS", 10_000),
    stateFile: env.STATE_FILE ?? "keeper-state.json",
    dryRun: env.DRY_RUN === "true",
  };
}

export function loadKeypair(file: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf8"))));
}
