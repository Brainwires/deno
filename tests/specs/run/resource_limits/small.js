// Fits in every limit; exits with the code given as its argument.
const sum = new Array(1000).fill(0).map((_, i) => i).reduce((a, b) => a + b);
console.log("sum:", sum);
Deno.exitCode = Number(Deno.args[0] ?? 0);
