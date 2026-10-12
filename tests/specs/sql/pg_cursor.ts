// Cursors: batches from a server-side cursor, row-by-row iteration, early
// exit closing the cursor, backpressure, errors, and inside transactions and
// reserved connections.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 2 });

const batches = [];
for await (const rows of sql`select n from generate_series(1, 7) n`.cursor(3)) {
  batches.push(rows.map((r) => r.n));
}
console.log("batches:", batches);

const one = [];
for await (
  const rows of sql`select ${"a"}::text as v union all select 'b'`.cursor()
) {
  one.push(rows.length + ":" + rows[0].v);
}
console.log("default size 1:", one);

const values = [];
for await (
  const rows of sql`select n, n * 2 from generate_series(1, 3) n`.values()
    .cursor(2)
) {
  values.push(rows);
}
console.log("values:", JSON.stringify(values));

let sum = 0;
let count = 0;
for await (const row of sql`select n from generate_series(1, 1000) n`) {
  sum += row.n;
  count++;
}
console.log("rows:", count, sum);

// Backpressure: only the batches asked for are fetched, so a cursor over a
// billion rows ends as soon as the loop does.
let taken = 0;
const started = Date.now();
// (A set-returning function in the select list streams; in FROM it would be
// materialized first.)
for await (
  const rows of sql`select generate_series(1, 1000000000) as n`.cursor(10)
) {
  taken += rows.length;
  if (taken >= 30) break;
}
let rowsTaken = 0;
for await (const _row of sql`select generate_series(1, 1000000000) as n`) {
  if (++rowsTaken === 250) break;
}
console.log("early exit:", taken, rowsTaken, Date.now() - started < 5000);

// The pool is fine afterwards (the cursor's connection was returned).
console.log(
  "pool after:",
  (await Promise.all([1, 2, 3].map((i) => sql`select ${i}::int as i`)))
    .map((r) => r[0].i),
);

// An error while fetching rejects the loop; the connection is still usable.
try {
  for await (
    const _rows of sql`select 1 / (5 - n) from generate_series(1, 10) n`.cursor(
      2,
    )
  ) {
    // keep going
  }
} catch (e) {
  console.log("error:", (e as Deno.PostgresError).code);
}
console.log("after error:", (await sql`select 1 as ok`)[0].ok);

// Inside a transaction: the cursor lives in it and is closed on early exit.
await sql.begin(async (tx) => {
  await tx`create temp table cur (n int) on commit drop`;
  await tx`insert into cur select generate_series(1, 5)`;
  const seen = [];
  for await (const rows of tx`select n from cur order by n`.cursor(2)) {
    seen.push(...rows.map((r) => r.n));
    if (seen.length >= 4) break;
  }
  const [{ open }] = await tx`select count(*)::int as open from pg_cursors
    where name like 'deno_sql_cursor_%'`;
  console.log("in transaction:", seen, "open cursors:", open);
});

// A reserved connection outside a transaction: one is opened for the
// cursor and committed after it.
{
  const reserved = await sql.reserve();
  const seen = [];
  for await (
    const rows of reserved`select n from generate_series(1, 4) n`.cursor(3)
  ) {
    seen.push(rows.length);
  }
  const [{ open }] = await reserved`select count(*)::int as open from pg_cursors
    where name like 'deno_sql_cursor_%'`;
  // Not left in a transaction: SAVEPOINT needs one.
  const status = await reserved`savepoint s`.then(
    () => "in a transaction",
    (e) => e.code === "25P01" ? "no transaction" : e.code,
  );
  console.log("reserved:", seen, "open cursors:", open, status);
  reserved.release();
}

// A started query can't become a cursor, and a cursor can't be awaited.
const started2 = sql`select 1`;
await started2;
try {
  for await (const _ of started2.cursor(1)) { /* */ }
} catch (e) {
  console.log("started:", (e as Error).message);
}
const consumed = sql`select 1 as x`;
for await (const _ of consumed.cursor(1)) { /* */ }
try {
  await consumed;
} catch (e) {
  console.log("consumed:", (e as Error).message);
}
try {
  for await (const _ of sql`select 1`.cursor(0)) { /* */ }
} catch (e) {
  console.log("size 0:", (e as Error).name);
}
await sql.close();
