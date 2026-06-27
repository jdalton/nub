//! `nub cook <script>` — a proof-of-concept AOT path.
//!
//! Compile-once / run-fast for the *supported surface only*. The verb asks
//! PerryTS whether a script is compilable (`perry check --check-deps`), and if
//! so AOT-compiles it to a native host executable (`perry compile`), VERIFIES
//! the binary against Node (verify-before-trust), caches the verified binary
//! blob keyed by `sha256(source) + perry-version + flags`, and execs it. The
//! cache is a Rust `cacache` store under nub's cache dir — reads and writes
//! happen in-process, with NO Node in the cache path. A second run of an
//! unchanged script is a cache hit and skips the compile + verify entirely.
//!
//! The full decision flow:
//!
//! ```text
//! nub cook <file> [--debug]:
//!   key = sha256(source) + perry_version + compile_flags
//!   cacache: VERIFIED binary for key?         → exec it (fast path, Node-free)
//!   cacache: "not-cook-safe" verdict?        → run on Node (skip re-attempt)
//!   else (fresh source):
//!     perry check --check-deps --format json    → not compilable → error-path → Node
//!       (gate on summary.compilation_guaranteed; fall back to summary.success;
//!        check evaluated ZERO files [an extension check's discovery doesn't
//!        glob, e.g. .tsx/.jsx/.js] is INDETERMINATE → proceed to compile,
//!        NOT a Node bail)
//!     perry compile -o <tmp>  (clean exit)      → non-clean → error-path → Node
//!     VERIFY-BEFORE-TRUST (differential):
//!       run the cooked binary AND Node with the SAME args; compare stdout+exit
//!         equivalent → store VERIFIED binary  → exec it
//!         diverge    → error-path; DISCARD binary; cache "not-cook-safe"
//! ```
//!
//! Three design boundaries make this safe to ship as a PoC verb:
//!
//!   * PerryTS OWNS the compilability decision. nub never guesses which APIs
//!     compile; it asks `perry check` and trusts its verdict — the summary's
//!     `compilation_guaranteed` field (codegen-aware) when present, falling back
//!     to `success` (frontend-only) otherwise. A negative verdict (an
//!     unsupported package/API) is NOT an error — nub prints a concise cause and
//!     falls back to running the script on nub's normal Node path, so the script
//!     still runs. Same for a missing `perry`/toolchain. One nuance: `perry
//!     check`'s file discovery doesn't glob every extension its compiler accepts
//!     — an input it doesn't discover (measured against perry 0.5.1206: only
//!     `.ts` is discovered; `.mts`/`.cts`/`.tsx`/`.jsx`/`.js` are NOT) makes check
//!     evaluate ZERO files. That is INDETERMINATE, not a "no": nub then proceeds
//!     to `perry compile` (which DOES handle the whole family) and lets
//!     verify-before-trust decide — so cook attempts every extension perry's
//!     compiler accepts, without nub itself guessing which compile. A real
//!     compile failure still routes to Node.
//!
//!   * VERIFY-BEFORE-TRUST. nub never trusts a freshly compiled binary on
//!     perry's word alone: it runs the binary AND Node on the same args and
//!     only caches/execs the binary if their stdout + exit code match. A
//!     divergence discards the binary and records a "not-cook-safe" verdict
//!     for that source so it's never re-attempted until the source changes.
//!
//!   * VERIFY-ONCE / ARG-DOMAIN BOUNDARY. The cache key is
//!     `sha256(source) + perry_version + flags` — the ARGV is NOT part of it,
//!     and verify runs exactly once, on the FIRST invocation's argv/stdin. Every
//!     later run is a warm cache hit that execs the cached binary for ANY argv /
//!     stdin WITHOUT re-verifying. So "verified" means SMOKE-TESTED on one input,
//!     NOT proven equivalent across the whole input domain: a compiler bug that
//!     only manifests on an input the first run didn't exercise would slip past
//!     the gate. This is an accepted PoC limitation. The natural future
//!     strengthening is to key the verdict per ARG-SET (verify each distinct
//!     argv once, cache its verdict) — the verdict cache already supports a
//!     finer key, so this generalizes without a new mechanism.
//!
//!   * The MEASURED win is COLD-START / CLI latency: a cooked binary starts in
//!     ~14ms vs Node's ~28ms (no V8 warmup, no module graph, no transpile) —
//!     ~2.04× vs the Node floor. We have NOT benchmarked compute throughput or
//!     RAM, so this makes no compute/memory claim; native AOT (no V8 heap, no
//!     JIT warmup, smaller RSS) plausibly helps those too. The real boundary on
//!     whether cook applies is the SUPPORTED SURFACE — does PerryTS compile the
//!     script — not the workload's shape. The cold-path cost (check + LLVM
//!     compile + two verify runs) only amortizes from a persistent warm cache.
//!
//! Side-effect caveat: the first cook of a script DOUBLE-RUNS it once (the
//! cooked binary + Node) to verify behavioral equivalence. cook is therefore
//! intended for pure / CLI / compute scripts; a `--no-verify` bless path for
//! effectful scripts is a future refinement, not this PoC.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use sha2::{Digest, Sha256};

/// Compile flags passed to `perry compile`. Captured in the cache key so a
/// change in how we compile invalidates a stale binary. Empty today (the PoC
/// compiles for the native host with perry's defaults); kept as a slice so the
/// key stays sound the moment a flag is added.
const COMPILE_FLAGS: &[&str] = &[];

/// Hard wall-clock bound on `perry --version` — a trivial call that should
/// return in milliseconds. It sits on the cold path of every run (it keys the
/// cache), so a wedged version probe must not hang cook before the gate is even
/// reached. On timeout → `None` → Node fallback.
const PERRY_VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Internal test/diagnostic seam (NOT a documented user knob): when set to a
/// number of milliseconds, EVERY perry-subprocess bound is clamped to that
/// value. This lets the e2e + unit tests force a wedged perry stub to trip the
/// timeout in milliseconds rather than waiting out the real multi-second bounds,
/// exercising the genuine timeout → Node-fallback wiring. Absent in normal use,
/// so the real constants above apply. `__NUB`-prefixed = internal plumbing.
fn timeout_for(default: Duration) -> Duration {
    if let Some(ms) = std::env::var("__NUB_COOK_PERRY_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    default
}

/// Hard wall-clock bound on `perry check`. The gate is meant to be a fast
/// "can this compile?" question; a perry that wedges on it (a parser hang, a
/// dependency-resolution loop) must NEVER hang `nub cook` — on timeout the gate
/// fails CLOSED to "perry unavailable", which routes to Node. Generous enough
/// that a healthy check (tens of ms) never trips it, tight enough that a wedge
/// is caught in seconds rather than the ~17.5-minute hang that motivated this.
const PERRY_CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Hard wall-clock bound on `perry compile`. A real native AOT compile + link
/// is far heavier than the check (LLVM codegen, linking a multi-MB binary —
/// measured tens of seconds cold on first build of the stdlib archive), so this
/// bound is much longer than the check's. On timeout the compile is treated as
/// a (non-attributable) failure → Node fallback, same as any other compile
/// failure. The point is only to bound a genuine WEDGE, not to race a slow but
/// progressing compile.
const PERRY_COMPILE_TIMEOUT: Duration = Duration::from_secs(120);

/// Grace window for the pipe-reader threads to drain after the child LEADER has
/// exited (the `Completed` path). A child whose descendants all released the
/// pipes drains in well under this — the readers are already finished and the
/// first poll returns. The window only matters if a backgrounded grandchild
/// still holds a pipe open; after it elapses we fell the group to force EOF.
/// Short, because by this point the leader is already gone — we're only waiting
/// out a stray descendant, not a legitimately-slow compile.
const READER_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// The marker value stored under the verdict key when a script compiled but
/// FAILED the verify-before-trust differential (or wasn't compilable). Its mere
/// presence is the signal; the bytes are a human-readable breadcrumb.
const VERDICT_NOT_SAFE: &[u8] = b"not-cook-safe";

/// Where to file a perry-attributable gap.
const PERRY_ISSUES_URL: &str = "https://github.com/PerryTS/perry/issues";

/// Inputs to [`run`]. Bundled so the CLI dispatch passes one value and the two
/// run-on-Node closures (inherited fallback + captured verify probe) ride
/// alongside the script/flags rather than as a long positional list.
pub struct CookArgs<'a, F, G>
where
    F: FnOnce() -> Result<i32>,
    G: FnOnce() -> Result<(Vec<u8>, i32)>,
{
    /// Path to the script to compile and run.
    pub script: &'a str,
    /// Args forwarded to the compiled binary / script.
    pub forwarded_args: &'a [String],
    /// `--node` / NODE_COMPAT: skip the AOT path entirely, run plain Node.
    pub compat_mode: bool,
    /// `--debug`: dump perry's RAW stdout/stderr on a failure (default: a
    /// concise pretty-printed cause).
    pub debug: bool,
    /// Inherited-stdio fallback: actually run the script on nub's Node path.
    /// Used on every non-fast-path outcome so the script always runs.
    pub run_on_node: F,
    /// Captured Node run for the verify-before-trust differential — same Node
    /// path as `run_on_node`, but returns `(stdout, exit_code)` instead of
    /// inheriting the terminal.
    pub run_on_node_captured: G,
}

/// Outcome of the compilability gate.
enum Gate {
    /// `perry check` said the script compiles. Carries the resolved perry
    /// version string, which is part of the cache key.
    Compilable { perry_version: String },
    /// `perry check` said the script does NOT compile. Carries perry's
    /// human-readable diagnostics (one per line) for the pretty cause, plus the
    /// raw `perry check` stdout/stderr for the `--debug` dump.
    Unsupported {
        diagnostics: Vec<String>,
        raw: RawPerryOutput,
    },
    /// `perry check` ran cleanly but evaluated ZERO files — it did not actually
    /// assess this input. perry's `check` file-discovery doesn't glob every
    /// extension its compiler accepts: an extension it doesn't discover
    /// (measured against perry 0.5.1206: only `.ts` is discovered and yields a
    /// real summary, while `.mts`, `.cts`, `.tsx`, `.jsx`, `.js` are NOT) yields
    /// `{"errors":0,"files":0,"success":true,…}` with no codegen-aware summary
    /// — NOT a "does not compile" verdict, just "check didn't look at it."
    /// `perry compile`, by contrast, handles the whole family. So an
    /// indeterminate check is NOT a reason to bail to Node: we PROCEED to
    /// compile-and-verify and let verify-before-trust (the real arbiter) decide.
    /// This keeps cook extension-agnostic — it attempts perry for any input
    /// perry's compiler can take — without nub guessing compilability itself.
    /// Carries the resolved perry version (cache key).
    Indeterminate { perry_version: String },
    /// perry (or its toolchain) is unavailable — fall back to Node with a
    /// one-line note. Carries the note.
    Unavailable { note: String },
}

/// A cook failure, carrying the human cause and the raw perry output for
/// `--debug`. Every failure routes through [`fail`] → pretty-print or raw dump →
/// Node fallback, so the script always runs.
struct CookFailure {
    /// One-line "what went wrong" for the default (non-debug) message.
    cause: String,
    /// Whether this failure is attributable to PerryTS (an unsupported API or a
    /// compile error) — i.e. worth filing upstream, so the issue link is shown.
    /// A nub-side / environment failure (cache I/O, missing toolchain note) is
    /// NOT, and neither is a verify DIVERGENCE: a correct-but-nondeterministic
    /// script (time/randomness) legitimately differs across the two verify runs,
    /// so blaming the compiler would misattribute the cause — the issue link is
    /// suppressed there.
    perry_attributable: bool,
    /// Raw perry stdout/stderr captured during the failing step, dumped verbatim
    /// under `--debug`. `None` when there's no raw perry output (e.g. a pure
    /// nub-side cache error).
    raw: Option<RawPerryOutput>,
}

/// Raw perry output for a `--debug` dump.
struct RawPerryOutput {
    step: &'static str,
    stdout: String,
    stderr: String,
}

/// Run `nub cook <script> [args…]`.
pub fn run<F, G>(args: CookArgs<'_, F, G>) -> Result<i32>
where
    F: FnOnce() -> Result<i32>,
    G: FnOnce() -> Result<(Vec<u8>, i32)>,
{
    let CookArgs {
        script,
        forwarded_args,
        compat_mode,
        debug,
        run_on_node,
        run_on_node_captured,
    } = args;

    let script_path = Path::new(script);
    if !script_path.exists() {
        anyhow::bail!("nub cook: no such file: {script}");
    }

    // `--node` / NODE_COMPAT means "no augmentation" — and cook IS an
    // augmentation (an alternate runtime). Honor the escape hatch: skip the
    // whole AOT path and run plain Node.
    if compat_mode {
        return run_on_node();
    }

    let source = std::fs::read(script_path)
        .map_err(|e| anyhow::anyhow!("nub cook: cannot read {script}: {e}"))?;

    let perry = match locate_perry() {
        Some(p) => p,
        None => {
            eprintln!(
                "nub cook: PerryTS not found on PATH.\n\
                 \x20\x20cook AOT-compiles supported scripts to a fast-starting native binary.\n\
                 \x20\x20Install PerryTS (https://github.com/PerryTS/perry) to enable it.\n\
                 \x20\x20Running on Node instead."
            );
            return run_on_node();
        }
    };

    // The cache key needs the perry version, so resolve it first. A failure
    // here means perry is unusable → fall back.
    let perry_version = match perry_version(&perry) {
        Some(v) => v,
        None => {
            eprintln!("nub cook: could not determine PerryTS version. Running on Node instead.");
            return run_on_node();
        }
    };
    let key = cache_key(&source, &perry_version);

    // WARM PATHS, keyed by the (source, perry-version, flags) content hash.
    // Both are decided WITHOUT spawning perry or Node — the key IS the change
    // detector, so any edit to the source re-runs the cold path.
    if let Ok(cache) = cook_cache_dir() {
        // 1. A recorded "not-cook-safe" verdict: this exact source already
        //    failed the gate or the verify differential. Don't re-check /
        //    re-compile / re-verify it every run — go straight to Node.
        if has_verdict(&cache, &key) {
            tracing::debug!(%key, "cook verdict=not-safe (warm); running on Node");
            return run_on_node();
        }
        // 2. A VERIFIED binary: it already passed `perry check`, compiled
        //    cleanly, and matched Node's stdout+exit when it was first built.
        //    Materialize + exec directly — no perry, no Node.
        if cacache::metadata_sync(&cache, &key)
            .ok()
            .flatten()
            .is_some()
        {
            match run_cached(&cache, &key, forwarded_args) {
                Ok(code) => return Ok(code),
                Err(e) => {
                    // Cached entry unusable (corrupt/truncated) → fall through
                    // to the cold path, which recompiles + re-verifies.
                    tracing::debug!(%key, "cook warm path failed ({e}); recompiling");
                }
            }
        }
    }

    // COLD PATH: no verdict, no verified binary. Ask perry whether the script
    // compiles.
    match gate(&perry, script_path, &perry_version) {
        Gate::Unavailable { note } => fail(
            CookFailure {
                cause: note,
                perry_attributable: false,
                raw: None,
            },
            debug,
            run_on_node,
        ),
        Gate::Unsupported { diagnostics, raw } => fail(
            CookFailure {
                cause: format!(
                    "{script} uses APIs PerryTS does not compile:\n{}",
                    indent(&diagnostics)
                ),
                perry_attributable: true,
                raw: Some(raw),
            },
            debug,
            run_on_node,
        ),
        // Both Compilable (check said yes) and Indeterminate (check evaluated
        // zero files — an extension it doesn't discover, e.g. a `.tsx`/`.jsx`)
        // proceed to compile-and-verify. The difference is only WHY we're
        // compiling; the path is identical, and verify-before-trust is the real
        // arbiter for both. Folding them keeps the cook-attempts-the-whole-TS-
        // family behavior in one place.
        Gate::Compilable {
            perry_version: gate_version,
        }
        | Gate::Indeterminate {
            perry_version: gate_version,
        } => {
            // Defensive: the version that gated the script should equal the one
            // that keyed the cache (same binary in the same invocation). If a
            // perry self-update raced between the two reads, re-key so the blob
            // is stored under the version that actually compiled it.
            let key = if gate_version == perry_version {
                key
            } else {
                cache_key(&source, &gate_version)
            };
            compile_verify_run(
                &perry,
                script_path,
                &key,
                forwarded_args,
                debug,
                run_on_node,
                run_on_node_captured,
            )
        }
    }
}

/// Cold-path core: compile → verify-before-trust → store + exec, or error-path
/// + verdict + Node fallback on any failure.
fn compile_verify_run<F, G>(
    perry: &Path,
    script: &Path,
    key: &str,
    args: &[String],
    debug: bool,
    run_on_node: F,
    run_on_node_captured: G,
) -> Result<i32>
where
    F: FnOnce() -> Result<i32>,
    G: FnOnce() -> Result<(Vec<u8>, i32)>,
{
    let cache = match cook_cache_dir() {
        Ok(c) => c,
        Err(e) => {
            return fail(
                CookFailure {
                    cause: format!("could not resolve nub cache dir: {e}"),
                    perry_attributable: false,
                    raw: None,
                },
                debug,
                run_on_node,
            );
        }
    };
    if let Err(e) = std::fs::create_dir_all(&cache) {
        return fail(
            CookFailure {
                cause: format!("could not create cook cache dir: {e}"),
                perry_attributable: false,
                raw: None,
            },
            debug,
            run_on_node,
        );
    }

    // 1. Compile (assert a clean exit).
    let binary_bytes = match compile(perry, script) {
        Ok(bytes) => bytes,
        Err(CompileError {
            cause,
            attributable,
            raw,
        }) => {
            return fail(
                CookFailure {
                    cause: format!("could not compile {}: {cause}", script.display()),
                    perry_attributable: attributable,
                    raw,
                },
                debug,
                run_on_node,
            );
        }
    };

    // Materialize the candidate binary to an executable path so we can run it
    // for the verify probe. (Reused as the exec path if it passes.)
    let exec_path = match materialize(&cache, key, &binary_bytes) {
        Ok(p) => p,
        Err(e) => {
            return fail(
                CookFailure {
                    cause: format!("could not place cooked binary: {e}"),
                    perry_attributable: false,
                    raw: None,
                },
                debug,
                run_on_node,
            );
        }
    };

    // 2. VERIFY-BEFORE-TRUST: run the cooked binary AND Node with the same
    //    args, captured, and compare stdout + exit code. stderr is informational
    //    (warnings legitimately differ between AOT and Node) and NOT compared.
    //    This runs ONCE, on THIS invocation's argv (the arg-domain boundary in
    //    the module doc): a later warm hit execs the cached binary for any argv
    //    without re-verifying, so equivalence is smoke-tested on one input, not
    //    proven across the input domain.
    //
    //    KNOWN LIMITATION: these two verify runs are NOT wall-clock bounded (the
    //    timeout guard wraps only the perry subprocesses — the incident it fixed
    //    was a wedged `perry check`). A cooked binary that loops forever, or a
    //    hanging Node reference, would hang cook here. Acceptable for this PoC
    //    verb; bounding them is the natural follow-up if it ever bites.
    let cooked = match capture_binary(&exec_path, args) {
        Ok(o) => o,
        Err(e) => {
            discard_binary(&exec_path);
            return fail(
                CookFailure {
                    cause: format!("cooked binary failed to run during verify: {e}"),
                    perry_attributable: true,
                    raw: None,
                },
                debug,
                run_on_node,
            );
        }
    };
    let (node_stdout, node_code) = match run_on_node_captured() {
        Ok(pair) => pair,
        Err(e) => {
            // We could not get a Node reference to verify against — don't trust
            // the binary, but this is a nub/Node-side failure, not perry's.
            discard_binary(&exec_path);
            return fail(
                CookFailure {
                    cause: format!("could not run Node reference for verify: {e}"),
                    perry_attributable: false,
                    raw: None,
                },
                debug,
                run_on_node,
            );
        }
    };

    if let VerifyOutcome::Diverged = verify_outcome(&cooked, &node_stdout, node_code) {
        // DIVERGENCE: the cooked binary does NOT behave like Node. Discard it,
        // record a not-cook-safe verdict so this source is never re-attempted
        // until it changes, then run on Node.
        discard_binary(&exec_path);
        record_verdict(&cache, key);
        return fail(
            divergence_failure(script, &cooked, &node_stdout, node_code),
            debug,
            run_on_node,
        );
    }

    // 3. VERIFIED. Store the binary blob under the key (only verified binaries
    //    are ever cached) and return the verify run's exit code — the script has
    //    ALREADY produced its output via the captured cooked run above, so we
    //    do NOT exec it again (avoids a third run / double side-effects this
    //    invocation). Replay the captured stdout to the terminal.
    if let Err(e) = cacache::write_sync(&cache, key, &binary_bytes) {
        // Caching failed but the binary verified — still surface its output.
        tracing::debug!(%key, "cook verified but cache write failed ({e})");
    }
    use std::io::Write;
    let _ = std::io::stdout().write_all(&cooked.stdout);
    let _ = std::io::stdout().flush();
    Ok(cooked.code)
}

/// The verify-before-trust verdict: does the cooked binary behave like Node?
enum VerifyOutcome {
    /// stdout AND exit code match — the binary is trustworthy for this input.
    Equivalent,
    /// stdout or exit code differs — do not trust the binary.
    Diverged,
}

/// The verify-before-trust comparison, factored out so the equivalence rule is
/// pinned by a test instead of living only on the perry-dependent path. The
/// binary is trusted IFF its stdout AND exit code match Node's; stderr is
/// informational (AOT/Node warnings legitimately differ) and is NOT compared.
fn verify_outcome(cooked: &Captured, node_stdout: &[u8], node_code: i32) -> VerifyOutcome {
    if cooked.stdout == node_stdout && cooked.code == node_code {
        VerifyOutcome::Equivalent
    } else {
        VerifyOutcome::Diverged
    }
}

/// Build the [`CookFailure`] for a verify DIVERGENCE. Factored out so the
/// `perry_attributable: false` classification is pinned by a test.
///
/// A divergence is NOT necessarily a compiler defect: a correct-but-
/// nondeterministic script (`Date.now()`, `Math.random()`, time/PID, unordered
/// iteration) produces different bytes on the two verify runs and would diverge
/// even with a perfect compiler. So this case is `perry_attributable: false` —
/// the PerryTS-issue link is reserved for `perry check`/`perry compile`
/// FAILURES, which are genuinely the compiler's. The cause names nondeterminism
/// as the likely benign explanation.
fn divergence_failure(
    script: &Path,
    cooked: &Captured,
    node_stdout: &[u8],
    node_code: i32,
) -> CookFailure {
    let cause = format!(
        "cooked binary's output differed from Node's for {} — not trusting it.\n\
         \x20\x20exit: cooked={} node={}\n\
         \x20\x20stdout: {}\n\
         \x20\x20This is expected for nondeterministic scripts (time, randomness, PID,\n\
         \x20\x20unordered iteration) and is not necessarily a compiler bug.",
        script.display(),
        cooked.code,
        node_code,
        describe_stdout_diff(&cooked.stdout, node_stdout),
    );
    CookFailure {
        cause,
        perry_attributable: false,
        raw: None,
    }
}

/// THE error helper. On ANY cook failure: pretty-print a concise cause (or,
/// under `--debug`, dump perry's raw stdout/stderr), append the PerryTS issue
/// link IFF the failure is perry-attributable, then run the script on Node so it
/// always executes.
fn fail<F>(failure: CookFailure, debug: bool, run_on_node: F) -> Result<i32>
where
    F: FnOnce() -> Result<i32>,
{
    eprintln!("nub cook: {}", failure.cause);

    if debug {
        if let Some(raw) = &failure.raw {
            eprintln!("--- perry {} (raw, --debug) ---", raw.step);
            if !raw.stdout.trim().is_empty() {
                eprintln!("[stdout]\n{}", raw.stdout.trim_end());
            }
            if !raw.stderr.trim().is_empty() {
                eprintln!("[stderr]\n{}", raw.stderr.trim_end());
            }
            eprintln!("--- end perry output ---");
        } else {
            eprintln!("  (--debug: no raw PerryTS output captured for this step)");
        }
    }

    if let Some(line) = issue_link_line(failure.perry_attributable) {
        eprintln!("{line}");
    }
    eprintln!("  Running on Node instead.");
    run_on_node()
}

/// The "file a PerryTS issue" line `fail()` prints, IFF the failure is perry-
/// attributable. A divergence (`perry_attributable: false`, see
/// [`divergence_failure`]) returns `None`, so the link is suppressed; a real
/// `perry check`/`perry compile` failure returns the link. Factored out so the
/// suppression contract is pinned by a test without capturing `fail()`'s stderr.
fn issue_link_line(perry_attributable: bool) -> Option<String> {
    perry_attributable
        .then(|| format!("  This looks like a PerryTS gap — file an issue: {PERRY_ISSUES_URL}"))
}

/// Indent each diagnostic line by two spaces for the pretty cause block.
fn indent(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A short, bounded description of how two stdout buffers differ, for the
/// pretty (non-debug) divergence message. Avoids dumping potentially-huge
/// output; `--debug` is for the full picture.
fn describe_stdout_diff(cooked: &[u8], node: &[u8]) -> String {
    if cooked.len() != node.len() {
        return format!(
            "differ in length ({} vs {} bytes)",
            cooked.len(),
            node.len()
        );
    }
    "differ in content (same length)".to_string()
}

/// Resolve the `perry` CLI: an explicit `PERRY_BIN` override (used by the
/// e2e/demo against a dev build) wins; otherwise the first `perry` on PATH.
fn locate_perry() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PERRY_BIN") {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Some(p);
        }
        return None;
    }
    which_in_path("perry")
}

/// Minimal PATH lookup (no extra dep): scan `$PATH` for an executable named
/// `name`. Returns the first hit.
fn which_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The result of running a child process under a wall-clock bound — either it
/// finished within the deadline (carrying its captured output), or it was
/// killed because it ran past the deadline.
enum Timed {
    /// The process completed within the timeout. Mirrors `std::process::Output`.
    Completed {
        status: std::process::ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// The process ran past `timeout` and was killed + reaped. The `step` names
    /// which perry invocation wedged, for the diagnostic.
    TimedOut { step: &'static str },
}

/// Run `cmd` to completion OR kill it once `timeout` elapses — the core
/// no-hang guarantee for cook's perry subprocesses. Pure `std` + `libc` (a
/// workspace dep already), no `wait-timeout` crate, fully portable.
///
/// WHY this shape (and not `wait-timeout` or a single `wait()` thread): the
/// child's stdout/stderr are CAPTURED (perry's NDJSON / diagnostics), so its
/// pipes must be drained concurrently or a chatty child would deadlock against
/// a full pipe buffer before we ever reach the timeout. So we spawn one reader
/// thread per pipe (each runs `read_to_end`) and keep the `Child` handle in this
/// thread to poll `try_wait` against a deadline. The poll interval is short
/// relative to both timeouts, so a healthy fast child is detected on the next
/// tick and does not wait out a slice.
///
/// THE TWO no-hang hazards this guards (both found empirically — a naive
/// kill-the-child-then-join hung the full outer timeout):
///
///   1. **A grandchild inheriting the pipes.** perry is typically `sh -c …` →
///      a real perry/clang/ld child, so the process we spawn is NOT the leaf.
///      If we kill only the direct child, an orphaned grandchild keeps the
///      stdout/stderr write-ends OPEN, the pipes never reach EOF, and a reader
///      thread blocked in `read_to_end` would hang FOREVER. Fix: spawn the
///      child in its OWN process group (`process_group(0)`) and, on timeout,
///      SIGKILL the WHOLE GROUP (`kill(-pgid)`) so every descendant dies and
///      the pipes close.
///   2. **Joining a reader thread on the timeout path.** Even with the group
///      kill, we do NOT block-join the reader threads on timeout — we DETACH
///      them. The whole point of the timeout is to return at the deadline no
///      matter what; a join is one more place a wedged descendant could pin us.
///      A detached reader thread either finishes (pipe closed by the group
///      kill) or is a harmless idle thread that exits with the process; the
///      timeout path is what must never block, and it doesn't.
///
/// `step` is the perry phase name ("check"/"compile"/"--version") echoed back in
/// a `TimedOut` so the caller's diagnostic can name what wedged.
fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    step: &'static str,
) -> std::io::Result<Timed> {
    use std::io::Read;

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Own process group so a timeout can SIGKILL the whole subtree (perry's sh →
    // perry → clang/ld children), not just the leader. See hazard (1) above.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    let child_pid = child.id();

    // Drain both pipes on their own threads so a child that writes more than a
    // pipe buffer's worth before exiting can't deadlock us before the deadline.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = std::time::Instant::now() + timeout;
    // Poll cadence: small relative to either bound, so a fast child returns
    // promptly and the worst-case overshoot past the deadline is one tick.
    let tick = Duration::from_millis(25);
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(tick);
            }
        }
    };

    match status {
        Some(status) => {
            // The LEADER has exited, but that does NOT guarantee the pipes are at
            // EOF: a backgrounded GRANDCHILD that inherited the stdout/stderr
            // write-ends (perry's sh → perry → clang/ld tree, where the wrapper
            // can exit 0 while a helper lives on) keeps them open, and a reader
            // thread's `read_to_end` would block on it FOREVER — the same hazard
            // (1) the timeout path guards, present on the clean-exit path too. So
            // we do NOT blindly block-join: we wait for the readers with a short
            // GRACE bound (the healthy case — no straggler — finishes on the
            // first tick), and if a straggler pins the pipes past the grace
            // window we SIGKILL the whole group (closing the pipes) and detach,
            // so this returns regardless. The pgid is still the exited leader's
            // pid; the group-kill sweeps any survivor.
            let (stdout, stderr) =
                join_readers_bounded(out_thread, err_thread, child_pid, READER_DRAIN_GRACE);
            Ok(Timed::Completed {
                status,
                stdout,
                stderr,
            })
        }
        None => {
            // Past the deadline and still running → SIGKILL the whole process
            // group (hazard 1), reap the leader (no zombie), and DETACH the
            // reader threads (hazard 2) so this returns at the deadline
            // regardless of any descendant. Order: group-kill first (closes the
            // pipes), then reap, then drop the join handles.
            kill_process_group(child_pid);
            let _ = child.kill(); // belt-and-suspenders: also kill the leader directly
            let _ = child.wait(); // reap the leader so it's not a zombie
            drop(out_thread);
            drop(err_thread);
            Ok(Timed::TimedOut { step })
        }
    }
}

/// Join the two pipe-reader threads with a GRACE bound, returning their drained
/// `(stdout, stderr)`. In the healthy case (the child and all its descendants
/// have released the pipe write-ends) both threads are already finished and this
/// returns on the first poll. If a straggling descendant still holds a pipe open
/// past `grace`, we SIGKILL the whole process group of `leader_pid` (closing the
/// write-ends so the blocked `read_to_end` returns EOF), give one more short poll
/// window, then DETACH any thread that still hasn't finished and return what was
/// read. This is what makes the clean-exit (`Completed`) path un-hangable by a
/// surviving grandchild — the same no-hang guarantee the timeout path already
/// provides. Poll via `is_finished()` so we never block on a `join()`.
fn join_readers_bounded(
    out_thread: std::thread::JoinHandle<Vec<u8>>,
    err_thread: std::thread::JoinHandle<Vec<u8>>,
    leader_pid: u32,
    grace: Duration,
) -> (Vec<u8>, Vec<u8>) {
    let tick = Duration::from_millis(5);
    let wait_both = |deadline: std::time::Instant| {
        while !(out_thread.is_finished() && err_thread.is_finished()) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(tick);
        }
        true
    };

    if !wait_both(std::time::Instant::now() + grace) {
        // A descendant still holds a pipe open. Close the pipes by felling the
        // whole group, then give the now-unblocked readers a brief window to
        // finish their final read before we give up on them.
        kill_process_group(leader_pid);
        let _ = wait_both(std::time::Instant::now() + grace);
    }

    // Recover each buffer if the thread finished; otherwise (a thread still
    // pinned despite the group-kill — should not happen, but must not block)
    // detach it and substitute empty bytes. A finished thread `join()`s
    // immediately; we never join an unfinished one.
    let stdout = if out_thread.is_finished() {
        out_thread.join().unwrap_or_default()
    } else {
        Vec::new()
    };
    let stderr = if err_thread.is_finished() {
        err_thread.join().unwrap_or_default()
    } else {
        Vec::new()
    };
    (stdout, stderr)
}

/// SIGKILL an entire process group by the group leader's pid. cook spawns each
/// perry subprocess in its own group (`process_group(0)` → pgid == child pid),
/// so this fells perry AND every descendant it spawned (sh, clang, ld) in one
/// shot — the no-hang requirement when the leaf, not the leader, is what wedged.
/// No-op on non-unix (the direct `child.kill()` is the fallback there).
#[cfg(unix)]
fn kill_process_group(leader_pid: u32) {
    // Kernel `pid_t` is i32; on every supported platform a real pid fits, so the
    // conversion never fails. If it ever did, the group-kill is best-effort
    // cleanup — skip it rather than send to a wrong/negative target.
    let Ok(pid) = i32::try_from(leader_pid) else {
        return;
    };
    // SAFETY: a plain `kill(2)` syscall; the negated pid targets the process
    // group `pid`. Benign if the group is already gone (returns ESRCH).
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_leader_pid: u32) {
    // No portable process-group kill; the caller's `child.kill()` handles the
    // leader, and Windows' default job/console teardown reaps the rest.
}

/// Ask perry whether the script compiles. `version` is the already-resolved
/// perry version (the caller fetches it once for the cache key).
///
/// `perry check --check-deps --format json <script>` emits NDJSON: zero or more
/// diagnostic objects (one per line) followed by a
/// `{"type":"summary",…,"success":bool,"compilation_guaranteed":bool}` line.
///
/// We gate on `compilation_guaranteed` when present, falling back to `success`
/// when it's absent. `success` reflects only the FRONTEND checks (parse / HIR /
/// dependency resolution) and can be `true` for a script that still fails at
/// codegen; `compilation_guaranteed` is the stronger field that also accounts
/// for codegen, so gating on it trims false-positives before the compile step.
/// The fallback keeps us forward/backward compatible with a perry that doesn't
/// emit the field. Either way, verify-before-trust still backstops the decision.
/// We collect the human-readable `message` of each non-summary line for the
/// fallback notice.
fn gate(perry: &Path, script: &Path, version: &str) -> Gate {
    let mut cmd = Command::new(perry);
    cmd.arg("check")
        .arg("--check-deps")
        .arg("--format")
        .arg("json")
        .arg(script);

    // BOUNDED: a wedged `perry check` must never hang `nub cook`. On timeout we
    // fail CLOSED to Unavailable (→ Node) — the gate's verdict is "we couldn't
    // get an answer in time", which is exactly the don't-trust-it / run-on-Node
    // case, never a hard error or a hang.
    let bound = timeout_for(PERRY_CHECK_TIMEOUT);
    match run_with_timeout(cmd, bound, "check") {
        Ok(Timed::Completed { stdout, stderr, .. }) => {
            // perry writes the NDJSON to stdout; a nonzero exit (check failed)
            // still carries the summary line, so we parse stdout regardless of
            // exit status — the verdict comes from the summary, not the code.
            let stdout = String::from_utf8_lossy(&stdout);
            classify_check(&stdout, &stderr, version)
        }
        Ok(Timed::TimedOut { step }) => Gate::Unavailable {
            note: format!(
                "`perry {step}` did not finish within {} — running on Node instead",
                fmt_bound(bound)
            ),
        },
        Err(e) => Gate::Unavailable {
            note: format!("failed to run `perry check`: {e}"),
        },
    }
}

/// Format a timeout bound for a diagnostic — seconds when it's a whole-second
/// bound (the real constants), milliseconds when sub-second (the test seam),
/// so the message always reports the bound that ACTUALLY applied rather than a
/// hardcoded constant that the test override would contradict.
fn fmt_bound(d: Duration) -> String {
    if d.subsec_millis() == 0 && d.as_secs() > 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

/// Map `perry check`'s NDJSON stdout to a [`Gate`]. Factored out of [`gate`] so
/// the classification — including the zero-files-evaluated → `Indeterminate`
/// rule that lets an undiscovered extension (e.g. `.tsx`) through to compile —
/// is pinned by a unit test without spawning perry.
fn classify_check(stdout: &str, stderr: &[u8], version: &str) -> Gate {
    let mut compilable: Option<bool> = None;
    let mut diagnostics: Vec<String> = Vec::new();
    // How many files perry's `check` actually evaluated. The codegen-aware
    // summary reports `files_checked`; the zero-files result line (emitted for
    // an extension check's discovery doesn't glob — `.mts`/`.cts`/`.tsx`/`.jsx`/
    // `.js` against perry 0.5.1206) reports a top-level `files`. Either at 0
    // means check did NOT assess this input — distinct from a negative verdict.
    let mut files_evaluated: Option<u64> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("summary") {
            // Prefer `compilation_guaranteed` (codegen-aware); fall back to
            // `success` (frontend-only) when the field is absent.
            compilable = value
                .get("compilation_guaranteed")
                .and_then(|c| c.as_bool())
                .or_else(|| value.get("success").and_then(|s| s.as_bool()));
            if let Some(n) = value.get("files_checked").and_then(|f| f.as_u64()) {
                files_evaluated = Some(n);
            }
        } else if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
            // Prefix the perry diagnostic code where present, so the user sees
            // exactly which API/package isn't supported.
            match value.get("code").and_then(|c| c.as_str()) {
                Some(code) => diagnostics.push(format!("[{code}] {msg}")),
                None => diagnostics.push(msg.to_string()),
            }
        } else if let Some(n) = value.get("files").and_then(|f| f.as_u64()) {
            // The non-typed "no files found" result line (no `type:"summary"`):
            // `{"errors":0,"files":0,"success":true,"warnings":0}`. Capture its
            // file count so a zero here reads as indeterminate, not unavailable.
            files_evaluated = Some(n);
        }
    }

    // perry's `check` evaluated nothing (e.g. a `.tsx`/`.jsx`, which its
    // discovery doesn't glob) — NOT a negative verdict. Hand off to compile +
    // verify-before-trust rather than guessing.
    if files_evaluated == Some(0) {
        return Gate::Indeterminate {
            perry_version: version.to_string(),
        };
    }

    match compilable {
        Some(true) => Gate::Compilable {
            perry_version: version.to_string(),
        },
        Some(false) => {
            if diagnostics.is_empty() {
                diagnostics.push("(no diagnostics reported)".into());
            }
            Gate::Unsupported {
                diagnostics,
                raw: RawPerryOutput {
                    step: "check",
                    stdout: stdout.to_string(),
                    stderr: String::from_utf8_lossy(stderr).into_owned(),
                },
            }
        }
        None => Gate::Unavailable {
            note: format!(
                "`perry check` produced no summary (stderr: {})",
                String::from_utf8_lossy(stderr).trim()
            ),
        },
    }
}

/// Resolve perry's version (`perry --version` → `perry 0.5.1180`), memoized on
/// disk keyed by the perry binary's path + mtime + size. The version is part of
/// the cache key, so the warm path needs it every run — but spawning
/// `perry --version` each time costs ~13ms, dwarfing the cached-binary exec.
/// The memo turns that into a tiny file read; it invalidates automatically when
/// the perry binary changes (a self-update bumps mtime/size → cache miss →
/// re-spawn). On any cache-read hiccup we just re-spawn perry.
fn perry_version(perry: &Path) -> Option<String> {
    let stamp = perry_stamp(perry);
    if let Some(stamp) = &stamp {
        if let Some(v) = read_version_memo(stamp) {
            return Some(v);
        }
    }
    // BOUNDED like the other perry calls: `perry --version` is on the cold path
    // of every run, so a wedged version probe must not hang cook before the gate
    // is even reached. A timeout / non-zero exit → None → Node fallback.
    let mut cmd = Command::new(perry);
    cmd.arg("--version");
    let (status, stdout) =
        match run_with_timeout(cmd, timeout_for(PERRY_VERSION_TIMEOUT), "--version").ok()? {
            Timed::Completed { status, stdout, .. } => (status, stdout),
            Timed::TimedOut { .. } => return None,
        };
    if !status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&stdout).trim().to_string();
    if v.is_empty() {
        return None;
    }
    if let Some(stamp) = &stamp {
        write_version_memo(stamp, &v);
    }
    Some(v)
}

/// A fingerprint of the perry binary (canonical path + mtime + size). Any
/// in-place upgrade changes mtime/size, so a stale memo is never trusted.
fn perry_stamp(perry: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(perry).unwrap_or_else(|_| perry.to_path_buf());
    let meta = std::fs::metadata(&canonical).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(mtime.to_le_bytes());
    hasher.update(b"\0");
    hasher.update(meta.len().to_le_bytes());
    Some(format!("cook-perry-version/{:x}", hasher.finalize()))
}

fn read_version_memo(stamp: &str) -> Option<String> {
    let cache = cook_cache_dir().ok()?;
    let bytes = cacache::read_sync(&cache, stamp).ok()?;
    let v = String::from_utf8(bytes).ok()?;
    if v.is_empty() { None } else { Some(v) }
}

fn write_version_memo(stamp: &str, version: &str) {
    if let Ok(cache) = cook_cache_dir() {
        let _ = std::fs::create_dir_all(&cache);
        let _ = cacache::write_sync(&cache, stamp, version.as_bytes());
    }
}

/// Cache key = `sha256(source) + perry-version + compile-flags`. Any change to
/// the source, the compiler, or how we invoke it produces a new key, so a stale
/// binary is never served. The key is the hex digest of all three concatenated
/// — domain-separated by NUL so e.g. a source ending in the version string
/// can't collide with a different (source, version) pair. The verified-binary
/// blob is stored under this key; the not-cook-safe verdict under
/// [`verdict_key`], derived from the same digest.
fn cache_key(source: &[u8], perry_version: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nub-cook-v1\0");
    hasher.update(source);
    hasher.update(b"\0perry\0");
    hasher.update(perry_version.as_bytes());
    hasher.update(b"\0flags\0");
    hasher.update(COMPILE_FLAGS.join(" ").as_bytes());
    format!("cook/{:x}", hasher.finalize())
}

/// The verdict key for a binary key: the same content hash under a distinct
/// namespace, so the "not-cook-safe" marker and the (absent) verified binary
/// never collide but are both addressed by the SAME source content. A source
/// edit changes `cache_key` → changes this too → the verdict no longer applies,
/// which is exactly the re-attempt-on-change behavior we want.
fn verdict_key(key: &str) -> String {
    let tail = key.rsplit('/').next().unwrap_or(key);
    format!("cook-verdict/{tail}")
}

/// True if a "not-cook-safe" verdict is recorded for this key.
fn has_verdict(cache: &Path, key: &str) -> bool {
    cacache::metadata_sync(cache, verdict_key(key))
        .ok()
        .flatten()
        .is_some()
}

/// Record a "not-cook-safe" verdict for this key (best-effort; a write
/// failure just means we re-attempt next run, which is safe).
fn record_verdict(cache: &Path, key: &str) {
    let _ = cacache::write_sync(cache, verdict_key(key), VERDICT_NOT_SAFE);
}

/// nub's cook cache dir (`<nub-cache>/cook`), reusing nub's canonical
/// cache-dir resolution.
fn cook_cache_dir() -> Result<PathBuf> {
    let dir = nub_core::node::discovery::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("could not resolve nub cache dir"))?
        .join("cook");
    Ok(dir)
}

/// Warm path: read the cached VERIFIED binary blob (NO perry, NO Node),
/// materialize it, and exec. Returns an error if the cached entry is missing or
/// fails its integrity check, so the caller can fall through to a recompile.
fn run_cached(cache: &Path, key: &str, args: &[String]) -> Result<i32> {
    // cacache verifies the content-integrity hash on read, so a corrupted /
    // truncated entry surfaces as an Err here rather than a bad exec.
    let bytes = cacache::read_sync(cache, key)?;
    tracing::debug!(%key, "cook cache hit (warm path, verified binary)");
    let exec_path = materialize(cache, key, &bytes)?;
    exec_binary(&exec_path, args)
}

/// A failed `perry compile`, with the raw output for `--debug`.
struct CompileError {
    cause: String,
    /// Whether the failure is PerryTS's fault (it ran but produced a bad result:
    /// a non-clean exit, an unreadable/empty output binary) vs. a nub-side /
    /// environment failure BEFORE or AT spawn (cache-dir resolution, temp-file
    /// creation, failure to even spawn perry). Drives whether the "file a
    /// PerryTS issue" link is shown — never blame perry for a nub-side failure.
    attributable: bool,
    raw: Option<RawPerryOutput>,
}

/// `perry compile <script> -o <tmp>` → read the produced native binary's bytes.
/// Asserts a CLEAN exit; a non-clean compile is a [`CompileError`] carrying
/// perry's raw stdout/stderr for the `--debug` dump. The temp file lives in
/// nub's cook dir so it's on the same filesystem as the cache (no cross-device
/// copy) and is cleaned up on drop.
fn compile(perry: &Path, script: &Path) -> std::result::Result<Vec<u8>, CompileError> {
    let cache = cook_cache_dir().map_err(|e| CompileError {
        cause: format!("could not resolve cache dir: {e}"),
        attributable: false,
        raw: None,
    })?;
    let tmp = tempfile::Builder::new()
        .prefix("cook-compile-")
        .tempfile_in(&cache)
        .map_err(|e| CompileError {
            cause: format!("could not create temp output: {e}"),
            attributable: false,
            raw: None,
        })?;
    let out_path = tmp.path().to_path_buf();
    // Drop the file handle but keep the path: perry writes the executable to
    // out_path itself.
    drop(tmp);

    let mut cmd = Command::new(perry);
    cmd.arg("compile")
        .arg(script)
        .arg("-o")
        .arg(&out_path)
        .args(COMPILE_FLAGS);

    // BOUNDED: a wedged `perry compile` must never hang `nub cook`. On timeout
    // we clean up the partial output and report a NON-attributable failure (a
    // timeout isn't proof the compiler is wrong — it might just be slow on this
    // box), which routes to the Node fallback like any other compile failure.
    let bound = timeout_for(PERRY_COMPILE_TIMEOUT);
    let output = match run_with_timeout(cmd, bound, "compile") {
        Ok(Timed::Completed {
            status,
            stdout,
            stderr,
        }) => std::process::Output {
            status,
            stdout,
            stderr,
        },
        Ok(Timed::TimedOut { step }) => {
            let _ = std::fs::remove_file(&out_path);
            return Err(CompileError {
                cause: format!("`perry {step}` did not finish within {}", fmt_bound(bound)),
                // A timeout is not evidence of a compiler bug (could be a slow
                // host / cold stdlib build) — don't point the user at a perry
                // issue for it; just fall back to Node.
                attributable: false,
                raw: None,
            });
        }
        Err(e) => {
            return Err(CompileError {
                // Couldn't even spawn perry — a nub-side/environment failure.
                cause: format!("failed to run `perry compile`: {e}"),
                attributable: false,
                raw: None,
            });
        }
    };

    // Clean-exit check. A non-clean exit is perry's own compile failure.
    if !output.status.success() {
        let _ = std::fs::remove_file(&out_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CompileError {
            cause: format!(
                "`perry compile` exited with {} ({})",
                output.status,
                first_nonempty_line(&stderr).unwrap_or("no stderr")
            ),
            attributable: true,
            raw: Some(RawPerryOutput {
                step: "compile",
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: stderr.into_owned(),
            }),
        });
    }

    let bytes = std::fs::read(&out_path).map_err(|e| CompileError {
        // perry exited clean but the output isn't readable — a bad result from
        // perry, attributable to it.
        cause: format!("could not read compiled binary: {e}"),
        attributable: true,
        raw: None,
    })?;
    let _ = std::fs::remove_file(&out_path);
    if bytes.is_empty() {
        return Err(CompileError {
            // Clean exit but empty output — perry produced a bad binary.
            cause: "`perry compile` produced an empty binary".to_string(),
            attributable: true,
            raw: Some(RawPerryOutput {
                step: "compile",
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }),
        });
    }
    Ok(bytes)
}

fn first_nonempty_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// Write the binary bytes to a stable, executable path under the cache dir
/// (`<cache>/bin/<key-tail>`) and `chmod +x` it. The bytes passed in are always
/// the trusted source — either the just-compiled candidate (cold path) or the
/// cacache integrity-checked verified blob (warm path) — so we ALWAYS rewrite
/// (atomically via temp+rename) rather than reusing whatever happens to sit at
/// the path. A length-only "skip if same size" reuse would risk execing a
/// stale/partial prior `bin/` artifact whose bytes don't match the verified
/// blob (the cacache integrity guarantee covers the blob, NOT the materialized
/// file); rewriting every time keeps the executed artifact byte-identical to
/// the trusted bytes. The rewrite is cheap relative to the process exec that
/// follows.
fn materialize(cache: &Path, key: &str, bytes: &[u8]) -> Result<PathBuf> {
    let bin_dir = cache.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    // The key is `cook/<hex>`; use the hex tail as the filename.
    let tail = key.rsplit('/').next().unwrap_or(key);
    let exec_path = bin_dir.join(tail);

    // Write to a sibling temp then rename, so a concurrent cook never execs a
    // half-written file and the swap to the trusted bytes is atomic.
    let tmp = tempfile::Builder::new()
        .prefix(".cook-mat-")
        .tempfile_in(&bin_dir)?;
    std::fs::write(tmp.path(), bytes)?;
    tmp.persist(&exec_path)
        .map_err(|e| anyhow::anyhow!("could not place cook binary: {}", e.error))?;
    set_executable(&exec_path)?;
    Ok(exec_path)
}

/// Remove a materialized binary that failed verification, so a later run doesn't
/// find a stale executable on disk. Best-effort.
fn discard_binary(exec_path: &Path) {
    let _ = std::fs::remove_file(exec_path);
}

#[cfg(unix)]
fn set_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(p, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_p: &Path) -> Result<()> {
    Ok(())
}

/// Captured output of a cooked-binary run (for the verify differential).
struct Captured {
    stdout: Vec<u8>,
    code: i32,
}

/// Run the materialized native binary with `args`, CAPTURING stdout + exit code
/// (stderr inherits — informational). Used by the verify probe; the binary is a
/// standalone host executable — no Node, no V8.
fn capture_binary(path: &Path, args: &[String]) -> Result<Captured> {
    let output = Command::new(path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("failed to exec cooked binary: {e}"))?;
    Ok(Captured {
        stdout: output.stdout,
        code: exit_code(&output.status),
    })
}

/// Exec the materialized native binary, forwarding args + inheriting stdio, and
/// return its exit code. Used by the WARM path (a previously-verified binary).
fn exec_binary(path: &Path, args: &[String]) -> Result<i32> {
    let status = Command::new(path)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to exec cooked binary: {e}"))?;
    Ok(exit_code(&status))
}

fn exit_code(status: &std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            // Signal death: mirror the shell convention 128 + signal.
            status.signal().map(|s| 128 + s).unwrap_or(1)
        }
        #[cfg(not(unix))]
        {
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that mutate the process-global `PERRY_BIN` (and the
    /// `__NUB_COOK_PERRY_TIMEOUT_MS` seam). cargo runs the tests in one binary
    /// multi-threaded, so two tests racing on the same env var would clobber
    /// each other's view (`set_var` is `unsafe` for exactly this reason). Each
    /// such test holds this guard across its set→run→remove window. Lock-poisoning
    /// from an unrelated panic must not cascade, so we recover the guard.
    static PERRY_BIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn cache_key_changes_with_source() {
        let k1 = cache_key(b"console.log(1)", "perry 0.5.1180");
        let k2 = cache_key(b"console.log(2)", "perry 0.5.1180");
        assert_ne!(k1, k2, "different source must yield a different key");
        assert!(k1.starts_with("cook/"));
    }

    #[test]
    fn cache_key_changes_with_perry_version() {
        let k1 = cache_key(b"console.log(1)", "perry 0.5.1180");
        let k2 = cache_key(b"console.log(1)", "perry 0.6.0");
        assert_ne!(
            k1, k2,
            "a different perry version must invalidate the cached binary"
        );
    }

    #[test]
    fn cache_key_is_stable_for_identical_inputs() {
        let k1 = cache_key(b"const x = 1;", "perry 0.5.1180");
        let k2 = cache_key(b"const x = 1;", "perry 0.5.1180");
        assert_eq!(k1, k2, "identical inputs must hit the same cache entry");
    }

    #[test]
    fn verdict_key_is_distinct_namespace_but_same_content() {
        let k = cache_key(b"const x = 1;", "perry 0.5.1180");
        let vk = verdict_key(&k);
        assert_ne!(k, vk, "verdict and binary must not share a key");
        assert!(vk.starts_with("cook-verdict/"));
        // Same source → same verdict key (the re-attempt-on-change contract).
        let vk2 = verdict_key(&cache_key(b"const x = 1;", "perry 0.5.1180"));
        assert_eq!(vk, vk2);
        // Changed source → different verdict key (verdict no longer applies).
        let vk3 = verdict_key(&cache_key(b"const x = 2;", "perry 0.5.1180"));
        assert_ne!(vk, vk3);
    }

    #[test]
    fn verdict_roundtrip_marks_and_reads_not_safe() {
        // A recorded verdict is detected on the warm path; absent → not detected.
        let dir = tempfile::tempdir().unwrap();
        let key = cache_key(b"effectful-or-divergent", "perry 0.5.1180");
        assert!(!has_verdict(dir.path(), &key), "no verdict initially");
        record_verdict(dir.path(), &key);
        assert!(
            has_verdict(dir.path(), &key),
            "a recorded not-cook-safe verdict must be detected"
        );
        // A different source has no verdict (independent keys).
        let other = cache_key(b"different-source", "perry 0.5.1180");
        assert!(!has_verdict(dir.path(), &other));
    }

    #[test]
    fn cache_roundtrip_is_node_free() {
        // The cache path is the Rust `cacache` crate end-to-end — no perry, no
        // Node. Prove a write then read returns the same bytes.
        let dir = tempfile::tempdir().unwrap();
        let key = cache_key(b"payload-source", "perry 0.5.1180");
        let payload = b"\x7fELF-not-really-but-opaque-bytes".to_vec();
        cacache::write_sync(dir.path(), &key, &payload).unwrap();
        let back = cacache::read_sync(dir.path(), &key).unwrap();
        assert_eq!(back, payload);
    }

    #[test]
    fn materialize_writes_executable_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let key = cache_key(b"src", "perry 0.5.1180");
        let bytes = b"#!/bin/sh\necho hi\n".to_vec();
        let p1 = materialize(dir.path(), &key, &bytes).unwrap();
        assert!(p1.exists());
        let read_back = std::fs::read(&p1).unwrap();
        assert_eq!(read_back, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p1).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "materialized binary must be executable");
        }
        // Second call returns the same path without error (cache hit reuse).
        let p2 = materialize(dir.path(), &key, &bytes).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn discard_binary_removes_a_failed_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let key = cache_key(b"diverged", "perry 0.5.1180");
        let p = materialize(dir.path(), &key, b"opaque").unwrap();
        assert!(p.exists());
        discard_binary(&p);
        assert!(!p.exists(), "a diverged binary must be removed from disk");
    }

    #[test]
    fn stdout_diff_describes_length_and_content_mismatch() {
        assert!(describe_stdout_diff(b"abc", b"abcd").contains("length"));
        assert!(describe_stdout_diff(b"abc", b"abd").contains("content"));
    }

    #[test]
    fn verify_outcome_trusts_only_matching_stdout_and_exit() {
        // The verify-before-trust rule the cold path gates on: equivalent IFF
        // stdout AND exit code match. Differing stdout, differing exit, or both
        // all diverge; only a full match is trusted (→ cache + exec).
        let cooked = |out: &[u8], code: i32| Captured {
            stdout: out.to_vec(),
            code,
        };
        assert!(matches!(
            verify_outcome(&cooked(b"42\n", 0), b"42\n", 0),
            VerifyOutcome::Equivalent
        ));
        assert!(matches!(
            verify_outcome(&cooked(b"42\n", 0), b"43\n", 0),
            VerifyOutcome::Diverged
        ));
        assert!(matches!(
            verify_outcome(&cooked(b"42\n", 1), b"42\n", 0),
            VerifyOutcome::Diverged
        ));
        // stderr is NOT part of the comparison — only stdout + exit are captured,
        // so a binary whose stdout+exit match is Equivalent regardless of stderr.
    }

    #[test]
    fn divergence_is_not_perry_attributable_and_names_nondeterminism() {
        // The review's point: a verify divergence is NOT reported as a PerryTS
        // gap — it can be benign nondeterminism (time, randomness, PID, unordered
        // iteration), so the issue link is suppressed (`perry_attributable:false`)
        // and the cause names that benign explanation. `fail()` shows the issue
        // link IFF perry_attributable, so this flag is exactly what gates it.
        let cooked = Captured {
            stdout: b"1750000000000\n".to_vec(),
            code: 0,
        };
        let f = divergence_failure(Path::new("clock.ts"), &cooked, b"1750000000001\n", 0);
        assert!(
            !f.perry_attributable,
            "a verify divergence must NOT carry the PerryTS-issue link"
        );
        assert!(f.raw.is_none(), "a divergence has no raw perry output");
        assert!(
            f.cause.contains("nondeterministic"),
            "the cause must name nondeterminism as the likely benign explanation"
        );
        assert!(f.cause.contains("clock.ts"), "the cause names the script");
    }

    #[test]
    fn divergence_suppresses_the_issue_link_a_real_failure_keeps_it() {
        // `fail()` prints the PerryTS-issue link IFF the failure is perry-
        // attributable, via `issue_link_line`. Drive the REAL production path: a
        // divergence failure (built by `divergence_failure`) must suppress the
        // link; a perry-attributable failure must show it. This is the contrast
        // the review asked for — a divergence is NOT reported as a compiler gap.
        let cooked = Captured {
            stdout: b"a".to_vec(),
            code: 0,
        };
        let divergence = divergence_failure(Path::new("x.ts"), &cooked, b"b", 0);
        assert!(
            issue_link_line(divergence.perry_attributable).is_none(),
            "a verify divergence must NOT print the PerryTS-issue link"
        );

        let link = issue_link_line(true).expect("an attributable failure shows the link");
        assert!(
            link.contains(PERRY_ISSUES_URL),
            "the attributable link points at PerryTS issues"
        );
    }

    /// Replica of [`gate`]'s summary-keying decision: prefer
    /// `compilation_guaranteed`, fall back to `success`, collect diagnostics.
    /// Keeps the test free of a real `perry` while pinning the exact field
    /// precedence the gate relies on.
    fn parse_summary(stdout: &str) -> (Option<bool>, Vec<String>) {
        let mut compilable = None;
        let mut diags = Vec::new();
        for line in stdout.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v.get("type").and_then(|t| t.as_str()) == Some("summary") {
                compilable = v
                    .get("compilation_guaranteed")
                    .and_then(|c| c.as_bool())
                    .or_else(|| v.get("success").and_then(|s| s.as_bool()));
            } else if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
                diags.push(m.to_string());
            }
        }
        (compilable, diags)
    }

    #[test]
    fn gate_keys_on_compilation_guaranteed_when_present() {
        // A summary carrying compilation_guaranteed is what the gate trusts —
        // even when `success` disagrees, the codegen-aware field decides.
        // Real perry keeps the two aligned; we assert the codegen field wins.
        let stdout = "{\"code\":\"D002\",\"message\":\"eval() cannot be compiled\",\"type\":\"diagnostic\"}\n{\"type\":\"summary\",\"success\":true,\"compilation_guaranteed\":false}";
        let (compilable, diags) = parse_summary(stdout);
        assert_eq!(
            compilable,
            Some(false),
            "compilation_guaranteed must override success when both are present"
        );
        assert_eq!(diags.len(), 1);
        assert!(diags[0].contains("eval"));

        let ok = "{\"type\":\"summary\",\"success\":false,\"compilation_guaranteed\":true}";
        assert_eq!(parse_summary(ok).0, Some(true));
    }

    #[test]
    fn gate_falls_back_to_success_when_field_absent() {
        // An older perry without compilation_guaranteed: the gate uses `success`.
        let no_field_fail = "{\"code\":\"R003\",\"message\":\"Package 'express' not found\",\"type\":\"diagnostic\"}\n{\"type\":\"summary\",\"success\":false}";
        let (compilable, diags) = parse_summary(no_field_fail);
        assert_eq!(compilable, Some(false));
        assert_eq!(diags.len(), 1);
        assert!(diags[0].contains("express"));

        let no_field_ok = "{\"type\":\"summary\",\"success\":true}";
        assert_eq!(parse_summary(no_field_ok).0, Some(true));
    }

    // ---- classify_check: the gate's stdout→Gate mapping, driven on the REAL
    //      production fn with captured perry output for each shape.
    //      perry 0.5.1206: `.ts` is discovered and yields the codegen-aware
    //      summary (→ Compilable); an extension check's discovery doesn't glob
    //      (`.mts`/`.cts`/`.tsx`/`.jsx`/`.js`) yields the zero-files result line,
    //      which must classify as Indeterminate → compile, NOT a Node bail.
    //      Both paths reach `perry compile`, which handles them all (verified
    //      e2e on `.ts`/`.mts`/`.cts`).

    #[test]
    fn classify_ts_summary_is_compilable() {
        // A `.ts` perry check fully evaluates: codegen-aware summary, one file.
        let stdout = r#"{"compilation_guaranteed":true,"deps_checked":true,"errors":0,"files_checked":1,"files_modified":0,"success":true,"type":"summary","warnings":0}"#;
        assert!(matches!(
            classify_check(stdout, b"", "perry 0.5.1206"),
            Gate::Compilable { .. }
        ));
    }

    #[test]
    fn classify_undiscovered_ext_zero_files_is_indeterminate_not_unavailable() {
        // The exact line perry emits for an extension its discovery doesn't glob
        // (`.mts`/`.cts`/`.tsx`/`.jsx`/`.js` against perry 0.5.1206): no `type:"summary"`,
        // top-level `files:0`. This is NOT "does not compile" and NOT "perry
        // unavailable" — it's "check didn't look," so it must route to
        // compile-and-verify, where perry compile DOES handle the file. A
        // regression that mapped this to Unavailable (the pre-fix behavior)
        // would silently never AOT-compile those extensions.
        let zero_files = r#"{"errors":0,"files":0,"success":true,"warnings":0}"#;
        assert!(
            matches!(
                classify_check(zero_files, b"", "perry 0.5.1206"),
                Gate::Indeterminate { .. }
            ),
            "zero files evaluated must be Indeterminate (→ compile), not a Node bail"
        );
    }

    #[test]
    fn classify_zero_files_checked_in_summary_is_also_indeterminate() {
        // The other zero-files shape: a real summary whose `files_checked` is 0
        // (perry globbed a dir and matched nothing of the named input's kind).
        // Same meaning — check assessed nothing — so same Indeterminate verdict,
        // regardless of the (vacuous) `compilation_guaranteed:true` it carries.
        let summary_zero = r#"{"compilation_guaranteed":true,"deps_checked":true,"errors":0,"files_checked":0,"success":true,"type":"summary","warnings":0}"#;
        assert!(matches!(
            classify_check(summary_zero, b"", "perry 0.5.1206"),
            Gate::Indeterminate { .. }
        ));
    }

    #[test]
    fn classify_negative_verdict_is_unsupported_with_diagnostics() {
        // A genuine "does not compile" (>=1 file evaluated, compilation_guaranteed
        // false) stays Unsupported and keeps its diagnostics — the zero-files rule
        // must not swallow a real rejection.
        let neg = "{\"code\":\"D002\",\"message\":\"eval() cannot be compiled to native code\",\"type\":\"diagnostic\"}\n{\"compilation_guaranteed\":false,\"errors\":1,\"files_checked\":1,\"success\":false,\"type\":\"summary\",\"warnings\":0}";
        match classify_check(neg, b"", "perry 0.5.1206") {
            Gate::Unsupported { diagnostics, .. } => {
                assert!(diagnostics.iter().any(|d| d.contains("eval")));
            }
            other => panic!(
                "expected Unsupported, got a different gate ({})",
                gate_name(&other)
            ),
        }
    }

    #[test]
    fn classify_no_output_is_unavailable() {
        // Empty stdout (perry produced nothing parseable and evaluated nothing it
        // reported) → Unavailable. files_evaluated is None here (no result line at
        // all), so this is distinct from the explicit zero-files Indeterminate.
        assert!(matches!(
            classify_check("", b"some stderr", "perry 0.5.1206"),
            Gate::Unavailable { .. }
        ));
    }

    /// Name a [`Gate`] for a test panic message (the enum isn't `Debug`).
    fn gate_name(g: &Gate) -> &'static str {
        match g {
            Gate::Compilable { .. } => "Compilable",
            Gate::Indeterminate { .. } => "Indeterminate",
            Gate::Unsupported { .. } => "Unsupported",
            Gate::Unavailable { .. } => "Unavailable",
        }
    }

    // ---- run_with_timeout: the no-hang core. A wedged perry subprocess must be
    //      killed at the deadline, never block cook indefinitely (the bug this
    //      whole change exists to prevent — a prior `nub cook` HUNG ~17.5 min on
    //      a wedged `perry check`). These drive the real timeout primitive with
    //      short bounds and stub subprocesses so the assertions are fast +
    //      deterministic; the production bounds are seconds, the seam is the same.

    /// Write a `#!/bin/sh` stub at a temp path and make it executable. Returns
    /// a `TempPath` (keeps the file alive + auto-deletes; deref to `&Path` via
    /// `&*stub`) — used to stand in for a perry that returns promptly or wedges.
    ///
    /// `into_temp_path()` is load-bearing, NOT cosmetic: it consumes the
    /// `NamedTempFile`, CLOSING its writable fd while keeping the path. Linux
    /// returns `ETXTBSY` ("Text file busy") when you exec a file that still has
    /// an open writable fd, so a bare `NamedTempFile` (which holds that fd)
    /// makes every `Command::new(&*stub)` here fail to spawn on Linux
    /// (macOS does not enforce this, which is why it passes locally there).
    #[cfg(unix)]
    fn write_sh_stub(body: &str) -> tempfile::TempPath {
        let tmp = tempfile::Builder::new()
            .prefix("cook-stub-")
            .suffix(".sh")
            .tempfile()
            .unwrap();
        std::fs::write(tmp.path(), format!("#!/bin/sh\n{body}\n")).unwrap();
        set_executable(tmp.path()).unwrap();
        tmp.into_temp_path()
    }

    #[test]
    #[cfg(unix)]
    fn timeout_kills_a_wedged_child_within_the_bound() {
        // A child that would sleep far past the bound must be killed AT the
        // deadline and reported TimedOut — this is the load-bearing guarantee:
        // a wedged perry can NEVER hang cook. We give a 200ms bound to a child
        // that sleeps 60s and assert (a) the verdict is TimedOut and (b) the
        // call returned in well under the child's sleep (proving the kill, not a
        // wait-it-out), naming the step so the diagnostic can attribute it.
        let stub = write_sh_stub("sleep 60");
        let mut cmd = Command::new(&*stub);
        let started = std::time::Instant::now();
        let timed =
            run_with_timeout(cmd_for(&mut cmd), Duration::from_millis(200), "check").unwrap();
        let elapsed = started.elapsed();
        assert!(
            matches!(timed, Timed::TimedOut { step: "check" }),
            "a child past the deadline must be killed and reported TimedOut"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the kill must happen at the deadline, not wait out the child's 60s sleep (took {elapsed:?})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn timeout_kills_a_grandchild_that_holds_the_pipes_open() {
        // The empirically-found hang this whole design guards against: perry is
        // `sh -c …` → a real perry/clang/ld LEAF, so the wedge is usually in a
        // GRANDCHILD, not the process we spawn. Here the stub backgrounds a
        // `sleep 60` that INHERITS the stdout/stderr pipes, then the stub's own
        // shell blocks on `wait`. If run_with_timeout killed only the leader and
        // then JOINED the reader threads, those threads would block on
        // read_to_end forever (the orphaned sleep keeps the pipe write-end open,
        // so the pipe never reaches EOF) — the exact full-timeout hang observed
        // against a real wedged perry. The process-group SIGKILL + detached
        // readers must make this return at the deadline regardless.
        let stub = write_sh_stub("sleep 60 & wait");
        let mut cmd = Command::new(&*stub);
        let started = std::time::Instant::now();
        let timed =
            run_with_timeout(cmd_for(&mut cmd), Duration::from_millis(200), "check").unwrap();
        let elapsed = started.elapsed();
        assert!(
            matches!(timed, Timed::TimedOut { .. }),
            "a wedged grandchild must still be reported TimedOut"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "a grandchild holding the pipes must NOT hang the join — must return at the deadline (took {elapsed:?})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn clean_exit_with_a_grandchild_holding_the_pipes_does_not_hang() {
        // The clean-exit twin of the grandchild hang: the LEADER exits 0
        // immediately (NOT a timeout) while a backgrounded grandchild that
        // INHERITED the stdout/stderr pipes lives on, holding the write-ends
        // open. A blind `out_thread.join()` on the Completed path would block on
        // `read_to_end` FOREVER (the leader's exit does NOT close fds the
        // grandchild still holds, so the pipe never reaches EOF) — a clean exit
        // is just as capable of hanging cook as a wedge. The reader-drain grace
        // + group-kill must make this return PROMPTLY with the leader's real
        // (success) status. The stub backgrounds `sleep 60` WITHOUT redirecting
        // its stdout/stderr, so it keeps the captured pipe fds; the leader then
        // `exit 0`s without reaping it. The grandchild stays in the leader's
        // process group (no setpgid), so the group-kill fells it once the grace
        // window elapses. (Verified load-bearing: with the join unbounded this
        // test hangs; the bound makes it return.)
        let stub = write_sh_stub("sleep 60 & exit 0");
        let mut cmd = Command::new(&*stub);
        let started = std::time::Instant::now();
        let timed = run_with_timeout(cmd_for(&mut cmd), Duration::from_secs(10), "check").unwrap();
        let elapsed = started.elapsed();
        match timed {
            Timed::Completed { status, .. } => {
                assert!(status.success(), "the leader exited 0");
            }
            Timed::TimedOut { .. } => {
                panic!("a leader that exited 0 must be Completed, not TimedOut")
            }
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "a clean exit with a grandchild holding the pipes must NOT hang the \
             Completed-path join — it must drain-grace + group-kill and return (took {elapsed:?})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn timeout_returns_output_for_a_prompt_child() {
        // The other side: a child that finishes well within the bound returns
        // Completed with its real exit status + captured stdout — the timeout
        // path must not corrupt or drop the output of a healthy run.
        let stub = write_sh_stub("printf 'hello from stub'; exit 0");
        let mut cmd = Command::new(&*stub);
        match run_with_timeout(cmd_for(&mut cmd), Duration::from_secs(10), "check").unwrap() {
            Timed::Completed { status, stdout, .. } => {
                assert!(status.success(), "stub exited 0");
                assert_eq!(stdout, b"hello from stub");
            }
            Timed::TimedOut { .. } => panic!("a prompt child must not be reported TimedOut"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn timeout_captures_nonzero_exit_without_killing() {
        // A child that exits NON-ZERO promptly is Completed (its bad status
        // captured), NOT TimedOut — the timeout only fires on a genuine wedge,
        // and a non-clean exit flows to the normal failure handling, not the
        // kill path. (cook treats the exit 2 the prompt observed exactly this
        // way: a captured non-zero status, parsed/handled, never a panic.)
        let stub = write_sh_stub("printf 'on stderr' 1>&2; exit 2");
        let mut cmd = Command::new(&*stub);
        match run_with_timeout(cmd_for(&mut cmd), Duration::from_secs(10), "compile").unwrap() {
            Timed::Completed { status, stderr, .. } => {
                assert_eq!(
                    status.code(),
                    Some(2),
                    "the non-zero exit is captured as-is"
                );
                assert_eq!(stderr, b"on stderr");
            }
            Timed::TimedOut { .. } => panic!("a prompt non-zero exit is not a timeout"),
        }
    }

    /// Take a `&mut Command` and produce the owned `Command` `run_with_timeout`
    /// consumes, without re-typing the builder. (A small shim so the stub tests
    /// read cleanly.)
    fn cmd_for(cmd: &mut Command) -> Command {
        std::mem::replace(cmd, Command::new("true"))
    }

    #[test]
    #[cfg(unix)]
    fn wedged_perry_check_falls_back_to_node_does_not_hang() {
        // END-TO-END of the robustness fix: point PERRY_BIN at a perry stub that
        // WEDGES on `check` (sleeps far longer than the bound), clamp the bound
        // to a few ms via the test seam, and assert run() falls back to Node
        // PROMPTLY rather than hanging. This drives the real production wiring:
        // gate() → run_with_timeout → TimedOut → Gate::Unavailable → run_on_node.
        // The stub answers `--version` instantly (so we reach the gate) then
        // wedges on `check` — exactly the shape of the hang that motivated this.
        let stub = write_sh_stub(
            "case \"$1\" in\n  --version) echo 'perry 9.9.9-stub'; exit 0;;\n  check) sleep 60;;\n  *) exit 0;;\nesac",
        );
        let script = write_tmp_ts(b"console.log(1)\n");
        let script_path = script.path().to_string_lossy().into_owned();

        // Hold the env lock across the whole set→run→remove window so a
        // concurrent PERRY_BIN test can't observe (or clobber) our stub.
        let _guard = PERRY_BIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: process-global env; serialized by PERRY_BIN_LOCK above, set
        // for this test's duration and removed before returning.
        unsafe {
            std::env::set_var("PERRY_BIN", &*stub);
            std::env::set_var("__NUB_COOK_PERRY_TIMEOUT_MS", "50");
        }
        let started = std::time::Instant::now();
        let p = run_probe(&script_path, false);
        let elapsed = started.elapsed();
        unsafe {
            std::env::remove_var("PERRY_BIN");
            std::env::remove_var("__NUB_COOK_PERRY_TIMEOUT_MS");
        }

        assert_eq!(p.result.unwrap(), 0);
        assert!(
            p.fell_back,
            "a wedged perry check must fall back to running on Node"
        );
        assert!(
            !p.captured_ran,
            "a gate timeout must NOT reach the verify probe"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "cook must NOT hang on a wedged perry — it must time out and fall back fast (took {elapsed:?})"
        );
    }

    // ---- run() decision-flow tests (no perry on the box required) ----
    //
    // These drive the top-level decision logic with instrumented closures so we
    // observe WHICH path run() took (the cooked fast path vs the Node
    // fallback) without needing perry or a real compile. The perry-dependent
    // paths (gate-pass → compile → verify) are exercised by the e2e harness,
    // gated on perry being present; here we lock down the perry-INDEPENDENT
    // branches that decide "run on Node" before perry is ever consulted.

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Outcome of a `run()` probe: did the script run on Node (the fallback),
    /// did the verify probe run, and what did `run()` return.
    struct Probe {
        result: Result<i32>,
        fell_back: bool,
        captured_ran: bool,
    }

    /// Drive `run()` with instrumented closures and observe WHICH path it took
    /// (the cooked fast path vs the Node fallback) without needing perry or a
    /// real compile. Constructs the `CookArgs` inline (no complex returned
    /// type), runs, and reports the flags + result.
    fn run_probe(script: &str, compat_mode: bool) -> Probe {
        let fell_back = AtomicBool::new(false);
        let captured = AtomicBool::new(false);
        let result = run(CookArgs {
            script,
            forwarded_args: &[],
            compat_mode,
            debug: false,
            run_on_node: || {
                fell_back.store(true, Ordering::SeqCst);
                Ok(0)
            },
            run_on_node_captured: || {
                captured.store(true, Ordering::SeqCst);
                Ok((Vec::new(), 0))
            },
        });
        Probe {
            result,
            fell_back: fell_back.load(Ordering::SeqCst),
            captured_ran: captured.load(Ordering::SeqCst),
        }
    }

    fn write_tmp_ts(body: &[u8]) -> tempfile::NamedTempFile {
        let tmp = tempfile::Builder::new().suffix(".ts").tempfile().unwrap();
        std::fs::write(tmp.path(), body).unwrap();
        tmp
    }

    #[test]
    fn compat_mode_skips_aot_and_runs_on_node() {
        // `--node` / NODE_COMPAT must bypass the whole AOT path (and never touch
        // perry or the verify probe) — it's the zero-augmentation escape hatch.
        let tmp = write_tmp_ts(b"console.log(1)\n");
        let p = run_probe(&tmp.path().to_string_lossy(), true);
        assert_eq!(p.result.unwrap(), 0);
        assert!(p.fell_back, "compat must run on Node");
        assert!(!p.captured_ran, "compat must NOT run the verify probe");
    }

    #[test]
    fn missing_perry_falls_back_to_node() {
        // With PERRY_BIN pointed at a nonexistent path, locate_perry() returns
        // None → run() must fall back to Node, never attempting the verify probe.
        let tmp = write_tmp_ts(b"console.log(1)\n");
        let path = tmp.path().to_string_lossy().into_owned();

        // Serialize against the other PERRY_BIN-mutating test (the binary runs
        // multi-threaded) so neither observes the other's value.
        let _guard = PERRY_BIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: process-global env; serialized by PERRY_BIN_LOCK; restored
        // before returning.
        unsafe {
            std::env::set_var("PERRY_BIN", "/nonexistent/perry-binary-for-test");
        }
        let p = run_probe(&path, false);
        unsafe {
            std::env::remove_var("PERRY_BIN");
        }
        assert_eq!(p.result.unwrap(), 0);
        assert!(p.fell_back, "missing perry must run on Node");
        assert!(
            !p.captured_ran,
            "missing perry must NOT run the verify probe"
        );
    }

    #[test]
    fn missing_file_is_an_error_not_a_silent_fallback() {
        // A nonexistent script is a usage error (bail), distinct from a
        // compilability fallback — the user named a file that isn't there.
        let p = run_probe("/nonexistent/cook-script.ts", false);
        let err = p.result.unwrap_err();
        assert!(err.to_string().contains("no such file"));
        assert!(!p.fell_back);
    }
}
