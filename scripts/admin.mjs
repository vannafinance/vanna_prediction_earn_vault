// One-off admin CLI for outcome_yield_pool: initialize_config / allow_market / create_pool.
// Usage: node scripts/admin.mjs <command> [...args]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";

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

const commands = { initializeConfig, allowMarket, createPool, mergeAndDeposit, setSwapPolicy, showConfig };

if (!commands[cmd]) {
  console.error(`Unknown command "${cmd}". Available: ${Object.keys(commands).join(", ")}`);
  process.exit(1);
}

await commands[cmd]();
