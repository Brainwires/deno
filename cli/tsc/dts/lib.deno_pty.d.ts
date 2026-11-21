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

  /** What runs in a terminal right now, from
   * {@linkcode Deno.Pty.foreground}: the process a shell put in the
   * foreground (`vim`, a build), or the shell itself while it waits at its
   * prompt.
   *
   * Everything but `pid` and `name` is what the OS tells about the process
   * at the moment of the call, and is `null` when it refuses: a process of
   * another user (a `sudo` in the terminal) hides its arguments and working
   * directory.
   *
   * @category Subprocess */
  export interface PtyForegroundProcess {
    /** The terminal's foreground process group (`tcgetpgrp`). `null` on
     * Windows, which has no foreground process group. */
    pgid: number | null;
    /** The process described: the group's leader, or another member of the
     * group once the leader has exited (a pipeline whose first command is
     * done). On Windows, see {@linkcode Deno.Pty.foreground}. */
    pid: number;
    /** Its name: `vim` (Linux: at most 15 bytes, the kernel's `comm`;
     * Windows: the image name, `vim.exe`). */
    name: string;
    /** Its arguments, the program first. */
    argv: string[] | null;
    /** Its working directory. */
    cwd: string | null;
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
    /** The terminal device the child runs on: `/dev/ttys004` on macOS,
     * `/dev/pts/3` on Linux (what `tty` prints in it). `null` on Windows,
     * where a ConPTY is not a named device. */
    readonly ttyName: string | null;
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
    /** The terminal's foreground process, as a terminal emulator shows it in
     * a tab title: asked of the OS at each call, never cached. `null` once
     * the terminal is closed, or when nothing is in its foreground (every
     * process on it has exited).
     *
     * On macOS and Linux this is the terminal's foreground process group
     * (`tcgetpgrp`) and its leader. Windows has no foreground process group,
     * and a console's process list can be read only from inside the console,
     * so there the process is found in the tree below the terminal's
     * program: while the process is a shell (`cmd.exe`, `powershell.exe`,
     * `pwsh.exe`, `bash.exe`, …) with a running child, its child started
     * last is taken; the answer is the first process that is not a shell,
     * or a shell with no child. A program started in the background there
     * (`start /b`) can be reported in place of an older foreground one.
     *
     * No permission is needed beyond the `--allow-run` the terminal was
     * opened with: only processes running on this terminal are described.
     *
     * ```ts
     * const pty = Deno.openPty({ cmd: "sh", cols: 80, rows: 24 });
     * const fg = pty.foreground();
     * // { pgid: 4242, pid: 4242, name: "sh", argv: ["sh"], cwd: "/home/me" }
     * ```
     */
    foreground(): PtyForegroundProcess | null;
    /** Sends `signal` to the child (and only the child: not to the process
     * group it leads). On Windows every signal but `0` ends the process and
     * every process it started (they share a job object; one started with
     * `detached: true` has left it), each with exit code 1.
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
   * On Windows the terminal is a ConPTY (Windows 10 version 1809 or later;
   * an older Windows throws {@linkcode Deno.errors.NotSupported}). Its output
   * is the console's rendering of the child's: the text with VT sequences
   * around it, lines ended as the console draws them.
   *
   * @tags allow-run
   * @category Subprocess */
  export function openPty(options: PtyOptions): Pty;
}
