// Copyright 2018-2026 the Deno authors. MIT license.
(function () {
const { primordials } = __bootstrap;
const {
  SafeMap,
  ArrayPrototypeForEach,
  ArrayPrototypePush,
  ArrayPrototypeConcat,
  ArrayPrototypeSlice,
  StringPrototypeIndexOf,
  StringPrototypeSlice,
  StringPrototypeStartsWith,
} = primordials;

// This module ports:
// - https://github.com/nodejs/node/blob/master/src/node_options-inl.h
// - https://github.com/nodejs/node/blob/master/src/node_options.cc
// - https://github.com/nodejs/node/blob/master/src/node_options.h

// Quote-aware tokenizer for NODE_OPTIONS. Node.js uses a shell-like parser
// that respects single and double quotes, so `--title="hello world"` is a
// single token whose value is `hello world`, not two tokens.
// denext: inside double quotes a backslash escapes the next character, as in
// Node's `ParseNodeOptionsEnvVar` (and the CLI's parser), so a quoted value
// holding `"` can't end the quote early and add tokens.
function splitNodeOptions(input: string): string[] {
  const args: string[] = [];
  let current = "";
  let inDouble = false;
  let inSingle = false;

  for (let i = 0; i < input.length; i++) {
    const ch = input[i];
    if (ch === "\\" && inDouble && i + 1 < input.length) {
      i++;
      current += input[i];
    } else if (ch === '"' && !inSingle) {
      inDouble = !inDouble;
    } else if (ch === "'" && !inDouble) {
      inSingle = !inSingle;
    } else if (
      (ch === " " || ch === "\t" || ch === "\n" || ch === "\r") && !inDouble &&
      !inSingle
    ) {
      if (current.length > 0) {
        ArrayPrototypePush(args, current);
        current = "";
      }
    } else {
      current += ch;
    }
  }
  if (current.length > 0) {
    ArrayPrototypePush(args, current);
  }
  return args;
}

/** Gets the all options for Node.js
 * This function is expensive to execute. `getOptionValue` in `internal/options.ts`
 * should be used instead to get a specific option. */
type OptionValue = { value: string | boolean };

let optionsMap: Map<string, OptionValue> | undefined;
let execArgvOptionsMap: Map<string, OptionValue> | undefined;
let execArgvSnapshot: string[] | undefined;

function setOptionSourceExecArgv(execArgv: string[]) {
  execArgvSnapshot = ArrayPrototypeSlice(execArgv);
  optionsMap = undefined;
  execArgvOptionsMap = undefined;
}

function createDefaultOptions() {
  return new SafeMap([
    ["--warnings", { value: true }],
    ["--pending-deprecation", { value: false }],
    ["--expose-internals", { value: false }],
    ["--title", { value: "" }],
  ]);
}

function parseOption(options: Map<string, OptionValue>, arg: string) {
  if (StringPrototypeStartsWith(arg, "--title=")) {
    options.set("--title", { value: StringPrototypeSlice(arg, 8) });
    return;
  }
  if (StringPrototypeStartsWith(arg, "--tls-cipher-list=")) {
    options.set("--tls-cipher-list", {
      value: StringPrototypeSlice(arg, "--tls-cipher-list=".length),
    });
    return;
  }
  if (StringPrototypeStartsWith(arg, "--max-http-header-size=")) {
    options.set("--max-http-header-size", {
      value: StringPrototypeSlice(arg, "--max-http-header-size=".length),
    });
    return;
  }
  switch (arg) {
    case "--insecure-http-parser":
      options.set("--insecure-http-parser", { value: true });
      break;
    case "--no-insecure-http-parser":
      options.set("--insecure-http-parser", { value: false });
      break;
    case "--no-warnings":
      options.set("--warnings", { value: false });
      break;
    // denext: Node's permission model. The node shim maps the grants onto
    // Deno's flags and passes these along for `process.permission`.
    case "--permission":
    case "--experimental-permission":
    case "--allow-worker":
    case "--allow-wasi":
    case "--allow-inspector":
    case "--allow-child-process":
    case "--allow-addons":
    case "--allow-net":
      options.set(arg, { value: true });
      break;
    case "--pending-deprecation":
      options.set("--pending-deprecation", { value: true });
      break;
    case "--expose-internals":
    case "--expose_internals":
      options.set("--expose-internals", { value: true });
      break;
    case "--tls-min-v1.0":
    case "--tls-min-v1.1":
    case "--tls-min-v1.2":
    case "--tls-min-v1.3":
    case "--tls-max-v1.2":
    case "--tls-max-v1.3":
    case "--use-bundled-ca":
    case "--use-openssl-ca":
    case "--use-system-ca":
      options.set(arg, { value: true });
      break;
    case "--no-tls-min-v1.0":
      options.set("--tls-min-v1.0", { value: false });
      break;
    case "--no-tls-min-v1.1":
      options.set("--tls-min-v1.1", { value: false });
      break;
    case "--no-tls-min-v1.2":
      options.set("--tls-min-v1.2", { value: false });
      break;
    case "--no-tls-min-v1.3":
      options.set("--tls-min-v1.3", { value: false });
      break;
    case "--no-tls-max-v1.2":
      options.set("--tls-max-v1.2", { value: false });
      break;
    case "--no-tls-max-v1.3":
      options.set("--tls-max-v1.3", { value: false });
      break;
    case "--no-use-bundled-ca":
      options.set("--use-bundled-ca", { value: false });
      break;
    case "--no-use-openssl-ca":
      options.set("--use-openssl-ca", { value: false });
      break;
    case "--no-use-system-ca":
      options.set("--use-system-ca", { value: false });
      break;
    // denext: the diagnostic report's switches, which the node shim passes
    // in NODE_OPTIONS (see internal/process/report.ts).
    case "--report-compact":
    case "--report-exclude-env":
    case "--report-exclude-network":
    case "--report-on-fatalerror":
    case "--report-on-signal":
    case "--report-uncaught-exception":
    // denext: process.exit()'s trace (see process.ts).
    case "--trace-exit":
      options.set(arg, { value: true });
      break;
    case "--no-trace-exit":
      options.set("--trace-exit", { value: false });
      break;
    default:
      if (StringPrototypeStartsWith(arg, "--report-")) {
        const eq = StringPrototypeIndexOf(arg, "=");
        if (eq !== -1) {
          let name = StringPrototypeSlice(arg, 0, eq);
          // Node's aliases: `--report-directory` is `--report-dir`.
          if (name === "--report-directory") name = "--report-dir";
          if (
            name === "--report-dir" || name === "--report-filename" ||
            name === "--report-signal"
          ) {
            options.set(name, { value: StringPrototypeSlice(arg, eq + 1) });
            break;
          }
        }
      }
      // denext: `--heapsnapshot-signal` and `--diagnostic-dir`, which the
      // node shim passes in NODE_OPTIONS (see process.ts).
      if (StringPrototypeStartsWith(arg, "--heapsnapshot-signal=")) {
        options.set("--heapsnapshot-signal", {
          value: StringPrototypeSlice(arg, "--heapsnapshot-signal=".length),
        });
        break;
      }
      if (StringPrototypeStartsWith(arg, "--heapsnapshot-near-heap-limit=")) {
        options.set("--heapsnapshot-near-heap-limit", {
          value: StringPrototypeSlice(
            arg,
            "--heapsnapshot-near-heap-limit=".length,
          ),
        });
        break;
      }
      if (StringPrototypeStartsWith(arg, "--trace-event-file-pattern=")) {
        options.set("--trace-event-file-pattern", {
          value: StringPrototypeSlice(
            arg,
            "--trace-event-file-pattern=".length,
          ),
        });
        break;
      }
      if (StringPrototypeStartsWith(arg, "--diagnostic-dir=")) {
        options.set("--diagnostic-dir", {
          value: StringPrototypeSlice(arg, "--diagnostic-dir=".length),
        });
        break;
      }
      if (StringPrototypeStartsWith(arg, "--dns-result-order=")) {
        const value = StringPrototypeSlice(
          arg,
          "--dns-result-order=".length,
        );
        options.set("--dns-result-order", { value });
      }
      break;
  }
}

function getExecArgv() {
  if (execArgvSnapshot) {
    return execArgvSnapshot;
  }
  // `globalThis.process` is a lazy getter that loads `node:process`. During
  // node:process's OWN cold bootstrap (when the require system is deferred
  // out of the snapshot), reading it re-enters that load and the `default`
  // export is still in the temporal dead zone -- accessing the getter throws
  // rather than yielding undefined, so `?.` doesn't help. Guard it: no exec
  // args are available pre-bootstrap anyway, so fall back to `[]`.
  try {
    return globalThis.process?.execArgv ?? [];
  } catch {
    return [];
  }
}

// denext: one NODE_OPTIONS token, quoted as Node's parser reads it
// (`"..."`, with `\` before `"` and `\`), so a value with spaces or quotes
// stays one token.
// The node shim's `node_options_token` does the same.
function quoteNodeOptionsToken(token: string): string {
  let out = '"';
  for (let i = 0; i < token.length; i++) {
    const ch = token[i];
    if (ch === '"' || ch === "\\") out += "\\";
    out += ch;
  }
  return out + '"';
}

function getOptions() {
  if (optionsMap) {
    return { options: optionsMap };
  }

  const options = createDefaultOptions();
  const nodeOptions = Deno.env.get("NODE_OPTIONS");
  const envArgs = nodeOptions ? splitNodeOptions(nodeOptions) : [];
  const execArgv = getExecArgv();
  const args = ArrayPrototypeConcat(envArgs, execArgv);
  ArrayPrototypeForEach(args, (arg) => parseOption(options, arg));
  optionsMap = options;
  return { options };
}

function getExecArgvOptions() {
  if (execArgvOptionsMap) {
    return { options: execArgvOptionsMap };
  }
  const options = new SafeMap();
  ArrayPrototypeForEach(getExecArgv(), (arg) => parseOption(options, arg));
  execArgvOptionsMap = options;
  return { options };
}

return {
  getExecArgvOptions,
  getOptions,
  quoteNodeOptionsToken,
  setOptionSourceExecArgv,
  splitNodeOptions,
};
})();
