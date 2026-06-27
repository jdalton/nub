# `nub cook` — AOT-compiled native binaries (proof of concept)

`nub cook <script>` AOT-compiles a supported TypeScript/JavaScript script to a
native host executable via [PerryTS](https://github.com/PerryTS/perry), VERIFIES
it against nub-augmented Node, caches the verified binary, and runs it. The payoff is
**cold-start latency**: a native binary starts in ~14ms versus Node's ~28ms — no
V8 warmup, no module graph, no transpile. Run once to compile + verify; every run
after is a cache hit.

cook attempts the whole TypeScript family — `.ts`, `.mts` (ESM), `.cts` (CJS),
and `.tsx` — plus plain JavaScript. PerryTS's compiler handles all of them; the
fixtures here cover `.ts`/`.mts`/`.cts`. Anything the compiler can't handle falls
back to Node (see below).

This is a proof of concept. The win we **measured** is cold start — **2.04× vs
the Node floor** (below). We did not benchmark compute throughput or memory here,
so this makes no compute/RAM claim; native AOT (no V8 heap, no JIT warmup,
smaller RSS) plausibly helps those too. The boundary on whether `nub cook`
applies is the **supported surface** — does PerryTS compile the script — not the
workload's shape. PerryTS owns the "can this compile?" decision; nub asks it,
then independently checks the compiled binary against nub's Node before trusting
it.

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

### The vendored PerryTS (the benchmark target)

The benchmark is pinned to a vendored PerryTS at [`vendor/perry`](../../vendor/perry),
a git submodule. This pins the perry the numbers were measured against and makes
"test latest" a one-line submodule bump rather than relying on whatever perry is
installed locally. cook itself stays version-agnostic — it subprocess-calls perry
and cache-keys on `perry --version`; the submodule is only the test/bench target.

Build the vendored perry (its own target dir, so it never collides with nub's):

```sh
cd vendor/perry
export CARGO_TARGET_DIR=/tmp/perry-target
cargo build --release --bin perry            # the CLI
cargo build --release -p perry-runtime-static # libperry_runtime.a (the link archive)
```

`perry compile` links against `libperry_runtime.a`, which the
`perry-runtime-static` crate emits next to the `perry` binary — build both. To
test a newer perry, `git -C vendor/perry fetch && git -C vendor/perry checkout
<commit>`, rebuild, re-run [`bench.sh`](bench.sh), and commit the new gitlink.

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

### Startup comparison

The cooked native binary vs. plain Node (the floor) vs. running the same script
through `nub <file>` (transpile + Node), measured with `hyperfine` against a
**release** build of nub and the vendored PerryTS (macOS arm64, Node 22, perry
0.5.1206 from `vendor/perry`):

```
$ hyperfine --warmup 5 -N './fib.cooked 30' 'node fib.mjs 30' 'nub fib.ts 30'
Benchmark 1: ./fib.cooked 30
  Time (mean ± σ):      13.8 ms ±   1.0 ms
Benchmark 2: node fib.mjs 30
  Time (mean ± σ):      28.1 ms ±   1.5 ms
Benchmark 3: nub fib.ts 30
  Time (mean ± σ):      56.6 ms ±   3.0 ms
Summary
  './fib.cooked 30' ran
    2.04 ± 0.19 times faster than 'node fib.mjs 30'
    4.12 ± 0.38 times faster than 'nub fib.ts 30'
```

About **2.04× faster cold start than plain Node** — the native binary skips V8
warmup, the module graph, and transpilation entirely. The Node floor is the fair,
conservative comparison; the `nub <file>` row also carries nub's transpile +
augmentation preload, so read cook-vs-Node as the headline. Reproduce with
[`bench.sh`](bench.sh), which compiles `fib.ts` with the vendored perry and runs
the three-way `hyperfine`.

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
`--debug` to dump PerryTS's raw output instead of the concise cause:

```
$ nub cook unsupported.ts --debug
nub cook: unsupported.ts uses APIs PerryTS does not compile:
  [D002] eval() cannot be compiled to native code (unsupported.ts:5)
--- perry check (raw, --debug) ---
[stdout]
{"code":"D002", … ,"message":"eval() cannot be compiled to native code …"}
{"type":"summary","success":false, … }
[stderr]
Error: Check failed with errors
--- end perry output ---
  This looks like a PerryTS gap — file an issue: https://github.com/PerryTS/perry/issues
  Running on Node instead.
the answer is 42
```

## Caveats

- **The boundary is the supported surface, not the workload shape.** The win we
  **measured** here is cold start: **2.04× vs the Node floor** (`fib.ts`, below).
  We did **not** benchmark compute throughput or memory, so this PoC makes no
  compute/RAM claim — but native AOT (no V8 heap, no JIT warmup, smaller RSS)
  plausibly helps those too, so we don't claim compute-heavy work sees "little to
  no win." The real gate on whether `nub cook` applies is **does PerryTS compile
  the script** — the supported surface — not whether the workload is
  startup-bound or compute-bound. The cold-path cost (the check, the LLVM
  compile, and the two verify runs) only amortizes from a **persistent warm
  cache**; an ephemeral environment (e.g. CI without a warm cache) pays the full
  cold cost on every run, so the startup win is for repeated local runs of a
  stable script.
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
