// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC data channels (DENEXT.md item 364): `RTCPeerConnection`,
// `RTCDataChannel` and friends as globals. Two peers in one isolate connect
// over loopback (no STUN). Every test fails on a build without the API
// (`RTCPeerConnection is not defined`).

import {
  assert,
  assertEquals,
  assertRejects,
  assertThrows,
} from "./test_util.ts";

function once<T extends Event = Event>(
  target: EventTarget,
  type: string,
  timeoutMs = 20_000,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`timed out waiting for '${type}'`)),
      timeoutMs,
    );
    target.addEventListener(type, (e) => {
      clearTimeout(timer);
      resolve(e as T);
    }, { once: true });
  });
}

/** Forwards trickled candidates, holding them until the remote side has a
 * remote description. */
function trickle(from: RTCPeerConnection, to: RTCPeerConnection) {
  const pending: RTCIceCandidateInit[] = [];
  let ready = false;
  from.addEventListener("icecandidate", (e) => {
    const candidate = (e as RTCPeerConnectionIceEvent).candidate;
    if (!candidate) return;
    if (ready) to.addIceCandidate(candidate);
    else pending.push(candidate.toJSON());
  });
  return () => {
    ready = true;
    for (const c of pending.splice(0)) to.addIceCandidate(c);
  };
}

async function connectPair(
  init: RTCDataChannelInit = {},
  config?: RTCConfiguration,
) {
  const a = new RTCPeerConnection(config);
  const b = new RTCPeerConnection(config);
  const toB = trickle(a, b);
  const toA = trickle(b, a);
  const channel = a.createDataChannel("chat", init);
  const remoteChannel = once<RTCDataChannelEvent>(b, "datachannel");
  const opened = once(channel, "open");
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  toB();
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription!);
  toA();
  const remote = (await remoteChannel).channel;
  await opened;
  if (remote.readyState !== "open") await once(remote, "open");
  return { a, b, channel, remote };
}

function messages(channel: RTCDataChannel, count: number) {
  return new Promise<unknown[]>((resolve, reject) => {
    const got: unknown[] = [];
    const timer = setTimeout(
      () => reject(new Error(`got ${got.length} of ${count} messages`)),
      20_000,
    );
    channel.addEventListener("message", (e) => {
      got.push((e as MessageEvent).data);
      if (got.length === count) {
        clearTimeout(timer);
        resolve(got);
      }
    });
  });
}

Deno.test("webrtcGlobals", () => {
  assertEquals(typeof RTCPeerConnection, "function");
  assertEquals(typeof RTCSessionDescription, "function");
  assertEquals(typeof RTCIceCandidate, "function");
  assertEquals(typeof RTCDataChannel, "function");
  // deno-lint-ignore no-explicit-any
  assertThrows(() => new (RTCDataChannel as any)(), TypeError);
  // deno-lint-ignore no-explicit-any
  assertThrows(() => new (RTCStatsReport as any)(), TypeError);
  assertEquals(
    Object.getOwnPropertyDescriptor(globalThis, "RTCPeerConnection")
      ?.enumerable,
    false,
  );
});

Deno.test("webrtcSessionDescription", () => {
  const d = new RTCSessionDescription({ type: "offer", sdp: "v=0\r\n" });
  assertEquals(d.type, "offer");
  assertEquals(d.sdp, "v=0\r\n");
  assertEquals(JSON.parse(JSON.stringify(d)), {
    type: "offer",
    sdp: "v=0\r\n",
  });
  assertThrows(
    () => new RTCSessionDescription({ type: "bogus" as RTCSdpType }),
    TypeError,
  );
});

Deno.test("webrtcIceCandidateParsing", () => {
  const c = new RTCIceCandidate({
    candidate:
      "candidate:842163049 1 udp 1677729535 203.0.113.7 46154 typ srflx raddr 10.0.0.2 rport 5000",
    sdpMid: "0",
    sdpMLineIndex: 0,
  });
  assertEquals(c.foundation, "842163049");
  assertEquals(c.component, "rtp");
  assertEquals(c.protocol, "udp");
  assertEquals(c.priority, 1677729535);
  assertEquals(c.address, "203.0.113.7");
  assertEquals(c.port, 46154);
  assertEquals(c.type, "srflx");
  assertEquals(c.relatedAddress, "10.0.0.2");
  assertEquals(c.relatedPort, 5000);
  assertEquals(c.toJSON().sdpMid, "0");
  assertThrows(() => new RTCIceCandidate({ candidate: "x" }), TypeError);
});

Deno.test("webrtcConfigurationValidation", () => {
  assertThrows(
    () => new RTCPeerConnection({ iceServers: [{ urls: "http://x" }] }),
    DOMException,
  );
  const noCreds = assertThrows(
    () =>
      new RTCPeerConnection({
        iceServers: [{ urls: "turn:turn.example.org" }],
      }),
    DOMException,
  );
  assertEquals((noCreds as DOMException).name, "InvalidAccessError");
  assertThrows(
    () =>
      new RTCPeerConnection({
        iceTransportPolicy: "none" as RTCIceTransportPolicy,
      }),
    TypeError,
  );
  const pc = new RTCPeerConnection({
    iceServers: [{ urls: ["stun:stun.example.org:3478"] }],
  });
  assertEquals(
    pc.getConfiguration().iceServers?.[0].urls,
    ["stun:stun.example.org:3478"],
  );
  assertEquals(pc.signalingState, "stable");
  assertEquals(pc.connectionState, "new");
  assertEquals(pc.localDescription, null);
  pc.close();
});

Deno.test("webrtcCreateDataChannelValidation", () => {
  const pc = new RTCPeerConnection();
  assertThrows(
    () =>
      pc.createDataChannel("x", { maxRetransmits: 1, maxPacketLifeTime: 1 }),
    TypeError,
  );
  assertThrows(
    () => pc.createDataChannel("x", { negotiated: true }),
    TypeError,
  );
  const dc = pc.createDataChannel("x", { protocol: "p" });
  assertEquals(dc.label, "x");
  assertEquals(dc.protocol, "p");
  assertEquals(dc.ordered, true);
  assertEquals(dc.readyState, "connecting");
  assertEquals(dc.binaryType, "arraybuffer");
  assertThrows(() => dc.send("early"), DOMException);
  pc.close();
  assertEquals(pc.signalingState, "closed");
  assertThrows(() => pc.createDataChannel("y"), DOMException);
});

Deno.test("webrtcClosedConnectionRejects", async () => {
  const pc = new RTCPeerConnection();
  pc.close();
  pc.close(); // idempotent
  await assertRejects(() => pc.createOffer(), DOMException);
  await assertRejects(() => pc.setLocalDescription(), DOMException);
  await assertRejects(() => pc.getStats(), DOMException);
});

Deno.test("webrtcLoopbackBothWays", async () => {
  const { a, b, channel, remote } = await connectPair();
  assertEquals(remote.label, "chat");
  assertEquals(remote.id, channel.id);
  assert(channel.id !== null);
  assertEquals(a.connectionState, "connected");
  assertEquals(b.connectionState, "connected");
  assertEquals(a.signalingState, "stable");

  const toB = messages(remote, 2);
  const toA = messages(channel, 1);
  channel.send("hello from a");
  channel.send(new Uint8Array([1, 2, 3]));
  remote.send("hello from b");
  const [text, bytes] = await toB;
  assertEquals(text, "hello from a");
  assert(bytes instanceof ArrayBuffer);
  assertEquals([...new Uint8Array(bytes as ArrayBuffer)], [1, 2, 3]);
  assertEquals(await toA, ["hello from b"]);

  // The local description carries the gathered candidates.
  assert(a.localDescription!.sdp.includes("a=candidate:"));
  assert(a.sctp !== null && a.sctp.maxMessageSize >= 65536);
  a.close();
  b.close();
});

Deno.test("webrtcNonTrickleOfferAnswer", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const channel = a.createDataChannel("full");
  const gathered = (pc: RTCPeerConnection) =>
    pc.iceGatheringState === "complete"
      ? Promise.resolve()
      : new Promise<void>((resolve) => {
        pc.addEventListener("icecandidate", (e) => {
          if (!(e as RTCPeerConnectionIceEvent).candidate) resolve();
        });
      });
  await a.setLocalDescription(await a.createOffer());
  await gathered(a);
  await b.setRemoteDescription(a.localDescription!);
  await b.setLocalDescription(await b.createAnswer());
  await gathered(b);
  const dc = once<RTCDataChannelEvent>(b, "datachannel");
  await a.setRemoteDescription(b.localDescription!);
  await once(channel, "open");
  assertEquals((await dc).channel.label, "full");
  a.close();
  b.close();
});

Deno.test("webrtcBinaryKinds", async () => {
  const { a, b, channel, remote } = await connectPair();
  const got = messages(remote, 5);
  const view = new Uint8Array([9, 9, 1, 2, 9]).subarray(2, 4);
  channel.send(new Uint8Array([1, 2]).buffer);
  channel.send(view);
  channel.send(new DataView(new Uint8Array([3, 4, 5]).buffer, 1, 2));
  channel.send(new Blob([new Uint8Array([6, 7])]));
  // Sent after the Blob, delivered after it too.
  channel.send("after-blob");
  const [ab, sub, dv, blob, text] = await got;
  assertEquals([...new Uint8Array(ab as ArrayBuffer)], [1, 2]);
  assertEquals([...new Uint8Array(sub as ArrayBuffer)], [1, 2]);
  assertEquals([...new Uint8Array(dv as ArrayBuffer)], [4, 5]);
  assertEquals([...new Uint8Array(blob as ArrayBuffer)], [6, 7]);
  assertEquals(text, "after-blob");

  remote.binaryType = "blob";
  const asBlob = messages(remote, 1);
  channel.send(new Uint8Array([42]));
  const [received] = await asBlob;
  assert(received instanceof Blob);
  assertEquals([...new Uint8Array(await (received as Blob).arrayBuffer())], [
    42,
  ]);
  a.close();
  b.close();
});

Deno.test("webrtcLargeAndOrderedMessages", async () => {
  const { a, b, channel, remote } = await connectPair();
  const max = a.sctp!.maxMessageSize;
  const big = new Uint8Array(max);
  for (let i = 0; i < big.length; i++) big[i] = i % 251;
  assertThrows(() => channel.send(new Uint8Array(max + 1)), TypeError);
  const got = messages(remote, 201);
  channel.send(big);
  for (let i = 0; i < 200; i++) channel.send(String(i));
  const [first, ...rest] = await got;
  const received = new Uint8Array(first as ArrayBuffer);
  assertEquals(received.length, big.length);
  assertEquals(received, big);
  assertEquals(rest, Array.from({ length: 200 }, (_, i) => String(i)));
  a.close();
  b.close();
});

Deno.test("webrtcUnorderedUnreliableChannel", async () => {
  const { a, b, channel, remote } = await connectPair({
    ordered: false,
    maxRetransmits: 0,
  });
  assertEquals(channel.ordered, false);
  assertEquals(channel.maxRetransmits, 0);
  assertEquals(remote.ordered, false);
  assertEquals(remote.maxRetransmits, 0);
  const got = messages(remote, 50);
  for (let i = 0; i < 50; i++) channel.send(String(i));
  // Loopback loses nothing; arrival order is not asserted.
  const values = ((await got) as string[]).map(Number).sort((x, y) => x - y);
  assertEquals(values, Array.from({ length: 50 }, (_, i) => i));
  a.close();
  b.close();
});

Deno.test("webrtcNegotiatedChannel", async () => {
  const { a, b, channel } = await connectPair();
  const x = a.createDataChannel("side", { negotiated: true, id: 7 });
  const y = b.createDataChannel("side", { negotiated: true, id: 7 });
  assertEquals(x.id, 7);
  if (x.readyState !== "open") await once(x, "open");
  if (y.readyState !== "open") await once(y, "open");
  const got = messages(y, 1);
  x.send("negotiated");
  assertEquals(await got, ["negotiated"]);
  assertEquals(channel.readyState, "open");
  a.close();
  b.close();
});

Deno.test("webrtcBufferedAmount", async () => {
  const { a, b, channel } = await connectPair();
  channel.bufferedAmountLowThreshold = 1024;
  assertEquals(channel.bufferedAmountLowThreshold, 1024);
  const chunk = new Uint8Array(16 * 1024);
  for (let i = 0; i < 64; i++) channel.send(chunk);
  assert(channel.bufferedAmount > 1024, `${channel.bufferedAmount}`);
  await once(channel, "bufferedamountlow");
  assert(channel.bufferedAmount <= 1024, `${channel.bufferedAmount}`);
  a.close();
  b.close();
});

Deno.test("webrtcChannelClose", async () => {
  const { a, b, channel, remote } = await connectPair();
  const remoteClosed = once(remote, "close");
  const localClosed = once(channel, "close");
  channel.close();
  assertEquals(channel.readyState, "closing");
  await localClosed;
  await remoteClosed;
  assertEquals(channel.readyState, "closed");
  assertEquals(remote.readyState, "closed");
  assertThrows(() => channel.send("late"), DOMException);
  assertEquals(a.connectionState, "connected");
  a.close();
  b.close();
});

Deno.test("webrtcConnectionCloseClosesChannels", async () => {
  const { a, b, channel } = await connectPair();
  const closed = once(channel, "close");
  a.close();
  assertEquals(a.connectionState, "closed");
  assertEquals(a.iceConnectionState, "closed");
  assertEquals(channel.readyState, "closed");
  await closed;
  b.close();
});

Deno.test("webrtcGetStats", async () => {
  const { a, b } = await connectPair();
  const report = await a.getStats();
  assert(report instanceof RTCStatsReport);
  const types = new Set([...report.values()].map((s) => s.type));
  assert(types.has("data-channel"), [...types].join(","));
  assert(types.has("peer-connection"), [...types].join(","));
  for (const [id, stats] of report) assertEquals(stats.id, id);
  a.close();
  b.close();
});

Deno.test(
  "webrtcLeastPrivilegeLoopback",
  { permissions: { net: ["127.0.0.1"], sys: false } },
  async () => {
    // Without interface enumeration only loopback candidates are gathered
    // (reported as an icecandidateerror), which is enough on one machine.
    const { a, b, channel, remote } = await connectPair();
    assert(!a.localDescription!.sdp.includes("typ srflx"));
    const got = messages(remote, 1);
    channel.send("scoped");
    assertEquals(await got, ["scoped"]);
    a.close();
    b.close();
  },
);

Deno.test(
  "webrtcIceServerNeedsNetPermission",
  { permissions: { net: ["127.0.0.1"] } },
  () => {
    assertThrows(
      () =>
        new RTCPeerConnection({
          iceServers: [{ urls: "stun:stun.example.org" }],
        }),
      Deno.errors.NotCapable,
    );
  },
);

Deno.test(
  "webrtcRefusedRemoteCandidateRejects",
  { permissions: { net: ["127.0.0.1"] } },
  async () => {
    const a = new RTCPeerConnection();
    const b = new RTCPeerConnection();
    a.createDataChannel("x");
    await a.setLocalDescription();
    await b.setRemoteDescription(a.localDescription!);
    await assertRejects(
      () =>
        b.addIceCandidate({
          candidate: "candidate:1 1 udp 2130706431 192.0.2.1 9999 typ host",
          sdpMid: "0",
        }),
      Deno.errors.NotCapable,
    );
    // mDNS (.local) and TCP candidates are accepted and skipped.
    await b.addIceCandidate({
      candidate: "candidate:2 1 udp 2130706431 abcd.local 9999 typ host",
      sdpMid: "0",
    });
    await b.addIceCandidate({ candidate: "", sdpMid: "0" });
    await b.addIceCandidate(undefined);
    const sdp = a.localDescription!.sdp.replace(
      /\r\n$/,
      "\r\na=candidate:3 1 udp 2130706431 192.0.2.1 9999 typ host\r\n",
    );
    const c = new RTCPeerConnection();
    await assertRejects(
      () => c.setRemoteDescription({ type: "offer", sdp }),
      Deno.errors.NotCapable,
    );
    a.close();
    b.close();
    c.close();
  },
);
