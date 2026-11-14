// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC data channels (DENEXT.md item 370): `RTCPeerConnection`,
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
  // getStats() still resolves after close(), as the spec now has it.
  assert((await pc.getStats()) instanceof RTCStatsReport);
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
  // createOffer / createAnswer resolve to plain, editable dictionaries.
  const offer = await a.createOffer();
  assertEquals(Object.getPrototypeOf(offer), Object.prototype);
  offer.sdp = offer.sdp!;
  await a.setLocalDescription(offer);
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
  assertEquals(max, 262144);
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

// --- Standard surface beyond data channels (certificates, media, transports)

async function negotiate(a: RTCPeerConnection, b: RTCPeerConnection) {
  const toB = trickle(a, b);
  const toA = trickle(b, a);
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  toB();
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription!);
  toA();
}

function connected(pc: RTCPeerConnection) {
  return pc.connectionState === "connected"
    ? Promise.resolve()
    : new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`still ${pc.connectionState}`)),
        20_000,
      );
      pc.addEventListener("connectionstatechange", () => {
        if (pc.connectionState === "connected") {
          clearTimeout(timer);
          resolve();
        }
      });
    });
}

/** The next `count` RTP packets of a receiver, header included. */
async function rtpPackets(receiver: RTCRtpReceiver, count: number) {
  const reader = Deno.webrtc.readRtp(receiver).getReader();
  const got: Uint8Array[] = [];
  const timer = setTimeout(() => reader.cancel(), 20_000);
  while (got.length < count) {
    const { value, done } = await reader.read();
    if (done) break;
    got.push(value);
  }
  clearTimeout(timer);
  await reader.cancel();
  return got;
}

Deno.test("webrtcMediaGlobals", () => {
  for (
    const name of [
      "MediaStream",
      "MediaStreamTrack",
      "MediaStreamTrackEvent",
      "RTCCertificate",
      "RTCDtlsTransport",
      "RTCIceTransport",
      "RTCRtpReceiver",
      "RTCRtpSender",
      "RTCRtpTransceiver",
      "RTCTrackEvent",
    ]
  ) {
    assertEquals(
      typeof (globalThis as Record<string, unknown>)[name],
      "function",
    );
  }
  // deno-lint-ignore no-explicit-any
  assertThrows(() => new (MediaStreamTrack as any)(), TypeError);
  // deno-lint-ignore no-explicit-any
  assertThrows(() => new (RTCRtpSender as any)(), TypeError);
  assertEquals(typeof Deno.webrtc.EncodedMediaTrack, "function");
});

Deno.test("webrtcGenerateCertificate", async () => {
  const ecdsa = await RTCPeerConnection.generateCertificate(
    {
      name: "ECDSA",
      namedCurve: "P-256",
    } as AlgorithmIdentifier & RTCCertificateKeygenAlgorithm,
  );
  assert(ecdsa instanceof RTCCertificate);
  const thirtyDays = 30 * 24 * 3600 * 1000;
  assert(Math.abs(ecdsa.expires - (Date.now() + thirtyDays)) < 60_000);
  const [fp] = ecdsa.getFingerprints();
  assertEquals(fp.algorithm, "sha-256");
  assert(/^([0-9a-f]{2}:){31}[0-9a-f]{2}$/.test(fp.value!));

  const rsa = await RTCPeerConnection.generateCertificate({
    name: "RSASSA-PKCS1-v1_5",
    modulusLength: 2048,
    publicExponent: new Uint8Array([1, 0, 1]),
    hash: "SHA-256",
    expires: 1000,
  });
  assert(rsa.expires <= Date.now() + 1000);
  await assertRejects(
    () =>
      RTCPeerConnection.generateCertificate({
        name: "ECDSA",
        namedCurve: "P-521",
      }),
    DOMException,
  );
  await assertRejects(
    () => RTCPeerConnection.generateCertificate({ name: "AES-GCM" }),
    DOMException,
  );
});

Deno.test("webrtcCertificatesConfiguration", async () => {
  const cert = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
  });
  const rsa = await RTCPeerConnection.generateCertificate({
    name: "RSASSA-PKCS1-v1_5",
    modulusLength: 2048,
    publicExponent: new Uint8Array([1, 0, 1]),
    hash: "SHA-256",
  });
  const a = new RTCPeerConnection({ certificates: [cert] });
  const b = new RTCPeerConnection({ certificates: [rsa] });
  assertEquals(a.getConfiguration().certificates, [cert]);
  // The local description's fingerprint is the given certificate's.
  const channel = a.createDataChannel("c");
  await negotiate(a, b);
  const fp = cert.getFingerprints()[0].value!.toUpperCase();
  assert(a.localDescription!.sdp.toUpperCase().includes(fp));
  const rsaFp = rsa.getFingerprints()[0].value!.toUpperCase();
  assert(b.localDescription!.sdp.toUpperCase().includes(rsaFp));
  // An ECDSA and an RSA peer complete DTLS.
  await once(channel, "open");
  // A connection's certificates can't change.
  const other = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
  });
  const err = assertThrows(
    () => a.setConfiguration({ certificates: [other] }),
    DOMException,
  );
  assertEquals((err as DOMException).name, "InvalidModificationError");
  a.close();
  b.close();

  const expired = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
    expires: 0,
  });
  const e = assertThrows(
    () => new RTCPeerConnection({ certificates: [expired] }),
    DOMException,
  );
  assertEquals((e as DOMException).name, "InvalidAccessError");
});

Deno.test("webrtcCertificateExportImport", async () => {
  const cert = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
  });
  const pem = Deno.webrtc.exportCertificate(cert);
  assert(pem.includes("-----BEGIN CERTIFICATE-----"));
  const back = Deno.webrtc.importCertificate(pem);
  assertEquals(back.getFingerprints(), cert.getFingerprints());
  assertEquals(
    Math.floor(back.expires / 1000),
    Math.floor(cert.expires / 1000),
  );
  assertThrows(() => Deno.webrtc.importCertificate("nope"), DOMException);
});

Deno.test("webrtcAddTransceiver", async () => {
  const pc = new RTCPeerConnection();
  assertThrows(
    () => pc.addTransceiver("text" as "audio"),
    TypeError,
  );
  assertThrows(
    () =>
      pc.addTransceiver("audio", {
        direction: "upwards" as RTCRtpTransceiverDirection,
      }),
    TypeError,
  );
  assertThrows(
    () =>
      pc.addTransceiver("video", {
        sendEncodings: [{ scaleResolutionDownBy: 0.5 }],
      }),
    RangeError,
  );
  assertThrows(
    () =>
      pc.addTransceiver("video", {
        sendEncodings: [{ rid: "a" }, { rid: "a" }],
      }),
    TypeError,
  );
  const audio = pc.addTransceiver("audio");
  const video = pc.addTransceiver("video", { direction: "recvonly" });
  assertEquals(pc.getTransceivers(), [audio, video]);
  assertEquals(audio.direction, "sendrecv");
  assertEquals(video.direction, "recvonly");
  assertEquals(audio.currentDirection, null);
  assertEquals(audio.mid, null);
  assertEquals(audio.sender.track, null);
  assertEquals(audio.receiver.track.kind, "audio");
  assertEquals(audio.receiver.track.muted, true);
  assertEquals(audio.receiver.track.readyState, "live");
  assertEquals(pc.getSenders(), [audio.sender, video.sender]);
  assertEquals(pc.getReceivers(), [audio.receiver, video.receiver]);
  const offer = await pc.createOffer();
  assert(offer.sdp!.includes("m=audio"));
  assert(offer.sdp!.includes("m=video"));
  assert(offer.sdp!.includes("a=recvonly"));
  await pc.setLocalDescription(offer);
  assertEquals(audio.mid !== null, true);
  // direction can change; "stopped" is not settable.
  video.direction = "inactive";
  assertEquals(video.direction, "inactive");
  assertThrows(() => {
    video.direction = "stopped";
  }, TypeError);
  pc.close();
});

Deno.test("webrtcNegotiationNeededOnce", async () => {
  const pc = new RTCPeerConnection();
  let count = 0;
  pc.onnegotiationneeded = () => count++;
  pc.addTransceiver("audio");
  pc.addTransceiver("video");
  await new Promise((r) => setTimeout(r, 50));
  assertEquals(count, 1);
  pc.close();
});

Deno.test("webrtcCapabilities", () => {
  const video = RTCRtpSender.getCapabilities("video")!;
  const mimes = video.codecs.map((c) => c.mimeType.toLowerCase());
  for (const m of ["video/vp8", "video/vp9", "video/h264", "video/av1"]) {
    assert(mimes.includes(m), `${m} in ${mimes}`);
  }
  assert(video.codecs.every((c) => c.clockRate === 90000));
  const audio = RTCRtpReceiver.getCapabilities("audio")!;
  const opus = audio.codecs.find((c) =>
    c.mimeType.toLowerCase() === "audio/opus"
  )!;
  assertEquals(opus.clockRate, 48000);
  assertEquals(opus.channels, 2);
  assertEquals(RTCRtpSender.getCapabilities("text"), null);
  assert(Array.isArray(video.headerExtensions));
});

Deno.test("webrtcSetCodecPreferences", async () => {
  const pc = new RTCPeerConnection();
  const t = pc.addTransceiver("video");
  const err = assertThrows(
    () => t.setCodecPreferences([{ mimeType: "video/nope", clockRate: 90000 }]),
    DOMException,
  );
  assertEquals((err as DOMException).name, "InvalidModificationError");
  const vp9 = RTCRtpReceiver.getCapabilities("video")!.codecs.filter((c) =>
    c.mimeType.toLowerCase() === "video/vp9"
  );
  t.setCodecPreferences(vp9);
  const offer = await pc.createOffer();
  const mline = offer.sdp!.split("\r\n").find((l) => l.startsWith("m=video"))!;
  const pts = mline.split(" ").slice(3);
  const rtpmap = (pt: string) =>
    offer.sdp!.split("\r\n").find((l) => l.startsWith(`a=rtpmap:${pt} `));
  assert(rtpmap(pts[0])!.toLowerCase().includes("vp9"), mline);
  t.setCodecPreferences([]);
  pc.close();
});

Deno.test("webrtcEncodedMediaTrack", () => {
  const track = new Deno.webrtc.EncodedMediaTrack({
    kind: "video",
    mimeType: "video/VP8",
    label: "camera",
  });
  assert(track instanceof MediaStreamTrack);
  assertEquals(track.kind, "video");
  assertEquals(track.label, "camera");
  assertEquals(track.readyState, "live");
  assertEquals(track.codec.clockRate, 90000);
  assertThrows(
    () =>
      new Deno.webrtc.EncodedMediaTrack({
        kind: "audio",
        mimeType: "video/VP8",
      }),
    TypeError,
  );
  // Not sent anywhere yet.
  assertEquals(track.writeFrame(new Uint8Array(10)), 0);
  const clone = track.clone();
  assert(clone instanceof Deno.webrtc.EncodedMediaTrack);
  assert(clone.id !== track.id);
  track.stop();
  assertEquals(track.readyState, "ended");
  assertEquals(clone.readyState, "live");
});

Deno.test("webrtcMediaStream", () => {
  const t1 = new Deno.webrtc.EncodedMediaTrack({
    kind: "audio",
    mimeType: "audio/opus",
  });
  const t2 = new Deno.webrtc.EncodedMediaTrack({
    kind: "video",
    mimeType: "video/VP8",
  });
  const s = new MediaStream([t1]);
  assert(s.id.length > 0);
  s.addTrack(t2);
  s.addTrack(t2);
  assertEquals(s.getTracks(), [t1, t2]);
  assertEquals(s.getAudioTracks(), [t1]);
  assertEquals(s.getVideoTracks(), [t2]);
  assertEquals(s.getTrackById(t2.id), t2);
  assertEquals(s.active, true);
  s.removeTrack(t1);
  assertEquals(s.getTracks(), [t2]);
  assertEquals(new MediaStream(s).getTracks(), [t2]);
  t2.stop();
  assertEquals(s.active, false);
});

Deno.test("webrtcVideoFramesEndToEnd", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const track = new Deno.webrtc.EncodedMediaTrack({
    kind: "video",
    mimeType: "video/VP8",
  });
  const stream = new MediaStream([track]);
  const sender = a.addTrack(track, stream);
  assertEquals(sender.track, track);
  const gotTrack = once<RTCTrackEvent>(b, "track");
  await negotiate(a, b);
  const ev = await gotTrack;
  assertEquals(ev.track.kind, "video");
  assertEquals(ev.streams.length, 1);
  assertEquals(ev.streams[0].id, stream.id);
  assertEquals(ev.transceiver.receiver, ev.receiver);
  assertEquals(ev.transceiver.currentDirection, "recvonly");
  assertEquals(a.getTransceivers()[0].currentDirection, "sendonly");
  await connected(a);
  await connected(b);
  assertEquals(sender.transport!.state, "connected");
  assertEquals(sender.transport, a.sctp?.transport ?? sender.transport);

  const unmuted = once(ev.track, "unmute");
  const packets = rtpPackets(ev.receiver, 3);
  // A VP8 key frame is opaque to the packetizer: any bytes go.
  const frame = new Uint8Array(3000).map((_, i) => i & 0xff);
  const interval = setInterval(
    () => track.writeFrame(frame, { duration: 33 }),
    30,
  );
  try {
    const got = await packets;
    await unmuted;
    assertEquals(ev.track.muted, false);
    // RTP version 2, the negotiated VP8 payload type.
    const params = ev.receiver.getParameters();
    const vp8 = params.codecs.find((c) =>
      c.mimeType.toLowerCase() === "video/vp8"
    )!;
    for (const p of got) {
      assertEquals(p[0] >> 6, 2);
      assertEquals(p[1] & 0x7f, vp8.payloadType);
    }
  } finally {
    clearInterval(interval);
  }
  const outbound = [...(await a.getStats(track)).values()].filter((s) =>
    s.type === "outbound-rtp"
  );
  assertEquals(outbound.length, 1);
  assert(outbound[0].packetsSent > 0);
  const inbound = [...(await ev.receiver.getStats()).values()].filter((s) =>
    s.type === "inbound-rtp"
  );
  assertEquals(inbound.length, 1);
  assert(ev.receiver.getSynchronizationSources().length > 0);
  a.close();
  b.close();
});

Deno.test("webrtcRtpAndKeyFrameRequests", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const track = new Deno.webrtc.EncodedMediaTrack({
    kind: "audio",
    mimeType: "audio/opus",
  });
  a.addTransceiver(track, { direction: "sendonly" });
  const gotTrack = once<RTCTrackEvent>(b, "track");
  await negotiate(a, b);
  const { receiver } = await gotTrack;
  await connected(a);
  // A finished RTP packet: SSRC and payload type are replaced by the
  // sender's.
  const packet = new Uint8Array(12 + 4);
  packet[0] = 0x80;
  packet[1] = 99;
  packet.set([1, 2, 3, 4], 12);
  const packets = rtpPackets(receiver, 2);
  let seq = 0;
  const interval = setInterval(() => {
    packet[2] = 0;
    packet[3] = seq++;
    track.writeRtp(packet);
  }, 20);
  try {
    const got = await packets;
    assertEquals([...got[0].subarray(12)], [1, 2, 3, 4]);
  } finally {
    clearInterval(interval);
  }
  // The receiver asks the sender for a key frame.
  const asked = once(track, "keyframerequest");
  assert(Deno.webrtc.requestKeyFrame(receiver));
  await asked;
  a.close();
  b.close();
});

Deno.test("webrtcRemoveTrackAndStop", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const track = new Deno.webrtc.EncodedMediaTrack({
    kind: "audio",
    mimeType: "audio/opus",
  });
  const sender = a.addTrack(track, new MediaStream([track]));
  const gotTrack = once<RTCTrackEvent>(b, "track");
  await negotiate(a, b);
  const remote = await gotTrack;
  const [remoteStream] = remote.streams;
  assertEquals(remoteStream.getTracks(), [remote.track]);
  // removeTrack: the sender drops its track and stops sending; the remote
  // side removes the track from its stream.
  a.removeTrack(sender);
  assertEquals(sender.track, null);
  assertEquals(a.getTransceivers()[0].direction, "recvonly");
  const removed = once<MediaStreamTrackEvent>(remoteStream, "removetrack");
  await negotiate(a, b);
  assertEquals((await removed).track, remote.track);
  assertEquals(remoteStream.getTracks(), []);
  // addTrack doesn't reuse a transceiver whose sender has had a track.
  const again = a.addTrack(track);
  assert(again !== sender);
  assertEquals(a.getTransceivers().length, 2);
  // stop(): stopping until negotiated, then stopped and gone.
  const t = a.getTransceivers()[0];
  // Both sides recvonly: the answer made it inactive.
  assertEquals(t.currentDirection, "inactive");
  t.stop();
  assertEquals(t.direction, "stopped");
  assertEquals(t.currentDirection, "inactive");
  await negotiate(a, b);
  assertEquals(t.currentDirection, "stopped");
  assertEquals(t.receiver.track.readyState, "ended");
  assert(!a.getTransceivers().includes(t));
  // Invalid uses.
  assertThrows(() => a.addTrack(track), DOMException);
  const other = new RTCPeerConnection();
  assertThrows(() => other.removeTrack(again), DOMException);
  other.close();
  a.close();
  b.close();
});

Deno.test("webrtcSenderParameters", async () => {
  const pc = new RTCPeerConnection();
  const t = pc.addTransceiver("video", {
    sendEncodings: [{ rid: "hi" }, { rid: "lo", scaleResolutionDownBy: 2 }],
  });
  await assertRejects(
    () => t.sender.setParameters({} as RTCRtpSendParameters),
    DOMException,
  );
  const params = t.sender.getParameters();
  assertEquals(params.encodings.map((e) => e.rid), ["hi", "lo"]);
  assertEquals(params.encodings[1].scaleResolutionDownBy, 2);
  assertEquals(typeof params.transactionId, "string");
  assertEquals(params.rtcp.reducedSize, false);
  params.encodings[0].maxBitrate = 500_000;
  await t.sender.setParameters(params);
  assertEquals(t.sender.getParameters().encodings[0].maxBitrate, 500_000);
  // A stale transaction id (from an earlier task) or a changed read-only
  // member is refused.
  const p2 = t.sender.getParameters();
  await new Promise((r) => setTimeout(r, 0));
  // No getParameters() in this task: InvalidStateError.
  const expired = await assertRejects(() => t.sender.setParameters(p2));
  assertEquals((expired as DOMException).name, "InvalidStateError");
  // Another transaction's id: InvalidModificationError.
  const current = t.sender.getParameters();
  assert(current.transactionId !== p2.transactionId);
  const stale = await assertRejects(() => t.sender.setParameters(p2));
  assertEquals((stale as DOMException).name, "InvalidModificationError");
  const p3 = t.sender.getParameters();
  p3.encodings.pop();
  const shrunk = await assertRejects(() => t.sender.setParameters(p3));
  assertEquals((shrunk as DOMException).name, "InvalidModificationError");
  const p4 = t.sender.getParameters();
  p4.encodings[0].scaleResolutionDownBy = 0.5;
  await assertRejects(() => t.sender.setParameters(p4), RangeError);
  pc.close();
});

Deno.test("webrtcReplaceTrack", async () => {
  const pc = new RTCPeerConnection();
  const t = pc.addTransceiver("audio");
  const audio = new Deno.webrtc.EncodedMediaTrack({
    kind: "audio",
    mimeType: "audio/opus",
  });
  const video = new Deno.webrtc.EncodedMediaTrack({
    kind: "video",
    mimeType: "video/VP8",
  });
  await t.sender.replaceTrack(audio);
  assertEquals(t.sender.track, audio);
  await assertRejects(() => t.sender.replaceTrack(video), TypeError);
  await t.sender.replaceTrack(null);
  assertEquals(t.sender.track, null);
  pc.close();
});

Deno.test("webrtcLegacyCallbacks", async () => {
  const a = new RTCPeerConnection();
  a.createDataChannel("x");
  const offer = await new Promise<RTCSessionDescriptionInit>(
    (resolve, reject) => {
      const p = a.createOffer(resolve, reject);
      assert(p instanceof Promise);
    },
  );
  assertEquals(offer.type, "offer");
  await new Promise<void>((resolve, reject) =>
    a.setLocalDescription(offer, resolve, reject)
  );
  assertEquals(a.signalingState, "have-local-offer");
  const failed = await new Promise<DOMException>((resolve) =>
    a.addIceCandidate(
      { candidate: "candidate:1 1 udp 1 127.0.0.1 9 typ host", sdpMid: "nope" },
      () => resolve(new DOMException("resolved")),
      resolve,
    )
  );
  assert(failed instanceof DOMException);
  a.close();
});

Deno.test("webrtcTransportsAndSelectedPair", async () => {
  const { a, b } = await connectPair();
  const dtls = a.sctp!.transport!;
  assert(dtls instanceof RTCDtlsTransport);
  assertEquals(dtls.state, "connected");
  const ice = dtls.iceTransport;
  assert(ice instanceof RTCIceTransport);
  assertEquals(ice.component, "rtp");
  assertEquals(ice.role, "controlling");
  assertEquals(b.sctp!.transport!.iceTransport.role, "controlled");
  assert(["connected", "completed"].includes(ice.state));
  assertEquals(ice.gatheringState, "complete");
  const local = ice.getLocalParameters()!;
  assert(
    a.localDescription!.sdp.includes(`a=ice-ufrag:${local.usernameFragment}`),
  );
  assertEquals(
    ice.getRemoteParameters()!.usernameFragment,
    b.sctp!.transport!.iceTransport.getLocalParameters()!.usernameFragment,
  );
  const pair = ice.getSelectedCandidatePair()!;
  assertEquals(pair.local!.address, "127.0.0.1");
  assertEquals(pair.remote!.address, "127.0.0.1");
  b.close();
  a.close();
  // close() fires no events; the transports report closed.
  assertEquals(dtls.state, "closed");
  assertEquals(ice.state, "closed");
});

Deno.test("webrtcSetConfigurationRules", () => {
  const pc = new RTCPeerConnection({ bundlePolicy: "max-bundle" });
  const err = assertThrows(
    () => pc.setConfiguration({ bundlePolicy: "balanced" }),
    DOMException,
  );
  assertEquals((err as DOMException).name, "InvalidModificationError");
  assertThrows(
    () =>
      pc.setConfiguration({
        bundlePolicy: "max-bundle",
        iceCandidatePoolSize: 256,
      }),
    TypeError,
  );
  assertThrows(
    () =>
      pc.setConfiguration({
        bundlePolicy: "max-bundle",
        iceServers: [{ urls: "bogus:x" }],
      }),
    DOMException,
  );
  pc.setConfiguration({
    bundlePolicy: "max-bundle",
    iceServers: [{ urls: "stun:stun.example.org" }],
  });
  assertEquals(pc.getConfiguration().iceServers![0].urls, [
    "stun:stun.example.org",
  ]);
  pc.close();
});

Deno.test("webrtcRollbackRemovesRemoteTransceivers", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  a.addTransceiver("audio");
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  assertEquals(b.getTransceivers().length, 1);
  assertEquals(b.getTransceivers()[0].direction, "recvonly");
  await b.setRemoteDescription({ type: "rollback" });
  assertEquals(b.getTransceivers().length, 0);
  assertEquals(b.signalingState, "stable");
  a.close();
  b.close();
});

// --- Deno.webrtc.Network (server-side network settings) --------------------

function candidatesOf(pc: RTCPeerConnection) {
  return pc.localDescription!.sdp.split("\r\n").filter((l) =>
    l.startsWith("a=candidate:")
  ).map((l) => new RTCIceCandidate({ candidate: l.slice(2), sdpMid: "0" }));
}

async function connectWith(
  serverConfig: RTCConfiguration,
  init: RTCDataChannelInit = {},
) {
  // The client offers, the server (on the network) answers.
  const client = new RTCPeerConnection();
  const server = new RTCPeerConnection(serverConfig);
  const channel = client.createDataChannel("net", init);
  const remote = once<RTCDataChannelEvent>(server, "datachannel");
  const opened = once(channel, "open");
  await negotiate(client, server);
  await opened;
  const serverChannel = (await remote).channel;
  if (serverChannel.readyState !== "open") await once(serverChannel, "open");
  return { client, server, channel, serverChannel };
}

Deno.test("webrtcNetworkValidation", () => {
  assertThrows(
    () => Deno.webrtc.createNetwork({ port: 0, portRange: { min: 1, max: 2 } }),
    TypeError,
  );
  assertThrows(
    () => Deno.webrtc.createNetwork({ portRange: { min: 10, max: 5 } }),
    RangeError,
  );
  assertThrows(
    () => Deno.webrtc.createNetwork({ publicAddresses: ["not an ip"] }),
    TypeError,
  );
  assertThrows(
    () =>
      Deno.webrtc.createNetwork({
        publicAddresses: ["203.0.113.7", "203.0.113.8"],
      }),
    TypeError,
  );
  assertThrows(
    () =>
      new RTCPeerConnection({
        denoNetwork: {} as Deno.webrtc.Network,
      }),
    TypeError,
  );
  const net = Deno.webrtc.createNetwork();
  const pc = new RTCPeerConnection({ denoNetwork: net });
  assertEquals(pc.getConfiguration().denoNetwork, net);
  const other = Deno.webrtc.createNetwork();
  const err = assertThrows(
    () => pc.setConfiguration({ denoNetwork: other }),
    DOMException,
  );
  assertEquals((err as DOMException).name, "InvalidModificationError");
  pc.close();
  other.close();
  net.close();
  const closed = assertThrows(
    () => new RTCPeerConnection({ denoNetwork: net }),
    DOMException,
  );
  assertEquals((closed as DOMException).name, "InvalidStateError");
});

Deno.test("webrtcNetworkSharedPort", async () => {
  // port 0: one shared socket on a port the OS picks.
  using net = Deno.webrtc.createNetwork({ port: 0 });
  const [address] = net.localAddresses;
  const port = Number(address.slice(address.lastIndexOf(":") + 1));
  assert(port > 0, address);
  const one = await connectWith({ denoNetwork: net });
  const two = await connectWith({ denoNetwork: net });
  // Both server connections gather on the one port.
  for (const pc of [one.server, two.server]) {
    const ports = candidatesOf(pc).map((c) => c.port);
    assert(ports.length > 0);
    assert(ports.every((p) => p === port), `${ports} vs ${port}`);
  }
  // Each pair's traffic reaches its own connection.
  const toOne = messages(one.serverChannel, 1);
  const toTwo = messages(two.serverChannel, 1);
  one.channel.send("for one");
  two.channel.send("for two");
  assertEquals(await toOne, ["for one"]);
  assertEquals(await toTwo, ["for two"]);
  const back = messages(two.channel, 1);
  two.serverChannel.send("from two");
  assertEquals(await back, ["from two"]);
  for (const p of [one, two]) {
    p.client.close();
    p.server.close();
  }
});

Deno.test("webrtcNetworkPortRange", async () => {
  using net = Deno.webrtc.createNetwork({
    portRange: { min: 47200, max: 47220 },
  });
  const { client, server } = await connectWith({ denoNetwork: net });
  const ports = candidatesOf(server).map((c) => c.port!);
  assert(ports.length > 0);
  assert(ports.every((p) => p >= 47200 && p <= 47220), `${ports}`);
  client.close();
  server.close();
});

Deno.test("webrtcNetworkPublicAddressSrflx", async () => {
  using net = Deno.webrtc.createNetwork({
    publicAddresses: ["203.0.113.7"],
    publicAddressCandidate: "srflx",
  });
  const { client, server } = await connectWith({ denoNetwork: net });
  const candidates = candidatesOf(server);
  const srflx = candidates.find((c) => c.type === "srflx")!;
  assertEquals(srflx.address, "203.0.113.7");
  assertEquals(srflx.relatedAddress, "127.0.0.1");
  assert(
    candidates.some((c) => c.type === "host" && c.address === "127.0.0.1"),
  );
  client.close();
  server.close();
});

Deno.test("webrtcNetworkPublicAddressHost", async () => {
  using net = Deno.webrtc.createNetwork({
    publicAddresses: ["203.0.113.7/127.0.0.1"],
  });
  // The public address replaces the private one; the connection still
  // completes (the client is reached peer-reflexively, and what arrives on
  // the private address is the public candidate's).
  const { client, server, channel, serverChannel } = await connectWith({
    denoNetwork: net,
  });
  const hosts = candidatesOf(server).filter((c) => c.type === "host");
  assert(hosts.length > 0);
  assert(hosts.every((c) => c.address === "203.0.113.7"), `${hosts}`);
  const got = messages(serverChannel, 1);
  channel.send("through nat");
  assertEquals(await got, ["through nat"]);
  client.close();
  server.close();
});

Deno.test("webrtcNetworkIceLite", async () => {
  using net = Deno.webrtc.createNetwork({ iceLite: true });
  const { client, server, channel, serverChannel } = await connectWith({
    denoNetwork: net,
  });
  assert(server.localDescription!.sdp.includes("a=ice-lite"));
  const got = messages(serverChannel, 1);
  channel.send("lite");
  assertEquals(await got, ["lite"]);
  client.close();
  server.close();
});

Deno.test("webrtcNetworkMaxMessageSize", async () => {
  using net = Deno.webrtc.createNetwork({ maxMessageSize: 4096 });
  const { client, server, channel } = await connectWith({ denoNetwork: net });
  // The server advertises 4096; the client may send no more.
  assertEquals(client.sctp!.maxMessageSize, 4096);
  assertThrows(() => channel.send(new Uint8Array(4097)), TypeError);
  client.close();
  server.close();
});

Deno.test("webrtcSpecDetails", async () => {
  // Candidate strings parse strictly: a bad field leaves every derived
  // attribute null; relayProtocol and url come from the init dictionary.
  const bad = new RTCIceCandidate({
    candidate: "candidate:1 x udp 2113937151 192.0.2.1 5000 typ host",
    sdpMid: "0",
  });
  assertEquals([bad.foundation, bad.port, bad.type], [null, null, null]);
  const relay = new RTCIceCandidate({
    candidate:
      "candidate:1 1 UDP 100 192.0.2.1 5000 typ RELAY raddr 0.0.0.0 rport 0",
    sdpMid: "0",
    relayProtocol: "tls",
    url: "turns:turn.example.org",
  } as RTCIceCandidateInit);
  assertEquals([relay.protocol, relay.type], ["udp", "relay"]);
  // deno-lint-ignore no-explicit-any
  assertEquals((relay as any).relayProtocol, "tls");
  // deno-lint-ignore no-explicit-any
  assertEquals((relay as any).url, "turns:turn.example.org");
  const noRport = new RTCIceCandidate({
    candidate: "candidate:1 1 udp 100 192.0.2.1 5000 typ srflx raddr 0.0.0.0",
    sdpMid: "0",
  });
  assertEquals(noRport.foundation, null);

  // [EnforceRange] on the data channel integers; ICE server URLs.
  const pc = new RTCPeerConnection();
  assertThrows(
    () => pc.createDataChannel("x", { maxRetransmits: -1 }),
    TypeError,
  );
  assertThrows(
    () => new RTCPeerConnection({ iceServers: null as unknown as [] }),
    TypeError,
  );
  for (const urls of [[], "stun:stun.example.org#frag"]) {
    const e = assertThrows(
      () => new RTCPeerConnection({ iceServers: [{ urls }] }),
      DOMException,
    );
    assertEquals((e as DOMException).name, "SyntaxError");
  }
  // A non-negotiated channel's id waits for SCTP, whatever `id` says.
  assertEquals(pc.createDataChannel("y", { id: 42 }).id, null);
  // Audio codecs say how many channels they have.
  const pcmu = RTCRtpSender.getCapabilities("audio")!.codecs.find((c) =>
    c.mimeType.toLowerCase() === "audio/pcmu"
  )!;
  assertEquals(pcmu.channels, 1);
  // A promise-returning method rejects on a wrong `this`.
  const p = (RTCPeerConnection.prototype.createOffer as () => Promise<unknown>)
    .call({});
  assert(p instanceof Promise);
  await assertRejects(() => p, TypeError);
  // restartIce() before the first negotiation fires nothing.
  let needed = 0;
  pc.onnegotiationneeded = () => needed++;
  pc.restartIce();
  await new Promise((r) => setTimeout(r, 20));
  assertEquals(needed, 0);
  pc.close();

  // rtcpMuxPolicy "require": a media section without a=rtcp-mux.
  const a = new RTCPeerConnection();
  a.addTransceiver("audio");
  const offer = await a.createOffer();
  const b = new RTCPeerConnection();
  const noMux = offer.sdp!.replace(/a=rtcp-mux\r\n/g, "");
  const mux = await assertRejects(() =>
    b.setRemoteDescription({ type: "offer", sdp: noMux })
  );
  assertEquals((mux as DOMException).name, "InvalidAccessError");

  // ICE stays "new" while no remote candidate is known.
  const noCandidates = offer.sdp!.replace(/a=candidate:[^\r\n]*\r\n/g, "");
  await b.setRemoteDescription({ type: "offer", sdp: noCandidates });
  await b.setLocalDescription();
  await new Promise((r) => setTimeout(r, 300));
  assertEquals(b.iceConnectionState, "new");
  // The same RTCSessionDescription object while it doesn't change.
  assert(b.remoteDescription === b.remoteDescription);
  a.close();
  b.close();

  // sctp exists once an answer negotiates m=application, before it connects.
  const c = new RTCPeerConnection();
  const d = new RTCPeerConnection();
  c.createDataChannel("s");
  assertEquals(c.sctp, null);
  await c.setLocalDescription();
  await d.setRemoteDescription(c.localDescription!);
  await d.setLocalDescription();
  assert(d.sctp instanceof RTCSctpTransport);
  await c.setRemoteDescription(d.localDescription!);
  assert(c.sctp instanceof RTCSctpTransport);
  assert(c.sctp!.maxMessageSize > 0);
  c.close();
  d.close();
});

Deno.test("webrtcDtmf", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const track = new Deno.webrtc.EncodedMediaTrack({
    kind: "audio",
    mimeType: "audio/opus",
  });
  const sender = a.addTrack(track);
  assertEquals(a.addTransceiver("video").sender.dtmf, null);
  const dtmf = sender.dtmf!;
  assert(dtmf instanceof RTCDTMFSender);
  assertEquals(dtmf.canInsertDTMF, false); // not negotiated yet
  const gotTrack = once<RTCTrackEvent>(b, "track");
  await negotiate(a, b);
  const { receiver } = await gotTrack;
  await connected(a);
  assertEquals(dtmf.canInsertDTMF, true);
  const err = assertThrows(() => dtmf.insertDTMF("1x"), DOMException);
  assertEquals((err as DOMException).name, "InvalidCharacterError");
  const pt =
    receiver.getParameters().codecs.find((c) =>
      c.mimeType.toLowerCase() === "audio/telephone-event" &&
      c.clockRate === 48000
    )!.payloadType;
  const reader = Deno.webrtc.readRtp(receiver).getReader();
  const tones: string[] = [];
  const done = new Promise<void>((resolve) => {
    dtmf.ontonechange = (e) => {
      tones.push(e.tone);
      if (e.tone === "") resolve();
    };
  });
  dtmf.insertDTMF("1#", 40, 30);
  assertEquals(dtmf.toneBuffer, "1#");
  // The first telephone event on the wire: event 1, not yet ended.
  let first: Uint8Array | undefined;
  while (!first) {
    const { value } = await reader.read();
    if ((value![1] & 0x7f) === pt) first = value;
  }
  assertEquals(first[12], 1);
  assertEquals(first[13] & 0x80, 0);
  await done;
  assertEquals(tones, ["1", "#", ""]);
  await reader.cancel();
  a.close();
  b.close();
});

Deno.test("webrtcBufferedAmountCountsQueuedBlobs", async () => {
  const { a, b, channel, remote } = await connectPair();
  const got = messages(remote, 2);
  // A Blob (and what waits behind it) counts from the send() call on.
  channel.send(new Blob([new Uint8Array(5)]));
  assertEquals(channel.bufferedAmount, 5);
  channel.send("abc");
  assertEquals(channel.bufferedAmount, 8);
  assertEquals((await got).length, 2);
  a.close();
  b.close();
});

Deno.test("webrtcSctpStateChange", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  a.createDataChannel("t");
  // sctp exists from the answer and announces "connected".
  const toB = trickle(a, b);
  const toA = trickle(b, a);
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  toB();
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription!);
  toA();
  const sctp = a.sctp!;
  if (sctp.state !== "connected") {
    assertEquals(sctp.state, "connecting");
    await once(sctp, "statechange");
  }
  assertEquals(sctp.state, "connected");
  a.close();
  b.close();
});
