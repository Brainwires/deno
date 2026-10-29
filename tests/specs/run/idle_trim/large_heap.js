// Keeps 600,000 small objects (well over 32 MB of heap) alive, then lets the
// loop go idle twice.
// With DENO_IDLE_TRIM_MAX_GC_HEAP_MB=32 the first trim runs the full GC,
// which leaves the heap over the limit, so the second trim skips the GC and
// only runs malloc_trim.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const keep = [];
for (let i = 0; i < 600_000; i++) {
  keep.push({ i, s: `item-${i}-${"x".repeat(48)}` });
}
console.log("first");
await sleep(800);
console.log("second");
// Some garbage, so the RSS has grown since the first trim.
let junk = Array.from({ length: 200_000 }, (_, i) => `${"y".repeat(100)}${i}`);
junk = null;
await sleep(800);
console.log(`kept: ${keep.length}`);
