// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC ICE (wpt100:ice): the standalone `new RTCIceTransport()` of W3C
// webrtc-ice, the end-of-candidates candidate, and a local description that
// gains each candidate with its `icecandidate` event. Two transports or
// peers in one isolate connect over loopback.

import { assert, assertEquals, assertThrows } from "./test_util.ts";

// The webrtc-ice members are not in the DOM type declarations.
// deno-lint-ignore no-explicit-any
type IceTransport = any;
// deno-lint-ignore no-explicit-any
const RTCIceTransportCtor = (globalThis as any).RTCIceTransport;

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

Deno.test("webrtcIceTransportConstructor", () => {
  const t: IceTransport = new RTCIceTransportCtor();
  try {
    assertEquals(t.role, null);
    assertEquals(t.state, "new");
    assertEquals(t.gatheringState, "new");
    assertEquals(t.getLocalCandidates(), []);
    assertEquals(t.getRemoteCandidates(), []);
    assertEquals(t.getSelectedCandidatePair(), null);
    const local = t.getLocalParameters();
    assert(local.usernameFragment.length >= 4);
    assert(local.password.length >= 22);
    assertEquals(t.getRemoteParameters(), null);
  } finally {
    t.stop();
  }
});

Deno.test("webrtcIceTransportValidation", () => {
  const t: IceTransport = new RTCIceTransportCtor();
  try {
    assertThrows(() => t.gather({ iceServers: null }), TypeError);
    assertThrows(() => t.start({}), TypeError);
    assertThrows(
      () => t.start({ usernameFragment: "abc", password: "p".repeat(22) }),
      DOMException,
    );
    assertThrows(
      () => t.start({ usernameFragment: "abcd", password: "p".repeat(21) }),
      DOMException,
    );
    assertThrows(
      () =>
        t.addRemoteCandidate(
          new RTCIceCandidate({ candidate: "invalid", sdpMid: "" }),
        ),
      DOMException,
    );
    t.start({ usernameFragment: "uuuu", password: "p".repeat(22) });
    assertEquals(t.role, "controlled");
    assertEquals(t.state, "new");
    t.addRemoteCandidate(
      new RTCIceCandidate({
        candidate: "candidate:1 1 udp 2113929471 203.0.113.100 10100 typ host",
        sdpMid: "",
      }),
    );
    assertEquals(t.state, "checking");
    assertThrows(
      () =>
        t.start(
          { usernameFragment: "uuuu", password: "p".repeat(22) },
          "controlling",
        ),
      DOMException,
    );
    t.start({ usernameFragment: "vvvv", password: "q".repeat(22) });
    assertEquals(t.state, "new");
    assertEquals(t.getRemoteCandidates(), []);
  } finally {
    t.stop();
  }
  assertThrows(() => t.gather({}), DOMException);
});

Deno.test("webrtcIceTransportGatherAndConnect", async () => {
  const a: IceTransport = new RTCIceTransportCtor();
  const b: IceTransport = new RTCIceTransportCtor();
  try {
    a.onicecandidate = (e: RTCPeerConnectionIceEvent) => {
      if (e.candidate) b.addRemoteCandidate(e.candidate);
    };
    b.onicecandidate = (e: RTCPeerConnectionIceEvent) => {
      if (e.candidate) a.addRemoteCandidate(e.candidate);
    };
    const complete = once(a, "gatheringstatechange");
    a.gather({});
    b.gather({});
    assertEquals(a.gatheringState, "gathering");
    a.start(b.getLocalParameters(), "controlling");
    b.start(a.getLocalParameters(), "controlled");
    const aPair = once(a, "selectedcandidatepairchange");
    const aConnected = once(a, "statechange");
    const bConnected = once(b, "statechange");
    await complete;
    assertEquals(a.gatheringState, "complete");
    await aPair;
    const pair = a.getSelectedCandidatePair();
    assert(pair !== null);
    assert(
      a.getLocalCandidates().some((c: RTCIceCandidate) =>
        c.candidate === pair.local.candidate
      ),
    );
    await aConnected;
    await bConnected;
    assertEquals(a.state, "connected");
    assertEquals(b.state, "connected");
  } finally {
    a.stop();
    b.stop();
  }
  assertEquals(a.getSelectedCandidatePair(), null);
  assertEquals(a.state, "closed");
});

Deno.test("webrtcLocalDescriptionGainsAnnouncedCandidates", async () => {
  const pc = new RTCPeerConnection();
  try {
    pc.addTransceiver("audio");
    await pc.setLocalDescription();
    const before = pc.pendingLocalDescription!;
    assert(!before.sdp.includes("a=candidate:"));
    const event = await once<RTCPeerConnectionIceEvent>(pc, "icecandidate");
    assert(event.candidate !== null);
    const after = pc.pendingLocalDescription!;
    assert(after !== before);
    assert(after.sdp.includes("a=candidate:"));
    assert(!before.sdp.includes("a=candidate:"));
  } finally {
    pc.close();
  }
});

Deno.test("webrtcEndOfCandidatesBeforeComplete", async () => {
  const pc = new RTCPeerConnection();
  try {
    pc.addTransceiver("audio", { direction: "recvonly" });
    const seen: string[] = [];
    const done = new Promise<void>((resolve) => {
      pc.onicecandidate = (e) => {
        if (e.candidate === null) {
          seen.push(`null:${pc.iceGatheringState}`);
          resolve();
        } else if (e.candidate.candidate === "") {
          seen.push(`end:${pc.iceGatheringState}`);
        }
      };
    });
    await pc.setLocalDescription();
    await done;
    assertEquals(seen, ["end:gathering", "null:complete"]);
  } finally {
    pc.close();
  }
});

Deno.test("webrtcRollbackOfFirstOfferResetsGathering", async () => {
  const pc = new RTCPeerConnection();
  try {
    pc.addTransceiver("audio", { direction: "recvonly" });
    await pc.setLocalDescription();
    while (pc.iceGatheringState !== "complete") {
      await once(pc, "icegatheringstatechange");
    }
    const toNew = once(pc, "icegatheringstatechange");
    await pc.setLocalDescription({ type: "rollback" });
    await toNew;
    assertEquals(pc.iceGatheringState, "new");
  } finally {
    pc.close();
  }
});
