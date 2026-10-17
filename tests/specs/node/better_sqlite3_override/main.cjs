const Database = require("better-sqlite3");
const assert = require("node:assert");

const db = new Database(":memory:");
console.log("memory", db.memory, db.name, db.open);

// statements: run, get, all, iterate, pluck, raw, expand, columns
db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, data BLOB)");
const insert = db.prepare("INSERT INTO users (name, data) VALUES (@name, ?)");
console.log("run", insert.run({ name: "ada" }, Buffer.from([1, 2])));
insert.run({ name: "grace" }, null);
console.log("reader", insert.reader, insert.readonly);
const select = db.prepare("SELECT id, name, data FROM users ORDER BY id");
console.log("get", select.get());
console.log("blob is a Buffer", Buffer.isBuffer(select.get().data));
console.log("all", select.all().map((r) => r.name).join(","));
console.log("pluck", select.pluck().all().join(","));
console.log("raw", JSON.stringify(select.raw().get()));
select.raw(false);
db.exec("CREATE TABLE posts (id INTEGER PRIMARY KEY, user INTEGER)");
db.prepare("INSERT INTO posts (id, user) VALUES (7, 1)").run();
const join = db.prepare(
  "SELECT users.id, posts.id, 1 + 1 AS two FROM users JOIN posts ON posts.user = users.id",
);
console.log("expand", JSON.stringify(join.expand().get()));
console.log("columns", select.columns().map((c) => c.name).join(","));
let names = [];
for (const row of select.iterate()) names.push(row.name);
console.log("iterate", names.join(","));

// binding errors as better-sqlite3 reports them
assert.throws(() => insert.run({ name: "x" }), RangeError);
assert.throws(() => insert.run({ name: "x" }, true), TypeError);
assert.throws(() => db.prepare("SELECT 1; SELECT 2"), RangeError);

// SqliteError with SQLite's extended code name
db.exec("CREATE TABLE t (a PRIMARY KEY)");
db.prepare("INSERT INTO t VALUES (1)").run();
try {
  db.prepare("INSERT INTO t VALUES (1)").run();
} catch (err) {
  console.log(
    "error",
    err instanceof Database.SqliteError,
    err.code,
    err.message,
  );
}

// transactions, nested with savepoints
const add = db.prepare("INSERT INTO t VALUES (?)");
const addMany = db.transaction((values) => {
  for (const v of values) add.run(v);
});
addMany([2, 3]);
try {
  db.transaction(() => {
    addMany([4]);
    throw new Error("rollback");
  })();
} catch {}
console.log("rows", db.prepare("SELECT count(*) FROM t").pluck().get());
console.log("inTransaction", db.inTransaction);

// pragma, user functions, aggregates
console.log("pragma", db.pragma("user_version", { simple: true }));
db.function("double", (x) => x * 2);
db.aggregate("total", { start: 0, step: (acc, x) => acc + x });
console.log(
  "function",
  db.prepare("SELECT double(21)").pluck().get(),
  db.prepare("SELECT total(a) FROM t").pluck().get(),
);

// integers past 2^53: lossy numbers, or BigInt with safeIntegers
db.exec("CREATE TABLE big (n INTEGER)");
db.prepare("INSERT INTO big VALUES (?)").run(2n ** 60n);
const big = db.prepare("SELECT n FROM big").pluck();
console.log("bigint", typeof big.get(), big.safeIntegers().get());

// serialize, and a database opened from the bytes
const copy = new Database(db.serialize());
console.log("serialize", copy.prepare("SELECT count(*) FROM t").pluck().get());
copy.close();

// backup to a file
const file = "backup.db";
db.backup(file).then((progress) => {
  const restored = new Database(file, { readonly: true });
  console.log(
    "backup",
    progress.remainingPages,
    restored.prepare("SELECT count(*) FROM t").pluck().get(),
  );
  restored.close();
  db.close();
  console.log("closed", db.open);
  assert.throws(() => db.prepare("SELECT 1"), TypeError);
});
