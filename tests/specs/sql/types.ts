// The Deno.SQL typings: valid uses type-check; misuses are errors.
export async function valid() {
  const sql: Deno.SQL = new Deno.SQL("postgres://u@localhost/db", { max: 2 });
  const rows = await sql<
    { id: number; name: string }
  >`select id, name from t where id = ${1}`;
  const first: { id: number; name: string } | undefined = rows[0];
  const count: number = rows.count;
  const command: string = rows.command;
  const values: Deno.SQLResult<unknown[]> = await sql`select 1`.values();
  await sql`insert into t ${sql({ a: 1 }, "a")}`;
  await sql.unsafe("select $1", [1]).simple();
  const n: number = await sql.begin(async (tx) => {
    await tx.savepoint("sp", async (sp) => await sp`select 1`);
    return 1;
  });
  const reserved = await sql.reserve();
  reserved.release();
  await sql.close({ timeout: 5 });
  const lite = new Deno.SQL({ adapter: "sqlite", filename: ":memory:" });
  const err = new Deno.SQL.PostgresError("x", { code: "23505" });
  const code: string | undefined = err.code;
  const isSql: boolean = err instanceof Deno.SQL.SQLError;
  const def: Deno.SQL = Deno.sql;
  return [first, count, command, values, n, lite, code, isSql, def];
}

export async function invalid(sql: Deno.SQL) {
  // @ts-expect-error: max is a number
  new Deno.SQL({ max: "2" });
  // @ts-expect-error: unknown adapter
  new Deno.SQL({ adapter: "mysql" });
  // @ts-expect-error: begin needs a callback
  await sql.begin();
  // @ts-expect-error: unsafe takes text
  sql.unsafe(1);
  // @ts-expect-error: count is a number
  const s: string = (await sql`select 1`).count;
  return s;
}
