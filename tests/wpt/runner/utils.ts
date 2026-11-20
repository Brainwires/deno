// Copyright 2018-2026 the Deno authors. MIT license.
/// FLAGS

import { parseArgs } from "@std/cli/parse-args";
import { checkCiHash, join, resolve, ROOT_PATH } from "../../../tools/util.js";

const args = parseArgs(Deno.args, {
  "--": true,
  boolean: [
    "all",
    "quiet",
    "release",
    "no-interactive",
    "inspect-brk",
    "no-ignore",
    "document",
    "no-document",
    "dom-shim",
    "no-hosts",
  ],
  string: ["json", "wptreport", "binary"],
});

export const {
  all,
  json,
  wptreport,
  quiet,
  release,
  rebuild,
  ["--"]: rest,
  ["auto-config"]: autoConfig,
  ["inspect-brk"]: inspectBrk,
  ["no-ignore"]: noIgnore,
  ["no-hosts"]: noHosts,
  binary,
} = args;

/**
 * The runner's document environment (tier T1, see README.md): `"auto"` gives
 * it to the window-scoped tests of `DOCUMENT_SUITES`, `"on"` (`--document`,
 * or its old name `--dom-shim`) to every window-scoped test, and `"off"`
 * (`--no-document`, tier T0) to none.
 */
export const documentMode: "auto" | "on" | "off" = args["no-document"]
  ? "off"
  : args.document || args["dom-shim"]
  ? "on"
  : "auto";

/**
 * The suites whose window-scoped tests get the document environment by
 * default. They test APIs the runtime implements (WebRTC, Media Capture), not
 * the DOM: the environment is plumbing there. A suite about the DOM or HTML
 * itself never gets it by default, since the environment would then be the
 * thing under test.
 */
export const DOCUMENT_SUITES: readonly string[] = [
  "webrtc",
  "webrtc-encoded-transform",
  "webrtc-extensions",
  "webrtc-ice",
  "webrtc-identity",
  "webrtc-priority",
  "webrtc-stats",
  "webrtc-svc",
  "mediacapture-streams",
];

/**
 * Suites whose results are tracked but never fail a run: their failures are
 * listed, and a test file the expectation file doesn't list yet runs as if
 * expected to pass (so `wpt.ts update` can fill the file in).
 */
export const NON_GATING_SUITES: readonly string[] = ["mediacapture-streams"];

/** The top-level suite directory of a test path such as `/webrtc/x.html`. */
export function suiteOf(path: string): string {
  return path.split("/")[1] ?? "";
}

/**
 * Whether a test (its URL path) runs with the document environment. Only a
 * window-scoped test gets it: a suite's own `.html` file or a `.window.js`
 * test. An `.any.js` test is written for every global and a `.worker.js` one
 * for a worker, so neither is defined as a document.
 */
export function wantsDocument(pathname: string): boolean {
  if (documentMode === "off") return false;
  const name = pathname.slice(pathname.lastIndexOf("/") + 1);
  if (/\.any\./.test(name) || /\.worker\.html$/.test(name)) return false;
  return documentMode === "on" ||
    DOCUMENT_SUITES.includes(suiteOf(pathname));
}

export function denoBinary() {
  if (binary) {
    return resolve(binary);
  }
  const ext = Deno.build.os === "windows" ? ".exe" : "";
  return join(
    ROOT_PATH,
    `./target/${release ? "release" : "debug"}/deno${ext}`,
  );
}

/// CI SKIP CACHE

async function getWptSubmoduleHash(): Promise<string> {
  const gitResult = await new Deno.Command("git", {
    args: ["rev-parse", "HEAD"],
    cwd: join(ROOT_PATH, "tests", "wpt", "suite"),
    stdout: "piped",
    stderr: "piped",
  }).output();
  return new TextDecoder().decode(gitResult.stdout).trim();
}

export function checkWptCiHash() {
  return checkCiHash("wpt", async (hasher) => {
    const hashDenoBinaryTask = hasher.hashFile(denoBinary());
    const submoduleHash = await getWptSubmoduleHash();
    await hashDenoBinaryTask;
    hasher.writeSync(submoduleHash);
  });
}

/// WPT TEST MANIFEST

export interface Manifest {
  items: {
    testharness: ManifestFolder;
  };
}
export interface ManifestFolder {
  [key: string]: ManifestFolder | ManifestTest;
}
export type ManifestTest = [
  hash: string,
  ...variations: ManifestTestVariation[],
];
export type ManifestTestVariation = [
  path: string,
  options: ManifestTestOptions,
];
export interface ManifestTestOptions {
  // deno-lint-ignore camelcase
  script_metadata: [string, string][];
  timeout?: string;
}

const MANIFEST_PATH = join(ROOT_PATH, "./tests/wpt/runner/manifest.json");

export async function updateManifest() {
  const status = await runPy(
    [
      "wpt",
      "manifest",
      "--tests-root",
      ".",
      "-p",
      MANIFEST_PATH,
      ...(rebuild ? ["--rebuild"] : []),
    ],
    {},
  ).status;
  assert(status.success, "updating wpt manifest should succeed");
}

export function getManifest(): Manifest {
  const manifestText = Deno.readTextFileSync(MANIFEST_PATH);
  return JSON.parse(manifestText);
}

/// WPT TEST EXPECTATIONS

export const EXPECTATIONS_DIR = join(
  ROOT_PATH,
  "./tests/wpt/runner/expectations",
);

export interface TestExpectation {
  ignore?: boolean;
  expectedFailures?: string[];
  flaky?: boolean;
}

export interface Expectation {
  [key: string]: Expectation | boolean | TestExpectation;
}

export function getExpectation(): Expectation {
  const expectation: Expectation = {};
  for (const entry of Deno.readDirSync(EXPECTATIONS_DIR)) {
    if (
      !entry.isFile || !entry.name.endsWith(".json") ||
      entry.name === "schema.json"
    ) continue;
    const suiteName = entry.name.slice(0, -".json".length);
    const text = Deno.readTextFileSync(join(EXPECTATIONS_DIR, entry.name));
    expectation[suiteName] = JSON.parse(text);
  }
  return expectation;
}

export function saveExpectation(
  expectation: Expectation,
  path?: string,
) {
  if (path) {
    // Write single merged file (used for tmp diff in `run` command)
    Deno.writeTextFileSync(
      path,
      JSON.stringify(expectation, undefined, "  ") + "\n",
    );
    return;
  }

  // Write each top-level key to its own file in the expectations directory
  const existingFiles = new Set<string>();
  for (const entry of Deno.readDirSync(EXPECTATIONS_DIR)) {
    if (entry.isFile && entry.name.endsWith(".json")) {
      existingFiles.add(entry.name);
    }
  }

  const writtenFiles = new Set<string>();
  for (const [key, value] of Object.entries(expectation)) {
    const fileName = `${key}.json`;
    writtenFiles.add(fileName);
    Deno.writeTextFileSync(
      join(EXPECTATIONS_DIR, fileName),
      JSON.stringify(value, undefined, "  ") + "\n",
    );
  }

  // Remove files for keys no longer present (but never remove schema.json)
  for (const fileName of existingFiles) {
    if (!writtenFiles.has(fileName) && fileName !== "schema.json") {
      Deno.removeSync(join(EXPECTATIONS_DIR, fileName));
    }
  }
}

export function getExpectFailForCase(
  expectation: boolean | TestExpectation,
  caseName: string,
): boolean {
  if (noIgnore) return false;
  if (typeof expectation === "boolean") {
    return !expectation;
  }
  return expectation.expectedFailures?.includes(caseName) ?? false;
}

/// UTILS

class AssertionError extends Error {
  override name = "AssertionError";
  constructor(message: string) {
    super(message);
  }
}

export function assert(condition: unknown, message: string): asserts condition {
  if (!condition) {
    throw new AssertionError(message);
  }
}

export function runPy<T extends Omit<Deno.CommandOptions, "cwd">>(
  args: string[],
  options: T,
): Deno.ChildProcess {
  const cmd = Deno.build.os == "windows" ? "python.exe" : "python3";
  return new Deno.Command(cmd, {
    args,
    stdout: "inherit",
    stderr: "inherit",
    ...options,
    cwd: join(ROOT_PATH, "./tests/wpt/suite/"),
  }).spawn();
}

export async function runGitDiff(args: string[]): Promise<void> {
  await new Deno.Command("git", {
    args: ["diff", ...args],
    stdout: "inherit",
    stderr: "inherit",
    cwd: ROOT_PATH,
  }).output();
}

export async function checkPy3Available() {
  const { success, stdout, stderr } = await runPy(["--version"], {
    stdout: "piped",
    stderr: "piped",
  }).output();
  assert(success, "failed to run python --version");
  // Python 2 printed its version to stderr.
  const output = new TextDecoder().decode(stdout) +
    new TextDecoder().decode(stderr);
  const match = output.match(/Python (\d+)\.(\d+)/);
  const [major, minor] = match ? [Number(match[1]), Number(match[2])] : [0, 0];
  // The suite's `wpt` script asks for 3.9 or newer; 3.11 is the floor here,
  // the oldest the runner is used with. (An earlier 3.11-only check was for
  // https://github.com/web-platform-tests/wpt/issues/44427 on an older
  // suite.)
  assert(
    major === 3 && minor >= 11,
    `The ${
      Deno.build.os == "windows" ? "python.exe" : "python3"
    } in your path is ${
      match ? match[0] : "not Python"
    }; the WPT server needs Python 3.11 or newer.`,
  );
}

export async function cargoBuild() {
  if (binary) return;
  const { success } = await new Deno.Command("cargo", {
    args: ["build", ...(release ? ["--release"] : [])],
    cwd: ROOT_PATH,
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  assert(success, "cargo build failed");
}

export function escapeLoneSurrogates(input: string): string;
export function escapeLoneSurrogates(input: string | null): string | null;
export function escapeLoneSurrogates(input: string | null): string | null {
  if (input === null) return null;
  return input.replace(
    /[\uD800-\uDFFF]/gu,
    (match) => `U+${match.charCodeAt(0).toString(16)}`,
  );
}

/// WPTREPORT

export async function generateRunInfo(): Promise<unknown> {
  const oses: Record<string, string> = {
    "windows": "win",
    "darwin": "mac",
    "linux": "linux",
    "freebsd": "freebsd",
    "openbsd": "openbsd",
  };
  const proc = await new Deno.Command("git", {
    args: ["rev-parse", "HEAD"],
    cwd: join(ROOT_PATH, "tests", "wpt", "suite"),
    stderr: "inherit",
  }).output();
  const revision = (new TextDecoder().decode(proc.stdout)).trim();
  const proc2 = await new Deno.Command(denoBinary(), {
    args: ["eval", "console.log(JSON.stringify(Deno.version))"],
    cwd: join(ROOT_PATH, "tests", "wpt", "suite"),
  }).output();
  const version = JSON.parse(new TextDecoder().decode(proc2.stdout));
  const runInfo = {
    "os": oses[Deno.build.os],
    "processor": Deno.build.arch,
    "version": "unknown",
    "os_version": "unknown",
    "bits": 64,
    "has_sandbox": true,
    "webrender": false,
    "automation": false,
    "linux_distro": "unknown",
    "revision": revision,
    "python_version": 3,
    "product": "deno",
    "debug": false,
    "browser_version": version.deno,
    "browser_channel": version.deno.includes("+") ? "canary" : "stable",
    "verify": false,
    "wasm": false,
    "headless": true,
  };
  return runInfo;
}
