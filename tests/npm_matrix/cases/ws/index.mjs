import { WebSocketServer, WebSocket } from "ws";
const w = new WebSocketServer({ port: 0, perMessageDeflate: true }); w.on("connection", (s) => s.on("message", (m) => s.send("echo:" + m)));
w.on("listening", () => { const c = new WebSocket(`ws://127.0.0.1:${w.address().port}`, { perMessageDeflate: true });
  c.on("open", () => c.send("x".repeat(2000))); c.on("message", (m) => { if (String(m) !== "echo:" + "x".repeat(2000)) process.exit(1); c.close(); w.close(); console.log("SMOKE_OK"); }); });
