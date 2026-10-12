// begin() / savepoint() pipelining: BEGIN goes out with the first
// statements, and COMMIT right behind queries the callback returns without
// awaiting. The outcomes must be the ones of the unpipelined protocol.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const notices: string[] = [];
const sql = new Deno.SQL(url, {
  max: 2,
  onnotice: (n) => notices.push(n.message),
});
await sql`drop table if exists denosql_pipe`;
await sql`create table denosql_pipe (id serial primary key, v text)`;
const values = async () =>
  (await sql`select v from denosql_pipe order by id`.values()).map((r) => r[0]);
const code = (e: unknown) =>
  (e as Deno.PostgresError).code ?? (e as Error).message;

// Returned queries (an array) are all awaited, in order, and committed.
const results = await sql.begin((tx) => [
  tx`insert into denosql_pipe (v) values (${"a"})`,
  tx`insert into denosql_pipe (v) values (${"b"})`,
  tx`select count(*)::int as n from denosql_pipe`,
]);
console.log("array:", results.map((r) => r.count), results[2][0].n);

// A returned statement that fails on the server rolls everything back.
try {
  await sql.begin((tx) => [
    tx`insert into denosql_pipe (v) values (${"rolled back 1"})`,
    tx`select 1/0`,
  ]);
} catch (e) {
  console.log("server failure:", code(e), await values());
}

// One that fails before it is sent (client-side) rolls back too.
try {
  await sql.begin((tx) => [
    tx`insert into denosql_pipe (v) values (${"rolled back 2"})`,
    tx`select ${"nul \0 here"}::text`,
  ]);
} catch (e) {
  console.log("client failure (NUL):", code(e), await values());
}
try {
  await sql.begin((tx) => [
    tx`insert into denosql_pipe (v) values (${"rolled back 3"})`,
    tx.unsafe("select $1::int + $2::int as n", [1]),
  ]);
} catch (e) {
  console.log("client failure (parameters):", code(e), await values());
}
try {
  await sql.begin((tx) => [
    tx`insert into denosql_pipe (v) values (${"rolled back 4"})`,
    tx`select ${1}, ${tx.unsafe("select $1", [2])}`,
  ]);
} catch (e) {
  console.log(
    "client failure (builder):",
    (e as Error).message,
    await values(),
  );
}

// Statements go on the wire in call order even when the first one has to be
// prepared and the second is already cached.
await sql.begin((tx) => tx`select current_setting('app.order', true) as v`);
const [, ordered] = await sql.begin((tx) => [
  tx.unsafe(
    `select set_config('app.order', $1, true) as fresh_${Date.now()}`,
    ["first"],
  ),
  tx`select current_setting('app.order', true) as v`,
]);
console.log("order:", ordered[0].v);

// The RLS shape: setup + query started together inside an async callback.
const rls = await sql.begin(async (tx) => {
  const setup = tx`select set_config('app.role', ${"anon"}, true)`.execute();
  const work = tx`select current_setting('app.role', true) as role`.execute();
  return (await Promise.all([setup, work]))[1];
});
console.log("rls:", rls[0].role);

// BEGIN options (pipelined when they are transaction modes).
console.log(
  "modes:",
  await sql.begin(
    "isolation level repeatable read, read only",
    (tx) =>
      tx`select current_setting('transaction_isolation') as i,
        current_setting('transaction_read_only') as r`,
  ),
  await sql.begin(
    "ISOLATION LEVEL SERIALIZABLE READ WRITE NOT DEFERRABLE",
    async (tx) =>
      await tx`select current_setting('transaction_isolation') as i,
        current_setting('transaction_read_only') as r,
        current_setting('transaction_deferrable') as d`,
  ),
);
try {
  await sql.begin(
    "read only",
    (tx) => tx`insert into denosql_pipe (v) values (${"read only"})`,
  );
} catch (e) {
  console.log("read only:", code(e), await values());
}
// Other options keep the awaited BEGIN: a bad one fails before the callback.
let called = false;
try {
  await sql.begin("isolation level whatever", (tx) => {
    called = true;
    return tx`insert into denosql_pipe (v) values (${"never"})`;
  });
} catch (e) {
  console.log("bad options:", code(e), "callback called:", called);
}

// A pipelined BEGIN that fails (the reserved connection's own transaction is
// aborted): the statements behind it report BEGIN's error, and so does
// begin(), even when the callback swallows it.
const reserved = await sql.reserve();
await reserved`begin`;
await reserved`select 1/0`.catch(() => {});
let seen = "";
try {
  await reserved.begin(async (tx) => {
    try {
      await tx`select 1 as x`;
    } catch (e) {
      seen = code(e);
    }
    return "swallowed";
  });
} catch (e) {
  console.log("begin failure:", code(e), "statement saw:", seen);
}
try {
  await reserved.begin((tx) => [tx`select 1 as x`]);
} catch (e) {
  console.log("begin failure (returned):", code(e));
}
await reserved`rollback`;
console.log("reserved after:", (await reserved`select 1 as x`)[0].x);
reserved.release();

// Savepoints: pipelined the same way; a failure rolls back to the savepoint
// and the transaction goes on.
const outcome = await sql.begin(async (tx) => {
  await tx`insert into denosql_pipe (v) values (${"outer"})`;
  const inner = await tx.savepoint((sp) => [
    sp`insert into denosql_pipe (v) values (${"inner kept"})`,
    sp`select 2 as x`,
  ]);
  const errors: string[] = [];
  try {
    await tx.savepoint((sp) => [
      sp`insert into denosql_pipe (v) values (${"inner dropped 1"})`,
      sp`select 1/0`,
    ]);
  } catch (e) {
    errors.push(code(e));
  }
  try {
    await tx.savepoint((sp) => [
      sp`insert into denosql_pipe (v) values (${"inner dropped 2"})`,
      sp`select ${"\0"}::text`,
    ]);
  } catch (e) {
    errors.push(code(e));
  }
  try {
    await tx.savepoint("deep", async (sp) => {
      await sp`insert into denosql_pipe (v) values (${"deep kept"})`;
      await sp.savepoint((sp2) => [
        sp2`insert into denosql_pipe (v) values (${"deeper dropped"})`,
        sp2`select 1/0`,
      ]);
    });
  } catch (e) {
    errors.push(code(e));
  }
  await tx.savepoint("again", async (sp) => {
    await sp`insert into denosql_pipe (v) values (${"again kept"})`;
  });
  return [inner[1][0].x, errors];
});
console.log("savepoints:", outcome, await values());

// A callback that throws synchronously rolls back what it started.
try {
  await sql.begin((tx) => {
    tx`insert into denosql_pipe (v) values (${"thrown"})`.execute();
    throw new Error("sync throw");
  });
} catch (e) {
  console.log("sync throw:", (e as Error).message, await values());
}

// Nothing leaked: a BEGIN on a connection still in a transaction would
// draw a "there is already a transaction in progress" warning.
notices.length = 0;
await Promise.all([1, 2].map(() => sql.begin((tx) => tx`select 1`)));
console.log("after:", notices.length === 0 ? "no warnings" : notices);

await sql`drop table denosql_pipe`;
await sql.close();
