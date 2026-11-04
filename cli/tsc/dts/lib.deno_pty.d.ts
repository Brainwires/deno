// Copyright 2018-2026 the Deno authors. MIT license.

/// <reference no-default-lib="true" />
/// <reference lib="esnext" />
/// <reference lib="esnext.disposable" />

declare namespace Deno {
  /** Options for {@linkcode Deno.openPty}: the program to run on a new
   * pseudo-terminal and the terminal's size. The program, its arguments,
   * working directory and environment are resolved and checked exactly as
   * {@linkcode Deno.Command} does it, so the same `--allow-run` permission
   * (and allow-list) applies.
   *
   * @category Subprocess */
  export interface PtyOptions {
    /** The program to run: a path, or a name looked up on the environment's
     * `PATH` (as `Deno.Command`'s `command`). */
    cmd: string | URL;
    /** Arguments to pass to the program. */
    args?: string[];
    /** The child's working directory.
     *
     * If not specified, the `cwd` of the parent process is used. */
    cwd?: string | URL;
    /** Clear environmental variables from the parent process.
     *
     * Doesn't guarantee that only `env` variables are present, as the OS may
     * set environmental variables for processes.
     *
     * @default {false} */
    clearEnv?: boolean;
    /** Environmental variables to pass to the child, in addition to the
     * parent's (or instead of them, with `clearEnv`). The child's `TERM` is
     * whatever these say (a terminal application expects one, such as
     * `xterm-256color`); nothing is added. */
    env?: Record<string, string>;
    /** The terminal's width in columns (1 to 65535). */
    cols: number;
    /** The terminal's height in rows (1 to 65535). */
    rows: number;
  }

  /** A child process on a pseudo-terminal, from {@linkcode Deno.openPty}.
   *
   * The child's stdin, stdout and stderr are the terminal: what it prints
   * arrives on `readable` (with the terminal's own processing, echo and
   * `\r\n` line ends included), and what is written to `writable` is its
   * keyboard input. A terminal has no half-close, so closing `writable`
   * sends the child no end-of-file; write `"\x04"` (the end-of-file
   * character) for that. `readable` ends once the child, and every process
   * holding the terminal open, is gone; reading it to the end (or
   * cancelling it) releases the terminal, as does disposing the handle.
   *
   * ```ts
   * await using pty = Deno.openPty({
   *   cmd: "bash",
   *   args: ["-i"],
   *   cols: 120,
   *   rows: 40,
   *   env: { TERM: "xterm-256color" },
   * });
   * const writer = pty.writable.getWriter();
   * await writer.write(new TextEncoder().encode("echo hello\r"));
   * for await (const chunk of pty.readable) {
   *   // the echo, then "hello"
   * }
   * const { code } = await pty.status;
   * ```
   *
   * @category Subprocess */
  export interface Pty extends AsyncDisposable {
    /** The child's process id. */
    readonly pid: number;
    /** The child's output, as the terminal delivers it. */
    readonly readable: ReadableStream<Uint8Array<ArrayBuffer>>;
    /** The child's input: what a keyboard would send. */
    readonly writable: WritableStream<Uint8Array<ArrayBufferLike>>;
    /** A promise that resolves once the child has exited, with its exit
     * code and terminating signal (if any), as {@linkcode Deno.ChildProcess}
     * reports them. */
    readonly status: Promise<CommandStatus>;
    /** Changes the terminal's size (1 to 65535 each way). The child's
     * foreground process is told (`SIGWINCH` on Unix). */
    resize(cols: number, rows: number): void;
    /** Sends `signal` to the child (and only the child: not to the process
     * group it leads). On Windows every signal but `0` ends the process.
     *
     * @param [signal="SIGHUP"] what a closing terminal sends. */
    kill(signal?: Signal | number): void;
    /** Sends the child `SIGHUP`, waits for it to exit and releases the
     * terminal. */
    [Symbol.asyncDispose](): Promise<void>;
  }

  /** Spawns a child process on a new pseudo-terminal, as a terminal
   * emulator does: for a shell, an editor, or any program that wants a
   * real terminal (colors, line editing, `isatty`). Requires `--allow-run`
   * for the program, exactly as {@linkcode Deno.Command} does.
   *
   * ```ts
   * const pty = Deno.openPty({ cmd: "sh", cols: 80, rows: 24 });
   * ```
   *
   * To spawn through `Deno.Command` instead, pass `pty: { cols, rows }` in
   * its options: the child's stdio is then the terminal, and the process's
   * {@linkcode Deno.ChildProcess.pty} is its `readable`, `writable` and
   * `resize`.
   *
   * Windows is not supported yet: there, `Deno.openPty` and `Deno.Command`
   * with `pty` throw {@linkcode Deno.errors.NotSupported}.
   *
   * @tags allow-run
   * @category Subprocess */
  export function openPty(options: PtyOptions): Pty;
}
