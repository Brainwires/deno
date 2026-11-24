// Copyright 2018-2026 the Deno authors. MIT license.

// Identity for WebRTC 1.0 (ext/webrtc/06_identity.js): the RTCPeerConnection
// members, RTCIdentityAssertion, RTCConfiguration.peerIdentity, the IdP
// error details of RTCError and the validation of a=identity in
// setRemoteDescription().

import {
  assert,
  assertEquals,
  assertNotStrictEquals,
  assertRejects,
  assertStrictEquals,
  assertThrows,
} from "./test_util.ts";

function assertOperationError(error: unknown) {
  assert(error instanceof DOMException, "expected a DOMException");
  assertStrictEquals(error.constructor, DOMException);
  assertEquals(error.name, "OperationError");
}

// A remote offer with `line` added at session level.
async function offerWith(line: string): Promise<RTCSessionDescriptionInit> {
  const pc = new RTCPeerConnection();
  try {
    const offer = await pc.createOffer();
    const sdp = offer.sdp!;
    const at = sdp.indexOf("\r\nm=");
    return {
      type: "offer",
      sdp: at === -1
        ? `${sdp}${line}\r\n`
        : `${sdp.slice(0, at)}\r\n${line}${sdp.slice(at)}`,
    };
  } finally {
    pc.close();
  }
}

Deno.test("webrtcIdentityAssertion", () => {
  const assertion = new RTCIdentityAssertion("idp.example", "alice@example");
  assertEquals(assertion.idp, "idp.example");
  assertEquals(assertion.name, "alice@example");
  assertion.name = "bob@example";
  assertEquals(assertion.name, "bob@example");
  assertEquals(String(assertion), "[object RTCIdentityAssertion]");
  assertEquals(RTCIdentityAssertion.length, 2);
  assertThrows(
    // @ts-expect-error: too few arguments
    () => new RTCIdentityAssertion("idp.example"),
    TypeError,
  );
  const getter = Object.getOwnPropertyDescriptor(
    RTCIdentityAssertion.prototype,
    "idp",
  )!.get!;
  assertThrows(() => getter.call({}), TypeError);
});

Deno.test("webrtcIdentityMembers", () => {
  const pc = new RTCPeerConnection();
  try {
    assertEquals(pc.idpLoginUrl, null);
    assertEquals(pc.idpErrorInfo, null);
    assertStrictEquals(pc.peerIdentity, pc.peerIdentity);
    assert(pc.peerIdentity instanceof Promise);
    assertEquals(RTCPeerConnection.prototype.setIdentityProvider.length, 1);
    assertEquals(RTCPeerConnection.prototype.getIdentityAssertion.length, 0);
    assertThrows(
      () =>
        Object.getOwnPropertyDescriptor(
          RTCPeerConnection.prototype,
          "peerIdentity",
        )!.get!.call({}),
      TypeError,
    );
  } finally {
    pc.close();
  }
});

Deno.test("webrtcSetIdentityProviderArguments", () => {
  const pc = new RTCPeerConnection();
  // @ts-expect-error: too few arguments
  assertThrows(() => pc.setIdentityProvider(), TypeError);
  assertThrows(
    () => pc.setIdentityProvider("idp.example", { protocol: "a/b" }),
    DOMException,
    "must not include",
  );
  assertThrows(
    () => pc.setIdentityProvider("idp.example", { protocol: "a\\b" }),
    DOMException,
    "must not include",
  );
  pc.setIdentityProvider("idp.example", { protocol: "idp.js?x=1" });
  pc.close();
  assertThrows(
    () => pc.setIdentityProvider("idp.example"),
    DOMException,
    "closed",
  );
});

Deno.test("webrtcGetIdentityAssertionWithoutProvider", async () => {
  const pc = new RTCPeerConnection();
  try {
    const error = await assertRejects(() => pc.getIdentityAssertion());
    assert(error instanceof DOMException);
    assertEquals(error.name, "InvalidStateError");
  } finally {
    pc.close();
  }
});

Deno.test("webrtcRTCErrorIdpDetails", () => {
  const error = new RTCError({
    errorDetail: "idp-load-failure",
    httpRequestStatusCode: 404,
  }, "not found");
  assertEquals(error.errorDetail, "idp-load-failure");
  assertEquals(error.httpRequestStatusCode, 404);
  assertEquals(
    new RTCError({ errorDetail: "idp-need-login" }).httpRequestStatusCode,
    null,
  );
  assertThrows(
    // @ts-expect-error: not an RTCErrorDetailType
    () => new RTCError({ errorDetail: "idp-unknown" }),
    TypeError,
  );
});

Deno.test("webrtcConfigurationPeerIdentity", () => {
  const toStringThrows = {
    toString() {
      throw new RangeError("no");
    },
  };
  assertThrows(
    () =>
      new RTCPeerConnection({
        peerIdentity: toStringThrows as unknown as string,
      }),
    RangeError,
  );
  const pc = new RTCPeerConnection({ peerIdentity: "bob@example.org" });
  try {
    assertEquals(pc.getConfiguration().peerIdentity, "bob@example.org");
    // Leaving it out keeps the target peer identity.
    pc.setConfiguration({});
    assertEquals(pc.getConfiguration().peerIdentity, "bob@example.org");
    assertThrows(
      () => pc.setConfiguration({ peerIdentity: "carol@example.org" }),
      DOMException,
      "peerIdentity",
    );
  } finally {
    pc.close();
  }
  const plain = new RTCPeerConnection();
  assertEquals(plain.getConfiguration().peerIdentity, undefined);
  plain.close();
});

Deno.test("webrtcTargetPeerIdentityWithoutAssertion", async () => {
  const offer = await offerWith("a=x-nothing:1");
  const pc = new RTCPeerConnection({ peerIdentity: "bob@example.org" });
  try {
    const peerIdentity = pc.peerIdentity;
    const error = await assertRejects(() => pc.setRemoteDescription(offer));
    assertOperationError(error);
    assertStrictEquals(await assertRejects(() => peerIdentity), error);
    // The description was not applied, and peerIdentity stays rejected.
    assertEquals(pc.signalingState, "stable");
    assertStrictEquals(pc.peerIdentity, peerIdentity);
  } finally {
    pc.close();
  }
});

Deno.test("webrtcMalformedAssertionWithoutTarget", async () => {
  const offer = await offerWith("a=identity:!!!");
  const pc = new RTCPeerConnection();
  try {
    const first = pc.peerIdentity;
    // No target peer identity: setRemoteDescription doesn't wait for the
    // validation, which fails on its own.
    await pc.setRemoteDescription(offer);
    assertEquals(pc.signalingState, "have-remote-offer");
    assertOperationError(await assertRejects(() => first));
    // A new, pending peerIdentity for a later description.
    assertNotStrictEquals(pc.peerIdentity, first);
  } finally {
    pc.close();
  }
});

Deno.test(
  "webrtcIdentityProviderLoadFailure",
  { permissions: { net: ["127.0.0.1"] } },
  async () => {
    // A loopback port nothing listens on.
    const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
    const { port } = listener.addr as Deno.NetAddr;
    listener.close();
    const pc = new RTCPeerConnection();
    try {
      pc.setIdentityProvider(`127.0.0.1:${port}`, { protocol: "idp.js" });
      const error = await assertRejects(() => pc.getIdentityAssertion());
      assert(error instanceof RTCError, "expected an RTCError");
      assertEquals(error.errorDetail, "idp-load-failure");
      assertEquals(error.httpRequestStatusCode, null);
      // createOffer() rejects with a new OperationError.
      assertOperationError(await assertRejects(() => pc.createOffer()));
    } finally {
      pc.close();
    }
  },
);

Deno.test("webrtcTrackIsolationAttributes", () => {
  const descriptor = Object.getOwnPropertyDescriptor(
    MediaStreamTrack.prototype,
    "isolated",
  )!;
  assert(descriptor.get !== undefined && descriptor.set === undefined);
  assertThrows(() => descriptor.get!.call({}), TypeError);
  const handler = Object.getOwnPropertyDescriptor(
    MediaStreamTrack.prototype,
    "onisolationchange",
  )!;
  assert(handler.get !== undefined && handler.set !== undefined);
});

// The IdP proxy is remote code: its worker runs with no permission at all,
// whatever the page has (this test runs with -A). Each probe must be
// refused, and the assertion it returns must still reach the page.
Deno.test("webrtcIdentityProviderWorkerHasNoPermissions", async () => {
  const cert = Deno.readTextFileSync("tests/testdata/tls/localhost.crt");
  const key = Deno.readTextFileSync("tests/testdata/tls/localhost.key");
  const readPath = Deno.realPathSync("tests/testdata/tls/localhost.crt");
  const dir = Deno.makeTempDirSync({ prefix: "webrtc-idp-" });
  const writePath = `${dir}/written-by-idp.txt`;
  const modulePath = `${dir}/module.js`;
  Deno.writeTextFileSync(modulePath, "export const loaded = true;\n");
  // The page itself may do each of these (so a refusal below is the
  // worker's own): it reads the module it offers the IdP.
  assertEquals(Deno.readTextFileSync(modulePath).length > 0, true);
  const { promise: listening, resolve: onListen } = Promise.withResolvers<
    number
  >();
  let proxyLoads = 0;
  let otherRequests = 0;
  const server = Deno.serve({
    hostname: "localhost",
    port: 0,
    cert,
    key,
    onListen: ({ port }) => onListen(port),
  }, (request) => {
    const { pathname } = new URL(request.url);
    if (pathname !== "/.well-known/idp-proxy/idp.js") {
      otherRequests++;
      return new Response("not for the IdP", { status: 200 });
    }
    proxyLoads++;
    return new Response(idpProxy, {
      headers: { "content-type": "application/javascript" },
    });
  });
  const port = await listening;
  const paths = JSON.stringify({
    read: readPath,
    write: writePath,
    ffi: `${dir}/libnothing.so`,
    module: new URL(
      `file://${Deno.build.os === "windows" ? "/" : ""}${
        modulePath.replaceAll("\\", "/")
      }`,
    ).href,
    url: `https://localhost:${port}/from-the-idp`,
    port,
  });
  // A classic script, run by the proxy's worker; each probe reports the
  // error's name and message, or "allowed".
  const idpProxy = `
    var paths = ${paths};
    function describe(error) {
      return {
        name: error && error.name ? String(error.name) : typeof error,
        message: error && error.message ? String(error.message) : String(error),
      };
    }
    async function probe(fn) {
      try {
        await fn();
        return { name: "allowed", message: "" };
      } catch (error) {
        return describe(error);
      }
    }
    rtcIdentityProvider.register({
      async generateAssertion(contents, origin, options) {
        var probes = {
          read: await probe(function () {
            return Deno.readTextFileSync(paths.read);
          }),
          write: await probe(function () {
            Deno.writeTextFileSync(paths.write, "x");
          }),
          fetch: await probe(function () {
            return fetch(paths.url).then(function (r) { return r.text(); });
          }),
          connect: await probe(function () {
            return Deno.connect({ hostname: "127.0.0.1", port: paths.port })
              .then(function (conn) { conn.close(); });
          }),
          env: await probe(function () {
            return Deno.env.get("PATH");
          }),
          run: await probe(function () {
            return new Deno.Command("deno", { args: ["--version"] })
              .outputSync();
          }),
          ffi: await probe(function () {
            return Deno.dlopen(paths.ffi, {});
          }),
          sys: await probe(function () {
            return Deno.hostname();
          }),
          importFile: await probe(function () {
            return import(paths.module);
          }),
        };
        return {
          idp: { domain: location.host, protocol: "idp.js" },
          assertion: JSON.stringify(probes),
        };
      },
      validateAssertion(assertion, origin) {
        return { identity: "nobody@localhost", contents: "" };
      },
    });
  `;
  const pc = new RTCPeerConnection();
  try {
    pc.setIdentityProvider(`localhost:${port}`, { protocol: "idp.js" });
    // The assertion still completes: base64 of { idp, assertion }.
    const value = await pc.getIdentityAssertion();
    const { idp, assertion } = JSON.parse(atob(value));
    assertEquals(idp, { domain: `localhost:${port}`, protocol: "idp.js" });
    const probes = JSON.parse(assertion) as Record<
      string,
      { name: string; message: string }
    >;
    for (
      const name of [
        "read",
        "write",
        "fetch",
        "connect",
        "env",
        "run",
        "ffi",
        "sys",
      ]
    ) {
      assertEquals(
        probes[name].name,
        "NotCapable",
        `the IdP worker's ${name} probe: ${JSON.stringify(probes[name])}`,
      );
    }
    // A file: module is refused too (no read permission, and a remote
    // script may not load a local module).
    assert(
      probes.importFile.name !== "allowed",
      `the IdP worker imported a file: module: ${
        JSON.stringify(probes.importFile)
      }`,
    );
    assertEquals(proxyLoads, 1);
    // the refused fetch never reached the server
    assertEquals(otherRequests, 0);
    let written = true;
    try {
      Deno.statSync(writePath);
    } catch {
      written = false;
    }
    assertEquals(written, false, "the IdP worker wrote a file");
  } finally {
    pc.close();
    await server.shutdown();
    Deno.removeSync(dir, { recursive: true });
  }
});
