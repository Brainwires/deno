// A compiled program resolving from a host directory whose node_modules/pkg
// is a link into a --deny-read scope.
import { createRequire } from "node:module";

const cwd = Deno.cwd();
Deno.mkdirSync(`${cwd}/host/node_modules`, { recursive: true });
try {
  Deno.removeSync(`${cwd}/host/node_modules/pkg`);
} catch {
  // not there yet
}
Deno.symlinkSync(`${cwd}/denied`, `${cwd}/host/node_modules/pkg`);
const require = createRequire(`${cwd}/host/main.cjs`);
let text;
try {
  text = require.resolve("pkg/sub");
} catch (error) {
  text = String((error as Error).message);
}
console.log(
  `compiled require.resolve: ${
    text.includes("LEAKED_VALUE") ? "seen" : "hidden"
  }`,
);
