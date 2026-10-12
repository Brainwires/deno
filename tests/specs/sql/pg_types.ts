// Postgres type mapping, both directions.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1 });

await sql`drop schema if exists denosql_types cascade`;
await sql`create schema denosql_types`;
await sql`create type denosql_types.mood as enum ('sad', 'happy')`;
await sql`create domain denosql_types.posint as int check (value > 0)`;
await sql`create type denosql_types.pair as (a int, b text)`;

const [row] = await sql`select
  1::int2 as i2, 2::int4 as i4, 9007199254740993::int8 as i8,
  1.5::float4 as f4, 0.1::float8 as f8, 'Infinity'::float8 as finf,
  12345.678900::numeric as num, -0.000123::numeric as small, 'NaN'::numeric as nan,
  true as t, false as f, null::text as n,
  'héllo ✓ 𝒳'::text as txt, 'x'::varchar as vc, 'ab'::char(4) as ch, 'nm'::name as nm,
  '{"a":[1,2,{"b":null}],"s":"q\\"uote"}'::json as js, '{"k": "v", "n": 1.5}'::jsonb as jsb,
  '\\xdeadbeef'::bytea as by, ''::bytea as empty_by,
  '2024-02-29 12:34:56.789+00'::timestamptz as tstz,
  '2024-02-29 12:34:56.789'::timestamp as ts, '1969-07-20'::date as d,
  'infinity'::timestamptz as inf, '-infinity'::date as ninf,
  '12:34:56.5'::time as tm, '12:34:56+05:30'::timetz as tmtz,
  '1 year 2 mons 3 days 04:05:06.5'::interval as iv, '-1 day +02:00'::interval as iv2,
  'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid as id,
  '10.0.0.1/24'::inet as ip, '::1'::inet as ip6, '10.0.0.0/8'::cidr as net,
  '08:00:2b:01:02:03'::macaddr as mac, B'10101'::bit(5) as bits,
  '1.5'::money as money, '0/16B3748'::pg_lsn as lsn, point(1.5, -2) as pt,
  'happy'::denosql_types.mood as mood, 5::denosql_types.posint as dom,
  row(1, 'x')::denosql_types.pair as pair,
  array[1, null, 3]::int4[] as arr, array[['a', 'b'], ['c', null]]::text[] as arr2,
  '{}'::int4[] as empty_arr, array['2024-01-01'::date] as darr,
  array['{"x":1}'::jsonb] as jarr, array['happy', 'sad']::denosql_types.mood[] as moods,
  '[1,10)'::int4range as rng`;
console.log(row);

// bigint: true
const big = new Deno.SQL(url, { max: 1, bigint: true });
console.log(
  await big`select 9007199254740993::int8 as i8, array[1, 2]::int8[] as a, 2::int4 as i4`,
);
await big.close();

// Parameters of every kind into typed columns, read back.
await sql`create table denosql_types.t (
  i int4, b bigint, n numeric, f float8, bo bool, t text, j jsonb, js json,
  by bytea, ts timestamptz, d date, u uuid, ia int4[], ta text[], ja jsonb,
  m denosql_types.mood, iv interval)`;
const values = {
  i: 42,
  b: 9007199254740993n,
  n: "123456789012345678901234567890.123",
  f: 1.25,
  bo: true,
  t: 'it\'s a "test" \\ with $1 and ?1 ✓',
  j: { nested: { a: [1, "two", null] }, quote: "'\"" },
  js: [1, { two: 2 }],
  by: new Uint8Array([0, 1, 2, 254, 255]),
  ts: new Date("2001-02-03T04:05:06.789Z"),
  d: "2020-12-31",
  u: "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
  ia: [1, 2, null, 4],
  ta: ['a "quoted" item', "back\\slash", "comma,brace{}", "NULL", null, ""],
  ja: ["x", 1],
  m: "sad",
  iv: "1 day 02:00:00",
};
await sql`insert into denosql_types.t ${sql(values)}`;
const [back] = await sql`select * from denosql_types.t`;
function equal(a: unknown, b: unknown): boolean {
  if (a instanceof Date && b instanceof Date) {
    return a.getTime() === b.getTime();
  }
  if (a instanceof Uint8Array && b instanceof Uint8Array) {
    return a.length === b.length && a.every((x, i) => x === b[i]);
  }
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((x, i) => equal(x, b[i]));
  }
  if (a && b && typeof a === "object" && typeof b === "object") {
    const ka = Object.keys(a).sort();
    const kb = Object.keys(b).sort();
    return equal(ka, kb) &&
      ka.every((k) =>
        equal(
          (a as Record<string, unknown>)[k],
          (b as Record<string, unknown>)[k],
        )
      );
  }
  return a === b;
}
const expected: Record<string, unknown> = {
  ...values,
  b: "9007199254740993",
  d: new Date("2020-12-31T00:00:00Z"),
};
for (const key of Object.keys(values)) {
  const got = back[key];
  console.log(
    key,
    equal(got, expected[key])
      ? "ok"
      : `MISMATCH ${Deno.inspect(got)} != ${Deno.inspect(expected[key])}`,
  );
}

// nulls and undefined
await sql`insert into denosql_types.t ${sql({ i: null, t: undefined })}`;
console.log(await sql`select i, t, j from denosql_types.t where i is null`);

// .values() and .raw()
console.log(await sql`select 1 as a, 'x' as b`.values());
console.log(
  await sql`select 1::int4 as a, 'x'::text as b, null::text as c`.raw(),
);

// Duplicate column names: the last one wins, as in assignment order.
console.log(await sql`select 1 as x, 2 as x`);
console.log(await sql`select 1 as x, 2 as x`.values());

// count / command
const ins =
  await sql`insert into denosql_types.t (i) values (1), (2), (3) returning i`;
console.log(ins.command, ins.count, ins.length);
const upd = await sql`update denosql_types.t set i = i + 1 where i in ${
  sql([1, 2])
}`;
console.log(upd.command, upd.count, upd.length);
const sel = await sql`with x as (select 1) select * from denosql_types.t`;
console.log(sel.command, sel.count === sel.length);
const del =
  await sql`/* comment */ delete from denosql_types.t where i = ${99}`;
console.log(del.command, del.count);

// Unnamed statements (prepare: false) take the same parameters.
const unnamed = new Deno.SQL(url, { max: 1, prepare: false });
console.log(
  await unnamed`select ${1}::int + ${2}::int as sum, ${new Uint8Array([
    7,
  ])}::bytea as by, ${{ a: 1 }}::jsonb as j, ${[1, 2]}::int[] as a`,
);
await unnamed.close();

// .simple(): several statements, text values.
console.log(await sql`select 1 as a; select 'two' as b, null as c`.simple());

// unsafe()
console.log(await sql.unsafe("select $1::text || $2::text as s", ["a'", "b"]));

// Ranges and multiranges as { lower, upper, lowerInclusive, upperInclusive,
// empty } (null for an unbounded side).
console.log(
  await sql`select int4range(1, 10) as r, '(,2020-01-02]'::daterange as d,
    'empty'::numrange as e, '{[1,3), [5,6)}'::int4multirange as m`,
);
// Geometry as points ({ x, y }) and shapes made of them; tsvector as the
// server's text.
console.log(
  await sql`select point(1, 2) as p, line(point(0, 0), point(1, 1)) as l,
    lseg(point(0, 0), point(1, 1)) as s, box(point(0, 0), point(2, 2)) as b,
    path(polygon '((0,0),(1,0),(1,1))') as pa, '[(0,0),(1,1)]'::path as op,
    polygon '((0,0),(1,0),(1,1))' as pg, circle(point(0, 0), 3) as c`,
);
const [tsv] = await sql`select $$'a''b':1A,2 c$$::tsvector as t,
  to_tsvector('english', 'The quick brown foxes') as en`;
console.log(tsv);
const [{ same }] =
  await sql`select ${tsv.t}::tsvector::text = ${tsv.t} as same`;
console.log("tsvector round trip:", same);

// Booleans where the server expects a number or text; anonymous records as
// arrays of their values.
console.log(
  await sql`select ${true}::int as bi, ${false}::text as bt,
    row(1, 'x', null, '2024-01-02'::date, row(2)) as rec`,
);

// sql.array(values, type): an array parameter cast to type[].
console.log(
  await sql`select ${sql.array([1, 2, 3], "int4")} as i,
    ${sql.array(["a", "b c"], "text")} as t, ${sql.array([], "int8")} as e,
    pg_typeof(${sql.array([1.5], "double precision")})::text as dp,
    ${sql.array([[1, 2], [3, 4]], "int4[]")} as nested`,
);
await sql`create table denosql_types.arr (tags text[])`;
await sql`insert into denosql_types.arr ${
  sql({ tags: sql.array(["x", "y"], "text") })
}`;
console.log(await sql`select tags from denosql_types.arr`);
try {
  sql.array([1], "int; drop table x");
} catch (e) {
  console.log("bad type:", (e as Error).name);
}

await sql`drop schema denosql_types cascade`;
await sql.close();
