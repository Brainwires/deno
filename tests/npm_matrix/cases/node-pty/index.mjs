import pty from "node-pty";
const p = pty.spawn("/bin/echo", ["pty-hello"], { cols: 80, rows: 24 }); let out = "";
p.onData((d) => out += d); p.onExit(() => { if (!out.includes("pty-hello")) { console.error(out); process.exit(1); } console.log("SMOKE_OK"); });
