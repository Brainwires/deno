// Copyright 2018-2026 the Deno authors. MIT license.
// The unconfined half: a free port, the confined server on it, and the
// clients (the confined process can't connect to itself).
const probe = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const port = (probe.addr as Deno.NetAddr).port;
probe.close();

const server = new Deno.Command(Deno.execPath(), {
  args: [
    "run",
    "--quiet",
    "--kernel-sandbox=enforce",
    "--allow-ffi",
    `--allow-net-listen=127.0.0.1:${port}`,
    "prebound.ts",
    String(port),
  ],
  stdout: "piped",
  stderr: "piped",
}).spawn();

async function ask(): Promise<string> {
  // bound from the server's startup: a connection waits in the backlog
  // until the program accepts
  for (let i = 0;; i++) {
    try {
      const conn = await Deno.connect({ hostname: "127.0.0.1", port });
      const text = await new Response(conn.readable).text();
      if (text) return text;
    } catch (err) {
      if (i > 200) throw err;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
}

console.log(`first listener answered: ${await ask()}`);
console.log(`second listener answered: ${await ask()}`);
const { code, stdout, stderr } = await server.output();
await Deno.stdout.write(stdout);
await Deno.stderr.write(stderr);
console.log(`server exited with ${code}`);
