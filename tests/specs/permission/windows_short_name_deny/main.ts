// On Windows an 8.3 alias (LONGNA~1) opens the same entry as its long name
// but compared as another string: the no-follow checks (rename, remove,
// symlink) resolve the parent and keep the final name as given, so a denied
// path's parent could be moved or removed by its alias, and a grant or deny
// scope written as an alias matched nothing written long. Every existing
// component is now read as its long name.
function outcome(fn: () => void) {
  try {
    fn();
    return "done";
  } catch (e) {
    return e instanceof Deno.errors.NotCapable ? "refused" : `error ${e}`;
  }
}
let short = Deno.readTextFileSync("short.txt").trim();
if (short === "") {
  console.log(
    `${
      Deno.readTextFileSync("reason.txt").trim()
    }; checking the long name instead`,
  );
  short = "longname";
}

console.log(
  "rename a parent of a denied path by its short name:",
  outcome(() => Deno.renameSync(`base/${short}`, "base/moved")),
);
console.log(
  "remove a parent of a denied path by its short name:",
  outcome(() => Deno.removeSync(`base/${short}`, { recursive: true })),
);
console.log(
  "write into the denied path by its short name:",
  outcome(() => Deno.writeTextFileSync(`base/${short}/b/new.txt`, "x")),
);
console.log(
  "remove the denied file by its short name:",
  outcome(() => Deno.removeSync(`base/${short}/b/prog`)),
);
console.log(
  "still packaged:",
  Deno.readTextFileSync("base/longname/b/prog") === "packaged",
);
console.log(
  "write beside the denied path by its short name:",
  outcome(() => Deno.writeTextFileSync(`base/${short}/other/new.txt`, "x")),
);

// Scopes written with the short name, checked against the long name.
function child(flags: string[], script: string) {
  const { stdout, stderr } = new Deno.Command(Deno.execPath(), {
    args: ["run", ...flags, script],
  }).outputSync();
  const decoder = new TextDecoder();
  return (decoder.decode(stdout) + decoder.decode(stderr)).trim();
}
console.log(
  child(
    ["--allow-read", "--allow-write=base", `--deny-write=base/${short}/b`],
    "child_deny.ts",
  ),
);
console.log(
  child(
    ["--allow-read", `--allow-write=base/${short}/other`],
    "child_allow.ts",
  ),
);

// The alias feeds the comparison only: the operation keeps the path as
// given (a compiled binary's virtual file system is keyed on the
// executable's path as spelled, so a read through its long name missed it).
try {
  Deno.readTextFileSync(`base/${short}/other/missing.txt`);
  console.log("the operation keeps the alias in its path: (no error)");
} catch (e) {
  console.log(
    "the operation keeps the alias in its path:",
    e instanceof Deno.errors.NotFound && String(e).includes(short),
  );
}
