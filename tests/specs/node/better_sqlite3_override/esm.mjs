import Database, { SqliteError } from "better-sqlite3";

const db = new Database(":memory:");
console.log(db.prepare("SELECT sqlite_version() IS NOT NULL").pluck().get());
console.log(SqliteError === Database.SqliteError);
db.close();
