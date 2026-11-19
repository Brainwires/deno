// Copyright 2018-2026 the Deno authors. MIT license.
import {
  delay,
  join,
  ROOT_PATH,
  TextLineStream,
  toFileUrl,
} from "../../../tools/util.js";
import {
  assert,
  denoBinary,
  ManifestTestOptions,
  runPy,
  wantsDocument,
} from "./utils.ts";
import { DOMParser } from "jsr:@b-fuze/deno-dom@0.1.56/wasm";

export async function runWithTestUtil<T>(
  verbose: boolean,
  f: () => Promise<T>,
): Promise<T> {
  const proc = runPy([
    "wpt",
    "serve",
    "--config",
    "../../../tests/wpt/runner/config.json",
  ], {
    stdout: verbose ? "inherit" : "piped",
    stderr: verbose ? "inherit" : "piped",
  });

  const start = performance.now();
  while (true) {
    await delay(1000);
    try {
      const req = await fetch("http://localhost:8000/");
      await req.body?.cancel();
      if (req.status == 200) {
        break;
      }
    } catch (_err) {
      // do nothing if this fails
    }
    const passedTime = performance.now() - start;
    if (passedTime > 15000) {
      try {
        proc.kill("SIGINT");
      } catch {
        // Might have already died
      }
      await proc.status;
      throw new Error("Timed out while trying to start wpt test util.");
    }
  }

  if (verbose) console.log(`Started wpt test util.`);

  try {
    return await f();
  } finally {
    if (verbose) console.log("Killing wpt test util.");
    try {
      proc.kill("SIGINT");
    } catch {
      // Might have already died
    }
    await proc.status;
  }
}

export interface TestResult {
  cases: TestCaseResult[];
  harnessStatus: TestHarnessStatus | null;
  duration: number;
  status: number;
  stderr: string;
  /** The runner stopped the test process because it ran out of time. */
  timedOut?: boolean;
}

export interface TestHarnessStatus {
  status: number;
  message: string | null;
  stack: string | null;
}

export interface TestCaseResult {
  name: string;
  passed: boolean;
  status: number;
  message: string | null;
  stack: string | null;
}

export async function runSingleTest(
  url: URL,
  _options: ManifestTestOptions,
  reporter: (result: TestCaseResult) => void,
  inspectBrk: boolean,
  timeouts: { long: number; default: number },
  fileTimeout = 2 * 60_000,
): Promise<TestResult> {
  const abort = new AbortController();
  let backstop: number | undefined;
  try {
    return await Promise.race([
      runSingleTestInner(
        url,
        _options,
        reporter,
        inspectBrk,
        timeouts,
        abort.signal,
      ),
      new Promise<TestResult>((resolve) => {
        backstop = setTimeout(() => {
          // Stop the test process too, not only the wait for it.
          abort.abort();
          resolve({
            cases: [],
            harnessStatus: null,
            duration: fileTimeout,
            status: 1,
            stderr: `test file timed out after ${fileTimeout / 1000}s\n`,
            timedOut: true,
          });
        }, fileTimeout);
      }),
    ]);
  } finally {
    clearTimeout(backstop);
  }
}

async function runSingleTestInner(
  url: URL,
  _options: ManifestTestOptions,
  reporter: (result: TestCaseResult) => void,
  inspectBrk: boolean,
  timeouts: { long: number; default: number },
  signal: AbortSignal,
): Promise<TestResult> {
  const timeout = _options.timeout === "long"
    ? timeouts.long
    : timeouts.default;
  const filename = url.pathname.substring(
    url.pathname.lastIndexOf("/") + 1,
    url.pathname.indexOf("."),
  );
  const { title } = Object.fromEntries(_options.script_metadata || []);
  const bundle = await generateBundle(url, wantsDocument(url.pathname));
  const tempFile = await Deno.makeTempFile({
    prefix: "wpt-bundle-",
    suffix: ".js",
  });

  let interval;
  try {
    await Deno.writeTextFile(tempFile, bundle);

    const startTime = new Date().getTime();

    const args = [
      "run",
      "-A",
      "--unstable-webgpu",
      "--unstable-net",
      "--v8-flags=--expose-gc",
    ];

    if (inspectBrk) {
      args.push("--inspect-brk");
    }

    args.push(
      "--enable-testing-features-do-not-use",
      "--location",
      url.toString(),
      "--cert",
      join(ROOT_PATH, `./tests/wpt/runner/certs/cacert.pem`),
      tempFile,
      "[]",
    );

    const start = performance.now();
    const proc = new Deno.Command(denoBinary(), {
      args,
      env: {
        NO_COLOR: "1",
        // The fake camera and microphone of `navigator.mediaDevices`, as WPT
        // runs browsers with `--use-fake-device-for-media-stream`.
        DENO_FAKE_MEDIA_DEVICES: "1",
      },
      signal,
      stdout: "null",
      stderr: "piped",
    }).spawn();
    let timedOut = false;

    const cases = [];
    let stderr = "";

    let harnessStatus = null;

    const lines = proc.stderr.pipeThrough(new TextDecoderStream()).pipeThrough(
      new TextLineStream(),
    );
    interval = setInterval(() => {
      const passedTime = performance.now() - start;
      if (passedTime > timeout) {
        timedOut = true;
        try {
          proc.kill("SIGINT");
        } catch {
          // Race: proc may have terminated between the for-await loop
          // draining stderr and `clearInterval` running in `finally`.
        }
      }
    }, 1000);
    for await (const line of lines as AsyncIterable<string>) {
      if (line.startsWith("{")) {
        const data = JSON.parse(line);
        const result = { ...data, passed: data.status == 0 };
        if (/^Untitled( \d+)?$/.test(result.name)) {
          result.name = `${title || filename}${result.name.slice(8)}`;
        }
        cases.push(result);
        reporter(result);
      } else if (line.startsWith("#$#$#{")) {
        harnessStatus = JSON.parse(line.slice(5));
      } else {
        stderr += line + "\n";
      }
    }

    const duration = new Date().getTime() - startTime;

    const { code } = await proc.status;
    return {
      status: code,
      harnessStatus,
      duration,
      cases,
      stderr,
      // A test that finished its harness before the kill landed did not time
      // out.
      timedOut: timedOut && harnessStatus === null,
    };
  } finally {
    clearInterval(interval);
    await Deno.remove(tempFile);
  }
}

function getShim(test: string): string {
  const shim = [];

  shim.push("globalThis.window = globalThis;");

  if (test.includes("streams/transferable")) {
    shim.push(`
      {
        const { port1, port2 } = new MessageChannel();
        port2.addEventListener('message', (e) => {
          queueMicrotask(() => {
            globalThis.dispatchEvent(e);
          });
        });
        port2.start();
        globalThis.postMessage = (message, targetOriginOrOptions, transfer) => {
          let options = targetOriginOrOptions;
          if (transfer || typeof targetOriginOrOptions === 'string') {
            options = { transfer };
          }
          return port1.postMessage(message, options);
        };
      }
    `);
  }

  return shim.join("\n");
}

/** A runner file under `tests/wpt/runner/`, read once. */
const runnerFiles = new Map<string, Promise<string | null>>();
function readRunnerFile(name: string): Promise<string | null> {
  let file = runnerFiles.get(name);
  if (!file) {
    file = Deno.readTextFile(join(ROOT_PATH, "./tests/wpt/runner", name))
      .catch((err) => {
        if (err instanceof Deno.errors.NotFound) return null;
        throw err;
      });
    runnerFiles.set(name, file);
  }
  return file;
}

function runnerFileUrl(name: string): string {
  return toFileUrl(join(ROOT_PATH, "./tests/wpt/runner", name)).href;
}

/**
 * One node of the test file's own markup, in document order, for the document
 * environment to instantiate (see `loadMarkup` in dom_shim.js): `p` is the
 * parent's index (-1 for `<html>`), `t` an element's local name and `a` its
 * attributes, `x` a text node's data.
 */
interface MarkupNode {
  p: number;
  t?: string;
  a?: [string, string][];
  x?: string;
}

// deno-lint-ignore no-explicit-any
type ParsedNode = any;

/**
 * Flattens the parsed page under `<html>` into `MarkupNode`s (elements and
 * non-blank text; no comments) and returns them with each `<script>` element
 * and its index, in document order.
 */
function flattenMarkup(
  documentElement: ParsedNode,
): { nodes: MarkupNode[]; scripts: { element: ParsedNode; index: number }[] } {
  const nodes: MarkupNode[] = [];
  const scripts: { element: ParsedNode; index: number }[] = [];
  const visit = (node: ParsedNode, parent: number) => {
    if (node.nodeType === 3) {
      const data = String(node.nodeValue ?? "");
      if (data.trim() !== "") nodes.push({ p: parent, x: data });
      return;
    }
    if (node.nodeType !== 1) return;
    const index = nodes.length;
    const attributes: [string, string][] = [];
    for (const name of node.getAttributeNames() as string[]) {
      attributes.push([name, String(node.getAttribute(name) ?? "")]);
    }
    const localName = String(node.localName ?? node.tagName).toLowerCase();
    nodes.push(
      attributes.length > 0
        ? { p: parent, t: localName, a: attributes }
        : { p: parent, t: localName },
    );
    if (localName === "script") scripts.push({ element: node, index });
    for (const child of node.childNodes) visit(child, index);
  };
  if (documentElement) visit(documentElement, -1);
  return { nodes, scripts };
}

/** A bundle step that calls one of the document environment's runner hooks. */
function documentHook(name: string, ...args: unknown[]): string {
  return `document[Symbol.for(${JSON.stringify(`deno.wpt.${name}`)})](${
    args.map((arg) => JSON.stringify(arg)).join(", ")
  });`;
}

/**
 * The test file as one program: its `<script>`s in document order, with the
 * runner's `testharnessreport.js` and `testdriver-vendor.js` in place of the
 * suite's. With `useDocument`, the document environment (dom_shim.js, then
 * media_env.js when present) is installed right after `testharness.js`, the
 * page's own markup is instantiated into it up to each script before that
 * script runs (as an HTML parser would have), and the rest after the last one.
 */
async function generateBundle(
  location: URL,
  useDocument: boolean,
): Promise<string> {
  const res = await fetch(location);
  const body = await res.text();
  const doc = new DOMParser().parseFromString(body, "text/html");
  assert(doc, "document should have been parsed");
  const { nodes, scripts } = flattenMarkup(doc.documentElement);
  const title = doc.getElementsByTagName("title")[0]?.childNodes[0]?.nodeValue;
  const scriptContents: [string, string][] = [];
  let inlineScriptCount = 0;
  if (title) {
    const url = new URL(`#${inlineScriptCount}`, location);
    inlineScriptCount++;
    scriptContents.push([
      url.href,
      `globalThis.META_TITLE=${JSON.stringify(title)}`,
    ]);
  }
  const shim = getShim(location.pathname);
  // Once the document environment is installed, each script sees its own
  // element as `document.currentScript` while it runs.
  let domInstalled = false;
  const runScript = (url: string, index: number, contents: string) => {
    scriptContents.push([url, shim]);
    if (domInstalled) {
      scriptContents.push([url, documentHook("setCurrentScript", index)]);
    }
    scriptContents.push([url, contents]);
    if (domInstalled) {
      scriptContents.push([url, documentHook("setCurrentScript")]);
    }
  };
  const installDocument = async () => {
    scriptContents.push([
      runnerFileUrl("dom_shim.js"),
      (await readRunnerFile("dom_shim.js"))!,
    ]);
    // The media elements (`<video>` / `<audio>`) over the runtime's received
    // frames, when the runner has them.
    const mediaEnv = await readRunnerFile("media_env.js");
    if (mediaEnv !== null) {
      scriptContents.push([runnerFileUrl("media_env.js"), mediaEnv]);
    }
    scriptContents.push([
      runnerFileUrl("dom_shim.js") + "#markup",
      documentHook("loadMarkup", nodes),
    ]);
    domInstalled = true;
  };
  for (const { element: script, index } of scripts) {
    const src = script.getAttribute("src");
    if (src) {
      const url = new URL(src, location);
      let contents: string | null = null;
      let scriptUrl = url.href;
      if (url.pathname === "/resources/testharnessreport.js") {
        scriptUrl = runnerFileUrl("testharnessreport.js");
        contents = await readRunnerFile("testharnessreport.js");
      } else if (url.pathname === "/resources/testdriver-vendor.js") {
        // WPT's vendor hook for `test_driver` (see testdriver-vendor.js).
        scriptUrl = runnerFileUrl("testdriver-vendor.js");
        contents = await readRunnerFile("testdriver-vendor.js");
      } else {
        const res = await fetch(url);
        if (res.ok) {
          contents = await res.text();
        } else {
          await res.body?.cancel();
        }
      }
      if (contents !== null) {
        runScript(scriptUrl, index, contents);
        if (
          useDocument && !domInstalled &&
          url.pathname === "/resources/testharness.js"
        ) {
          await installDocument();
        }
      }
    } else {
      const url = new URL(`#${inlineScriptCount}`, location);
      inlineScriptCount++;
      runScript(url.href, index, script.textContent);
    }
  }
  if (domInstalled) {
    scriptContents.push([
      runnerFileUrl("dom_shim.js") + "#markup-end",
      documentHook("finishMarkup"),
    ]);
  }

  return scriptContents.map(([url, contents]) => `
(function() {
  const [_,err] = Deno[Deno.internal].core.evalContext(${
    JSON.stringify(contents)
  }, ${JSON.stringify(url)});
  if (err !== null) {
    throw err?.thrown;
  }
})();`).join("\n");
}
