// The import-heavy entry: a 60-module star-export barrel, enumerated with
// Object.values and reduced. This is the exact shape PerryTS/perry#5736 reported
// — Object.values/entries over a wide (480-binding) export-* namespace — which
// PerryTS#5738 made O(1)-per-key. The enumeration order is the barrel's own
// (m0..m59), so the reduce is deterministic without a sort.
import * as mods from "./mods/index.mts";
const n = Number(process.argv[2] ?? "20");
const fns = Object.values(mods).filter(
  (v): v is (x: number) => number => typeof v === "function",
);
let acc = 0;
for (let i = 0; i < n; i++) acc = (acc + fns[i % fns.length](i)) % 1000003;
console.log(acc);
