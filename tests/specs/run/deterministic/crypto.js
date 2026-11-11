// Whether the cryptographic random sources drew the values a process seeded
// with 1 draws first: `--seed=1` seeds them (as upstream's `--seed` does),
// `--deterministic` never does, whatever its seed.
import { randomBytes } from "node:crypto";

const seeded = [
  ["getRandomValues", "97,24,48,211,100,26,104,249"],
  ["randomUUID", "4a690dcc-25d1-44b0-9ac9-48325ac18f6d"],
  ["randomBytes", "d32564371735f32c"],
];
const drawn = [
  crypto.getRandomValues(new Uint8Array(8)).join(","),
  crypto.randomUUID(),
  randomBytes(8).toString("hex"),
];
seeded.forEach(([name, value], i) => {
  console.log(`${name}: ${drawn[i] === value ? "seeded" : "random"}`);
});
console.log(Math.random());
