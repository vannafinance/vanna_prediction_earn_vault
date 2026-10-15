// One-off admin CLI for outcome_yield_pool: initialize_config / allow_market / create_pool.
// Usage: node scripts/admin.mjs <command> [...args]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddressSync, TOKEN_PROGRAM_ID } from "@solana/spl-token";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, "..");

const RPC_URL = process.env.RPC_URL ?? "https://rpc-devnet.solana.vanna.finance";
const walletPath = process.env.WALLET_PATH ?? path.join(process.env.HOME, ".config/solana/id.json");
const secret = JSON.parse(fs.readFileSync(walletPath, "utf8"));
const walletKeypair = Keypair.fromSecretKey(Uint8Array.from(secret));
const wallet = new anchor.Wallet(walletKeypair);

const connection = new Connection(RPC_URL, "confirmed");
const provider = new anchor.AnchorProvider(connection, wallet, { commitment: "confirmed" });
anchor.setProvider(provider);

const idl = JSON.parse(fs.readFileSync(path.join(root, "target/idl/outcome_yield_pool.json"), "utf8"));
const program = new anchor.Program(idl, provider);

const [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], program.programId);

const cmd = process.argv[2];

async function initializeConfig() {
  const resolver = process.argv[3] ? new PublicKey(process.argv[3]) : wallet.publicKey;
  const keeper = process.argv[4] ? new PublicKey(process.argv[4]) : wallet.publicKey;
  const maxSwapLossBps = Number(process.argv[5] ?? 10);

  const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
  const [programData] = PublicKey.findProgramAddressSync(
    [program.programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE,
  );

  const sig = await program.methods
    .initializeConfig(resolver, keeper, maxSwapLossBps)
    .accounts({
      admin: wallet.publicKey,
      programData,
    })
    .rpc();

  console.log("initialize_config tx:", sig);
  console.log("config PDA:", configPda.toBase58());
}

async function allowMarket() {
  const market = new PublicKey(process.argv[3]);
  const [allowedMarketPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("allowed_market"), market.toBuffer()],
    program.programId,
  );
  const sig = await program.methods
    .allowMarket()
    .accounts({
      admin: wallet.publicKey,
      market,
    })
    .rpc();
  console.log("allow_market tx:", sig);
  console.log("allowed_market PDA:", allowedMarketPda.toBase58());
}

const VANNA_PROGRAM_ID = new PublicKey("H37YVBG3eGtz89ofd7iDLc2PJgn12778392XeU2QJcGC");
const USDC_MINT = new PublicKey("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");

const reservePda = (mint) =>
  PublicKey.findProgramAddressSync([Buffer.from("reserve"), mint.toBuffer()], VANNA_PROGRAM_ID)[0];
const shareMintPda = (mint) =>
  PublicKey.findProgramAddressSync([Buffer.from("share_mint"), mint.toBuffer()], VANNA_PROGRAM_ID)[0];

async function createPool() {
  const market = new PublicKey(process.argv[3]);
  const yesMint = new PublicKey(process.argv[4]);
  const noMint = new PublicKey(process.argv[5]);
  const collateralMint = new PublicKey(process.argv[6]);

  const vannaReserve = reservePda(USDC_MINT);
  const vannaShareMint = shareMintPda(USDC_MINT);

  const [poolPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("pool"), market.toBuffer(), vannaReserve.toBuffer()],
    program.programId,
  );

  const sig = await program.methods
    .createPool()
    .accounts({
      payer: wallet.publicKey,
      market,
      vannaReserve,
      usdcMint: USDC_MINT,
      vannaShareMint,
      collateralMint,
      yesMint,
      noMint,
    })
    .rpc();

  console.log("create_pool tx:", sig);
  console.log("pool PDA:", poolPda.toBase58());
}

const JUPITER_PROGRAM_ID = new PublicKey("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
const TOKEN_2022_PROGRAM_ID = new PublicKey("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

async function tokenAmount(pubkey) {
  const info = await connection.getAccountInfo(pubkey);
  if (!info) return 0n;
  return info.data.readBigUInt64LE(64);
}

/** Merges the pool's complete YES/NO sets into CASH, swaps to USDC via a real Jupiter route, and
 * supplies the USDC to Vanna. Only the config's keeper may accept a swap slightly below par. */
async function mergeAndDeposit() {
  const market = new PublicKey(process.argv[3]);
  const vannaReserve = reservePda(USDC_MINT);
  const [poolPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("pool"), market.toBuffer(), vannaReserve.toBuffer()],
    program.programId,
  );
  const pool = await program.account.pool.fetch(poolPda);

  const yesBal = await tokenAmount(pool.poolYes);
  const noBal = await tokenAmount(pool.poolNo);
  const completeSets = yesBal < noBal ? yesBal : noBal;
  console.log("pool_yes", yesBal.toString(), "pool_no", noBal.toString(), "complete_sets", completeSets.toString());
  if (completeSets === 0n) {
    console.log("Nothing to merge (no complete YES+NO set in the pool yet).");
    return;
  }

  const quoteRes = await fetch(
    `https://lite-api.jup.ag/swap/v1/quote?inputMint=${pool.collateralMint.toBase58()}&outputMint=${USDC_MINT.toBase58()}&amount=${completeSets}&slippageBps=50&dexes=Manifest`,
  );
  const quote = await quoteRes.json();
  if (quote.error) throw new Error(`Jupiter quote failed: ${JSON.stringify(quote)}`);
  console.log("Jupiter quote:", quote.inAmount, "CASH ->", quote.outAmount, "USDC");

  const swapIxRes = await fetch("https://lite-api.jup.ag/swap/v1/swap-instructions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      quoteResponse: quote,
      userPublicKey: poolPda.toBase58(),
      destinationTokenAccount: pool.poolUsdc.toBase58(),
      wrapAndUnwrapSol: false,
    }),
  });
  const swapIxData = await swapIxRes.json();
  if (swapIxData.error) throw new Error(`Jupiter swap-instructions failed: ${JSON.stringify(swapIxData)}`);
  const swapIx = swapIxData.swapInstruction;

  const remainingAccounts = swapIx.accounts.map((a) => ({
    pubkey: new PublicKey(a.pubkey),
    isSigner: false,
    // The pool PDA signs this CPI via seeds inside the program, which itself always zeroes its
    // own writable flag for the Jupiter call (see jupiter.rs `!is_pool`); declaring it writable
    // here too just risks a duplicate-account privilege-escalation check, so keep it false.
    isWritable: a.pubkey === poolPda.toBase58() ? false : a.isWritable,
  }));
  const swapData = Buffer.from(swapIx.data, "base64");

  const ix = await program.methods
    .mergeAndDeposit(swapData)
    .accounts({
      caller: wallet.publicKey,
      config: configPda,
      pool: poolPda,
      predict: {
        market,
        collateralMint: pool.collateralMint,
        yesMint: pool.yesMint,
        noMint: pool.noMint,
        marketCashVault: pool.marketCashVault,
        poolCash: pool.poolCash,
        poolYes: pool.poolYes,
        poolNo: pool.poolNo,
        predictProgram: new PublicKey("prediCtPZCttYMvm2W3PtxmMxLmT1dtN7riU6Cxh6tM"),
      },
      vanna: {
        vannaProgram: VANNA_PROGRAM_ID,
        vannaProtocolConfig: pool.vannaProtocolConfig,
        vannaAssetConfig: pool.vannaAssetConfig,
        vannaReserve: pool.vannaReserve,
        usdcMint: USDC_MINT,
        vannaLiquidityVault: pool.vannaLiquidityVault,
        vannaShareMint: pool.vannaShareMint,
        poolUsdc: pool.poolUsdc,
        poolVannaShares: pool.poolVannaShares,
        splTokenProgram: new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
        associatedTokenProgram: new PublicKey("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
        systemProgram: new PublicKey("11111111111111111111111111111111"),
      },
      jupiterProgram: JUPITER_PROGRAM_ID,
      tokenProgram: TOKEN_2022_PROGRAM_ID,
    })
    .remainingAccounts(remainingAccounts)
    .instruction();

  const lutAddresses = swapIxData.addressLookupTableAddresses ?? [];
  const lutAccounts = [];
  for (const addr of lutAddresses) {
    const res = await connection.getAddressLookupTable(new PublicKey(addr));
    if (res.value) lutAccounts.push(res.value);
  }

  const latest = await connection.getLatestBlockhash("confirmed");
  const message = new anchor.web3.TransactionMessage({
    payerKey: wallet.publicKey,
    recentBlockhash: latest.blockhash,
    instructions: [anchor.web3.ComputeBudgetProgram.setComputeUnitLimit({ units: 700_000 }), ix],
  }).compileToV0Message(lutAccounts);
  const tx = new anchor.web3.VersionedTransaction(message);
  tx.sign([walletKeypair]);
  const sig = await connection.sendTransaction(tx, { skipPreflight: false, maxRetries: 3 });
  await connection.confirmTransaction({ signature: sig, ...latest }, "confirmed");

  console.log("merge_and_deposit tx:", sig);
}

/** World market data comes from the app's own API (reads real mainnet, already proven to work
 *  for every market on the browse page) rather than a raw fork read: a market account that no
 *  transaction has touched on the Surfpool fork yet isn't cloned there, so a bare
 *  `getAccountInfo` on the fork for it fails with "Internal error" even though the market is
 *  perfectly real — only a transaction's own account resolution triggers the fork's lazy clone. */
const rawAppUrl = process.env.APP_URL ?? "http://localhost:3000";
const APP_URL = /^https?:\/\//.test(rawAppUrl) ? rawAppUrl : `https://${rawAppUrl}`;
const CASH_MINT = new PublicKey("CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH");

/**
 * Onboards one or more World markets for the yield pool in a single run: calls `allow_market`
 * (admin-only) then `create_pool` for each market pubkey given, looking up yesMint/noMint via the
 * app's `/api/world/markets/:id` route so you only need to pass the market pubkey, nothing else.
 * Safe to re-run: skips either step if it's already done for that market.
 * Usage: node scripts/admin.mjs onboardMarket <MARKET_PUBKEY> [MARKET_PUBKEY_2] ...
 * (set APP_URL if the app isn't on http://localhost:3000)
 */
async function onboardMarket() {
  const marketArgs = process.argv.slice(3);
  if (marketArgs.length === 0) {
    console.error("Usage: node scripts/admin.mjs onboardMarket <MARKET_PUBKEY> [MARKET_PUBKEY_2] ...");
    process.exit(1);
  }

  const vannaReserve = reservePda(USDC_MINT);
  const vannaShareMint = shareMintPda(USDC_MINT);

  const failed = [];
  for (const marketStr of marketArgs) {
    const market = new PublicKey(marketStr);
    console.log(`\n=== ${market.toBase58()} ===`);
    try {
      await onboardOne(market);
    } catch (error) {
      console.error("  FAILED:", error instanceof Error ? error.message : error);
      failed.push(market.toBase58());
    }
  }
  if (failed.length) {
    console.log(`\n${failed.length} market(s) failed, re-run just these:\n${failed.join(" ")}`);
  }

  async function onboardOne(market) {
    const res = await fetch(`${APP_URL}/api/world/markets/${market.toBase58()}`);
    const data = await res.json();
    if (!data.market) {
      console.error("  market lookup failed via app API, skipping:", data.error ?? `HTTP ${res.status}`);
      return;
    }
    const yesMint = new PublicKey(data.market.yesMint);
    const noMint = new PublicKey(data.market.noMint);
    const cashMint = CASH_MINT;
    console.log("  question:", data.market.question);
    console.log("  yesMint: ", yesMint.toBase58());
    console.log("  noMint:  ", noMint.toBase58());

    // The fork's getAccountInfo intermittently throws "Internal error" for an account it has
    // never been asked about before (same quirk documented in healRouteAccounts/pool-keeper.
    // server.ts) rather than cleanly returning null — a single throw is not trustworthy evidence
    // of absence. Confirmed live: "pool already exists" checks for KNOWN-good pools (e.g. Seattle,
    // Athletic Bilbao — tested extensively earlier this session) returned a false negative once,
    // which then sent a real create_pool at an already-initialised pool and failed. Retry a few
    // times before concluding "not found"; only a true account read (null value, no throw) or
    // every attempt failing counts as "doesn't exist".
    const existsOnFork = async (pubkey) => {
      for (let attempt = 0; attempt < 4; attempt++) {
        try {
          const info = await connection.getAccountInfo(pubkey);
          if (info) return true;
          return false; // clean null — genuinely doesn't exist, no need to retry
        } catch {
          if (attempt < 3) await new Promise((r) => setTimeout(r, 400));
        }
      }
      return false;
    };

    const [allowedMarketPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("allowed_market"), market.toBuffer()],
      program.programId,
    );
    if (await existsOnFork(allowedMarketPda)) {
      console.log("  allow_market: already allowed, skipping");
    } else {
      const sig = await program.methods.allowMarket().accounts({ admin: wallet.publicKey, market }).rpc();
      console.log("  allow_market tx:", sig);
      // The fork can confirm this tx yet still simulate create_pool against slightly-stale state
      // right after — the next instruction's simulation doesn't always see the write that just
      // landed (same eventual-consistency quirk as the share-mint orphan case above). Poll for the
      // account to actually be readable before moving on, instead of racing straight into create_pool.
      for (let attempt = 0; attempt < 10 && !(await existsOnFork(allowedMarketPda)); attempt++) {
        await new Promise((r) => setTimeout(r, 500));
      }
    }

    const [poolPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("pool"), market.toBuffer(), vannaReserve.toBuffer()],
      program.programId,
    );
    if (await existsOnFork(poolPda)) {
      console.log("  create_pool: pool already exists, skipping. Pool PDA:", poolPda.toBase58());
      return;
    }

    // create_pool inits 7 accounts seeded/derived off the pool PDA — the share mints plus 5 token
    // accounts owned by the pool (pool_cash, pool_yes, pool_no, pool_usdc, pool_vanna_shares). Any
    // of them can only ever be legitimate alongside the pool itself (all 7 are created together in
    // this one transaction) — if the pool doesn't exist (just confirmed above) but one of these
    // does, it's orphaned garbage from an earlier attempt that existed before a fork reset wiped
    // just the pool account. Observed live for "Will Seattle win?": pool missing, but its pool_yes
    // ATA still held real Token-2022 data, which made create_pool fail with the confusing "Provided
    // owner is not allowed" (not a clean "already in use", because `associated_token::init` behaves
    // differently than a plain PDA `init` when the target address already has token-account data).
    // Clear all 7 via the reset cheatcode before retrying. Resetting a genuinely-nonexistent PDA is
    // a harmless no-op (nothing on mainnet to reclone, so it just stays cleared).
    const [yesSharesPda] = PublicKey.findProgramAddressSync([Buffer.from("yes_shares"), poolPda.toBuffer()], program.programId);
    const [noSharesPda] = PublicKey.findProgramAddressSync([Buffer.from("no_shares"), poolPda.toBuffer()], program.programId);
    const poolCashAta = getAssociatedTokenAddressSync(cashMint, poolPda, true, TOKEN_2022_PROGRAM_ID);
    const poolYesAta = getAssociatedTokenAddressSync(yesMint, poolPda, true, TOKEN_2022_PROGRAM_ID);
    const poolNoAta = getAssociatedTokenAddressSync(noMint, poolPda, true, TOKEN_2022_PROGRAM_ID);
    const poolUsdcAta = getAssociatedTokenAddressSync(USDC_MINT, poolPda, true, TOKEN_PROGRAM_ID);
    const poolVannaSharesAta = getAssociatedTokenAddressSync(vannaShareMint, poolPda, true, TOKEN_PROGRAM_ID);
    await Promise.all(
      [yesSharesPda, noSharesPda, poolCashAta, poolYesAta, poolNoAta, poolUsdcAta, poolVannaSharesAta].map((pk) =>
        fetch(connection.rpcEndpoint, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "surfnet_resetAccount", params: [pk.toBase58()] }),
        }).catch(() => null),
      ),
    );

    const sig2 = await program.methods
      .createPool()
      .accounts({
        payer: wallet.publicKey,
        market,
        vannaReserve,
        usdcMint: USDC_MINT,
        vannaShareMint,
        collateralMint: cashMint,
        yesMint,
        noMint,
      })
      .rpc();
    console.log("  create_pool tx:", sig2);
    console.log("  pool PDA:", poolPda.toBase58());
  }
}

async function setSwapPolicy() {
  const keeper = new PublicKey(process.argv[3]);
  const maxSwapLossBps = Number(process.argv[4] ?? 10);
  const sig = await program.methods
    .setSwapPolicy(keeper, maxSwapLossBps)
    .accounts({ admin: wallet.publicKey })
    .rpc();
  console.log("set_swap_policy tx:", sig);
  console.log("keeper:", keeper.toBase58(), "maxSwapLossBps:", maxSwapLossBps);
}

async function showConfig() {
  const cfg = await program.account.config.fetch(configPda);
  console.log("config PDA:", configPda.toBase58());
  console.log(cfg);
}

const commands = { initializeConfig, allowMarket, createPool, onboardMarket, mergeAndDeposit, setSwapPolicy, showConfig };

if (!commands[cmd]) {
  console.error(`Unknown command "${cmd}". Available: ${Object.keys(commands).join(", ")}`);
  process.exit(1);
}

await commands[cmd]();
