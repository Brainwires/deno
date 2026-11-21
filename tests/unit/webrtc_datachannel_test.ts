// Copyright 2018-2026 the Deno authors. MIT license.

// RTCDataChannel transfer (W3C WebRTC 6.2), priority (webrtc-priority),
// send() size checks, max-message-size renegotiation, `open` ordering and
// RTCCertificate serialization. Two peers in one isolate connect over
// loopback; transferred channels live in a worker.

import { assert, assertEquals, assertThrows } from "./test_util.ts";

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

function withMaxMessageSize(
  desc: RTCSessionDescription,
  value: number,
): RTCSessionDescriptionInit {
  return {
    type: desc.type,
    sdp: desc.sdp.replace(
      /a=max-message-size:\d+\r\n/,
      `a=max-message-size:${value}\r\n`,
    ),
  };
}

// A worker that takes a transferred channel, reports its attributes and
// events, and echoes what it receives.
const ECHO_WORKER = `
self.onmessage = ({ data }) => {
  if (data.channel) {
    const ch = data.channel;
    self.postMessage({
      type: "init",
      label: ch.label,
      readyState: ch.readyState,
      id: ch.id,
      priority: ch.priority,
      ordered: ch.ordered,
      maxRetransmits: ch.maxRetransmits,
      isChannel: ch instanceof RTCDataChannel,
    });
    ch.onopen = () => self.postMessage({ type: "open", id: ch.id });
    ch.onclosing = () => self.postMessage({ type: "closing" });
    ch.onclose = () => self.postMessage({ type: "close" });
    ch.onmessage = (e) => ch.send("echo:" + e.data);
  }
};
`;

function echoWorker() {
  const worker = new Worker(
    `data:application/javascript,${encodeURIComponent(ECHO_WORKER)}`,
    { type: "module" },
  );
  const replies: Record<string, unknown>[] = [];
  let notify = () => {};
  worker.onmessage = (e) => {
    replies.push(e.data);
    notify();
  };
  const next = async (type: string) => {
    const deadline = Date.now() + 20_000;
    while (true) {
      const i = replies.findIndex((r) => r.type === type);
      if (i >= 0) return replies.splice(i, 1)[0];
      if (Date.now() > deadline) throw new Error(`no '${type}' from worker`);
      await new Promise<void>((resolve) => {
        notify = resolve;
        setTimeout(resolve, 100);
      });
    }
  };
  return { worker, next };
}

Deno.test("webrtcDataChannelPriority", () => {
  const pc = new RTCPeerConnection();
  try {
    assertEquals(pc.createDataChannel("a").priority, "low");
    assertEquals(
      pc.createDataChannel("b", { priority: "high" }).priority,
      "high",
    );
    assertEquals(
      pc.createDataChannel("c", { priority: "very-low" }).priority,
      "very-low",
    );
    assertThrows(
      () =>
        pc.createDataChannel("d", {
          priority: "urgent" as RTCPriorityType,
        }),
      TypeError,
    );
  } finally {
    pc.close();
  }
});

Deno.test("webrtcDataChannelPriorityReachesThePeer", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    a.createDataChannel("prio", { priority: "medium" });
    const remote = once<RTCDataChannelEvent>(b, "datachannel");
    await negotiate(a, b);
    assertEquals((await remote).channel.priority, "medium");
  } finally {
    a.close();
    b.close();
  }
});

Deno.test("webrtcDataChannelOpenIsItsOwnTask", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    a.createDataChannel("no-open");
    let opened = false;
    const closed = new Promise<void>((resolve) => {
      b.ondatachannel = ({ channel }) => {
        assertEquals(channel.readyState, "open");
        channel.onopen = () => opened = true;
        channel.onclose = () => resolve();
        channel.close();
      };
    });
    await negotiate(a, b);
    await closed;
    await new Promise((resolve) => setTimeout(resolve, 200));
    assert(!opened, "a channel closed in `datachannel` reports no `open`");
  } finally {
    a.close();
    b.close();
  }
});

Deno.test("webrtcDataChannelSendChecksMaxMessageSize", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    const channel = a.createDataChannel("big");
    const opened = once(channel, "open");
    await negotiate(a, b);
    await opened;
    const max = a.sctp!.maxMessageSize;
    assertThrows(() => channel.send(new Uint8Array(max + 1)), TypeError);
    assertThrows(
      () => channel.send(new Blob([new Uint8Array(max + 1)])),
      TypeError,
    );
    assertEquals(channel.bufferedAmount, 0);
  } finally {
    a.close();
    b.close();
  }
});

Deno.test("webrtcSctpMaxMessageSizeRenegotiates", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    a.createDataChannel("mms");
    for (const size of [1, 2, 0, 1]) {
      await a.setLocalDescription();
      await b.setRemoteDescription(
        withMaxMessageSize(a.localDescription!, size),
      );
      await b.setLocalDescription();
      await a.setRemoteDescription(b.localDescription!);
      assertEquals(b.sctp!.maxMessageSize, size === 0 ? 262144 : size);
    }
  } finally {
    a.close();
    b.close();
  }
});

Deno.test("webrtcSctpMaxMessageSizeBeforeAnAnswer", async () => {
  const pc = new RTCPeerConnection();
  try {
    pc.createDataChannel("x");
    const offer = await pc.createOffer();
    await pc.setRemoteDescription(offer);
    await pc.setRemoteDescription({ type: "rollback" });
    await pc.setRemoteDescription(offer);
    assertEquals(pc.sctp!.maxMessageSize, 65536);
  } finally {
    pc.close();
  }
});

Deno.test("webrtcDataChannelTransferableOnlyInItsTask", async () => {
  const pc = new RTCPeerConnection();
  const { port1, port2 } = new MessageChannel();
  try {
    const late = pc.createDataChannel("late");
    await new Promise((resolve) => setTimeout(resolve, 0));
    assertThrows(
      () => port1.postMessage(late, [late]),
      DOMException,
      "not transferable",
    );
    const sent = pc.createDataChannel("sent", {
      negotiated: true,
      id: 3,
    });
    assertThrows(() => sent.send("x"), DOMException);
    assertThrows(
      () => port1.postMessage(sent, [sent]),
      DOMException,
      "not transferable",
    );
    // Not in the transfer list: not serializable either.
    const fresh = pc.createDataChannel("fresh");
    assertThrows(() => structuredClone(fresh));
  } finally {
    port1.close();
    port2.close();
    pc.close();
  }
});

// The worker ends asynchronously after terminate().
Deno.test({
  name: "webrtcDataChannelTransferToWorker",
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const { worker, next } = echoWorker();
  try {
    const channel = a.createDataChannel("moved", {
      ordered: false,
      maxRetransmits: 2,
      priority: "high",
    });
    worker.postMessage({ channel }, [channel]);
    assertEquals(channel.readyState, "closed", "the original is detached");
    const init = await next("init");
    assertEquals(init.label, "moved");
    assertEquals(init.readyState, "connecting");
    assertEquals(init.id, null);
    assertEquals(init.priority, "high");
    assertEquals(init.ordered, false);
    assertEquals(init.maxRetransmits, 2);
    assertEquals(init.isChannel, true);

    const remote = once<RTCDataChannelEvent>(b, "datachannel");
    await negotiate(a, b);
    const peer = (await remote).channel;
    const open = await next("open");
    assertEquals(open.id, peer.id);

    const echoed = once<MessageEvent>(peer, "message");
    peer.send("hi");
    assertEquals((await echoed).data, "echo:hi");

    peer.close();
    await next("close");
  } finally {
    worker.terminate();
    a.close();
    b.close();
  }
});

// The connection's data-channel stats object for a channel transferred to a
// worker names the channel's id, which the original (detached, "closed")
// no longer reads (WPT webrtc-stats RTCDataChannel-stats.html, offerer on
// worker).
Deno.test({
  name: "webrtcDataChannelTransferredChannelStatsHaveItsId",
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const { worker, next } = echoWorker();
  try {
    const channel = a.createDataChannel("stats-moved");
    worker.postMessage({ channel }, [channel]);
    await next("init");
    const remote = once<RTCDataChannelEvent>(b, "datachannel");
    await negotiate(a, b);
    const peer = (await remote).channel;
    const open = await next("open");
    assertEquals(open.id, peer.id);
    const stats = [...(await a.getStats()).values()].filter((s) =>
      s.type === "data-channel"
    );
    assertEquals(stats.length, 1);
    assertEquals(stats[0].label, "stats-moved");
    assertEquals(stats[0].dataChannelIdentifier, open.id);
    assertEquals(stats[0].state, "open");
    assertEquals(stats[0].messagesSent, 0);
    assertEquals(stats[0].bytesSent, 0);
  } finally {
    worker.terminate();
    a.close();
    b.close();
  }
});

Deno.test({
  name: "webrtcDataChannelTransferInDatachannelEvent",
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const { worker, next } = echoWorker();
  try {
    const channel = a.createDataChannel("answerer");
    b.ondatachannel = ({ channel: remote }) => {
      remote.onopen = () => {
        throw new Error("the transferred original must not open");
      };
      worker.postMessage({ channel: remote }, [remote]);
    };
    const opened = once(channel, "open");
    await negotiate(a, b);
    await opened;
    const init = await next("init");
    assertEquals(init.readyState, "open");
    await next("open");
    const echoed = once<MessageEvent>(channel, "message");
    channel.send("ping");
    assertEquals((await echoed).data, "echo:ping");
  } finally {
    worker.terminate();
    a.close();
    b.close();
  }
});

Deno.test("webrtcCertificateStructuredClone", async () => {
  const certificate = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
  });
  const clone = structuredClone(certificate);
  assert(clone instanceof RTCCertificate);
  assert(clone !== certificate);
  assertEquals(clone.expires, certificate.expires);
  assertEquals(clone.getFingerprints(), certificate.getFingerprints());
  // A clone works as a connection's certificate.
  const pc = new RTCPeerConnection({ certificates: [clone] });
  pc.close();
});

Deno.test({
  name: "webrtcCertificateCannotBePostedToAWorker",
  sanitizeOps: false,
  sanitizeResources: false,
}, async () => {
  const certificate = await RTCPeerConnection.generateCertificate({
    name: "ECDSA",
    namedCurve: "P-256",
  });
  const source = `
    self.onmessage = () => self.postMessage("message");
    self.onmessageerror = () => self.postMessage("messageerror");
  `;
  const worker = new Worker(
    `data:application/javascript,${encodeURIComponent(source)}`,
    { type: "module" },
  );
  try {
    const reply = new Promise((resolve) => {
      worker.onmessage = (e) => resolve(e.data);
    });
    worker.postMessage(certificate);
    assertEquals(await reply, "messageerror");
  } finally {
    worker.terminate();
  }
});
