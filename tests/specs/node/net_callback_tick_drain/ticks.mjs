// A server's `connection` callback and a client's `connect` callback that
// fire in the same event-loop turn each run their `process.nextTick`
// callbacks before the next native callback starts, as in Node
// (InternalCallbackScope). Without that, both callbacks run first and the
// ticks after them.
import net from "node:net";

const events = [];
const server = net.createServer((conn) => {
  events.push("connection");
  process.nextTick(() => events.push("connection tick"));
  conn.end();
});
server.listen(0, "127.0.0.1", () => {
  const client = net.connect(server.address().port, "127.0.0.1", () => {
    events.push("connect");
    process.nextTick(() => events.push("connect tick"));
  });
  client.on("close", () => {
    server.close(() => {
      // Each callback is directly followed by its own tick.
      let ok = events.length === 4;
      for (let i = 0; i < events.length; i += 2) {
        ok &&= events[i + 1] === `${events[i]} tick`;
      }
      console.log(ok ? "interleaved" : `not interleaved: ${events.join(", ")}`);
    });
  });
  client.resume();
});
