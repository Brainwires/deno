// A dependency of the main module.
export function fromDependency() {
  return Deno.disarmResourceLimits();
}
