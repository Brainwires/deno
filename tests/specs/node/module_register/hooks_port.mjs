let port;
export function initialize(data) {
  port = data.port;
  port.postMessage("initialize");
}
export async function resolve(specifier, context, nextResolve) {
  if (specifier === "node:os") port.postMessage(`resolve ${specifier}`);
  return nextResolve(specifier, context);
}
