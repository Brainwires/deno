import fs from "node:fs";
import { attempt } from "./attempt.ts";

const root = Deno.args[0] ?? ".";

// within the scope
attempt(
  "readTextFile file",
  () => Deno.readTextFileSync(`${root}/safe/file.txt`),
);
attempt(
  "readTextFile inner link",
  () => Deno.readTextFileSync(`${root}/safe/inner`),
);
attempt(
  "readDir scope",
  () =>
    [...Deno.readDirSync(`${root}/safe`)].map((e) => e.name).sort().join(","),
);
attempt(
  "lstat link",
  () => String(Deno.lstatSync(`${root}/safe/link`).isSymlink),
);
attempt("readLink link", () => Deno.readLinkSync(`${root}/safe/link`));
// `..` is lexical in the path as given
attempt(
  "readTextFile dirlink/..",
  () => Deno.readTextFileSync(`${root}/safe/dirlink/../file.txt`),
);

// out of the scope through a link
attempt("readTextFile link", () => Deno.readTextFileSync(`${root}/safe/link`));
attempt(
  "readTextFile under dirlink",
  () => Deno.readTextFileSync(`${root}/safe/dirlink/secret.txt`),
);
attempt(
  "readTextFile dirlink/dir/..",
  () => Deno.readTextFileSync(`${root}/safe/dirlink/dir/../secret.txt`),
);
attempt("open link", () => Deno.openSync(`${root}/safe/link`).close());
attempt("stat link", () => Deno.statSync(`${root}/safe/link`));
attempt(
  "lstat under dirlink",
  () => Deno.lstatSync(`${root}/safe/dirlink/secret.txt`),
);
attempt("readDir dirlink", () => [...Deno.readDirSync(`${root}/safe/dirlink`)]);
attempt("realPath link", () => Deno.realPathSync(`${root}/safe/link`));
attempt("realPath dangling", () => Deno.realPathSync(`${root}/safe/dangling`));
attempt("watchFs dirlink", () => Deno.watchFs(`${root}/safe/dirlink`).close());
attempt(
  "node readFileSync link",
  () => fs.readFileSync(`${root}/safe/link`, "utf8"),
);
attempt(
  "node readFileSync under dirlink",
  () => fs.readFileSync(`${root}/safe/dirlink/secret.txt`, "utf8"),
);
attempt("node statSync link", () => fs.statSync(`${root}/safe/link`));
attempt(
  "node readdirSync dirlink",
  () => fs.readdirSync(`${root}/safe/dirlink`),
);
attempt("node realpathSync link", () => fs.realpathSync(`${root}/safe/link`));
attempt(
  "node existsSync link",
  () => String(fs.existsSync(`${root}/safe/link`)),
);
attempt(
  "node openSync link",
  () => fs.closeSync(fs.openSync(`${root}/safe/link`, "r")),
);
