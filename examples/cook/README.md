# `nub cook` — AOT-compiled native binaries (proof of concept)

`nub cook <script>` AOT-compiles a supported TypeScript/JavaScript script to a
native host executable via [PerryTS](https://github.com/PerryTS/perry), VERIFIES
it against nub-augmented Node, caches the verified binary, and runs it. The payoff
is **cold-start latency**: a native binary skips V8 warmup, the module graph, and
transpilation entirely. Run once to compile + verify; every run after is a cache
hit.

This example doubles as a **map of the Node startup-optimization landscape**. The
[benchmark below](#the-startup-landscape-cold-start) puts cook next to plain Node,
a TypeScript loader (tsx), and the two startup levers Node ships natively — a
**V8 startup snapshot** and the **V8 compile cache** — across two scripts. The
short version: each lever cuts a *different* part of startup, so each wins on a
different workload, and the chart shows exactly where.

cook attempts the whole TypeScript family — `.ts`, `.mts` (ESM), `.cts` (CJS),
and `.tsx` — plus plain JavaScript. PerryTS's compiler handles all of them;
anything it can't handle falls back to Node (see below).

## The startup landscape (cold start)

Five ways to start the same script, measured cold (a fresh process per run) with
`hyperfine --warmup 5 -N`, on macOS arm64, **Node v26.3.1**, perry 0.5.1189:

![cold start across approaches](cold-start.svg)

| approach | what it removes from startup |
| --- | --- |
| **node** | nothing — the baseline (native `.mts` type-stripping) |
| **cook** (perry native AOT) | the entire V8/Node runtime — there is no Node process |
| **tsx** | nothing; *adds* a TypeScript loader hook on top of Node |
| **v8 snapshot** | parse + compile + instantiate (a pre-baked heap), but **not** the process boot |
| **v8 compile-cache** | parse + compile (a warm code cache), but **not** instantiate or the boot |

The reproducer is [`bench.sh`](bench.sh) (`./examples/cook/bench.sh`), which builds
each artifact, checks every approach prints identical output, runs the two
hyperfine groups, and regenerates the chart with [`make-chart.mjs`](make-chart.mjs).

### What the numbers say

**Trivial script** ([`fib.mts`](fib.mts) — first 30 Fibonacci numbers, no
imports, startup-dominated):

| approach | cold start | vs node |
| --- | --- | --- |
| cook | 11.6 ms ± 1.3 | **5.05× faster** |
| v8 snapshot | 36.9 ms ± 1.9 | 1.58× faster |
| v8 compile-cache | 45.4 ms ± 3.0 | 1.29× faster |
| tsx | 56.5 ms ± 2.2 | 1.03× (≈ node) |
| node | 58.3 ms ± 3.4 | — |

**Import-heavy script** ([`heavy/heavy.mts`](heavy/heavy.mts) — 480 functions
across 60 ESM modules, so parse/compile/instantiate is a real cost):

| approach | cold start | vs node |
| --- | --- | --- |
| v8 snapshot | 39.3 ms ± 2.6 | **2.12× faster** |
| v8 compile-cache | 53.5 ms ± 2.7 | 1.55× faster |
| node | 83.1 ms ± 2.9 | — |
| tsx | 85.8 ms ± 2.9 | 0.97× (≈ node) |
| cook | 203.5 ms ± 5.8 | 0.41× (slower) |

Three honest readings of this data:

1. **cook removes the runtime, so it owns the trivial case.** On `fib` there is
   almost no code to parse — the cost is the V8/Node process boot itself, and
   only cook eliminates that. It starts in ~12 ms regardless of script size, a
   flat ~5× under plain Node.
2. **Snapshot and compile-cache scale with code, not boot — so they grow on the
   heavy script.** Neither removes the Node process; they only cut
   parse/compile/instantiate. On trivial `fib` that part is small, so the win is
   modest (snapshot 1.58×, compile-cache 1.29×) — though notably **not** within
   noise: a snapshot also bakes in Node's *own* bootstrap heap, which it skips
   re-compiling on every run. On the import-heavy script the parse/compile cost
   is large, and the same two levers pull clearly ahead (snapshot **2.12×**,
   compile-cache 1.55×). This is the case they exist for: short scripts with a
   real module graph.
3. **cook is slower on the heavy script here — and that's reported, not hidden.**
   The perry build used (0.5.1189) carries a per-module initialization cost in
   the produced binary that grows with the module count; a 60-module binary pays
   ~190 ms of it at startup, which swamps the runtime-removal win. cook's
   sweet spot is small, self-contained scripts (a CLI, a tool, a single hot
   path), not a large module graph — at least with this perry build.

`tsx` is in the chart as the reproducible stand-in for a "TypeScript via a Node
loader hook" tier (the role nub's own `nub <file>` run path fills). It tracks
plain Node within noise here because the script transpiles trivially; a loader's
cost shows up on heavier transpile work, not on this fixture.

### One-time costs (not in the cold-start bars)

Every approach above amortizes a setup step that the timed runs don't pay:

- **cook** — the `perry compile` + verify (the first cook of a script). Amortized
  by nub's persistent cache; an ephemeral environment (CI with no warm cache)
  pays it every run.
- **v8 snapshot** — `node --build-snapshot` produces the `.blob` once. The blob is
  Node-version-specific.
- **v8 compile-cache** — the first run populates `NODE_COMPILE_CACHE`; every run
  after reads it. The benchmark primes it once before timing.

### Why snapshot uses an inlined CJS entry

A V8 startup snapshot is built from a **CommonJS, synchronous** entry: V8's
`mksnapshot` cannot evaluate an ESM module graph at build time (`import()` reports
`Not supported`). So the snapshot entries here
([`fib-snapshot-entry.cjs`](fib-snapshot-entry.cjs),
[`heavy/heavy-snapshot-entry.cjs`](heavy/heavy-snapshot-entry.cjs)) inline the
same logic the `.mts` files run and register it with
`v8.startupSnapshot.setDeserializeMainFunction`. The equivalence gate in
`bench.sh` confirms the snapshot prints byte-identical output to plain Node before
any timing. The heavy fixture and its snapshot entry are both regenerated from
[`heavy/gen-mods.mjs`](heavy/gen-mods.mjs) — it emits the 60 modules, the barrel,
and the inlined CJS snapshot entry from one source of truth, so the inlined
functions can't drift from the modules.

## How it works

`nub cook script.ts [--debug]`:

1. **Cache key** = `sha256(source)` + the PerryTS version + the compile flags.
   The source content is the change detector — any edit changes the key, so a
   stale binary is never served and there's no watcher.
2. **Warm paths** (decided with no perry, no Node):
   - A cached **verified binary** for the key → materialize + exec it directly.
     This is the fast, Node-free path.
   - A recorded **`not-cook-safe` verdict** for the key → run on Node directly,
     without re-checking or re-compiling. (See "verify" below.)
3. **Cold path** (fresh source):
   1. `perry check --check-deps` → if the script uses an API/package PerryTS
      can't compile, nub prints the diagnostic and falls back to Node. The gate
      reads the summary's `compilation_guaranteed` field (codegen-aware) when
      present, falling back to `success` (a frontend-only parse/HIR/deps check)
      for an older perry that doesn't emit it. Gating on the stronger field
      trims false-positives before the compile step; verify-before-trust still
      backstops the decision. When `perry check` evaluates **zero files** — its
      file discovery doesn't glob every extension the compiler accepts, so an
      undiscovered one (`.mts`, `.cts`, `.tsx`, `.jsx`, `.js`) reports zero
      files — that is *indeterminate*, not a "no": nub proceeds to compile (which
      **does** handle those extensions) and lets verify-before-trust decide,
      rather than bailing to Node on a check that never looked at the file.
   2. `perry compile -o <tmp>` → asserts a clean exit; a non-clean compile falls
      back to Node.
   3. **Verify-before-trust** (the differential): nub runs the freshly compiled
      binary AND nub-augmented Node on the **same args**, and compares **stdout +
      exit code** (stderr is informational and not compared — AOT and Node
      warnings legitimately differ).
      - **Equivalent** → the binary is stored in the cache as **verified** and
        its output is used.
      - **Diverged** → nub **discards** the binary, records a `not-cook-safe`
        verdict for that source (so it isn't re-attempted until the source
        changes), prints what diverged, and runs on Node.

A divergence does **not** necessarily mean PerryTS miscompiled the script. The
verify step assumes deterministic output, so a correct-but-**nondeterministic**
script — one that prints `Date.now()`, `Math.random()`, a timestamp, a PID, or
relies on unordered iteration — will legitimately produce different bytes on the
two verify runs and be recorded `not-cook-safe`. nub frames that case as
"output differed (expected for nondeterministic scripts)" and does **not** show
the PerryTS issue link for it; the issue link is reserved for `perry check` /
`perry compile` failures, which are genuinely the compiler's. A future
`--no-verify` bless path covers trusting such scripts deliberately.

### What "verified" means (the verify-once / arg-domain boundary)

The cache key is `sha256(source) + perry_version + compile_flags` — **the argv
is not part of it**, and verify runs **once**, on the **first** invocation's
args/stdin. Every run after that is a warm cache hit: nub execs the cached
binary for **any** later argv/stdin **without re-verifying**. So "verified"
means **smoke-tested on one input — not proven equivalent across the input
domain**. A compiler bug that only manifests on an input the first run didn't
exercise would slip past the gate.

The natural future strengthening is to key the verdict **per arg-set** — verify
each distinct argv once and cache that verdict — which the verdict cache already
supports without a new mechanism.

### The verify oracle is nub-augmented Node, not plain Node

The reference nub diffs the cooked binary against is **nub-augmented** Node — the
same transpile + preload pipeline `nub <file>` uses, not a bare `node`. So the
equivalence cook checks is "the PerryTS binary behaves like **nub's** Node," not
"like **plain** Node." This is deliberate: nub's augmentation layer is part of
the runtime contract a cooked script is replacing, so it belongs in the oracle.

The cache is a Rust [`cacache`][cacache] store under nub's cache dir
(`~/.cache/nub/cook`). The cache read/write is pure Rust — **no Node, no JS**
in the cache path. **Only a verified binary is ever cached or executed** — nub
never trusts a freshly compiled binary on perry's word alone.

[cacache]: https://crates.io/crates/cacache

## Requirements

- [PerryTS](https://github.com/PerryTS/perry) on `PATH` (or `PERRY_BIN=/path/to/perry`).
- A working clang/LLVM toolchain (PerryTS links a native binary).

Without either, `nub cook` prints a clear note and runs on Node instead.

To reproduce the benchmark you also need [`hyperfine`](https://github.com/sharkdp/hyperfine),
a Node ≥ 22.18 (native `.mts` type-stripping; the V8 compile cache needs ≥ 22.8),
and — for the `tsx` row — a `tsx` install reachable from the example (`npm i tsx`
in `examples/cook`, or set `TSX_LOADER`). The tsx row is skipped if absent.
[`make-chart.mjs`](make-chart.mjs) rasterizes the `.png` via `rsvg-convert` or
macOS `qlmanage` if present; the `.svg` is the source of truth and renders on
GitHub directly.

## Demo

### Supported script — compiled, verified, cached

```
$ nub cook fib.ts 10
0 1 1 2 3 5 8 13 21 34
```

The first run compiles, verifies against Node, and caches. Output is identical to
Node:

```
$ node fib.ts 10        # (via nub's normal run path)
0 1 1 2 3 5 8 13 21 34
```

Every subsequent run is a cache hit — it execs the verified binary directly, with
no perry and no Node involved.

### Unsupported script — graceful Node fallback

`unsupported.ts` uses `eval()`, which can't be AOT-compiled:

```
$ nub cook unsupported.ts
nub cook: unsupported.ts uses APIs PerryTS does not compile:
  [D002] eval() cannot be compiled to native code (unsupported.ts:5)
  This looks like a PerryTS gap — file an issue: https://github.com/PerryTS/perry/issues
  Running on Node instead.
the answer is 42
```

The script still runs — the AOT path is an optimization, never a gate. Pass
`--debug` to dump PerryTS's raw output instead of the concise cause.

## Caveats

- **The boundary is the supported surface, not the workload shape.** The gate on
  whether `nub cook` applies is **does PerryTS compile the script** — not whether
  the workload is startup-bound or compute-bound. The cold-path cost (the check,
  the LLVM compile, and the two verify runs) only amortizes from a **persistent
  warm cache**; an ephemeral environment (e.g. CI without a warm cache) pays the
  full cold cost on every run.
- **cook's win is workload-shaped (see the benchmark).** It is largest on small,
  self-contained scripts where the V8/Node boot dominates. A large module graph
  can erase the win — and, with the perry build measured here, invert it (the
  import-heavy row above). Measure your own script; don't assume cook is always
  faster.
- **First-run double-run (side-effect caveat).** Verify-before-trust runs the
  script **twice** on its first cook — once as the cooked binary and once on
  Node — to confirm they behave identically. cook is therefore intended for
  **pure / CLI / compute** scripts; running an effectful script (one that writes
  files, sends network requests, etc.) through `nub cook` for the first time
  will perform those side effects twice. A `--no-verify` / bless path for trusted
  effectful scripts is a future refinement, not this PoC. After the first
  (verified) run, the warm path execs the cached binary once, like any program.
- **Supported surface only.** PerryTS decides what compiles. Anything it rejects
  — or any binary whose behavior diverges from Node in the verify step — falls
  back to Node.
- **First run pays the compile + verify cost.** Both are amortized by the cache;
  the first cook of a script is slower than just running it on Node.
- **Depends on PerryTS + a native toolchain.** Both must be present; otherwise
  nub runs the script on Node.
- **Explicit opt-in.** `nub cook` is its own verb — `nub <file>` and `nub run`
  never AOT-compile. cook is how you deliberately "seal in" the fast start for
  a specific script.
