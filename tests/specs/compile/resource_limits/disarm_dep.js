// A dependency of the main module.
export function fromDependency() {
  return Deno.disarmResourceLimits();
}

// The op itself, handed over as a value: whoever calls it is the caller the
// op sees.
export const handed = Deno.disarmResourceLimits;
