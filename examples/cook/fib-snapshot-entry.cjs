// V8 startup-snapshot build entry for fib (the `--build-snapshot` input).
//
// A snapshot bakes the *already parsed + compiled + instantiated* heap into a
// blob; `node --snapshot-blob fib.blob <n>` deserializes that heap and runs the
// registered main function — skipping parse/compile/instantiate, but NOT the
// Node/V8 process boot. The build entry must be CommonJS and synchronous: V8's
// mksnapshot cannot evaluate an ESM module graph (`import()` reports "Not
// supported" at build time), so the fib logic is inlined here rather than
// imported from fib.mts.
//
// Under --snapshot-blob there is no script path in argv, so the first user
// argument is process.argv[1] (not [2] as when running a script file).
"use strict";
const v8 = require("node:v8");

function fib(count) {
  const out = [];
  let a = 0n;
  let b = 1n;
  for (let i = 0; i < count; i++) {
    out.push(a);
    [a, b] = [b, a + b];
  }
  return out;
}

v8.startupSnapshot.setDeserializeMainFunction(() => {
  const n = Number(process.argv[1] ?? "20");
  console.log(fib(n).join(" "));
});
