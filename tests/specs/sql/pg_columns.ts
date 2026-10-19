// Deno.SQL over Postgres: result column metadata (`result.columns`) and the
// type round-trip matrix (each type, sent as a parameter and read back).
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1 });
// prepare: false runs unnamed statements, whose columns come from the
// stream rather than a cached statement.
const unnamed = new Deno.SQL(url, { max: 1, prepare: false });

await sql`drop schema if exists denosql_cols cascade`;
await sql`create schema denosql_cols`;
await sql`create type denosql_cols.mood as enum ('sad', 'happy')`;
await sql`create domain denosql_cols.posint as int check (value > 0)`;
await sql`create table denosql_cols.pets (
  id int8 generated always as identity primary key,
  name varchar(40) not null, price numeric(10, 2), mood denosql_cols.mood,
  tags text[])`;

const [{ table_oid }] =
  await sql`select 'denosql_cols.pets'::regclass::oid::int8 as table_oid`;
function show(label: string, result: Deno.SQLResult<unknown>) {
  const columns = (result.columns as Deno.SQLPostgresColumn[]).map((c) => ({
    ...c,
    // The table's OID differs per run.
    tableOid: c.tableOid === null
      ? null
      : String(c.tableOid) === table_oid
      ? "pets"
      : c.tableOid,
    // So do a user type's.
    typeOid: c.typeSchema === "denosql_cols" ? "user" : c.typeOid,
    baseTypeOid: c.baseTypeOid !== null && c.baseTypeOid >= 16384
      ? "user"
      : c.baseTypeOid,
  }));
  console.log(label, result.length, Object.keys(result));
  for (const c of columns) console.log(" ", JSON.stringify(c));
}

for (const [name, client] of [["prepared", sql], ["unnamed", unnamed]]) {
  // No rows: the columns are still described (for unnamed statements too).
  show(
    `${name} empty`,
    await client`select * from denosql_cols.pets where id < 0`,
  );
  show(
    `${name} expressions`,
    await client`select 1::int4 as a, 'x'::text as a, null as n,
      array[1]::int8[] as arr, 'sad'::denosql_cols.mood as m,
      2::denosql_cols.posint as d, int4range(1, 3) as r,
      '{[1,2)}'::int4multirange as mr, 1.5::numeric(5, 2) as num`,
  );
  show(
    `${name} insert`,
    await client`insert into denosql_cols.pets (name) values ('Rex')`,
  );
  show(
    `${name} returning`,
    await client`insert into denosql_cols.pets (name, price)
      values ('Tom', 9.5) returning id, name, price`,
  );
}

// .values() and .raw() rows line up with the columns, duplicates included.
const values = await sql`select 1 as x, 2 as x`.values();
console.log(values, values.columns.map((c) => c.name));
const raw = await sql`select 1::int4 as a`.raw();
console.log(raw.columns.map((c) => [c.name, c.type]));
// Cursor batches carry them.
for await (const batch of sql`select id from denosql_cols.pets`.cursor(1)) {
  console.log("batch", batch.length, batch.columns.map((c) => c.type));
}
// .simple() knows only the names.
const simple = await sql`select 1 as a, 'b' as b`.simple();
console.log(JSON.stringify(simple.columns));
// Not enumerable: rows print as before.
const rows = await sql`select name from denosql_cols.pets order by id`;
console.log(rows, Object.keys(rows), "columns" in rows);

// The type round trip: each value sent as a parameter of that type and read
// back, with its column's type and the JS type it arrives as. `bigint: true`
// changes int8 only.
function describe(value: unknown): string {
  if (value === null) return "null";
  if (value instanceof Date) return `Date ${value.toISOString()}`;
  if (value instanceof Uint8Array) return `Uint8Array[${value.join(",")}]`;
  if (Array.isArray(value)) return `[${value.map(describe).join(", ")}]`;
  if (typeof value === "bigint") return `bigint ${value}n`;
  return `${typeof value} ${JSON.stringify(value)}`;
}
const matrix: [string, unknown][] = [
  ["int2", 7],
  ["int4", -2147483648],
  ["int8", "9007199254740993"],
  ["int8", 9007199254740993n],
  ["float4", 1.5],
  ["float8", 0.1],
  ["numeric", "123456789012345678901234567890.123"],
  ["numeric", "NaN"],
  ["bool", false],
  ["text", "héllo ✓ 𝒳"],
  ["varchar", "v"],
  ["json", { a: [1, null, "x"] }],
  ["jsonb", { b: { c: true } }],
  // A string is sent as text, so a JSON string value is its JSON text.
  ["jsonb", JSON.stringify("a string")],
  ["bytea", new Uint8Array([0, 1, 254, 255])],
  ["timestamptz", new Date("2024-02-29T12:34:56.789Z")],
  ["timestamp", new Date("2024-02-29T12:34:56.789Z")],
  ["date", "2024-02-29"],
  ["date", new Date("2024-02-29T00:00:00Z")],
  ["uuid", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"],
  ["int4[]", [1, null, 3]],
  ["int8[]", ["1", "9007199254740993"]],
  ["text[]", ["a", "b c", null, ""]],
  ["jsonb[]", [{ x: 1 }, [2]]],
  ["date[]", ["2024-01-01"]],
  ["uuid[]", ["a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"]],
  ["bool[]", [[true, false], [false, true]]],
  ["denosql_cols.mood", "happy"],
  ["denosql_cols.mood[]", ["sad", "happy"]],
  ["denosql_cols.posint", 5],
];
const bigints = new Deno.SQL(url, { max: 1, bigint: true });
for (const [type, value] of matrix) {
  const text = `select $1::${type} as v`;
  const [row] = await sql.unsafe(text, [value]);
  // An unnamed statement's parameters are typed by the server after they
  // are encoded, so a json[] element that is an array reads as a dimension.
  const unnamedRow = await unnamed.unsafe(text, [value]).then(
    ([r]) => r,
    (e) => ({ v: `error ${e.code}` }),
  );
  const result = await bigints.unsafe(text, [value]);
  const column = result.columns[0] as Deno.SQLPostgresColumn;
  const got = describe(row.v);
  console.log(
    `${type} (${column.type}, ${column.typeKind}${
      column.baseType ? ` of ${column.baseType}` : ""
    }): ${describe(value)} -> ${got}${
      describe(unnamedRow.v) === got
        ? ""
        : ` (unnamed: ${describe(unnamedRow.v)})`
    }${
      describe(result[0].v) === got ? "" : ` (bigint: ${describe(result[0].v)})`
    }`,
  );
}
// The values that come back are accepted as parameters again: a second
// round trip leaves them unchanged. The exception is a top-level JSON string:
// it comes back as a JavaScript string, and a string bound to json / jsonb
// is taken as JSON text, so it has to be sent as JSON.stringify(value).
for (const [type, value] of matrix) {
  const text = `select $1::${type} as v`;
  const [first] = await sql.unsafe(text, [value]);
  const second = await sql.unsafe(text, [first.v]).then(
    ([r]) => describe(r.v),
    (e) => `error ${e.code}`,
  );
  if (describe(first.v) !== second) {
    console.log(`${type}: ${describe(first.v)} -> ${second}`);
  }
}
console.log("second round trip done");

await bigints.close();
await unnamed.close();
await sql`drop schema denosql_cols cascade`;
await sql.close();
