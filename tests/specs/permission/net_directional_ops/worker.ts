// Reports whether this worker may listen on and connect to 127.0.0.1.
async function attempt(fn: () => unknown): Promise<string> {
  try {
    await fn();
    return "allowed";
  } catch (e) {
    const m = /Requires (net(?: connect| listen)?) access/.exec(
      (e as Error).message,
    );
    return m ? `denied (${m[1]})` : "allowed";
  }
}
const listen = await attempt(() =>
  Deno.listen({ hostname: "127.0.0.1", port: 0 }).close()
);
const connect = await attempt(() =>
  Deno.connect({ hostname: "127.0.0.1", port: 1 })
);
self.postMessage(`listen ${listen}, connect ${connect}`);
