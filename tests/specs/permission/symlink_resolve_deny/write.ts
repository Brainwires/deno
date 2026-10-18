import fs from "node:fs";

function attempt(name: string, fn: () => unknown) {
  try {
    fn();
    console.log(`${name}: allowed (BUG)`);
  } catch (e) {
    if (e instanceof Deno.errors.NotCapable) {
      console.log(`${name}: denied`);
    } else {
      console.log(`${name}: error (${(e as Error).name})`);
    }
  }
}

// dangling symlinks: the file would be created inside the denied directory
attempt(
  "writeTextFile dangling",
  () => Deno.writeTextFileSync("allowed/dangling", "x"),
);
attempt(
  "open create dangling",
  () =>
    Deno.openSync("allowed/dangling", { write: true, create: true }).close(),
);
attempt(
  "writeTextFile chain",
  () => Deno.writeTextFileSync("allowed/chain1", "x"),
);
attempt(
  "writeTextFile via dir link",
  () => Deno.writeTextFileSync("allowed/via_dir", "x"),
);
attempt(
  "copyFile to dangling",
  () => Deno.copyFileSync("allowed/src.txt", "allowed/dangling"),
);
attempt(
  "node writeFileSync dangling",
  () => fs.writeFileSync("allowed/dangling", "x"),
);
attempt(
  "node appendFileSync chain",
  () => fs.appendFileSync("allowed/chain1", "x"),
);
attempt(
  "node copyFileSync to dangling",
  () => fs.copyFileSync("allowed/src.txt", "allowed/dangling"),
);
attempt(
  "node cpSync to dangling",
  () => fs.cpSync("allowed/src.txt", "allowed/dangling"),
);

// following operations on a link to an existing denied file
attempt(
  "copyFile to existing",
  () => Deno.copyFileSync("allowed/src.txt", "allowed/existing"),
);
attempt("truncate existing", () => Deno.truncateSync("allowed/existing", 0));
attempt("chmod existing", () => Deno.chmodSync("allowed/existing", 0o600));
attempt(
  "chown existing",
  () => Deno.chownSync("allowed/existing", Deno.uid(), Deno.gid()),
);
attempt("utime existing", () => Deno.utimeSync("allowed/existing", 1, 1));
attempt(
  "node truncateSync existing",
  () => fs.truncateSync("allowed/existing", 0),
);

// metadata changes through a descriptor opened (read-only) through the link
{
  const file = Deno.openSync("allowed/existing");
  attempt("FsFile utime existing", () => file.utimeSync(1, 1));
  file.close();
  const fd = fs.openSync("allowed/existing", "r");
  attempt("node fchmodSync existing", () => fs.fchmodSync(fd, 0o600));
  attempt(
    "node fchownSync existing",
    () => fs.fchownSync(fd, Deno.uid()!, Deno.gid()!),
  );
  attempt("node futimesSync existing", () => fs.futimesSync(fd, 1, 1));
  fs.closeSync(fd);
}

// no-follow operations under a symlinked directory
attempt(
  "remove under dir link",
  () => Deno.removeSync("allowed/dir/remove.txt"),
);
attempt(
  "remove recursive under dir link",
  () => Deno.removeSync("allowed/dir/tree", { recursive: true }),
);
attempt(
  "node rmSync recursive under dir link",
  () => fs.rmSync("allowed/dir/tree", { recursive: true }),
);
attempt(
  "rename from dir link",
  () => Deno.renameSync("allowed/dir/rename.txt", "allowed/moved.txt"),
);
attempt(
  "rename into dir link",
  () => Deno.renameSync("allowed/src_rename.txt", "allowed/dir/moved.txt"),
);
attempt("mkdir under dir link", () => Deno.mkdirSync("allowed/dir/sub"));
attempt(
  "link into dir link",
  () => Deno.linkSync("allowed/src.txt", "allowed/dir/hard.txt"),
);
attempt(
  "node unlinkSync under dir link",
  () => fs.unlinkSync("allowed/dir/remove.txt"),
);
attempt(
  "node mkdirSync under dir link",
  () => fs.mkdirSync("allowed/dir/sub2"),
);

// a cycle is an error, never a write
attempt(
  "writeTextFile loop",
  () => Deno.writeTextFileSync("allowed/loop1", "x"),
);

// the links themselves stay usable
Deno.removeSync("allowed/dangling");
Deno.renameSync("allowed/chain1", "allowed/chain1_renamed");
console.log("links removable: ok");

console.log(
  "denied entries:",
  [...Deno.readDirSync("denied")].map((e) => e.name).sort().join(","),
);
console.log("secret:", Deno.readTextFileSync("denied/secret.txt"));
console.log(
  "secret mode unchanged:",
  (Deno.statSync("denied/secret.txt").mode! & 0o777) !== 0o600,
);
