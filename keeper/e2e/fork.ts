// End-to-end setup on a Surfpool mainnet fork, so the keeper can be run against real World markets, real Jupiter
// routes and a Vanna USDC reserve. Uses Surfpool cheatcodes, so it refuses any RPC that is not local.
//
//   surfpool start --network mainnet --no-tui --no-deploy -y
//   solana program deploy --use-rpc -u localhost --program-id ../target/deploy/outcome_yield_pool-keypair.json \
//     ../target/deploy/outcome_yield_pool.so
//   npm run fork -- vanna ../fixtures/vanna/vanna_lending.so   # Vanna at its program id (a build of it, via cheatcode)
//   npm run fork -- setup [market]                             # Vanna USDC reserve, pool config, allowlist, pool
//   npm run fork -- deposit yes 50                             # a user supplies 50 YES; `no` for the other side
//   npm run fork -- status
//
// The pool config's keeper is `fork-keeper.json` (created here); run the keeper with KEEPER_KEYPAIR pointing at it.

import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  AccountMeta,
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  CASH_MINT,
  DEFAULT_POOL_PROGRAM_ID,
  SYSTEM_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  USDC_MINT,
  VANNA_PROGRAM_ID,
  configPda,
} from "../src/constants.ts";
import { decodePool, tokenAmount } from "../src/codec.ts";
import { formatUnits } from "../src/keeper.ts";

const RPC = process.env.RPC_URL ?? "http://127.0.0.1:8899";
if (!/^https?:\/\/(127\.0\.0\.1|localhost)(:\d+)?\/?$/.test(RPC)) throw new Error(`refusing non-local RPC ${RPC}`);
const connection = new Connection(RPC, "confirmed");
const PROGRAM = process.env.POOL_PROGRAM_ID ? new PublicKey(process.env.POOL_PROGRAM_ID) : DEFAULT_POOL_PROGRAM_ID;
const LOADER = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
/** World's "Bitcoin 2026 Year-High / Above $99,999.99", open until the end of 2026. */
const DEFAULT_MARKET = "4jdharW9HB7FRB46qgkvy1aCyNXEAaLWzkCCx8vgbCV2";
const HERE = path.dirname(new URL(import.meta.url).pathname);
const KEEPER_LOSS_BPS = 10;

const sighash = (name: string) => createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
const w = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: true });
const r = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: false });
const pda = (seeds: (Buffer | Uint8Array)[], program: PublicKey) => PublicKey.findProgramAddressSync(seeds, program)[0];
const ata = (owner: PublicKey, mint: PublicKey, tokenProgram: PublicKey) =>
  pda([owner.toBuffer(), tokenProgram.toBuffer(), mint.toBuffer()], ASSOCIATED_TOKEN_PROGRAM_ID);
const u64 = (n: bigint | number) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n));
  return b;
};
const u16 = (n: number) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return b;
};

function keypair(file: string): Keypair {
  if (!fs.existsSync(file)) fs.writeFileSync(file, JSON.stringify([...Keypair.generate().secretKey]));
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf8"))));
}
const admin = keypair(process.env.ADMIN_KEYPAIR ?? path.join(os.homedir(), ".config", "solana", "id.json"));
const keeper = keypair(path.join(HERE, "fork-keeper.json"));
const user = keypair(path.join(HERE, "fork-user.json"));

async function cheat(method: string, params: unknown[]): Promise<unknown> {
  const res = await fetch(RPC, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const json = await res.json();
  if (json.error) throw new Error(`${method}: ${JSON.stringify(json.error)}`);
  return json.result;
}

async function send(ixs: TransactionInstruction[], signers: Keypair[], label: string): Promise<void> {
  const sig = await sendAndConfirmTransaction(connection, new Transaction().add(...ixs), signers, {
    commitment: "confirmed",
  });
  console.log(`✔ ${label}: ${sig}`);
}

async function exists(key: PublicKey): Promise<boolean> {
  return (await connection.getAccountInfo(key)) !== null;
}

/* VANNA (mirrors the test harness's `vanna` module) */

const vanna = {
  protocol: pda([Buffer.from("protocol")], VANNA_PROGRAM_ID),
  asset: pda([Buffer.from("asset"), USDC_MINT.toBuffer()], VANNA_PROGRAM_ID),
  reserve: pda([Buffer.from("reserve"), USDC_MINT.toBuffer()], VANNA_PROGRAM_ID),
  shareMint: pda([Buffer.from("share_mint"), USDC_MINT.toBuffer()], VANNA_PROGRAM_ID),
  get vault() {
    return ata(this.reserve, USDC_MINT, TOKEN_PROGRAM_ID);
  },
};

function vannaIx(disc: number[], keys: AccountMeta[], args: Buffer): TransactionInstruction {
  return new TransactionInstruction({ programId: VANNA_PROGRAM_ID, keys, data: Buffer.concat([Buffer.from(disc), args]) });
}

async function setupVanna(): Promise<void> {
  const a = admin.publicKey;
  if (!(await exists(vanna.protocol))) {
    const args = Buffer.concat([Keypair.generate().publicKey.toBuffer(), Buffer.from([8])]);
    const ix = vannaIx([188, 233, 252, 106, 134, 146, 202, 91], [w(a, true), r(a, true), w(vanna.protocol), r(SYSTEM_PROGRAM_ID)], args);
    await send([ix], [admin], "Vanna initialize_protocol");
  }
  if (!(await exists(vanna.reserve))) {
    const feed = Buffer.alloc(32, 1);
    const register = vannaIx(
      [102, 222, 185, 83, 217, 237, 73, 58],
      [r(a, true), w(a, true), w(vanna.protocol), r(USDC_MINT), w(vanna.asset), r(TOKEN_PROGRAM_ID), r(SYSTEM_PROGRAM_ID)],
      Buffer.concat([feed, u64(0), u16(8_000), u16(8_500), u16(500), u16(1_000), Buffer.from([16, 14, 0, 0]), Buffer.from([1, 1])]),
    );
    const curve = [100_000_000_000_000_000n, 300_000_000_000_000_000n, 3_500_000_000_000_000_000n];
    const initialize = vannaIx(
      [112, 120, 94, 208, 244, 120, 140, 116],
      [
        r(a, true),
        w(a, true),
        r(vanna.protocol),
        w(vanna.asset),
        r(USDC_MINT),
        w(vanna.reserve),
        w(vanna.vault),
        w(vanna.shareMint),
        r(TOKEN_PROGRAM_ID),
        r(TOKEN_PROGRAM_ID),
        r(ASSOCIATED_TOKEN_PROGRAM_ID),
        r(SYSTEM_PROGRAM_ID),
      ],
      Buffer.concat([...curve.map(u64), u16(1_000), u64(0), u64(0), Buffer.from([0])]),
    );
    await send([register, initialize], [admin], "Vanna USDC asset + reserve");
  }
  // A seed lender, as a live reserve would have.
  const seedUsdc = ata(a, USDC_MINT, TOKEN_PROGRAM_ID);
  const seedShares = ata(a, vanna.shareMint, TOKEN_PROGRAM_ID);
  if (!(await exists(seedShares))) {
    await cheat("surfnet_setTokenAccount", [a.toBase58(), USDC_MINT.toBase58(), { amount: 10_000_000_000 }, TOKEN_PROGRAM_ID.toBase58()]);
    const supply = vannaIx(
      [52, 247, 77, 250, 74, 122, 100, 33],
      [
        w(a, true),
        r(vanna.protocol),
        r(vanna.asset),
        w(vanna.reserve),
        r(USDC_MINT),
        w(seedUsdc),
        w(vanna.vault),
        w(vanna.shareMint),
        w(seedShares),
        r(TOKEN_PROGRAM_ID),
        r(TOKEN_PROGRAM_ID),
        r(ASSOCIATED_TOKEN_PROGRAM_ID),
        r(SYSTEM_PROGRAM_ID),
      ],
      Buffer.concat([u64(1_000_000_000), u64(1)]),
    );
    await send([supply], [admin], "Vanna seed lender supplies 1,000 USDC");
  }
}

/* POOL */

interface Market {
  market: PublicKey;
  yesMint: PublicKey;
  noMint: PublicKey;
}

async function readMarket(market: PublicKey): Promise<Market> {
  const account = await connection.getAccountInfo(market);
  if (!account) throw new Error(`market ${market.toBase58()} not found`);
  return {
    market,
    yesMint: new PublicKey(account.data.subarray(72, 104)),
    noMint: new PublicKey(account.data.subarray(104, 136)),
  };
}

function poolAddresses(m: Market) {
  const pool = pda([Buffer.from("pool"), m.market.toBuffer(), vanna.reserve.toBuffer()], PROGRAM);
  return {
    pool,
    allowed: pda([Buffer.from("allowed_market"), m.market.toBuffer()], PROGRAM),
    yesShares: pda([Buffer.from("yes_shares"), pool.toBuffer()], PROGRAM),
    noShares: pda([Buffer.from("no_shares"), pool.toBuffer()], PROGRAM),
    cash: ata(pool, CASH_MINT, TOKEN_2022_PROGRAM_ID),
    yes: ata(pool, m.yesMint, TOKEN_2022_PROGRAM_ID),
    no: ata(pool, m.noMint, TOKEN_2022_PROGRAM_ID),
    usdc: ata(pool, USDC_MINT, TOKEN_PROGRAM_ID),
    vannaShares: ata(pool, vanna.shareMint, TOKEN_PROGRAM_ID),
  };
}

function vannaGroup(p: ReturnType<typeof poolAddresses>): AccountMeta[] {
  return [
    r(VANNA_PROGRAM_ID),
    r(vanna.protocol),
    r(vanna.asset),
    w(vanna.reserve),
    r(USDC_MINT),
    w(vanna.vault),
    w(vanna.shareMint),
    w(p.usdc),
    w(p.vannaShares),
    r(TOKEN_PROGRAM_ID),
    r(ASSOCIATED_TOKEN_PROGRAM_ID),
    r(SYSTEM_PROGRAM_ID),
  ];
}

function poolIx(name: string, keys: AccountMeta[], args: Buffer = Buffer.alloc(0)): TransactionInstruction {
  return new TransactionInstruction({ programId: PROGRAM, keys, data: Buffer.concat([sighash(name), args]) });
}

async function setup(marketArg: string): Promise<void> {
  for (const k of [keeper.publicKey, user.publicKey]) {
    if ((await connection.getBalance(k)) < LAMPORTS_PER_SOL) {
      await connection.confirmTransaction(await connection.requestAirdrop(k, 100 * LAMPORTS_PER_SOL), "confirmed");
    }
  }
  await setupVanna();

  const config = configPda(PROGRAM);
  if (!(await exists(config))) {
    const programData = pda([PROGRAM.toBuffer()], LOADER);
    const args = Buffer.concat([admin.publicKey.toBuffer(), keeper.publicKey.toBuffer(), u16(KEEPER_LOSS_BPS)]);
    const ix = poolIx(
      "initialize_config",
      [w(admin.publicKey, true), w(config), r(PROGRAM), r(programData), r(SYSTEM_PROGRAM_ID)],
      args,
    );
    await send([ix], [admin], `pool config (keeper ${keeper.publicKey.toBase58()}, ${KEEPER_LOSS_BPS} bps)`);
  }

  const m = await readMarket(new PublicKey(marketArg));
  const p = poolAddresses(m);
  if (!(await exists(p.allowed))) {
    const ix = poolIx("allow_market", [w(admin.publicKey, true), r(config), r(m.market), w(p.allowed), r(SYSTEM_PROGRAM_ID)]);
    await send([ix], [admin], `allow market ${m.market.toBase58()}`);
  }
  if (!(await exists(p.pool))) {
    const ix = poolIx("create_pool", [
      w(admin.publicKey, true),
      r(p.allowed),
      r(m.market),
      r(vanna.reserve),
      r(USDC_MINT),
      r(vanna.shareMint),
      r(CASH_MINT),
      r(m.yesMint),
      r(m.noMint),
      w(p.pool),
      w(p.yesShares),
      w(p.noShares),
      w(p.cash),
      w(p.yes),
      w(p.no),
      w(p.usdc),
      w(p.vannaShares),
      r(TOKEN_2022_PROGRAM_ID),
      r(TOKEN_PROGRAM_ID),
      r(ASSOCIATED_TOKEN_PROGRAM_ID),
      r(SYSTEM_PROGRAM_ID),
    ]);
    await send([ix], [admin], `create pool ${p.pool.toBase58()}`);
  }
  fs.writeFileSync(path.join(HERE, "fork-market.txt"), m.market.toBase58());
  console.log(`pool ${p.pool.toBase58()} ready; keeper wallet ${keeper.publicKey.toBase58()}`);
}

function createAtaIdempotent(payer: PublicKey, owner: PublicKey, mint: PublicKey, tokenProgram: PublicKey) {
  return new TransactionInstruction({
    programId: ASSOCIATED_TOKEN_PROGRAM_ID,
    keys: [w(payer, true), w(ata(owner, mint, tokenProgram)), r(owner), r(mint), r(SYSTEM_PROGRAM_ID), r(tokenProgram)],
    data: Buffer.from([1]),
  });
}

async function deposit(side: string, whole: string): Promise<void> {
  const isYes = side === "yes";
  if (!isYes && side !== "no") throw new Error("side must be yes or no");
  const amount = BigInt(Math.round(Number(whole) * 1e6));
  const m = await readMarket(new PublicKey(fs.readFileSync(path.join(HERE, "fork-market.txt"), "utf8").trim()));
  const p = poolAddresses(m);
  const mint = isYes ? m.yesMint : m.noMint;
  const shareMint = isYes ? p.yesShares : p.noShares;
  const u = user.publicKey;
  const userOutcome = ata(u, mint, TOKEN_2022_PROGRAM_ID);

  // Give the user outcome tokens the way a World trade would leave them: in their Token-2022 ATA.
  await send([createAtaIdempotent(u, u, mint, TOKEN_2022_PROGRAM_ID), createAtaIdempotent(u, u, shareMint, TOKEN_2022_PROGRAM_ID)], [user], "user token accounts");
  const held = tokenAmount((await connection.getAccountInfo(userOutcome))!.data);
  await cheat("surfnet_setTokenAccount", [u.toBase58(), mint.toBase58(), { amount: Number(held + amount) }, TOKEN_2022_PROGRAM_ID.toBase58()]);

  const ix = poolIx(
    "deposit",
    [
      r(u, true),
      r(p.pool),
      r(m.market),
      r(mint),
      w(userOutcome),
      w(isYes ? p.yes : p.no),
      w(shareMint),
      w(ata(u, shareMint, TOKEN_2022_PROGRAM_ID)),
      ...vannaGroup(p),
      r(TOKEN_2022_PROGRAM_ID),
    ],
    Buffer.concat([Buffer.from([isYes ? 1 : 0]), u64(amount)]),
  );
  await send([ix], [user], `deposit ${whole} ${side.toUpperCase()}`);
}

async function status(): Promise<void> {
  const m = await readMarket(new PublicKey(fs.readFileSync(path.join(HERE, "fork-market.txt"), "utf8").trim()));
  const p = poolAddresses(m);
  const pool = decodePool(p.pool, (await connection.getAccountInfo(p.pool))!.data);
  const keys = [pool.poolYes, pool.poolNo, pool.poolCash, pool.poolUsdc, pool.poolVannaShares];
  const infos = await connection.getMultipleAccountsInfo(keys);
  const [yes, no, cash, usdc, shares] = infos.map((i) => (i ? tokenAmount(i.data) : 0n));
  console.log(`pool ${p.pool.toBase58()} (${pool.open ? "open" : "settled"})`);
  console.log(`  dangling YES ${formatUnits(yes)}  NO ${formatUnits(no)}  complete sets ${formatUnits(yes < no ? yes : no)}`);
  console.log(`  idle CASH ${formatUnits(cash)}  idle USDC ${formatUnits(usdc)}  Vanna shares ${shares}`);
}

async function putVanna(elf: string): Promise<void> {
  const program = VANNA_PROGRAM_ID;
  const programData = pda([program.toBuffer()], LOADER);
  const header = Buffer.alloc(45);
  header.writeUInt32LE(3, 0); // UpgradeableLoaderState::ProgramData
  header.writeBigUInt64LE(BigInt(Math.max(0, (await connection.getSlot()) - 10)), 4);
  header[12] = 1;
  admin.publicKey.toBuffer().copy(header, 13);
  const data = Buffer.concat([header, fs.readFileSync(elf)]);
  const programAccount = Buffer.alloc(36);
  programAccount.writeUInt32LE(2, 0); // UpgradeableLoaderState::Program
  programData.toBuffer().copy(programAccount, 4);
  const rent = (n: number) => connection.getMinimumBalanceForRentExemption(n);
  const set = async (key: PublicKey, bytes: Buffer, executable: boolean) =>
    cheat("surfnet_setAccount", [
      key.toBase58(),
      { lamports: await rent(bytes.length), data: bytes.toString("hex"), owner: LOADER.toBase58(), executable },
    ]);
  await set(programData, data, false);
  await set(program, programAccount, true);
  console.log(`✔ Vanna at ${program.toBase58()}`);
}

const [command, ...args] = process.argv.slice(2);
const commands: Record<string, () => Promise<void>> = {
  vanna: () => putVanna(args[0] ?? path.join(HERE, "..", "..", "fixtures", "vanna", "vanna_lending.so")),
  setup: () => setup(args[0] ?? DEFAULT_MARKET),
  deposit: () => deposit(args[0], args[1]),
  status,
};
if (!commands[command]) {
  console.error(`usage: fork <${Object.keys(commands).join("|")}>`);
  process.exit(1);
}
commands[command]().then(
  () => process.exit(0),
  (e) => {
    console.error(e instanceof Error ? (e.stack ?? e.message) : e);
    if (e?.logs) console.error(e.logs.join("\n"));
    process.exit(1);
  },
);
