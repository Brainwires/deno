import { readFileSync } from "node:fs";

export async function load(url, context, nextLoad) {
  if (url.endsWith("/math.mjs")) {
    // Hooks run with the process's permissions: --allow-read=. does not
    // cover the parent directory.
    readFileSync("../outside_of_allowed_dir.txt");
  }
  return nextLoad(url, context);
}
