// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The launcher's view of a compiled program under its own supervisor: the
// PID it started stays the supervisor's (the program is its child, forked
// in place: no exec ever changes what that PID is), a signal sent to it
// reaches the program, and it ends as the program ends.
async function start(what: string) {
  const child = new Deno.Command("./prog_bpid", {
    args: [what],
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

{
  const { child, line, rest } = await start("wait");
  const parent = Number(await line());
  console.log(
    `the program is a child of the launched PID: ${parent === child.pid}`,
  );
  child.kill("SIGTERM");
  const status = await child.status;
  console.log(await rest());
  console.log(`exit code after SIGTERM: ${status.code}`);
}
{
  const { child, line } = await start("exit");
  await line();
  console.log(`exit code: ${(await child.status).code}`);
}
{
  const { child, line } = await start("kill");
  await line();
  console.log(`ended by: ${(await child.status).signal}`);
}
