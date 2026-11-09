// The async resource graph an HTTP request makes, as async_hooks reports it:
// each callback runs as its resource (between before and after), and the
// trigger chain leads from an fs callback back through the request parser and
// the connection to the listening server.
"use strict";
const ah = require("node:async_hooks");
const http = require("node:http");
const fs = require("node:fs");
const KEEP = new Set([
  "TCPSERVERWRAP",
  "TCPWRAP",
  "HTTPINCOMINGMESSAGE",
  "HTTPCLIENTREQUEST",
  "FSREQCALLBACK",
  "Timeout",
]);
const resources = new Map();
const active = new Set();
ah.createHook({
  init(id, type, trigger, resource) {
    resources.set(id, { type, trigger, resource });
  },
  before(id) {
    active.add(id);
  },
  after(id) {
    active.delete(id);
  },
}).enable();
const out = [];
function where(label) {
  const id = ah.executionAsyncId();
  const own = resources.get(id);
  const chain = [];
  for (let cur = id, i = 0; i < 20 && resources.has(cur); i++) {
    const r = resources.get(cur);
    if (KEEP.has(r.type) && chain.at(-1) !== r.type) chain.push(r.type);
    cur = r.trigger;
  }
  const resourceMatches = own !== undefined &&
    ah.executionAsyncResource() === own.resource;
  out.push(
    `${label}: runs as ${own?.type ?? "?"}, between before/after: ${
      active.has(id)
    }, executionAsyncResource matches: ${resourceMatches}, chain: ${
      chain.join(" < ")
    }`,
  );
}
const server = http.createServer((req, res) => {
  where("request handler");
  fs.stat(__filename, () => {
    where("fs.stat callback");
    fs.readFile(__filename, () => {
      where("fs.readFile callback");
      setTimeout(() => {
        where("setTimeout callback");
        res.end("ok");
      }, 1);
    });
  });
});
server.on("connection", () => where("connection event"));
server.listen(0, () => {
  http.get({ port: server.address().port, agent: false }, (res) => {
    where("client response callback");
    res.resume();
    res.on("end", () => {
      server.close(() => {
        for (const line of out) console.log(line);
      });
    });
  });
});
