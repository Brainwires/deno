// The net permission is checked for the host before connecting.
const sql = new Deno.SQL("postgres://u:p@127.0.0.1:1/db?sslmode=disable");
try {
  await sql`select 1`;
} catch (e) {
  console.log("net:", e instanceof Deno.errors.NotCapable);
}
await sql.close();
// The default client reads its connection string from the environment.
try {
  new Deno.SQL();
} catch (e) {
  console.log("env:", e instanceof Deno.errors.NotCapable);
}
