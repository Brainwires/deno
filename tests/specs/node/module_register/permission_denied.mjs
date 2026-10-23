import { register } from "node:module";

register("./hooks_read_outside.mjs", import.meta.url);
try {
  await import("./math.mjs");
  console.log("loaded");
} catch (err) {
  console.log(/Requires read access/.test(String(err?.message ?? err)));
}
