import { createServer } from "node:http"; import { Server } from "socket.io"; import { io } from "socket.io-client";
const h = createServer(); const s = new Server(h); s.on("connection", (c) => c.on("ping", (m, ack) => ack(m + "!")));
h.listen(0, () => { const c = io(`http://127.0.0.1:${h.address().port}`, { transports: ["websocket"] });
  c.emit("ping", "hi", (r) => { const c2 = io(`http://127.0.0.1:${h.address().port}`, { transports: ["polling"] }); c2.emit("ping", "p", (r2) => {
    if (r !== "hi!" || r2 !== "p!") process.exit(1); c.close(); c2.close(); s.close(); console.log("SMOKE_OK"); }); }); });
