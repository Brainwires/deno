// :memory: needs no permission; a file needs read (+ write unless readonly).
const memory = new Deno.SQL(":memory:");
console.log(await memory`select 1 as one`);
// Without full permissions ATTACH (and VACUUM INTO) can't reach other files.
try {
  await memory`attach database 'other.db' as other`;
} catch (e) {
  console.log("attach:", (e as Deno.SQLiteError).code);
}
await memory.close();

const file = new Deno.SQL("sqlite://perm.db");
try {
  await file`select 1`;
} catch (e) {
  console.log("file:", e instanceof Deno.errors.NotCapable);
}
