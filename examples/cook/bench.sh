#!/usr/bin/env bash
# Cold-start comparison for `nub cook`: the AOT-compiled native binary vs the two
# ways you'd otherwise run the same script — plain `node` and `nub` (transpile +
# Node). The point is COLD start: a fresh process per run, startup-dominated work
# (small fib `n`), measured with `hyperfine --warmup 5 -N` (≥30 runs).
#
#   ./examples/cook/bench.sh
#
# Env overrides:
#   PERRY_BIN   perry binary   (default: ~/projects/perry/target/release/perry)
#   NODE_BIN    node binary    (default: first `node` on PATH; ≥22.18 for native
#                               .mts type-stripping)
#   NUB_BIN     nub binary     (default: first `nub` on PATH)
#   N           fib argument   (default: 30)
#   HN          heavy-script arg (default: 100 — kept small on purpose so the
#                               heavy group stays STARTUP-dominated, not a compute
#                               benchmark)
#   RUNS        hyperfine min runs (default: 40)
#
# The three approaches (the trivial group):
#   1. cook (perry AOT) — compile the script to a native binary, run it. This is
#                         exactly what `nub cook` produces (cook wraps perry); the
#                         bench calls perry directly so it needs no nub-cook build.
#   2. node             — `node script.mts`  (baseline; native .mts type-stripping)
#   3. nub              — `nub script.ts`    (transpile + Node — the normal run path)
#
# Two groups:
#   - fib   (trivial): startup-dominated, almost no code to load — cook removes the
#                      runtime entirely and owns this case.
#   - heavy (480 fns across 60 ESM modules): a real parse/compile/instantiate cost.
#                      The heavy group is cook vs node only (the import-graph cost
#                      is the question; the nub run-path bar doesn't add to it).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PERRY_BIN="${PERRY_BIN:-$HOME/projects/perry/target/release/perry}"
NODE_BIN="${NODE_BIN:-node}"
NUB_BIN="${NUB_BIN:-nub}"
N="${N:-30}"
HN="${HN:-100}"
RUNS="${RUNS:-40}"

command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
command -v "$NODE_BIN" >/dev/null || { echo "node not found: $NODE_BIN" >&2; exit 1; }
command -v "$NUB_BIN"  >/dev/null || { echo "nub not found: $NUB_BIN"  >&2; exit 1; }
[ -x "$PERRY_BIN" ] || { echo "missing perry: $PERRY_BIN" >&2; exit 1; }

NODE_VER="$("$NODE_BIN" --version)"
PERRY_VER="$("$PERRY_BIN" --version | head -1)"
NUB_VER="$("$NUB_BIN" --version | head -1)"
echo "node:   $NODE_VER  ($NODE_BIN)"
echo "perry:  $PERRY_VER"
echo "nub:    $NUB_VER"
echo

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ── fib (trivial) — cook vs node vs nub ──────────────────────────────────────
echo "── fib (trivial)  (n=$N) ───────────────────────────────────────────"
cooked="$work/fib.cooked"
timeout 180 "$PERRY_BIN" compile "$here/fib.mts" -o "$cooked" >/dev/null 2>&1
chmod +x "$cooked"

# Equivalence gate: cook, node, and nub must print identical stdout before timing.
want="$("$NODE_BIN" "$here/fib.mts" "$N")"
got_cook="$("$cooked" "$N")"
got_nub="$("$NUB_BIN" "$here/fib.ts" "$N")"
[ "$got_cook" = "$want" ] || { echo "DIVERGENCE: cook != node" >&2; exit 1; }
[ "$got_nub"  = "$want" ] || { echo "DIVERGENCE: nub != node"  >&2; exit 1; }
echo "equivalence: cook, node, nub print identical output ✓"

hyperfine --warmup 5 --min-runs "$RUNS" -N --export-json "$here/results-fib.json" \
  -n "cook" "$cooked $N" \
  -n "node" "$NODE_BIN $here/fib.mts $N" \
  -n "nub"  "$NUB_BIN $here/fib.ts $N"
echo

# ── heavy (60-module import graph) — cook vs node ────────────────────────────
echo "── heavy (60 modules)  (n=$HN) ─────────────────────────────────────"
cooked_h="$work/heavy.cooked"
timeout 180 "$PERRY_BIN" compile "$here/heavy/heavy.mts" -o "$cooked_h" >/dev/null 2>&1
chmod +x "$cooked_h"

want_h="$("$NODE_BIN" "$here/heavy/heavy.mts" "$HN")"
got_ch="$("$cooked_h" "$HN")"
[ "$got_ch" = "$want_h" ] || { echo "DIVERGENCE: cook != node (heavy)" >&2; exit 1; }
echo "equivalence: cook, node print identical output ✓"

hyperfine --warmup 5 --min-runs "$RUNS" -N --export-json "$here/results-heavy.json" \
  -n "cook" "$cooked_h $HN" \
  -n "node" "$NODE_BIN $here/heavy/heavy.mts $HN"
echo

# Regenerate the chart from the two result JSONs.
"$NODE_BIN" "$here/make-chart.mjs" \
  --fib "$here/results-fib.json" \
  --heavy "$here/results-heavy.json" \
  --node-version "$NODE_VER" \
  --out "$here/cold-start"

echo "wrote $here/cold-start.svg (+ .png if a rasterizer is available)"
