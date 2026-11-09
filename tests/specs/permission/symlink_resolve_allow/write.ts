import fs from "node:fs";
import { attempt } from "./attempt.ts";

const root = Deno.args[0] ?? ".";
const safe = `${root}/safe`;

// within the scope
attempt(
  "writeTextFile new",
  () => Deno.writeTextFileSync(`${safe}/new.txt`, "x"),
);
attempt(
  "writeTextFile inner link",
  () => Deno.writeTextFileSync(`${safe}/inner`, "ok"),
);
attempt("mkdir new", () => Deno.mkdirSync(`${safe}/newdir`));
attempt("remove new", () => Deno.removeSync(`${safe}/new.txt`));
attempt(
  "node writeFileSync new",
  () => fs.writeFileSync(`${safe}/node.txt`, "x"),
);
attempt("node unlinkSync new", () => fs.unlinkSync(`${safe}/node.txt`));

// out of the scope through a link: an existing and a missing final name
attempt(
  "writeTextFile link",
  () => Deno.writeTextFileSync(`${safe}/link`, "x"),
);
attempt(
  "writeTextFile dangling",
  () => Deno.writeTextFileSync(`${safe}/dangling`, "x"),
);
attempt(
  "open create dangling",
  () =>
    Deno.openSync(`${safe}/dangling`, { write: true, create: true }).close(),
);
attempt(
  "writeTextFile under dirlink",
  () => Deno.writeTextFileSync(`${safe}/dirlink/new.txt`, "x"),
);
attempt("mkdir under dirlink", () => Deno.mkdirSync(`${safe}/dirlink/newdir`));
attempt(
  "remove under dirlink",
  () => Deno.removeSync(`${safe}/dirlink/secret.txt`),
);
attempt(
  "remove recursive under dirlink",
  () => Deno.removeSync(`${safe}/dirlink/dir`, { recursive: true }),
);
attempt(
  "rename into dirlink",
  () => Deno.renameSync(`${safe}/rename.txt`, `${safe}/dirlink/moved.txt`),
);
attempt(
  "copyFile into dirlink",
  () => Deno.copyFileSync(`${safe}/file.txt`, `${safe}/dirlink/copy.txt`),
);
attempt("chmod link", () => Deno.chmodSync(`${safe}/link`, 0o777));
attempt("utime link", () => Deno.utimeSync(`${safe}/link`, 0, 0));
attempt(
  "node writeFileSync under dirlink",
  () => fs.writeFileSync(`${safe}/dirlink/node.txt`, "x"),
);
attempt(
  "node appendFileSync link",
  () => fs.appendFileSync(`${safe}/link`, "x"),
);
attempt(
  "node rmSync under dirlink",
  () => fs.rmSync(`${safe}/dirlink/secret.txt`),
);
attempt(
  "node cpSync into dirlink",
  () => fs.cpSync(`${safe}/file.txt`, `${safe}/dirlink/cp.txt`),
);

console.log(
  "outside entries:",
  [...Deno.readDirSync("outside")].map((e) => e.name).sort().join(","),
);
console.log("secret:", Deno.readTextFileSync("outside/secret.txt"));
