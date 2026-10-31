// Only the main module's own code may disarm the limits, and only while it
// evaluates: every other caller gets NotCapable and the limits stay.
import vm from "node:vm";
import { fromDependency } from "./disarm_dep.js";

function attempt(name, fn) {
  try {
    console.log(name, fn());
  } catch (e) {
    console.log(name, e.name);
  }
}

attempt("dependency", () => fromDependency());
attempt("eval", () => eval("Deno.disarmResourceLimits()"));
attempt("function", () => new Function("return Deno.disarmResourceLimits()")());
// A script named after the main module is still another script.
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
  attempt("after evaluation", () => Deno.disarmResourceLimits());
  // Still armed: --max-time ends the program.
  setTimeout(() => console.log("not reached"), 60_000);
}, 0);
