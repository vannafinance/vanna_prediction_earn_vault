#!/usr/bin/env python3
"""Snapshots World's live prediCt program and the three allowlisted markets from Solana mainnet.

The pool's tests run against these bytes: the real prediCt program (dumped from its programdata account) and the
real Market, outcome mints, market CASH vault and CASH mint, exactly as they are on chain at snapshot time.

    python3 scripts/fetch_world_fixtures.py [--rpc URL]

Writes fixtures/world/predict.so and fixtures/world/accounts/<pubkey>.json, and refreshes config/markets.json.
Standard library only.
"""

import argparse
import base64
import hashlib
import json
import pathlib
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "fixtures" / "world"

PREDICT = "prediCtPZCttYMvm2W3PtxmMxLmT1dtN7riU6Cxh6tM"
CASH_MINT = "CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH"
TOKEN_2022 = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
MARKET_DISCRIMINATOR = bytes.fromhex("dbbed53700e3c69a")

# The three markets World lists for this pool. World models each outcome of an event as its own binary market, so
# each entry is the market for the outcome shown in the app.
MARKETS = [
    {"title": "NE Patriots vs JAC Jaguars", "outcome": "JAC Jaguars", "market": "HTX1g6exzFFHnoPypBCT3Xr7H7LFNom8jZhjZdo3PMZk"},
    {"title": "Columbus vs Miami", "outcome": "Miami", "market": "BpaizvwfmCCraJJpFvsSCxPXAigULktom9FoiKiYyJJf"},
    {"title": "Bitcoin 2026 Year-High", "outcome": "Above $99,999.99", "market": "4jdharW9HB7FRB46qgkvy1aCyNXEAaLWzkCCx8vgbCV2"},
]

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58encode(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


def b58decode(s: str) -> bytes:
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\0" * (len(s) - len(s.lstrip("1"))) + raw


class Rpc:
    def __init__(self, url: str):
        self.url = url

    def call(self, method, params):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, data=body, headers={"content-type": "application/json"})
        for attempt in range(6):
            try:
                reply = json.load(urllib.request.urlopen(req, timeout=60))
                if "result" in reply:
                    return reply["result"]
                err = reply.get("error")
            except Exception as e:  # noqa: BLE001
                err = e
            time.sleep(1.5 * (attempt + 1))
        raise SystemExit(f"{method} failed: {err}")

    def accounts(self, keys):
        res = self.call("getMultipleAccounts", [keys, {"encoding": "base64"}])
        return res["context"]["slot"], res["value"]

    def accounts_batched(self, keys):
        """Reads any number of accounts; returns the newest slot seen. Callers check the result for consistency."""
        slots, values = [], []
        for i in range(0, len(keys), 100):
            slot, batch = self.accounts(keys[i : i + 100])
            slots.append(slot)
            values += batch
        return max(slots), values


# ed25519 curve check, for deriving program addresses without dependencies.
_P = 2**255 - 19
_D = (-121665 * pow(121666, _P - 2, _P)) % _P


def _on_curve(b: bytes) -> bool:
    y = int.from_bytes(b, "little") & ((1 << 255) - 1)
    if y >= _P:
        return False
    u, v = (y * y - 1) % _P, (_D * y * y + 1) % _P
    x2 = u * pow(v, _P - 2, _P) % _P
    if x2 == 0:
        return True
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P == 0:
        return True
    x = x * pow(2, (_P - 1) // 4, _P) % _P
    return (x * x - x2) % _P == 0


def find_pda(seeds, program: str) -> str:
    for bump in range(255, -1, -1):
        h = hashlib.sha256(b"".join(seeds) + bytes([bump]) + b58decode(program) + b"ProgramDerivedAddress").digest()
        if not _on_curve(h):
            return b58encode(h)
    raise ValueError("no PDA")


def user_settings(owner: str) -> str:
    return find_pda([b"user_settings", b58decode(owner)], PREDICT)


def cash_ata(owner: str) -> str:
    return find_pda([b58decode(owner), b58decode(TOKEN_2022), b58decode(CASH_MINT)], ATA_PROGRAM)


def payout_cash_accounts(rpc: Rpc, owners) -> list:
    """The CASH account prediCt pays each owner's winnings into: its UserSettings payout wallet's, else its own."""
    owners = sorted(owners)
    settings = [user_settings(o) for o in owners]
    _, infos = rpc.accounts_batched(settings)
    wallets = set()
    for owner, info in zip(owners, infos):
        if info is not None and info["owner"] == PREDICT:
            wallets.add(b58encode(base64.b64decode(info["data"][0])[41:73]))
        else:
            wallets.add(owner)
    return settings + [cash_ata(w) for w in sorted(wallets)]


def describe(rpc: Rpc, m: dict) -> dict:
    _, (info,) = rpc.accounts([m["market"]])
    if info is None:
        raise SystemExit(f"market {m['market']} ({m['title']}) no longer exists: World closed it")
    data = base64.b64decode(info["data"][0])
    assert info["owner"] == PREDICT and data[:8] == MARKET_DISCRIMINATOR, m
    assert b58encode(data[40:72]) == CASH_MINT, m
    yes, no, vault = (b58encode(data[o : o + 32]) for o in (72, 104, 136))
    return {**m, "yesMint": yes, "noMint": no, "cashVault": vault}


def holders(rpc: Rpc, mint: str):
    """Every Token-2022 account of `mint`, and the wallets that own them."""
    res = rpc.call(
        "getProgramAccounts",
        [TOKEN_2022, {"encoding": "base64", "filters": [{"memcmp": {"offset": 0, "bytes": mint}}]}],
    )
    accounts, owners = [], set()
    for a in res:
        data = base64.b64decode(a["account"]["data"][0])
        if len(data) >= 165 and b58encode(data[:32]) == mint:
            accounts.append(a["pubkey"])
            owners.add(b58encode(data[32:64]))
    return accounts, owners


def consistent(m: dict, found: dict) -> bool:
    def u64(key, offset):
        return int.from_bytes(base64.b64decode(found[key]["data"][0])[offset : offset + 8], "little")

    held = {m["yesMint"]: 0, m["noMint"]: 0}
    for h in m["holders"]:
        held[b58encode(base64.b64decode(found[h]["data"][0])[:32])] += u64(h, 64)
    supply_yes, supply_no = u64(m["yesMint"], 36), u64(m["noMint"], 36)
    return held == {m["yesMint"]: supply_yes, m["noMint"]: supply_no} and u64(m["cashVault"], 64) == supply_yes == supply_no


def find_resolved(rpc: Rpc) -> list:
    """One resolved-but-not-yet-closed market per outcome, so settlement can be checked on real resolved accounts."""
    res = rpc.call(
        "getProgramAccounts",
        [PREDICT, {"encoding": "base64", "filters": [{"dataSize": 320}], "dataSlice": {"offset": 176, "length": 10}}],
    )
    picked = {}
    for a in sorted(res, key=lambda a: a["pubkey"]):
        d = base64.b64decode(a["account"]["data"][0])
        slot, outcome = int.from_bytes(d[:8], "little"), int.from_bytes(d[8:10], "little")
        if slot and outcome in (0, 10000):
            picked.setdefault(outcome, a["pubkey"])
    return [
        {"title": f"resolved on mainnet, {'YES' if o else 'NO'} won", "outcome": o, "market": picked[o]}
        for o in (10000, 0)
    ]


def save_account(pubkey: str, acc: dict, slot: int):
    (OUT / "accounts").mkdir(parents=True, exist_ok=True)
    doc = {
        "pubkey": pubkey,
        "slot": slot,
        "lamports": acc["lamports"],
        "owner": acc["owner"],
        "executable": acc["executable"],
        "data": acc["data"][0],
    }
    (OUT / "accounts" / f"{pubkey}.json").write_text(json.dumps(doc, indent=1) + "\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="https://api.mainnet-beta.solana.com")
    rpc = Rpc(ap.parse_args().rpc)
    OUT.mkdir(parents=True, exist_ok=True)

    # prediCt is an upgradeable program: the executable is the programdata account minus its 45-byte header.
    _, (program,) = rpc.accounts([PREDICT])
    programdata = b58encode(base64.b64decode(program["data"][0])[4:36])
    slot, (pd,) = rpc.accounts([programdata])
    elf = base64.b64decode(pd["data"][0])[45:]
    (OUT / "predict.so").write_bytes(elf)
    print(f"predict.so  {len(elf)} bytes  sha256 {hashlib.sha256(elf).hexdigest()}  slot {slot}")

    markets_out = [describe(rpc, m) for m in MARKETS]
    resolved_out = [describe(rpc, m) for m in find_resolved(rpc)]

    # Everything is re-read in one consistent state: every holder's balance must add up to its mint's supply, and
    # the market vault must hold exactly one CASH per outstanding complete set.
    for attempt in range(5):
        keys = [CASH_MINT]
        for m in markets_out:
            (yes_accounts, yes_owners), (no_accounts, no_owners) = holders(rpc, m["yesMint"]), holders(rpc, m["noMint"])
            m["holders"] = sorted(yes_accounts + no_accounts)
            keys += [m["market"], m["yesMint"], m["noMint"], m["cashVault"]] + m["holders"]
            keys += payout_cash_accounts(rpc, yes_owners | no_owners)
        for m in resolved_out:
            keys += [m["market"], m["yesMint"], m["noMint"], m["cashVault"]]
        keys = list(dict.fromkeys(keys))
        slot, infos = rpc.accounts_batched(keys)
        found = {k: v for k, v in zip(keys, infos) if v is not None}
        if all(consistent(m, found) for m in markets_out):
            break
        print("balances moved while snapshotting; retrying")
    else:
        raise SystemExit("could not take a consistent snapshot")

    for key, info in found.items():
        save_account(key, info, slot)
    unix_timestamp = rpc.call("getBlockTime", [slot])
    print(f"{len(found)} accounts at slot {slot} (unix time {unix_timestamp})")
    for m in markets_out + resolved_out:
        print(f"  {m['market']}  {m['title']}: {len(m.get('holders', []))} holder accounts")

    snapshot = {"slot": slot, "unixTimestamp": unix_timestamp, "markets": markets_out, "resolved": resolved_out}
    (OUT / "markets.json").write_text(json.dumps(snapshot, indent=1) + "\n")
    config = {
        "description": "World markets the admin allowlists with `allow_market`. `market` is the prediCt Market "
        "account; the pool reads the YES/NO mints and CASH vault from it on-chain, the others are listed for "
        "reference. Regenerate with scripts/fetch_world_fixtures.py.",
        "predictProgram": PREDICT,
        "collateralMint": CASH_MINT,
        "markets": [{k: v for k, v in m.items() if k != "holders"} for m in markets_out],
    }
    (ROOT / "config" / "markets.json").write_text(json.dumps(config, indent=2) + "\n")


if __name__ == "__main__":
    main()
