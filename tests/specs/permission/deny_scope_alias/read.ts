// Run with `--allow-read --deny-read=rootlink/secret,dlink`: the protected
// files must be denied by every name, not only the one the deny scope uses.
import fs from "node:fs";
import { attempt } from "./attempt.ts";

attempt("as written", () => Deno.readTextFileSync("rootlink/secret/file.txt"));
attempt(
  "by the real name",
  () => Deno.readTextFileSync("real/secret/file.txt"),
);
attempt("stat by the real name", () => Deno.statSync("real/secret/file.txt"));
attempt("readDir by the real name", () => [...Deno.readDirSync("real/secret")]);
attempt(
  "node readFileSync by the real name",
  () => fs.readFileSync("real/secret/file.txt"),
);
attempt(
  "through another link",
  () => Deno.readTextFileSync("other/via/file.txt"),
);
attempt(
  "link scope, as written",
  () => Deno.readTextFileSync("dlink/file.txt"),
);
attempt(
  "link scope, target by the real name",
  () => Deno.readTextFileSync("real/secret2/file.txt"),
);
attempt("unrelated file", () => Deno.readTextFileSync("real/ok.txt"));
