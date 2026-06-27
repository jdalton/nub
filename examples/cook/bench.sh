#!/usr/bin/env bash
# Reproducible cold-start benchmark for `nub cook`, run against the vendored
# PerryTS build (`vendor/perry`, pinned by the submodule) — NOT whatever perry
# happens to be on PATH. Bumping the submodule and re-running this is how the
# numbers track "latest perry".
#
#   ./examples/cook/bench.sh
#
# Env overrides:
#   PERRY_BIN  perry binary       (default: vendor/perry/target/release/perry)
#   NUB_BIN    nub release binary (default: target/release/nub)
#   N          fib argument       (default: 30)
#
# What it measures: the cooked native binary vs the Node floor (`node fib.mjs`)
# vs `nub <file>` (transpile + augmented Node). The cooked binary is produced by
# `perry compile` directly — the exact compile `nub cook` runs internally — so
# the bench is independent of the cook cache's on-disk layout.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

PERRY_BIN="${PERRY_BIN:-$repo/vendor/perry/target/release/perry}"
NUB_BIN="${NUB_BIN:-$repo/target/release/nub}"
N="${N:-30}"

for bin in "$PERRY_BIN" "$NUB_BIN"; do
  [ -x "$bin" ] || { echo "missing executable: $bin" >&2; exit 1; }
done
command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
command -v node >/dev/null || { echo "node not found" >&2; exit 1; }

echo "perry:  $("$PERRY_BIN" --version)"
echo "node:   $(node --version)"
echo

cooked="$here/fib.cooked"
# Bound the compile so a wedged perry can't hang the bench (cook bounds it too).
timeout 120 "$PERRY_BIN" compile "$here/fib.ts" -o "$cooked"
chmod +x "$cooked"
trap 'rm -f "$cooked"' EXIT

# Sanity: the cooked binary and Node must agree before we time them.
got="$("$cooked" "$N")"
want="$(node "$here/fib.mjs" "$N")"
[ "$got" = "$want" ] || { echo "DIVERGENCE: cooked != node" >&2; exit 1; }

hyperfine --warmup 5 -N \
  "$cooked $N" \
  "node $here/fib.mjs $N" \
  "$NUB_BIN $here/fib.ts $N"
