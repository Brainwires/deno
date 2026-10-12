// query.cancel(): before it starts, while it waits for a connection, while
// the server runs it, and behind other queries pipelined on its connection
// (only the cancelled one fails).
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1 });
const code = (e: unknown) => (e as Deno.SQLError).code;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// Not started: never runs.
const never = sql`select 1`.cancel();
try {
  await never;
} catch (e) {
  console.log("before start:", code(e), never.cancelled);
}

// Running: the server stops it.
{
  const started = Date.now();
  const q = sql`select pg_sleep(10)`.execute();
  await sleep(300);
  q.cancel();
  try {
    await q;
  } catch (e) {
    console.log(
      "running:",
      code(e),
      ((e as Error).cause as Deno.PostgresError)?.code,
      e instanceof Deno.SQL.SQLError,
      Date.now() - started < 3000,
    );
  }
  console.log("connection still usable:", (await sql`select 1 as ok`)[0].ok);
}

// Waiting for the pool (its only connection is reserved): rejects at once.
{
  const reserved = await sql.reserve();
  const q = sql`select 1`.execute();
  await sleep(50);
  const started = Date.now();
  q.cancel();
  try {
    await q;
  } catch (e) {
    console.log("waiting:", code(e), Date.now() - started < 500);
  }
  reserved.release();
  console.log("pool after:", (await sql`select 2 as ok`)[0].ok);
}

// Pipelined behind another query on the same connection: the cancel request
// waits for its turn, so the query ahead completes.
{
  const ahead = sql`select pg_sleep(0.5), 'ahead' as v`.execute();
  const target = sql`select pg_sleep(10), 'target' as v`.execute();
  const behind = sql`select 'behind' as v`.execute();
  await sleep(50);
  target.cancel();
  const results = await Promise.allSettled([ahead, target, behind]);
  console.log(
    "pipelined:",
    results.map((r) =>
      r.status === "fulfilled" ? r.value[0].v : code(r.reason)
    ),
  );
}

// In a transaction: the transaction fails and rolls back.
try {
  await sql.begin(async (tx) => {
    const q = tx`select pg_sleep(10)`.execute();
    setTimeout(() => q.cancel(), 200);
    await q;
  });
} catch (e) {
  console.log("transaction:", code(e));
}

// After it finished: a no-op.
const done = sql`select 3 as v`;
console.log("finished:", (await done)[0].v, (done.cancel(), (await done)[0].v));

// A statement_timeout is not a cancel(): the server's own error.
try {
  await sql.begin(async (tx) => {
    await tx`set local statement_timeout = 100`;
    await tx`select pg_sleep(5)`;
  });
} catch (e) {
  console.log("statement_timeout:", code(e));
}
await sql.close();
