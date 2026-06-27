// The `.mts` (ESM TypeScript) sibling of fib.ts — same computation, ESM module
// form (the trailing `export {}` makes it an explicit module). PerryTS compiles
// the whole TypeScript family (`.ts`/`.mts`/`.cts`/`.tsx`), so `nub cook fib.mts`
// AOT-compiles + verifies + caches it exactly like fib.ts.
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

export {};
