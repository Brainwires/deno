// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.openPty`: a child process on a pseudo-terminal (DENEXT.md item 259).
// The ops are in `lib.rs`; this module validates the options, wires the
// master into a `ReadableStream` / `WritableStream` pair and builds the
// handle. `Deno.Command` with `pty` (40_process.js) and the `node-pty` shim
// (ext/node) are built on `spawnPty` below.

(function () {
const { core, primordials } = __bootstrap;
const {
  op_pty_foreground_process,
  op_pty_kill,
  op_pty_resize,
  op_pty_spawn,
  op_pty_wait,
} = core.ops;
const {
  ArrayIsArray,
  ArrayPrototypeMap,
  NumberIsInteger,
  ObjectEntries,
  ObjectFreeze,
  ObjectPrototypeIsPrototypeOf,
  ObjectSetPrototypeOf,
  PromisePrototypeThen,
  String,
  SymbolAsyncDispose,
  TypeError,
} = primordials;
const { pathFromURL } = core.loadExtScript("ext:deno_web/00_infra.js");
const { URLPrototype } = core.loadExtScript("ext:deno_web/00_url.js");
const { readableStreamForRid, writableStreamForRid } = core.loadExtScript(
  "ext:deno_web/06_streams.js",
);

const EMPTY_OPTIONS = ObjectFreeze(ObjectSetPrototypeOf({}, null));

function checkSize(name, value, prefix) {
  if (!NumberIsInteger(value) || value < 1 || value > 0xffff) {
    throw new TypeError(
      `${prefix}: '${name}' must be an integer between 1 and 65535, received ${value}`,
    );
  }
}

// Spawns `options.cmd` on a new pseudo-terminal. Returns the handle
// `Deno.openPty` hands out plus what the runtime's other users of a pty
// need (the master's resource id, the wait promise for ref / unref).
function spawnPty(options, apiName) {
  const prefix = `Failed to execute '${apiName}'`;
  if (options === null || typeof options !== "object") {
    throw new TypeError(`${prefix}: 'options' must be an object`);
  }
  const {
    cmd,
    args = [],
    cwd = undefined,
    env = EMPTY_OPTIONS,
    clearEnv = false,
    cols,
    rows,
  } = options;
  if (
    typeof cmd !== "string" && !ObjectPrototypeIsPrototypeOf(URLPrototype, cmd)
  ) {
    throw new TypeError(`${prefix}: 'cmd' must be a string or a URL`);
  }
  if (!ArrayIsArray(args)) {
    throw new TypeError(`${prefix}: 'args' must be an array`);
  }
  checkSize("cols", cols, prefix);
  checkSize("rows", rows, prefix);

  const { pid, masterRid, childRid } = op_pty_spawn({
    cmd: pathFromURL(cmd),
    args: ArrayPrototypeMap(args, String),
    cwd: pathFromURL(cwd),
    clearEnv,
    env: ObjectEntries(env),
    cols,
    rows,
  }, apiName);

  const waitPromise = op_pty_wait(childRid);
  let waitComplete = false;
  const status = PromisePrototypeThen(waitPromise, (res) => {
    waitComplete = true;
    return res;
  });

  // One resource serves both directions. It closes when `readable` reaches
  // its end (the child's side of the terminal is gone) or is cancelled, and
  // on dispose. `writable` shares it without closing it: a terminal has no
  // half-close (an end-of-file for the child is a "\x04" written to it).
  const readable = readableStreamForRid(masterRid, true);
  const writable = writableStreamForRid(masterRid, false);

  const handle = {
    pid,
    readable,
    writable,
    status,
    resize(cols, rows) {
      checkSize("cols", cols, "Failed to execute 'resize'");
      checkSize("rows", rows, "Failed to execute 'resize'");
      op_pty_resize(masterRid, cols, rows);
    },
    kill(signal = "SIGHUP") {
      if (waitComplete) {
        throw new TypeError("Child process has already terminated");
      }
      op_pty_kill(childRid, signal);
    },
    async [SymbolAsyncDispose]() {
      try {
        op_pty_kill(childRid, "SIGHUP");
      } catch {
        // already gone
      }
      await status;
      core.tryClose(masterRid);
    },
  };
  return {
    __proto__: null,
    handle,
    masterRid,
    childRid,
    waitPromise,
    isWaitComplete: () => waitComplete,
  };
}

function openPty(options) {
  return spawnPty(options, "Deno.openPty()").handle;
}

// The name of the terminal's foreground process, where the platform tells
// (for `node-pty`'s `process`); `null` where it doesn't or once the master
// is closed.
function foregroundProcess(masterRid) {
  try {
    return op_pty_foreground_process(masterRid);
  } catch {
    return null;
  }
}

return { openPty, spawnPty, foregroundProcess };
})();
