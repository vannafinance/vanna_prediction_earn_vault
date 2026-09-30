// The loop: events and a periodic sweep decide which pools to look at, one merge runs at a time.

import bs58 from "bs58";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import type { KeeperConfig } from "./config.ts";
import {
  BASIS_POINTS,
  CASH_MINT,
  JUPITER_PROGRAM_ID,
  SWAP_DUST_UNITS,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  USDC_MINT,
  configPda,
} from "./constants.ts";
import {
  DISCRIMINATORS,
  decodeConfig,
  decodePool,
  explainFailure,
  marketDetermined,
  tokenAmount,
  touchedPools,
  type ConfigState,
  type PoolState,
} from "./codec.ts";
import { poolRoute, quote, RouteRejected } from "./jupiter.ts";
import { LookupTables } from "./lookup.ts";
import {
  LandedFailure,
  MAX_COMPUTE_UNITS,
  build,
  computeBudget,
  lookupAddresses,
  mergeIx,
  sendAndConfirm,
  simulate,
} from "./merge.ts";
import { log } from "./log.ts";

/** What one look at a pool came to. */
type Outcome =
  | { kind: "merged"; signature: string }
  | { kind: "idle" }
  /** Dry run: the merge simulated cleanly and was not sent. */
  | { kind: "simulated" }
  /** Nothing to do until the pool is settled or gone; stop watching it. */
  | { kind: "closed"; why: string }
  /** Look again later: after `afterMs`, or with the pool's exponential backoff when unset. */
  | { kind: "retry"; afterMs?: number; why: string };

interface Watch {
  timer?: NodeJS.Timeout;
  /** Waiting in, or running from, the merge queue. */
  busy: boolean;
  /** An event arrived while busy: look again afterwards. */
  dirty: boolean;
  failures: number;
  closed: boolean;
}

const BACKOFF_MS = 30_000;
const MAX_BACKOFF_MS = 15 * 60_000;
const RACE_RETRY_MS = 1_000;
const CONFIG_TTL_MS = 60_000;
/** How long a venue that refused a pool swap is left out of quotes. */
const VENUE_BAN_MS = 15 * 60_000;

export function formatUnits(raw: bigint): string {
  const whole = raw / 1_000_000n;
  const frac = (raw % 1_000_000n).toString().padStart(6, "0");
  return `${whole}.${frac}`;
}

export class Keeper {
  private readonly lookup: LookupTables;
  private readonly watches = new Map<string, Watch>();
  private queue: Promise<void> = Promise.resolve();
  private config?: { value: ConfigState; at: number };
  private subscription?: number;
  private sweeper?: NodeJS.Timeout;
  private warnedNotKeeper = false;
  /** Venue label -> time until which quotes leave it out. */
  private readonly avoided = new Map<string, number>();

  constructor(
    private readonly cfg: KeeperConfig,
    private readonly connection: Connection,
    private readonly payer: Keypair,
  ) {
    this.lookup = new LookupTables(connection, payer, cfg.stateFile);
  }

  async start(): Promise<void> {
    await this.describe();
    this.subscription = this.connection.onLogs(
      this.cfg.programId,
      ({ err, logs, signature }) => {
        if (err) return;
        for (const pool of touchedPools(logs, this.cfg.programId)) {
          this.schedule(pool, this.cfg.debounceMs, `event ${signature.slice(0, 8)}…`);
        }
      },
      "confirmed",
    );
    log.info(`listening for Deposit / Redeem events of ${this.cfg.programId.toBase58()}`);
    await this.sweepAndSchedule();
    this.sweeper = setInterval(() => {
      this.sweepAndSchedule().catch((e) => log.error(`sweep failed: ${message(e)}`));
    }, this.cfg.sweepMs);
  }

  async stop(): Promise<void> {
    if (this.sweeper) clearInterval(this.sweeper);
    if (this.subscription !== undefined) await this.connection.removeOnLogsListener(this.subscription);
    for (const watch of this.watches.values()) clearTimeout(watch.timer);
    await this.queue;
  }

  /** One pass over every pool, merging each that needs it, then return. For cron-style runs. */
  async runOnce(): Promise<void> {
    await this.describe();
    for (const pool of await this.sweep()) await this.runPoolNow(pool);
  }

  /** One pool, retrying at once while the only fix is a fresh quote (a race, or a venue now left out). */
  async runPoolNow(pool: PublicKey): Promise<Outcome> {
    let outcome = await this.runPool(pool);
    for (let attempt = 1; attempt < 5 && outcome.kind === "retry" && outcome.afterMs === RACE_RETRY_MS; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, RACE_RETRY_MS));
      outcome = await this.runPool(pool);
    }
    return outcome;
  }

  async runPool(pool: PublicKey): Promise<Outcome> {
    const outcome = await this.inspect(pool).catch((e): Outcome => this.failure(e));
    this.report(pool, outcome);
    return outcome;
  }

  private async describe(): Promise<void> {
    const config = await this.poolConfig();
    const lamports = await this.connection.getBalance(this.payer.publicKey);
    log.info(
      `keeper ${this.payer.publicKey.toBase58()} (${(lamports / 1e9).toFixed(4)} SOL), program ` +
        `${this.cfg.programId.toBase58()}, config keeper ${config.keeper.toBase58()} ` +
        `(max loss ${config.maxSwapLossBps} bps)${this.cfg.dryRun ? ", DRY RUN" : ""}`,
    );
    if (lamports < 0.05e9) log.warn("keeper balance is low: lookup tables and fees are paid from it");
  }

  /** Open pools holding at least `minMerge` complete sets. */
  async sweep(): Promise<PublicKey[]> {
    const accounts = await this.connection.getProgramAccounts(this.cfg.programId, {
      filters: [{ memcmp: { offset: 0, bytes: bs58.encode(DISCRIMINATORS.pool) } }],
    });
    const open = accounts.map((a) => decodePool(a.pubkey, a.account.data)).filter((p) => p.open);
    const due: PublicKey[] = [];
    for (let i = 0; i < open.length; i += 50) {
      const chunk = open.slice(i, i + 50);
      const balances = await this.connection.getMultipleAccountsInfo(chunk.flatMap((p) => [p.poolYes, p.poolNo]));
      chunk.forEach((pool, j) => {
        const [yes, no] = [balances[2 * j], balances[2 * j + 1]];
        if (!yes || !no) return;
        const sets = min(tokenAmount(yes.data), tokenAmount(no.data));
        if (sets >= this.cfg.minMerge) due.push(pool.address);
      });
    }
    log.info(`sweep: ${open.length} open pool(s), ${due.length} with complete sets to merge`);
    return due;
  }

  private async sweepAndSchedule(): Promise<void> {
    for (const pool of await this.sweep()) this.schedule(pool, 0, "sweep");
  }

  private schedule(pool: PublicKey, delayMs: number, reason: string): void {
    const key = pool.toBase58();
    let watch = this.watches.get(key);
    if (!watch) {
      watch = { busy: false, dirty: false, failures: 0, closed: false };
      this.watches.set(key, watch);
    }
    if (watch.closed) return;
    if (watch.busy) {
      watch.dirty = true;
      return;
    }
    if (watch.timer) return;
    const w = watch;
    w.timer = setTimeout(() => {
      w.timer = undefined;
      w.busy = true;
      this.queue = this.queue
        .then(() => this.process(pool, w, reason))
        .catch((e) => {
          w.busy = false;
          log.error(`pool ${short(pool)}: ${message(e)}`);
        });
    }, delayMs);
  }

  private async process(pool: PublicKey, watch: Watch, reason: string): Promise<void> {
    log.info(`pool ${short(pool)}: checking (${reason})`);
    const outcome = await this.runPool(pool);
    watch.busy = false;
    switch (outcome.kind) {
      case "merged":
        watch.failures = 0;
        // Deposits that landed while this merge was built are left for the next look.
        this.schedule(pool, this.cfg.debounceMs, "after merge");
        return;
      case "idle":
      case "simulated":
        watch.failures = 0;
        break;
      case "closed":
        watch.closed = true;
        return;
      case "retry":
        watch.failures += 1;
        this.schedule(pool, outcome.afterMs ?? Math.min(MAX_BACKOFF_MS, BACKOFF_MS * 2 ** (watch.failures - 1)), "retry");
        return;
    }
    if (watch.dirty) {
      watch.dirty = false;
      this.schedule(pool, this.cfg.debounceMs, "event during merge");
    }
  }

  private report(pool: PublicKey, outcome: Outcome): void {
    switch (outcome.kind) {
      case "merged":
      case "simulated":
        return; // logged where it happened, with the amounts
      case "idle":
        return log.info(`pool ${short(pool)}: nothing to merge`);
      case "closed":
        return log.info(`pool ${short(pool)}: no longer merging (${outcome.why})`);
      case "retry": {
        const when = outcome.afterMs === undefined ? "with backoff" : `in ${Math.round(outcome.afterMs / 1000)}s`;
        return log.warn(`pool ${short(pool)}: ${outcome.why}; retrying ${when}`);
      }
    }
  }

  private async poolConfig(): Promise<ConfigState> {
    if (this.config && Date.now() - this.config.at < CONFIG_TTL_MS) return this.config.value;
    const account = await this.connection.getAccountInfo(configPda(this.cfg.programId));
    if (!account) throw new Error("pool config is not initialized");
    const value = decodeConfig(account.data);
    this.config = { value, at: Date.now() };
    if (!value.keeper.equals(this.payer.publicKey) && !this.warnedNotKeeper) {
      this.warnedNotKeeper = true;
      log.warn("this wallet is not the config's keeper: its swaps must return par, which CASH -> USDC rarely does");
    }
    return value;
  }

  /** Looks at one pool and merges it if it holds complete sets and the price clears the program's floor. */
  private async inspect(address: PublicKey): Promise<Outcome> {
    const account = await this.connection.getAccountInfo(address);
    if (!account) return { kind: "closed", why: "pool account not found" };
    const pool = decodePool(address, account.data);
    if (!pool.open) return { kind: "closed", why: "settled" };

    const [yes, no, market] = await this.connection.getMultipleAccountsInfo([pool.poolYes, pool.poolNo, pool.market]);
    if (!yes || !no) throw new Error("pool token accounts not found");
    if (!market || marketDetermined(market.data)) {
      return { kind: "closed", why: "World resolved the market, so it refuses merges; settle the pool instead" };
    }
    const sets = min(tokenAmount(yes.data), tokenAmount(no.data));
    if (sets < this.cfg.minMerge) return { kind: "idle" };

    const config = await this.poolConfig();
    const lossBps = config.keeper.equals(this.payer.publicKey) ? BigInt(config.maxSwapLossBps) : 0n;
    const scaled = (sets * (BASIS_POINTS - lossBps)) / BASIS_POINTS;
    const floor = scaled > SWAP_DUST_UNITS ? scaled - SWAP_DUST_UNITS : 0n;
    const q = await quote(this.cfg.jupiter, CASH_MINT, USDC_MINT, sets, this.avoidedVenues());
    const out = BigInt(q.outAmount);
    const needed = floor + (sets * this.cfg.headroomBps) / BASIS_POINTS;
    if (out < needed) {
      return {
        kind: "retry",
        afterMs: this.cfg.priceRetryMs,
        why: `quote ${formatUnits(out)} USDC for ${formatUnits(sets)} CASH is under the floor ${formatUnits(needed)}`,
      };
    }

    const route = await poolRoute(this.cfg.jupiter, q, this.cfg.programId, pool, sets);
    const own = await this.lookup.forPool(pool.address, lookupAddresses(this.cfg.programId, pool));
    const jupiterTables = await Promise.all(route.lookupTables.map((k) => this.connection.getAddressLookupTable(k)));
    const tables = [own, ...jupiterTables.flatMap((t) => (t.value ? [t.value] : []))];
    const ix = mergeIx(this.cfg.programId, this.payer.publicKey, pool, route);

    const trial = await build(this.connection, this.payer, ix, tables, MAX_COMPUTE_UNITS, this.cfg.priorityMicroLamports);
    const sim = await simulate(this.connection, trial.tx);
    if (sim.err) return this.failed(sim.logs, sim.err, "simulation", route.venues);
    const summary =
      `${formatUnits(sets)} CASH via ${route.venues.join(" + ")}, ` +
      `quoted ${formatUnits(out)} USDC (floor ${formatUnits(floor)})`;
    if (this.cfg.dryRun) {
      log.info(`pool ${short(address)}: dry run, would merge ${summary}; simulation used ${sim.unitsConsumed} CU`);
      return { kind: "simulated" };
    }

    const final = await build(
      this.connection,
      this.payer,
      ix,
      tables,
      computeBudget(sim.unitsConsumed),
      this.cfg.priorityMicroLamports,
    );
    let signature: string;
    try {
      signature = await sendAndConfirm(this.connection, final);
    } catch (e) {
      if (e instanceof LandedFailure) return this.failed(e.logs, e.err, `transaction ${e.signature}`, route.venues);
      throw e;
    }
    log.info(`pool ${short(address)}: merged ${summary}: ${signature}`);
    return { kind: "merged", signature };
  }

  private avoidedVenues(): string[] {
    const now = Date.now();
    for (const [venue, until] of this.avoided) if (until <= now) this.avoided.delete(venue);
    return [...this.avoided.keys()];
  }

  private failure(e: unknown): Outcome {
    if (e instanceof RouteRejected) return { kind: "retry", afterMs: this.cfg.priceRetryMs, why: `route rejected: ${e.message}` };
    return { kind: "retry", why: message(e) };
  }

  /** Decides what to do after the program (or a program it called) refused the merge. */
  private failed(logs: string[], err: unknown, stage: string, venues: string[]): Outcome {
    const failure = explainFailure(logs, this.cfg.programId);
    const why = `${stage} failed: ${failure ? `${failure.program} ${failure.poolError ?? failure.message}` : JSON.stringify(err)}`;
    switch (failure?.poolError) {
      case "PoolSettled":
        return { kind: "closed", why };
      case "SwapAmountMismatch":
      case "MergeAmountMismatch":
        // Someone else merged or deposited in between: the route no longer matches the pool's sets.
        return { kind: "retry", afterMs: RACE_RETRY_MS, why };
      case "SwapBelowMinimum":
        return { kind: "retry", afterMs: this.cfg.priceRetryMs, why };
    }
    const neutral = [this.cfg.programId, JUPITER_PROGRAM_ID, TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID].map((k) => k.toBase58());
    if (failure && !neutral.includes(failure.program)) {
      // A venue refused the pool's swap. Some check who calls them or what signs; leave the route's venues out of
      // quotes for a while and re-quote now.
      for (const venue of venues) this.avoided.set(venue, Date.now() + VENUE_BAN_MS);
      return { kind: "retry", afterMs: RACE_RETRY_MS, why: `${why}; avoiding ${venues.join(", ")} for 15 minutes` };
    }
    if (failure && failure.program === JUPITER_PROGRAM_ID.toBase58()) {
      // Jupiter's own slippage check: the price moved since the quote.
      return { kind: "retry", afterMs: this.cfg.priceRetryMs, why };
    }
    for (const line of logs.slice(-6)) log.warn(`  ${line}`);
    return { kind: "retry", why };
  }
}

function min(a: bigint, b: bigint): bigint {
  return a < b ? a : b;
}

function short(key: PublicKey): string {
  return key.toBase58().slice(0, 8) + "…";
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
