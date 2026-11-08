// Copyright 2018-2026 the Deno authors. MIT license.
// The API follows node-pty's lib/ (Copyright (c) 2012-2015 Christopher
// Jeffrey, 2016 Daniel Imms, 2018 Microsoft Corporation, MIT license).

// deno-lint-ignore-file deno-internal/prefer-primordials

// `node-pty` over `Deno.openPty` (DENEXT.md item 261). node-pty is a native
// addon (`pty.node` / `conpty.node` plus a `spawn-helper`) built at install or
// shipped prebuilt. On Unix the package's entry module resolves to this one
// instead (`maybeNodePtyOverride` in 01_require.js), a package that carries
// the addon too (its `native` export is then the addon, DENEXT.md items 268
// and 269); on Windows only a package without the addon does. This implements
// node-pty 1.x's public API - `spawn(file, args, options)` and the `IPty` it
// returns: `pid`, `cols`, `rows`, `process`, `onData` / `onExit` (disposable
// events), the `on('data' | 'exit' | 'close')` emitter, `write`, `resize`,
// `kill`, `pause` / `resume`, `clear`, `handleFlowControl` - on the runtime's
// own pseudo-terminal, under `--allow-run` like `Deno.Command`.

(function () {
const { core } = __bootstrap;
const { spawnPty, foregroundProcess } = core.loadExtScript(
  "ext:deno_pty/01_pty.js",
);
const { EventEmitter } = core.loadExtScript("ext:deno_node/_events.mjs");
const { Buffer } = core.loadExtScript("ext:deno_node/internal/buffer.mjs");
const { constants: osConstants } = core.loadExtScript("ext:deno_node/os.ts");
const { isWindows } = core.loadExtScript("ext:deno_node/_util/os.ts");

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const DEFAULT_NAME = "xterm";
const DEFAULT_FILE = isWindows ? "cmd.exe" : "sh";
// node-pty's flow-control messages: XOFF pauses, XON resumes
const FLOW_CONTROL_PAUSE = "\x13";
const FLOW_CONTROL_RESUME = "\x11";

// node-pty's `EventEmitter2`: a function that subscribes and returns a
// disposable.
class Event {
  #listeners = [];
  #event;
  get event() {
    if (!this.#event) {
      this.#event = (listener) => {
        this.#listeners.push(listener);
        return {
          dispose: () => {
            const index = this.#listeners.indexOf(listener);
            if (index !== -1) this.#listeners.splice(index, 1);
          },
        };
      };
    }
    return this.#event;
  }
  fire(data) {
    const queue = this.#listeners.slice();
    for (let i = 0; i < queue.length; i++) {
      queue[i].call(undefined, data);
    }
  }
}

function checkType(name, value, type) {
  if (value === undefined) return;
  const actual = typeof value;
  if (actual !== type) {
    throw new Error(`${name} must be a ${type} (not a ${actual})`);
  }
}

// What node-pty strips from an inherited environment: the terminal
// multiplexer and size variables of the terminal this process runs in.
function sanitizeEnv(env) {
  delete env["TMUX"];
  delete env["TMUX_PANE"];
  delete env["STY"];
  delete env["WINDOW"];
  delete env["WINDOWID"];
  delete env["TERMCAP"];
  delete env["COLUMNS"];
  delete env["LINES"];
}

function signalNumber(name) {
  if (name == null) return undefined;
  return osConstants.signals[name];
}

class Terminal extends EventEmitter {
  #spawn;
  #file;
  #cols;
  #rows;
  #onData = new Event();
  #onExit = new Event();
  #decoder;
  #writer;
  #writeChain = Promise.resolve();
  #paused = false;
  #resume = null;
  #readable;
  #closed = false;
  handleFlowControl;
  #flowControlPause;
  #flowControlResume;

  constructor(file, args, opt) {
    super();
    if (typeof args === "string") {
      throw new Error(
        "args as a string is not supported; pass an array (Deno's node-pty runs on Deno.openPty)",
      );
    }
    args = args || [];
    file = file || DEFAULT_FILE;
    opt = opt || {};
    checkType("name", opt.name, "string");
    checkType("cols", opt.cols, "number");
    checkType("rows", opt.rows, "number");
    checkType("cwd", opt.cwd, "string");
    checkType("env", opt.env, "object");
    checkType(
      "encoding",
      opt.encoding === null ? undefined : opt.encoding,
      "string",
    );
    if (opt.uid !== undefined || opt.gid !== undefined) {
      throw new Error(
        "uid and gid are not supported: Deno's node-pty runs the child as the current user",
      );
    }
    this.handleFlowControl = !!opt.handleFlowControl;
    this.#flowControlPause = opt.flowControlPause || FLOW_CONTROL_PAUSE;
    this.#flowControlResume = opt.flowControlResume || FLOW_CONTROL_RESUME;

    const process = globalThis.process;
    const inherited = opt.env === undefined || opt.env === process.env;
    const env = { ...(inherited ? process.env : opt.env) };
    if (inherited) sanitizeEnv(env);
    const cwd = opt.cwd || process.cwd();
    env.PWD = cwd;
    const name = opt.name || env.TERM || DEFAULT_NAME;
    env.TERM = name;
    const encoding = opt.encoding === undefined ? "utf8" : opt.encoding;
    this.#cols = opt.cols || DEFAULT_COLS;
    this.#rows = opt.rows || DEFAULT_ROWS;
    this.#file = file;

    // the whole environment is given, so nothing of this process's leaks
    // past node-pty's sanitizing
    this.#spawn = spawnPty({
      cmd: file,
      args: args.map(String),
      cwd,
      env,
      clearEnv: true,
      cols: this.#cols,
      rows: this.#rows,
    }, "node-pty spawn()");
    this.#decoder = encoding === null ? null : new TextDecoder(encoding);
    this.#readable = this.#spawn.handle.readable;
    this.#writer = this.#spawn.handle.writable.getWriter();
    this.#run();
  }

  // Reads the terminal to its end, then reports the exit: `data` events as
  // chunks arrive (held while paused), `close` once the terminal is gone,
  // `exit` after it with the status, as node-pty orders them.
  async #run() {
    const reader = this.#readable.getReader();
    try {
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        // a chunk read while paused waits for `resume()`
        while (this.#paused) {
          await new Promise((resolve) => {
            this.#resume = resolve;
          });
        }
        const data = this.#decoder === null
          ? Buffer.from(value)
          : this.#decoder.decode(value, { stream: true });
        if (data.length !== 0) this.emit("data", data);
      }
      if (this.#decoder !== null) {
        const tail = this.#decoder.decode();
        if (tail.length !== 0) this.emit("data", tail);
      }
    } catch (err) {
      if (this.listenerCount("error") > 0) {
        this.emit("error", err);
      }
    }
    this.#closed = true;
    this.emit("close");
    const status = await this.#spawn.handle.status;
    const signal = signalNumber(status.signal);
    const exitCode = signal === undefined ? status.code : 0;
    this.emit("exit", exitCode, signal);
  }

  get onData() {
    return this.#onData.event;
  }
  get onExit() {
    return this.#onExit.event;
  }
  get pid() {
    return this.#spawn.handle.pid;
  }
  get cols() {
    return this.#cols;
  }
  get rows() {
    return this.#rows;
  }
  /** The name of the terminal's foreground process, else the file. */
  get process() {
    return foregroundProcess(this.#spawn.masterRid) || this.#file;
  }

  // node-pty's emitter forwards `data` and `exit` into the disposable events
  emit(eventName, ...args) {
    if (eventName === "data") {
      this.#onData.fire(args[0]);
    } else if (eventName === "exit") {
      this.#onExit.fire({ exitCode: args[0], signal: args[1] });
    }
    return super.emit(eventName, ...args);
  }

  write(data) {
    if (this.handleFlowControl) {
      // PAUSE/RESUME messages are not forwarded to the pty
      if (data === this.#flowControlPause) {
        this.pause();
        return;
      }
      if (data === this.#flowControlResume) {
        this.resume();
        return;
      }
    }
    this.#write(data);
  }

  #write(data) {
    if (this.#closed) return;
    const bytes = typeof data === "string"
      ? new TextEncoder().encode(data)
      : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    if (bytes.byteLength === 0) return;
    // writes go out in order; a failed one (the child is gone) is dropped
    this.#writeChain = this.#writeChain.then(
      () => this.#writer.write(bytes),
      () => {},
    ).catch(() => {});
  }

  resize(cols, rows) {
    if (
      cols <= 0 || rows <= 0 || isNaN(cols) || isNaN(rows) ||
      cols === Infinity || rows === Infinity
    ) {
      throw new Error("resizing must be done using positive cols and rows");
    }
    this.#spawn.handle.resize(cols, rows);
    this.#cols = cols;
    this.#rows = rows;
  }

  /** Nothing on Unix, as in node-pty (Windows clears ConPTY's buffer). */
  clear() {}

  kill(signal) {
    try {
      this.#spawn.handle.kill(signal || "SIGHUP");
    } catch {
      // swallowed, as node-pty does
    }
  }

  pause() {
    this.#paused = true;
  }

  resume() {
    this.#paused = false;
    const resume = this.#resume;
    this.#resume = null;
    if (resume !== null) resume();
  }

  /** Ends the terminal: `SIGHUP` to the child, the master closed. */
  destroy() {
    this.#closed = true;
    this.kill("SIGHUP");
    // the master closes now, as node-pty's socket does, not once the child
    // has exited: one that ignores SIGHUP does not keep the terminal open
    core.tryClose(this.#spawn.masterRid);
    this.#spawn.handle[Symbol.asyncDispose]().catch(() => {});
  }

  /** See net.Socket.end */
  end(data) {
    if (data !== undefined) this.#write(data);
    this.destroy();
  }

  get master() {
    return undefined;
  }
  get slave() {
    return undefined;
  }
}

function spawn(file, args, opt) {
  return new Terminal(file, args, opt);
}

/** @deprecated */
function fork(file, args, opt) {
  return new Terminal(file, args, opt);
}

/** @deprecated */
function createTerminal(file, args, opt) {
  return new Terminal(file, args, opt);
}

function open() {
  throw new Error(
    "node-pty's open() (a terminal with no child) is not available on Deno",
  );
}

// `native` is node-pty's addon, "not public API and could be removed at any
// time": nothing here (01_require.js puts the package's addon there when the
// package carries one).
const native = null;

return { spawn, fork, createTerminal, open, native, Terminal };
})();
