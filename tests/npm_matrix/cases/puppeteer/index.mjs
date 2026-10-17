import puppeteer from "puppeteer";
const b = await puppeteer.launch({ headless: true, args: ["--no-sandbox"] }); const p = await b.newPage();
await p.setContent("<h1>pp</h1>"); const t = await p.$eval("h1", (e) => e.textContent); await b.close();
if (t !== "pp") throw new Error("pp"); console.log("SMOKE_OK");
