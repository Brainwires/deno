// Copyright 2018-2026 the Deno authors. MIT license.
// Deep links, opened files, second-instance forwarding and scheme
// registration, through the OS wherever a runner can drive it:
//
// - cold start with a link + a file in argv; a second launch while the app
//   runs forwards its argv (`secondinstance`) and exits without starting a
//   runtime;
// - the runtime's startup registration makes the app the scheme's handler
//   (Linux: an XDG entry + mimeapps.list in a throwaway XDG_DATA_HOME /
//   XDG_CONFIG_HOME; Windows: HKCU\Software\Classes; macOS: LaunchServices);
// - links routed BY THE OS to the registered handler, warm (to the running
//   app) and cold (starting it): `xdg-open`, `start`, `open`; on macOS also a
//   file opened with the app (`open -a`);
// - a second app declaring the same scheme leaves it alone (owner "other")
//   until it forces the registration;
// - Windows: the app installed from the `.msi` the stock CLI builds
//   (`msiexec /i`, per machine), registering itself from Program Files and
//   receiving `start <scheme>://…` links cold and warm, then uninstalled.

// deno-lint-ignore-file no-explicit-any

import {
  type AppResult,
  type AreaReport,
  clearResults,
  type Env,
  exists,
  kill,
  killByPath,
  launch,
  type Launched,
  log,
  must,
  OS,
  packageApp,
  type Packaged,
  path,
  results,
  rm,
  seenPids,
  sh,
  short,
  sleep,
  tail,
  waitExit,
  waitResult,
  writeParams,
} from "../lib/runner.ts";

const LSREGISTER =
  "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

export async function run(env: Env, rep: AreaReport) {
  // Letters only: xdg-utils' generic xdg-open takes a URL whose scheme has a
  // digit for a file path.
  const scheme = `dnxlink${env.nonce}`;
  const ids = {
    a: `dev.denext.e2e${env.nonce}.linka`,
    b: `dev.denext.e2e${env.nonce}.linkb`,
  };
  const spec = (which: "a" | "b") => ({
    app: "deeplink",
    tag: which,
    name: which === "a" ? "E2ELinkA" : "E2ELinkB",
    identifier: ids[which],
    deepLinks: [scheme],
    appJson: { deepLinks: [scheme], singleInstance: true },
    launch: { appId: ids[which], singleInstance: true },
  });
  const a = await packageApp(env, spec("a"));
  const b = await packageApp(env, spec("b"));

  // A scratch directory with the files the launches open, and (Linux) the
  // throwaway XDG directories: the user's real MIME defaults are never read
  // or written.
  const scratch = path(env.workDir, "deeplink");
  await rm(scratch);
  await Deno.mkdir(scratch, { recursive: true });
  const sample = path(scratch, "sample.txt");
  const other = path(scratch, "other file.txt");
  await Deno.writeTextFile(sample, "sample");
  await Deno.writeTextFile(other, "other");
  const appEnv: Record<string, string> = {};
  if (OS === "linux") {
    appEnv.XDG_DATA_HOME = path(scratch, "xdg-data");
    appEnv.XDG_CONFIG_HOME = path(scratch, "xdg-config");
    await Deno.mkdir(path(appEnv.XDG_DATA_HOME, "applications"), {
      recursive: true,
    });
    await Deno.mkdir(appEnv.XDG_CONFIG_HOME, { recursive: true });
  }
  const winKey = `HKCU\\Software\\Classes\\${scheme}`;
  if (OS === "windows") await sh("reg", ["delete", winKey, "/f"]);

  const running: Launched[] = [];
  const params = (which: "a" | "b", extra: Record<string, unknown> = {}) =>
    writeParams("deeplink", { scheme, identifier: ids[which], ...extra });
  const start = async (p: Packaged, args: string[], label: string) => {
    const seen = await seenPids("deeplink");
    const l = await launch(env, p.exe, args, { cwd: scratch, env: appEnv });
    running.push(l);
    const res = await waitResult("deeplink", { seen, ms: 90_000 });
    if (!res) {
      rep.check(
        `${label}: the app started and wrote a result`,
        false,
        await tail(l.logFile),
      );
    }
    return { l, res };
  };
  const reread = async (res: AppResult | null) =>
    res
      ? (await results("deeplink")).find((x) => x.pid === res.pid) ?? res
      : null;
  const stopAll = async () => {
    for (const l of running.splice(0)) await kill(l, undefined);
    await killByPath(a.artifact);
    await killByPath(b.artifact);
    await sleep(1500);
  };
  // A link the OS routes to the registered handler. A cold start through
  // xdg-open may not return until the app exits, so its opener is not
  // awaited.
  const osOpen = async (
    target: string,
    opts: { app?: string; cold?: boolean } = {},
  ) => {
    if (OS === "darwin") {
      return await sh("open", opts.app ? ["-a", opts.app, target] : [target], {
        timeoutMs: 30_000,
      });
    }
    if (OS === "windows") {
      return await sh("powershell", [
        "-NoProfile",
        "-Command",
        `Start-Process '${target}'`,
      ], { timeoutMs: 30_000 });
    }
    if (opts.cold) {
      running.push(
        await launch(env, "xdg-open", [target], { cwd: scratch, env: appEnv }),
      );
      return { code: 0, out: "(xdg-open started)" };
    }
    return await sh("xdg-open", [target], { env: appEnv, timeoutMs: 30_000 });
  };

  try {
    await clearResults("deeplink");

    // 1. Cold start with a link and a file in argv. On macOS A also forces
    // its registration once the unforced checks ran: LaunchServices picks
    // among several apps declaring a scheme on its own, so B's "other"
    // answer below is only deterministic against an explicit default.
    await params("a", OS === "darwin" ? { force: true } : {});
    const coldUrl = `${scheme}://cold/1?x=1`;
    const { l: first, res: r1 } = await start(a, [
      coldUrl,
      "sample.txt",
      "--flag",
      "notafile",
    ], "cold argv");
    rep.merge("A cold argv", r1);
    const l1 = r1?.data.launch;
    rep.check(
      "cold argv: launchUrls is the link",
      JSON.stringify(l1?.launchUrls) === JSON.stringify([coldUrl]),
      l1,
    );
    rep.check(
      "cold argv: launchFiles is the existing file, absolute",
      Array.isArray(l1?.launchFiles) && l1.launchFiles.length === 1 &&
        samePath(l1.launchFiles[0], sample),
      l1?.launchFiles,
    );
    rep.check(
      "cold argv: Deno.args carries every argument",
      ["--flag", "notafile", coldUrl].every((x) => l1?.args?.includes(x)),
      l1?.args,
    );
    const o1 = r1?.data.owner;
    rep.check(
      "startup registration: the app owns the unowned scheme",
      o1?.later?.owner === "self",
      o1,
    );
    rep.check(
      "registerScheme(): registered, owner self",
      o1?.explicit?.registered === true && o1?.explicit?.owner === "self",
      o1?.explicit,
    );
    await checkRegistration(rep, env, scheme, a, ids.a, appEnv, "A");

    // 2. A second launch while A runs forwards its argv and exits.
    const before = await seenPids("deeplink");
    const t0 = Date.now();
    const second = await launch(env, a.exe, [
      `${scheme}://second/2`,
      "other file.txt",
      "--x",
    ], { cwd: scratch, env: appEnv });
    const st = await waitExit(second, 30_000);
    rep.check(
      "second instance: exits by itself",
      st !== null,
      st ?? await tail(second.logFile),
    );
    if (st === null) await kill(second);
    else {rep.check("second instance: exit code 0", st.code === 0, {
        code: st.code,
        ms: Date.now() - t0,
      });}
    await sleep(2000);
    const fresh = (await results("deeplink")).filter((x) => !before.has(x.pid));
    rep.check(
      "second instance: never started a runtime of its own",
      fresh.length === 0,
      fresh.map((x) => x.pid),
    );
    const r1b = await reread(r1);
    const si = (r1b?.data.events ?? []).filter((e: any) =>
      e.type === "secondinstance"
    );
    rep.check(
      "second instance: the first one gets secondinstance with the link, the file and the cwd",
      si.length === 1 &&
        JSON.stringify(si[0].detail.urls) ===
          JSON.stringify([`${scheme}://second/2`]) &&
        si[0].detail.files.length === 1 &&
        samePath(si[0].detail.files[0], other) &&
        samePath(si[0].detail.cwd, scratch) &&
        si[0].detail.args.includes("--x"),
      si,
    );

    // 3. A warm link routed by the OS to the running handler.
    const warmUrl = `${scheme}://warm/3`;
    const ow = await osOpen(warmUrl);
    rep.check(
      "OS-routed warm link: the opener succeeded",
      ow.code === 0,
      ow.out,
    );
    const warmType = OS === "darwin" ? "openurl" : "secondinstance";
    const gotWarm = await waitEvents(
      r1,
      (ev) =>
        ev.some((e: any) =>
          e.type === warmType &&
          (e.detail.url === warmUrl || e.detail.urls?.includes(warmUrl))
        ),
      30_000,
    );
    rep.check(
      `OS-routed warm link: the running app gets ${warmType}`,
      gotWarm.ok,
      gotWarm.events,
    );
    rep.check(
      "OS-routed warm link: no new instance",
      (await results("deeplink")).filter((x) => !before.has(x.pid)).length ===
        0,
    );

    if (OS === "darwin") {
      // 4. A file opened with the running app.
      const of = await osOpen(other, { app: a.artifact });
      rep.check("open -a <app> <file>: succeeded", of.code === 0, of.out);
      const gotFile = await waitEvents(
        r1,
        (ev) =>
          ev.some((e: any) =>
            e.type === "openfile" && samePath(e.detail.path, other)
          ),
        30_000,
      );
      rep.check(
        "open -a <app> <file>: the running app gets openfile",
        gotFile.ok,
        gotFile.events,
      );
    } else {
      rep.na(
        "a file opened with the running app (openfile)",
        "openfile is the macOS delivery; on Windows and Linux a file opened with the app is an argv, covered by the cold-argv and second-instance checks",
      );
    }
    void first;
    await stopAll();

    // 5. A cold link routed by the OS: the handler starts with it.
    const coldOsUrl = `${scheme}://cold/5`;
    const seen5 = await seenPids("deeplink");
    const oc = await osOpen(coldOsUrl, { cold: true });
    rep.check(
      "OS-routed cold link: the opener succeeded",
      oc.code === 0,
      oc.out,
    );
    const r5 = await waitResult("deeplink", { seen: seen5, ms: 90_000 });
    rep.check(
      "OS-routed cold link: the handler starts with the link in launchUrls",
      JSON.stringify(r5?.data.launch?.launchUrls) ===
        JSON.stringify([coldOsUrl]),
      r5?.data.launch ?? "no launch",
    );
    rep.check(
      "OS-routed cold link: it is app A",
      r5 ? samePath(r5.data.launch.execPath, a.exe) : false,
      r5?.data.launch?.execPath,
    );
    await stopAll();

    // 6. Another app declaring the scheme leaves it alone, until forced.
    // On macOS that other app is a stub bundle made the scheme's default
    // handler explicitly: LaunchServices itself may hand a scheme to any
    // newly launched app that declares it (a second runtime app included),
    // so only an explicit default is a stable "another app owns it".
    if (OS === "darwin") {
      await macForeignOwner();
    } else {
      const snapshot = await registrationSnapshot(scheme, appEnv);
      await params("b");
      const { res: rb } = await start(b, [], "B");
      rep.merge("B", rb);
      const ob = rb?.data.owner;
      rep.check(
        "B: the scheme is owned by another app",
        ob?.later?.owner === "other",
        ob,
      );
      rep.check(
        "B: registerScheme() without force does not take it",
        ob?.explicit?.registered === false && ob?.explicit?.owner === "other",
        ob?.explicit,
      );
      rep.check(
        "B: the OS registration is untouched",
        (await registrationSnapshot(scheme, appEnv)) === snapshot,
        {
          before: snapshot,
          after: await registrationSnapshot(scheme, appEnv),
        },
      );
      await stopAll();
      await params("b", { force: true });
      const { res: rbf } = await start(b, [], "B forced");
      const obf = rbf?.data.owner;
      rep.check(
        "B: registerScheme({ force: true }) takes the scheme over",
        obf?.forced?.registered === true && obf?.forced?.owner === "self" &&
          obf?.afterForce?.owner === "self",
        obf,
      );
      await stopAll();
      await params("a");
      const { res: ra2 } = await start(a, [], "A after B forced");
      rep.check(
        "A: now sees the scheme owned by another app",
        ra2?.data.owner?.later?.owner === "other",
        ra2?.data.owner,
      );
      await stopAll();
    }

    // 7. Windows: the app installed from the stock CLI's .msi.
    if (OS === "windows" && env.backend === "cef") {
      rep.na(
        "install from the .msi",
        "the stock CLI's .msi packs laufey's CEF executable as the app and the runtime as <App>.dll, which a CEF host behind CEF's bootstrap can't start (see windowsCefLayout); denext builds its .msi from the finished app directory",
      );
    } else if (OS === "windows") {
      await msi(env, rep, scheme, ids.a, winKey);
    } else {
      rep.na(
        "install from the .msi",
        "the stock CLI builds .msi installers only for Windows targets",
      );
    }
  } finally {
    await stopAll();
    if (OS === "windows") await sh("reg", ["delete", winKey, "/f"]);
    if (OS === "darwin") {
      await sh(LSREGISTER, ["-u", a.artifact]);
      await sh(LSREGISTER, ["-u", b.artifact]);
    }
  }

  async function waitEvents(
    res: AppResult | null,
    pred: (ev: any[]) => boolean,
    ms: number,
  ) {
    let ev: any[] = [];
    const end = Date.now() + ms;
    while (Date.now() < end) {
      ev = (await reread(res))?.data.events ?? [];
      if (pred(ev)) return { ok: true, events: ev };
      await sleep(250);
    }
    return { ok: false, events: ev };
  }

  async function macForeignOwner() {
    const foreignId = `dev.denext.e2e${env.nonce}.foreign`;
    const stub = path(scratch, "Foreign.app");
    await Deno.mkdir(path(stub, "Contents", "MacOS"), { recursive: true });
    await Deno.writeTextFile(
      path(stub, "Contents", "Info.plist"),
      `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>${foreignId}</string>
<key>CFBundleName</key><string>Foreign</string>
<key>CFBundleExecutable</key><string>Foreign</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleURLTypes</key><array><dict>
<key>CFBundleURLName</key><string>${foreignId}</string>
<key>CFBundleURLSchemes</key><array><string>${scheme}</string></array>
</dict></array></dict></plist>`,
    );
    const stubExe = path(stub, "Contents", "MacOS", "Foreign");
    await Deno.writeTextFile(stubExe, "#!/bin/sh\nexit 0\n");
    await Deno.chmod(stubExe, 0o755);
    await sh(LSREGISTER, ["-f", stub]);
    const set = await sh("osascript", [
      "-l",
      "JavaScript",
      "-e",
      `ObjC.import("CoreServices"); $.LSSetDefaultHandlerForURLScheme($("${scheme}"), $("${foreignId}"))`,
    ]);
    rep.check(
      "foreign stub: made the default handler (LSSetDefaultHandlerForURLScheme)",
      set.code === 0 && set.out.trim() === "0",
      set.out,
    );
    try {
      await params("a");
      const { res } = await start(a, [], "A with a foreign default");
      const o = res?.data.owner;
      rep.check(
        "A: a scheme another app is the default for is owned by another app",
        o?.atStart?.owner === "other" && o?.later?.owner === "other" &&
          o?.later?.handler === foreignId,
        o,
      );
      rep.check(
        "A: registerScheme() without force does not take it",
        o?.explicit?.registered === false && o?.explicit?.owner === "other",
        o?.explicit,
      );
      await stopAll();
      await params("a", { force: true });
      const { res: rf } = await start(
        a,
        [],
        "A forced over the foreign default",
      );
      const of = rf?.data.owner;
      rep.check(
        "A: registerScheme({ force: true }) takes the scheme over",
        of?.forced?.registered === true && of?.forced?.owner === "self" &&
          of?.afterForce?.owner === "self",
        of,
      );
      await stopAll();
    } finally {
      await sh(LSREGISTER, ["-u", stub]);
    }
  }

  async function msi(
    env: Env,
    rep: AreaReport,
    scheme: string,
    identifier: string,
    key: string,
  ) {
    const name = "E2ELinkMsi";
    const m = await packageApp(env, {
      ...spec("a"),
      tag: "msi",
      name,
      output: path("out", `${name}.msi`),
    });
    const msiFile = m.artifact;
    rep.check(
      "msi: the stock CLI built an .msi",
      await exists(msiFile),
      msiFile,
    );
    const logFile = path(env.workDir, "logs", "msiexec-install.log");
    const inst = await sh("msiexec", [
      "/i",
      msiFile,
      "/qn",
      "/norestart",
      "/l*v",
      logFile,
    ], { timeoutMs: 10 * 60_000 });
    rep.check(
      "msi: msiexec /i succeeded",
      inst.code === 0,
      inst.code === 0
        ? undefined
        : { code: inst.code, log: await tail(logFile) },
    );
    const installDir = path(
      Deno.env.get("ProgramFiles") ?? "C:\\Program Files",
      name,
    );
    const exe = path(installDir, `${name}.exe`);
    rep.check(
      "msi: installed under Program Files",
      await exists(exe),
      {
        exe,
        dir: await Array.fromAsync(Deno.readDir(installDir)).then(
          (e) => e.map((x) => x.name),
          (e) => String(e),
        ),
      },
    );
    if (!await exists(exe)) return;
    try {
      // As denext's packager does for every package: the launch config.
      await Deno.writeTextFile(
        path(installDir, "laufey-launch.json"),
        JSON.stringify({ appId: identifier, singleInstance: true }),
      );
      await sh("reg", ["delete", key, "/f"]);
      const installed: Packaged = {
        spec: spec("a"),
        artifact: installDir,
        exe,
        buildDir: installDir,
      };
      await params("a");
      const { res } = await start(installed, [], "msi first launch");
      rep.check(
        "msi: the installed app registers the scheme",
        res?.data.owner?.later?.owner === "self",
        res?.data.owner,
      );
      const cmd = await regCommand(key);
      rep.check(
        "msi: HKCU\\Software\\Classes\\<scheme>\\shell\\open\\command runs the installed exe with -- before the link",
        cmd !== null && cmd.toLowerCase().includes(`"${exe.toLowerCase()}"`) &&
          cmd.includes(' -- "%1"'),
        cmd,
      );
      const proto = await sh("reg", ["query", key, "/v", "URL Protocol"]);
      rep.check("msi: the key is a URL protocol", proto.code === 0, proto.out);
      await stopAll();
      await killByPath(installDir);
      const coldUrl = `${scheme}://msi/cold`;
      const seen = await seenPids("deeplink");
      const oc = await sh("powershell", [
        "-NoProfile",
        "-Command",
        `Start-Process '${coldUrl}'`,
      ], { timeoutMs: 30_000 });
      rep.check(
        "msi: Start-Process <scheme>:// succeeded",
        oc.code === 0,
        oc.out,
      );
      const rc = await waitResult("deeplink", { seen, ms: 90_000 });
      rep.check(
        "msi: a cold link starts the installed app with it in launchUrls",
        JSON.stringify(rc?.data.launch?.launchUrls) ===
            JSON.stringify([coldUrl]) &&
          samePath(rc?.data.launch?.execPath ?? "", exe),
        rc?.data.launch ?? "no launch",
      );
      const warmUrl = `${scheme}://msi/warm`;
      const before = await seenPids("deeplink");
      await sh("powershell", [
        "-NoProfile",
        "-Command",
        `Start-Process '${warmUrl}'`,
      ], { timeoutMs: 30_000 });
      const got = await waitEvents(
        rc,
        (ev) =>
          ev.some((e: any) =>
            e.type === "secondinstance" && e.detail.urls?.includes(warmUrl)
          ),
        30_000,
      );
      rep.check(
        "msi: a warm link reaches the running installed app (secondinstance)",
        got.ok,
        got.events,
      );
      rep.check(
        "msi: no second instance started",
        (await results("deeplink")).filter((x) => !before.has(x.pid)).length ===
          0,
      );
    } finally {
      await killByPath(installDir);
      await sleep(1000);
      const ulog = path(env.workDir, "logs", "msiexec-uninstall.log");
      const un = await sh("msiexec", [
        "/x",
        msiFile,
        "/qn",
        "/norestart",
        "/l*v",
        ulog,
      ], { timeoutMs: 10 * 60_000 });
      rep.check(
        "msi: msiexec /x succeeded",
        un.code === 0,
        un.code === 0 ? undefined : { code: un.code, log: await tail(ulog) },
      );
      rep.check("msi: the uninstall removed the app", !await exists(exe));
      await rm(m.buildDir);
    }
  }
}

/** Case- and separator-insensitive path equality (macOS /private/var
 * aliases included). */
export function samePath(x: string, y: string): boolean {
  const norm = (p: string) =>
    p.replace(/\\/g, "/").replace(/^\/private\//, "/").replace(/\/+$/, "")
      .toLowerCase();
  return typeof x === "string" && typeof y === "string" && norm(x) === norm(y);
}

async function regCommand(key: string): Promise<string | null> {
  const q = await sh("reg", ["query", `${key}\\shell\\open\\command`, "/ve"]);
  if (q.code !== 0) return null;
  const m = /REG_(?:EXPAND_)?SZ\s+(.*)$/m.exec(q.out);
  return m ? m[1].trim() : null;
}

/** What the OS's handler database says about the scheme, as a string to
 * compare before / after. */
async function registrationSnapshot(
  scheme: string,
  appEnv: Record<string, string>,
): Promise<string> {
  if (OS === "windows") {
    return (await sh("reg", [
      "query",
      `HKCU\\Software\\Classes\\${scheme}`,
      "/s",
    ])).out;
  }
  if (OS === "linux") {
    const list = await Deno.readTextFile(
      path(appEnv.XDG_CONFIG_HOME, "mimeapps.list"),
    ).catch(() => "(none)");
    const def = (await sh(
      "xdg-mime",
      ["query", "default", `x-scheme-handler/${scheme}`],
      { env: appEnv },
    )).out.trim();
    return `${list}\n--\n${def}`;
  }
  // macOS: LaunchServices' default handler (what getSchemeOwner reads) is
  // not exposed by a stable CLI; the apps' getSchemeOwner answers cover it.
  return "";
}

async function checkRegistration(
  rep: AreaReport,
  env: Env,
  scheme: string,
  p: Packaged,
  identifier: string,
  appEnv: Record<string, string>,
  label: string,
) {
  void env;
  if (OS === "windows") {
    const cmd = await regCommand(`HKCU\\Software\\Classes\\${scheme}`);
    rep.check(
      `${label}: HKCU\\Software\\Classes\\<scheme>\\shell\\open\\command runs this exe with -- before the link`,
      cmd !== null && cmd.toLowerCase().includes(`"${p.exe.toLowerCase()}"`) &&
        cmd.includes(' -- "%1"'),
      cmd,
    );
  } else if (OS === "linux") {
    const entry = path(
      appEnv.XDG_DATA_HOME,
      "applications",
      `${identifier}.desktop`,
    );
    const text = await Deno.readTextFile(entry).catch(() => null);
    rep.check(
      `${label}: an XDG entry named after the app id runs this exe with %u`,
      text !== null && text.includes(`x-scheme-handler/${scheme}`) &&
        text.includes(p.exe) && text.includes("%u"),
      text ?? `missing ${entry}`,
    );
    const def = (await sh(
      "xdg-mime",
      ["query", "default", `x-scheme-handler/${scheme}`],
      { env: appEnv },
    )).out.trim();
    rep.check(
      `${label}: xdg-mime reports it as the default handler`,
      def === `${identifier}.desktop`,
      def,
    );
  } else {
    const r = await sh("/usr/bin/plutil", [
      "-extract",
      "CFBundleURLTypes",
      "json",
      "-o",
      "-",
      path(p.artifact, "Contents", "Info.plist"),
    ]);
    rep.check(
      `${label}: Info.plist declares the scheme (CFBundleURLTypes)`,
      r.out.includes(scheme),
      short(r.out),
    );
  }
  log(`  (registration checked for ${label})`);
  void must;
}
