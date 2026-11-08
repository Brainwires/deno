// Copyright 2018-2026 the Deno authors. MIT license.
// runtime/ops/desktop_update/updater.js's download loop, without a runtime:
// the script is evaluated against fake ops and a fake fetch.
//
//   deno test -A --no-config --no-lock .github/denext-runtime/e2e/lib/

// deno-lint-ignore-file no-explicit-any camelcase -- the ops keep their Rust names

const SOURCE = await Deno.readTextFile(
  new URL(
    "../../../../runtime/ops/desktop_update/updater.js",
    import.meta.url,
  ),
);

function assert(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg);
}

interface Harness {
  updater: any;
  calls: string[];
  written: number;
}

/** Evaluates updater.js with `fetch` answering every request with `body`. */
function load(body: () => ReadableStream<Uint8Array>): Harness {
  const h: Harness = { updater: undefined, calls: [], written: 0 };
  const ops = {
    op_desktop_app_update_info: () => ({}),
    op_desktop_app_update_check: () => ({}),
    op_desktop_app_update_begin: () => {
      h.calls.push("begin");
      return {
        url: "https://updates.example/app.tar.gz",
        size: 12,
        version: "2.0.0",
      };
    },
    op_desktop_app_update_write: (chunk: Uint8Array) => {
      h.written += chunk.byteLength;
      return h.written;
    },
    op_desktop_app_update_finish: () => {
      h.calls.push("finish");
      return Promise.resolve();
    },
    op_desktop_app_update_abort: () => {
      h.calls.push("abort");
    },
    op_desktop_app_update_stage: () => Promise.resolve({}),
    op_desktop_app_update_apply: () => {},
    op_desktop_app_update_confirm: () => {},
  };
  const internal = Symbol("internal");
  const desktop: any = {};
  const fakeDeno: any = {
    internal,
    [internal]: { core: { ops } },
    desktop,
  };
  const fakeFetch = (_url: URL, init: { signal?: AbortSignal }) => {
    const src = body();
    const reader = src.getReader();
    // A body that honours the request's signal, as fetch's does.
    const stream = new ReadableStream<Uint8Array>({
      async pull(c) {
        const abort = new Promise<never>((_, reject) => {
          if (init.signal?.aborted) reject(init.signal.reason);
          init.signal?.addEventListener(
            "abort",
            () => reject(init.signal!.reason),
            { once: true },
          );
        });
        const r = await Promise.race([reader.read(), abort]);
        if (r.done) c.close();
        else c.enqueue(r.value);
      },
      cancel() {
        reader.cancel().catch(() => {});
      },
    });
    return Promise.resolve(new Response(stream, { status: 200 }));
  };
  new Function("Deno", "globalThis", "fetch", SOURCE)(
    fakeDeno,
    { Deno: fakeDeno },
    fakeFetch,
  );
  h.updater = desktop.updater;
  return h;
}

Deno.test("download: a body that stalls fails with a stall error", async () => {
  // one chunk, then nothing, ever
  const h = load(() =>
    new ReadableStream({
      start(c) {
        c.enqueue(new Uint8Array(4));
      },
    })
  );
  const started = Date.now();
  let error: any;
  try {
    await h.updater.download({ stallTimeoutMs: 50 });
  } catch (e) {
    error = e;
  }
  assert(error, "the download did not fail");
  assert(error.name === "AppUpdateError", `name: ${error.name}`);
  assert(error.code === "io", `code: ${error.code}`);
  assert(
    /stalled: no data for 50 ms/.test(error.message),
    `message: ${error.message}`,
  );
  assert(Date.now() - started < 5_000, "the stall timer did not fire");
  assert(h.written === 4, `written: ${h.written}`);
  assert(
    JSON.stringify(h.calls) === JSON.stringify(["begin", "abort"]),
    `calls: ${h.calls}`,
  );
});

Deno.test("download: slow chunks inside the stall timeout complete", async () => {
  // six chunks 40 ms apart (240 ms in all) under a 100 ms stall timeout:
  // each chunk resets it
  const h = load(() => {
    let n = 0;
    return new ReadableStream({
      async pull(c) {
        await new Promise((r) => setTimeout(r, 40));
        if (n++ === 6) c.close();
        else c.enqueue(new Uint8Array(2));
      },
    });
  });
  const out = await h.updater.download({ stallTimeoutMs: 100 });
  assert(out.version === "2.0.0" && out.size === 12, JSON.stringify(out));
  assert(h.written === 12, `written: ${h.written}`);
  assert(
    JSON.stringify(h.calls) === JSON.stringify(["begin", "finish"]),
    `calls: ${h.calls}`,
  );
});
