import { register } from "node:module";

// Hooks chain last-registered first: b runs, then calls into a.
console.log(register("./hooks_chain_a.mjs", import.meta.url, {
  data: { label: "a" },
}));
console.log(register(new URL("./hooks_chain_b.mjs", import.meta.url), {
  parentURL: import.meta.url,
  data: { label: "b" },
}));
console.log((await import("chain:x")).default);
