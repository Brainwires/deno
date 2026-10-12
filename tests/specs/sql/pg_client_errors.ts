// Client-side failures, against stand-in servers on loopback.

// A server that answers the TLS request with "N".
const noTls = Deno.listen({ hostname: "127.0.0.1", port: 0 });
(async () => {
  for await (const conn of noTls) {
    const buf = new Uint8Array(8);
    let read = 0;
    while (read < 8) {
      const n = await conn.read(buf.subarray(read));
      if (n === null) break;
      read += n;
    }
    // SSLRequest: length 8, code 80877103
    const view = new DataView(buf.buffer);
    console.log(
      "ssl request:",
      view.getInt32(0) === 8 && view.getInt32(4) === 80877103,
    );
    await conn.write(new TextEncoder().encode("N"));
    conn.close();
  }
})();
const port = (noTls.addr as Deno.NetAddr).port;
for (const sslmode of ["require", "verify-full"]) {
  const sql = new Deno.SQL(
    `postgres://u:p@127.0.0.1:${port}/db?sslmode=${sslmode}`,
  );
  try {
    await sql`select 1`;
  } catch (e) {
    const err = e as Deno.PostgresError;
    console.log(sslmode, err instanceof Deno.SQL.PostgresError, err.code);
  }
  await sql.close();
}

// A server that accepts and never answers.
const silent = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const held: Deno.Conn[] = [];
(async () => {
  for await (const conn of silent) held.push(conn);
})();
{
  const sql = new Deno.SQL({
    hostname: "127.0.0.1",
    port: (silent.addr as Deno.NetAddr).port,
    username: "u",
    sslmode: "disable",
    connectionTimeout: 0.3,
  });
  const started = Date.now();
  try {
    await sql`select 1`;
  } catch (e) {
    console.log(
      "timeout:",
      (e as Deno.PostgresError).code,
      Date.now() - started >= 250,
    );
  }
  await sql.close();
}

// Nothing listening.
const closed = Deno.listen({ hostname: "127.0.0.1", port: 0 });
const closedPort = (closed.addr as Deno.NetAddr).port;
closed.close();
{
  const sql = new Deno.SQL(
    `postgres://u@127.0.0.1:${closedPort}/db?sslmode=disable`,
  );
  try {
    await sql`select 1`;
  } catch (e) {
    console.log("refused:", (e as Deno.PostgresError).code);
  }
  await sql.close();
}

// Bad connection strings and options are TypeErrors up front.
for (
  const arg of [
    "mysql://u@localhost/db",
    "redis://localhost",
    "postgres://u@localhost/db?sslmode=bogus",
  ]
) {
  try {
    new Deno.SQL(arg);
  } catch (e) {
    console.log(e instanceof TypeError, (e as Error).message);
  }
}
try {
  new Deno.SQL({ hostname: "localhost", max: 0 });
} catch (e) {
  console.log(e instanceof TypeError, (e as Error).message);
}

noTls.close();
silent.close();
for (const c of held) c.close();
