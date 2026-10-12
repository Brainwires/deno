// With only --allow-read a database can be opened read-only.
const ro = new Deno.SQL("sqlite://ro.db?mode=ro");
console.log(await ro`select count(*) as n from t`);
try {
  await ro`insert into t values (2)`;
} catch (e) {
  console.log((e as Deno.SQLiteError).code);
}
await ro.close();
const rw = new Deno.SQL("sqlite://ro.db");
try {
  await rw`select 1`;
} catch (e) {
  console.log("read-write:", e instanceof Deno.errors.NotCapable);
}
