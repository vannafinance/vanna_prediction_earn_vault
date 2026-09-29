#!/usr/bin/env python3
"""Lists every World (prediCt) market on mainnet and whether it has a Vanna yield pool yet.

World creates one binary market per outcome of an event. Each needs its own pool, which is two instructions and no
deployment: `allow_market` (config admin) and `create_pool` (anyone). This script is the read-only half of adding
markets: it finds them, names them from their YES mint's Token-2022 metadata, and reports which still need a pool.

    python3 scripts/list_world_markets.py [--rpc URL] [--all] [--json out.json]

By default only open markets (World has not written a result) are listed; --all includes determined ones.
Standard library only.
"""

import argparse
import base64
import json
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from fetch_world_fixtures import CASH_MINT, MARKET_DISCRIMINATOR, PREDICT, Rpc, b58decode, b58encode, find_pda  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent
USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
TOKEN_METADATA_EXTENSION = 19


def constant(name: str) -> str:
    src = (ROOT / "programs/outcome_yield_pool/src/constants.rs").read_text()
    return re.search(rf'pub const {name}: Pubkey = pubkey!\("([^"]+)"\)', src).group(1)


def program_id() -> str:
    src = (ROOT / "programs/outcome_yield_pool/src/lib.rs").read_text()
    return re.search(r'declare_id!\("([^"]+)"\)', src).group(1)


def token_metadata(mint_data: bytes):
    """(name, symbol) from a Token-2022 mint's TokenMetadata extension, if it has one."""
    if len(mint_data) <= 166 or mint_data[165] != 1:  # AccountType::Mint
        return None
    i = 166
    while i + 4 <= len(mint_data):
        kind = int.from_bytes(mint_data[i : i + 2], "little")
        length = int.from_bytes(mint_data[i + 2 : i + 4], "little")
        value = mint_data[i + 4 : i + 4 + length]
        if kind == TOKEN_METADATA_EXTENSION:
            j = 64  # update authority, mint

            def string():
                nonlocal j
                n = int.from_bytes(value[j : j + 4], "little")
                s = value[j + 4 : j + 4 + n].decode("utf-8", "replace")
                j += 4 + n
                return s

            name, symbol = string(), string()
            return name, symbol
        if kind == 0 and length == 0:
            break
        i += 4 + length
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="https://api.mainnet-beta.solana.com")
    ap.add_argument("--all", action="store_true", help="include markets World has already determined")
    ap.add_argument("--json", help="also write the list to this file")
    args = ap.parse_args()
    rpc = Rpc(args.rpc)

    pool_program = program_id()
    vanna = constant("VANNA_PROGRAM_ID")
    reserve = find_pda([b"reserve", b58decode(USDC_MINT)], vanna)

    res = rpc.call(
        "getProgramAccounts",
        [PREDICT, {"encoding": "base64", "filters": [
            {"dataSize": 320},
            {"memcmp": {"offset": 0, "bytes": b58encode(MARKET_DISCRIMINATOR)}},
        ]}],
    )
    markets = []
    for a in res:
        d = base64.b64decode(a["account"]["data"][0])
        if b58encode(d[40:72]) != CASH_MINT:
            continue
        slot = int.from_bytes(d[176:184], "little")
        outcome = int.from_bytes(d[184:186], "little")
        if slot and not args.all:
            continue
        markets.append({
            "market": a["pubkey"],
            "yesMint": b58encode(d[72:104]),
            "noMint": b58encode(d[104:136]),
            "cashVault": b58encode(d[136:168]),
            "status": "open" if slot == 0 else f"determined ({'YES' if outcome == 10000 else 'NO' if outcome == 0 else outcome})",
        })

    # Titles from the YES mint's metadata; pool and allowlist status from the PDAs.
    keys = []
    for m in markets:
        m["allowedMarket"] = find_pda([b"allowed_market", b58decode(m["market"])], pool_program)
        m["pool"] = find_pda([b"pool", b58decode(m["market"]), b58decode(reserve)], pool_program)
        keys += [m["yesMint"], m["allowedMarket"], m["pool"]]
    _, infos = rpc.accounts_batched(keys) if keys else (0, [])
    for i, m in enumerate(markets):
        mint, allowed, pool = infos[3 * i : 3 * i + 3]
        meta = token_metadata(base64.b64decode(mint["data"][0])) if mint else None
        m["title"], m["symbol"] = meta if meta else ("?", "?")
        m["allowed"], m["hasPool"] = allowed is not None, pool is not None

    markets.sort(key=lambda m: (m["hasPool"], m["title"]))
    missing = [m for m in markets if not m["hasPool"]]
    print(f"{len(markets)} {'World' if args.all else 'open World'} markets, {len(missing)} without a pool "
          f"(pool program {pool_program}, Vanna reserve {reserve})\n")
    print(f"{'market':44}  {'symbol':12}  {'status':16}  allowed  pool  title")
    for m in markets:
        print(f"{m['market']:44}  {m['symbol'][:12]:12}  {m['status']:16}  {'yes' if m['allowed'] else 'no ':7}  "
              f"{'yes' if m['hasPool'] else 'no ':4}  {m['title'][:60]}")
    if args.json:
        pathlib.Path(args.json).write_text(json.dumps(markets, indent=2) + "\n")
        print(f"\nwrote {args.json}")


if __name__ == "__main__":
    main()
