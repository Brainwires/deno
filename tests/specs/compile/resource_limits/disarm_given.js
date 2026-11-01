// A main module that hands out its import.meta has decided: what it gave it to
// holds the capability while the main module evaluates.
import { withGiven } from "./disarm_dep.js";

console.log("given", withGiven(import.meta));
// The limits are lifted: --max-time=1 would have ended this.
await new Promise((resolve) => setTimeout(resolve, 1500));
console.log("done");
