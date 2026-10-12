// `timestamp` (without time zone) is local wall-clock time both ways, as in
// Bun, postgres.js and node-postgres; `timestamptz` and `date` are instants
// / UTC midnights. Run with TZ=America/Chicago (UTC-6, UTC-5 in summer).
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1, connection: { TimeZone: "UTC" } });
const iso = (d: unknown) => (d as Date).toISOString();
const local = (d: Date) =>
  `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()} ` +
  `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;

// Reading.
const [r] = await sql`select
  '2024-01-15 12:00:00'::timestamp as winter,
  '2024-07-15 12:00:00.123456'::timestamp as summer,
  '2024-03-10 02:30:00'::timestamp as gap,
  '2024-11-03 01:30:00'::timestamp as twice,
  '1969-07-20 20:17:40'::timestamp as old,
  array['2024-01-15 12:00'::timestamp, null] as arr,
  '2024-01-15 12:00:00+00'::timestamptz as tz,
  '2024-01-15'::date as d`;
console.log("winter:", iso(r.winter), local(r.winter));
console.log("summer:", iso(r.summer), local(r.summer));
// 02:30 never happened that night (the clock went 2:00 -> 3:00): read as
// new Date(2024, 2, 10, 2, 30) is, 03:30 CDT.
console.log(
  "gap:",
  iso(r.gap),
  r.gap.getTime() === new Date(2024, 2, 10, 2, 30).getTime(),
);
// 01:30 happened twice (3:00 CDT -> 2:00 CST... 2:00 -> 1:00): the first.
console.log(
  "twice:",
  iso(r.twice),
  r.twice.getTime() === new Date(2024, 10, 3, 1, 30).getTime(),
);
console.log("old:", iso(r.old), local(r.old));
console.log("array:", r.arr.map((d: Date | null) => d && iso(d)));
console.log("timestamptz:", iso(r.tz), "date:", iso(r.d));

// Sending: a Date is its local time for a timestamp, its instant for a
// timestamptz, whatever the session time zone.
const noon = new Date(2024, 0, 15, 12, 0, 0, 250);
const [s] = await sql`select ${noon}::timestamp::text as ts,
  ${noon}::timestamptz::text as tstz,
  ${new Date(2024, 6, 15, 12)}::timestamp::text as summer,
  ${[noon]}::timestamp[]::text as arr,
  ${new Date(Date.UTC(2024, 0, 2))}::date::text as d`;
console.log(s);

// Round trips through columns.
await sql`create temp table times (ts timestamp, tstz timestamptz, d date)`;
const values = [
  new Date(2024, 0, 15, 12),
  new Date(2024, 6, 15, 23, 59, 59, 999),
  new Date(2024, 2, 10, 3, 30), // just after the spring change
  new Date(2024, 10, 3, 0, 59), // just before the autumn change
];
for (const v of values) {
  await sql`insert into times ${
    sql({ ts: v, tstz: v, d: new Date(Date.UTC(2024, 1, 29)) })
  }`;
}
const back = await sql`select ts, tstz, d from times`;
console.log(
  "round trip:",
  back.every((row, i) =>
    row.ts.getTime() === values[i].getTime() &&
    row.tstz.getTime() === values[i].getTime() &&
    iso(row.d) === "2024-02-29T00:00:00.000Z"
  ),
);
// The second 01:30 (CST) of the autumn night goes in as 01:30 and so comes
// back as the first: a timestamp can't tell them apart.
const second = new Date(Date.UTC(2024, 10, 3, 7, 30));
const [{ ts }] = await sql`select ${second}::timestamp as ts`;
console.log("ambiguous:", local(second), iso(ts));
await sql.close();
