// Run with `--allow-read --allow-write --deny-write=rootlink/secret,dlink`.
import fs from "node:fs";
import { attempt } from "./attempt.ts";

attempt(
  "write by the real name",
  () => Deno.writeTextFileSync("real/secret/new.txt", "x"),
);
attempt(
  "node writeFileSync by the real name",
  () => fs.writeFileSync("real/secret/new2.txt", "x"),
);
attempt(
  "remove by the real name",
  () => Deno.removeSync("real/secret/remove.txt"),
);
attempt(
  "link scope, target by the real name",
  () => Deno.writeTextFileSync("real/secret2/new.txt", "x"),
);
attempt("unrelated file", () => Deno.writeTextFileSync("real/ok2.txt", "x"));
// last: before the fix this removed the protected tree
attempt(
  "recursive remove of a tree holding it",
  () => Deno.removeSync("real", { recursive: true }),
);
