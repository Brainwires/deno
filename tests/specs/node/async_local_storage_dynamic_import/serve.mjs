// A module first loaded inside one request must not leave that request's
// async context active for the next request the server receives.
import { als } from "./als.mjs";

let count = 0;
const server = Deno.serve({ port: 0, onListen() {} }, async () => {
  const seen = als.getStore() ?? "none";
  if (++count === 1) {
    const mod = await als.run("REQ1", () => import("./fresh.mjs"));
    return new Response(`${seen} (module top level: ${mod.atTop})`);
  }
  return new Response(seen);
});

const url = `http://127.0.0.1:${server.addr.port}/`;
console.log("request 1 saw:", await (await fetch(url)).text());
console.log("request 2 saw:", await (await fetch(url)).text());
await server.shutdown();
