// Copyright 2018-2026 the Deno authors. MIT license.
// deno-lint-ignore-file no-console
// Compiled with --allow-ffi and
// --allow-net=localhost:<echo>,localhost:<http>,2130706433:<other>: this
// process has no IP socket of its own; every one comes from the broker of
// the supervisor it was forked from.
const [echoPort, httpPort, otherPort] = Deno.args.map(Number);

function message(err: unknown): string {
  const text = err instanceof Error ? `${err.name}: ${err.message}` : `${err}`;
  // ports differ from run to run
  return text.replace(/:\d{2,5}\b/g, ":PORT").split("\n")[0];
}

async function attempt(what: string, fn: () => Promise<string>) {
  try {
    console.log(`${what}: ${await fn()}`);
  } catch (err) {
    console.log(`${what}: ${message(err)}`);
  }
}

async function echoOnce(conn: Deno.Conn): Promise<string> {
  await conn.write(new TextEncoder().encode("ping"));
  const buf = new Uint8Array(4);
  await conn.read(buf);
  conn.close();
  return new TextDecoder().decode(buf);
}

// the program runs in a child of the PID that was started (the launcher
// compares)
console.log(Deno.ppid);

await attempt("Deno.connect, a granted name", async () =>
  await echoOnce(
    await Deno.connect({ hostname: "localhost", port: echoPort }),
  ));

await attempt(
  "fetch, a granted name",
  async () => await (await fetch(`http://localhost:${httpPort}/`)).text(),
);

// Deno's own check refuses what the grant doesn't name, before any route
await attempt(
  "Deno.connect, a port the grant doesn't name",
  async () =>
    await echoOnce(
      await Deno.connect({ hostname: "localhost", port: otherPort }),
    ),
);

// Deno takes "2130706433" as a name, and the grant names it; the resolver
// reads it as 127.0.0.1, an address on this machine the grant doesn't
// name. Deno's checks alone would connect; the broker's authority refuses
// (the internal-address rule).
await attempt(
  "Deno.connect, another spelling of an address",
  async () =>
    await echoOnce(
      await Deno.connect({ hostname: "2130706433", port: otherPort }),
    ),
);

// native code has no socket that reaches an address, not even a granted one
{
  const isMac = Deno.build.os === "darwin";
  const libc = Deno.dlopen(
    isMac ? "/usr/lib/libSystem.B.dylib" : "libc.so.6",
    {
      socket: { parameters: ["i32", "i32", "i32"], result: "i32" },
      connect: { parameters: ["i32", "buffer", "u32"], result: "i32" },
      close: { parameters: ["i32"], result: "i32" },
    } as const,
  );
  const fd = libc.symbols.socket(2, 1, 0);
  let reached = false;
  if (fd >= 0) {
    const sa = new Uint8Array(16);
    const view = new DataView(sa.buffer);
    if (isMac) {
      sa[0] = 16;
      sa[1] = 2;
    } else {
      view.setUint16(0, 2, true);
    }
    view.setUint16(2, echoPort, false);
    sa.set([127, 0, 0, 1], 4);
    reached = libc.symbols.connect(fd, sa, 16) === 0;
    libc.symbols.close(fd);
  }
  console.log(
    `native connect to the granted port: ${reached ? "connected" : "refused"}`,
  );
}
