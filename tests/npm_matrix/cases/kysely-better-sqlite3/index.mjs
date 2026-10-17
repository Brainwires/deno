import Database from "better-sqlite3";
import { Kysely, SqliteDialect } from "kysely";
const db = new Kysely({ dialect: new SqliteDialect({ database: new Database(":memory:") }) });
await db.schema.createTable("users")
  .addColumn("id", "integer", (c) => c.primaryKey().autoIncrement())
  .addColumn("name", "text", (c) => c.notNull())
  .execute();
const result = await db.insertInto("users").values({ name: "ada" }).executeTakeFirst();
await db.transaction().execute((trx) => trx.insertInto("users").values({ name: "grace" }).execute());
const rows = await db.selectFrom("users").selectAll().orderBy("id").execute();
await db.destroy();
if (Number(result.insertId) !== 1 || rows.length !== 2 || rows[1].name !== "grace") throw new Error(JSON.stringify({ rows }));
console.log("SMOKE_OK");
