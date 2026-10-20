// Runs one network operation at a time and prints whether the permission
// check let it through: "allowed" (it succeeded, or failed for another
// reason, such as a refused connection), or "denied (<permission>)" with the
// permission the NotCapable error names ("net", "net connect" or
// "net listen"; a Node API's errno where it reports one instead). Node APIs report errors through callbacks and events, and
// some asynchronously, so uncaught errors and rejections during a probe count
// too. An operation still pending after a while passed its check.

type Probe = (fail: (e: unknown) => void) => unknown;

let current: ((result: string) => void) | null = null;

function denial(e: unknown): string | null {
  // fetch wraps the error in `cause`
  for (let cur = e, i = 0; cur != null && i < 5; i++) {
    const message = cur instanceof Error ? cur.message : String(cur);
    const m = /Requires (net(?: connect| listen)?) access/.exec(message);
    if (m) return `denied (${m[1]})`;
    cur = (cur as { cause?: unknown }).cause;
  }
  // Node's dns.lookup reports a refused check as EPERM and dgram's send as
  // UNKNOWN (an errno, as Node would), without the permission's name.
  const code = (e as { code?: unknown } | null)?.code;
  if (code === "EPERM" || code === "UNKNOWN") return `denied (${code})`;
  return null;
}

globalThis.addEventListener("error", (event) => {
  event.preventDefault();
  current?.(denial(event.error) ?? "allowed");
});
globalThis.addEventListener("unhandledrejection", (event) => {
  event.preventDefault();
  current?.(denial(event.reason) ?? "allowed");
});

export async function probe(name: string, fn: Probe) {
  const result = await new Promise<string>((resolve) => {
    let settled = false;
    const done = (result: string) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(result);
    };
    const fail = (e: unknown) => done(denial(e) ?? "allowed");
    const timer = setTimeout(() => done("allowed"), 1500);
    current = done;
    try {
      Promise.resolve(fn(fail)).then(() => done("allowed"), fail);
    } catch (e) {
      fail(e);
    }
  });
  current = null;
  console.log(`${name}: ${result}`);
}

// Resolves when `emitter` emits `ok`, rejects on "error".
export function settle(
  emitter: {
    once(event: string, listener: (...args: unknown[]) => void): unknown;
  },
  ok: string,
): Promise<void> {
  return new Promise((resolve, reject) => {
    emitter.once(ok, () => resolve());
    emitter.once("error", (e) => reject(e));
  });
}
