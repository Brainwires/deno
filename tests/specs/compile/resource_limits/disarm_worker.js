try {
  postMessage(String(Deno.disarmResourceLimits()));
} catch (e) {
  postMessage(e.name);
}
