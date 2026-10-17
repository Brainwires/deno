import Redis from "ioredis"; const r = new Redis(56379, "127.0.0.1"); await r.set("a", "1"); const p = await r.pipeline().incr("a").get("a").exec();
await r.quit(); if (p[1][1] !== "2") throw new Error("ioredis"); console.log("SMOKE_OK");
