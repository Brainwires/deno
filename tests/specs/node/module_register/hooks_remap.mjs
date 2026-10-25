export async function resolve(specifier, context, nextResolve) {
  if (specifier === "virtual-math") {
    return nextResolve("./math.mjs", context);
  }
  return nextResolve(specifier, context);
}
