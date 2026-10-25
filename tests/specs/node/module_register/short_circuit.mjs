import { register } from "node:module";

register("./hooks_short_circuit.mjs", import.meta.url);
console.log((await import("virtual:hello")).default);
try {
  await import("incomplete");
} catch (err) {
  console.log(err.message.includes("did not call the next hook in its chain"));
}
