// Copyright 2018-2026 the Deno authors. MIT license.

// denext: preloaded into every node_compat test the harness runs with
// `deno run` (tests/node_compat/mod.rs). Node's `common.skip(msg)` prints
// `1..0 # Skipped: <msg>` and calls `process.exit()` at once, while
// `common.printSkipMessage(msg)` prints the same line for one case and lets
// the rest of the test run. Only the first is a whole-test skip, so when
// `process.exit()` reaches `process.reallyExit()` with `skip` from
// test/common/index.js on the stack, this writes SKIP_MARKER to stderr for the
// harness to see. Nothing else changes; a child the test spawns does not get
// the preload.
import process from "node:process";

const SKIP_MARKER = "denext-node-compat: exited through common.skip()\n";

function calledFromCommonSkip() {
  const { prepareStackTrace, stackTraceLimit } = Error;
  try {
    Error.stackTraceLimit = Infinity;
    Error.prepareStackTrace = (_error, frames) => frames;
    const frames = new Error().stack;
    return Array.isArray(frames) && frames.some((frame) => {
      const file = frame.getFileName() ?? "";
      return frame.getFunctionName() === "skip" &&
        /[\\/]test[\\/]common[\\/]index\.js$/.test(file);
    });
  } catch {
    return false;
  } finally {
    Error.prepareStackTrace = prepareStackTrace;
    Error.stackTraceLimit = stackTraceLimit;
  }
}

const reallyExit = process.reallyExit;
process.reallyExit = function (code) {
  if (calledFromCommonSkip()) {
    try {
      Deno.stderr.writeSync(new TextEncoder().encode(SKIP_MARKER));
    } catch {
      // The harness then counts the run as a pass, as it did before.
    }
  }
  return reallyExit.call(this, code);
};
