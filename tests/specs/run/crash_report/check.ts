// Runs a binary with the crash-report self-test on and checks the report it
// wrote (DENEXT.md item 180).
//
//   check.ts file <binary|deno> <args...>  a file:// report: print its fields
//   check.ts hang <binary|deno> <args...>  an https:// endpoint that accepts
//                                          and never answers: the process
//                                          still ends within 8 s
import { pathToFileURL } from "node:url";

const [mode, bin, ...args] = Deno.args;
const exe = bin === "deno" ? Deno.execPath() : bin;
const env: Record<string, string> = {
  DENEXT_CRASH_REPORT_SELFTEST: "1",
  DENEXT_CRASH_REPORT_APP: "denext@0.0.0-test",
  RUST_BACKTRACE: "0",
};

if (mode === "hang") {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const held: Deno.Conn[] = [];
  (async () => {
    for await (const conn of listener) held.push(conn);
  })();
  env.DENEXT_CRASH_REPORT_URL =
    `https://127.0.0.1:${listener.addr.port}/report`;
  const started = performance.now();
  const out = await new Deno.Command(exe, {
    args,
    env,
    stdout: "null",
    stderr: "null",
  }).output();
  const seconds = (performance.now() - started) / 1000;
  console.log("exited with success:", out.success);
  console.log("ended within 8 s:", seconds < 8, seconds < 8 ? "" : seconds);
  console.log("the endpoint was reached:", held.length > 0);
  for (const conn of held) conn.close();
  listener.close();
  Deno.exit(0);
}

const path = `${Deno.cwd()}/crash-report.jsonl`;
env.DENEXT_CRASH_REPORT_URL = pathToFileURL(path).href;
const out = await new Deno.Command(exe, {
  args,
  env,
  stdout: "null",
  stderr: "null",
}).output();
console.log("exited with success:", out.success);
const lines = Deno.readTextFileSync(path).split("\n").filter(Boolean);
console.log("report lines:", lines.length);
const r = JSON.parse(lines[0]);
for (
  const key of ["schema", "kind", "binary", "message", "app", "subcommand"]
) {
  console.log(`${key}:`, r[key]);
}
console.log("version is this runtime's:", r.version === Deno.version.deno);
console.log("gitHash is a string:", typeof r.gitHash === "string");
console.log(
  "target is a release triple:",
  /^(x86_64|aarch64)-(apple-darwin|unknown-linux-gnu|pc-windows-msvc)$/.test(
    r.target,
  ),
);
console.log("arch:", r.arch === Deno.build.arch);
console.log("stack is a source location:", /^at .+:\d+:\d+$/.test(r.stack));
console.log("thread:", r.thread);
console.log(
  "trace is a VLQ string or null:",
  r.trace === null || /^[A-Za-z0-9_-]+$/.test(r.trace),
);
console.log("uptimeMs is a number:", typeof r.uptimeMs === "number");
console.log("id is a UUID:", /^[0-9a-f-]{36}$/.test(r.id));
console.log("time is RFC 3339 UTC:", /^\d{4}-\d\d-\d\dT[\d:.]+Z$/.test(r.time));
console.log(
  "nothing private:",
  ["env", "argv", "args", "cwd", "hostname", "user", "appVersion"].every(
    (k) => !(k in r),
  ),
);
