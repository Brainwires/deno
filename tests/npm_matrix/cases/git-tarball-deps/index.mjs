import isNumber from "is-number";
import kindOf from "kind-of";
import leftPad from "left-pad";
import { readFileSync } from "node:fs";

if (!isNumber(5) || isNumber("x")) throw new Error("is-number");
if (kindOf([]) !== "array") throw new Error("kind-of");
if (leftPad("1", 3, "0") !== "001") throw new Error("left-pad");
const version = JSON.parse(
  readFileSync(new URL(import.meta.resolve("kind-of/package.json"))),
).version;
if (!version.startsWith("6.")) throw new Error(`kind-of ${version}`);
console.log("SMOKE_OK");
