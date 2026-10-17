import knex from "knex";
const db = knex({ client: "better-sqlite3", connection: { filename: ":memory:" }, useNullAsDefault: true });
await db.schema.createTable("users", (t) => { t.increments("id"); t.string("name"); t.boolean("active"); });
const [id] = await db("users").insert({ name: "ada", active: true });
await db.transaction(async (trx) => { await trx("users").insert({ name: "grace", active: false }); });
const rows = await db("users").select().orderBy("id");
await db.destroy();
if (id !== 1 || rows.length !== 2 || rows[1].name !== "grace" || rows[0].active !== 1) throw new Error(JSON.stringify({ id, rows }));
console.log("SMOKE_OK");
