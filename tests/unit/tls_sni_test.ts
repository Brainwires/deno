// Copyright 2018-2026 the Deno authors. MIT license.
import { assertEquals, assertRejects } from "./test_util.ts";
const { serverNameSymbol } =
  // @ts-expect-error TypeScript (as of 3.7) does not support indexing namespaces by symbol
  Deno[Deno.internal];

const cert = Deno.readTextFileSync("tests/testdata/tls/localhost.crt");
const key = Deno.readTextFileSync("tests/testdata/tls/localhost.key");
const caCert = Deno.readTextFileSync("tests/testdata/tls/RootCA.pem");
const certEcc = Deno.readTextFileSync("tests/testdata/tls/localhost_ecc.crt");
const keyEcc = Deno.readTextFileSync("tests/testdata/tls/localhost_ecc.key");

async function resolvesWithin(promise: Promise<unknown>, ms: number) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise.then(() => "resolved"),
      new Promise<"blocked">((resolve) => {
        timer = setTimeout(() => resolve("blocked"), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

Deno.test(
  { permissions: { net: true, read: true } },
  async function listenResolver() {
    const sniRequests: string[] = [];
    const keys: Record<string, { cert: string; key: string }> = {
      "server-1": { cert, key },
      "server-2": { cert: certEcc, key: keyEcc },
      "fail-server-3": { cert: "(invalid)", key: "(bad)" },
    };
    const listener = Deno.listenTls({
      hostname: "localhost",
      port: 0,
      resolveCertificate(serverName) {
        sniRequests.push(serverName);
        return keys[serverName]!;
      },
    });

    for (
      const server of ["server-1", "server-2", "fail-server-3", "fail-server-4"]
    ) {
      const conn = await Deno.connectTls({
        hostname: "localhost",
        [serverNameSymbol]: server,
        port: listener.addr.port,
      });
      const serverConn = await listener.accept();
      if (server.startsWith("fail-")) {
        await assertRejects(async () => await conn.handshake());
        await assertRejects(async () => await serverConn.handshake());
      } else {
        await conn.handshake();
        await serverConn.handshake();
      }
      conn.close();
      serverConn.close();
    }

    assertEquals(sniRequests, [
      "server-1",
      "server-2",
      "fail-server-3",
      "fail-server-4",
    ]);
    listener.close();
  },
);

Deno.test(
  { permissions: { net: true, read: true } },
  async function listenResolverDoesNotBlockUnrelatedSni() {
    const slowStarted = Promise.withResolvers<void>();
    const releaseSlow = Promise.withResolvers<void>();
    const handshakes: Promise<unknown>[] = [];
    const connections: { close(): void }[] = [];

    const listener = Deno.listenTls({
      hostname: "localhost",
      port: 0,
      async resolveCertificate(serverName) {
        if (serverName === "slow-server") {
          slowStarted.resolve();
          await releaseSlow.promise;
        }
        return { cert, key };
      },
    });

    try {
      const slowClient = await Deno.connectTls({
        hostname: "localhost",
        [serverNameSymbol]: "slow-server",
        port: listener.addr.port,
        caCerts: [caCert],
        unsafelyDisableHostnameVerification: true,
      });
      const slowServer = await listener.accept();
      connections.push(slowClient, slowServer);
      handshakes.push(slowClient.handshake(), slowServer.handshake());

      await slowStarted.promise;

      const fastClient = await Deno.connectTls({
        hostname: "localhost",
        [serverNameSymbol]: "fast-server",
        port: listener.addr.port,
        caCerts: [caCert],
        unsafelyDisableHostnameVerification: true,
      });
      const fastServer = await listener.accept();
      connections.push(fastClient, fastServer);
      const fastHandshake = Promise.all([
        fastClient.handshake(),
        fastServer.handshake(),
      ]);
      handshakes.push(fastHandshake);

      assertEquals(
        await resolvesWithin(fastHandshake, 1_000),
        "resolved",
      );

      releaseSlow.resolve();
      await Promise.all(handshakes);
    } finally {
      releaseSlow.resolve();
      listener.close();
      for (const conn of connections) {
        try {
          conn.close();
        } catch {
          // The connection may already have been closed by the handshake path.
        }
      }
      await Promise.allSettled(handshakes);
    }
  },
);

Deno.test(
  { permissions: { net: true, read: true } },
  async function listenResolverClientHello() {
    let clientHello: Deno.TlsClientHello | undefined;
    const listener = Deno.listenTls({
      hostname: "localhost",
      port: 0,
      resolveCertificate(_serverName, hello) {
        clientHello = hello;
        return { cert, key };
      },
    });

    const conn = await Deno.connectTls({
      hostname: "localhost",
      [serverNameSymbol]: "server-1",
      port: listener.addr.port,
      alpnProtocols: ["h2", "http/1.1"],
    } as Deno.ConnectTlsOptions);
    const serverConn = await listener.accept();
    await Promise.all([conn.handshake(), serverConn.handshake()]);
    conn.close();
    serverConn.close();
    listener.close();

    // The ClientHello carries the client's offer: the ALPN list it sent and
    // the raw IANA code points it advertised.
    assertEquals(clientHello!.alpnProtocols, ["h2", "http/1.1"]);
    assertEquals(clientHello!.cipherSuites.length > 0, true);
    assertEquals(clientHello!.signatureSchemes.length > 0, true);
    assertEquals(clientHello!.supportedGroups.length > 0, true);
    for (const code of clientHello!.cipherSuites) {
      assertEquals(typeof code, "number");
    }
  },
);

Deno.test(
  { permissions: { net: true, read: true } },
  async function listenResolverCachedByName() {
    let calls = 0;
    const listener = Deno.listenTls({
      hostname: "localhost",
      port: 0,
      resolveCertificate() {
        calls++;
        return { cert, key };
      },
    });

    async function connect() {
      const conn = await Deno.connectTls({
        hostname: "localhost",
        [serverNameSymbol]: "server-1",
        port: listener.addr.port,
      });
      const serverConn = await listener.accept();
      await Promise.all([conn.handshake(), serverConn.handshake()]);
      conn.close();
      serverConn.close();
    }

    // Resolutions are cached by server name, so connecting twice with the
    // same name only invokes the callback once.
    await connect();
    await connect();
    assertEquals(calls, 1);
    listener.close();
  },
);

Deno.test(
  { permissions: { net: true, read: true } },
  async function serveResolver() {
    const ac = new AbortController();
    const { promise, resolve } = Promise.withResolvers<number>();
    const serverNames: string[] = [];

    const server = Deno.serve({
      hostname: "localhost",
      port: 0,
      signal: ac.signal,
      onListen: ({ port }) => resolve(port),
      resolveCertificate(serverName) {
        serverNames.push(serverName);
        return { cert, key };
      },
    }, () => new Response("Hello SNI"));

    const port = await promise;
    const caCert = Deno.readTextFileSync("tests/testdata/tls/RootCA.pem");
    const client = Deno.createHttpClient({ caCerts: [caCert] });
    const resp = await fetch(`https://localhost:${port}/`, {
      client,
      headers: { "connection": "close" },
    });
    assertEquals(await resp.text(), "Hello SNI");

    client.close();
    ac.abort();
    await server.finished;

    assertEquals(serverNames, ["localhost"]);
  },
);
