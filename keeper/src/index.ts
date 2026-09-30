import { Connection, PublicKey } from "@solana/web3.js";
import { loadConfig, loadKeypair } from "./config.ts";
import { Keeper } from "./keeper.ts";
import { log } from "./log.ts";

const USAGE = `usage: keeper <command>
  run            watch events, sweep every SWEEP_MS, merge as pools fill (default)
  once           merge every pool that needs it, then exit
  pool <address> look at one pool, merge it if it needs it, then exit`;

async function main(): Promise<void> {
  const [command = "run", arg] = process.argv.slice(2);
  const cfg = loadConfig();
  const connection = new Connection(cfg.rpcUrl, { commitment: "confirmed", wsEndpoint: cfg.wsUrl });
  const keeper = new Keeper(cfg, connection, loadKeypair(cfg.keypairPath));

  switch (command) {
    case "run": {
      await keeper.start();
      const shutdown = async (signal: string) => {
        log.info(`${signal}: finishing the merge in flight, then exiting`);
        await keeper.stop();
        process.exit(0);
      };
      process.on("SIGINT", () => void shutdown("SIGINT"));
      process.on("SIGTERM", () => void shutdown("SIGTERM"));
      return;
    }
    case "once":
      await keeper.runOnce();
      break;
    case "pool":
      if (!arg) throw new Error(USAGE);
      await keeper.runPoolNow(new PublicKey(arg));
      break;
    default:
      throw new Error(USAGE);
  }
  // web3.js keeps its WebSocket open; one-shot commands end here.
  process.exit(0);
}

main().catch((e) => {
  log.error(e instanceof Error ? e.message : String(e));
  process.exit(1);
});
