// A dependency of the main module.
export function withNothing() {
  return Deno.disarmResourceLimits();
}

export function withItsOwnImportMeta() {
  return Deno.disarmResourceLimits(import.meta);
}

// The op itself, handed over as a value: the argument decides, not the caller.
export const handed = Deno.disarmResourceLimits;

// What the main module chooses to give it.
export function withGiven(importMeta) {
  return Deno.disarmResourceLimits(importMeta);
}
