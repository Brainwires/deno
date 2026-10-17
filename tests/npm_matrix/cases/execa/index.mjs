import { execa, execaNode } from "execa"; const { stdout } = await execa("echo", ["hi"]); const p = await execa({ shell: true })`echo a | tr a b`;
const n = await execaNode("child.mjs", ["z"], { ipc: true }); if (stdout !== "hi" || p.stdout !== "b" || n.stdout !== "child z") throw new Error(JSON.stringify([stdout, p.stdout, n.stdout])); console.log("SMOKE_OK");
