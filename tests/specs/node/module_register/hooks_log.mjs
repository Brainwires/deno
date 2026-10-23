export async function resolve(specifier, context, nextResolve) {
  if (specifier === "./math.mjs") console.log("async resolve " + specifier);
  return nextResolve(specifier, context);
}
