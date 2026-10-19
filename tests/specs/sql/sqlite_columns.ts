// Deno.SQL over SQLite: result column metadata (`result.columns`) and the
// type round-trip matrix (each declared type, every kind of JS value).
const sql = new Deno.SQL(":memory:");

function show(label: string, result: Deno.SQLResult<unknown>) {
  console.log(label, Object.keys(result), JSON.stringify(result.columns));
}

await sql`create table owners (id integer primary key, name text not null)`;
await sql`create table pets (
  id integer primary key, owner integer references owners (id),
  name varchar(40) not null, weight real, photo blob, born date,
  meta json, untyped)`;
await sql`create view pet_names as select p.name as pet, o.name as owner
  from pets p join owners o on o.id = p.owner`;

// No rows: the columns are still described.
show("empty", await sql`select * from pets`);
// Aliases, an expression, a join, a subquery and a view.
await sql`insert into owners (name) values ('Ada')`;
await sql`insert into pets (owner, name) values (1, 'Rex')`;
show(
  "join",
  await sql`select p.id, p.name as pet_name, o.name, upper(o.name) as up,
    1 + 1 as two, (select max(weight) from pets) as heaviest
    from pets p join owners o on o.id = p.owner`,
);
show("view", await sql`select * from pet_names`);
show(
  "subquery",
  await sql`select x from (select name as x from owners)`,
);
// Statements without a result set have none; RETURNING has its columns.
show("insert", await sql`insert into owners (name) values ('Bob')`);
show(
  "returning",
  await sql`insert into owners (name) values ('Eve') returning id, name`,
);
// .values() / .raw(): columns line up with the cells, duplicates included.
const values = await sql`select id, name, id from owners limit 1`.values();
console.log(values, values.columns.map((c) => c.name));
const raw = await sql`select name from owners limit 1`.raw();
console.log(raw.columns.length, raw.columns[0].name);
// A cursor's batches carry them too.
for await (const batch of sql`select id from owners`.cursor(2)) {
  console.log("batch", batch.length, batch.columns[0].name);
}
// simple(): no result set, so no columns.
const simple = await sql`create table t2 (a int)`.simple();
console.log("simple", simple.columns);
// The property is not enumerable, so rows print as before.
const rows = await sql`select name from owners order by id`;
console.log(rows, Object.keys(rows), "columns" in rows);

// Type round trip: each value bound into a column of each declared type and
// read back, with the JS type it comes back as.
await sql`create table matrix (
  i integer, r real, t text, b blob, n numeric, bo boolean, d datetime,
  j json, u)`;
const inputs: [string, unknown][] = [
  ["number", 42],
  ["fraction", 1.5],
  ["max safe", Number.MAX_SAFE_INTEGER],
  ["bigint", 9007199254740993n],
  ["string", "héllo ✓"],
  ["numeric string", "12.50"],
  ["boolean", true],
  ["bytes", new Uint8Array([0, 1, 255])],
  ["date", new Date("2024-02-29T12:34:56.789Z")],
  ["object", { a: [1, "x"] }],
  ["array", [1, 2]],
  ["null", null],
];
function describe(value: unknown): string {
  if (value === null) return "null";
  if (value instanceof Uint8Array) return `Uint8Array[${value.join(",")}]`;
  return `${typeof value} ${
    typeof value === "bigint" ? `${value}n` : JSON.stringify(value)
  }`;
}
for (const column of ["i", "r", "t", "b", "n", "bo", "d", "j", "u"]) {
  const cells: string[] = [];
  for (const [, value] of inputs) {
    await sql`delete from matrix`;
    await sql`insert into matrix (${sql(column)}) values (${value})`;
    const result = await sql`select ${sql(column)} from matrix`;
    cells.push(describe((result[0] as Record<string, unknown>)[column]));
  }
  const declared = (await sql`select ${sql(column)} from matrix`).columns[0];
  console.log(`${column} (${declared.type}):`);
  inputs.forEach(([label], i) => console.log(`  ${label}: ${cells[i]}`));
}
// bigint: true reads every integer as a bigint.
const big = new Deno.SQL(":memory:", { bigint: true });
const [b] = await big`select 1 as one, 9007199254740993 as huge, 1.5 as f`;
console.log(b);
await big.close();
await sql.close();
