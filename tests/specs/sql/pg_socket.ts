// A Unix-domain socket host: a directory (`host`, `?host=`) or the socket
// itself (`path`). No TLS over a local socket, whatever the sslmode.
const dir = Deno.env.get("DENO_SQL_TEST_POSTGRES_SOCKET")!;
const port = Number(Deno.env.get("DENO_SQL_TEST_POSTGRES_SOCKET_PORT") ?? 5432);
const url = new URL(Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!);
const password = decodeURIComponent(url.password);

async function check(label: string, sql: Deno.SQL) {
  try {
    const [row] = await sql`select inet_server_addr() is null as local,
      (select ssl from pg_stat_ssl where pid = pg_backend_pid()) as ssl`;
    console.log(label, row);
  } catch (e) {
    console.log(label, "error:", (e as Error).message);
  } finally {
    await sql.close();
  }
}

await check(
  "host:",
  new Deno.SQL({
    host: dir,
    port,
    username: "postgres",
    password,
    database: "postgres",
  }),
);
await check(
  "?host=:",
  new Deno.SQL(
    `postgres://postgres:${encodeURIComponent(password)}@/postgres?host=${
      encodeURIComponent(dir)
    }&port=${port}&sslmode=require`,
  ),
);
await check(
  "path:",
  new Deno.SQL({
    path: `${dir}/.s.PGSQL.${port}`,
    username: "postgres",
    password,
  }),
);
