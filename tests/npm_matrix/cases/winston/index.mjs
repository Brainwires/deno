import winston from "winston"; import "winston-daily-rotate-file"; import fs from "node:fs";
const l = winston.createLogger({ format: winston.format.json(), transports: [new winston.transports.Console(), new winston.transports.File({ filename: "w.log" })] });
l.info("hello-winston", { k: 1 }); l.end(); await new Promise((r) => l.on("finish", r)); await new Promise((r) => setTimeout(r, 300));
if (!fs.readFileSync("w.log", "utf8").includes("hello-winston")) process.exit(1); console.log("SMOKE_OK");
