// `Deno.sql` and `new Deno.SQL()` read DATABASE_URL (and friends).
const sql = Deno.sql;
console.log(sql === Deno.sql, typeof sql, sql instanceof Deno.SQL);
const { hostname, port, username, database, sslMode, max } = sql
  .options as Record<string, unknown>;
console.log({ hostname, port, username, database, sslMode, max });
console.log("password hidden:", !("password" in sql.options));
