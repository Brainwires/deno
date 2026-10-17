import { ESLint } from "eslint"; const e = new ESLint(); const [r] = await e.lintFiles(["bad.js"]);
if (!r.messages.some(m => m.ruleId === "no-unused-vars")) throw new Error(JSON.stringify(r.messages)); console.log("SMOKE_OK");
