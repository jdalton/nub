#!/usr/bin/env bash
# Module-count scaling sweep: isolate the perry per-module-init cost in the
# produced binary by timing cooked-binary cold start vs plain-node cold start
# across several module counts K. The compute arg (HN) is held small so every
# run stays startup-dominated.
#
#   ./examples/cook/scaling-sweep.sh
#
# For each K in KS:
#   1. gen K modules (8 fns each) + barrel + entry.mts (heavy/scaling-gen.mjs)
#   2. perry compile entry.mts -> native binary (built ONCE, outside timing)
#   3. hyperfine the cooked binary cold start (--warmup 5, >=RUNS runs)
#   4. hyperfine plain `node entry.mts` cold start
# All fixtures + per-K JSON land in a temp dir (auto-cleaned); the summary TSV +
# the least-squares fit (cook/node slope, floor, crossover K*) print at the end.
#
# Env overrides: PERRY_BIN, NODE_BIN, HN (compute arg, default 100), RUNS
# (default 40), KS (default "1 5 15 30 45 60").
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PERRY_BIN="${PERRY_BIN:-$HOME/projects/perry/target/release/perry}"
NODE_BIN="${NODE_BIN:-node}"
HN="${HN:-100}"          # compute arg — small, startup-dominated
RUNS="${RUNS:-40}"
KS=(${KS:-1 5 15 30 45 60})

command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
[ -x "$PERRY_BIN" ] || { echo "missing perry: $PERRY_BIN" >&2; exit 1; }

NODE_VER="$("$NODE_BIN" --version)"
PERRY_VER="$("$PERRY_BIN" --version)"
PLATFORM="$(uname -sm)"
echo "node:     $NODE_VER"
echo "perry:    $PERRY_VER"
echo "platform: $PLATFORM"
echo "HN (compute arg): $HN   RUNS: $RUNS   KS: ${KS[*]}"
echo

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

summary="$work/sweep-summary.tsv"
echo -e "K\tfns\tcook_mean_ms\tcook_stddev_ms\tnode_mean_ms\tnode_stddev_ms" > "$summary"

for K in "${KS[@]}"; do
  echo "── K=$K ($((K*8)) fns) ─────────────────────────────────────────"
  d="$work/k$K"
  "$NODE_BIN" "$here/heavy/scaling-gen.mjs" "$K" "$d"
  cooked="$d/cooked"

  # Build the cooked native binary ONCE — outside the timed loop.
  timeout 240 "$PERRY_BIN" compile "$d/entry.mts" -o "$cooked" >/dev/null 2>&1 || {
    echo "perry compile FAILED at K=$K" >&2; exit 1; }
  chmod +x "$cooked"

  # Equivalence: cooked binary must print the same as node before timing.
  want="$("$NODE_BIN" "$d/entry.mts" "$HN")"
  got="$("$cooked" "$HN")"
  [ "$got" = "$want" ] || { echo "DIVERGENCE at K=$K: cooked($got) != node($want)" >&2; exit 1; }
  echo "equivalence ✓ (output=$want)"

  out_cook="$work/sweep-k$K-cook.json"
  out_node="$work/sweep-k$K-node.json"

  hyperfine --warmup 5 --min-runs "$RUNS" -N \
    --export-json "$out_cook" -n "cook-k$K" "$cooked $HN"
  hyperfine --warmup 5 --min-runs "$RUNS" -N \
    --export-json "$out_node" -n "node-k$K" "$NODE_BIN $d/entry.mts $HN"

  # Pull means/stddevs (seconds) -> ms into the summary.
  read cm cs <<<"$("$NODE_BIN" -e 'const r=require(process.argv[1]).results[0];process.stdout.write((r.mean*1000).toFixed(2)+" "+(r.stddev*1000).toFixed(2))' "$out_cook")"
  read nm ns <<<"$("$NODE_BIN" -e 'const r=require(process.argv[1]).results[0];process.stdout.write((r.mean*1000).toFixed(2)+" "+(r.stddev*1000).toFixed(2))' "$out_node")"
  echo -e "$K\t$((K*8))\t$cm\t$cs\t$nm\t$ns" >> "$summary"
  echo
done

echo "==== SUMMARY ===="
cat "$summary"
echo

# Linear fits: cook = a_cook + b_cook*K ; node = a_node + b_node*K (least squares
# on the per-K means). b_* is the ms/module slope; a_* is the K->0 floor; the
# crossover K* is where the two lines meet.
"$NODE_BIN" - "$summary" <<'EOF'
const fs = require("node:fs");
const lines = fs.readFileSync(process.argv[2], "utf8").trim().split("\n").slice(1);
const rows = lines.map(l => l.split("\t").map(Number));
const K = rows.map(r => r[0]);
const cook = rows.map(r => r[2]);
const node = rows.map(r => r[4]);
function fit(xs, ys) {
  const n = xs.length;
  const sx = xs.reduce((a,b)=>a+b,0), sy = ys.reduce((a,b)=>a+b,0);
  const sxx = xs.reduce((a,x)=>a+x*x,0), sxy = xs.reduce((a,x,i)=>a+x*ys[i],0);
  const b = (n*sxy - sx*sy) / (n*sxx - sx*sx); // slope
  const a = (sy - b*sx) / n;                   // intercept
  const ym = sy/n;
  const ssTot = ys.reduce((s,y)=>s+(y-ym)**2,0);
  let ssRes = 0; for (let i=0;i<n;i++){ const pred=b*xs[i]+a; ssRes+=(ys[i]-pred)**2; }
  const r2 = 1 - ssRes/ssTot;
  return { a, b, r2 };
}
const fc = fit(K, cook), fn = fit(K, node);
console.log(`cook fit:  intercept(floor K->0) = ${fc.a.toFixed(2)} ms   slope = ${fc.b.toFixed(3)} ms/module   R^2 = ${fc.r2.toFixed(4)}`);
console.log(`node fit:  intercept(floor K->0) = ${fn.a.toFixed(2)} ms   slope = ${fn.b.toFixed(3)} ms/module   R^2 = ${fn.r2.toFixed(4)}`);
// crossover: fc.a + fc.b*K = fn.a + fn.b*K  ->  K* = (fn.a - fc.a)/(fc.b - fn.b)
const kStar = (fn.a - fc.a) / (fc.b - fn.b);
console.log(`crossover K* (cook == node) = ${kStar.toFixed(1)} modules  (~${(kStar*8).toFixed(0)} fns)`);
EOF
