# Vanna Prediction Earn Vault

Yield-bearing outcome tokens on Solana: holders of [World](https://world.xyz/) prediction-market positions earn lending yield on them through [Vanna](../Protocol_V1_Solana)'s USDC reserve, without giving up their bet.

Users deposit the YES and NO outcome tokens of a World market. The pool matches them into complete sets and merges each set into 1 CASH through World's `prediCt` program. It swaps the CASH into USDC through Jupiter and lends the USDC in Vanna's USDC reserve. Yield flows back to each side by redeeming USDC, swapping it back into CASH and splitting it into fresh YES/NO pairs. The scarce (fully matched) side earns the full rate and the surplus side is diluted by its utilization.

The design rests on one fact, which prediCt enforces exactly and fee-free:

```
split(N):  N CASH       -> N YES + N NO
merge(N):  N YES + N NO -> N CASH
```

No outcome-token price is ever used. The pool never trades outcome tokens through JanusFI, BisonFI or DFlow; those are only where users buy the tokens they deposit. The one price the pool does touch is CASH/USDC, because World settles in CASH and Vanna lends USDC. See [Swaps and the par rule](#swaps-and-the-par-rule).

## Integrations

| Program | Address | Used for |
|---|---|---|
| World prediCt | `prediCtPZCttYMvm2W3PtxmMxLmT1dtN7riU6Cxh6tM` | `split` / `merge` CPIs; Market account read at pool creation, deposit and settlement |
| CASH (Token-2022) | `CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH` | World's only collateral |
| World operator | `DDucv2DeUsTsg1rfAcWAnUSUVpqfdHEzxX66ARB2JYVg` | Resolves markets; the pool never needs its signature |
| Vanna lending | `H37YVBG3eGtz89ofd7iDLc2PJgn12778392XeU2QJcGC` | `lender_supply` / `lender_redeem` / `public_refresh_reserve` on its USDC reserve |
| USDC (SPL) | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` | The asset of Vanna's USDC reserve |
| Jupiter v6 | `JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4` | CASH <-> USDC swaps (`route`, `shared_accounts_route`) |
| JanusFI / BisonFI / DFlow | `JanusX…`, `2DNbz…`, `DF1ow…` | Not called. Users trade outcome tokens through them before depositing |

World publishes no IDL for prediCt. The integration started from the decoded [technical reference](https://github.com/chainstacklabs/world-xyz-research/blob/main/docs/reference.md), keeps every assumption in [`predict.rs`](programs/outcome_yield_pool/src/predict.rs), and is **tested against World's deployed program**: the suite runs the real prediCt bytecode over a mainnet snapshot of the three live markets (see [Tests run on real code](#tests-run-on-real-code)). That turned up two things the reference got wrong or left open:

- **Where the outcome lives.** `determine_outcome` writes the resolution slot as a u64 at offset 176 (zero while open) and the outcome as a u16 at offset 184. The pool originally guessed 272/273, which would never have settled. Confirmed against the live program and against all 179 resolved-but-not-yet-closed markets on mainnet at snapshot time.
- **prediCt's `split` / `merge` signer must be writable.** Passing the pool PDA read-only failed every merge and every redemption that needs a split.

Also confirmed live: the 11-account `split` / `merge` roster (from real mainnet transactions), fee-free par conversion, the Market offsets for collateral (40), YES mint (72), NO mint (104) and CASH vault (136), and that `split` and `merge` are closed once a market is determined (`MarketAlreadyDetermined`).

**Vanna is not deployed at `H37YVB…` yet.** At the time of writing that address has no account and no transaction history on mainnet, devnet or testnet, and the Vanna source declares `BZ812nUv…`, which is live on devnet. The pool calls `VANNA_PROGRAM_ID` in [`constants.rs`](programs/outcome_yield_pool/src/constants.rs); change it there if the deployment lands elsewhere, and rebuild the test fixture.

## Lifecycle

```
            deposit ─┐        ┌─ redeem
                     ▼        │
   Open ──► merge_and_deposit ┴──► (World: determine_outcome) ──► settle ──► Settled ──► redeem_settled
                                                                    │
                                     (World: close_market) ──► settle_by_resolver
```

**Open.**

- `deposit(is_yes, amount)` moves outcome tokens in and mints that side's shares, priced against the side's assets before the tokens arrived. It's refused once World has determined the outcome, even before anyone calls `settle`: World may already have paid the pool CASH for its winning tokens, which open-market pricing doesn't count, so a late deposit would buy into it at a discount.
- `merge_and_deposit(swap_data)` merges `min(YES, NO)` complete sets into CASH, swaps exactly that CASH into USDC with the caller's Jupiter route, and supplies all the pool's USDC to Vanna.
- `redeem(is_yes, shares, swap_data)` pays dangling tokens first. On a shortfall it redeems that much USDC from Vanna, swaps it into CASH with the caller's Jupiter route and splits it; the other half of each new pair stays in the pool for the opposite side. `swap_data` is ignored when dangling tokens cover the redemption.

**Settlement.** World writes the outcome with `determine_outcome`, pays winners with `redeem_outcome_for_user`, burns losers with `burn_worthless_outcome`, and then deletes both mints, the CASH vault and the Market account with `close_market`. After that the pool can no longer turn CASH back into tokens, so it has an explicit settled phase:

- `settle()` is permissionless. It reads the winner from the Market account (10000 = YES, 0 = NO) and rejects any other value.
- `settle_by_resolver(yes_won)` is only for a market that World closed before anyone called `settle`. It's refused while the Market account still exists.
- `redeem_settled(is_yes, shares)` pays out once settled, with no swap:
  - **Winning side:** its remaining tokens, plus any CASH World already paid the pool for them, plus the whole invested USDC. Paid as tokens first, then CASH, then USDC redeemed from Vanna.
  - **Losing side:** its own tokens back, which World pays nothing for.

## Yield: Vanna's USDC reserve

Every pool lends into the same reserve, Vanna's reserve for USDC (`["reserve", USDC]` under Vanna's program). `create_pool` checks the reserve is exactly that account and records the Vanna accounts the pool will pass. The pool is a PDA seeded by `(market, reserve)`, and each pool holds its own Vanna share account, so a pool's stake is its own share balance and nothing else.

- **Invested balance** = idle USDC in the pool + `floor(shares * lender_total_assets / share_supply)`, where `lender_total_assets = liquidity + borrows - protocol fees`. This is exactly what Vanna's `lender_redeem` would pay (`vanna_reserve_layout_matches_what_the_pool_reads`). The pool calls `public_refresh_reserve` first, so interest is accrued to the second.
- **An empty position never calls Vanna**, so a pool holding only dangling tokens can always be priced and exited, whatever state Vanna is in.
- **Exits redeem the fewest shares that cover the USDC needed**, `ceil(usdc * supply / total)`. That never exceeds the pool's shares while the amount is within its position.
- **Interest is shared pro rata by reserve shares** across pools and every other Vanna lender (`interest_is_shared_pro_rata_by_reserve_shares`).

## Swaps and the par rule

The pool counts USDC at par with CASH. Every swap is checked against that par, so no swap can make the invested balance overstate what the pool can pay.

- **The caller picks the Jupiter route; the pool checks the result.** Only exact-input `route` and `shared_accounts_route` are accepted, with no platform fee, no fee account and no output redirect. The pool PDA must be the authority, the source and destination must be the pool's CASH and USDC accounts, and the mints must match. Only one pool token account may be the source and one the destination, so the YES, NO and Vanna-share accounts never reach the swap. Neither may this program, Vanna, prediCt, or any other account this program owns. Afterwards the pool checks that exactly the declared input left, measures what arrived, and checks that both token accounts are still plain pool accounts. This mirrors Vanna's own Jupiter adapter.
- **Merges: 1 USDC per CASH, unless the keeper merges.** Selling CASH pays the AMM fee, about 2.3 bp on the live whirlpool, so a par-only merge doesn't go through today. The config's `keeper` may accept up to `max_swap_loss_bps` below par (capped at 100 bp by `MAX_SWAP_LOSS_BPS_CAP`). Everyone else must get par, because a caller who could accept less could route through a venue they price and keep the difference.
- **Redemptions: 1 CASH per USDC, from everyone.** Buying CASH with USDC lands at or above par on the live whirlpool. A worse price would take the difference from both sides' holders, so it's refused (`a_sandwiched_redemption_fails_instead_of_paying_below_par`).
- **Size limits on a redemption's route.** It must swap at least the shortfall and at most 0.1% + 0.001 USDC more. That's enough headroom for a client whose quote is a few seconds old, but not enough to pull the pool's funds out of Vanna for nothing. Any surplus CASH becomes pairs for both sides.
- **Fee rounding.** Any swap may fall `SWAP_DUST_UNITS` (5 raw units, 0.000005) short of its floor. AMMs round a unit or two against the trader however small the swap, so without this a small redemption could never exit. If that rounding leaves a redemption's split short, the redeemer is paid what the split produced, at most 5 units under their claim.
- **Merge exactly the merged CASH.** A merge swaps exactly `min(YES, NO)`, never CASH that was already idle.

A merge or redemption with its route names more accounts than a legacy transaction holds, so clients send v0 transactions with an address lookup table, as they would for any Jupiter swap.

## Design choices

| Choice | Why |
|---|---|
| One program; each pool is a PDA seeded by `(market, reserve)` | Every market's funds sit in token accounts only its own pool can sign for, so markets are isolated physically |
| One Token-2022 share mint per side per pool (yYES, yNO), with the pool as mint authority | Wallet, transfer and `approve` support for free; an approved delegate can redeem on the owner's behalf |
| Deposits go through the `deposit` instruction; a plain transfer into the pool is a donation to that side | Token-2022 has no receive hook, so a transfer cannot mint shares |
| `merge_and_deposit` is its own instruction, sent by the keeper or appended to a deposit | A failed CPI cannot be caught on Solana, so a merge that Vanna or the swap refuses must not block deposits |
| Vanna's USDC reserve as the yield vault, reached by a CASH <-> USDC swap checked against par | World settles in CASH; Vanna lends USDC |
| A settled phase: `settle` / `redeem_settled` | World closes split/merge and deletes the market after resolving it |
| The admin allowlists markets and sets the swap keeper; pool creation for an allowed market is permissionless | A curated market set; the admin controls no pool funds |
| `VIRTUAL_SHARES = 1e3`, and share mints have 3 more decimals than CASH | An empty side keeps a defined, un-manipulable share price, and supplies stay within u64 |

## Allowed markets

World models each outcome of an event as its own binary market, so each entry is the market for the outcome shown in the app. They were found on-chain by matching the outcome mints' Token-2022 metadata:

| Event | Outcome | prediCt Market | YES mint |
|---|---|---|---|
| NE Patriots vs JAC Jaguars | JAC Jaguars | `HTX1g6exzFFHnoPypBCT3Xr7H7LFNom8jZhjZdo3PMZk` | `2KoffWttRBQ7NkcV7SQ7e72rSLejVcvtRagpcPVTJqwq` (`JJAG-Y`) |
| Columbus vs Miami | Miami | `BpaizvwfmCCraJJpFvsSCxPXAigULktom9FoiKiYyJJf` | `4DTXzSRELKn8pPUWo57NepfVFCkkpMTS2ggo68zsPQQg` (`IMIA-Y`) |
| Bitcoin 2026 Year-High | Above $99,999.99 | `4jdharW9HB7FRB46qgkvy1aCyNXEAaLWzkCCx8vgbCV2` | `AuMjrnXxaQm2TMziDh3pfunZPZvYDVAAvUutW2iurPc6` (`BTCH100-Y`) |

[`config/markets.json`](config/markets.json) has the full set (NO mints, CASH vaults). Call `allow_market` once per market; the pool reads the mints and vault from the Market account itself. For every other World market, `scripts/list_world_markets.py` lists them all with their pool status.

## Accepted risks

- **World's operator is fully trusted.** It writes outcomes with no on-chain oracle, and its permanent delegate can seize or burn outcome tokens from any account, including the pool's. Bridge holds the same power over CASH (CASH has a permanent delegate too).
- **Payouts follow prediCt's `UserSettings`.** `redeem_outcome_for_user` pays a holder's winnings to the payout wallet in its `["user_settings", holder]` account if one exists. Creating or updating it needs the holder's signature, which the pool PDA never gives, so the pool is always paid into its own CASH account (`nobody_can_set_up_payout_redirection_for_the_pool`).
- **Non-binary outcomes.** Live prediCt accepts any outcome in 0–10000 (for example 5000 for a void). Every resolved market seen on mainnet used 0 or 10000; `settle` refuses anything else, which leaves such a pool to the resolver fallback, and that fallback only records a winner.
- **Swapping costs both sides.** Each merge pays the CASH -> USDC fee (about 2.3 bp today) out of the invested balance, which both sides share. The keeper is trusted to route merges well; the most it can lose on one merge is `max_swap_loss_bps`. Fee rounding costs up to 5 raw units per swap.
- **CASH/USDC depeg.** USDC is counted at par. If CASH trades above USDC, redemptions that need a swap fail until it's back at or below par. Redemptions covered by dangling tokens still work, and winners are paid in USDC with no swap. If CASH trades below USDC beyond the keeper's allowance, merges wait.
- **Vanna must pay out.** A redemption that needs USDC fails while Vanna's reserve is frozen, the protocol is halted, or borrowers hold the liquidity (`winners_wait_for_vanna_liquidity_when_borrowers_hold_it`). A merge fails while Vanna refuses supply (paused, supply-capped). Both roll back whole and succeed once Vanna allows them. Vanna's share rounding costs up to one unit per supply or redemption, and its first-ever lender must supply at least 1000 raw units.
- **CASH extensions.** CASH is a Bridge-issued Token-2022 mint with a transfer-hook extension whose program is currently unset. If Bridge sets one, transfers would need extra accounts.
- **Settlement timing.** Call `settle` promptly after `determine_outcome`, because World closes markets shortly afterwards. The resolver fallback is the only trusted path, and it only opens once the market is gone.
- **Admin bootstrap.** Only the program's upgrade authority can call `initialize_config`, because a freshly deployed program can't be invoked in its deployment slot and so the first caller can't be trusted to be the deployer. Initialize the config before revoking the upgrade authority.

## Repository structure

```
programs/outcome_yield_pool/
  src/instructions/            admin, create_pool, deposit, merge_and_deposit, redeem, settle, redeem_settled
  src/predict.rs               World prediCt: Market layout, split/merge CPIs
  src/vanna.rs                 Vanna lending: Reserve layout, share math, refresh/supply/redeem CPIs
  src/jupiter.rs               Jupiter routes: validation, pool-signed CPI, balance checks
  src/math.rs                  share <-> asset conversion with virtual offsets
  tests/                       LiteSVM suites over real World, Jupiter, Orca and Vanna bytecode
config/markets.json            the World markets the tests allowlist
fixtures/world/                mainnet snapshot: prediCt program, markets, holders, CASH mint
fixtures/swap/                 mainnet snapshot: Jupiter and Whirlpool programs, CASH/USDC whirlpool, USDC mint
fixtures/vanna/                Vanna's lending program, built from source at VANNA_PROGRAM_ID
scripts/fetch_world_fixtures.py  refreshes fixtures/world
scripts/fetch_swap_fixtures.py   refreshes fixtures/swap
scripts/build_vanna_fixture.sh   rebuilds fixtures/vanna from ../Protocol_V1_Solana
scripts/list_world_markets.py    lists every World market on mainnet and whether it has a pool yet
```

## Tests run on real code

There are no mocks. Every program the pool calls runs as its real bytecode in LiteSVM:

- **World.** [`scripts/fetch_world_fixtures.py`](scripts/fetch_world_fixtures.py) snapshots the deployed prediCt program and CASH mint. For each of the three markets it saves the Market account, both outcome mints, the market CASH vault, every holder's YES/NO token account, and their `UserSettings` and payout CASH accounts. It also saves one resolved-YES and one resolved-NO market that World had not yet closed. The script checks that holder balances add up to each mint's supply and that the vault holds one CASH per outstanding set. Tests drive World's real instructions, including the operator's `determine_outcome`, `redeem_outcome_for_user`, `burn_worthless_outcome` and `close_market`. A full wind-down pays or burns every real holder so that `close_market` can delete the mints.
- **Swaps.** [`scripts/fetch_swap_fixtures.py`](scripts/fetch_swap_fixtures.py) asks Jupiter's API for direct CASH <-> USDC routes through the Orca whirlpool in both directions, at several sizes. It snapshots every account those routes name, plus the Jupiter and Whirlpool programs. The tests rebuild the route for any amount and run it through Jupiter, exactly as a client would.
- **Vanna.** [`scripts/build_vanna_fixture.sh`](scripts/build_vanna_fixture.sh) builds `Protocol_V1_Solana/programs/vanna_lending` with its program id rewritten to `VANNA_PROGRAM_ID`, since Anchor refuses to run under any other id. The tests set Vanna up the way its admin would: protocol, a USDC reserve over the real USDC mint, a seed lender, and a collateral asset. A real borrower draws USDC and pays interest as the clock moves, which is where the pools' yield comes from.

The clock starts at the later of the two snapshots, since the whirlpool refuses a clock older than its last update. Only these things are not real:

- **CASH, USDC and collateral balances** are written into token accounts, because only Bridge and Circle can mint them.
- **Pyth prices** for the borrower's health check are written directly, as Vanna's own suite does.
- **The operator's signature** is a placeholder: the chain runs with signature verification off, so tests can send exactly the instructions World's operator key sends.

Re-snapshot with the scripts above. World's markets must still exist on mainnet: once World closes one, replace it in the script.

| Test suite | Covers |
|---|---|
| `factory.rs` | Config, allowlist, swap policy, pool creation, Vanna reserve validation |
| `deposit.rs` | Pricing, merges into Vanna, donations |
| `redeem.rs` | Dangling-first, Vanna redemption + swap + split, route sizing, dust |
| `authorization.rs` | Owners, delegates, transfers |
| `deferred_merge.rs` | Merges refused by Vanna or by the swap policy, and their recovery |
| `yield_distribution.rs` | Real Vanna interest by side and by utilization |
| `resolution.rs` | Settlement, USDC payouts, resolver fallback, late-deposit exploit |
| `market_isolation.rs` | Pools sharing one reserve never reach each other's funds |
| `swap_guards.rs` | Every way a route could reach past the swap; sandwiches; Vanna refusing |
| `vanna_multi_pool.rs` | Three markets, three pools, one reserve: account collisions, reserve reconciliation, pro-rata interest, conversions, full lifecycle |
| `invariant.rs` | Random operations over all three pools, with conservation and reserve reconciliation after every step |
| `world_live.rs` | The pool's assumptions about prediCt, the live whirlpool and Vanna's layout |

## Development

Requires Solana 3.1.10 (platform tools v1.52) and Anchor 1.2. The fixtures are committed; rebuild them only to refresh.

```sh
cargo build-sbf                        # builds the pool into target/deploy
cargo test -p outcome_yield_pool       # unit tests + LiteSVM integration suites
cargo clippy --workspace --lib

scripts/build_vanna_fixture.sh         # after a Vanna change, or if VANNA_PROGRAM_ID changes
python3 scripts/fetch_swap_fixtures.py # refresh the CASH/USDC route
python3 scripts/fetch_world_fixtures.py
```

The tests load `target/deploy/*.so`, so run `cargo build-sbf` after every program change.

`anchor build` in Anchor 1.2 defaults to SBPF v3 with platform-tools v1.57, which LiteSVM can't load. `anchor test` works because its test script rebuilds with `cargo build-sbf` first (`anchor test --skip-build` avoids the extra build). Deploy the `cargo build-sbf` output that the tests ran against, or `ANCHOR_BUILD_SBF_ARCH=v0 anchor build --tools-version v1.52`.

## License

GPL-2.0-or-later. See [LICENSE](LICENSE).
