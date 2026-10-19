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
  const column: Deno.SQLColumn = values.columns[0];
  const name: string = column.name;
  const type: string | null = column.type;
  const nullable: boolean | null = column.nullable;
  if ("typeOid" in column) {
    const oid: number = column.typeOid;
    const kind: Deno.SQLPostgresTypeKind | null = column.typeKind;
    const table: number | null = column.tableOid;
    console.log(oid, kind, table);
  } else {
    const database: string | null = column.database;
    console.log(database);
  }
  console.log(name, type, nullable);
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
  for await (const batch of sql<{ id: number }>`select id from t`.cursor(10)) {
    const id: number | undefined = batch[0]?.id;
    const batchCount: number = batch.count;
    console.log(id, batchCount);
  }
  for await (const row of sql<{ id: number }>`select id from t`) {
    const id: number = row.id;
    console.log(id);
  }
  const running = sql`select pg_sleep(1)`.execute();
  running.cancel();
  const sub: Deno.SQLListener = await sql.listen(
    "chan",
    (payload: string, info) => console.log(payload, info.pid),
    () => {},
  );
  await sub.unlisten();
  await sql.notify("chan", "hi");
  await sql.file("q.sql", [1]);
  await sql`select ${sql.array([1, 2], "int4")}`;
  const hooked = new Deno.SQL({
    host: "/var/run/postgresql",
    tls: { cert: "pem", key: "pem" },
    onconnect: (error, conn) => console.log(error, conn?.parameters),
    onclose: (error, conn) => console.log(error, conn.id),
    onnotice: (notice: Deno.SQLNotice) => console.log(notice.severity),
  });
  return [first, count, command, values, n, lite, code, isSql, def, hooked];
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
  // @ts-expect-error: listen needs a callback
  await sql.listen("chan");
  // @ts-expect-error: payloads are strings
  sql.listen("chan", (payload: number) => payload);
  // @ts-expect-error: array() takes an array
  sql.array(1);
  // @ts-expect-error: count is a number
  const s: string = (await sql`select 1`).count;
  // @ts-expect-error: a column's name is a string
  const c: number = (await sql`select 1`).columns[0].name;
  return [s, c];
}
