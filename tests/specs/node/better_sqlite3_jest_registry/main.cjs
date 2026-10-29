"use strict";
const fs = require("node:fs");
const registryRequire = require("./registry.cjs");

const Database = registryRequire("better-sqlite3");
const { SqliteError } = Database;

const db = new Database(":memory:");
console.log("foreign_keys:", db.pragma("foreign_keys", { simple: true }));
console.log("pragma set:", db.pragma("cache_size = -8000"));
db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)");

const insert = db.prepare("INSERT INTO t (name) VALUES (?)");
console.log("statement database:", insert.database === db);
db.transaction((names) => {
  for (const name of names) insert.run(name);
})(["a", "b", "c"]);
db.transaction(() => {
  insert.run("d");
  try {
    db.transaction(() => {
      insert.run("e");
      throw new Error("inner");
    })();
  } catch {
    // the savepoint rolled back
  }
})();
console.log(
  "names:",
  db.prepare("SELECT name FROM t ORDER BY id").pluck().all(),
);

db.function("twice", (x) => x * 2);
console.log("function:", db.prepare("SELECT twice(21)").pluck().get());
db.aggregate("joined", { start: "", step: (acc, value) => acc + value });
console.log(
  "aggregate:",
  db.prepare("SELECT joined(name) FROM t").pluck().get(),
);

try {
  insert.run(null);
} catch (err) {
  console.log("error:", err instanceof SqliteError, err.code);
}

// another test file: the package evaluated again, the addon (cached by the
// host's require) shared; each database throws its own registry's class
registryRequire.fresh();
const Database2 = registryRequire("better-sqlite3");
const db2 = new Database2(":memory:");
try {
  db2.exec("SELECT * FROM missing");
} catch (err) {
  console.log(
    "second registry:",
    Database2 !== Database,
    err instanceof Database2.SqliteError,
    err instanceof SqliteError,
  );
}
db2.close();
try {
  insert.run(null);
} catch (err) {
  console.log("first registry kept:", err instanceof SqliteError);
}

for (
  const row of db.prepare("SELECT id, name FROM t WHERE id <= 2").iterate()
) {
  console.log("row:", row);
}
console.log("state:", db.name, db.memory, db.readonly, db.inTransaction);

const copy = new Database(db.serialize());
console.log(
  "serialized:",
  copy.prepare("SELECT count(*) FROM t").pluck().get(),
);
copy.close();

db.backup("backup.db").then((progress) => {
  console.log("backup:", progress.remainingPages);
  const restored = new Database("backup.db", { readonly: true });
  console.log(
    "restored:",
    restored.prepare("SELECT count(*) FROM t").pluck().get(),
  );
  restored.close();
  fs.rmSync("backup.db");
  db.close();
  console.log("open:", db.open);
});
