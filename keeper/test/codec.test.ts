import assert from "node:assert/strict";
import * as fs from "node:fs";
import { test } from "node:test";
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  DISCRIMINATORS,
  decodeConfig,
  decodePool,
  encodeMergeData,
  explainFailure,
  programEvents,
  touchedPools,
  type PoolState,
} from "../src/codec.ts";
import { DEFAULT_POOL_PROGRAM_ID, JUPITER_PROGRAM_ID, USDC_MINT, VANNA_PROGRAM_ID } from "../src/constants.ts";
import { checkRoute, RouteRejected } from "../src/jupiter.ts";

const IDL = new URL("../../target/idl/outcome_yield_pool.json", import.meta.url);
const PROGRAM = DEFAULT_POOL_PROGRAM_ID;
const key = () => Keypair.generate().publicKey;

test("discriminators match the program's IDL", { skip: !fs.existsSync(IDL) && "run anchor build first" }, () => {
  const idl = JSON.parse(fs.readFileSync(IDL, "utf8"));
  const find = (list: { name: string; discriminator: number[] }[], name: string) =>
    Buffer.from(list.find((x) => x.name === name)!.discriminator);
  assert.deepEqual(DISCRIMINATORS.mergeAndDeposit, find(idl.instructions, "merge_and_deposit"));
  assert.deepEqual(DISCRIMINATORS.pool, find(idl.accounts, "Pool"));
  assert.deepEqual(DISCRIMINATORS.config, find(idl.accounts, "Config"));
  assert.deepEqual(DISCRIMINATORS.depositEvent, find(idl.events, "Deposit"));
  assert.deepEqual(DISCRIMINATORS.redeemEvent, find(idl.events, "Redeem"));
  assert.deepEqual(DISCRIMINATORS.mergeEvent, find(idl.events, "MergeAndDeposit"));

  const errors: { code: number; name: string }[] = idl.errors;
  const failure = (code: number) =>
    explainFailure([`Program ${PROGRAM.toBase58()} failed: custom program error: 0x${code.toString(16)}`], PROGRAM);
  for (const { code, name } of errors) assert.equal(failure(code)?.poolError, name);
});

function depositEvent(pool: PublicKey): string {
  const data = Buffer.concat([DISCRIMINATORS.depositEvent, pool.toBuffer(), Buffer.alloc(1 + 32 + 32 + 16)]);
  return `Program data: ${data.toString("base64")}`;
}

test("events are read only from the pool program's own frames", () => {
  const pool = key();
  const lookalike = key();
  const logs = [
    `Program ${PROGRAM.toBase58()} invoke [1]`,
    "Program log: Instruction: Deposit",
    `Program ${JUPITER_PROGRAM_ID.toBase58()} invoke [2]`,
    depositEvent(lookalike), // another program printing the same bytes is ignored
    `Program ${JUPITER_PROGRAM_ID.toBase58()} consumed 100 of 200 compute units`,
    `Program ${JUPITER_PROGRAM_ID.toBase58()} success`,
    depositEvent(pool),
    `Program ${PROGRAM.toBase58()} success`,
  ];
  assert.equal(programEvents(logs, PROGRAM).length, 1);
  assert.deepEqual(
    touchedPools(logs, PROGRAM).map((p) => p.toBase58()),
    [pool.toBase58()],
  );
});

test("a failure names the innermost program", () => {
  // The PrivilegeEscalation the keeper hit before the pool passed its writability through.
  const logs = [
    `Program ${PROGRAM.toBase58()} invoke [1]`,
    `Program ${JUPITER_PROGRAM_ID.toBase58()} invoke [2]`,
    "D7svJ3inSTpi6KGyENbBEmPen2cFsSbHRRegQhwRzP1k's writable privilege escalated",
    `Program ${JUPITER_PROGRAM_ID.toBase58()} failed: Cross-program invocation with unauthorized signer or writable account`,
    `Program ${PROGRAM.toBase58()} failed: Cross-program invocation with unauthorized signer or writable account`,
  ];
  const failure = explainFailure(logs, PROGRAM)!;
  assert.equal(failure.program, JUPITER_PROGRAM_ID.toBase58());
  assert.equal(failure.poolError, undefined);

  const below = explainFailure([`Program ${PROGRAM.toBase58()} failed: custom program error: 0x1785`], PROGRAM)!;
  assert.equal(below.poolError, "SwapBelowMinimum");
});

test("merge_and_deposit data is selector, u32 length, bytes", () => {
  const data = encodeMergeData(Buffer.from([7, 8, 9]));
  assert.deepEqual(data.subarray(0, 8), DISCRIMINATORS.mergeAndDeposit);
  assert.equal(data.readUInt32LE(8), 3);
  assert.deepEqual([...data.subarray(12)], [7, 8, 9]);
});

function poolAccount(fields: PublicKey[], status: number): Buffer {
  return Buffer.concat([DISCRIMINATORS.pool, ...fields.map((f) => f.toBuffer()), Buffer.from([status, 0, 255])]);
}

test("pool and config layouts", () => {
  const fields = Array.from({ length: 17 }, key);
  const address = key();
  const pool = decodePool(address, poolAccount(fields, 0));
  assert.ok(pool.open);
  assert.ok(pool.market.equals(fields[0]));
  assert.ok(pool.poolVannaShares.equals(fields[16]));
  assert.equal(decodePool(address, poolAccount(fields, 1)).open, false);
  assert.throws(() => decodePool(address, Buffer.alloc(600)));

  const [admin, resolver, keeper] = [key(), key(), key()];
  const loss = Buffer.alloc(3);
  loss.writeUInt16LE(10);
  const config = decodeConfig(
    Buffer.concat([DISCRIMINATORS.config, admin.toBuffer(), resolver.toBuffer(), keeper.toBuffer(), loss]),
  );
  assert.ok(config.keeper.equals(keeper));
  assert.equal(config.maxSwapLossBps, 10);
});

/** A `route` with one AMM hop, shaped like Jupiter's API output for the pool PDA. */
function route(pool: PoolState, amount: bigint) {
  const keys = [
    key(), // token program
    pool.address,
    pool.poolCash,
    pool.poolUsdc,
    JUPITER_PROGRAM_ID, // destination_token_account: unset
    USDC_MINT,
    JUPITER_PROGRAM_ID, // platform_fee_account: unset
    key(), // event authority
    JUPITER_PROGRAM_ID,
    key(), // AMM
    pool.address,
  ];
  const tail = Buffer.alloc(19);
  tail.writeBigUInt64LE(amount, 0);
  tail.writeBigUInt64LE(amount, 8);
  const data = Buffer.concat([Buffer.from([229, 23, 203, 151, 122, 227, 173, 42]), Buffer.alloc(6), tail]);
  return { data, keys };
}

test("routes are checked the way the program checks them", () => {
  const pool = decodePool(key(), poolAccount(Array.from({ length: 17 }, key), 0));
  const jupiter = JUPITER_PROGRAM_ID.toBase58();
  const ok = route(pool, 50n);
  assert.doesNotThrow(() => checkRoute(jupiter, ok.data, ok.keys, PROGRAM, pool, 50n));

  const rejects = (mutate: (r: { data: Buffer; keys: PublicKey[] }) => void, amount = 50n) => {
    const r = route(pool, 50n);
    mutate(r);
    assert.throws(() => checkRoute(jupiter, r.data, r.keys, PROGRAM, pool, amount), RouteRejected);
  };
  rejects(() => {}, 49n); // spends a different amount than the pool's sets
  rejects((r) => (r.keys[1] = key())); // someone else signs
  rejects((r) => (r.keys[3] = key())); // output leaves the pool
  rejects((r) => (r.keys[6] = key())); // fee account
  rejects((r) => (r.data[r.data.length - 1] = 5)); // platform fee bps
  rejects((r) => r.keys.push(pool.poolYes)); // another pool account in reach of its signature
  rejects((r) => r.keys.push(VANNA_PROGRAM_ID));
  rejects((r) => r.keys.push(PROGRAM));
  rejects((r) => r.data.fill(0, 0, 8)); // not route / shared_accounts_route
});
