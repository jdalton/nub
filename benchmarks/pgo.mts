#!/usr/bin/env nub
// Profile-guided-optimization build for the `nub` CLI.
//
// Three-phase rustc PGO flow:
//
//   1. Build nub with -Cprofile-generate (instrumented binary).
//   2. Train against nub's hot CPU paths — a hermetic Verdaccio install
//      (resolver / store / linker), a transpile of the multi-file TS
//      corpus (the oxc hot path), and a `nub run` (script-runner
//      orchestration) — so the profile covers the install/resolve and
//      transpile work the prompt calls out as the CPU win.
//   3. Merge .profraw via llvm-profdata, recompile with -Cprofile-use.
//
// Adapted from aube standalone's benchmarks/pgo.bash (the proven recipe);
// the install-training half reuses the vendored hermetic registry rig at
// vendor/aube/benchmarks/ (Verdaccio + the cold/warm configs already in
// nub's tree), so nub's PGO numbers train against the same registry model
// aube's do. The transpile + run training is nub-specific.
//
// This is the dogfood path: nub building nub. Run it with
//   nub benchmarks/pgo.mts
// It uses ONLY `node:` builtins (no nub-specific globals/APIs — brand
// boundary, and it stays node-runnable as a fallback:
//   node --experimental-strip-types benchmarks/pgo.mts) and ERASABLE
// TypeScript only (type annotations Node's --experimental-strip-types
// removes at load): no enums, no namespaces, no parameter properties.
//
// Local default: target/release-pgo/nub using profile=release-pgo.
//
// CI hooks (env vars; NUB_-prefixed — this is nub's own build tooling, not
// a user knob):
//   NUB_PGO_NO_LOCK=1          skip /tmp/nub-bench.lock acquisition (also
//                              auto-skipped if `flock` is missing, e.g. on
//                              macOS).
//   NUB_PGO_PROFILE=<profile>  cargo profile for both phases (default:
//                              release-pgo).
//   NUB_PGO_TARGET=<triple>    cross-compilation target (default: host).
//                              Output lands at target/<triple>/<profile>/.
//   NUB_PGO_BUILD_TOOL=<tool>  `cargo` (default) or `cross`. cross is used
//                              in CI for the Linux GNU/musl targets so the
//                              resulting binary keeps cross's older glibc
//                              baseline. The cross-built INSTRUMENTED binary
//                              runs on the amd64 host for training (cross's
//                              glibc is forward-compatible), which is why
//                              only the x86_64-on-amd64 cross targets are
//                              PGO-trainable — an arm64 cross binary can't
//                              execute on the x64 runner without QEMU.
//   NUB_PGO_SKIP_FINAL_BUILD=1 stop after merging .profraw (profile only).
//
// This script ONLY adds an optimized build path. The default release
// pipeline (`cargo build --release`) is untouched and remains the fallback
// for every platform that does not opt into PGO.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";

const HELP = `pgo — profile-guided-optimization build for the nub CLI

Usage:
  nub  benchmarks/pgo.mts
  node --experimental-strip-types benchmarks/pgo.mts

Three-phase rustc PGO flow: instrument (-Cprofile-generate) -> train against
nub's hot CPU paths (hermetic install + TS transpile + nub run) -> merge
.profraw with llvm-profdata -> rebuild with -Cprofile-use.

Env hooks (nub's own build tooling, not a user knob):
  NUB_PGO_NO_LOCK=1           skip /tmp/nub-bench.lock (auto-skip if flock missing)
  NUB_PGO_PROFILE=<profile>   cargo profile for both phases (default: release-pgo)
  NUB_PGO_TARGET=<triple>     cross-compilation target (default: host)
  NUB_PGO_BUILD_TOOL=<tool>   cargo (default) or cross
  NUB_PGO_SKIP_FINAL_BUILD=1  stop after merging .profraw (profile only)
`;

// SCRIPT_DIR / REPO_ROOT — this file lives at <repo>/benchmarks/pgo.mts.
const SCRIPT_DIR = dirname(resolve(process.argv[1]));
const REPO_ROOT = resolve(SCRIPT_DIR, "..");
// Reuse the vendored hermetic registry rig (Verdaccio + cold/warm configs +
// throttle proxy), already present in nub's tree under vendor/aube.
const HERMETIC_DIR = join(REPO_ROOT, "vendor/aube/benchmarks");

// Honor CARGO_TARGET_DIR so the script finds the build output wherever cargo
// wrote it (a per-worktree target dir locally, the default `target/` in CI).
// A relative CARGO_TARGET_DIR is resolved against the repo root, matching
// cargo's own interpretation.
function targetRoot(): string {
  const env = process.env.CARGO_TARGET_DIR;
  if (env && env.length > 0) {
    return isAbsolute(env) ? env : join(REPO_ROOT, env);
  }
  return join(REPO_ROOT, "target");
}

const TARGET_ROOT = targetRoot();
const PGO_DATA_DIR = join(TARGET_ROOT, "pgo-data");
const PGO_PROFRAW_DIR = join(PGO_DATA_DIR, "profraw");
const PGO_MERGED = join(PGO_DATA_DIR, "merged.profdata");

const PGO_PROFILE = process.env.NUB_PGO_PROFILE ?? "release-pgo";
const PGO_TARGET = process.env.NUB_PGO_TARGET ?? "";
const PGO_BUILD_TOOL = process.env.NUB_PGO_BUILD_TOOL ?? "cargo";

// With a target triple set, the build emits a one-element --target arg and the
// output lands under target/<triple>/<profile>/.
const TARGET_ARGS: string[] = PGO_TARGET ? [`--target=${PGO_TARGET}`] : [];
const TARGET_DIR_PART = PGO_TARGET ? `${PGO_TARGET}/` : "";

function log(msg: string): void {
  process.stderr.write(`${msg}\n`);
}

function die(msg: string): never {
  process.stderr.write(`ERROR: ${msg}\n`);
  process.exit(1);
}

// Run a command, streaming its output; throw on non-zero exit (mirrors
// `set -e`). `env` is merged onto process.env.
function run(
  cmd: string,
  args: string[],
  opts: { cwd?: string; env?: Record<string, string | undefined> } = {},
): void {
  execFileSync(cmd, args, {
    cwd: opts.cwd,
    env: opts.env ? { ...process.env, ...opts.env } : process.env,
    stdio: "inherit",
  });
}

// Run a command for its stdout; trimmed. Throws on non-zero exit.
function capture(cmd: string, args: string[]): string {
  return execFileSync(cmd, args, { encoding: "utf8" }).trim();
}

// Run a training command that MUST NOT abort the whole PGO build on failure
// (the bash original wraps every training run in `|| true`). A transient
// registry hiccup would otherwise sink the release leg under `set -e`; the
// real gate is the profraw-count check after the loop. Output is suppressed
// to match the bash `>/dev/null 2>&1`.
function trainRun(
  cmd: string,
  args: string[],
  opts: { cwd?: string; env?: Record<string, string | undefined> } = {},
): void {
  spawnSync(cmd, args, {
    cwd: opts.cwd,
    env: opts.env ? { ...process.env, ...opts.env } : process.env,
    stdio: "ignore",
  });
}

function profrawFiles(): string[] {
  if (!existsSync(PGO_PROFRAW_DIR)) return [];
  return readdirSync(PGO_PROFRAW_DIR).filter((f) => f.endsWith(".profraw"));
}

function clearProfraw(): void {
  for (const f of profrawFiles()) {
    rmSync(join(PGO_PROFRAW_DIR, f), { force: true });
  }
}

if (process.argv.slice(2).some((a) => a === "-h" || a === "--help")) {
  process.stdout.write(HELP);
  process.exit(0);
}

// Drive the install training against the hermetic Verdaccio mirror with the
// same throttle defaults aube's bench harness uses, so the registry behavior
// (and thus the resolver/store hot paths exercised) matches. Set on
// process.env so both this script's helpers and the spawned hermetic bash
// inherit them.
process.env.BENCH_HERMETIC = process.env.BENCH_HERMETIC ?? "1";
process.env.BENCH_BANDWIDTH = process.env.BENCH_BANDWIDTH ?? "500mbit";
process.env.BENCH_LATENCY = process.env.BENCH_LATENCY ?? "50ms";
// Force the bundled metadata primer on for the hermetic mirror — the primer
// is keyed to npmjs.org, so without this nub would skip its own warm-cache
// fast path on the bench registry. (nub honors the AUBE_* build/runtime knob
// inside the embedded engine — this is the engine's bench seam, not a
// user-facing nub knob.)
process.env.AUBE_FORCE_METADATA_PRIMER = process.env.AUBE_FORCE_METADATA_PRIMER ?? "true";

// ---------- /tmp/nub-bench.lock ----------
// flock is Linux-only; on macOS it isn't on PATH, so the lock is skipped
// (matching the bash `command -v flock` guard). The lock is held for the
// process lifetime by keeping the flock child alive; it releases when this
// process exits. We DON'T fail hard if locking is unavailable — only when an
// explicit flock acquisition times out. Acquisition is observed via a sentinel
// file the flock child touches (see LOCK_SENTINEL) because this script polls
// synchronously and can't service async stdout/exit events while it waits.
let lockChild: ReturnType<typeof spawn> | undefined;
// Sentinel file the flock child touches the instant it holds the lock. The
// script is fully synchronous (a blocking `spawnSync("sleep")` poll loop), so
// the lock child's stdout/exit events can NEVER fire while we wait — they're
// event-loop-bound and the loop blocks the loop. We make acquisition observable
// through the filesystem instead (the same pattern hermeticStart uses with its
// urlFile), polling existsSync(LOCK_SENTINEL) rather than an async-set flag.
// Per-pid path keeps concurrent runs from colliding; shell-safe (no spaces).
const LOCK_SENTINEL = join(tmpdir(), `nub-bench-lock-${process.pid}.acquired`);
function acquireLock(): void {
  if (process.env.NUB_PGO_NO_LOCK || !hasCmd("flock")) {
    log(">>> Skipping /tmp/nub-bench.lock (NUB_PGO_NO_LOCK or flock missing)");
    return;
  }
  log(">>> Acquiring /tmp/nub-bench.lock (30 min timeout)");
  // The flock child touches LOCK_SENTINEL the moment it holds the lock, then
  // sleeps to keep holding it; killing the child in cleanup releases the lock.
  // flock's own `-w 1800` bounds its internal wait when the lock is contended.
  rmSync(LOCK_SENTINEL, { force: true });
  const child = spawn(
    "flock",
    ["-w", "1800", "/tmp/nub-bench.lock", "-c", `touch ${shq(LOCK_SENTINEL)}; exec sleep 100000`],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  // Poll the sentinel synchronously — independent of the event loop, so the
  // blocking sleep below doesn't starve the signal. Deadline sits just above
  // flock's own 1800s wait so the contended case (flock blocks internally until
  // the other holder releases, then acquires and touches the sentinel) is fully
  // covered; if the sentinel never appears by then, acquisition genuinely failed.
  const deadline = Date.now() + 1810_000;
  while (!existsSync(LOCK_SENTINEL) && Date.now() < deadline) {
    // flock returns instantly when uncontended, so this loop typically runs a
    // handful of iterations; when contended it tracks flock's internal wait.
    spawnSync("sleep", ["0.1"]);
  }
  if (!existsSync(LOCK_SENTINEL)) {
    child.kill("SIGKILL");
    die("failed to acquire /tmp/nub-bench.lock after 30 min");
  }
  lockChild = child;
  log(">>> Lock acquired");
}

function hasCmd(cmd: string): boolean {
  // `command -v` is a shell builtin, so run it through bash; mirrors the bash
  // original's `command -v flock`. Absence of bash or the command both yield
  // a non-zero status -> treat as "not available".
  const r = spawnSync("bash", ["-c", `command -v ${cmd}`], { stdio: "ignore" });
  return r.status === 0;
}

// ---------- llvm-profdata resolution ----------
// llvm-profdata MUST match the rustc that instrumented the binary — a version
// skew silently produces an empty merge (caught defensively in phase 3a).
// Resolve it from the active toolchain's sysroot.
function resolveLlvmProfdata(): string {
  const host = capture("rustc", ["-vV"])
    .split("\n")
    .map((l) => l.match(/^host:\s*(.+)$/))
    .find(Boolean)?.[1];
  if (!host) die("could not parse `host:` from `rustc -vV`");
  const sysroot = capture("rustc", ["--print", "sysroot"]);
  const bin = join(sysroot, "lib/rustlib", host, "bin/llvm-profdata");
  if (!isExecutable(bin)) {
    log(`ERROR: llvm-profdata not found at ${bin}`);
    log("  Install with: rustup component add llvm-tools-preview");
    process.exit(1);
  }
  return bin;
}

function isExecutable(p: string): boolean {
  try {
    const st = statSync(p);
    return st.isFile() && (st.mode & 0o111) !== 0;
  } catch {
    return false;
  }
}

// ---------- hermetic registry lifecycle ----------
// The Verdaccio + throttle-proxy lifecycle lives in the vendored, UNCHANGED
// hermetic.bash (default-preserving — standalone aube's rig is byte-for-byte
// the source of truth). Reimplementing its ~330 lines in TS would diverge
// from that source. Instead we run ONE long-lived bash that sources it, calls
// hermetic_start (backgrounding Verdaccio + the proxy as ITS children so they
// outlive each TS training run), writes BENCH_REGISTRY_URL to a file, then
// blocks on stdin; closing stdin triggers hermetic_stop. All training
// orchestration stays here in TS.
type Hermetic = {
  registryUrl: string;
  stop: () => void;
};

function hermeticStart(instrumentedBin: string): Hermetic {
  const urlFile = mkdtempSync(join(tmpdir(), "nub-pgo-herm.")) + "/url";
  const script = [
    'set -euo pipefail',
    `SCRIPT_DIR=${shq(HERMETIC_DIR)}`,
    `source ${shq(join(HERMETIC_DIR, "hermetic.bash"))}`,
    "hermetic_start",
    `printf '%s' "$BENCH_REGISTRY_URL" > ${shq(urlFile)}`,
    // Block until our parent closes stdin (read returns non-zero at EOF),
    // then tear the registry + proxy down. `trap` covers a kill, too.
    "trap hermetic_stop EXIT",
    "while IFS= read -r _line; do :; done",
  ].join("\n");

  const child = spawn("bash", ["-c", script], {
    cwd: REPO_ROOT,
    env: { ...process.env, AUBE_BIN: instrumentedBin },
    stdio: ["pipe", "inherit", "inherit"],
  });

  // Wait for hermetic_start to publish the registry URL (warm pass can take a
  // while on a cold cache; Verdaccio readiness itself is fast).
  const deadline = Date.now() + 30 * 60_000;
  let url = "";
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      die("hermetic registry bash exited before publishing BENCH_REGISTRY_URL");
    }
    if (existsSync(urlFile)) {
      url = readFileSync(urlFile, "utf8").trim();
      if (url.length > 0) break;
    }
    spawnSync("sleep", ["0.5"]);
  }
  if (url.length === 0) die("hermetic registry failed to start (no BENCH_REGISTRY_URL)");

  const stop = () => {
    try {
      child.stdin?.end();
    } catch {
      /* already gone */
    }
    // Give bash's EXIT trap a moment to run hermetic_stop, then ensure it's
    // gone. Idempotent — safe to call from finally even if already stopped.
    const t = Date.now() + 15_000;
    while (child.exitCode === null && Date.now() < t) spawnSync("sleep", ["0.2"]);
    if (child.exitCode === null) child.kill("SIGTERM");
    rmSync(dirname(urlFile), { recursive: true, force: true });
  };

  return { registryUrl: url, stop };
}

// Shell-quote a single argument for embedding in the bash -c script.
function shq(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

// ---------- training ----------
// LLVM_PROFILE_FILE forces the instrumented binary to write profraw to a host
// path we control, regardless of what `-Cprofile-generate=<dir>` baked in at
// compile time (matters for the cross case where the compile-time path is a
// container path). %m disambiguates per module signature, %p per process —
// together they keep the training runs from colliding.
const PROFRAW_PATTERN = join(PGO_PROFRAW_DIR, "nub-%m-%p.profraw");

// --- install training: 3 cold + 3 warm hermetic installs ---
// Cold runs each get a fresh dir so the resolver, registry, store, and linker
// hot paths all run end-to-end. Warm runs reuse the last cold dir so the
// frozen-lockfile fast path is also represented.
function installCold(bin: string, trainDir: string, registryUrl: string, i: number): string {
  const runDir = join(trainDir, `cold.${i}`);
  rmSync(runDir, { recursive: true, force: true });
  mkdirSync(join(runDir, "home"), { recursive: true });
  copyFileSync(join(HERMETIC_DIR, "fixture.package.json"), join(runDir, "package.json"));
  writeFileSync(join(runDir, ".npmrc"), `registry=${registryUrl}\n`);
  writeFileSync(join(runDir, "home/.npmrc"), `registry=${registryUrl}\n`);
  log(`  train: cold install (${i})`);
  trainRun(bin, ["install", "--ignore-scripts"], {
    cwd: runDir,
    env: { HOME: join(runDir, "home") },
  });
  return runDir;
}

function installWarm(bin: string, runDir: string, i: number): void {
  log(`  train: warm install (${i})`);
  trainRun(bin, ["install", "--ignore-scripts"], {
    cwd: runDir,
    env: { HOME: join(runDir, "home") },
  });
}

// --- transpile training: the 100-module TS corpus, cold + warm cache ---
// This is nub's signature hot path (oxc-based TS/JSX transpile). The FIRST
// augmented run (empty cache under the throwaway HOME) profiles the full
// parse+transform+emit work across the 100-module import graph; the second
// run profiles the cache-hit fast path. Always the AUGMENTED entrypoint
// (`nub <file>`, NOT `--node`, which would disable augmentation and run plain
// Node with no oxc transpile — defeating the point). A throwaway HOME isolates
// the transpile cache so the developer's ~/.cache/nub is untouched.
function transpileTrain(bin: string, trainDir: string): void {
  const home = join(trainDir, "transpile-home");
  rmSync(home, { recursive: true, force: true });
  mkdirSync(home, { recursive: true });
  const entry = "benchmarks/multi-file/entry.ts";
  log("  train: transpile (cold cache)");
  trainRun(bin, [entry], { cwd: REPO_ROOT, env: { HOME: home } });
  log("  train: transpile (warm cache)");
  trainRun(bin, [entry], { cwd: REPO_ROOT, env: { HOME: home } });
}

// --- run training: script-runner orchestration ---
function runTrain(bin: string, trainDir: string): void {
  const runDir = join(trainDir, "run");
  rmSync(runDir, { recursive: true, force: true });
  mkdirSync(join(runDir, "home"), { recursive: true });
  writeFileSync(
    join(runDir, "package.json"),
    `{"name":"pgo-run","scripts":{"noop":"node -e 0"}}\n`,
  );
  log("  train: nub run (orchestration)");
  trainRun(bin, ["run", "noop"], { cwd: runDir, env: { HOME: join(runDir, "home") } });
}

// ---------- main ----------
function main(): void {
  acquireLock();

  const llvmProfdata = resolveLlvmProfdata();

  mkdirSync(PGO_PROFRAW_DIR, { recursive: true });
  clearProfraw();
  rmSync(PGO_MERGED, { force: true });

  // With NUB_PGO_BUILD_TOOL=cross, rustc runs inside a container that mounts
  // the project at a path that may differ from the host, so a host-path
  // `-Cprofile-use` baked into RUSTFLAGS would be invisible in phase 3b.
  // Bind-mount PGO_DATA_DIR at the same host path inside the container so the
  // RUSTFLAGS value resolves. Harmless on the host-side phase 1 build.
  if (PGO_BUILD_TOOL === "cross") {
    const prior = process.env.CROSS_CONTAINER_OPTS ?? "";
    process.env.CROSS_CONTAINER_OPTS = `${prior} -v ${PGO_DATA_DIR}:${PGO_DATA_DIR}:rw`;
  }

  // ---------- Phase 1: instrumented build ----------
  log(
    `>>> [1/3] Building instrumented binary (${PGO_BUILD_TOOL}, profile=${PGO_PROFILE}` +
      `${PGO_TARGET ? `, target=${PGO_TARGET}` : ""})`,
  );
  run(PGO_BUILD_TOOL, ["build", `--profile=${PGO_PROFILE}`, ...TARGET_ARGS, "-p", "nub-cli"], {
    env: { RUSTFLAGS: `-Cprofile-generate=${PGO_PROFRAW_DIR}` },
  });

  const instrumentedBin = join(TARGET_ROOT, `${TARGET_DIR_PART}${PGO_PROFILE}`, "nub");
  if (!isExecutable(instrumentedBin)) {
    die(`instrumented binary missing at ${instrumentedBin}`);
  }

  // ---------- Phase 2: training ----------
  log(">>> [2/3] Training against nub's hot CPU paths");

  const trainDir = mkdtempSync(join(process.env.TMPDIR ?? tmpdir(), "nub-pgo-train."));
  const hermetic = hermeticStart(instrumentedBin);

  // try/finally replaces the bash `trap cleanup EXIT`: the registry is always
  // torn down and the scratch dir removed, even if training throws.
  try {
    // hermetic_start runs a warm step on first invocation against a given
    // cache dir, executing the instrumented binary against the npmjs uplink.
    // In CI that warm step fires every run (no persisted cache) and would
    // otherwise contribute non-representative profraw covering the uplink
    // path. Drop those before the real training runs land.
    clearProfraw();

    // LLVM_PROFILE_FILE only matters for the instrumented binary, so it's set
    // on process.env just around the training loop and cleared after.
    process.env.LLVM_PROFILE_FILE = PROFRAW_PATTERN;
    let lastColdDir = "";
    for (const i of [1, 2, 3]) {
      lastColdDir = installCold(instrumentedBin, trainDir, hermetic.registryUrl, i);
    }
    for (const i of [1, 2, 3]) {
      installWarm(instrumentedBin, lastColdDir, i);
    }
    transpileTrain(instrumentedBin, trainDir);
    runTrain(instrumentedBin, trainDir);
    delete process.env.LLVM_PROFILE_FILE;
  } finally {
    hermetic.stop();
    rmSync(trainDir, { recursive: true, force: true });
  }

  // Sanity check: confirm training actually wrote profraw. Without
  // LLVM_PROFILE_FILE this silently produced zero files on cross-built
  // targets — llvm-profdata then merged nothing and phase 3b failed. Fail
  // loudly here instead.
  const profrawCount = profrawFiles().length;
  if (profrawCount === 0) {
    log(`ERROR: no .profraw files written to ${PGO_PROFRAW_DIR} after training`);
    log("  Training ran but the instrumented binary did not record profile data.");
    log("  Check LLVM_PROFILE_FILE handling and (for cross) the host/container mount.");
    process.exit(1);
  }
  log(`>>> ${profrawCount} .profraw files collected`);

  // ---------- Phase 3a: merge ----------
  log(">>> [3/3] Merging profile data");
  run(llvmProfdata, ["merge", "-o", PGO_MERGED, PGO_PROFRAW_DIR]);

  // Defense in depth: a version mismatch between the rustc that instrumented
  // and the host's llvm-profdata can produce a 0-exit silent no-op. Confirm
  // the merged file exists before phase 3b reads it.
  if (!existsSync(PGO_MERGED)) {
    log(`ERROR: ${PGO_MERGED} was not produced by llvm-profdata merge`);
    log(
      "  Check that the host's llvm-profdata version matches the rustc that built the instrumented binary.",
    );
    process.exit(1);
  }
  log(`>>> merged profile written: ${statSync(PGO_MERGED).size} bytes`);

  if (process.env.NUB_PGO_SKIP_FINAL_BUILD) {
    log(">>> Skipping final optimized build (NUB_PGO_SKIP_FINAL_BUILD=1)");
    log(`>>> Profile ready at: ${PGO_MERGED}`);
    process.exit(0);
  }

  // ---------- Phase 3b: optimize ----------
  log(">>> Rebuilding with -Cprofile-use");
  // -Cllvm-args=-pgo-warn-missing-function=false: silence LLVM's per-symbol
  // "no profile data available for function …" notes during phase 3b.
  // Coverage gaps are expected — the training run can't exercise every code
  // path — and a warning per uncovered symbol drowns the build log. The
  // functions still compile, just without PGO data (the documented fallback).
  run(PGO_BUILD_TOOL, ["build", `--profile=${PGO_PROFILE}`, ...TARGET_ARGS, "-p", "nub-cli"], {
    env: {
      RUSTFLAGS: `-Cprofile-use=${PGO_MERGED} -Cllvm-args=-pgo-warn-missing-function=false`,
    },
  });

  // Phase 3b wrote to the same path as phase 1, so the file at
  // instrumentedBin is now the PGO-optimized build.
  log(`>>> PGO build complete: ${instrumentedBin}`);
  run("ls", ["-lh", instrumentedBin]);
}

try {
  main();
} finally {
  // Release the bench lock on any exit path (the bash original relied on the
  // fd closing at process exit; flock here is held by lockChild) and clear the
  // sentinel so a later run in the same pid namespace can't observe a stale one.
  if (lockChild && lockChild.exitCode === null) lockChild.kill("SIGKILL");
  rmSync(LOCK_SENTINEL, { force: true });
}
