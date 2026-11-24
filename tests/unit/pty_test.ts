// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.openPty` and `Deno.Command` with `pty` (DENEXT.md items 259 and
// 260). Every test here fails on a build without the API (`Deno.openPty is
// not a function`). On Windows the terminal is a ConPTY: its output is a
// console's rendering (VT sequences around the text, lines ended as the
// console draws them), so the Windows cases check the text only (with the
// sequences taken out where the console may draw runs of spaces as cursor
// moves); the tests that drive `sh`, `cat` or `stty` run on Unix only, and
// the Windows ones drive `cmd.exe`, `mode`, PowerShell and WSL where present.

import {
  assert,
  assertEquals,
  assertMatch,
  assertStringIncludes,
  assertThrows,
} from "./test_util.ts";

const isWindows = Deno.build.os === "windows";
const shell = isWindows ? "cmd.exe" : "sh";
const shellArgs = (script: string) =>
  isWindows ? ["/d", "/c", script] : ["-c", script];
const EOL = "\r\n";
// What follows a line of the child's output: "\r\n" on a Unix terminal, not
// asserted on a ConPTY.
const LINE_END = isWindows ? "" : EOL;

async function readAll(stream: ReadableStream<Uint8Array>): Promise<string> {
  let text = "";
  const decoder = new TextDecoder();
  for await (const chunk of stream) {
    text += decoder.decode(chunk, { stream: true });
  }
  return text + decoder.decode();
}

// Reads until `needle` has been seen (a string, or a test of all the text
// read so far), or the stream ended, returning what was read; the reader is
// released, not cancelled, so the terminal stays readable.
async function readUntil(
  stream: ReadableStream<Uint8Array>,
  needle: string | ((text: string) => boolean),
): Promise<string> {
  const seen = typeof needle === "string"
    ? (text: string) => text.includes(needle)
    : needle;
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  try {
    while (!seen(text)) {
      const { value, done } = await reader.read();
      if (done) break;
      text += decoder.decode(value, { stream: true });
    }
  } finally {
    reader.releaseLock();
  }
  return text;
}

// The environment of a child spawned under a scoped `--allow-run`: only
// PATH, so the host's own `LD_*` / `DYLD_*` variables (which need full run
// permission) do not decide the outcome.
const scopedEnv = {
  clearEnv: true,
  env: { PATH: Deno.env.get("PATH") ?? "/usr/bin:/bin" },
};

// The text a console draws, its VT sequences replaced by a space (a cursor
// move the console drew in place of spaces stays a separator).
const VT_SEQUENCE =
  // deno-lint-ignore no-control-regex
  /\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-_])/g;
const plainText = (text: string) => text.replace(VT_SEQUENCE, " ");

async function write(pty: Deno.Pty, text: string) {
  const writer = pty.writable.getWriter();
  await writer.write(new TextEncoder().encode(text));
  writer.releaseLock();
}

Deno.test(
  { permissions: { run: true, read: true } },
  async function ptyEchoRoundTrip() {
    const pty = Deno.openPty({
      cmd: shell,
      args: shellArgs("echo hello from pty"),
      cols: 80,
      rows: 24,
    });
    assert(pty.pid > 0);
    const output = await readAll(pty.readable);
    // a terminal turns "\n" into "\r\n"
    assertStringIncludes(output, `hello from pty${LINE_END}`);
    const status = await pty.status;
    assertEquals(status.success, true);
    assertEquals(status.code, 0);
    assertEquals(status.signal, null);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyInputIsEchoedAndRead() {
    // `cat` echoes its input back; the terminal echoes what is typed too
    const pty = Deno.openPty({
      cmd: "cat",
      cols: 80,
      rows: 24,
    });
    await write(pty, "ping\r");
    // the echo ("ping\r\n") and cat's copy of the line
    const seen = await readUntil(pty.readable, `ping${EOL}ping${EOL}`);
    assertStringIncludes(seen, `ping${EOL}ping${EOL}`);
    // end-of-file for the child: the terminal has no half-close
    await write(pty, "\x04");
    const rest = await readAll(pty.readable);
    const status = await pty.status;
    assertEquals(status.code, 0, rest);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyIsATerminalWithTheGivenSize() {
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "stty size; tty >/dev/null && echo is-a-tty"],
      cols: 132,
      rows: 43,
    });
    const output = await readAll(pty.readable);
    assertStringIncludes(output, `43 132${EOL}`);
    assertStringIncludes(output, `is-a-tty${EOL}`);
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyResizeIsVisibleToTheChild() {
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "stty size; read line; stty size"],
      cols: 80,
      rows: 24,
    });
    const first = await readUntil(pty.readable, `24 80${EOL}`);
    assertStringIncludes(first, `24 80${EOL}`);
    pty.resize(100, 50);
    await write(pty, "go\r");
    const output = await readAll(pty.readable);
    assertStringIncludes(output, `50 100${EOL}`);
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  function ptyResizeRejectsBadSizes() {
    const pty = Deno.openPty({
      cmd: shell,
      args: shellArgs("echo sized"),
      cols: 80,
      rows: 24,
    });
    assertThrows(() => pty.resize(0, 24), TypeError, "'cols'");
    assertThrows(() => pty.resize(80, 70000), TypeError, "'rows'");
    assertThrows(() => pty.resize(1.5, 24), TypeError, "'cols'");
    return pty[Symbol.asyncDispose]();
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  async function ptyExitCode() {
    const pty = Deno.openPty({
      cmd: shell,
      args: shellArgs("exit 42"),
      cols: 80,
      rows: 24,
    });
    await readAll(pty.readable);
    const status = await pty.status;
    assertEquals(status.success, false);
    assertEquals(status.code, 42);
    assertEquals(status.signal, null);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyKillReportsTheSignal() {
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "echo ready; exec sleep 30"],
      cols: 80,
      rows: 24,
    });
    await readUntil(pty.readable, "ready");
    pty.kill("SIGTERM");
    const status = await pty.status;
    assertEquals(status.success, false);
    assertEquals(status.signal, "SIGTERM");
    assertEquals(status.code, 128 + 15);
    await readAll(pty.readable);
    // the child is reaped: killing again is an error, as for Deno.Command
    assertThrows(
      () => pty.kill("SIGTERM"),
      TypeError,
      "already terminated",
    );
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyKillDefaultsToSighup() {
    // What a closing terminal sends. Two children, each in a state the
    // signal can't race: the test used to trap HUP in a shell that then
    // ran `exec sleep`, and a kill that arrived before the exec (a loaded
    // machine) ran the trap, so the child exited 3 instead of dying of the
    // signal.
    // 1. Nothing traps it, before or after the exec: the child dies of it.
    const plain = Deno.openPty({
      cmd: "sh",
      args: ["-c", "echo ready; exec sleep 30"],
      cols: 80,
      rows: 24,
    });
    await readUntil(plain.readable, "ready");
    plain.kill();
    const status = await plain.status;
    await readAll(plain.readable);
    assertEquals(status.signal, "SIGHUP");
    assertEquals(status.code, 128 + 1);
    // 2. A shell that traps it, and stays the shell, says which signal came
    // (it runs the trap once the `sleep` in the foreground ends).
    const trapping = Deno.openPty({
      cmd: "sh",
      args: [
        "-c",
        "trap 'echo hup; exit 3' HUP; echo ready; while :; do sleep 0.1; done",
      ],
      cols: 80,
      rows: 24,
    });
    await readUntil(trapping.readable, "ready");
    trapping.kill();
    const output = await readAll(trapping.readable);
    assertStringIncludes(output, `hup${EOL}`);
    const trapped = await trapping.status;
    assertEquals(trapped.signal, null);
    assertEquals(trapped.code, 3);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyKillOnlyReachesTheChild() {
    // `kill` signals the pid `openPty` returned, not the process group it
    // leads: a background job of the shell, in the same group, is not
    // signalled. (It ignores SIGHUP because the kernel sends the terminal's
    // foreground group one when its session leader, the shell, exits; a
    // SIGTERM reaching it could only come from a group-wide kill.)
    // The job says `ready` once it ignores SIGHUP, and the test waits for
    // that: a SIGTERM sent as soon as the shell had printed the job's pid
    // could end the shell before the job had run its `trap`, and the
    // kernel's SIGHUP then killed the job (ESRCH below, on a loaded machine).
    const pty = Deno.openPty({
      cmd: "sh",
      args: [
        "-c",
        "trap 'echo term; exit 4' TERM; (trap '' HUP; echo ready; exec sleep 30 >/dev/null 2>&1) </dev/null & echo pid=$!; wait $!",
      ],
      cols: 80,
      rows: 24,
    });
    const seen = await readUntil(
      pty.readable,
      (text) => /pid=\d+\r?\n/.test(text) && text.includes("ready"),
    );
    const grandchild = Number(seen.match(/pid=(\d+)/)?.[1]);
    assert(grandchild > 0, seen);
    pty.kill("SIGTERM");
    const output = await readAll(pty.readable);
    assertStringIncludes(output, `term${EOL}`);
    assertEquals((await pty.status).code, 4);
    // the grandchild outlived the shell: `kill` was scoped to the child
    Deno.kill(grandchild, "SIGTERM");
  },
);

Deno.test(
  { permissions: { run: true, read: true, write: true }, ignore: !isWindows },
  async function ptyEnvAndCwdOnWindows() {
    const cwd = await Deno.makeTempDir({ prefix: "deno_pty_test" });
    try {
      // wide enough that the line is never wrapped by the console
      const pty = Deno.openPty({
        cmd: "cmd.exe",
        args: ["/d", "/c", "echo [%PTY_TEST_VAR%^|%CD%]"],
        cwd,
        env: { PTY_TEST_VAR: "yes" },
        cols: 400,
        rows: 24,
      });
      const output = plainText(await readAll(pty.readable)).toLowerCase();
      // the directory as given, or its long form where the temporary
      // directory was named by an 8.3 short path
      const real = (await Deno.realPath(cwd)).replace(/^\\\\\?\\/, "");
      assert(
        [cwd, real].some((dir) =>
          output.includes(`[yes|${dir.toLowerCase()}]`)
        ),
        output,
      );
      assertEquals((await pty.status).code, 0);
    } finally {
      await Deno.remove(cwd, { recursive: true });
    }
  },
);

Deno.test(
  { permissions: { run: true, read: true, write: true }, ignore: isWindows },
  async function ptyEnvAndCwd() {
    const cwd = await Deno.makeTempDir({ prefix: "deno_pty_test" });
    try {
      const pty = Deno.openPty({
        cmd: "sh",
        args: ["-c", 'echo "$TERM|$PTY_TEST_VAR|$(pwd)"'],
        cwd,
        env: { TERM: "xterm-256color", PTY_TEST_VAR: "yes" },
        cols: 80,
        rows: 24,
      });
      const output = await readAll(pty.readable);
      assertStringIncludes(output, `xterm-256color|yes|`);
      // the cwd as the shell sees it (symlinks resolved, as `pwd -P`)
      const real = await Deno.realPath(cwd);
      assertStringIncludes(output, `|${real}${EOL}`);
      assertEquals((await pty.status).code, 0);
    } finally {
      await Deno.remove(cwd, { recursive: true });
    }
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyClearEnv() {
    const pty = Deno.openPty({
      cmd: "/bin/sh",
      args: ["-c", "env | grep -c ^PATH= || echo none; echo TERM=$TERM"],
      clearEnv: true,
      env: { TERM: "dumb" },
      cols: 80,
      rows: 24,
    });
    const output = await readAll(pty.readable);
    assertStringIncludes(output, `none${EOL}`);
    assertStringIncludes(output, `TERM=dumb${EOL}`);
    await pty.status;
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  async function ptyAsyncDispose() {
    let status: Deno.CommandStatus | undefined;
    {
      // A child that neither writes nor exits: the dispose must not depend
      // on its output (`pause` waits for a key that never comes).
      await using pty = Deno.openPty({
        cmd: shell,
        args: shellArgs(isWindows ? "pause >nul" : "exec sleep 30"),
        cols: 80,
        rows: 24,
      });
      pty.status.then((s) => status = s);
    }
    // disposed: the child is gone and its status settled
    const settled = status;
    assert(settled !== undefined);
    if (isWindows) {
      // a kill on Windows ends the child's job, each process with exit code 1
      assertEquals(settled.code, 1);
      assertEquals(settled.signal, null);
    } else {
      assertEquals(settled.signal, "SIGHUP");
    }
    // the master is closed with it: the stream ends or errors, never hangs
    await new Promise((resolve) => setTimeout(resolve, 0));
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  async function ptyReadableEndsWhenTheChildExits() {
    const pty = Deno.openPty({
      cmd: shell,
      args: shellArgs("echo bye"),
      cols: 80,
      rows: 24,
    });
    // reading `readable` to its end closes the terminal: nothing is left
    // for the resource sanitizer
    const output = await readAll(pty.readable);
    assertStringIncludes(output, "bye");
    await pty.status;
  },
);

Deno.test(
  { permissions: { run: false, read: true } },
  function ptyRequiresRunPermission() {
    assertThrows(
      () =>
        Deno.openPty({
          cmd: shell,
          args: shellArgs("echo nope"),
          cols: 80,
          rows: 24,
        }),
      Deno.errors.NotCapable,
      "run",
    );
  },
);

Deno.test(
  { permissions: { run: ["sh"], read: true }, ignore: isWindows },
  async function ptyScopedRunPermissionAllowsOnlyTheListedProgram() {
    // `--allow-run=sh`: sh may run, anything else may not. The child gets
    // only PATH: an `LD_*` variable inherited from the host (CI sets
    // LD_LIBRARY_PATH) needs full run permission, which is not what this
    // checks.
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "echo allowed"],
      ...scopedEnv,
      cols: 80,
      rows: 24,
    });
    assertStringIncludes(await readAll(pty.readable), "allowed");
    await pty.status;
    assertThrows(
      () => Deno.openPty({ cmd: "cat", ...scopedEnv, cols: 80, rows: 24 }),
      Deno.errors.NotCapable,
      'Requires run access to "cat"',
    );
    assertThrows(
      () =>
        Deno.openPty({
          cmd: Deno.execPath(),
          ...scopedEnv,
          cols: 80,
          rows: 24,
        }),
      Deno.errors.NotCapable,
      "Requires run access to",
    );
  },
);

Deno.test(
  { permissions: { run: ["sh"], read: true }, ignore: isWindows },
  function ptyRefusesLdPreloadWithoutFullRunPermission() {
    // the same rule as Deno.Command: a child with LD_* / DYLD_* set needs
    // --allow-run for everything, not an allow-list
    assertThrows(
      () =>
        Deno.openPty({
          cmd: "sh",
          args: ["-c", "true"],
          clearEnv: true,
          env: { LD_PRELOAD: "/nope.so", DYLD_INSERT_LIBRARIES: "/nope" },
          cols: 80,
          rows: 24,
        }),
      Deno.errors.NotCapable,
      "LD_PRELOAD",
    );
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  function ptyMissingProgram() {
    assertThrows(
      () =>
        Deno.openPty({
          cmd: "this-program-does-not-exist-pty",
          cols: 80,
          rows: 24,
        }),
      Deno.errors.NotFound,
    );
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  function ptyOptionValidation() {
    assertThrows(
      // @ts-expect-error: missing cols / rows
      () => Deno.openPty({ cmd: shell }),
      TypeError,
      "'cols'",
    );
    assertThrows(
      () => Deno.openPty({ cmd: shell, cols: 80, rows: 0 }),
      TypeError,
      "'rows'",
    );
    assertThrows(
      // @ts-expect-error: cmd must be a string or URL
      () => Deno.openPty({ cmd: 42, cols: 80, rows: 24 }),
      TypeError,
      "'cmd'",
    );
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  function ptyRefusesNulInArguments() {
    // a NUL would end a Windows command line there, dropping what follows
    assertThrows(
      () =>
        Deno.openPty({
          cmd: shell,
          args: [...shellArgs("echo a"), "b\0c"],
          cols: 80,
          rows: 24,
        }),
      Error,
      "nul byte",
    );
  },
);

Deno.test(
  { permissions: { run: true, read: true, write: true }, ignore: !isWindows },
  async function ptyBatchFileArgumentsCannotRunCommands() {
    // A `.bat` runs through cmd.exe: its arguments are escaped against cmd's
    // own parsing, as Deno.Command does it (BatBadBut), so a quote in one
    // cannot end the quoting and start a second command.
    const dir = Deno.makeTempDirSync();
    const script = `${dir}\\args.bat`;
    Deno.writeTextFileSync(script, "@echo off\r\necho arg=%1\r\n");
    const marker = `${dir}\\pwned.txt`;
    const pty = Deno.openPty({
      cmd: script,
      args: [`"&echo pwned>${marker}&rem `],
      cols: 80,
      rows: 24,
    });
    const output = await readAll(pty.readable);
    await pty.status;
    assertStringIncludes(output, "arg=");
    assertThrows(() => Deno.statSync(marker), Deno.errors.NotFound);
    Deno.removeSync(dir, { recursive: true });
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyInputReachesTheChildOnWindows() {
    // `set /p` reads a line from the console's input: what is written to
    // `writable` (Enter is "\r"); a resize of the running console works too
    const pty = Deno.openPty({
      cmd: shell,
      args: ["/d", "/v:on", "/c", "set /p line=&echo got:!line!"],
      cols: 80,
      rows: 24,
    });
    pty.resize(100, 50);
    await write(pty, "hello\r");
    const output = await readAll(pty.readable);
    assertStringIncludes(output, "got:hello");
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyKillEndsTheChildOnWindows() {
    // any signal ends the child's job (exit code 1, as Deno.kill on
    // Windows); the output ends once the console is closed after the exit
    const pty = Deno.openPty({
      cmd: shell,
      args: shellArgs("pause >nul"),
      cols: 80,
      rows: 24,
    });
    pty.kill("SIGTERM");
    const status = await pty.status;
    assertEquals(status.success, false);
    assertEquals(status.code, 1);
    assertEquals(status.signal, null);
    await readAll(pty.readable);
    assertThrows(
      () => pty.kill("SIGTERM"),
      TypeError,
      "already terminated",
    );
  },
);

// Whether a process with this id and image name runs (`tasklist`'s CSV
// line names both).
function windowsProcessRuns(pid: number, image: string): boolean {
  const { stdout } = new Deno.Command("tasklist", {
    args: ["/fi", `PID eq ${pid}`, "/fo", "csv", "/nh"],
    stdin: "null",
    stdout: "piped",
    stderr: "null",
  }).outputSync();
  const text = new TextDecoder().decode(stdout).toLowerCase();
  return text.includes(`"${image.toLowerCase()}","${pid}"`);
}

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyKillEndsTheProcessTreeOnWindows() {
    // The shell's child is in the job the terminal put the shell in: a kill
    // ends it too, as closing a Unix terminal ends its session. (A kill
    // that reached the shell alone left `ping` running for 5 minutes.)
    const pty = Deno.openPty({
      cmd: "cmd.exe",
      args: ["/d", "/c", "ping -n 300 127.0.0.1"],
      cols: 80,
      rows: 24,
    });
    const drained = readAll(pty.readable);
    const fg = await foregroundUntil(
      pty,
      (fg) => fg?.name.toLowerCase() === "ping.exe",
    );
    assert(fg !== null && fg.pid !== pty.pid);
    const grandchild = fg.pid;
    assert(windowsProcessRuns(grandchild, "ping.exe"));
    pty.kill();
    const status = await pty.status;
    assertEquals(status.code, 1);
    await drained;
    // the job's processes are ended together; wait for the system to have
    // removed the grandchild, which takes a moment on a loaded machine
    const deadline = Date.now() + 60_000;
    while (windowsProcessRuns(grandchild, "ping.exe")) {
      assert(Date.now() < deadline, `ping.exe ${grandchild} outlived the kill`);
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyResizeIsVisibleToTheChildOnWindows() {
    // `mode con` prints the console's size: before the resize, then (after
    // a line of input) after it
    const pty = Deno.openPty({
      cmd: "cmd.exe",
      args: ["/d", "/c", "mode con & set /p line= & echo resized: & mode con"],
      cols: 80,
      rows: 24,
    });
    const first = await readUntil(
      pty.readable,
      (text) => /Columns:\s+80\b/.test(plainText(text)),
    );
    assertMatch(plainText(first), /Columns:\s+80\b/);
    pty.resize(100, 50);
    await write(pty, "go\r");
    const output = plainText(await readAll(pty.readable));
    const after = output.slice(output.lastIndexOf("resized:"));
    assertMatch(after, /Columns:\s+100\b/);
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyUtf8RoundTripOnWindows() {
    // A ConPTY's output is UTF-8 whatever the console's code page; under
    // code page 65001 the shell's own output is too. Non-ASCII text from
    // the command line comes back intact.
    const pty = Deno.openPty({
      cmd: "cmd.exe",
      args: ["/d", "/c", "chcp 65001 >nul & echo [héllo ünï → 世界 ✓]"],
      cols: 120,
      rows: 24,
    });
    const output = plainText(await readAll(pty.readable));
    for (const piece of ["héllo", "ünï", "→", "世界", "✓"]) {
      assertStringIncludes(output, piece);
    }
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyVtOutputPassesThroughOnWindows() {
    // The console interprets the child's VT sequences and draws them for
    // the terminal: a title (OSC 0) and a colour (SGR 31) the shell set
    // arrive as sequences, not as text.
    const pty = Deno.openPty({
      cmd: "cmd.exe",
      args: [
        "/d",
        "/c",
        "title pty-vt-title & echo \x1b[31mred-text\x1b[0m",
      ],
      cols: 80,
      rows: 24,
    });
    const output = await readAll(pty.readable);
    // deno-lint-ignore no-control-regex
    assertMatch(output, /\x1b\]0;[^\x07\x1b]*pty-vt-title/);
    // deno-lint-ignore no-control-regex
    assertMatch(output, /\x1b\[(?:[0-9;]*;)?31(?:;[0-9;]*)?m[\s\S]*red-text/);
    assert(!plainText(output).includes("[31m"), "an SGR drawn as text");
    assertEquals((await pty.status).code, 0);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptySpawnsPowerShellOnWindows() {
    // PowerShell 7 (`pwsh`) and Windows PowerShell (`powershell.exe`), each
    // where it is installed: both run on the terminal and see its size (one
    // that is not installed is passed over).
    for (const cmd of ["pwsh.exe", "powershell.exe"]) {
      let pty: Deno.Pty;
      try {
        pty = Deno.openPty({
          cmd,
          args: [
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "'width=' + $Host.UI.RawUI.WindowSize.Width + ';ok'",
          ],
          cols: 110,
          rows: 30,
        });
      } catch (err) {
        if (err instanceof Deno.errors.NotFound) continue;
        throw err;
      }
      const output = plainText(await readAll(pty.readable));
      assertStringIncludes(output, "width=110;ok", cmd);
      assertEquals((await pty.status).code, 0, cmd);
    }
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptySpawnsWslOnWindows() {
    // Only where WSL is installed and has a distribution (`wsl.exe -l -q`
    // lists them, in UTF-16; it fails where none is set up or WSL is off).
    let distros = "";
    try {
      const listed = await new Deno.Command("wsl.exe", {
        args: ["-l", "-q"],
        stdin: "null",
        stdout: "piped",
        stderr: "null",
      }).output();
      if (listed.success) {
        distros = new TextDecoder("utf-16le").decode(listed.stdout)
          .replaceAll("\0", "").trim();
      }
    } catch (err) {
      if (!(err instanceof Deno.errors.NotFound)) throw err;
    }
    // No WSL distribution on this machine: nothing to check.
    if (distros === "") return;
    const pty = Deno.openPty({
      cmd: "wsl.exe",
      args: ["-e", "sh", "-c", "echo wsl-pty-$((40 + 2))"],
      cols: 80,
      rows: 24,
    });
    const output = plainText(await readAll(pty.readable));
    assertStringIncludes(output, "wsl-pty-42");
    assertEquals((await pty.status).code, 0);
  },
);

// `Deno.Command` with `pty` (item 260): thin sugar over the same spawn.

Deno.test(
  { permissions: { run: true, read: true } },
  async function commandPtySpawnsOnATerminal() {
    const child = new Deno.Command(shell, {
      args: shellArgs(
        isWindows ? "echo from command" : "tty; echo from command",
      ),
      pty: { cols: 80, rows: 24 },
    }).spawn();
    assert(child.pid > 0);
    const output = await readAll(child.pty.readable);
    assertStringIncludes(output, `from command${LINE_END}`);
    if (!isWindows) {
      assertMatch(output, /\/dev\/(pts\/\d+|tty[a-z0-9]+)/);
    }
    const status = await child.status;
    assertEquals(status.code, 0);
    // stdio is the terminal, not pipes
    assertThrows(() => child.stdout, TypeError, "not piped");
    assertThrows(() => child.stdin, TypeError, "not piped");
    assertThrows(() => child.stderr, TypeError, "not piped");
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function commandPtyResizeKillAndDispose() {
    const child = new Deno.Command("sh", {
      args: ["-c", "stty size; exec sleep 30"],
      pty: { cols: 80, rows: 24 },
    }).spawn();
    assertStringIncludes(
      await readUntil(child.pty.readable, EOL),
      `24 80${EOL}`,
    );
    child.pty.resize(90, 30);
    child.kill("SIGTERM");
    const status = await child.status;
    assertEquals(status.signal, "SIGTERM");
    await readAll(child.pty.readable);
    await child[Symbol.asyncDispose]();
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  async function commandPtyConflictingOptionsThrow() {
    for (
      const options of [
        { stdin: "piped" },
        { stdout: "piped" },
        { stderr: "null" },
        { uid: 1000 },
        { gid: 1000 },
        { detached: true },
        { windowsRawArguments: true },
      ] as const
    ) {
      const name = Object.keys(options)[0];
      assertThrows(
        () =>
          new Deno.Command(shell, {
            args: shellArgs("echo no"),
            pty: { cols: 80, rows: 24 },
            ...options,
          }).spawn(),
        TypeError,
        `'${name}' cannot be set together with 'pty'`,
      );
    }
    const command = new Deno.Command(shell, {
      args: shellArgs("echo no"),
      pty: { cols: 80, rows: 24 },
    });
    assertThrows(() => command.output(), TypeError, "'pty'");
    assertThrows(() => command.outputSync(), TypeError, "'pty'");
    const plain = new Deno.Command(shell, {
      args: shellArgs("echo no"),
      stdin: "null",
      stdout: "null",
      stderr: "null",
    }).spawn();
    assertThrows(
      () => plain.pty,
      TypeError,
      "not spawned on a pseudo-terminal",
    );
    await plain.status;
  },
);

Deno.test(
  { permissions: { run: false, read: true } },
  function commandPtyRequiresRunPermission() {
    assertThrows(
      () =>
        new Deno.Command(shell, {
          args: shellArgs("echo nope"),
          pty: { cols: 80, rows: 24 },
        }).spawn(),
      Deno.errors.NotCapable,
      "run",
    );
  },
);

// `Deno.Pty.foreground()` and `ttyName` (DENEXT.md item 462).

// What `sh` is called once it runs (macOS's /bin/sh re-executes bash, dash
// or zsh).
const SH_NAME = /^(sh|bash|dash|zsh)$/;

// Polls `foreground()` until `accept` takes its answer: a shell starts a
// command (fork, a new process group, exec) in steps the terminal shows one
// by one.
async function foregroundUntil(
  pty: { foreground(): Deno.PtyForegroundProcess | null },
  accept: (fg: Deno.PtyForegroundProcess | null) => boolean,
): Promise<Deno.PtyForegroundProcess | null> {
  const deadline = Date.now() + 60_000;
  let fg = pty.foreground();
  while (!accept(fg)) {
    if (Date.now() > deadline) {
      throw new Error(`foreground never matched: ${JSON.stringify(fg)}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
    fg = pty.foreground();
  }
  return fg;
}

Deno.test(
  {
    // env: the test hands its own PATH to the shell (the runtime reads none)
    permissions: { run: true, read: true, write: true, env: ["PATH"] },
    ignore: isWindows,
  },
  async function ptyForegroundFollowsTheShellsJobs() {
    const dir = await Deno.realPath(await Deno.makeTempDir());
    const other = await Deno.realPath(await Deno.makeTempDir());
    try {
      // an interactive shell has job control: each command runs in a process
      // group of its own, which the shell makes the terminal's foreground
      const path = Deno.env.get("PATH") ?? "/usr/bin:/bin";
      // the child's argv[0] is the program as the runtime resolved it, as
      // Deno.Command execs it: the first directory on PATH with `sh`
      let resolvedSh: string | undefined;
      for (const entry of path.split(":")) {
        if (entry === "") continue;
        try {
          if ((await Deno.stat(`${entry}/sh`)).isFile) {
            resolvedSh = `${entry}/sh`;
            break;
          }
        } catch {
          // not there
        }
      }
      assert(resolvedSh !== undefined, `no sh on ${path}`);
      const pty = Deno.openPty({
        cmd: "sh",
        args: ["-i"],
        cwd: dir,
        clearEnv: true,
        env: { PATH: path, PS1: "$ " },
        cols: 80,
        rows: 24,
      });
      // drained for the whole test, so the shell never blocks on output
      let output = "";
      const drained = (async () => {
        const decoder = new TextDecoder();
        for await (const chunk of pty.readable) {
          output += decoder.decode(chunk, { stream: true });
        }
      })();

      // at its prompt, the shell is its terminal's foreground process
      const atPrompt = await foregroundUntil(
        pty,
        (fg) => fg?.pid === pty.pid && fg.argv !== null,
      );
      assert(atPrompt !== null);
      assertEquals(atPrompt.pgid, pty.pid);
      // the name is the program's own: macOS's /bin/sh runs another shell
      assertMatch(atPrompt.name, SH_NAME);
      assertEquals(atPrompt.argv, [resolvedSh, "-i"]);
      assertEquals(atPrompt.cwd, dir);

      // the device is the one `tty` names inside the terminal
      assert(pty.ttyName !== null);
      assertMatch(pty.ttyName, /^\/dev\/(pts\/\d+|ttys\d+)$/);
      await write(pty, "tty\r");
      const ttyDeadline = Date.now() + 60_000;
      while (!output.includes(`${pty.ttyName}\r\n`)) {
        assert(Date.now() < ttyDeadline, `no tty in ${JSON.stringify(output)}`);
        await new Promise((resolve) => setTimeout(resolve, 10));
      }

      // a command in the foreground: its own group, its arguments, and the
      // directory the shell was in when it started it
      await write(pty, `cd '${other}'\r`);
      await write(pty, "sleep 300\r");
      const running = await foregroundUntil(
        pty,
        (fg) => fg?.name === "sleep" && fg.argv !== null,
      );
      assert(running !== null);
      assertEquals(running.argv, ["sleep", "300"]);
      assertEquals(running.cwd, other);
      assertEquals(running.pgid, running.pid);
      assert(running.pid !== pty.pid);

      // interrupted: the shell is in the foreground again, where it went
      await write(pty, "\x03");
      const back = await foregroundUntil(pty, (fg) => fg?.pid === pty.pid);
      assertMatch(back?.name ?? "", SH_NAME);
      assertEquals(back?.cwd, other);

      // a pipeline whose first command (the group's leader) has exited is
      // described by the member still running
      await write(pty, "true | sleep 301\r");
      const member = await foregroundUntil(
        pty,
        (fg) => fg?.name === "sleep" && fg.argv?.[1] === "301",
      );
      assert(member !== null);
      assert(member.pgid !== null && member.pgid !== pty.pid);
      await write(pty, "\x03");
      await foregroundUntil(pty, (fg) => fg?.pid === pty.pid);

      await write(pty, "exit\r");
      const status = await pty.status;
      assertEquals(status.signal, null);
      await drained;
      // the terminal is closed: nothing to describe, and no throw
      assertEquals(pty.foreground(), null);
      assertMatch(pty.ttyName, /^\/dev\//);
    } finally {
      await Deno.remove(dir, { recursive: true });
      await Deno.remove(other, { recursive: true });
    }
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: isWindows },
  async function ptyForegroundAfterExecAndAfterTheChildIsGone() {
    // `exec` keeps the process and its group: the same pid, another program
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "echo ready; exec sleep 300"],
      cols: 80,
      rows: 24,
    });
    await readUntil(pty.readable, "ready");
    // Linux renames the process (comm) during exec before it lays out the
    // new argv, so for a moment the name is `sleep` and the command line is
    // still empty (argv null): wait for both.
    const fg = await foregroundUntil(
      pty,
      (fg) => fg?.name === "sleep" && fg.argv !== null,
    );
    assertEquals(fg?.pid, pty.pid);
    assertEquals(fg?.pgid, pty.pid);
    assertEquals(fg?.argv, ["sleep", "300"]);
    pty.kill("SIGKILL");
    await pty.status;
    // reaped, the terminal still open: its session is over, so it has no
    // foreground process (never a stale or unrelated one)
    assertEquals(pty.foreground(), null);
    await readAll(pty.readable);
    assertEquals(pty.foreground(), null);
  },
);

Deno.test(
  { permissions: { run: true, read: true }, ignore: !isWindows },
  async function ptyForegroundOnWindowsIsFoundInTheProcessTree() {
    const pty = Deno.openPty({
      cmd: "cmd.exe",
      args: ["/d", "/k", "echo ready"],
      cols: 80,
      rows: 24,
    });
    let output = "";
    const drained = (async () => {
      const decoder = new TextDecoder();
      for await (const chunk of pty.readable) {
        output += decoder.decode(chunk, { stream: true });
      }
    })();
    // a ConPTY is not a named device
    assertEquals(pty.ttyName, null);
    // the shell at its prompt
    const atPrompt = await foregroundUntil(
      pty,
      (fg) => fg?.pid === pty.pid && fg.argv !== null && fg.cwd !== null,
    );
    assert(atPrompt !== null);
    assertEquals(atPrompt.pgid, null);
    assertEquals(atPrompt.name.toLowerCase(), "cmd.exe");
    assertEquals(atPrompt.argv?.slice(1), ["/d", "/k", "echo ready"]);
    assertEquals(atPrompt.cwd?.toLowerCase(), Deno.cwd().toLowerCase());

    // a command the shell waits for
    await write(pty, "ping -n 300 127.0.0.1\r");
    const running = await foregroundUntil(
      pty,
      (fg) => fg?.name.toLowerCase() === "ping.exe" && fg.argv !== null,
    );
    assert(running !== null);
    assert(running.pid !== pty.pid);
    assertEquals(running.pgid, null);
    assertEquals(running.argv?.slice(1), ["-n", "300", "127.0.0.1"]);

    // Ctrl+C ends it: the shell again. A console delivers Ctrl+C only to a
    // process that has attached its handler; one sent while ping is still
    // starting is lost (as when typed), so it is sent again until the shell
    // is back
    const ctrlCDeadline = Date.now() + 60_000;
    let fg = pty.foreground();
    while (fg?.pid !== pty.pid) {
      if (Date.now() > ctrlCDeadline) {
        throw new Error(`Ctrl+C never ended ping: ${JSON.stringify(fg)}`);
      }
      await write(pty, "\x03");
      const sent = Date.now();
      while (fg?.pid !== pty.pid && Date.now() - sent < 2_000) {
        await new Promise((resolve) => setTimeout(resolve, 10));
        fg = pty.foreground();
      }
    }

    await write(pty, "exit\r");
    await pty.status;
    await drained;
    assert(output.includes("ready"));
    assertEquals(pty.foreground(), null);
  },
);

Deno.test(
  { permissions: { run: true, read: true } },
  async function commandPtyHasForegroundAndTtyName() {
    const child = new Deno.Command(shell, {
      args: isWindows
        ? ["/d", "/c", "ping -n 300 127.0.0.1"]
        : ["-c", "echo ready; exec sleep 300"],
      pty: { cols: 80, rows: 24 },
    }).spawn();
    const drained = readAll(child.pty.readable);
    const fg = await foregroundUntil(
      child.pty,
      (fg) => fg !== null && /^(sleep|ping\.exe)$/i.test(fg.name),
    );
    assert(fg !== null);
    if (isWindows) {
      assertEquals(child.pty.ttyName, null);
      assertEquals(fg.pgid, null);
    } else {
      assertMatch(child.pty.ttyName ?? "", /^\/dev\//);
      assertEquals(fg.pid, child.pid);
      assertEquals(fg.pgid, child.pid);
    }
    if (isWindows) {
      // the shell's child holds the console: end it, then the shell
      new Deno.Command("taskkill", {
        args: ["/f", "/t", "/pid", String(child.pid)],
        stdout: "null",
        stderr: "null",
      }).outputSync();
    } else {
      child.kill("SIGKILL");
    }
    await child.status;
    await drained;
    assertEquals(child.pty.foreground(), null);
  },
);
