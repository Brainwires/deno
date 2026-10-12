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
// sql.file() needs read permission for the file.
try {
  await new Deno.SQL("postgres://u:p@127.0.0.1:2/db").file("query.sql");
} catch (e) {
  console.log("file:", e instanceof Deno.errors.NotCapable);
}
// A Unix-socket host needs what Deno.connect({ transport: "unix" }) needs:
// read and write access to the socket path.
const unix = new Deno.SQL({ host: "/tmp/denosql-no-such-dir", port: 5432 });
try {
  await unix`select 1`;
} catch (e) {
  console.log("unix socket:", e instanceof Deno.errors.NotCapable);
}
await unix.close();
// sslcert / sslkey files need read permission when connecting.
const cert = new Deno.SQL(
  "postgres://u:p@127.0.0.1:2/db?sslmode=require&sslcert=c.pem&sslkey=k.pem",
);
try {
  await cert`select 1`;
} catch (e) {
  console.log("sslcert:", e instanceof Deno.errors.NotCapable);
}
await cert.close();
