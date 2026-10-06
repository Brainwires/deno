// Copyright 2018-2026 the Deno authors. MIT license.
// The in-app half of the denext runtime e2e harness (../../run.ts is the
// other half). Every test app imports this.
//
// An app reads what to do from `<dir>/<area>.params.json` (written by the
// runner before each launch; a launch the OS starts, such as a deep link,
// inherits no environment from the runner, so nothing is passed through the
// environment) and writes `<dir>/<area>-<pid>.json` as it goes: a list of
// checks, each `pass`, `fail` or `n/a` (with the reason), plus free-form
// data. `<dir>` is `$DENEXT_E2E_DIR`, else the directory of the run that
// packaged the app (e2e_run.ts, written by the runner), else
// `~/.denext-e2e`: the same for the runner and the app, and never shared by
// two runs on one machine. A launch the runner starts itself also carries
// `$DENEXT_E2E_LAUNCH`, which the result records, so the runner tells that
// launch's result from any other app's.
//
// The desktop APIs under test are not in the stock CLI's type declarations
// (apps are packaged with --no-check), so they are reached through `any`.

// deno-lint-ignore-file no-explicit-any

import { RUN_DIR } from "./e2e_run.ts";

export const desktop: any = (Deno as any).desktop;
export const BrowserWindow: any = (Deno as any).BrowserWindow;

export const OS = Deno.build.os;

export function e2eDir(): string {
  const explicit = Deno.env.get("DENEXT_E2E_DIR");
  if (explicit) return explicit;
  if (RUN_DIR) return RUN_DIR;
  // A launch the OS starts on Windows (a registered scheme) has USERPROFILE
  // but not necessarily HOME; prefer it there so every launch agrees.
  const home =
    (Deno.build.os === "windows"
      ? Deno.env.get("USERPROFILE") ?? Deno.env.get("HOME")
      : Deno.env.get("HOME")) ?? ".";
  return `${home}/.denext-e2e`;
}

export type Status = "pass" | "fail" | "n/a";

export interface Check {
  name: string;
  status: Status;
  detail?: unknown;
  reason?: string;
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Poll `f` every 50 ms until it is true or `ms` passed. */
export async function waitFor(
  f: () => boolean | Promise<boolean>,
  ms = 10000,
): Promise<boolean> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await f()) return true;
    await sleep(50);
  }
  return await f();
}

/** Resolve when `target` fires `type` (true), or after `ms` (false). */
export function once(target: EventTarget, type: string, ms = 8000) {
  return new Promise<Event | null>((resolve) => {
    const t = setTimeout(() => resolve(null), ms);
    target.addEventListener(type, (e) => {
      clearTimeout(t);
      resolve(e);
    }, { once: true });
  });
}

/** The error a promise rejects with, or null when it resolves. */
export async function errorOf(
  p: Promise<unknown> | (() => unknown),
): Promise<(Error & { code?: string }) | null> {
  try {
    await (typeof p === "function" ? p() : p);
    return null;
  } catch (e) {
    return e as Error & { code?: string };
  }
}

export function describeError(e: unknown): string {
  if (e instanceof Error) {
    const code = (e as Error & { code?: string }).code;
    return `${e.name}${code ? `(${code})` : ""}: ${e.message}`;
  }
  return String(e);
}

export class Report {
  readonly area: string;
  readonly file: string;
  readonly params: Record<string, any>;
  readonly checks: Check[] = [];
  readonly data: Record<string, unknown> = {};
  #finished = false;

  constructor(area: string) {
    this.area = area;
    const dir = e2eDir();
    Deno.mkdirSync(dir, { recursive: true });
    this.file = `${dir}/${area}-${Deno.pid}.json`;
    let params: Record<string, any> = {};
    try {
      params = JSON.parse(Deno.readTextFileSync(`${dir}/${area}.params.json`));
    } catch { /* no params: the defaults */ }
    this.params = params;
    this.write();
    // An uncaught error still leaves a result the runner can show.
    globalThis.addEventListener("error", (e) => {
      this.fail("uncaught error", describeError((e as ErrorEvent).error));
      this.data.error = describeError((e as ErrorEvent).error);
      this.write();
    });
    globalThis.addEventListener("unhandledrejection", (e) => {
      this.fail("unhandled rejection", describeError(e.reason));
      this.write();
    });
  }

  write() {
    const body = {
      area: this.area,
      pid: Deno.pid,
      os: OS,
      arch: Deno.build.arch,
      startedAt: startedAt,
      launch,
      params: this.params,
      checks: this.checks,
      data: this.data,
      done: this.#finished,
    };
    const tmp = `${this.file}.tmp`;
    Deno.writeTextFileSync(
      tmp,
      JSON.stringify(
        body,
        (_k, v) => typeof v === "bigint" ? `${v}n` : v,
        2,
      ),
    );
    Deno.renameSync(tmp, this.file);
  }

  check(name: string, ok: boolean, detail?: unknown): boolean {
    this.checks.push({
      name,
      status: ok ? "pass" : "fail",
      ...(detail === undefined ? {} : { detail }),
    });
    this.write();
    return ok;
  }

  fail(name: string, detail?: unknown) {
    this.check(name, false, detail);
  }

  /** A check that cannot run here, and why (never a silent skip). */
  na(name: string, reason: string) {
    this.checks.push({ name, status: "n/a", reason });
    this.write();
  }

  /** Run `f`; a throw is a failed check named `name`. */
  async step(name: string, f: () => Promise<void> | void) {
    try {
      await f();
    } catch (e) {
      this.fail(`${name}: threw`, describeError(e));
    }
  }

  /** Where the app is (shown when it times out). */
  mark(stage: string) {
    const stages = (this.data.stages ??= []) as string[];
    stages.push(`${new Date().toISOString()} ${stage}`);
    this.write();
  }

  set(key: string, value: unknown) {
    this.data[key] = value;
    this.write();
  }

  /** Mark the result complete. Does not exit. */
  done() {
    this.#finished = true;
    this.write();
  }

  /** Mark the result complete and exit the app. */
  finish(code = 0): never {
    this.done();
    Deno.exit(code);
  }
}

const startedAt = new Date().toISOString();
/** The runner's token for this launch (none when the OS started it). */
const launch = Deno.env.get("DENEXT_E2E_LAUNCH") ?? null;

/** `p`'s value, or `timeout` after `ms` (a hang becomes a failed check,
 * not a stuck app). */
export async function within<T>(
  p: Promise<T>,
  ms: number,
): Promise<{ value: T } | { timeout: true }> {
  let t: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      p.then((value) => ({ value })),
      new Promise<{ timeout: true }>((resolve) => {
        t = setTimeout(() => resolve({ timeout: true }), ms);
      }),
    ]);
  } finally {
    clearTimeout(t);
  }
}

/** A BrowserWindow whose page keeps `title` as its document title, so the
 * native window title (which engines take from the page) is `title` too:
 * the input helpers find windows by title. The app's handler must call
 * {@linkcode shared} first. */
export function titledWindow(
  title: string,
  options: Record<string, unknown> = {},
): any {
  const w = new BrowserWindow({ title, width: 600, height: 400, ...options });
  const origin = Deno.env.get("DENO_DESKTOP_APP_ORIGIN") ?? "app://localhost";
  w.navigate(`${origin}/__e2e/titled?t=${encodeURIComponent(title)}`);
  return w;
}

/** The harness's own routes; null for the app's. */
export function shared(req: Request): Response | null {
  const url = new URL(req.url);
  if (url.pathname === "/__e2e/titled") {
    const t = url.searchParams.get("t") ?? "";
    return html(page(t.replace(/[<&]/g, "")));
  }
  return null;
}

export function page(title: string, body = "", script = ""): string {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title></head>` +
    `<body><h1>${title}</h1>${body}${
      script ? `<script type="module">${script}</script>` : ""
    }</body></html>`;
}

export function html(body: string): Response {
  return new Response(body, {
    headers: { "content-type": "text/html; charset=utf-8" },
  });
}

// A 1x1 PNG.
export const PNG = new Uint8Array([
  0x89,
  0x50,
  0x4E,
  0x47,
  0x0D,
  0x0A,
  0x1A,
  0x0A,
  0x00,
  0x00,
  0x00,
  0x0D,
  0x49,
  0x48,
  0x44,
  0x52,
  0x00,
  0x00,
  0x00,
  0x01,
  0x00,
  0x00,
  0x00,
  0x01,
  0x08,
  0x06,
  0x00,
  0x00,
  0x00,
  0x1F,
  0x15,
  0xC4,
  0x89,
  0x00,
  0x00,
  0x00,
  0x0A,
  0x49,
  0x44,
  0x41,
  0x54,
  0x78,
  0x9C,
  0x63,
  0x00,
  0x01,
  0x00,
  0x00,
  0x05,
  0x00,
  0x01,
  0x0D,
  0x0A,
  0x2D,
  0xB4,
  0x00,
  0x00,
  0x00,
  0x00,
  0x49,
  0x45,
  0x4E,
  0x44,
  0xAE,
  0x42,
  0x60,
  0x82,
]);
