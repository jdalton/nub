// A small CLI that prints the first N Fibonacci numbers (default 20).
// In-scope for PerryTS: pure compute + console + process.argv — no npm deps.
// `nub cook fib.ts` compiles this to a native binary on first run, caches it,
// and runs the cached binary on every subsequent run.

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
