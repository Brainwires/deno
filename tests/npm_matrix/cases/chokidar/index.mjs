import chokidar from "chokidar"; import fs from "node:fs"; fs.mkdirSync("w", { recursive: true });
const w = chokidar.watch("w", { ignoreInitial: true }); await new Promise(r => w.on("ready", r));
const ev = new Promise(r => w.on("add", r)); fs.writeFileSync("w/f.txt", "x"); const p = await Promise.race([ev, new Promise(r => setTimeout(() => r(null), 5000))]);
await w.close(); if (!p) { console.error("no event"); process.exit(1); } console.log("SMOKE_OK");
