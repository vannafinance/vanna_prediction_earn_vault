#!/usr/bin/env bash
# Builds Vanna's lending program from source for the test suite, at the program id the pool calls.
#
#   scripts/build_vanna_fixture.sh [path/to/Protocol_V1_Solana]
#
# The source declares a different program id, and Anchor refuses to run under any id but its own, so the build runs on
# a temporary copy with `declare_id!` rewritten to `VANNA_PROGRAM_ID` from constants.rs. The source tree is not touched.
# Writes fixtures/vanna/vanna_lending.so. Once Vanna is deployed at that id, dump the live program instead:
#
#   solana program dump -u mainnet-beta <VANNA_PROGRAM_ID> fixtures/vanna/vanna_lending.so
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:-$ROOT/../Protocol_V1_Solana}"
SRC="$(cd "$SRC" && pwd)"
PROGRAM_ID="$(sed -n 's/^pub const VANNA_PROGRAM_ID: Pubkey = pubkey!("\(.*\)");$/\1/p' "$ROOT/programs/outcome_yield_pool/src/constants.rs")"
[ -n "$PROGRAM_ID" ] || { echo "VANNA_PROGRAM_ID not found in constants.rs" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cp "$SRC/Cargo.toml" "$SRC/Cargo.lock" "$SRC/rust-toolchain.toml" "$WORK/"
mkdir -p "$WORK/programs"
rsync -a --exclude target "$SRC/programs/vanna_lending" "$WORK/programs/"

LIB="$WORK/programs/vanna_lending/src/lib.rs"
sed -i.bak "s/^declare_id!(\"[^\"]*\");/declare_id!(\"$PROGRAM_ID\");/" "$LIB"
grep -q "declare_id!(\"$PROGRAM_ID\")" "$LIB" || { echo "could not rewrite declare_id!" >&2; exit 1; }

(cd "$WORK" && cargo build-sbf --manifest-path programs/vanna_lending/Cargo.toml)
mkdir -p "$ROOT/fixtures/vanna"
cp "$WORK/target/deploy/vanna_lending.so" "$ROOT/fixtures/vanna/vanna_lending.so"
echo "fixtures/vanna/vanna_lending.so  $(shasum -a 256 "$ROOT/fixtures/vanna/vanna_lending.so" | cut -d' ' -f1)  id $PROGRAM_ID  source $(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo unknown)"
