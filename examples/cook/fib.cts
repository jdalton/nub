// The `.cts` (CommonJS TypeScript) sibling of fib.ts — same computation, CJS
// module form. PerryTS compiles the whole TypeScript family, so `nub cook
// fib.cts` AOT-compiles + verifies + caches it exactly like fib.ts. (A `.cts`
// that `require()`s a package PerryTS can't resolve compiles to a binary whose
// output differs from Node's; verify-before-trust catches that divergence and
// runs the script on Node — see unsupported.ts for the fallback in action.)
const n = Number(process.argv[2] ?? "20");

function fib(count: number): bigint[] {
  const out: bigint[] = [];
  let a = 0n;
  let b = 1n;
  for (let i = 0; i < count; i++) {
    out.push(a);
    [a, b] = [b, a + b];
  }
  return out;
}

console.log(fib(n).join(" "));
