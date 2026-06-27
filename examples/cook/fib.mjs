const n = Number(process.argv[2] ?? "20");
function fib(c){const o=[];let a=0n,b=1n;for(let i=0;i<c;i++){o.push(a);[a,b]=[b,a+b];}return o;}
console.log(fib(n).join(" "));
