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

/**
 * The Windows CEF layout. The stock CLI ships laufey's CEF executable as
 * `<App>.exe` and the runtime as `<App>.dll`; laufey's CEF executable is now
 * CEF's bootstrap.exe (Chromium's sandbox), which loads its host library
 * `laufey.dll` under the name `<App>.dll`, and the host loads the runtime as
 * `<App>.runtime.dll`. denext's packager moves the two files the same way.
 * No-op elsewhere, or for a laufey without the host library.
 */
export async function windowsCefLayout(artifact: string, exe: string) {
  const host = path(artifact, "laufey.dll");
  if (OS !== "windows" || !await exists(host)) return;
  const stem = exe.replace(/\.exe$/i, "");
  if (stem.toLowerCase() === path(artifact, "laufey").toLowerCase()) {
    throw new Error(`an app named laufey can't take the CEF layout: ${exe}`);
  }
  await Deno.rename(`${stem}.dll`, `${stem}.runtime.dll`);
  await Deno.rename(host, `${stem}.dll`);
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
  // The app writes its results where this run reads them, however it is
  // launched (the OS starts some launches, with none of our environment).
  await Deno.writeTextFile(
    path(src, "_shared", "e2e_run.ts"),
    `export const RUN_DIR: string | null = ${JSON.stringify(DIR)};\n`,
  );
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
  await windowsCefLayout(artifact, exe);
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
  /** Every process this launch started, seen while it ran (kill()). */
  tree: ProcessTree;
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
  // CEF's bootstrap starts a Windows CEF app in its executable's directory;
  // a launcher that gives it another one names it in LAUFEY_CWD, which the
  // host changes back to (laufey docs/backends.md). A launch from a shell or
  // shortcut has no such launcher and starts in the executable's directory.
  const cwdEnv = OS === "windows" && env.backend === "cef" && opts.cwd
    ? { LAUFEY_CWD: opts.cwd }
    : {};
  const child = new Deno.Command(exe, {
    args,
    cwd: opts.cwd,
    env: { ...cwdEnv, ...opts.env },
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
  const tree = new ProcessTree(child.pid);
  await tree.track();
  tree.poll();
  const l: Launched = {
    child,
    pid: child.pid,
    tree,
    logFile,
    status: null,
    exited: child.status.then(async (s) => {
      l.status = s;
      tree.stopPolling();
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

// ---------------------------------------------------------------------------
// Killing: by pid and start time, never by command-line pattern. Chromium
// re-execs its zygote, renderer and GPU helpers through /proc/self/exe, so
// their command line need not name the artifact; a pattern match (pkill -f)
// missed them and they leaked into the next launch.

/** A process as the OS lists it: its parent, its start time (a reused pid
 * has another start time) and its state (`Z`: a zombie, which has already
 * let go of its files and locks). */
export interface ProcInfo {
  ppid: number;
  start: string;
  state: string;
}

/** Every process on the machine (Linux: /proc/<pid>/stat; macOS: ps). */
export async function processTable(): Promise<Map<number, ProcInfo>> {
  const t = new Map<number, ProcInfo>();
  if (OS === "linux") {
    const reads: Promise<void>[] = [];
    for await (const e of Deno.readDir("/proc")) {
      if (!/^\d+$/.test(e.name)) continue;
      reads.push(
        Deno.readTextFile(`/proc/${e.name}/stat`).then((stat) => {
          // `pid (comm) state ppid ...`: comm may hold spaces and parens.
          const f = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
          // Fields 4 (ppid) and 22 (starttime, clock ticks since boot).
          t.set(Number(e.name), {
            ppid: Number(f[1]),
            start: f[19],
            state: f[0],
          });
        }, () => {/* exited meanwhile */}),
      );
    }
    await Promise.all(reads);
  } else {
    const r = await sh("ps", ["-A", "-o", "pid=,ppid=,stat=,lstart="]);
    for (const line of r.out.split("\n")) {
      const f = line.trim().split(/\s+/);
      if (f.length < 4 || !/^\d+$/.test(f[0])) continue;
      t.set(Number(f[0]), {
        ppid: Number(f[1]),
        start: f.slice(3).join(" "),
        state: f[2],
      });
    }
  }
  return t;
}

/** Wait (up to `ms`) until none of `pids` runs as the process `start` names
 * (a zombie has ended); the ones still running. A SIGKILLed process can take
 * a while to end (a write to disk in progress), and until it has ended it
 * still holds its locks: the next launch's engine would find its profile's
 * databases locked (Chromium's localStorage then runs in memory only). */
async function waitGone(
  pids: Map<number, string>,
  ms = 10_000,
): Promise<number[]> {
  const until = Date.now() + ms;
  for (;;) {
    const t = await processTable();
    const left = [...pids].filter(([pid, start]) => {
      const p = t.get(pid);
      return p?.start === start && !p.state.startsWith("Z");
    }).map(([pid]) => pid);
    if (!left.length || Date.now() > until) return left;
    await sleep(50);
  }
}

function signal(pid: number, sig: Deno.Signal) {
  if (pid <= 1 || pid === Deno.pid) return;
  try {
    Deno.kill(pid, sig);
  } catch { /* gone */ }
}

/** A launched process and every descendant seen while it ran, each with its
 * start time. A descendant reparented after its parent exited (to init or a
 * subreaper) is still tracked once it was seen. */
export class ProcessTree {
  readonly seen = new Map<number, string>();
  #timer: ReturnType<typeof setInterval> | undefined;
  #busy = false;

  constructor(readonly root: number) {}

  /** Take a snapshot and add every live descendant of a tracked process. */
  async track(): Promise<Map<number, ProcInfo>> {
    const t = await processTable();
    this.absorb(t);
    return t;
  }

  /** Add the descendants of tracked processes in `t`. */
  absorb(t: Map<number, ProcInfo>) {
    const root = t.get(this.root);
    if (!this.seen.has(this.root) && root) this.seen.set(this.root, root.start);
    const children = new Map<number, number[]>();
    for (const [pid, p] of t) {
      const c = children.get(p.ppid);
      if (c) c.push(pid);
      else children.set(p.ppid, [pid]);
    }
    // Walk from every tracked process still alive as the one recorded.
    const queue = [...this.seen].filter(([pid, start]) =>
      t.get(pid)?.start === start
    ).map(([pid]) => pid);
    while (queue.length) {
      for (const c of children.get(queue.pop()!) ?? []) {
        if (c === Deno.pid || this.seen.has(c)) continue;
        this.seen.set(c, t.get(c)!.start);
        queue.push(c);
      }
    }
  }

  /** The tracked processes alive in `t` (same pid, same start time). */
  alive(t: Map<number, ProcInfo>): number[] {
    return [...this.seen].filter(([pid, start]) => t.get(pid)?.start === start)
      .map(([pid]) => pid);
  }

  /** Snapshot every `ms` until stopPolling() (or kill). */
  poll(ms = 250) {
    if (this.#timer !== undefined) return;
    const timer = setInterval(() => {
      if (this.#busy) return;
      this.#busy = true;
      this.track().catch(() => {}).finally(() => this.#busy = false);
    }, ms);
    Deno.unrefTimer(timer);
    this.#timer = timer;
  }

  stopPolling() {
    clearInterval(this.#timer);
    this.#timer = undefined;
  }
}

/** SIGSTOP every live tracked process (nothing forks or is reparented while
 * the tree is walked), re-walk until no new descendant appears, then SIGKILL
 * every tracked process whose start time still matches. */
async function killTree(tree: ProcessTree) {
  tree.stopPolling();
  let t = await tree.track();
  const stopped = new Set<number>();
  for (let round = 0; round < 20; round++) {
    const fresh = tree.alive(t).filter((pid) => !stopped.has(pid));
    if (!fresh.length) break;
    for (const pid of fresh) {
      signal(pid, "SIGSTOP");
      stopped.add(pid);
    }
    t = await tree.track();
  }
  const doomed = new Map<number, string>();
  for (const pid of tree.alive(t)) {
    signal(pid, "SIGKILL");
    doomed.set(pid, tree.seen.get(pid)!);
  }
  const left = await waitGone(doomed);
  if (left.length) log(`kill: still running after SIGKILL: ${left.join(" ")}`);
}

/** Kill `l` and everything it started. */
export async function kill(l: Launched | number, artifact?: string) {
  const pid = typeof l === "number" ? l : l.pid;
  if (OS === "windows") {
    await sh("taskkill", ["/F", "/T", "/PID", String(pid)]);
  } else {
    if (typeof l !== "number" && l.status !== null) {
      // The app ended by itself: what it started (Chromium's storage service
      // committing the profile, say) is given a moment to end too, as after a
      // user's quit, rather than being killed mid-write.
      l.tree.stopPolling();
      const t = await l.tree.track();
      await waitGone(
        new Map(l.tree.alive(t).map((p) => [p, l.tree.seen.get(p)!])),
        5_000,
      );
    }
    await killTree(typeof l === "number" ? new ProcessTree(l) : l.tree);
  }
  if (artifact) await killByPath(artifact);
  if (typeof l !== "number") await waitExit(l, 10_000);
}

/** The pids whose executable lives under `dir` (Linux: the /proc/<pid>/exe
 * link, which a /proc/self/exe re-exec keeps; macOS: ps's comm, the
 * executable's path as launched). */
export async function pidsUnder(dir: string): Promise<number[]> {
  const roots = new Set([dir.replace(/\/+$/, "")]);
  roots.add((await Deno.realPath(dir).catch(() => dir)).replace(/\/+$/, ""));
  const under = (exe: string) =>
    [...roots].some((d) => exe === d || exe.startsWith(d + "/"));
  const pids: number[] = [];
  if (OS === "linux") {
    for await (const e of Deno.readDir("/proc")) {
      if (!/^\d+$/.test(e.name)) continue;
      const exe = await Deno.readLink(`/proc/${e.name}/exe`).catch(() => "");
      if (exe && under(exe)) pids.push(Number(e.name));
    }
  } else {
    const r = await sh("ps", ["-A", "-o", "pid=,comm="]);
    for (const line of r.out.split("\n")) {
      const m = line.match(/^\s*(\d+)\s+(.+?)\s*$/);
      if (m && under(m[2])) pids.push(Number(m[1]));
    }
  }
  return pids.filter((p) => p !== Deno.pid);
}

/** Kill every process whose executable lives under `dir`. */
export async function killByPath(dir: string) {
  if (OS === "windows") {
    const ps = `Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith('${
      dir.replace(/'/g, "''")
    }', 'OrdinalIgnoreCase') } | Stop-Process -Force`;
    await sh("powershell", ["-NoProfile", "-Command", ps]);
    return;
  }
  // Stop them first, then kill only what still runs from `dir` with the
  // start time it had: a pid that exited and was reused in between is let go.
  const before = await processTable();
  const pids = await pidsUnder(dir);
  if (!pids.length) return;
  for (const pid of pids) signal(pid, "SIGSTOP");
  const after = await processTable();
  const still = new Set(await pidsUnder(dir));
  const doomed = new Map<number, string>();
  for (const pid of pids) {
    const start = before.get(pid)?.start;
    const same = start !== undefined && after.get(pid)?.start === start;
    signal(pid, same && still.has(pid) ? "SIGKILL" : "SIGCONT");
    if (same && still.has(pid)) doomed.set(pid, start);
  }
  const left = await waitGone(doomed);
  if (left.length) {
    log(`killByPath: still running after SIGKILL: ${left.join(" ")}`);
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
  /** The launch token (`DENEXT_E2E_LAUNCH`) of the launch that wrote it;
   * null for an app the OS started. */
  launch?: string | null;
  params: Record<string, unknown>;
  checks: Check[];
  data: Record<string, any>;
  done: boolean;
  file: string;
}

/** Where the apps of this run write their results: `$DENEXT_E2E_DIR`, else
 * a directory of this run's own under WORK_DIR, baked into every app it
 * packages (packageApp). A directory shared by every run on the machine (the
 * old `~/.denext-e2e`) let one run collect another's result, and read its
 * params: e.g. a node:http app's result under the devswitch launch's label. */
export const DIR = Deno.env.get("DENEXT_E2E_DIR") ??
  (Deno.env.get("WORK_DIR")
    ? path(Deno.env.get("WORK_DIR")!, "e2e-results")
    : e2eDir());

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
    /** Results to skip ({@linkcode seenResults}). */
    seen?: Set<string>;
    pid?: number;
    until?: (r: AppResult) => boolean;
    ms?: number;
    /** Only a result of this launch (see {@linkcode ofLaunch}). */
    launch?: string;
  } = {},
): Promise<AppResult | null> {
  const until = opts.until ?? ((r) => r.done);
  const end = Date.now() + (opts.ms ?? 90_000);
  let last: AppResult | null = null;
  while (Date.now() < end) {
    for (const r of await results(area)) {
      if (opts.seen?.has(resultKey(r))) continue;
      if (opts.pid !== undefined && r.pid !== opts.pid) continue;
      if (opts.launch !== undefined && !ofLaunch(r, opts.launch)) continue;
      last = r;
      if (until(r)) return r;
    }
    await sleep(250);
  }
  return opts.until ? null : last && last.done ? last : null;
}

/** Whether `r` may be launch `token`'s result: it carries that token, or
 * none (an app the OS started, which inherits nothing from the runner).
 * Another launch's result, which a new pid alone would not tell apart, never
 * is. */
export function ofLaunch(r: AppResult, token: string): boolean {
  return !r.launch || r.launch === token;
}

/** One app run's result: its pid and when it started. A pid alone doesn't
 * tell runs apart: Windows hands a pid to a new process seconds after the
 * last one with it ended, and the new run's result then replaces the old
 * one's file (`<area>-<pid>.json`). Keyed by pid, a launch's result looked
 * like one written before it and was skipped until the wait ran out (the
 * exit area on Windows, Brainwires/deno run 37720814135: launch #5 got #2's
 * pid). */
export function resultKey(r: Pick<AppResult, "pid" | "startedAt">): string {
  return `${r.pid}@${r.startedAt}`;
}

/** The results of `area` written so far ({@linkcode resultKey}s): what a
 * launch about to start must not take for its own. */
export async function seenResults(area: string): Promise<Set<string>> {
  return new Set((await results(area)).map(resultKey));
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
  // deno-lint-ignore no-console
  console.log(s);
}

let collectSeq = 0;

/** Run one launch: start, wait for a done result (or the timeout), wait for
 * the exit, kill whatever is left, and merge the app's checks. The result is
 * the one this launch wrote (its `DENEXT_E2E_LAUNCH` token), never another
 * app's that appeared meanwhile. */
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
  const seen = await seenResults(rep.area);
  const token = `${env.nonce}-${++collectSeq}`;
  const l = await launch(env, p.exe, opts.args, {
    cwd: opts.cwd,
    env: { ...opts.env, DENEXT_E2E_LAUNCH: token },
  });
  const r = await waitResult(rep.area, {
    seen,
    launch: token,
    ms: opts.ms ?? 90_000,
  });
  if (!r) {
    const partial = (await results(rep.area)).filter((x) =>
      !seen.has(resultKey(x)) && ofLaunch(x, token)
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
    const final = (await results(rep.area)).find((x) =>
      x.pid === r.pid && ofLaunch(x, token)
    ) ?? r;
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
