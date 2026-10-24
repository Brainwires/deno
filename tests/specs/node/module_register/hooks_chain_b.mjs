let label;
export function initialize(data) {
  label = data.label;
  return `initialized ${label}`;
}
export async function resolve(specifier, context, nextResolve) {
  if (specifier.startsWith("chain:")) {
    console.log(`${label} resolve ${specifier}`);
    return nextResolve(specifier + "+b", context);
  }
  return nextResolve(specifier, context);
}
