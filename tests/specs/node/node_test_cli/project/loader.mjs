export function resolve(specifier, context, next) {
  if (specifier === "virtual:x") {
    return { url: specifier, shortCircuit: true };
  }
  return next(specifier, context);
}
export function load(url, context, next) {
  if (url === "virtual:x") {
    return {
      format: "module",
      source: 'export default "virtual";',
      shortCircuit: true,
    };
  }
  return next(url, context);
}
