// denext (DENEXT.md item 159): `Deno.serve` takes `resolveCertificate` too,
// and an HTTPS client sees the certificate chosen for its host name.
const tls = "../../../testdata/tls/";
const cert = Deno.readTextFileSync(tls + "localhost.crt");
const key = Deno.readTextFileSync(tls + "localhost.key");
const caCert = Deno.readTextFileSync(tls + "RootCA.pem");

const names = [];
const ac = new AbortController();
const { promise: listening, resolve } = Promise.withResolvers();
const server = Deno.serve({
  hostname: "localhost",
  port: 0,
  signal: ac.signal,
  onListen: ({ port }) => resolve(port),
  resolveCertificate(serverName) {
    names.push(serverName);
    return { cert, key };
  },
}, () => new Response("hello over SNI"));

const port = await listening;
const client = Deno.createHttpClient({ caCerts: [caCert] });
const res = await fetch(`https://localhost:${port}/`, { client });
console.log(res.status, await res.text());
client.close();
ac.abort();
await server.finished;
console.log("names:", JSON.stringify(names));
