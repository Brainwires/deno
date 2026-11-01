// Only the main module's own import.meta disarms the limits, and only while it
// evaluates: everything else gets NotCapable and the limits stay.
import vm from "node:vm";
import { withItsOwnImportMeta, withNothing } from "./disarm_dep.js";

function attempt(name, fn) {
  try {
    console.log(name, fn());
  } catch (e) {
    console.log(name, e.name);
  }
}

attempt("nothing", () => Deno.disarmResourceLimits());
attempt("undefined", () => Deno.disarmResourceLimits(undefined));
attempt("string", () => Deno.disarmResourceLimits(import.meta.url));
attempt("dependency, nothing", () => withNothing());
attempt("dependency, its own", () => withItsOwnImportMeta());
// Nothing made from the main module's object is that object.
attempt(
  "look-alike",
  () => Deno.disarmResourceLimits({ url: import.meta.url, main: true }),
);
attempt("spread", () => Deno.disarmResourceLimits({ ...import.meta }));
attempt(
  "inheriting",
  () => Deno.disarmResourceLimits(Object.create(import.meta)),
);
attempt("proxy", () => Deno.disarmResourceLimits(new Proxy(import.meta, {})));
attempt("eval", () => eval("Deno.disarmResourceLimits()"));
attempt("function", () => new Function("return Deno.disarmResourceLimits()")());
attempt(
  "vm",
  () =>
    vm.runInThisContext("Deno.disarmResourceLimits()", {
      filename: import.meta.url,
    }),
);
const worker = new Worker(new URL("./disarm_worker.js", import.meta.url), {
  type: "module",
});
await new Promise((resolve) => {
  worker.onmessage = (e) => {
    console.log("worker", e.data);
    worker.terminate();
    resolve();
  };
});
setTimeout(() => {
  // The main module has finished evaluating.
  attempt("after evaluation", () => Deno.disarmResourceLimits(import.meta));
  // Still armed: --max-time ends the program.
  setTimeout(() => console.log("not reached"), 60_000);
}, 0);
