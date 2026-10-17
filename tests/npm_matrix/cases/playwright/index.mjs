import { chromium } from "playwright";
const b = await chromium.launch(); const p = await b.newPage(); await p.setContent("<h1 id=x>pw</h1>");
const t = await p.textContent("#x"); await b.close(); if (t !== "pw") throw new Error("pw"); console.log("SMOKE_OK");
