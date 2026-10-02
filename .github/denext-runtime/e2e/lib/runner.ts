// Copyright 2018-2026 the Deno authors. MIT license.
// The runner half of the denext runtime e2e harness: package a test app
// with the stock `deno desktop` CLI against the runtime under test, launch
// it the ways the OS would, collect the JSON the app writes, and add the
// checks only the outside can make (persistence across launches, files and
// registry entries on disk, processes that must or must not exist).

// deno-lint-ignore-file no-explicit-any

import { e2eDir } from "../apps/_shared/e2e.ts";
import type { Check, Status } from "../apps/_shared/e2e.ts";

export type { Check, Status };

export const OS = Deno.build.os;
export const HERE = new URL("..", import.meta.url);

/** A file under the harness's apps/ directory, as a native path. */
export function appsPath(rel: string): string {
  const p = decodeURIComponent(new URL(`apps/${rel}`, HERE).pathname);
  return OS === "windows"
    ? p.replace(/^\/([A-Za-z]:)/, "$1").replace(/\//g, "\\")
    : p;
}

export function path(...parts: string[]): string {
  return parts.join(OS === "windows" ? "\\" : "/").replace(
    /[\\/]+/g,
    OS === "windows" ? "\\" : "/",
  );
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface Env {
  target: string;
  backend: "webview" | "cef";
  /** The unpacked runtime archive (runtime lib + laufey/). */
  runtimeDir: string;
  /** Scratch space for this backend. */
  workDir: string;
  /** A short random lowercase tag: identifiers and schemes of this run are
   * unique, so OS state left by an earlier run (LaunchServices, the
   * registry, web data directories) cannot leak into this one. */
  nonce: string;
}

export function runtimeLib(target: string): string {
  if (target.includes("apple-darwin")) return "libdenort.dylib";
  if (target.includes("windows")) return "denort.dll";
  return "libdenort.so";
}

export async function exists(p: string): Promise<boolean> {
  return await Deno.lstat(p).then(() => true, () => false);
}

export async function rm(p: string) {
  await Deno.remove(p, { recursive: true }).catch(() => {});
}

export interface RunResult {
  code: number;
  out: string;
}

/** Run a command to completion; never throws for a non-zero exit. */
export async function sh(
  cmd: string,
  args: string[],
  opts: { cwd?: string; env?: Record<string, string>; timeoutMs?: number } = {},
): Promise<RunResult> {
  const child = new Deno.Command(cmd, {
    args,
    cwd: opts.cwd,
    env: opts.env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  let timer: ReturnType<typeof setTimeout> | undefined;
  if (opts.timeoutMs) {
    timer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch { /* exited */ }
    }, opts.timeoutMs);
  }
  const out = await child.output();
  clearTimeout(timer);
  const text = new TextDecoder().decode(out.stdout) +
    new TextDecoder().decode(out.stderr);
  return { code: out.code, out: text };
}

/** `sh`, but a non-zero exit is an error. */
export async function must(
  cmd: string,
  args: string[],
  opts: { cwd?: string; env?: Record<string, string>; timeoutMs?: number } = {},
): Promise<string> {
  const r = await sh(cmd, args, opts);
  if (r.code !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} exited ${r.code}:\n${r.out}`);
  }
  return r.out;
}

async function copyTree(from: string, to: string) {
  await Deno.mkdir(to, { recursive: true });
  for await (const e of Deno.readDir(from)) {
    const src = path(from, e.name);
    const dst = path(to, e.name);
    if (e.isDirectory) await copyTree(src, dst);
    else await Deno.copyFile(src, dst);
  }
}

// ---------------------------------------------------------------------------
// Packaging.

export interface AppSpec {
  /** The app directory under apps/. */
  app: string;
  /** desktop.app.name, also the output name. */
  name: string;
  identifier: string;
  /** deno.json `version`. */
  version?: string;
  /** `.deno-desktop/app.json` beyond the identifier (origin, deepLinks,
   * singleInstance, update, initialWindow). */
  appJson?: Record<string, unknown>;
  /** deno.json `desktop.app.deepLinks` (the stock CLI registers these in
   * the macOS Info.plist / the Linux .desktop entry). */
  deepLinks?: string[];
  /** `laufey-launch.json` (appId, customSchemes, singleInstance,
   * inspectable); written after packaging, as denext's packager does. */
  launch?: Record<string, unknown>;
  /** `--output` override (e.g. a `.msi`). */
  output?: string;
  /** A suffix for the build directory (several builds of one app). */
  tag?: string;
  /** More `compile.include` entries (worker modules). */
  include?: string[];
}

export interface Packaged {
  spec: AppSpec;
  /** The `.app` bundle (macOS) or the app directory. */
  artifact: string;
  exe: string;
  buildDir: string;
}

export function laufeyLaunchPath(artifact: string): string {
  return OS === "darwin"
    ? path(artifact, "Contents", "Resources", "laufey-launch.json")
    : path(artifact, "laufey-launch.json");
}

export async function findArtifact(
  out: string,
  name: string,
): Promise<{ artifact: string; exe: string }> {
  if (OS === "darwin") {
    const artifact = path(out, `${name}.app`);
    const plist = await Deno.readTextFile(
      path(artifact, "Contents", "Info.plist"),
    );
    const exeName = /<key>CFBundleExecutable<\/key>\s*<string>([^<]+)<\/string>/
      .exec(plist)?.[1];
    if (!exeName) throw new Error(`no CFBundleExecutable in ${artifact}`);
    return { artifact, exe: path(artifact, "Contents", "MacOS", exeName) };
  }
  const artifact = path(out, name);
  const exe = path(artifact, OS === "windows" ? `${name}.exe` : name);
  if (!await exists(exe)) {
    const listing = await Array.fromAsync(Deno.readDir(artifact)).catch(
      () => [],
    );
    throw new Error(
      `no ${exe}; ${artifact} has ${listing.map((e) => e.name).join(", ")}`,
    );
  }
  return { artifact, exe };
}

/** Ad-hoc sign a macOS bundle after the harness changed it. */
export async function adhocSign(artifact: string) {
  if (OS !== "darwin") return;
  await must("codesign", ["--force", "--deep", "--sign", "-", artifact]);
}

export async function packageApp(env: Env, spec: AppSpec): Promise<Packaged> {
  const buildDir = path(
    env.workDir,
    "build",
    `${spec.app}${spec.tag ? `-${spec.tag}` : ""}`,
  );
  await rm(buildDir);
  const src = path(buildDir, "src");
  await copyTree(appsPath("_shared"), path(src, "_shared"));
  const appDir = path(src, spec.app);
  await copyTree(appsPath(spec.app), appDir);
  const denoJson: Record<string, unknown> = {
    compile: { include: [".deno-desktop/app.json", ...spec.include ?? []] },
    desktop: {
      app: {
        name: spec.name,
        identifier: spec.identifier,
        ...(spec.deepLinks ? { deepLinks: spec.deepLinks } : {}),
      },
    },
  };
  if (spec.version) denoJson.version = spec.version;
  await Deno.writeTextFile(
    path(appDir, "deno.json"),
    JSON.stringify(denoJson, null, 2),
  );
  await Deno.mkdir(path(appDir, ".deno-desktop"), { recursive: true });
  await Deno.writeTextFile(
    path(appDir, ".deno-desktop", "app.json"),
    JSON.stringify({ identifier: spec.identifier, ...spec.appJson }, null, 2),
  );
  const output = spec.output ?? path("out", spec.name);
  const t0 = Date.now();
  await must(Deno.execPath(), [
    "desktop",
    "-A",
    "--no-check",
    "--backend",
    env.backend,
    "--output",
    output,
    "main.ts",
  ], {
    cwd: appDir,
    env: {
      DENORT_DESKTOP_BIN: path(env.runtimeDir, runtimeLib(env.target)),
      LAUFEY_DEV_DIR: path(env.runtimeDir, "laufey"),
    },
    timeoutMs: 15 * 60_000,
  });
  log(
    `packaged ${spec.app}${spec.tag ? ` (${spec.tag})` : ""} in ${
      Date.now() - t0
    } ms`,
  );
  if (spec.output?.endsWith(".msi")) {
    return { spec, artifact: path(appDir, spec.output), exe: "", buildDir };
  }
  const { artifact, exe } = await findArtifact(path(appDir, "out"), spec.name);
  if (spec.launch) {
    await Deno.writeTextFile(
      laufeyLaunchPath(artifact),
      JSON.stringify(spec.launch, null, 2) + "\n",
    );
  }
  await adhocSign(artifact);
  return { spec, artifact, exe, buildDir };
}

// ---------------------------------------------------------------------------
// Launching.

export interface Launched {
  child: Deno.ChildProcess;
  pid: number;
  logFile: string;
  exited: Promise<Deno.CommandStatus>;
  status: Deno.CommandStatus | null;
}

let launchSeq = 0;

export async function launch(
  env: Env,
  exe: string,
  args: string[] = [],
  opts: { cwd?: string; env?: Record<string, string> } = {},
): Promise<Launched> {
  const logFile = path(
    env.workDir,
    "logs",
    `launch-${++launchSeq}-${exe.split(/[\\/]/).pop()}.log`,
  );
  await Deno.mkdir(path(env.workDir, "logs"), { recursive: true });
  const f = await Deno.open(logFile, {
    write: true,
    create: true,
    truncate: true,
  });
  await f.write(new TextEncoder().encode(`$ ${exe} ${args.join(" ")}\n`));
  const child = new Deno.Command(exe, {
    args,
    cwd: opts.cwd,
    env: opts.env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const w = f.writable.getWriter();
  const pump = async (s: ReadableStream<Uint8Array>) => {
    for await (const c of s) await w.write(c).catch(() => {});
  };
  const pumps = Promise.all([pump(child.stdout), pump(child.stderr)]).finally(
    () => w.close().catch(() => {}),
  );
  const l: Launched = {
    child,
    pid: child.pid,
    logFile,
    status: null,
    exited: child.status.then(async (s) => {
      l.status = s;
      await pumps.catch(() => {});
      return s;
    }),
  };
  return l;
}

/** Wait for `l` to exit; null on timeout. */
export async function waitExit(
  l: Launched,
  ms: number,
): Promise<Deno.CommandStatus | null> {
  return await Promise.race([l.exited, sleep(ms).then(() => null)]);
}

/** Kill `l` and everything it started. */
export async function kill(l: Launched | number, artifact?: string) {
  const pid = typeof l === "number" ? l : l.pid;
  if (OS === "windows") {
    await sh("taskkill", ["/F", "/T", "/PID", String(pid)]);
  } else {
    try {
      Deno.kill(pid, "SIGKILL");
    } catch { /* gone */ }
  }
  if (artifact) await killByPath(artifact);
  if (typeof l !== "number") await waitExit(l, 10_000);
}

/** Kill every process whose executable lives under `dir`. */
export async function killByPath(dir: string) {
  if (OS === "windows") {
    const ps = `Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith('${
      dir.replace(/'/g, "''")
    }', 'OrdinalIgnoreCase') } | Stop-Process -Force`;
    await sh("powershell", ["-NoProfile", "-Command", ps]);
  } else {
    await sh("pkill", ["-KILL", "-f", dir]);
  }
}

export async function tail(file: string, lines = 40): Promise<string> {
  const t = await Deno.readTextFile(file).catch(() => "");
  return t.split(/\r?\n/).slice(-lines).join("\n");
}

// ---------------------------------------------------------------------------
// Results the apps write.

export interface AppResult {
  area: string;
  pid: number;
  startedAt: string;
  params: Record<string, unknown>;
  checks: Check[];
  data: Record<string, any>;
  done: boolean;
  file: string;
}

export const DIR = e2eDir();

export async function clearResults(area: string) {
  await Deno.mkdir(DIR, { recursive: true });
  for await (const e of Deno.readDir(DIR)) {
    if (e.name.startsWith(`${area}-`) || e.name === `${area}.params.json`) {
      await rm(path(DIR, e.name));
    }
  }
}

export async function writeParams(
  area: string,
  params: Record<string, unknown>,
) {
  await Deno.mkdir(DIR, { recursive: true });
  await Deno.writeTextFile(
    path(DIR, `${area}.params.json`),
    JSON.stringify(params, null, 2),
  );
}

export async function results(area: string): Promise<AppResult[]> {
  const out: AppResult[] = [];
  for await (const e of Deno.readDir(DIR)) {
    if (!e.name.startsWith(`${area}-`) || !e.name.endsWith(".json")) continue;
    const file = path(DIR, e.name);
    try {
      out.push({ ...JSON.parse(await Deno.readTextFile(file)), file });
    } catch { /* being written */ }
  }
  return out.sort((a, b) => a.startedAt.localeCompare(b.startedAt));
}

/** Wait until a result file of `area` not in `seen` matches `pred`. */
export async function waitResult(
  area: string,
  opts: {
    seen?: Set<number>;
    pid?: number;
    until?: (r: AppResult) => boolean;
    ms?: number;
  } = {},
): Promise<AppResult | null> {
  const until = opts.until ?? ((r) => r.done);
  const end = Date.now() + (opts.ms ?? 90_000);
  let last: AppResult | null = null;
  while (Date.now() < end) {
    for (const r of await results(area)) {
      if (opts.seen?.has(r.pid)) continue;
      if (opts.pid !== undefined && r.pid !== opts.pid) continue;
      last = r;
      if (until(r)) return r;
    }
    await sleep(250);
  }
  return opts.until ? null : last && last.done ? last : null;
}

export async function seenPids(area: string): Promise<Set<number>> {
  return new Set((await results(area)).map((r) => r.pid));
}

// ---------------------------------------------------------------------------
// The area's report.

export class AreaReport {
  readonly checks: Check[] = [];
  readonly notes: string[] = [];
  constructor(readonly area: string) {}

  check(name: string, ok: boolean, detail?: unknown): boolean {
    this.checks.push({
      name,
      status: ok ? "pass" : "fail",
      ...(detail === undefined ? {} : { detail }),
    });
    log(
      `  ${ok ? "PASS" : "FAIL"} ${name}${
        !ok && detail !== undefined ? ` :: ${short(detail)}` : ""
      }`,
    );
    return ok;
  }

  na(name: string, reason: string) {
    this.checks.push({ name, status: "n/a", reason });
    log(`  N/A  ${name} :: ${reason}`);
  }

  /** Merge an app's checks, prefixed with the launch's label. */
  merge(label: string, r: AppResult | null, logFile?: string) {
    if (!r) {
      this.check(`${label}: the app wrote a result`, false);
      return;
    }
    for (const c of r.checks) {
      const name = `${label}: ${c.name}`;
      if (c.status === "n/a") this.na(name, c.reason ?? "(no reason)");
      else this.check(name, c.status === "pass", c.detail);
    }
    if (!r.done) this.check(`${label}: the app finished`, false, logFile);
  }

  note(s: string) {
    this.notes.push(s);
    log(`  note: ${s}`);
  }
}

export function short(v: unknown, n = 600): string {
  const s = typeof v === "string" ? v : JSON.stringify(v);
  return s && s.length > n ? `${s.slice(0, n)}…` : String(s);
}

export function log(s: string) {
  console.log(s);
}

/** Run one launch: start, wait for a done result (or the timeout), wait for
 * the exit, kill whatever is left, and merge the app's checks. */
export async function launchAndCollect(
  env: Env,
  rep: AreaReport,
  label: string,
  p: Packaged,
  opts: {
    args?: string[];
    cwd?: string;
    env?: Record<string, string>;
    ms?: number;
    expectExit?: boolean;
  } = {},
): Promise<AppResult | null> {
  const seen = await seenPids(rep.area);
  const l = await launch(env, p.exe, opts.args, {
    cwd: opts.cwd,
    env: opts.env,
  });
  const r = await waitResult(rep.area, { seen, ms: opts.ms ?? 90_000 });
  if (!r) {
    const partial = (await results(rep.area)).filter((x) =>
      !seen.has(x.pid)
    ).pop() ?? null;
    rep.merge(label, partial, l.logFile);
    rep.check(
      `${label}: finished within ${(opts.ms ?? 90_000) / 1000} s`,
      false,
      await tail(l.logFile),
    );
    await kill(l, p.artifact);
    return partial;
  }
  if (opts.expectExit !== false) {
    const st = await waitExit(l, 30_000);
    // The app may add checks between `done` and its exit (quit() itself).
    const final = (await results(rep.area)).find((x) => x.pid === r.pid) ?? r;
    rep.merge(label, final, l.logFile);
    rep.check(
      `${label}: the app exited`,
      st !== null,
      st ?? (await tail(l.logFile)),
    );
    await kill(l, p.artifact);
    return final;
  }
  rep.merge(label, r, l.logFile);
  await kill(l, p.artifact);
  return r;
}
