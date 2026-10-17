import { createClient } from "redis"; const c = await createClient({ url: "redis://127.0.0.1:56379" }).connect();
const sub = c.duplicate(); await sub.connect(); let got; await sub.subscribe("ch", (m) => got = m);
await c.set("k", "v"); await c.publish("ch", "hello"); await new Promise(r => setTimeout(r, 200));
const v = await c.get("k"); await c.quit(); await sub.quit(); if (v !== "v" || got !== "hello") throw new Error("redis"); console.log("SMOKE_OK");
