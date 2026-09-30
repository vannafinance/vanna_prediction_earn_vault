# Keeper

A small Node service that keeps every pool's idle outcome tokens earning. It watches the pool program's `Deposit` and `Redeem` events, and whenever a pool holds complete YES + NO sets it sends `merge_and_deposit`. That one instruction merges the sets into CASH through World, swaps the CASH into USDC through Jupiter and supplies the USDC to Vanna's reserve.

Deposits never wait on the keeper. A user gets shares the moment `deposit` lands; the keeper only decides when the pool's idle tokens start earning.

## Why a keeper

- **Solana can't catch a failed CPI.** If `deposit` merged inline, a refusing swap or a paused Vanna would block deposits. So the merge is its own instruction, and something has to send it.
- **The route is picked off-chain.** The swap needs a Jupiter route for the exact amount at that moment, which only Jupiter's API can produce.
- **CASH sells a few bp under par.** The program lets only the config's `keeper` accept up to `max_swap_loss_bps` below par; anyone else must get par. Run this service with that key.

## Run it

```sh
cd keeper
npm install
KEEPER_KEYPAIR=~/keys/keeper.json RPC_URL=https://your-rpc npm start
```

The wallet must be the config's `keeper` (set with `set_swap_policy`) and hold SOL: about 0.0069 SOL of rent per pool for its lookup table (one-time, recoverable), plus fees of about 0.0000065 SOL per merge at the default priority fee.

| Command | What it does |
|---|---|
| `npm start` | Watch events, sweep all pools every `SWEEP_MS`, merge as pools fill. Stops cleanly on SIGINT/SIGTERM. |
| `npm run once` | Merge every pool that needs it, then exit. Good for cron. |
| `npm run pool -- <pool>` | Look at one pool, merge it if needed, then exit. |

`DRY_RUN=true` builds and simulates each merge without sending it.

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `RPC_URL` | `http://127.0.0.1:8899` | Solana RPC. Use one with WebSocket support. |
| `WS_URL` | derived from `RPC_URL` | WebSocket endpoint, if your provider uses a separate one. |
| `KEEPER_KEYPAIR` | `~/.config/solana/id.json` | The config's keeper wallet. |
| `POOL_PROGRAM_ID` | `2h3hJSZV…mskZ` | The pool program. |
| `JUPITER_API` | `https://lite-api.jup.ag/swap/v1` | Use `https://api.jup.ag/swap/v1` with `JUPITER_API_KEY` in production. |
| `JUPITER_API_KEY` | none | Sent as `x-api-key`. |
| `MIN_MERGE` | `1000000` (1 CASH) | Smallest number of complete sets worth a merge, in raw units. |
| `HEADROOM_BPS` | `1` | How far the quote must clear the program's floor before the keeper sends. |
| `SLIPPAGE_BPS` | `20` | Jupiter's own slippage check. The program's par floor applies regardless. |
| `MAX_ACCOUNTS` | `32` | Caps route size so the transaction fits in 1232 bytes. |
| `ONLY_DIRECT_ROUTES` | `true` | Single-hop CASH -> USDC. Multi-hop routes need intermediate accounts the pool can't create. |
| `DEBOUNCE_MS` | `3000` | Wait after an event, so a burst of deposits merges once. |
| `SWEEP_MS` | `300000` | Full scan of all pools, which catches anything the event stream missed. |
| `PRICE_RETRY_MS` | `60000` | Wait before re-quoting a pool whose price was under the floor. |
| `PRIORITY_MICROLAMPORTS` | `10000` | Compute-unit price. |
| `STATE_FILE` | `keeper-state.json` | Records each pool's lookup table. If lost, the keeper creates new tables. |

## What one merge looks like

1. Read the pool. Skip it if it is settled or World has resolved its market, since World refuses merges after that.
2. `sets = min(pool YES, pool NO)`. Stop if it is under `MIN_MERGE`.
3. Compute the program's floor: `sets × (1 − max_swap_loss_bps) − 5`, where the loss allowance is zero for anyone who isn't the keeper.
4. Quote `sets` CASH -> USDC on Jupiter. Wait `PRICE_RETRY_MS` if the quote is under floor + headroom.
5. Fetch the swap with the pool PDA as the user, then run every check the program makes: `route` or `shared_accounts_route` only, the exact amount, the pool as authority, its own CASH and USDC accounts, no fee account, and no other pool account, Vanna or prediCt in reach. Each account keeps the writability Jupiter gives it; some venues, such as Manifest, need the user writable.
6. Build a v0 transaction with the pool's lookup table (created on first use) and Jupiter's tables.
7. Simulate, size the compute budget from the simulation, send, confirm.
8. Look at the pool again a few seconds later for deposits that landed meanwhile.

## When a merge doesn't go through

| What happened | What the keeper does |
|---|---|
| Quote under the floor, or `SwapBelowMinimum` | Re-quotes after `PRICE_RETRY_MS`. Deposits keep working; the tokens just earn later. |
| A venue refuses the pool's swap (some venues check who calls them) | Leaves that venue out of quotes for 15 minutes and re-quotes at once. |
| Jupiter's slippage check | Re-quotes after `PRICE_RETRY_MS`. |
| `SwapAmountMismatch` (someone else merged or deposited in between) | Re-reads the pool after 1 s. |
| Pool settled, or the market resolved | Stops watching the pool. |
| Anything else | Logs the last program lines and retries with backoff, from 30 s up to 15 min. |

## Security

- The keeper key can't move pool funds. Every route passes the program's checks, which are the same ones the keeper runs before sending.
- The most a keeper can cost the pool is `max_swap_loss_bps` of each merge. Keep that allowance tight (the program caps it at 100 bp).
- Only give the key the SOL it needs.

## Tests

```sh
npm test            # discriminators vs the IDL, event parsing, error decoding, route checks
npm run typecheck
```

### End to end on a mainnet fork

[`e2e/fork.ts`](e2e/fork.ts) sets everything up on a [Surfpool](https://surfpool.run) fork. It uses Surfpool cheatcodes and refuses any non-local RPC.

```sh
surfpool start --network mainnet --no-tui --no-deploy -y
solana program deploy --use-rpc -u localhost --program-id ../target/deploy/outcome_yield_pool-keypair.json ../target/deploy/outcome_yield_pool.so

npm run fork -- vanna               # Vanna's program at its real id, from ../fixtures/vanna
npm run fork -- setup               # Vanna USDC reserve + seed lender, pool config, allowlist, pool on a live World market
KEEPER_KEYPAIR=e2e/fork-keeper.json npm start    # in another terminal

npm run fork -- deposit yes 50
npm run fork -- deposit no 50       # the keeper merges within seconds
npm run fork -- status
```

A run on 30 Sep 2026 against World's "Bitcoin 2026 Year-High" market merged 50, 20 and 10 CASH through Manifest, left nothing dangling but an unmatched 10 NO, and supplied the USDC to Vanna. On the fork, AlphaQ and Whirlpool refuse swaps because of fork state (stale clock, uncloned venue state); the keeper skipped them and routed around them.
