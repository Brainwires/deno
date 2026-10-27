// A local server stands in for an HTTP proxy: it answers a CONNECT itself and
// serves a GET. HTTP_PROXY (set by the test runner) points somewhere else, so
// both requests below succeed only when they connect to this server directly.
import http from "node:http";

const server = http.createServer((req, res) => res.end(`served ${req.url}`));
server.on("connect", (req, socket) => {
  socket.end(
    `HTTP/1.1 200 Connection Established\r\nx-target: ${req.url}\r\n\r\n`,
  );
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const { port } = server.address();

const connect = await new Promise((resolve) => {
  const req = http.request({
    host: "127.0.0.1",
    port,
    method: "CONNECT",
    path: "example.com:443",
    agent: false,
  });
  req.on("connect", (res, socket) => {
    socket.destroy();
    resolve(`${res.statusCode} ${res.headers["x-target"]}`);
  });
  req.on("error", (e) => resolve(`error ${e.code ?? e.name}: ${e.message}`));
  req.end();
});
console.log("connect:", connect);

const get = await new Promise((resolve) => {
  const options = { host: "127.0.0.1", port, path: "/direct", agent: false };
  http.get(options, (res) => {
    let data = "";
    res.setEncoding("utf8");
    res.on("data", (c) => data += c);
    res.on("end", () => resolve(data));
  }).on("error", (e) => resolve(`error ${e.code ?? e.name}`));
});
console.log("get:", get);

server.close();
