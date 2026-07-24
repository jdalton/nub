#!/usr/bin/env bash
# Regenerate the committed lockfiles from scratch.
# Run this when you change a fixture's package.json.
#
# pnpm-lock.yaml is regenerated with pnpm (below); nub.lock is regenerated with a
# current release nub for EVERY fixture (the tail of this script). The nub leg of
# each bench harness installs from nub.lock — its native lockfile — not the
# pnpm-lock.yaml compat-read.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/bench/install/fixtures"
NUB="${NUB:-$REPO_ROOT/target/release/nub}"

echo "=== Regenerating simple fixture lockfile ==="
(
  cd "$FIXTURE_DIR/simple"
  rm -rf node_modules pnpm-lock.yaml
  pnpm install --no-frozen-lockfile
  rm -rf node_modules
)

echo "=== Regenerating monorepo fixture lockfile ==="
(
  cd "$FIXTURE_DIR/monorepo"
  rm -rf node_modules packages/*/node_modules pnpm-lock.yaml
  pnpm install --no-frozen-lockfile
  rm -rf node_modules packages/*/node_modules
)

echo "=== Regenerating t3 fixture lockfile (pnpm) ==="
# t3-app: Bun's create-t3-app benchmark fixture — Next16/tRPC11/Drizzle/next-auth/Tailwind4
# package.json sourced from .repos/bun/bench/install/package.json
# bun.lock sourced from .repos/bun/bench/install/bun.lock (pre-committed, regen from bun if needed)
(
  cd "$FIXTURE_DIR/t3"
  rm -rf node_modules pnpm-lock.yaml
  pnpm install --no-frozen-lockfile
  rm -rf node_modules
)

echo "=== Regenerating nub.lock for every fixture (native nub lockfile) ==="
# nub writes its neutral nub.lock ONLY on a truly-fresh install — no foreign
# lockfile or packageManager signal present. So we install each fixture from its
# package.json alone in a temp copy, then copy the resulting nub.lock back.
[ -x "$NUB" ] || { echo "ERROR: nub not built at $NUB (cargo build --release -p nub-cli)"; exit 1; }
for fx in "$FIXTURE_DIR"/*/; do
  name="$(basename "$fx")"
  [ -f "$fx/package.json" ] || continue
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/gen-nublock-$name-XXXXXX")"
  cp -r "$fx/." "$tmp/"
  ( cd "$tmp"
    rm -f pnpm-lock.yaml pnpm-workspace.yaml bun.lock bun.lockb package-lock.json
    rm -rf node_modules packages/*/node_modules 2>/dev/null || true
    "$NUB" install >/dev/null 2>&1
  )
  if [ -f "$tmp/nub.lock" ]; then
    cp "$tmp/nub.lock" "$fx/nub.lock"
    echo "  $name: nub.lock ($(wc -l < "$fx/nub.lock" | tr -d ' ') lines)"
  else
    echo "  $name: FAILED to generate nub.lock" >&2
  fi
  rm -rf "$tmp"
done

echo "Done. Commit the updated lockfiles."
