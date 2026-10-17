import pg from "pg"; const p = new pg.Pool({ connectionString: "postgres://postgres:pw@127.0.0.1:55432/postgres" });
const r = await p.query("select $1::int + 1 as n, now() as t", [41]); const c = await p.connect(); await c.query("listen x");
const n = new Promise((res) => c.on("notification", (m) => res(m.payload))); await p.query("notify x, 'yo'"); const pl = await n; c.release(); await p.end();
if (r.rows[0].n !== 42 || pl !== "yo") throw new Error("pg"); console.log("SMOKE_OK");
