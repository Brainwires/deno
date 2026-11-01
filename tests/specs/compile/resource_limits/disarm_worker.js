try {
  postMessage(String(Deno.disarmResourceLimits(import.meta)));
} catch (e) {
  postMessage(e.name);
}
