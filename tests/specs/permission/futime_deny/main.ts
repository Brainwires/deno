// Deno.FsFile's utime acts through an open descriptor, and the open needed
// only read permission: the change needs write permission on the file's path
// (the partial check Deno.utime has: times touch only the entry's own
// metadata, so a folder that holds a denied path still takes them).
function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
async function outcomeAsync(fn: () => Promise<void>) {
  try {
    await fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
function mtime(path: string) {
  return Deno.statSync(path).mtime?.getTime();
}

using denied = Deno.openSync("base/a/b/prog", { read: true });
console.log(
  "utime on a denied file:",
  outcome(() => denied.utimeSync(1000, 1000)),
);
console.log(
  "utime on a denied file (async):",
  await outcomeAsync(() => denied.utime(1000, 1000)),
);
console.log("denied file times kept:", mtime("base/a/b/prog") !== 1000_000);

// A folder open read-only that holds the denied path.
using parent = Deno.openSync("base/a", { read: true });
console.log(
  "utime on a parent of a denied path:",
  outcome(() => parent.utimeSync(1000, 1000)),
);

// A file with nothing denied in or over it, under the write grant.
using other = Deno.openSync("base/other/file", { read: true });
console.log(
  "utime on another file:",
  outcome(() => other.utimeSync(1000, 1000)),
);
console.log("other file times changed:", mtime("base/other/file") === 1000_000);
