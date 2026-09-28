# Vendored aube index remapping

Consume canonical file indexes during fresh package resolution, moving one index
per package and cloning only for additional peer contexts. The public store API
and cache format stay compatible. This applies the same change as
[aube PR 1650](https://github.com/aubepkg/aube/pull/1650), inspired by
[upm's avoidance of expanded index copies](https://github.com/unjs/upm/blob/16ad722f616e5ddb7b55f67440d75b4d1a1c2c09/src/link.ts#L611).

## Measurement

These are nub's vendored-engine **per-package remapping microbenchmarks**, not
whole-install or whole-graph timings. The harness calls the production function
and checks output; setup and output destruction are outside the timer. Ten paired
process runs alternate base/candidate order. Each pair uses the same file and
peer-context counts. Frozen installs that skip fresh resolution do not benefit.

Base: `5fafc732`. Candidate: `127913b89dc87b07cf9be617678b7036d86be9a8`.
[Remote validation and measurement](https://github.com/jdalton/nub/actions/runs/36455148125).
Separate Cargo target directories; release builds with LTO disabled and 16 codegen
units; Rust 1.98.1. Host: Linux-6.17.0-1022-azure-x86_64-with-glibc2.39; CPU AMD EPYC 7763 64-Core Processor;
4 visible CPUs. Post-run load averages: [2.5546875, 2.31591796875, 2.0341796875].
The raw batch timings in milliseconds are in [results.json](results.json).

| Files | Placements | Base per remap | Candidate per remap | Reduction |
| ---: | ---: | ---: | ---: | ---: |
| 16 | 1 | 1.366 µs | 0.307 µs | 77.6% |
| 16 | 8 | 10.323 µs | 9.644 µs | 6.6% |
| 1,024 | 1 | 69.412 µs | 0.691 µs | 99.0% |
| 1,024 | 8 | 1,647.011 µs | 1,411.378 µs | 14.3% |
| 10,000 | 1 | 1,844.580 µs | 0.963 µs | 99.9% |
| 10,000 | 8 | 17,207.299 µs | 14,742.369 µs | 14.3% |

For 1,024 files and eight placements, batch samples span 158.40–160.98 ms
at baseline and 136.39–137.60 ms after the change. All six sample ranges are
separated in this run; the table reports medians, not whole-install gains.
The single-package fixture does not quantify grouping overhead across a large graph.

## Reproduce


Use separate checkouts and separate Cargo target directories for the two builds.
Unset any shared `CARGO_TARGET_DIR` before building so Cargo cannot reuse an
executable from the other checkout. From `vendor/aube/` in a clean candidate
checkout, build the example:

```sh
unset CARGO_TARGET_DIR
CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  mise exec -- cargo build --locked --release -p aube --example index_remap
cp target/release/examples/index_remap /tmp/index-candidate
```

In a separate checkout of the base revision, work from `vendor/aube/` and copy the candidate example into
`crates/aube/examples/index_remap.rs`. Extract the base implementation into the
module path the example imports:

```python
from pathlib import Path
root = Path("crates/aube")
source = (root / "src/commands/install/fetch.rs").read_text()
start = source.index("/// Re-key a canonical-indexed")
end = source.index("\n#[cfg(test)]", start)
(root / "src/commands/install/index_remap.rs").write_text(
    "use std::collections::BTreeMap;\n" + source[start:end])
p = root / "examples/index_remap.rs"
p.write_text(p.read_text().replace(
    "remap_indices_to_contextualized(input,",
    "remap_indices_to_contextualized(&input,"))
```

Build with the same command and copy the base executable to `/tmp/index-base`.
Run `compare.py` from the candidate checkout. Both executables receive the same
file count, iterations and peer-context count. No network or filesystem placement
occurs inside the timed operation.
