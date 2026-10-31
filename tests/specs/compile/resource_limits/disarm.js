// A compiled program whose entry point lifts its baked limits for a
// subcommand (`./out job`), then goes past every one of them: a heap well over
// --max-memory, more CPU than --max-cpu-time and more time than --max-time.
if (Deno.args[0] === "job") {
  console.log("disarmed", Deno.disarmResourceLimits());
}
console.log("start");
const kept = [];
for (let i = 0; i < 160; i++) {
  // 1 MB of doubles each.
  kept.push(new Array(125_000).fill(i + 0.5));
}
const busyUntil = performance.now() + 1500;
while (performance.now() < busyUntil) {
  // Burn CPU.
}
await new Promise((resolve) => setTimeout(resolve, 1000));
console.log("done", kept.length);
