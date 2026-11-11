// Reads the descriptor inherited as fd 3 through net.Socket({ fd }).
import net from "node:net";

try {
  const socket = new net.Socket({ fd: 3, readable: true, writable: false });
  socket.setEncoding("utf8");
  let text = "";
  socket.on("data", (d) => text += d);
  socket.on("end", () => console.log("read:", text.trim()));
} catch (err) {
  console.log("refused:", err.code);
}
