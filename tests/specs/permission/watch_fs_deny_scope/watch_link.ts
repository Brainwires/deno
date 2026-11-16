// Run with `--allow-read --allow-write` and no deny. A recursive watch
// doesn't walk into symlinked directories, and a change is reported only
// when its resolved path lies inside the watched tree (as upstream), so a
// change made through a link to a directory inside the tree is still
// reported, by its real path.
const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
Deno.mkdirSync("tree/real", { recursive: true });
Deno.symlinkSync("real", "tree/linked");
const watcher = Deno.watchFs("tree", { recursive: true });
const seen = new Set<string>();
const { promise: both, resolve } = Promise.withResolvers<boolean>();
(async () => {
  for await (const event of watcher) {
    for (const path of event.paths) {
      if (path.endsWith("a.txt")) seen.add("a");
      if (path.endsWith("b.txt")) seen.add("b");
    }
    if (seen.size === 2) resolve(true);
  }
})();
await delay(500);
Deno.writeTextFileSync("tree/real/a.txt", "x");
Deno.writeTextFileSync("tree/linked/b.txt", "x");
let timer: number | undefined;
const saw = await Promise.race([
  both,
  new Promise<boolean>((r) => timer = setTimeout(() => r(false), 10000)),
]);
clearTimeout(timer);
watcher.close();
console.log(`saw both changes: ${saw}`);
