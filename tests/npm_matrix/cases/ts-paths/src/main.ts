// tsconfig.json `paths` and extensionless relative imports, as a tsx or
// ts-node (tsconfig-paths) project writes them
import { greet } from "@/lib/greet";
import { twice } from "./lib";
import { shout } from "./lib/shout.js";

if (greet("deno") !== "hello deno") throw new Error("paths");
if (twice(2) !== 4) throw new Error("directory import");
if (shout("ok") !== "OK!") throw new Error(".js for .ts");
console.log("SMOKE_OK");
