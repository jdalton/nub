#!/usr/bin/env node
// Generator for the import-heavy benchmark fixture. Emits, deterministically:
//   mods/mNN.mts    — 60 ESM modules, 8 exported functions each (480 total)
//   mods/index.mts  — a barrel re-exporting all of them with `export *`
//
// The point is a real parse/compile/instantiate cost plus a wide (480-binding)
// star-export namespace to enumerate, with trivial per-call compute, so the
// heavy group stays startup-dominated. heavy.mts (the ESM entry that enumerates
// the barrel with Object.values and reduces) is hand-written and not regenerated
// here.
//
//   node heavy/gen-mods.mjs        # run from examples/cook
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const N = 60; // modules
const F = 8; // functions per module
const K = 6; // mix steps per function

mkdirSync(join(here, "mods"), { recursive: true });

// Body of one exported function mI_fF as a string. Trivial pure compute so the
// fixture stays startup-dominated rather than turning into a compute benchmark.
function fnSource(i, f) {
  let s = `export function m${i}_f${f}(x: number): number {\n  let acc = x + ${i} * ${f};\n`;
  for (let k = 0; k < K; k++) s += `  acc = (acc * ${k + 2} + ${i + f + k}) % 1000003;\n`;
  s += `  return acc;\n}\n`;
  return s;
}

for (let i = 0; i < N; i++) {
  let body = `// module ${i}\n`;
  for (let f = 0; f < F; f++) body += fnSource(i, f);
  writeFileSync(join(here, "mods", `m${i}.mts`), body);
}
let barrel = "";
for (let i = 0; i < N; i++) barrel += `export * from "./m${i}.mts";\n`;
writeFileSync(join(here, "mods", "index.mts"), barrel);

console.log(`wrote ${N} modules + barrel`);
