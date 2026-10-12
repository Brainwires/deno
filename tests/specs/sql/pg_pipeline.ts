// Single queries share pooled connections (pipelined: sent back to back,
// answered in order); transactions and reserved connections never share.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 2 });

const rows = await Promise.all(
  Array.from(
    { length: 200 },
    (_, i) => sql`select ${i}::int as i, pg_backend_pid() as pid`,
  ),
);
console.log(
  "200 queries:",
  rows.every((r, i) => r[0].i === i),
  "connections:",
  new Set(rows.map((r) => r[0].pid)).size,
);

// A failing query in the pipeline fails alone.
const mixed = await Promise.allSettled([
  sql`select 1 as v`,
  sql`select 1 / 0 as v`,
  sql`select 3 as v`,
  sql`select * from no_such_table`,
  sql`select 5 as v`,
]);
console.log(
  "mixed:",
  mixed.map((r) =>
    r.status === "fulfilled"
      ? r.value[0].v
      : (r.reason as Deno.PostgresError).code
  ),
);

// A transaction gets a connection of its own, even while queries flow.
const [txResult] = await Promise.all([
  sql.begin(async (tx) => {
    await tx`create temp table pipe (n int) on commit drop`;
    await Promise.all([1, 2, 3].map((n) => tx`insert into pipe values (${n})`));
    const pids = new Set();
    for (let i = 0; i < 5; i++) {
      pids.add((await tx`select pg_backend_pid() as pid`)[0].pid);
    }
    return [(await tx`select count(*)::int as n from pipe`)[0].n, pids.size];
  }),
  ...Array.from({ length: 50 }, (_, i) => sql`select ${i} as i`),
]);
console.log("transaction:", txResult);

// Prepared statements are shared by the pipelined queries.
const same = await Promise.all(
  Array.from({ length: 50 }, (_, i) => sql`select ${i}::int + 1 as n`),
);
console.log("prepared:", same.reduce((s, r) => s + r[0].n, 0));

// prepare: false pipelines unnamed statements.
const unprepared = new Deno.SQL(url, { max: 1, prepare: false });
const un = await Promise.all(
  Array.from({ length: 30 }, (_, i) => unprepared`select ${i}::int as i`),
);
console.log("unprepared:", un.every((r, i) => r[0].i === i));
await unprepared.close();

// onconnect / onclose get the error (or null) and the connection.
const events: string[] = [];
const hooked = new Deno.SQL(url, {
  max: 1,
  onconnect: (error, conn) =>
    events.push(
      `connect ${error} ${conn?.id} ${typeof conn?.parameters.server_version}`,
    ),
  onclose: (error, conn) => events.push(`close ${error} ${conn.id}`),
});
await hooked`select 1`;
await hooked.close();
const failing = new Deno.SQL("postgres://u:p@127.0.0.1:1/x?sslmode=disable", {
  onconnect: (error) =>
    events.push(`failed ${(error as Deno.PostgresError).code}`),
});
await failing`select 1`.catch(() => {});
await failing.close();
console.log("hooks:", events);
await sql.close();
