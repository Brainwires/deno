// Transactions and savepoints on Postgres.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 3 });
await sql`drop table if exists denosql_tx`;
await sql`create table denosql_tx (id serial primary key, v text)`;
const count = async () =>
  (await sql`select count(*)::int as n from denosql_tx`)[0].n;

try {
  await sql.begin(async (tx) => {
    await tx`insert into denosql_tx (v) values (${"one"})`;
    console.log(
      "inside:",
      (await tx`select count(*)::int as n from denosql_tx`)[0].n,
    );
    throw new Error("boom");
  });
} catch (e) {
  console.log("rolled back:", (e as Error).message, await count());
}

// A failed statement aborts the transaction; begin() rolls it back.
try {
  await sql.begin(async (tx) => {
    await tx`insert into denosql_tx (v) values (${"two"})`;
    await tx`insert into denosql_tx (id, v) values (${"not a number"}, 'x')`;
  });
} catch (e) {
  console.log(
    "statement error:",
    (e as Deno.PostgresError).code,
    await count(),
  );
}

const result = await sql.begin(async (tx) => {
  await tx`insert into denosql_tx (v) values (${"kept"})`;
  try {
    await tx.savepoint(async (sp) => {
      await sp`insert into denosql_tx (v) values (${"dropped"})`;
      throw new Error("inner");
    });
  } catch (e) {
    console.log("savepoint:", (e as Error).message);
  }
  await tx.savepoint("named", async (sp) => {
    await sp`insert into denosql_tx (v) values (${"nested ok"})`;
  });
  // Queries returned as an array run and resolve together.
  return [tx`select 1 as a`, tx`select 2 as b`];
});
console.log(
  "committed:",
  result,
  await sql`select v from denosql_tx order by id`.values(),
);

// BEGIN options.
try {
  await sql.begin("read only", async (tx) => {
    await tx`insert into denosql_tx (v) values ('ro')`;
  });
} catch (e) {
  console.log("read only:", (e as Deno.PostgresError).code);
}
console.log(
  await sql.begin(
    "isolation level serializable",
    (tx) => tx`show transaction_isolation`,
  ),
);

// Concurrent queries inside one transaction share its connection.
const pids = await sql.begin((tx) =>
  Promise.all([1, 2, 3, 4].map(() => tx`select pg_backend_pid() as pid`))
);
console.log("one connection:", new Set(pids.map((r) => r[0].pid)).size === 1);

// begin() inside a transaction is refused; savepoint() outside one too.
await sql.begin(async (tx) => {
  try {
    await tx.begin(() => {});
  } catch (e) {
    console.log((e as Error).message);
  }
});
try {
  await sql.savepoint(() => {});
} catch (e) {
  console.log((e as Error).message);
}

// reserve(): one connection until release().
const reserved = await sql.reserve();
const [a] = await reserved`select pg_backend_pid() as pid`;
const [b] = await reserved`select pg_backend_pid() as pid`;
console.log("reserved same connection:", a.pid === b.pid);
reserved.release();
try {
  await reserved`select 1`;
} catch (e) {
  console.log((e as Error).message);
}

await sql`drop table denosql_tx`;
await sql.close();
