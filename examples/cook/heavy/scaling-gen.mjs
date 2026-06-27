#!/usr/bin/env node
// Module-count scaling generator for the perry per-module-init cost study.
// Emits, for a given module count K, into <outdir>:
//   mods/mNN.mts   — K ESM modules, 8 exported functions each (same trivial
//                    body as examples/cook/heavy/gen-mods.mjs)
//   mods/index.mts — a barrel re-exporting all of them
//   entry.mts      — the ESM entry: imports the barrel, runs the same reduce
//                    driver as heavy.mts. argv arg kept small → startup-dominated.
//
//   node heavy/scaling-gen.mjs <K> <outdir>      # driven by ../scaling-sweep.sh
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const K = Number(process.argv[2]); // module count
const outdir = process.argv[3];
const F = 8; // functions per module (matches heavy/gen-mods.mjs)
const STEPS = 6; // mix steps per function (matches heavy/gen-mods.mjs's K=6)

if (!Number.isInteger(K) || K < 1 || !outdir) {
  console.error("usage: node heavy/scaling-gen.mjs <K:int>=1> <outdir>");
  process.exit(2);
}

mkdirSync(join(outdir, "mods"), { recursive: true });

function fnSource(i, f) {
  let s = `export function m${i}_f${f}(x: number): number {\n  let acc = x + ${i} * ${f};\n`;
  for (let k = 0; k < STEPS; k++) s += `  acc = (acc * ${k + 2} + ${i + f + k}) % 1000003;\n`;
  s += `  return acc;\n}\n`;
  return s;
}

for (let i = 0; i < K; i++) {
  let body = `// module ${i}\n`;
  for (let f = 0; f < F; f++) body += fnSource(i, f);
  writeFileSync(join(outdir, "mods", `m${i}.mts`), body);
}

let barrel = "";
for (let i = 0; i < K; i++) barrel += `export * from "./m${i}.mts";\n`;
writeFileSync(join(outdir, "mods", "index.mts"), barrel);

const entry = `import * as mods from "./mods/index.mts";
const n = Number(process.argv[2] ?? "20");
const fns = Object.entries(mods)
  .filter((e): e is [string, (x: number) => number] => typeof e[1] === "function")
  .sort((a, b) => {
    const pa = a[0].match(/m(\\d+)_f(\\d+)/)!, pb = b[0].match(/m(\\d+)_f(\\d+)/)!;
    return (+pa[1] - +pb[1]) || (+pa[2] - +pb[2]);
  })
  .map((e) => e[1]);
let acc = 0;
for (let i = 0; i < n; i++) acc = (acc + fns[i % fns.length](i)) % 1000003;
console.log(acc);
`;
writeFileSync(join(outdir, "entry.mts"), entry);

console.error(`K=${K}: wrote ${K} modules (${K * F} fns) + barrel + entry.mts → ${outdir}`);
