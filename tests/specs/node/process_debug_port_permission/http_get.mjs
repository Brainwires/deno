import http from "node:http";

const server = http.createServer((_req, res) => res.end("hello"));
server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  http.get(`http://127.0.0.1:${port}/`, (res) => {
    let body = "";
    res.setEncoding("utf8");
    res.on("data", (chunk) => body += chunk);
    res.on("end", () => {
      console.log(`status: ${res.statusCode}`);
      console.log(`body: ${body}`);
      server.close();
    });
  });
});
