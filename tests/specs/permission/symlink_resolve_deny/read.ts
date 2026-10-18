import fs from "node:fs";
import process from "node:process";

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

attempt(
  "readTextFile existing",
  () => Deno.readTextFileSync("allowed/existing"),
);
attempt(
  "readTextFile under dir link",
  () => Deno.readTextFileSync("allowed/dir/secret.txt"),
);
attempt("stat existing", () => Deno.statSync("allowed/existing"));
attempt("readDir dir link", () => [...Deno.readDirSync("allowed/dir")]);
attempt("lstat under dir link", () => Deno.lstatSync("allowed/dir/secret.txt"));
attempt(
  "node readFileSync under dir link",
  () => fs.readFileSync("allowed/dir/secret.txt"),
);
attempt("node statSync existing", () => fs.statSync("allowed/existing"));
attempt("node readdirSync dir link", () => fs.readdirSync("allowed/dir"));
attempt("node loadEnvFile", () => process.loadEnvFile("allowed/env"));

// the links themselves are still readable
console.log("lstat link:", Deno.lstatSync("allowed/existing").isSymlink);
console.log("readLink:", Deno.readLinkSync("allowed/env"));
console.log(
  "allowed entries:",
  [...Deno.readDirSync("allowed")].map((e) => e.name).sort().join(","),
);
