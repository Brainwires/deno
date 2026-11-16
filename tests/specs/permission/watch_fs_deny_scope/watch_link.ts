// Run with `--allow-read --allow-write` and no deny: a recursive watch still
// walks into a symlinked directory inside the tree (as upstream), so a change
// in the link's target is reported.
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
Deno.mkdirSync("shared", { recursive: true });
Deno.mkdirSync("tree", { recursive: true });
Deno.symlinkSync("../shared", "tree/linked");
const watcher = Deno.watchFs("tree", { recursive: true });
const { promise: seen, resolve } = Promise.withResolvers<boolean>();
(async () => {
  for await (const event of watcher) {
    if (event.paths.some((p) => p.endsWith("inner.txt"))) resolve(true);
  }
})();
await delay(500);
Deno.writeTextFileSync("shared/inner.txt", "x");
let timer: number | undefined;
const saw = await Promise.race([
  seen,
  new Promise<boolean>((r) => timer = setTimeout(() => r(false), 10000)),
]);
clearTimeout(timer);
watcher.close();
console.log(`saw the change through the link: ${saw}`);
