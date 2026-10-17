import { request, Agent, fetch as uf, setGlobalDispatcher, Pool } from "undici"; import http from "node:http";
const s = http.createServer((q, r) => r.end("u:" + q.url)); await new Promise(r => s.listen(0, r)); const u = `http://127.0.0.1:${s.address().port}`;
const { body } = await request(u + "/a"); const t1 = await body.text(); setGlobalDispatcher(new Agent({ keepAliveTimeout: 10 }));
const t2 = await (await uf(u + "/b")).text(); const pool = new Pool(u); const r3 = await pool.request({ path: "/c", method: "GET" }); const t3 = await r3.body.text(); await pool.close(); s.close();
if (t1 !== "u:/a" || t2 !== "u:/b" || t3 !== "u:/c") throw new Error([t1, t2, t3]); console.log("SMOKE_OK");
