// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent, Inc. and Node.js contributors. All rights reserved. MIT license.
(function () {
const { primordials } = __bootstrap;
const {
  ArrayPrototypeForEach,
  ArrayPrototypeIncludes,
  ArrayPrototypeMap,
  ObjectFreeze,
  ReflectApply,
  RegExpPrototypeTest,
  SafeRegExp,
  SafeSet,
  SetPrototypeEntries,
  SetPrototypeValues,
  StringPrototypeReplace,
  Symbol,
  SymbolIterator,
} = primordials;

const kInternal = Symbol("internal properties");

const replaceUnderscoresRegex = new SafeRegExp(/_/g);
const leadingDashesRegex = new SafeRegExp(/^--?/);
const trailingValuesRegex = new SafeRegExp(/=.*$/);

// This builds the initial process.allowedNodeEnvironmentFlags
// from data in the config binding.
function buildAllowedFlags() {
  // Node 26.5.1's process.allowedNodeEnvironmentFlags: the options its
  // option parser marks kAllowedInEnvvar, a `--no-` form for each boolean one,
  // and the aliases that expand only to such options.
  const allowedNodeEnvironmentFlags = [
    "--abort-on-uncaught-exception",
    "--addons",
    "--allow-addons",
    "--allow-child-process",
    "--allow-ffi",
    "--allow-fs-read",
    "--allow-fs-write",
    "--allow-inspector",
    "--allow-net",
    "--allow-wasi",
    "--allow-worker",
    "--async-context-frame",
    "--conditions",
    "--cpu-prof",
    "--cpu-prof-dir",
    "--cpu-prof-interval",
    "--cpu-prof-name",
    "--debug-arraybuffer-allocations",
    "--debug-port",
    "--deprecation",
    "--diagnostic-dir",
    "--disable-proto",
    "--disable-sigusr1",
    "--disable-warning",
    "--disable-wasm-trap-handler",
    "--disallow-code-generation-from-strings",
    "--dns-result-order",
    "--enable-etw-stack-walking",
    "--enable-fips",
    "--enable-network-family-autoselection",
    "--enable-source-maps",
    "--entry-url",
    "--es-module-specifier-resolution",
    "--experimental-abortcontroller",
    "--experimental-addon-modules",
    "--experimental-detect-module",
    "--experimental-eventsource",
    "--experimental-fetch",
    "--experimental-ffi",
    "--experimental-global-customevent",
    "--experimental-global-navigator",
    "--experimental-global-webcrypto",
    "--experimental-import-meta-resolve",
    "--experimental-import-text",
    "--experimental-json-modules",
    "--experimental-loader",
    "--experimental-modules",
    "--experimental-package-map",
    "--experimental-print-required-tla",
    "--experimental-quic",
    "--experimental-repl-await",
    "--experimental-report",
    "--experimental-require-module",
    "--experimental-shadow-realm",
    "--experimental-specifier-resolution",
    "--experimental-sqlite",
    "--experimental-stream-iter",
    "--experimental-strip-types",
    "--experimental-test-isolation",
    "--experimental-top-level-await",
    "--experimental-vfs",
    "--experimental-vm-modules",
    "--experimental-wasi-unstable-preview1",
    "--experimental-wasm-modules",
    "--experimental-websocket",
    "--experimental-webstorage",
    "--experimental-worker",
    "--expose-gc",
    "--extra-info-on-fatal-exception",
    "--force-async-hooks-checks",
    "--force-context-aware",
    "--force-fips",
    "--force-node-api-uncaught-exceptions-policy",
    "--frozen-intrinsics",
    "--global-search-paths",
    "--heap-prof",
    "--heap-prof-dir",
    "--heap-prof-interval",
    "--heap-prof-name",
    "--heapsnapshot-near-heap-limit",
    "--heapsnapshot-signal",
    "--http-parser",
    "--icu-data-dir",
    "--import",
    "--input-type",
    "--insecure-http-parser",
    "--inspect",
    "--inspect-brk",
    "--inspect-port",
    "--inspect-publish-uid",
    "--inspect-wait",
    "--interpreted-frames-native-stack",
    "--jitless",
    "--loader",
    "--localstorage-file",
    "--max-heap-size",
    "--max-http-header-size",
    "--max-old-space-size",
    "--max-old-space-size-percentage",
    "--max-semi-space-size",
    "--napi-modules",
    "--network-family-autoselection",
    "--network-family-autoselection-attempt-timeout",
    "--no-addons",
    "--no-allow-addons",
    "--no-allow-child-process",
    "--no-allow-ffi",
    "--no-allow-inspector",
    "--no-allow-net",
    "--no-allow-wasi",
    "--no-allow-worker",
    "--no-async-context-frame",
    "--no-cpu-prof",
    "--no-debug-arraybuffer-allocations",
    "--no-deprecation",
    "--no-disable-sigusr1",
    "--no-disable-wasm-trap-handler",
    "--no-enable-fips",
    "--no-enable-source-maps",
    "--no-entry-url",
    "--no-experimental-addon-modules",
    "--no-experimental-detect-module",
    "--no-experimental-eventsource",
    "--no-experimental-ffi",
    "--no-experimental-global-navigator",
    "--no-experimental-import-meta-resolve",
    "--no-experimental-import-text",
    "--no-experimental-print-required-tla",
    "--no-experimental-repl-await",
    "--no-experimental-require-module",
    "--no-experimental-shadow-realm",
    "--no-experimental-sqlite",
    "--no-experimental-stream-iter",
    "--no-experimental-vfs",
    "--no-experimental-vm-modules",
    "--no-experimental-websocket",
    "--no-experimental-webstorage",
    "--no-extra-info-on-fatal-exception",
    "--no-force-async-hooks-checks",
    "--no-force-context-aware",
    "--no-force-fips",
    "--no-force-node-api-uncaught-exceptions-policy",
    "--no-frozen-intrinsics",
    "--no-global-search-paths",
    "--no-heap-prof",
    "--no-insecure-http-parser",
    "--no-inspect",
    "--no-inspect-brk",
    "--no-inspect-wait",
    "--no-network-family-autoselection",
    "--no-node-snapshot",
    "--no-openssl-legacy-provider",
    "--no-openssl-shared-config",
    "--no-pending-deprecation",
    "--no-permission",
    "--no-permission-audit",
    "--no-preserve-symlinks",
    "--no-preserve-symlinks-main",
    "--no-report-compact",
    "--no-report-exclude-env",
    "--no-report-exclude-network",
    "--no-report-on-fatalerror",
    "--no-report-on-signal",
    "--no-report-uncaught-exception",
    "--no-require-module",
    "--no-strip-types",
    "--no-test-only",
    "--no-test-randomize",
    "--no-throw-deprecation",
    "--no-tls-max-v1.2",
    "--no-tls-max-v1.3",
    "--no-tls-min-v1.0",
    "--no-tls-min-v1.1",
    "--no-tls-min-v1.2",
    "--no-tls-min-v1.3",
    "--no-trace-deprecation",
    "--no-trace-env",
    "--no-trace-env-js-stack",
    "--no-trace-env-native-stack",
    "--no-trace-exit",
    "--no-trace-promises",
    "--no-trace-sigint",
    "--no-trace-sync-io",
    "--no-trace-tls",
    "--no-trace-uncaught",
    "--no-trace-warnings",
    "--no-track-heap-objects",
    "--no-use-bundled-ca",
    "--no-use-env-proxy",
    "--no-use-openssl-ca",
    "--no-use-system-ca",
    "--no-verify-base-objects",
    "--no-warnings",
    "--no-watch",
    "--no-watch-preserve-output",
    "--no-zero-fill-buffers",
    "--node-memory-debug",
    "--node-snapshot",
    "--openssl-config",
    "--openssl-legacy-provider",
    "--openssl-shared-config",
    "--pending-deprecation",
    "--perf-basic-prof",
    "--perf-basic-prof-only-functions",
    "--perf-prof",
    "--perf-prof-unwinding-info",
    "--permission",
    "--permission-audit",
    "--preserve-symlinks",
    "--preserve-symlinks-main",
    "--prof-process",
    "--redirect-warnings",
    "--report-compact",
    "--report-dir",
    "--report-directory",
    "--report-exclude-env",
    "--report-exclude-network",
    "--report-filename",
    "--report-on-fatalerror",
    "--report-on-signal",
    "--report-signal",
    "--report-uncaught-exception",
    "--require",
    "--require-module",
    "--secure-heap",
    "--secure-heap-min",
    "--snapshot-blob",
    "--stack-trace-limit",
    "--strip-types",
    "--test-coverage-branches",
    "--test-coverage-exclude",
    "--test-coverage-functions",
    "--test-coverage-include",
    "--test-coverage-lines",
    "--test-global-setup",
    "--test-isolation",
    "--test-name-pattern",
    "--test-only",
    "--test-random-seed",
    "--test-randomize",
    "--test-reporter",
    "--test-reporter-destination",
    "--test-rerun-failures",
    "--test-shard",
    "--test-skip-pattern",
    "--throw-deprecation",
    "--title",
    "--tls-cipher-list",
    "--tls-keylog",
    "--tls-max-v1.2",
    "--tls-max-v1.3",
    "--tls-min-v1.0",
    "--tls-min-v1.1",
    "--tls-min-v1.2",
    "--tls-min-v1.3",
    "--trace-deprecation",
    "--trace-env",
    "--trace-env-js-stack",
    "--trace-env-native-stack",
    "--trace-event-categories",
    "--trace-event-file-pattern",
    "--trace-events-enabled",
    "--trace-exit",
    "--trace-promises",
    "--trace-require-module",
    "--trace-sigint",
    "--trace-sync-io",
    "--trace-tls",
    "--trace-uncaught",
    "--trace-warnings",
    "--track-heap-objects",
    "--unhandled-rejections",
    "--use-bundled-ca",
    "--use-env-proxy",
    "--use-largepages",
    "--use-openssl-ca",
    "--use-system-ca",
    "--v8-pool-size",
    "--verify-base-objects",
    "--warnings",
    "--watch",
    "--watch-kill-signal",
    "--watch-path",
    "--watch-preserve-output",
    "--zero-fill-buffers",
    "-C",
    "-r",
  ];

  /*
    function isAccepted(to) {
      if (!to.startsWith("-") || to === "--") return true;
      const recursiveExpansion = aliases.get(to);
      if (recursiveExpansion) {
        if (recursiveExpansion[0] === to) {
          recursiveExpansion.splice(0, 1);
        }
        return recursiveExpansion.every(isAccepted);
      }
      return options.get(to).envVarSettings === kAllowedInEnvironment;
    }
    for (const { 0: from, 1: expansion } of aliases) {
      if (expansion.every(isAccepted)) {
        let canonical = from;
        if (canonical.endsWith("=")) {
          canonical = canonical.slice(0, canonical.length - 1);
        }
        if (canonical.endsWith(" <arg>")) {
          canonical = canonical.slice(0, canonical.length - 4);
        }
        allowedNodeEnvironmentFlags.push(canonical);
      }
    }
    */

  const trimLeadingDashes = (flag) =>
    StringPrototypeReplace(flag, leadingDashesRegex, "");

  // Save these for comparison against flags provided to
  // process.allowedNodeEnvironmentFlags.has() which lack leading dashes.
  const nodeFlags = ArrayPrototypeMap(
    allowedNodeEnvironmentFlags,
    trimLeadingDashes,
  );

  // Ignoring primordial lint. Extending from SafeSet prevents augmenting `keys`
  // and [SymbolIterator] instance methods.
  // deno-lint-ignore deno-internal/prefer-primordials
  class NodeEnvironmentFlagsSet extends Set {
    constructor(array) {
      super();
      this[kInternal] = { array };
    }

    add() {
      // No-op, `Set` API compatible
      return this;
    }

    delete() {
      // No-op, `Set` API compatible
      return false;
    }

    clear() {
      // No-op, `Set` API compatible
    }

    has(key) {
      // This will return `true` based on various possible
      // permutations of a flag, including present/missing leading
      // dash(es) and/or underscores-for-dashes.
      // Strips any values after `=`, inclusive.
      // TODO(addaleax): It might be more flexible to run the option parser
      // on a dummy option set and see whether it rejects the argument or
      // not.
      if (typeof key === "string") {
        key = StringPrototypeReplace(key, replaceUnderscoresRegex, "-");
        if (RegExpPrototypeTest(leadingDashesRegex, key)) {
          key = StringPrototypeReplace(key, trailingValuesRegex, "");
          return ArrayPrototypeIncludes(this[kInternal].array, key);
        }
        return ArrayPrototypeIncludes(nodeFlags, key);
      }
      return false;
    }

    entries() {
      this[kInternal].set ??= new SafeSet(this[kInternal].array);
      return SetPrototypeEntries(this[kInternal].set);
    }

    forEach(callback, thisArg = undefined) {
      ArrayPrototypeForEach(
        this[kInternal].array,
        (v) => ReflectApply(callback, thisArg, [v, v, this]),
      );
    }

    get size() {
      return this[kInternal].array.length;
    }

    values() {
      this[kInternal].set ??= new SafeSet(this[kInternal].array);
      return SetPrototypeValues(this[kInternal].set);
    }
  }
  NodeEnvironmentFlagsSet.prototype.keys =
    NodeEnvironmentFlagsSet
      .prototype[SymbolIterator] =
      NodeEnvironmentFlagsSet.prototype.values;

  ObjectFreeze(NodeEnvironmentFlagsSet.prototype.constructor);
  ObjectFreeze(NodeEnvironmentFlagsSet.prototype);

  return ObjectFreeze(
    new NodeEnvironmentFlagsSet(
      allowedNodeEnvironmentFlags,
    ),
  );
}

return {
  buildAllowedFlags,
};
})();
