const sqlite3 = require("sqlite3"); const db = new sqlite3.Database(":memory:");
db.serialize(() => { db.run("create table t(a)"); db.run("insert into t values (9)"); db.get("select a from t", (e, r) => { if (e || r.a !== 9) throw e || new Error("sqlite3"); db.close(); console.log("SMOKE_OK"); }); });
