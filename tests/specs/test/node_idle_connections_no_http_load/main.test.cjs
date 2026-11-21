// `deno test` closes the idle connections of Node's global HTTP agents after
// every test. In a file that has `node:module` loaded (any CommonJS test file)
// but never used `node:http` / `node:https`, that must not load them.
const internals = Deno[Deno.internal];

function assert(cond, message) {
  if (!cond) throw new Error(message);
}

Deno.test("first", () => {
  assert(
    typeof internals.closeIdleConnections === "function",
    "node:module is loaded",
  );
});

Deno.test("no http module was loaded after the first test", () => {
  assert(!internals.__nodeHttpLoaded, "node:http was loaded");
  assert(!internals.__nodeHttpsLoaded, "node:https was loaded");
  internals.closeIdleConnections();
  assert(!internals.__nodeHttpLoaded, "node:http was loaded");
  assert(!internals.__nodeHttpsLoaded, "node:https was loaded");
});

Deno.test("loaded agents are destroyed", () => {
  const http = require("node:http");
  const https = require("node:https");
  assert(internals.__nodeHttpLoaded, "node:http is marked loaded");
  assert(internals.__nodeHttpsLoaded, "node:https is marked loaded");
  const destroyed = [];
  const httpDestroy = http.globalAgent.destroy;
  const httpsDestroy = https.globalAgent.destroy;
  http.globalAgent.destroy = function () {
    destroyed.push("http");
    return httpDestroy.call(this);
  };
  https.globalAgent.destroy = function () {
    destroyed.push("https");
    return httpsDestroy.call(this);
  };
  try {
    internals.closeIdleConnections();
  } finally {
    http.globalAgent.destroy = httpDestroy;
    https.globalAgent.destroy = httpsDestroy;
  }
  assert(
    destroyed.join() === "http,https",
    `destroyed: ${destroyed.join()}`,
  );
});
