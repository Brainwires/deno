// sql.file(path, params): the query text read from a file when it runs.
const url = Deno.env.get("DENO_SQL_TEST_POSTGRES_URL")!;
const sql = new Deno.SQL(url, { max: 1 });
Deno.writeTextFileSync("query.sql", "select $1::int + $2::int as sum");
Deno.writeTextFileSync("multi.sql", "select 1 as a;\nselect 2 as b;");
console.log(await sql.file("query.sql", [2, 3]));
console.log(
  await sql.file(new URL("./query.sql", `file://${Deno.cwd()}/`), [1, 1]),
);
console.log(await sql.file("multi.sql").simple());
await sql.begin(async (tx) => {
  console.log("in a transaction:", (await tx.file("query.sql", [4, 5]))[0].sum);
});
try {
  await sql.file("missing.sql");
} catch (e) {
  console.log("missing:", e instanceof Deno.errors.NotFound);
}
try {
  await sql`select ${sql.file("query.sql")}`;
} catch (e) {
  console.log("as a fragment:", (e as Error).message);
}
const lazy = sql.file("later.sql");
Deno.writeTextFileSync("later.sql", "select 'read when run' as v");
console.log((await lazy)[0].v);
await sql.close();
