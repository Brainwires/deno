import { register, registerHooks } from "node:module";

register("./hooks_log.mjs", import.meta.url);
// In-thread hooks run before the off-thread ones, whatever the order of
// registration.
const hooks = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === "./math.mjs") console.log("sync resolve " + specifier);
    return nextResolve(specifier, context);
  },
});
console.log((await import("./math.mjs")).default);
hooks.deregister();
