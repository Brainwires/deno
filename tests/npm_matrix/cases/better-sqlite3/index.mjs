import Database from "better-sqlite3";
const db = new Database(":memory:"); db.exec("create table t(a)"); db.prepare("insert into t values (?)").run(5);
const r = db.prepare("select a from t").get(); db.close(); if (r.a !== 5) throw new Error("bsq"); console.log("SMOKE_OK");
