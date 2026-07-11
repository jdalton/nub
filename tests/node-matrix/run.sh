#!/usr/bin/env bash
# Node-version matrix smoke runner. Drives nub through a set of scenarios against
# whatever `node` is on PATH (the matrix selects the Node version via actions/setup-node,
# so this script is version-agnostic). Reuses ONE prebuilt nub binary across every Node
# version — the Node version is a PATH/runtime choice, not a rebuild.
#
# Usage:  run.sh <path-to-nub-binary> [--collision-must-pass]
#
#   --collision-must-pass : assert the async-loader-collision fixture EXITS 0 (used on
#                           Node-fixed legs, and as the post-fix regression gate). Without
#                           it, the collision fixture is run for signal/logging but a crash
#                           is reported as EXPECTED-on-broken-tier rather than a failure, so
#                           the matrix is honest about which Node versions carry the Node bug.
#                           (See NODE_BROKEN_BANDS below — the script auto-detects.)
set -uo pipefail

NUB="${1:?usage: run.sh <nub-binary> [--collision-must-pass]}"
COLLISION_MUST_PASS=0
[[ "${2:-}" == "--collision-must-pass" ]] && COLLISION_MUST_PASS=1

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIX="$HERE/fixtures"
# The Node version is whatever `node` resolves to on PATH — that is the version nub will
# augment (nub discovers its Node from PATH). Read it directly; nub's own `-e` resolves a
# possibly-different default and would mislabel the leg.
NODE_VER="$(node --version 2>/dev/null || echo unknown)"
echo "== Node $NODE_VER ==  nub: $NUB"

fails=0
pass() { echo "  PASS  $1"; }
fail() { echo "  FAIL  $1"; fails=$((fails + 1)); }

# ENGINES-REDIRECT GUARD (defeats the false-green class this matrix exists to prevent).
# nub discovers its Node from PATH, but an `engines.node` / `.node-version` / `packageManager`
# constraint that the PATH-selected Node does NOT satisfy makes nub REJECT it and fall through
# to the highest installed Node — so a leg labeled "Node 22.16" would silently run on 26 and
# mask version-specific bugs. The fixtures ship a PERMISSIVE engines (>=18) so no lower-tier
# leg is redirected; this assertion is the backstop — it fails the leg if the running Node is
# not the matrix-selected one (a stray pin, a transitive constraint, a floor bump). Probe from
# the pin-free fixtures dir so the probe itself isn't redirected.
# fixtures/package.json anchors that isolation: `workspaces: []` is a workspace boundary, ending
# nub's project walk at the fixtures dir so repo-root config (pnpm-workspace.yaml + the root
# engines) can never capture the legs.
ACTUAL_VER="$(cd "$FIX/async-loader-collision" && "$NUB" --eval 'process.stdout.write(process.version)' 2>/dev/null || true)"
if [[ -n "$ACTUAL_VER" && -n "$NODE_VER" && "$NODE_VER" != "unknown" && "$ACTUAL_VER" != "$NODE_VER" ]]; then
  fail "version mismatch — matrix selected $NODE_VER but nub ran on $ACTUAL_VER (a pin is masking this leg's coverage)"
fi

# Assert: running `nub <args>` exits 0 and stdout contains <needle>.
expect_ok_contains() {
  local label="$1" needle="$2"; shift 2
  local out rc
  out="$("$NUB" "$@" 2>/tmp/nm-err.txt)"; rc=$?
  if [[ $rc -eq 0 && "$out" == *"$needle"* ]]; then
    pass "$label"
  else
    fail "$label (exit=$rc, stdout=[$out], stderr=[$(cat /tmp/nm-err.txt)])"
  fi
}

# ── Scenario A + broad functional smoke ──────────────────────────────────────
expect_ok_contains "hello.js"          "HELLO_JS:42"        "$FIX/functional/hello.js"
expect_ok_contains "hello.ts (transpile + enum)" "HELLO_TS:nub:1" "$FIX/functional/hello.ts"
expect_ok_contains "ESM imports CJS"   "ESM_CJS:42"         "$FIX/functional/esm-imports-cjs.mjs"
expect_ok_contains "import.meta.resolve" "META_RESOLVE:ok"  "$FIX/functional/meta-resolve.mjs"
expect_ok_contains "Worker threads"    "WORKER:WORKER_PONG" "$FIX/functional/worker-main.mjs"

# ── Scenario B: async-loader × sync-hooks collision (the resolveSync class) ───
# A Node version carries the bug iff it has registerHooks AND is in a broken band; this
# script doesn't enumerate bands — it just runs the fixture and judges by the policy flag.
coll_out="$(cd "$FIX/async-loader-collision" && "$NUB" main.mjs 2>/tmp/nm-coll-err.txt)"; coll_rc=$?
coll_err="$(cat /tmp/nm-coll-err.txt)"
if [[ $coll_rc -eq 0 && "$coll_out" == *"COLLISION_OK"* ]]; then
  pass "async-loader collision (no resolveSync crash)"
elif [[ "$coll_err" == *"ERR_METHOD_NOT_IMPLEMENTED"* || "$coll_err" == *"resolveSync"* || "$coll_err" == *"loadSync"* ]]; then
  if [[ $COLLISION_MUST_PASS -eq 1 ]]; then
    fail "async-loader collision: resolveSync/loadSync crash (exit=$coll_rc) — nub did NOT recover.
        stderr: $coll_err"
  else
    echo "  XFAIL async-loader collision: resolveSync/loadSync crash on this Node (exit=$coll_rc) —
        EXPECTED while nub does not yet recover the sync-into-async hop on Node-broken versions.
        This is the bug class PR #98 targets. Promote to FAIL (pass --collision-must-pass) once nub recovers."
  fi
else
  fail "async-loader collision: unexpected failure (exit=$coll_rc, stdout=[$coll_out], stderr=[$coll_err])"
fi

echo "== Node $NODE_VER: $fails failure(s) =="
exit $((fails > 0 ? 1 : 0))
