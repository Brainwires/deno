// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// The unconfined half: a folder with a write deny inside the write allow,
// an echo server, then the parent under --kernel-sandbox=broker with a
// host-scoped network grant (its network is the broker's).
Deno.mkdirSync("bout/keep", { recursive: true });
for (const stale of ["bout/child.txt", "bout/grandchild.txt"]) {
  try {
    Deno.removeSync(stale);
  } catch {
    // not there
  }
}
const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
(async () => {
  for await (const conn of listener) {
    conn.readable.pipeTo(conn.writable).catch(() => {});
  }
})().catch(() => {});
const other = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const port = (listener.addr as Deno.NetAddr).port;
const otherPort = (other.addr as Deno.NetAddr).port;

const { code, stdout, stderr } = await new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--quiet",
    "--no-prompt",
    "--kernel-sandbox=broker",
    "--allow-run",
    "--allow-ffi",
    "--allow-env=DENO_INTERNAL_KERNEL_BROKER_DIAL",
    "--allow-read=.",
    "--allow-write=bout",
    "--deny-write=bout/keep",
    `--allow-net=localhost:${port}`,
    "brokered_parent.ts",
    String(port),
    String(otherPort),
    ...Deno.args,
  ],
}).output();
await Deno.stdout.write(stdout);
await Deno.stderr.write(stderr);
console.log(`parent exited with ${code}`);
listener.close();
other.close();
