import * as mods from "./mods/index.mts";
const n = Number(process.argv[2] ?? "20");
const fns = Object.entries(mods)
  .filter((e): e is [string, (x: number) => number] => typeof e[1] === "function")
  .sort((a, b) => {
    // numeric sort on the module/func indices encoded in mXX_fY
    const pa = a[0].match(/m(\d+)_f(\d+)/)!, pb = b[0].match(/m(\d+)_f(\d+)/)!;
    return (+pa[1] - +pb[1]) || (+pa[2] - +pb[2]);
  })
  .map((e) => e[1]);
let acc = 0;
for (let i = 0; i < n; i++) acc = (acc + fns[i % fns.length](i)) % 1000003;
console.log(acc);
