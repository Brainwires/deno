// Deno.SQL over SQLite: helpers, parameters, transactions, errors.
const sql = new Deno.SQL("sqlite://test.db");

await sql`create table users (
  id integer primary key, name text not null unique, age integer,
  data blob, meta text)`;

const inserted = await sql`insert into users ${sql({ name: "Ada", age: 36 })}`;
console.log(
  "insert:",
  inserted.count,
  inserted.command,
  inserted.lastInsertRowid,
);

await sql`insert into users ${
  sql([{ name: "Bob", age: 40 }, { name: "Eve", age: null }])
}`;
console.log(
  await sql`select id, name, age from users where name in ${
    sql(["Ada", "Eve"])
  } order by id`,
);

// Values are parameters, never text: quotes, backslashes, placeholders and
// unicode come back byte for byte.
const nasty = [
  "x'); drop table users; --",
  "\\'; select 1; --",
  'a"b',
  "$1 ?1 :name @p",
  "tab\tnew\nline",
  "unicode ✓ 𝒳 é",
];
for (const name of nasty) await sql`insert into users (name) values (${name})`;
for (const name of nasty) {
  const [row] = await sql`select name from users where name = ${name}`;
  console.log("roundtrip:", row.name === name);
}
console.log("users:", (await sql`select count(*) as n from users`)[0].n);

await sql`update users set ${
  sql({ age: 37, meta: { tags: ["x"] } })
} where name = ${"Ada"}`;
console.log(
  await sql`select name, age, meta from users where name = ${"Ada"}`.values(),
);

await sql`update users set data = ${new Uint8Array([
  0,
  1,
  255,
])} where name = ${"Bob"}`;
console.log((await sql`select data from users where name = ${"Bob"}`)[0].data);

const big = new Deno.SQL("sqlite://test.db", { bigint: true });
console.log(
  (await big`select 9007199254740993 as n, age from users where name = 'Bob'`)[
    0
  ],
);
await big.close();
console.log((await sql`select 9007199254740993 as n`)[0]);

// A throw inside begin() rolls back.
try {
  await sql.begin(async (tx) => {
    await tx`delete from users`;
    throw new Error("boom");
  });
} catch (e) {
  console.log("rolled back:", (e as Error).message);
}
console.log(
  "users after rollback:",
  (await sql`select count(*) as n from users`)[0].n,
);

// Savepoints: the inner failure is undone, the transaction commits.
const result = await sql.begin(async (tx) => {
  await tx`insert into users (name) values (${"Tx"})`;
  try {
    await tx.savepoint(async (sp) => {
      await sp`insert into users (name) values (${"Sp"})`;
      throw new Error("inner");
    });
  } catch (e) {
    console.log("savepoint rolled back:", (e as Error).message);
  }
  return "committed";
});
console.log(
  result,
  await sql`select name from users where name in ${sql(["Tx", "Sp"])}`.values(),
);

// A finished transaction refuses more queries.
let leaked: Deno.SQL | undefined;
await sql.begin((tx) => {
  leaked = tx;
});
try {
  await leaked!`select 1`;
} catch (e) {
  console.log("after commit:", (e as Error).message);
}

try {
  await sql`insert into users (name) values (${"Ada"})`;
} catch (e) {
  const err = e as Deno.SQLiteError;
  console.log(
    err instanceof Deno.SQL.SQLiteError,
    err instanceof Deno.SQL.SQLError,
    err.name,
    err.code,
    err.errno,
  );
}
try {
  await sql`selec 1`;
} catch (e) {
  console.log((e as Deno.SQLiteError).code);
}

await sql.unsafe(
  "create table t2 (a); insert into t2 values (1); insert into t2 values (2)",
).simple();
console.log(await sql.unsafe("select sum(a) as s from t2 where a > ?1", [0]));
const filter = sql`where a = ${2}`;
console.log(await sql`select a from t2 ${filter}`);
console.log(
  await sql`select ${sql("a")} from ${sql("t2")} order by a`.values(),
);
// A string is an identifier wherever it appears, even right after INSERT;
// dots separate schema and table.
await sql`insert into ${sql("main.t2")} ${sql({ a: 3 })}`;
await sql`create table ${sql('we"ird name')} (${sql("a b")})`;
await sql`insert into ${sql('we"ird name')} ${
  sql([{ "a b": 1 }, { "a b": 2 }])
}`;
console.log(
  await sql`select ${sql("a b")} from ${sql('we"ird name')}`.values(),
  await sql`select count(*) as n from t2`,
);
console.log(await sql`select * from t2 where a in ${sql([])}`);

const pending = sql`select 1 as one`;
pending.cancel();
try {
  await pending;
} catch (e) {
  console.log("cancelled:", (e as Deno.SQLError).code);
}

await sql.close();
try {
  await sql`select 1`;
} catch (e) {
  console.log("closed:", (e as Deno.SQLError).code);
}

const memory = new Deno.SQL(":memory:");
console.log(
  await memory`select ${1} + ${2} as sum, ${"text"} as t, ${null} as n, ${true} as b`,
);
await memory.close();
