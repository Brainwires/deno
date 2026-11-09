// A minimal reimplementation of OpenTelemetry's context propagation pattern:
// an AsyncLocalStorage context manager, an http instrumentation that wraps
// Server.prototype.emit("request") and http.request, and an fs instrumentation
// that binds callbacks. Every check prints "ok <name>" or "LOST <name> <got>".
"use strict";
const { AsyncLocalStorage } = require("node:async_hooks");
const { EventEmitter } = require("node:events");
const http = require("node:http");
const net = require("node:net");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");

const als = new AsyncLocalStorage();
const ROOT = { name: "root" };
const ctx = {
  active: () => als.getStore() ?? ROOT,
  with: (c, fn, thisArg, ...args) =>
    als.run(c, thisArg == null ? fn : fn.bind(thisArg), ...args),
  bind(c, target) {
    if (typeof target === "function") {
      return function (...args) {
        return ctx.with(c, () => target.apply(this, args));
      };
    }
    return target;
  },
};

const results = [];
function check(name, want) {
  const got = ctx.active().name;
  results.push(
    got === want ? `ok ${name}` : `LOST ${name} (got ${got}, want ${want})`,
  );
}

// http instrumentation: incoming
const origEmit = http.Server.prototype.emit;
http.Server.prototype.emit = function (event, ...args) {
  if (event !== "request" || this.uninstrumented) {
    return origEmit.apply(this, [event, ...args]);
  }
  const req = args[0];
  const span = { name: "server:" + req.url };
  return ctx.with(span, () => origEmit.apply(this, [event, ...args]));
};
// http instrumentation: outgoing
const origRequest = http.request;
http.request = function (...args) {
  const parent = ctx.active();
  const span = { name: "client:" + parent.name };
  return ctx.with(span, () => {
    const cb = args[args.length - 1];
    if (typeof cb === "function") args[args.length - 1] = ctx.bind(parent, cb);
    return origRequest.apply(this, args);
  });
};
// fs instrumentation: callback form keeps the caller's context
const origReadFile = fs.readFile;
fs.readFile = function (...args) {
  const parent = ctx.active();
  const span = { name: "fs:" + parent.name };
  return ctx.with(span, () => origReadFile.apply(this, args));
};

const tmp = path.join(os.tmpdir(), `otel-pattern-${process.pid}.txt`);
fs.writeFileSync(tmp, "hello");

const server = http.createServer(async (req, res) => {
  const want = "server:" + req.url;
  check(`${req.url} handler`, want);
  await null;
  check(`${req.url} after await`, want);
  await new Promise((r) => setTimeout(r, 1));
  check(`${req.url} after setTimeout promise`, want);
  setTimeout(() => check(`${req.url} setTimeout cb`, want), 1);
  setImmediate(() => check(`${req.url} setImmediate cb`, want));
  process.nextTick(() => check(`${req.url} nextTick cb`, want));
  queueMicrotask(() => check(`${req.url} queueMicrotask cb`, want));
  // instrumented callback fs: the callback runs in the fs span's context
  await new Promise((r) =>
    fs.readFile(tmp, () => {
      check(`${req.url} fs.readFile cb`, "fs:" + want);
      r();
    })
  );
  // uninstrumented fs calls keep the caller's context
  await new Promise((r) =>
    fs.stat(tmp, () => {
      check(`${req.url} fs.stat cb`, want);
      r();
    })
  );
  await fs.promises.readFile(tmp);
  check(`${req.url} after fs.promises`, want);
  req.on("data", () => {});
  req.on("end", () => check(`${req.url} req end`, want));
  res.on("finish", () => check(`${req.url} res finish`, want));
  res.on("close", () => check(`${req.url} res close`, want));
  // outgoing request from inside the handler
  if (req.url !== "/inner") {
    await new Promise((resolve) => {
      const out = http.request({
        port: server.address().port,
        path: "/inner",
        agent: false,
      }, (r2) => {
        check(`${req.url} outgoing response cb`, want);
        r2.on("data", () => {});
        r2.on("end", () => {
          check(`${req.url} outgoing response end`, want);
          resolve();
        });
      });
      out.on(
        "socket",
        () => check(`${req.url} outgoing socket event`, "client:" + want),
      );
      out.on(
        "finish",
        () => check(`${req.url} outgoing finish event`, "client:" + want),
      );
      out.end();
    });
  }
  setTimeout(() => res.end("done"), 2);
});

// The server listens inside a "boot" context: a request handler that no
// instrumentation wraps still runs in it (the parser's context comes from the
// connection, which comes from the listening handle), and a client socket's
// reads run in the context it was connected in.
const BOOT = { name: "boot" };
const bare = http.createServer((req, res) => {
  check("bare handler", "boot");
  setTimeout(() => res.end("x"), 1);
});
bare.uninstrumented = true;
const listenBare = () =>
  new Promise((resolve) => ctx.with(BOOT, () => bare.listen(0, resolve)));
const netEcho = net.createServer((c) => c.end("hi"));

server.listen(0, async () => {
  const port = server.address().port;
  await listenBare();
  await new Promise((resolve) => {
    ctx.with(
      ROOT,
      () =>
        origRequest({ port: bare.address().port, agent: false }, (r) => {
          r.resume();
          r.on("end", resolve);
        }).end(),
    );
  });
  await new Promise((resolve) => netEcho.listen(0, resolve));
  await new Promise((resolve) => {
    ctx.with({ name: "net-client" }, () => {
      const c = net.connect(netEcho.address().port);
      c.on("data", () => check("net client data", "net-client"));
      c.on("end", () => check("net client end", "net-client"));
      c.on("close", () => {
        check("net client close", "net-client");
        resolve();
      });
    });
  });
  bare.close();
  netEcho.close();
  const get = (p) =>
    new Promise((resolve, reject) => {
      origRequest({ port, path: p, agent: false }, (r) => {
        r.resume();
        r.on("end", resolve);
      }).on("error", reject).end();
    });
  // two concurrent requests: neither may see the other's context
  await Promise.all([get("/a"), get("/b")]);
  await new Promise((r) => setTimeout(r, 20));
  server.close();
  fs.unlinkSync(tmp);
  results.sort();
  for (const r of results) console.log(r);
  const lost = results.filter((r) => r.startsWith("LOST")).length;
  console.log(lost === 0 ? "ALL OK" : `${lost} LOST`);
});
