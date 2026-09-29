#!/usr/bin/env python3
"""Snapshots the CASH <-> USDC swap path from Solana mainnet: Jupiter v6, Orca Whirlpools and the CASH/USDC whirlpool.

The pool swaps merged CASH into USDC for Vanna's USDC reserve, and USDC back into CASH when a redemption needs a split.
The tests run those swaps through the real Jupiter and Whirlpool programs over this snapshot. Jupiter's own API picks
the accounts: a direct Whirlpool route is requested in both directions at several sizes, and every non-user account it
references is saved, together with the two program executables.

    python3 scripts/fetch_swap_fixtures.py [--rpc URL]

Writes fixtures/swap/{jupiter.so, whirlpool.so, routes.json} and fixtures/swap/accounts/<pubkey>.json.
Standard library only.
"""

import argparse
import base64
import hashlib
import json
import pathlib
import sys
import urllib.parse
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from fetch_world_fixtures import Rpc, b58encode  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "fixtures" / "swap"

JUPITER = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4"
WHIRLPOOL = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc"
CASH_MINT = "CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH"
USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
JUP_API = "https://lite-api.jup.ag/swap/v1"
# Any wallet: the user-specific accounts it produces are dropped, the test substitutes the pool's own.
PLACEHOLDER_USER = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"
# Raw 6-decimal amounts. The tick arrays saved must cover every swap size the tests make.
SIZES = [1_000_000, 1_000_000_000, 10_000_000_000]
# Fixed programs and sysvars the route names; they are not snapshotted as accounts.
BUILTINS = {
    JUPITER, WHIRLPOOL, "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", "11111111111111111111111111111111", CASH_MINT,
}


def http(url, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers={"content-type": "application/json", "user-agent": "fixtures"})
    return json.load(urllib.request.urlopen(req, timeout=30))


def swap_instruction(input_mint, output_mint, amount, shared):
    params = {"inputMint": input_mint, "outputMint": output_mint, "amount": amount, "slippageBps": 10,
              "dexes": "Whirlpool", "onlyDirectRoutes": "true"}
    quote = http(JUP_API + "/quote?" + urllib.parse.urlencode(params))
    if "routePlan" not in quote or len(quote["routePlan"]) != 1:
        raise SystemExit(f"no direct Whirlpool route for {amount}: {quote}")
    reply = http(JUP_API + "/swap-instructions", {"quoteResponse": quote, "userPublicKey": PLACEHOLDER_USER,
                                                  "useSharedAccounts": shared, "wrapAndUnwrapSol": False,
                                                  "skipUserAccountsRpcCalls": True})
    ix = reply["swapInstruction"]
    assert ix["programId"] == JUPITER, ix["programId"]
    return quote, ix


def program_elf(rpc, program):
    # An upgradeable program's executable is its programdata account minus the 45-byte header.
    _, (acc,) = rpc.accounts([program])
    programdata = b58encode(base64.b64decode(acc["data"][0])[4:36])
    _, (pd,) = rpc.accounts([programdata])
    return base64.b64decode(pd["data"][0])[45:]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="https://api.mainnet-beta.solana.com")
    rpc = Rpc(ap.parse_args().rpc)
    (OUT / "accounts").mkdir(parents=True, exist_ok=True)

    for name, program in [("jupiter.so", JUPITER), ("whirlpool.so", WHIRLPOOL)]:
        elf = program_elf(rpc, program)
        (OUT / name).write_bytes(elf)
        print(f"{name}  {len(elf)} bytes  sha256 {hashlib.sha256(elf).hexdigest()}")

    user_accounts = set()
    keys = [USDC_MINT]
    routes = {}
    for direction, (inp, out) in {"cash_to_usdc": (CASH_MINT, USDC_MINT), "usdc_to_cash": (USDC_MINT, CASH_MINT)}.items():
        hops = []
        for size in SIZES:
            for shared in (False, True):
                quote, ix = swap_instruction(inp, out, size, shared)
                metas = ix["accounts"]
                # `route`: user source/destination at 2/3. `shared_accounts_route`: at 3/6, program authority at 1.
                user_slots = (2, 3) if not shared else (3, 6)
                user_accounts.update(metas[i]["pubkey"] for i in user_slots)
                fixed = 9 if not shared else 13
                hop = metas[fixed:]
                keys += [m["pubkey"] for m in metas if m["pubkey"] not in BUILTINS and m["pubkey"] != PLACEHOLDER_USER]
                data = base64.b64decode(ix["data"])
                hops.append({"size": size, "shared": shared, "data": data.hex(), "outAmount": quote["outAmount"],
                             "fixed": [m["pubkey"] for m in metas[:fixed]],
                             "hop": [{"pubkey": m["pubkey"], "writable": m["isWritable"]} for m in hop]})
        routes[direction] = hops
        print(f"{direction}: {len(hops)} routes, out per {SIZES[1]} in = {hops[2]['outAmount']}")

    keys = [k for k in dict.fromkeys(keys) if k not in user_accounts]
    slot, infos = rpc.accounts_batched(keys)
    saved = 0
    for key, info in zip(keys, infos):
        if info is None:
            continue  # e.g. an event authority or an uninitialized whirlpool oracle
        doc = {"pubkey": key, "slot": slot, "lamports": info["lamports"], "owner": info["owner"],
               "executable": info["executable"], "data": info["data"][0]}
        (OUT / "accounts" / f"{key}.json").write_text(json.dumps(doc, indent=1) + "\n")
        saved += 1
    unix_timestamp = rpc.call("getBlockTime", [slot])
    (OUT / "routes.json").write_text(json.dumps({"slot": slot, "unixTimestamp": unix_timestamp,
                                                 "placeholderUser": PLACEHOLDER_USER,
                                                 "userAccounts": sorted(user_accounts), "routes": routes},
                                                indent=1) + "\n")
    print(f"{saved} accounts at slot {slot} (unix time {unix_timestamp})")


if __name__ == "__main__":
    main()
