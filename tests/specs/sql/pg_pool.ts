// The pool: max, waiting, wait timeout, idle timeout, max lifetime, close.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;

{
  const sql = new Deno.SQL(url, { max: 2 });
  const started = Date.now();
  const rows = await Promise.all(
    [1, 2, 3, 4].map(() => sql`select pg_backend_pid() as pid, pg_sleep(0.3)`),
  );
  const elapsed = Date.now() - started;
  console.log(
    "two waves:",
    elapsed >= 550,
    "connections:",
    new Set(rows.map((r) => r[0].pid)).size,
  );
  await sql.close();
}

{
  // Every connection busy: a query waits, then times out.
  const sql = new Deno.SQL(url, { max: 1, connectionTimeout: 0.4 });
  const reserved = await sql.reserve();
  const started = Date.now();
  try {
    await sql`select 1`;
  } catch (e) {
    console.log(
      "pool exhausted:",
      (e as Deno.PostgresError).code,
      Date.now() - started >= 350,
    );
  }
  // A waiter gets the connection as soon as it is released.
  const waiting = sql`select 1 as ok`.execute();
  setTimeout(() => reserved.release(), 100);
  console.log("after release:", await waiting);
  await sql.close();
}

{
  // Idle connections close after idleTimeout.
  const app = "denosql_idle_" + Date.now();
  const sql = new Deno.SQL(url, {
    max: 2,
    idleTimeout: 0.2,
    connection: { application_name: app },
  });
  const watcher = new Deno.SQL(url, { max: 1 });
  const open = async () =>
    (await watcher`select count(*)::int as n from pg_stat_activity where application_name = ${app}`)[
      0
    ].n;
  await Promise.all([sql`select pg_sleep(0.05)`, sql`select pg_sleep(0.05)`]);
  console.log("open:", await open());
  await new Promise((r) => setTimeout(r, 600));
  console.log("after idle timeout:", await open());
  await sql.close();
  await watcher.close();
}

{
  // maxLifetime: an old connection is replaced when released.
  const sql = new Deno.SQL(url, { max: 1, maxLifetime: 0.2 });
  const [a] = await sql`select pg_backend_pid() as pid`;
  const [b] = await sql`select pg_backend_pid() as pid`;
  await new Promise((r) => setTimeout(r, 300));
  const [c] = await sql`select pg_backend_pid() as pid`;
  const [d] = await sql`select pg_backend_pid() as pid`;
  console.log("lifetime:", a.pid === b.pid, b.pid !== d.pid || c.pid !== b.pid);
  await sql.close();
}

{
  // close() waits for running queries; close({ timeout: 0 }) does not.
  const sql = new Deno.SQL(url, { max: 2 });
  const slow = sql`select pg_sleep(0.3) as done`.execute();
  await new Promise((r) => setTimeout(r, 50));
  await sql.close();
  console.log("close waited:", (await slow).length);

  const sql2 = new Deno.SQL(url, { max: 2 });
  const killed = sql2`select pg_sleep(5)`.execute();
  await new Promise((r) => setTimeout(r, 100));
  const started = Date.now();
  await sql2.close({ timeout: 0 });
  try {
    await killed;
  } catch (e) {
    console.log(
      "closed now:",
      (e as Deno.PostgresError).code,
      Date.now() - started < 1000,
    );
  }
  try {
    await sql2`select 1`;
  } catch (e) {
    console.log("after close:", (e as Deno.PostgresError).code);
  }
}

{
  // A connection the server terminates is dropped from the pool.
  const sql = new Deno.SQL(url, { max: 1 });
  const killer = new Deno.SQL(url, { max: 1 });
  const [{ pid }] = await sql`select pg_backend_pid() as pid`;
  await killer`select pg_terminate_backend(${pid})`;
  await new Promise((r) => setTimeout(r, 100));
  const [{ pid: next }] = await sql`select pg_backend_pid() as pid`;
  console.log("reconnected:", next !== pid);
  await sql.close();
  await killer.close();
}
