// Copyright 2018-2026 the Deno authors. MIT license.
// A packaged desktop app under the kernel sandbox (DENEXT.md item 488):
// packaged strict (`--kernel-sandbox=enforce`, a scoped grant) with the
// fork's CLI, launched on this leg's engine. What the app must report
// follows from the engine, the OS and the host:
//
// | leg | probe |
// | --- | --- |
// | macOS, webview | kernel |
// | macOS, CEF | headless-only (`macos_cef_headless_only_reported`) |
// | Linux, a host that calls the runtime before its engine, webview | kernel |
// | the same, CEF | kernel where Chromium's namespace sandbox runs, else headless-only (`linux_cef_setuid_only_reported`) |
// | Linux, a host that doesn't | headless-only |
// | Windows | none |
//
// The app's checks carry the acceptance rows (`strict_native_write_refused`,
// `open_external_still_works`, and one row each for file dialogs,
// notifications, the tray, deep links, the secure store and sidecars); this
// side adds what only the outside sees: the file the native write would
// have made, whether a browser came for the opened link, and the updater's
// swap. `dev_build_off` has no row here: `deno desktop --hmr` launches its
// development build itself; the runtime's unit tests hold it
// (`kernel_sandbox::tests::auto_stage_matrix_desktop_packaged`,
// `kernel_apply::tests::a_desktop_app_that_applies_nothing_reports_none`).

import {
  type AreaReport,
  clearResults,
  type Env,
  exists,
  launchAndCollect,
  OS,
  packageApp,
  path,
  rm,
  sleep,
  writeParams,
} from "../lib/runner.ts";
import { e2eDir } from "../apps/_shared/e2e.ts";

/** Whether the laufey host of this runtime calls the runtime before it
 * starts its engine: it looks the entry point up by name, so the name is
 * in its binary. */
async function hostCallsPrestart(artifact: string, exe: string) {
  const needle = new TextEncoder().encode("laufey_runtime_prestart");
  // (the bundle's or the folder's main executable is the host)
  for (const file of [exe, artifact]) {
    let bytes: Uint8Array;
    try {
      bytes = await Deno.readFile(file);
    } catch {
      continue;
    }
    outer: for (let i = 0; i + needle.length <= bytes.length; i++) {
      for (let j = 0; j < needle.length; j++) {
        if (bytes[i + j] !== needle[j]) continue outer;
      }
      return true;
    }
  }
  return false;
}

/** The PIDs of this user's running Safari (macOS); `null` when they can't
 * be listed, which the cleanup treats as "one may have been running". */
async function safariPids(): Promise<Set<number> | null> {
  if (OS !== "darwin") return new Set();
  try {
    const out = await new Deno.Command("/usr/bin/pgrep", {
      // exact name, this user's processes only
      args: ["-x", "-U", String(Deno.uid() ?? ""), "Safari"],
      stdout: "piped",
      stderr: "null",
    }).output();
    // 0: matches; 1: none; anything else: pgrep itself failed
    if (out.code !== 0 && out.code !== 1) return null;
    const pids = new TextDecoder().decode(out.stdout).split("\n")
      .map((line) => Number(line.trim()))
      .filter((pid) => Number.isInteger(pid) && pid > 1);
    return new Set(pids);
  } catch {
    return null;
  }
}

/** Quits the Safari this area's opened link started, by PID, and nothing
 * else. It acts only when all of this holds: no Safari of this user was
 * running before the app launched (`before` is an empty set), a browser
 * came for this run's link (`came`), and a Safari process that wasn't there
 * before is running now. A Safari that was already running, and any other
 * browser, is left exactly as it is (the link's tab stays in it): never
 * `tell application "Safari" to quit`, which would close a browser this
 * area didn't start. (What is left: another job that starts Safari in the
 * same session during this area's few minutes, with none running before,
 * shares the one process this area's link landed in.) */
async function quitSafariStartedHere(
  before: Set<number> | null,
  came: boolean,
) {
  if (OS !== "darwin" || !came || before === null || before.size > 0) return;
  const now = await safariPids();
  if (now === null) return;
  for (const pid of now) {
    // (`before` is empty here; kept so the rule reads as it is meant)
    if (before.has(pid)) continue;
    // a normal quit, addressed to that process alone
    await new Deno.Command("/usr/bin/osascript", {
      args: ["-l", "JavaScript", "-e", `Application(${pid}).quit()`],
      stdout: "null",
      stderr: "null",
    }).output().catch(() => {});
    let alive = true;
    for (let i = 0; i < 20 && alive; i++) {
      await sleep(250);
      alive = (await safariPids())?.has(pid) ?? false;
    }
    if (alive) {
      // no Automation consent on this runner, or it didn't quit: end the
      // one process this run started
      try {
        Deno.kill(pid, "SIGTERM");
      } catch {
        // gone already
      }
    }
  }
}

/** A loopback listener that records the request paths it gets. */
function listen() {
  const seen: string[] = [];
  const server = Deno.serve(
    { hostname: "127.0.0.1", port: 0, onListen: () => {} },
    (req) => {
      seen.push(new URL(req.url).pathname);
      return new Response("e2e kernel sandbox");
    },
  );
  return { seen, port: server.addr.port, close: () => server.shutdown() };
}

export async function run(env: Env, rep: AreaReport) {
  // `--kernel-sandbox` for a desktop app, and the engine it bakes, are the
  // fork CLI's (FORK_CLI_AREAS).
  if (!env.forkDeno) {
    rep.na(
      "a packaged app under the kernel sandbox",
      "packaged with the stock CLI, which takes no --kernel-sandbox for a desktop app and bakes no engine; the fork CLI's e2e run covers it",
    );
    return;
  }
  const dir = e2eDir();
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE") ?? "";
  const identifier = `dev.denext.e2e${env.nonce}.kernelsandbox`;
  const scheme = `e2eks${env.nonce}`;
  const opener = OS === "darwin"
    ? "open"
    : OS === "windows"
    ? "rundll32"
    : "xdg-open";
  const p = await packageApp(env, {
    app: "kernelsandbox",
    forkCli: true,
    name: "E2EKernelSandbox",
    identifier,
    deepLinks: [scheme],
    include: ["fork_child.js"],
    permissions: [
      "--kernel-sandbox=enforce",
      // strict: the app's own folders and this run's results, nothing else
      `--allow-read=${dir},$DENO_DESKTOP_DATA`,
      `--allow-write=${dir},$DENO_DESKTOP_DATA`,
      "--allow-env",
      "--allow-ffi",
      `--allow-run=${opener}`,
    ],
  });
  const inside = path(dir, `ks-inside-${env.nonce}`);
  const outside = path(home, `denext-e2e-ks-outside-${env.nonce}`);
  const outsideChild = path(home, `denext-e2e-ks-outside-child-${env.nonce}`);
  const web = listen();
  // before anything of this area can start a browser
  const safariBefore = await safariPids();
  try {
    const prestart = await hostCallsPrestart(p.artifact, p.exe);
    rep.note(`the host calls the runtime before its engine: ${prestart}`);
    let expect: "kernel" | "headless-only" | "none" | "machine";
    let reason: string | undefined;
    if (OS === "windows") {
      expect = "none";
      reason = "no kernel enforcer for desktop apps";
    } else if (OS === "darwin") {
      if (env.backend === "cef") {
        expect = "headless-only";
        reason = "CEF on macOS";
      } else {
        expect = "kernel";
      }
    } else if (!prestart) {
      expect = "headless-only";
      reason = "before the runtime could confine";
    } else {
      expect = env.backend === "cef" ? "machine" : "kernel";
    }
    await clearResults("kernelsandbox");
    await writeParams("kernelsandbox", {
      expect,
      reason,
      inside,
      outside,
      outsideChild,
      scheme,
      // a browser coming for a link is observable on macOS (LaunchServices
      // always has one); a Linux runner has no browser to hand it to
      openUrl: `http://127.0.0.1:${web.port}/opened`,
      childOpenUrl: `http://127.0.0.1:${web.port}/child-opened`,
    });
    const r = await launchAndCollect(env, rep, "strict launch", p, {
      args: [`${scheme}://row`],
      ms: 4 * 60_000,
    });
    const kernel = r?.data?.kernelSandbox &&
      (r.data.kernelSandbox as { sandbox?: string }).sandbox === "kernel";
    if (OS !== "windows") {
      rep.check(
        "the file a native write inside the grant made exists",
        await exists(inside),
        inside,
      );
      rep.check(
        "the file a native write outside the grant would make exists exactly when the app isn't kernel-held",
        (await exists(outside)) === !kernel,
        { kernel, outside },
      );
    }
    if (kernel) {
      rep.check(
        "a fork's native write outside the grant made no file",
        !(await exists(outsideChild)),
        outsideChild,
      );
    }
    if (OS === "darwin" && r?.data?.openExternal === true) {
      // give the browser a moment to come
      for (let i = 0; i < 60 && !web.seen.includes("/opened"); i++) {
        await sleep(500);
      }
      rep.check(
        "open_external_still_works: the user's browser came for the link",
        web.seen.includes("/opened"),
        web.seen,
      );
      if (kernel) {
        rep.check(
          "no browser came for the link a child's `open` was given",
          !web.seen.includes("/child-opened"),
          web.seen,
        );
      }
    }
    // The updater's swap, from a kernel-held app: the swap helper is
    // started by the outside helper (the app's own child would inherit the
    // sandbox). This app's grant doesn't cover the install's folder, so
    // nothing is staged here; the update area's signed feed under the
    // sandbox is the full row, and until it runs there this is said, not
    // skipped silently.
    rep.note(
      "updater's bundle swap: the swap helper is routed through the outside helper in a kernel-held app (runtime unit: deno_sandbox::outside); staging needs a write grant over the install's parent folder, which this strict app doesn't have. Run the update area with a kernel-sandbox build for the full row.",
    );
  } finally {
    await web.close();
    for (const f of [inside, outside, outsideChild]) await rm(f);
    // The browser the opened link started is this run's to close, and only
    // that one: a self-hosted macOS runner may share its user session with
    // other jobs (or a person), so a Safari that was already there is never
    // touched.
    await quitSafariStartedHere(safariBefore, web.seen.length > 0);
  }
}
