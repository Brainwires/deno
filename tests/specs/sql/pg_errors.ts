// PostgresError carries the server's SQLSTATE and fields.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1 });
await sql`drop table if exists denosql_errors`;
await sql`create table denosql_errors (id int primary key, v text not null)`;
await sql`insert into denosql_errors values (1, 'a')`;

async function show(label: string, q: PromiseLike<unknown>) {
  try {
    await q;
    console.log(label, "no error");
  } catch (e) {
    const err = e as Deno.PostgresError;
    const fields = Object.fromEntries(
      [
        "code",
        "errno",
        "severity",
        "constraint",
        "table",
        "column",
        "position",
        "detail",
      ]
        .filter((k) =>
          (err as unknown as Record<string, unknown>)[k] !== undefined
        )
        .map((k) => [k, (err as unknown as Record<string, unknown>)[k]]),
    );
    console.log(
      label,
      err instanceof Deno.SQL.PostgresError,
      err.name,
      err.message,
      fields,
    );
  }
}

await show("unique:", sql`insert into denosql_errors values (${1}, ${"b"})`);
await show("not null:", sql`insert into denosql_errors (id) values (${2})`);
await show("syntax:", sql`selec 1`);
await show("undefined table:", sql`select * from denosql_nope`);
await show("division:", sql`select 1 / ${0}::int`);
await show("bad input:", sql`select ${"abc"}::int`);
await show("param count:", sql.unsafe("select $1::int, $2::int", [1]));
await show("nul byte:", sql`select ${"a\u0000b"}::text`);
// The connection is still usable after errors.
console.log(await sql`select count(*)::int as n from denosql_errors`);

// A cached statement whose result type changed fails once, then is
// prepared again.
await sql`drop table if exists denosql_stale`;
await sql`create table denosql_stale (a int)`;
await sql`insert into denosql_stale values (1)`;
const stale = () => sql`select * from denosql_stale`;
await stale();
await sql`alter table denosql_stale add column b int`;
await show("stale plan:", stale());
console.log("re-prepared:", await stale());
await sql`drop table denosql_stale`;

// Authentication failure.
const bad = new URL(url);
bad.password = "wrong-password";
const badSql = new Deno.SQL(bad.href);
await show("auth:", badSql`select 1`);
await badSql.close();

await sql`drop table denosql_errors`;
await sql.close();
