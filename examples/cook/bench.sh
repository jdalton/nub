#!/usr/bin/env bash
# Cold-start showcase of the Node startup-optimization landscape, run against one
# script per group across five approaches. The point is COLD start: a fresh
# process per run, startup-dominated work (small fib `n`), measured with
# `hyperfine --warmup 5 -N` (≥30 runs).
#
#   ./examples/cook/bench.sh
#
# Env overrides:
#   PERRY_BIN   perry binary        (default: ~/projects/perry/target/release/perry)
#   NODE_BIN    node binary         (default: first `node` on PATH; use ≥22.18 for
#                                    native .mts type-stripping, ≥22.8 for the
#                                    compile cache)
#   TSX_LOADER  tsx ESM loader .mjs  (default: auto-resolve a local `tsx` install)
#   N           fib argument         (default: 30)
#   HN          heavy-script arg     (default: 100 — kept small on purpose so the
#                                    heavy group stays STARTUP-dominated; node's
#                                    heavy time is flat from HN=1 to HN≈1000,
#                                    i.e. it's the 60-module parse/compile cost,
#                                    not the loop. A large HN turns it into a
#                                    compute benchmark, which is not the point.)
#   RUNS        hyperfine min runs   (default: 40)
#
# The five approaches (per group):
#   1. node             — `node script.mts`            (baseline; native .mts strip)
#   2. cook (perry AOT) — compile to a native binary, run it. This is exactly what
#                         `nub cook` produces (cook wraps perry) — the bench calls
#                         perry directly so it needs no nub build.
#   3. tsx              — `node --import tsx script.mts`  (a TypeScript loader hook;
#                         the reproducible stand-in for nub's transpile+Node run
#                         path, which the original 3-way bench measured as `nub run`)
#   4. v8 snapshot      — `node --snapshot-blob …`  (pre-baked parsed/compiled heap)
#   5. v8 compile cache — `NODE_COMPILE_CACHE=… node script.mts`  (warm code cache)
#
# Two groups, to show WHERE each lever pays off:
#   - fib   (trivial): startup-dominated, almost no code to parse/compile.
#   - heavy (480 fns across 60 ESM modules): a real parse/compile/instantiate cost.
#
# Approaches 4 and 5 do NOT remove the Node/V8 process boot — they only cut
# parse/compile/instantiate, so they shine on `heavy`, not on trivial `fib`.
# Only `cook` removes the runtime entirely.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PERRY_BIN="${PERRY_BIN:-$HOME/projects/perry/target/release/perry}"
NODE_BIN="${NODE_BIN:-node}"
N="${N:-30}"
HN="${HN:-100}"
RUNS="${RUNS:-40}"

command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
command -v "$NODE_BIN" >/dev/null || { echo "node not found: $NODE_BIN" >&2; exit 1; }
[ -x "$PERRY_BIN" ] || { echo "missing perry: $PERRY_BIN" >&2; exit 1; }

# Resolve a tsx ESM loader. Prefer an explicit TSX_LOADER; else look in this
# example's own node_modules, then a global-ish npm root. tsx is optional — if
# absent, the tsx row is skipped (and the bench says so).
resolve_tsx() {
  if [ -n "${TSX_LOADER:-}" ]; then echo "$TSX_LOADER"; return; fi
  for cand in \
    "$here/node_modules/tsx/dist/loader.mjs" \
    "$("$NODE_BIN" -e 'try{process.stdout.write(require.resolve("tsx/dist/loader.mjs"))}catch{}' 2>/dev/null)"; do
    [ -n "$cand" ] && [ -f "$cand" ] && { echo "$cand"; return; }
  done
  echo ""
}
TSX_LOADER="$(resolve_tsx)"

NODE_VER="$("$NODE_BIN" --version)"
PERRY_VER="$("$PERRY_BIN" --version)"
echo "node:   $NODE_VER  ($NODE_BIN)"
echo "perry:  $PERRY_VER"
[ -n "$TSX_LOADER" ] && echo "tsx:    $TSX_LOADER" || echo "tsx:    (not found — tsx row skipped)"
echo

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# ── one group: $1=label  $2=script.mts  $3=snapshot-entry.cjs  $4=arg  $5=json-out
run_group() {
  local label="$1" script="$2" snap_entry="$3" arg="$4" out="$5"
  local cooked="$work/$label.cooked"
  local blob="$work/$label.blob"
  local ccdir="$work/$label.ccache"

  echo "── $label  (arg=$arg) ─────────────────────────────────────────"

  # ── One-time BUILD costs, all OUTSIDE the timed loop below. hyperfine times
  #    only the RUNS, so cook's number is the cooked binary's *runtime* startup
  #    (the compiled program booting + initializing its modules), NOT this
  #    `perry compile` step; likewise the snapshot/compile-cache numbers are read
  #    cost, not the build below. Nothing here is in any reported bar.

  # Build the cooked native binary (the same compile `nub cook` runs).
  timeout 180 "$PERRY_BIN" compile "$script" -o "$cooked" >/dev/null 2>&1
  chmod +x "$cooked"

  # Build the snapshot blob.
  "$NODE_BIN" --snapshot-blob "$blob" --build-snapshot "$snap_entry" >/dev/null 2>&1

  # Prime the compile cache (one populate run; every timed run then reads it).
  mkdir -p "$ccdir"
  NODE_COMPILE_CACHE="$ccdir" "$NODE_BIN" "$script" "$arg" >/dev/null 2>&1

  # Equivalence gate: every approach must print identical stdout before timing.
  local want got
  want="$("$NODE_BIN" "$script" "$arg")"
  for desc in \
    "cooked|$cooked $arg" \
    "snapshot|$NODE_BIN --snapshot-blob $blob $arg" \
    "compile-cache|env NODE_COMPILE_CACHE=$ccdir $NODE_BIN $script $arg"; do
    got="$(eval "${desc#*|}")"
    [ "$got" = "$want" ] || { echo "DIVERGENCE in $label: ${desc%%|*} != node" >&2; exit 1; }
  done
  if [ -n "$TSX_LOADER" ]; then
    got="$("$NODE_BIN" --import "file://$TSX_LOADER" "$script" "$arg")"
    [ "$got" = "$want" ] || { echo "DIVERGENCE in $label: tsx != node" >&2; exit 1; }
  fi
  echo "equivalence: all approaches print identical output ✓"

  # Time them. -N disables the intermediate shell; --warmup 5; ≥RUNS runs.
  local -a cmds=(
    -n "node"             "$NODE_BIN $script $arg"
    -n "cook"             "$cooked $arg"
  )
  [ -n "$TSX_LOADER" ] && cmds+=( -n "tsx" "$NODE_BIN --import file://$TSX_LOADER $script $arg" )
  cmds+=(
    -n "v8 snapshot"      "$NODE_BIN --snapshot-blob $blob $arg"
    -n "v8 compile-cache" "env NODE_COMPILE_CACHE=$ccdir $NODE_BIN $script $arg"
  )
  hyperfine --warmup 5 --min-runs "$RUNS" -N --export-json "$out" "${cmds[@]}"
  echo
}

run_group "fib"   "$here/fib.mts"          "$here/fib-snapshot-entry.cjs"          "$N"  "$here/results-fib.json"
run_group "heavy" "$here/heavy/heavy.mts"  "$here/heavy/heavy-snapshot-entry.cjs"  "$HN" "$here/results-heavy.json"

# Regenerate the chart from the two result JSONs.
"$NODE_BIN" "$here/make-chart.mjs" \
  --fib "$here/results-fib.json" \
  --heavy "$here/results-heavy.json" \
  --node-version "$NODE_VER" \
  --out "$here/cold-start"

echo "wrote $here/cold-start.svg (+ .png via the chart script's note)"
