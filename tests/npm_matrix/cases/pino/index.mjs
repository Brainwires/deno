import pino from "pino"; import fs from "node:fs";
const l = pino(pino.transport({ targets: [{ target: "pino/file", options: { destination: "./out.log" } }, { target: "pino-pretty", options: { destination: 1 } }] }));
l.info({ a: 1 }, "hello-pino"); await new Promise((r) => setTimeout(r, 300)); l.flush();
await new Promise((r) => setTimeout(r, 1000)); const t = fs.readFileSync("out.log", "utf8"); if (!t.includes("hello-pino")) { console.error("no file log"); process.exit(1); } console.log("SMOKE_OK"); process.exit(0);
