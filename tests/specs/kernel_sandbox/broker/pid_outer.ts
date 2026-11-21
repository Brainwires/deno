// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The launcher's view of a brokered run: the PID it started is the
// supervisor's for the whole run (the program is a child of it), a signal
// sent to that PID reaches the program, and the PID ends as the program
// ends: with its exit code, or by the signal that killed it.
const base = [
  "run",
  "--quiet",
  "--no-prompt",
  "--kernel-sandbox=broker",
  "--allow-read=.",
];

async function start(args: string[]) {
  const child = new Deno.Command(Deno.execPath(), {
    args: [...base, ...args],
    stdout: "piped",
    stderr: "inherit",
  }).spawn();
  const reader = child.stdout.getReader();
  const decoder = new TextDecoder();
  let text = "";
  const line = async () => {
    while (!text.includes("\n")) {
      const { value, done } = await reader.read();
      if (done) break;
      text += decoder.decode(value);
    }
    const end = text.indexOf("\n");
    const out = end < 0 ? text : text.slice(0, end);
    text = end < 0 ? "" : text.slice(end + 1);
    return out;
  };
  const rest = async () => {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      text += decoder.decode(value);
    }
    return text.trim();
  };
  return { child, line, rest };
}

// a signal sent to the PID the launcher holds
{
  const { child, line, rest } = await start(["pid_inner.ts", "wait"]);
  const parent = Number(await line());
  console.log(`the program is a child of the launched PID: ${parent === child.pid}`);
  child.kill("SIGTERM");
  const status = await child.status;
  console.log(await rest());
  console.log(`exit code after SIGTERM: ${status.code}`);
}
// the program's own exit code
{
  const { child, line } = await start(["pid_inner.ts", "exit"]);
  await line();
  console.log(`exit code: ${(await child.status).code}`);
}
// the program is killed by a signal: so is the launched PID
{
  const { child, line } = await start(["--allow-run", "pid_inner.ts", "kill"]);
  await line();
  console.log(`ended by: ${(await child.status).signal}`);
}
