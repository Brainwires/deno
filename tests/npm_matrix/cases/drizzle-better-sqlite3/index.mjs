import Database from "better-sqlite3";
import { drizzle } from "drizzle-orm/better-sqlite3";
import { integer, sqliteTable, text } from "drizzle-orm/sqlite-core";
import { eq, sql } from "drizzle-orm";
const db = drizzle(new Database(":memory:"));
db.run(sql`create table users (id integer primary key autoincrement, name text not null, active integer default 1)`);
const users = sqliteTable("users", {
  id: integer("id").primaryKey({ autoIncrement: true }),
  name: text("name").notNull(),
  active: integer("active", { mode: "boolean" }).default(true),
});
const [row] = db.insert(users).values({ name: "ada" }).returning().all();
db.transaction((tx) => { tx.insert(users).values({ name: "grace", active: false }).run(); });
const got = db.select().from(users).where(eq(users.id, row.id)).get();
const { n } = db.select({ n: sql`count(*)` }).from(users).get();
db.$client.close();
if (got.name !== "ada" || got.active !== true || n !== 2) throw new Error(JSON.stringify({ got, n }));
console.log("SMOKE_OK");
