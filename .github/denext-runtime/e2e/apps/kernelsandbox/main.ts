// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: a packaged desktop app under the kernel sandbox (DENEXT.md item 489).
// The runner packages it with `--kernel-sandbox=enforce` and a scoped grant
// (strict), tells it what this leg's engine and host make of that
// (`expect`), and each launch reports:
//
// - what the capability probe says holds the app, and why;
// - a native write (FFI into libc: only the kernel holds it) inside and
//   outside the grant;
// - a link opened through `Deno.desktop.openExternal`;
// - one row per integration that must keep working inside the sandbox:
//   file dialogs, notifications, the tray, deep links, the secure store and
//   a sidecar (a fork of the app). The updater's swap is the runner's row.

// deno-lint-ignore-file no-explicit-any

import { fork } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  describeError,
  desktop,
  errorOf,
  html,
  OS,
  page,
  Report,
  shared,
  sleep,
  titledWindow,
  within,
} from "../_shared/e2e.ts";

const r = new Report("kernelsandbox");
Deno.serve((req) => shared(req) ?? html(page("e2e kernel sandbox")));
const win = titledWindow("E2E Kernel Sandbox");
await sleep(2500);

const p = r.params as {
  /** What the probe must say: "kernel", "headless-only" or "none". On a
   * Linux CEF leg with a host that calls the runtime first it is
   * "machine": kernel where Chromium's namespace sandbox works, else
   * headless-only. */
  expect: "kernel" | "headless-only" | "none" | "machine";
  /** A part the reason must contain (headless-only legs). */
  reason?: string;
  inside: string;
  outside: string;
  outsideChild: string;
  openUrl?: string;
  childOpenUrl?: string;
  scheme: string;
};

/** A native file creation: "written" or "refused". `creat` and not `open`,
 * whose mode is a variadic argument (FFI can't pass one on Apple arm64). */
function nativeCreate(path: string): string {
  try {
    const lib = Deno.dlopen(OS === "darwin" ? "libSystem.dylib" : "libc.so.6", {
      creat: { parameters: ["buffer", "u32"], result: "i32" },
      close: { parameters: ["i32"], result: "i32" },
    });
    const fd = lib.symbols.creat(
      new TextEncoder().encode(`${path}\0`),
      0o600,
    );
    if (fd >= 0) lib.symbols.close(fd);
    lib.close();
    return fd >= 0 ? "written" : "refused";
  } catch (e) {
    return `error: ${describeError(e)}`;
  }
}

let kernel = false;
let features: any = null;

await r.step("probe", async () => {
  features = await desktop.platformFeatures();
  const sandbox = features?.kernelSandbox;
  const reason = features?.kernelSandboxReason;
  kernel = sandbox === "kernel";
  r.set("kernelSandbox", {
    sandbox,
    reason,
    chromiumSandbox: features?.sandbox ?? null,
    chromiumSandboxReason: features?.sandboxReason ?? null,
  });
  r.check(
    "the probe reports kernel, headless-only or none, with a reason",
    ["kernel", "headless-only", "none"].includes(sandbox) &&
      typeof reason === "string" && reason.length > 0,
    { sandbox, reason },
  );
  if (p.expect === "machine") {
    // Linux CEF: confined exactly where web content keeps Chromium's
    // namespace sandbox; with only the setuid helper the app's own process
    // is left alone and the probe says so.
    const namespace = features?.sandbox === "namespace";
    r.check(
      "linux_cef: kernel-held exactly when Chromium's namespace sandbox runs",
      kernel === namespace,
      { sandbox, chromium: features?.sandbox },
    );
    if (!kernel) {
      r.check(
        "linux_cef_setuid_only_reported",
        sandbox === "headless-only" && /setuid|namespace sandbox/.test(reason),
        { sandbox, reason },
      );
    }
  } else {
    r.check(
      `the app is ${p.expect}`,
      sandbox === p.expect &&
        (p.reason === undefined || String(reason).includes(p.reason)),
      { sandbox, reason, want: p.reason },
    );
  }
  if (p.reason?.includes("CEF on macOS")) {
    // The app runs (this code does), unconfined, and says why.
    r.check(
      "macos_cef_headless_only_reported",
      sandbox === "headless-only" && String(reason).includes("CEF on macOS"),
      { sandbox, reason },
    );
  }
});

await r.step("native writes", () => {
  if (OS === "windows") {
    // No enforcer for desktop apps there (the probe says none), and no
    // libc to write through: nothing to measure.
    r.check("windows: the app is not kernel-held", !kernel);
    return;
  }
  const inside = nativeCreate(p.inside);
  const outside = nativeCreate(p.outside);
  r.set("native", { inside, outside });
  r.check(
    "a native write inside the grant works",
    inside === "written",
    inside,
  );
  // What Deno's own checks refuse either way; only the kernel holds FFI.
  r.check(
    "strict_native_write_refused: a native write outside the grant is refused exactly when the kernel holds the app",
    kernel ? outside === "refused" : outside === "written",
    { kernel, outside },
  );
});

await r.step("openExternal", async () => {
  r.check(
    "Deno.desktop.openExternal exists",
    typeof desktop.openExternal === "function",
  );
  const bad = await errorOf(
    Promise.resolve().then(() => desktop.openExternal("file:///etc/passwd")),
  );
  r.check(
    "openExternal refuses a URL that is not http, https or mailto",
    bad instanceof TypeError,
    describeError(bad),
  );
  if (p.openUrl) {
    // The runner listens at this URL and reports whether a browser came.
    const started = desktop.openExternal(p.openUrl);
    r.set("openExternal", started);
    r.check(
      "open_external_still_works: openExternal starts the system opener",
      started === true,
      started,
    );
  }
  if (p.childOpenUrl && kernel && OS === "darwin") {
    // The same opener as a child process inherits the sandbox, which
    // refuses LaunchServices: nothing opens (the runner checks no browser
    // came for this URL). This is what openExternal's helper is for.
    try {
      const out = await new Deno.Command("/usr/bin/open", {
        args: [p.childOpenUrl],
        stdout: "null",
        stderr: "piped",
      }).output();
      r.set("childOpen", {
        code: out.code,
        stderr: new TextDecoder().decode(out.stderr).slice(-400),
      });
      r.check(
        "a child's `open` is refused inside the sandbox (LaunchServices)",
        out.code !== 0,
        out.code,
      );
    } catch (e) {
      r.set("childOpen", describeError(e));
    }
  }
});

await r.step("row: file dialogs", async () => {
  const caps = desktop.windowCapabilities();
  if (!caps.fileDialogs) {
    r.check(
      "file dialogs: none in this session, showOpenDialog rejects",
      (await errorOf(desktop.dialog.showOpenDialog({}))) !== null,
    );
    return;
  }
  const ac = new AbortController();
  const opened = errorOf(desktop.dialog.showOpenDialog(win, {
    title: "e2e",
    properties: ["openFile"],
    signal: ac.signal,
  }));
  await sleep(4000);
  ac.abort();
  const settled = await within(opened, 60_000);
  r.check(
    "file dialogs: an open dialog shows and aborts inside the sandbox",
    "value" in settled &&
      (settled.value as Error | null)?.name === "AbortError",
    "value" in settled ? describeError(settled.value) : "never settled",
  );
});

await r.step("row: notifications", async () => {
  const N = (globalThis as any).Notification;
  if (typeof N !== "function") {
    r.fail("notifications: no Notification class");
    return;
  }
  const permission = await within(
    Promise.resolve(N.requestPermission?.() ?? N.permission),
    20_000,
  );
  r.set("notificationPermission", permission);
  const shown = await errorOf(
    Promise.resolve().then(() => {
      const n = new N("e2e kernel sandbox", { body: "row" });
      n.close?.();
    }),
  );
  r.check(
    "notifications: one is posted from inside the sandbox",
    shown === null,
    describeError(shown),
  );
});

await r.step("row: tray", () => {
  let tray: any = null;
  try {
    tray = new (Deno as any).Tray();
  } catch (e) {
    r.check(
      "tray: without a tray host new Deno.Tray() throws NotSupported, as outside the sandbox",
      features?.trayHost === false && e instanceof Deno.errors.NotSupported,
      describeError(e),
    );
    return;
  }
  r.check("tray: a tray icon is created inside the sandbox", tray !== null);
  tray.destroy();
});

await r.step("row: deep links", () => {
  // The runner starts this launch with a link of the app's scheme.
  r.set("launchUrls", desktop.launchUrls ?? null);
  r.check(
    "deep links: the launch link is delivered inside the sandbox",
    Array.isArray(desktop.launchUrls) &&
      desktop.launchUrls.some((u: string) => u.startsWith(`${p.scheme}://`)),
    desktop.launchUrls,
  );
});

await r.step("row: secure store", async () => {
  const store = desktop.secureStore;
  if (!store?.supported) {
    r.check(
      "secure store: not supported in this session, as outside the sandbox",
      store?.supported === false,
      store?.supported,
    );
    return;
  }
  const service = "dev.denext.e2e.kernelsandbox";
  const outcome = async (f: () => Promise<unknown>) => {
    try {
      return { value: await f() };
    } catch (e) {
      return { name: (e as Error).name, message: (e as Error).message };
    }
  };
  const set = await outcome(() =>
    store.set(service, "row", "v1", { timeout: 20000 })
  );
  const got = await outcome(() =>
    store.get(service, "row", { timeout: 20000 })
  );
  await outcome(() => store.delete(service, "row", { timeout: 20000 }));
  r.set("secureStore", { set, got });
  // A session whose keyring can't be written refuses the same way with and
  // without the sandbox: what must hold is that a value that was stored
  // reads back, and a refusal is the store's own (never a sandbox error).
  r.check(
    "secure store: a stored item reads back, or the store refuses as it does outside the sandbox",
    ("value" in set && "value" in got && got.value === "v1") ||
      (!("value" in set) && set.name === "SecureStoreUnavailable"),
    { set, got },
  );
});

await r.step("row: sidecars", async () => {
  const module = fileURLToPath(new URL("./fork_child.js", import.meta.url));
  const outcome = await new Promise<
    { code: number | null; message: any; stderr: string }
  >(
    (resolve) => {
      let message: any = null;
      let stderr = "";
      const child = fork(module, [], {
        stdio: ["ignore", "ignore", "pipe", "ipc"],
        env: {
          ...Deno.env.toObject(),
          E2E_KS_CHILD: JSON.stringify({ outside: p.outsideChild }),
        },
      });
      child.stderr?.on("data", (d: Uint8Array) => {
        stderr += new TextDecoder().decode(d);
      });
      child.on("message", (m: unknown) => {
        message = m;
      });
      const t = setTimeout(() => {
        child.kill("SIGKILL");
        resolve({ code: null, message, stderr: `${stderr} (timed out)` });
      }, 60_000);
      child.on("exit", (code: number | null) => {
        clearTimeout(t);
        resolve({ code, message, stderr: stderr.slice(-2000) });
      });
    },
  );
  r.set("sidecar", outcome);
  r.check(
    "sidecars: a fork of the app runs and answers over IPC inside the sandbox",
    outcome.code === 0 && outcome.message?.ok === true,
    outcome,
  );
  // A fork is held too: by the app's sandbox when the app has one, and by
  // its own where an enforcer exists and the app is headless-only.
  r.check(
    "sidecars: a fork's native write outside the grant is refused when the app is kernel-held",
    !kernel || outcome.message?.native === "refused",
    outcome.message,
  );
});

r.finish();
