import * as http from "node:http";

// DENO_SERVE_ADDRESS=memory:<name> (the desktop runtime's in-process
// transport): the node:http server listens on the memory channel instead of
// the requested TCP port, without an "error" (it used to fail with "unknown
// override kind: 5"). Only the desktop runtime can connect to a memory
// listener, so the request round trip is covered by the desktop e2e suite
// (.github/denext-runtime/e2e, area "origin").
const server = http.createServer((_req, res) => res.end("memory-only"));
server.on("error", (err) => {
  console.log(`error: ${err.message}`);
  Deno.exit(1);
});
server.listen(9998, "127.0.0.1", () => {
  console.log(`listening: ${server.listening}`);
  // The requested TCP port is not bound.
  Deno.connect({ hostname: "127.0.0.1", port: 9998 }).then(
    (conn) => {
      conn.close();
      console.log("tcp reachable: true");
    },
    () => console.log("tcp reachable: false"),
  ).then(() => {
    // Give an asynchronous listener failure a chance to surface.
    setTimeout(() => {
      server.close(() => console.log("closed"));
    }, 100);
  });
});
