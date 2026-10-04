// Copyright 2018-2026 the Deno authors. MIT license.

// This is not a bootstrap module: cli/rt/run.rs evaluates it with
// `execute_script` after bootstrap (like the rest of the `Deno.desktop` JS in
// cli/rt), when `__bootstrap.primordials` is no longer reachable, and before
// the app's main module runs. The console call reports an exception thrown by
// the app's own onProgress callback.
// deno-lint-ignore-file deno-internal/prefer-primordials no-console

// Deno.desktop.updater: full-app self-update (see desktop_update/mod.rs).
// Every trust decision is made in Rust; this file only moves bytes: it
// fetches the manifest and the archive (https only, every redirect hop
// checked) and streams the archive into the size-capped, hashing sink.
(() => {
  const core = Deno[Deno.internal].core;
  const {
    op_desktop_app_update_info,
    op_desktop_app_update_check,
    op_desktop_app_update_begin,
    op_desktop_app_update_write,
    op_desktop_app_update_finish,
    op_desktop_app_update_abort,
    op_desktop_app_update_stage,
    op_desktop_app_update_apply,
    op_desktop_app_update_confirm,
  } = core.ops;
  const desktop = globalThis.Deno?.desktop;
  if (!desktop || typeof op_desktop_app_update_info !== "function") return;

  const MAX_MANIFEST_BYTES = 1024 * 1024;
  const MAX_REDIRECTS = 5;
  const DEFAULT_CHECK_TIMEOUT_MS = 30_000;

  class AppUpdateError extends Error {
    constructor(code, message) {
      super(message);
      this.name = "AppUpdateError";
      this.code = code;
    }
  }

  // Ops throw "<code>: <message>"; surface the code as a property.
  function rethrow(e) {
    const m = /^([a-z_]+): ([\s\S]*)$/.exec(e?.message ?? "");
    if (m) throw new AppUpdateError(m[1], m[2]);
    throw e;
  }
  function call(fn, ...args) {
    try {
      return fn(...args);
    } catch (e) {
      rethrow(e);
    }
  }

  function isLoopback(url) {
    const h = url.hostname;
    return h === "localhost" || h === "127.0.0.1" || h === "[::1]" ||
      h === "::1" || /^127\.\d+\.\d+\.\d+$/.test(h);
  }

  function checkUrl(raw, insecure) {
    let url;
    try {
      url = new URL(raw);
    } catch {
      throw new AppUpdateError("invalid_manifest", `invalid url ${raw}`);
    }
    if (url.username || url.password) {
      throw new AppUpdateError(
        "insecure_url",
        "the url must not carry credentials",
      );
    }
    if (url.protocol === "https:") return url;
    if (url.protocol === "http:" && insecure && isLoopback(url)) return url;
    throw new AppUpdateError(
      "insecure_url",
      `refusing ${raw}: updates download over https only (http to a ` +
        "loopback host needs the dev-only allowInsecureLoopback option)",
    );
  }

  function clientFor(options) {
    const caCerts = options?.caCerts;
    if (Array.isArray(caCerts) && caCerts.length > 0) {
      return Deno.createHttpClient({ caCerts: [...caCerts] });
    }
    return undefined;
  }

  // GET `raw`, following at most MAX_REDIRECTS redirects, each hop checked.
  async function get(raw, options, signal, client) {
    const insecure = options?.allowInsecureLoopback === true;
    let url = checkUrl(raw, insecure);
    for (let hop = 0;; hop++) {
      const init = { cache: "no-store", redirect: "manual", signal };
      if (client) init.client = client;
      const resp = await fetch(url, init);
      if (resp.status >= 300 && resp.status < 400) {
        const location = resp.headers.get("location");
        await resp.body?.cancel();
        if (!location || hop >= MAX_REDIRECTS) {
          throw new AppUpdateError("io", `too many redirects from ${raw}`);
        }
        url = checkUrl(new URL(location, url).href, insecure);
        continue;
      }
      if (!resp.ok) {
        await resp.body?.cancel();
        throw new AppUpdateError("io", `HTTP ${resp.status} for ${url.href}`);
      }
      return resp;
    }
  }

  const updater = new EventTarget();

  function emitProgress(transferred, total, onProgress) {
    const detail = { transferred, total };
    updater.dispatchEvent(new CustomEvent("progress", { detail }));
    if (typeof onProgress === "function") {
      try {
        onProgress(detail);
      } catch (e) {
        console.error("Deno.desktop.updater onProgress threw:", e);
      }
    }
  }

  // Fetch and verify the signed manifest at `manifestUrl`.
  async function check(manifestUrl, options = {}) {
    const controller = new AbortController();
    const timeout = setTimeout(
      () => controller.abort(),
      options.timeoutMs ?? DEFAULT_CHECK_TIMEOUT_MS,
    );
    const client = clientFor(options);
    let bytes;
    try {
      const resp = await get(
        String(manifestUrl),
        options,
        controller.signal,
        client,
      );
      const chunks = [];
      let size = 0;
      const reader = resp.body.getReader();
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > MAX_MANIFEST_BYTES) {
          await reader.cancel();
          throw new AppUpdateError(
            "invalid_manifest",
            `the manifest is larger than ${MAX_MANIFEST_BYTES} bytes`,
          );
        }
        chunks.push(value);
      }
      bytes = new Uint8Array(size);
      let at = 0;
      for (const c of chunks) {
        bytes.set(c, at);
        at += c.byteLength;
      }
    } catch (e) {
      if (e instanceof AppUpdateError) throw e;
      throw new AppUpdateError(
        "io",
        controller.signal.aborted
          ? "the manifest request timed out"
          : `the manifest request failed: ${e?.message ?? e}`,
      );
    } finally {
      clearTimeout(timeout);
      client?.close();
    }
    const out = call(
      op_desktop_app_update_check,
      bytes,
      options.allowInsecureLoopback === true,
    );
    return {
      available: out.available,
      version: out.version,
      currentVersion: out.currentVersion,
      required: out.update?.required ?? false,
      releaseNotes: out.update?.releaseNotes ?? null,
      publishedAt: out.update?.publishedAt ?? null,
      size: out.update?.size ?? null,
      platform: out.update?.platform ?? null,
      sequence: out.update?.sequence ?? null,
      expiresAt: out.update?.expiresAt ?? null,
    };
  }

  // Stream the verified update's archive into the staging sink.
  async function download(options = {}) {
    const begin = call(op_desktop_app_update_begin);
    const controller = new AbortController();
    const signal = options.signal;
    const onAbort = () => controller.abort();
    signal?.addEventListener("abort", onAbort);
    const client = clientFor(options);
    let reader;
    try {
      const resp = await get(begin.url, options, controller.signal, client);
      const length = Number(resp.headers.get("content-length"));
      if (Number.isFinite(length) && length > begin.size) {
        await resp.body?.cancel();
        throw new AppUpdateError(
          "size_exceeded",
          `the server sends ${length} bytes, the manifest declares ${begin.size}`,
        );
      }
      reader = resp.body.getReader();
      let transferred = 0;
      emitProgress(0, begin.size, options.onProgress);
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        // Throws size_exceeded (and drops the download) past the declared
        // size: the stream is cancelled below, nothing more is read.
        transferred = call(op_desktop_app_update_write, value);
        emitProgress(transferred, begin.size, options.onProgress);
      }
      try {
        await op_desktop_app_update_finish();
      } catch (e) {
        rethrow(e);
      }
      return { version: begin.version, size: begin.size };
    } catch (e) {
      try {
        await reader?.cancel();
      } catch {
        // already closed
      }
      op_desktop_app_update_abort();
      if (e instanceof AppUpdateError) throw e;
      throw new AppUpdateError(
        "io",
        `the download failed: ${e?.message ?? e}`,
      );
    } finally {
      signal?.removeEventListener("abort", onAbort);
      client?.close();
    }
  }

  async function stage(options = {}) {
    try {
      return await op_desktop_app_update_stage(
        options.allowUnsignedDev === true,
      );
    } catch (e) {
      rethrow(e);
    }
  }

  // Start the swap helper, then quit. The helper waits for this process to
  // exit, swaps the install, and relaunches the new version.
  function applyAndRelaunch(options = {}) {
    call(op_desktop_app_update_apply, false);
    const quitting = typeof desktop.quit === "function"
      ? desktop.quit() !== false
      : false;
    if (!quitting && options.force === true) {
      Deno.exit(0);
    }
    if (!quitting) {
      // The quit was refused: withdraw the request, so the waiting helper
      // stands down instead of swapping whenever the app exits later.
      call(op_desktop_app_update_apply, true);
    }
    return { quitting };
  }

  Object.defineProperties(updater, {
    check: { value: check, enumerable: true },
    download: { value: download, enumerable: true },
    stage: { value: stage, enumerable: true },
    applyAndRelaunch: { value: applyAndRelaunch, enumerable: true },
    confirm: {
      value: function confirm() {
        return call(op_desktop_app_update_confirm);
      },
      enumerable: true,
    },
    status: {
      value: function status() {
        return op_desktop_app_update_info();
      },
      enumerable: true,
    },
    AppUpdateError: { value: AppUpdateError, enumerable: false },
  });
  Object.defineProperty(desktop, "updater", {
    value: updater,
    writable: false,
    configurable: true,
    enumerable: true,
  });
})();
