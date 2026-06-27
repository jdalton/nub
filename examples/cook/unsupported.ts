// Demonstrates the graceful fallback. `eval()` cannot be AOT-compiled to
// native code, so PerryTS rejects this script. `nub cook unsupported.ts`
// prints PerryTS's diagnostic (which API isn't supported), then runs the
// script on Node anyway — so it still works, just without the AOT speedup.
const answer = eval("6 * 7");
console.log("the answer is", answer);
