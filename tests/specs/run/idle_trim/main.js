// Allocates about 300 MB (JS objects and small ArrayBuffers, whose backing
// stores come from malloc), drops it, then sits idle for 1.5 s. With
// `--unstable-idle-trim` (and DENO_IDLE_TRIM_DELAY_MS=200) the runtime runs a
// full GC and malloc_trim(0) during that idle time, so the RSS falls back.
const quiet = Deno.args[0] === "quiet";
const rss = () => Deno.memoryUsage().rss;
const MB = 1024 * 1024;

const base = rss();
let objects = [];
let buffers = [];
for (let i = 0; i < 1_000_000; i++) {
  objects.push({ i, s: `item-${i}-${"x".repeat(i % 64)}` });
}
for (let i = 0; i < 150_000; i++) {
  const b = new Uint8Array(1024);
  b[0] = 1;
  buffers.push(b);
}
const peak = rss();
objects = null;
buffers = null;
if (!quiet) console.log(`grew: ${peak - base > 150 * MB}`);

await new Promise((resolve) => setTimeout(resolve, 1500));

const after = rss();
if (quiet) {
  console.log("done");
} else {
  console.log(`dropped: ${peak - after > (peak - base) / 2}`);
}
