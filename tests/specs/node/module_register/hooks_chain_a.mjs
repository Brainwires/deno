let label;
export function initialize(data) {
  label = data.label;
  return `initialized ${label}`;
}
export async function resolve(specifier, context, nextResolve) {
  if (specifier.startsWith("chain:")) {
    console.log(`${label} resolve ${specifier}`);
    return { url: "chain:" + specifier.slice(6) + "+a", shortCircuit: true };
  }
  return nextResolve(specifier, context);
}
export async function load(url, context, nextLoad) {
  if (url.startsWith("chain:")) {
    return {
      format: "module",
      source: `export default ${JSON.stringify(url)};`,
      shortCircuit: true,
    };
  }
  return nextLoad(url, context);
}
