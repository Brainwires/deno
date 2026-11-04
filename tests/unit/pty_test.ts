// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.openPty` and `Deno.Command` with `pty` (DENEXT.md items 259 and
// 260). Every test here fails on a build without the API (`Deno.openPty is
// not a function`); the Windows cases run only where ConPTY is.

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

async function readAll(stream: ReadableStream<Uint8Array>): Promise<string> {
  let text = "";
  const decoder = new TextDecoder();
  for await (const chunk of stream) {
    text += decoder.decode(chunk, { stream: true });
  }
  return text + decoder.decode();
}

// Reads until `needle` has been seen (or the stream ended), returning what
// was read; the reader is released, not cancelled, so the terminal stays
// readable.
async function readUntil(
  stream: ReadableStream<Uint8Array>,
  needle: string,
): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let text = "";
  try {
    while (!text.includes(needle)) {
      const { value, done } = await reader.read();
      if (done) break;
      text += decoder.decode(value, { stream: true });
    }
  } finally {
    reader.releaseLock();
  }
  return text;
}

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
    assertStringIncludes(output, `hello from pty${EOL}`);
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
    // what a closing terminal sends; a shell that traps it says so
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "trap 'echo hup; exit 3' HUP; echo ready; exec sleep 30"],
      cols: 80,
      rows: 24,
    });
    await readUntil(pty.readable, "ready");
    pty.kill();
    const status = await pty.status;
    await readAll(pty.readable);
    // `exec sleep` replaced the shell, so the signal ended it
    assertEquals(status.signal, "SIGHUP");
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
    const pty = Deno.openPty({
      cmd: "sh",
      args: [
        "-c",
        "trap 'echo term; exit 4' TERM; (trap '' HUP; exec sleep 30) </dev/null >/dev/null 2>&1 & echo $!; wait $!",
      ],
      cols: 80,
      rows: 24,
    });
    const seen = await readUntil(pty.readable, "\n");
    const grandchild = Number(seen.trim());
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
      await using pty = Deno.openPty({
        cmd: shell,
        args: shellArgs(isWindows ? "pause" : "exec sleep 30"),
        cols: 80,
        rows: 24,
      });
      pty.status.then((s) => status = s);
      if (isWindows) await readUntil(pty.readable, "Press");
    }
    // disposed: the child is gone and its status settled
    const settled = status;
    assert(settled !== undefined);
    if (!isWindows) {
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
    // `--allow-run=sh`: sh may run, anything else may not
    const pty = Deno.openPty({
      cmd: "sh",
      args: ["-c", "echo allowed"],
      cols: 80,
      rows: 24,
    });
    assertStringIncludes(await readAll(pty.readable), "allowed");
    await pty.status;
    assertThrows(
      () => Deno.openPty({ cmd: "cat", cols: 80, rows: 24 }),
      Deno.errors.NotCapable,
      "run",
    );
    assertThrows(
      () => Deno.openPty({ cmd: Deno.execPath(), cols: 80, rows: 24 }),
      Deno.errors.NotCapable,
      "run",
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
    assertStringIncludes(output, `from command${EOL}`);
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
