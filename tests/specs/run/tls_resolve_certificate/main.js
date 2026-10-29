// denext (DENEXT.md item 159): `resolveCertificate` picks the certificate per
// server name (SNI) on one listener.
const tls = "../../../testdata/tls/";
const rsa = {
  cert: Deno.readTextFileSync(tls + "localhost.crt"),
  key: Deno.readTextFileSync(tls + "localhost.key"),
};
const ecc = {
  cert: Deno.readTextFileSync(tls + "localhost_ecc.crt"),
  key: Deno.readTextFileSync(tls + "localhost_ecc.key"),
};
const caCert = Deno.readTextFileSync(tls + "RootCA.pem");
const { serverNameSymbol, getPeerCertificate } = Deno[Deno.internal];

function der(pem) {
  const body = pem.replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  return Uint8Array.from(atob(body), (c) => c.charCodeAt(0));
}
function same(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

const calls = [];
const listener = Deno.listenTls({
  hostname: "localhost",
  port: 0,
  resolveCertificate(serverName) {
    calls.push(serverName);
    if (serverName === "a.test") return rsa;
    if (serverName === "b.test") return ecc;
    // No SNI (an IP address, say): fall back to a default certificate.
    if (serverName === "") return rsa;
    throw new Error(`no certificate for ${serverName}`);
  },
});

async function connect(serverName, hostname = "localhost") {
  const conn = await Deno.connectTls({
    hostname,
    port: listener.addr.port,
    caCerts: [caCert],
    unsafelyDisableHostnameVerification: true,
    ...(serverName === undefined ? {} : { [serverNameSymbol]: serverName }),
  });
  const serverConn = await listener.accept();
  try {
    const [client] = await Promise.allSettled([
      conn.handshake(),
      serverConn.handshake(),
    ]);
    if (client.status === "rejected") return "refused";
    const peer = new Uint8Array(conn[getPeerCertificate](false).raw);
    if (same(peer, der(rsa.cert))) return "rsa";
    if (same(peer, der(ecc.cert))) return "ecc";
    return "other";
  } finally {
    conn.close();
    serverConn.close();
  }
}

console.log("a.test:", await connect("a.test"));
console.log("b.test:", await connect("b.test"));
console.log("a.test again:", await connect("a.test"));
console.log("unknown.test:", await connect("unknown.test"));
// An IP address sends no SNI. Dial the address the listener bound: "localhost"
// is ::1 on some hosts and 127.0.0.1 on others, and they are separate sockets.
console.log("no SNI:", await connect(undefined, listener.addr.hostname));
listener.close();
// Cached per name: a.test resolved once; the failed name is not cached.
console.log("calls:", JSON.stringify(calls));
