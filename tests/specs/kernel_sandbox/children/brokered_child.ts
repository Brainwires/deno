// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// A `deno run -A` started inside a brokered run. The kernel gives this
// process no IP socket and no change directly in bout/ (it holds a write
// deny); what works here works through the parent's broker.
const [port, otherPort, file] = Deno.args;

async function attempt(what: string, fn: () => Promise<string> | string) {
  try {
    console.log(`${what}: ${await fn()}`);
  } catch (err) {
    console.log(`${what}: ${(err as Error).name}`);
  }
}

console.log(
  `handed the way to the broker: ${
    Deno.env.get("DENO_INTERNAL_KERNEL_BROKER_DIAL") !== undefined
  }`,
);
await attempt("connect to the granted host", async () => {
  const conn = await Deno.connect({
    hostname: "localhost",
    port: Number(port),
  });
  await conn.write(new TextEncoder().encode("ping"));
  const buf = new Uint8Array(4);
  await conn.read(buf);
  conn.close();
  return new TextDecoder().decode(buf);
});
await attempt("create directly in the folder with the deny", () => {
  Deno.writeTextFileSync(file, "made by a child");
  return Deno.readTextFileSync(file);
});
// its own checks come first: the inherited grant is this process's too
await attempt("connect to a port the grant doesn't name", async () => {
  const conn = await Deno.connect({
    hostname: "localhost",
    port: Number(otherPort),
  });
  conn.close();
  return "connected";
});
await attempt("create in the deny", () => {
  Deno.writeTextFileSync("bout/keep/x.txt", "x");
  return "created";
});
