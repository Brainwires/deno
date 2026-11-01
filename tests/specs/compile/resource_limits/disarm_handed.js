// What the main-module check establishes, and what it does not: the main
// module's own call is what counts. A dependency that hands the op to the main
// module (a function value, a bound function, a getter the main module reads)
// adds no JavaScript frame of its own, so the main module invoking it at the
// top level passes, as a direct call would. (The dependency's own call to the
// op is refused: disarm_refused.)
import { handed } from "./disarm_dep.js";

console.log("handed", handed());
// The limits are lifted: --max-time=1 would have ended this.
await new Promise((resolve) => setTimeout(resolve, 1500));
console.log("done");
