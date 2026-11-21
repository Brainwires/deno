// `DENO_LOG_LAZY_LOAD=1` prints a line for each builtin module a process
// loads after startup.
const { code, stderr } = await new Deno.Command(Deno.execPath(), {
  args: ["test", "--no-check", "idle.test.cjs"],
  env: { DENO_LOG_LAZY_LOAD: "1" },
  stdout: "null",
  stderr: "piped",
}).output();
const lines = new TextDecoder().decode(stderr).split("\n")
  .filter((line) => line.startsWith("[lazy]"));
console.log("exit code:", code);
console.log("logged lazy loads:", lines.length > 0);
console.log(
  "http modules loaded:",
  lines.filter((line) =>
    /deno_node\/https?\.ts|node:_http_|node:tls/.test(line)
  )
    .map((line) => line.split(/\s+/)[2]),
);
