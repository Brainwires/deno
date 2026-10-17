import { Queue, Worker } from "bullmq"; const connection = { host: "127.0.0.1", port: 56379 };
const q = new Queue("ncpq", { connection }); await q.obliterate({ force: true }).catch(() => {});
const done = new Promise((res) => { const w = new Worker("ncpq", async (j) => j.data.n * 2, { connection }); w.on("completed", async (j, r) => { await w.close(); res(r); }); });
await q.add("j", { n: 21 }); const r = await done; await q.obliterate({ force: true }); await q.close(); if (r !== 42) throw new Error("bullmq"); console.log("SMOKE_OK");
