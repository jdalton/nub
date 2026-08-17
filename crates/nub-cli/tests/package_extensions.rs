//! Top-level `package.json#packageExtensions` under Nub identity (#492).
//!
//! `packageExtensions` is a sanctioned neutral field: it patches a
//! dependency's manifest at resolve time — adding to its dependencies /
//! optionalDependencies / peerDependencies / peerDependenciesMeta, add-only,
//! never overriding a declared range — matching pnpm's `packageExtensions`.
//! The baseline test proves the field shapes the resolved graph end-to-end
//! against the offline Verdaccio test registry.
//!
//! Tests self-skip when `NUB_TEST_REGISTRY` is unset — run via
//! `source tests/registry/start.bash` then
//! `cargo test -p nub-cli --test package_extensions`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn nub_binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("nub");
    path
}

fn pm_tmpdir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nub-pkgext-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The Verdaccio offline test registry URL (set by `tests/registry/start.bash`
/// via the `NUB_TEST_REGISTRY` env var). When set, tests run against the
/// pre-seeded offline registry instead of registry.npmjs.org.
fn test_registry() -> Option<String> {
    std::env::var("NUB_TEST_REGISTRY")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Run `nub <args>` in `dir` against a CALLER-OWNED store/cache so the second
/// install warm-hits the first's node_modules + freshness state. A fresh store
/// per call would re-resolve unconditionally and mask the freshness contract.
/// When `NUB_TEST_REGISTRY` is set, the install points at the offline Verdaccio
/// registry (no network to npmjs.org).
fn run_install_in_store(dir: &Path, store: &Path, cache: &Path, args: &[&str]) -> (String, i32) {
    let mut cmd = Command::new(nub_binary());
    cmd.args(args)
        .current_dir(dir)
        .env("XDG_DATA_HOME", store)
        .env("XDG_CACHE_HOME", cache)
        // CI=true (set by GitHub Actions) auto-defaults nub to Frozen mode,
        // where a packageExtensions drift hard-fails with ERR_NUB_OUTDATED_LOCKFILE
        // instead of re-resolving. This test asserts the re-resolve path, so
        // force the default Fix mode regardless of the host CI env. is_ci()
        // checks var presence, not value, so the var must be removed entirely.
        .env_remove("CI");
    if let Some(registry) = test_registry() {
        // Merge the registry line into any existing .npmrc rather than
        // overwriting it: a test may pre-write other settings that must
        // survive alongside the registry redirect.
        let npmrc = dir.join(".npmrc");
        let existing = std::fs::read_to_string(&npmrc).unwrap_or_default();
        if !existing
            .lines()
            .any(|l| l.trim_start().starts_with("registry="))
        {
            let prefix = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            std::fs::write(&npmrc, format!("{existing}{prefix}registry={registry}\n")).unwrap();
        }
        cmd.env("NO_PROXY", "localhost,127.0.0.1")
            .env("no_proxy", "localhost,127.0.0.1");
    }
    let out = cmd.output().expect("failed to spawn nub");
    (
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// Whether the virtual store under `node_modules/.store` holds any version of
/// `name`.
fn store_has(dir: &Path, name: &str) -> bool {
    let prefix = format!("{name}@");
    std::fs::read_dir(dir.join("node_modules/.store"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .any(|n| n.starts_with(&prefix))
}

/// A nub-identity project installs a zero-dep dependency from the offline
/// test registry and writes `nub.lock`. The graph holds the resolved package
/// and nothing else (no extension has injected a transitive yet).
#[test]
fn top_level_install_shapes_graph_under_nub_identity() {
    if test_registry().is_none() {
        eprintln!("skipping: NUB_TEST_REGISTRY not set — run: source tests/registry/start.bash");
        return;
    }
    let dir = pm_tmpdir("shape");
    let store = pm_tmpdir("store");
    let cache = pm_tmpdir("cache");

    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"pkgext","version":"1.0.0","dependencies":{"is-positive":"3.1.0"}}"#,
    )
    .unwrap();

    let (err1, code1) = run_install_in_store(&dir, &store, &cache, &["install"]);
    assert_eq!(code1, 0, "baseline install failed: {err1}");
    assert!(
        dir.join("nub.lock").is_file(),
        "the nub-identity install writes nub.lock: {err1}"
    );
    assert!(
        !store_has(&dir, "is-number"),
        "without the extension the graph must not contain is-number: {err1}"
    );
}

/// Adding a top-level `packageExtensions` entry that injects a dependency into
/// a resolved package must invalidate the install fast path so the injected
/// dep lands on re-install against the same store.
#[test]
fn package_extensions_edit_invalidates_freshness() {
    if test_registry().is_none() {
        eprintln!("skipping: NUB_TEST_REGISTRY not set — run: source tests/registry/start.bash");
        return;
    }
    let dir = pm_tmpdir("ext");
    let store = pm_tmpdir("store");
    let cache = pm_tmpdir("cache");

    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"pkgext","version":"1.0.0","dependencies":{"is-positive":"3.1.0"}}"#,
    )
    .unwrap();
    let (err1, code1) = run_install_in_store(&dir, &store, &cache, &["install"]);
    assert_eq!(code1, 0, "baseline install failed: {err1}");

    // Add a top-level packageExtensions entry injecting is-number into
    // is-positive. is-positive declares no deps, so the add-only merge adds it.
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"pkgext","version":"1.0.0","dependencies":{"is-positive":"3.1.0"},"packageExtensions":{"is-positive@3.1.0":{"dependencies":{"is-number":"7.0.0"}}}}"#,
    )
    .unwrap();

    // Re-install against the SAME store. The packageExtensions edit changes the
    // install shape digest, so the fast path re-resolves and pulls the injected
    // is-number. Without packageExtensions in the shape digest the install
    // short-circuits ("Already up to date") and is-number never lands.
    let (err2, code2) = run_install_in_store(&dir, &store, &cache, &["install"]);
    assert_eq!(code2, 0, "extended install failed: {err2}");
    assert!(
        store_has(&dir, "is-number"),
        "top-level packageExtensions must inject is-number into is-positive, and \
         the edit must invalidate the install fast path so it re-resolves: {err2}"
    );
}
