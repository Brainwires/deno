import axios from "axios"; import http from "node:http";
const s = http.createServer((q, r) => { let b = ""; q.on("data", (d) => b += d); q.on("end", () => { r.setHeader("content-type", "application/json"); r.end(JSON.stringify({ m: q.method, b })); }); });
s.listen(0, async () => { const u = `http://127.0.0.1:${s.address().port}`; const a = await axios.post(u, { x: 1 }); const g = await axios.get(u, { responseType: "stream" });
  let t = ""; for await (const ch of g.data) t += ch; s.close(); if (a.data.m !== "POST" || !t.includes("GET")) process.exit(1); console.log("SMOKE_OK"); });
