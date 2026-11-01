// A dependency can't disarm the limits, with its own import.meta or with
// nothing, however the op is reached; the main module's own import.meta can.
import { handed, withItsOwnImportMeta, withNothing } from "./disarm_dep.js";

function attempt(name, fn) {
  try {
    console.log(name, fn());
  } catch (e) {
    console.log(name, e.name);
  }
}

attempt("dependency, nothing", () => withNothing());
attempt("dependency, its own", () => withItsOwnImportMeta());
attempt("handed, nothing", () => handed());
console.log("main module", Deno.disarmResourceLimits(import.meta));
// The limits are lifted: --max-time=1 would have ended this.
await new Promise((resolve) => setTimeout(resolve, 1500));
console.log("done");
