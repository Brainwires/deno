// Run with `--allow-read --allow-write --deny-read=watched/secret`. A
// recursive watch is permission-checked on its root, but it must not report
// changes inside a deny scope below it, under any name.
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
const watcher = Deno.watchFs("watched", { recursive: true });
const seen: string[] = [];
let sawOk = false;
const { promise: okSeen, resolve: resolveOk } = Promise.withResolvers<void>();
(async () => {
  for await (const event of watcher) {
    for (const path of event.paths) {
      seen.push(path);
      if (path.endsWith("ok.txt")) {
        sawOk = true;
        resolveOk();
      }
    }
  }
})();
await delay(500);
Deno.writeTextFileSync("watched/secret/hidden.txt", "x");
try {
  Deno.lstatSync("watched/link");
  Deno.writeTextFileSync("watched/link/hidden2.txt", "x");
} catch {
  // no link (the `nolink` setup)
}
await delay(500);
Deno.writeTextFileSync("watched/ok/ok.txt", "x");
let timer: number | undefined;
await Promise.race([
  okSeen,
  new Promise((r) => timer = setTimeout(r, 10000)),
]);
clearTimeout(timer);
await delay(200);
watcher.close();
const reported = seen.filter((p) => p.includes("hidden"));
console.log(
  `reported: ${reported.length === 0 ? "none" : reported.join(", ")}`,
);
console.log(`saw the allowed change: ${sawOk}`);
