export async function resolve(specifier, context, nextResolve) {
  if (specifier === "virtual:hello") {
    return { url: "virtual:hello", shortCircuit: true };
  }
  if (specifier === "incomplete") {
    // Neither calls nextResolve nor signals a short circuit.
    return { url: new URL("./math.mjs", import.meta.url).href };
  }
  return nextResolve(specifier, context);
}
export async function load(url, context, nextLoad) {
  if (url === "virtual:hello") {
    return {
      format: "module",
      source: "export default 'hello from a virtual module';",
      shortCircuit: true,
    };
  }
  return nextLoad(url, context);
}
