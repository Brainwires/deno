// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// P6: measures, from outside the sandbox, that a grandchild the confined
// process orphaned is still confined (Linux: /proc/<pid>/status; macOS:
// sandbox_check), and that it still can't read outside the grant.
const inner = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--quiet",
    "--kernel-sandbox=enforce",
    "--allow-run=sh",
    "--allow-read=inside",
    "--allow-write=inside",
    "orphan.ts",
  ],
}).outputSync();
const pid = Number(
  /grandchild (\d+)/.exec(new TextDecoder().decode(inner.stdout))?.[1],
);
console.log(`inner exited: ${inner.code}`);

const result = "inside/orphan_result.txt";
let text = "";
for (let i = 0; i < 200 && !text.includes("\n"); i++) {
  await new Promise((r) => setTimeout(r, 50));
  try {
    text = Deno.readTextFileSync(result);
  } catch {
    // not yet
  }
}
const alive = (() => {
  try {
    Deno.kill(pid, "SIGCONT");
    return true;
  } catch {
    return false;
  }
})();
console.log(`grandchild alive: ${alive}`);
const ppid = new TextDecoder().decode(
  new Deno.Command("ps", { args: ["-o", "ppid=", "-p", String(pid)] })
    .outputSync().stdout,
).trim();
console.log(`orphaned: ${ppid !== "" && Number(ppid) !== inner.pid}`);

let confined = false;
if (Deno.build.os === "linux") {
  const status = Deno.readTextFileSync(`/proc/${pid}/status`);
  const field = (name: string) =>
    new RegExp(`^${name}:\\s*(\\S+)`, "m").exec(status)?.[1];
  confined = field("NoNewPrivs") === "1" && field("Seccomp") === "2";
} else if (Deno.build.os === "darwin") {
  const lib = Deno.dlopen("/usr/lib/libSystem.B.dylib", {
    sandbox_check: { parameters: ["i32", "pointer", "i32"], result: "i32" },
  });
  confined = lib.symbols.sandbox_check(pid, null, 0) !== 0;
  lib.close();
}
console.log(`grandchild confined: ${confined}`);
console.log(text.trim());
Deno.kill(pid, "SIGKILL");
