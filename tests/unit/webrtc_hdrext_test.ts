// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC extensions: RTP header extension control on `RTCRtpTransceiver`,
// `rtpHeaderEncryptionPolicy` (Cryptex, RFC 9335),
// `alwaysNegotiateDataChannels`, `targetLatency` and OAuth ICE credentials;
// the SDP rules a remote description is held to. Signalling only: no media
// flows here.

import {
  assert,
  assertEquals,
  assertRejects,
  assertThrows,
} from "./test_util.ts";

const MID = "urn:ietf:params:rtp-hdrext:sdes:mid";
const ABS_CAPTURE_TIME =
  "http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time";

function sections(sdp: string): string[] {
  return sdp.split(/\r?\nm=/).slice(1).map((s) => "m=" + s);
}

function extmapUris(section: string): string[] {
  return section.split(/\r?\n/)
    .filter((l) => l.startsWith("a=extmap:"))
    .map((l) => l.split(" ")[1]);
}

/** An `RTCConfiguration` with the webrtc-extensions members lib.dom lacks. */
function config(members: Record<string, unknown>): RTCConfiguration {
  return members as RTCConfiguration;
}

async function negotiate(pc1: RTCPeerConnection, pc2: RTCPeerConnection) {
  await pc1.setLocalDescription();
  await pc2.setRemoteDescription(pc1.localDescription!);
  await pc2.setLocalDescription();
  await pc1.setRemoteDescription(pc2.localDescription!);
}

Deno.test("webrtcExtensionsConfigurationMembers", () => {
  const pc = new RTCPeerConnection();
  try {
    // deno-lint-ignore no-explicit-any
    const config = pc.getConfiguration() as any;
    assertEquals(config.rtpHeaderEncryptionPolicy, "negotiate");
    assertEquals(config.alwaysNegotiateDataChannels, false);
    assertEquals(config.targetLatency, "lowest");
    pc.setConfiguration(config);
    assertThrows(
      () =>
        pc.setConfiguration({ ...config, alwaysNegotiateDataChannels: true }),
      DOMException,
    );
    assertThrows(
      () =>
        pc.setConfiguration({
          ...config,
          rtpHeaderEncryptionPolicy: "require",
        }),
      DOMException,
    );
    assertThrows(
      () => pc.setConfiguration({ ...config, targetLatency: "none" }),
      TypeError,
    );
  } finally {
    pc.close();
  }
  assertThrows(
    // deno-lint-ignore no-explicit-any
    () => new RTCPeerConnection({ targetLatency: "whatever" } as any),
    TypeError,
  );
  assertThrows(
    // deno-lint-ignore no-explicit-any
    () => new RTCPeerConnection({ rtpHeaderEncryptionPolicy: "x" } as any),
    TypeError,
  );
});

Deno.test("webrtcOAuthIceCredentials", () => {
  const error = assertThrows(() =>
    new RTCPeerConnection({
      iceServers: [{
        urls: "turns:turn.example.org",
        username: "user",
        credential: "cred",
        credentialType: "oauth",
        // deno-lint-ignore no-explicit-any
      } as any],
    })
  );
  assertEquals((error as DOMException).name, "InvalidAccessError");
  const pc = new RTCPeerConnection({
    iceServers: [{
      urls: "turns:turn.example.org",
      username: "user",
      credential: { macKey: "a2V5", accessToken: "dG9rZW4=" },
      credentialType: "oauth",
      // deno-lint-ignore no-explicit-any
    } as any],
  });
  try {
    // deno-lint-ignore no-explicit-any
    const server = pc.getConfiguration().iceServers![0] as any;
    assertEquals(server.credentialType, "oauth");
    assertEquals(server.credential.accessToken, "dG9rZW4=");
  } finally {
    pc.close();
  }
});

Deno.test("webrtcHeaderExtensionsToNegotiate", async () => {
  const pc = new RTCPeerConnection();
  try {
    // deno-lint-ignore no-explicit-any
    const t = pc.addTransceiver("video") as any;
    const list = t.getHeaderExtensionsToNegotiate();
    assertEquals(
      list.find((e: { uri: string }) => e.uri === MID)?.direction,
      "sendrecv",
    );
    assert(
      list.some((e: { uri: string }) => e.uri === "urn:3gpp:video-orientation"),
    );
    // Implemented, not offered by default.
    assertEquals(
      list.find((e: { uri: string }) => e.uri === ABS_CAPTURE_TIME)?.direction,
      "stopped",
    );
    assert(
      t.getNegotiatedHeaderExtensions().every((e: { direction: string }) =>
        e.direction === "stopped"
      ),
    );
    assertThrows(
      () => t.setHeaderExtensionsToNegotiate(list.slice(1)),
      DOMException,
    );
    const mid = list.map((e: object) => ({ ...e }));
    mid.find((e: { uri: string }) => e.uri === MID).direction = "sendonly";
    assertThrows(() => t.setHeaderExtensionsToNegotiate(mid), DOMException);
    const empty = list.map((e: object) => ({ ...e }));
    empty[0].uri = "";
    assertThrows(() => t.setHeaderExtensionsToNegotiate(empty), TypeError);

    const stop = list.map((e: object) => ({ ...e }));
    stop.find((e: { uri: string }) => e.uri === "urn:3gpp:video-orientation")
      .direction = "stopped";
    t.setHeaderExtensionsToNegotiate(stop);
    const offered = extmapUris(sections((await pc.createOffer()).sdp!)[0]);
    assert(offered.includes(MID));
    assert(!offered.includes("urn:3gpp:video-orientation"));
    assert(!offered.includes(ABS_CAPTURE_TIME));
  } finally {
    pc.close();
  }
});

Deno.test("webrtcHeaderExtensionsNegotiated", async () => {
  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection();
  try {
    // deno-lint-ignore no-explicit-any
    const t1 = pc1.addTransceiver("audio") as any;
    await pc1.setLocalDescription();
    await pc2.setRemoteDescription(pc1.localDescription!);
    // deno-lint-ignore no-explicit-any
    const t2 = pc2.getTransceivers()[0] as any;
    const list = t2.getHeaderExtensionsToNegotiate();
    const level = "urn:ietf:params:rtp-hdrext:ssrc-audio-level";
    list.find((e: { uri: string }) => e.uri === level).direction = "stopped";
    t2.setHeaderExtensionsToNegotiate(list);
    await pc2.setLocalDescription();
    await pc1.setRemoteDescription(pc2.localDescription!);
    const negotiated = t1.getNegotiatedHeaderExtensions();
    assertEquals(
      negotiated.find((e: { uri: string }) => e.uri === level).direction,
      "stopped",
    );
    assertEquals(
      negotiated.find((e: { uri: string }) => e.uri === MID).direction,
      "sendrecv",
    );
  } finally {
    pc1.close();
    pc2.close();
  }
});

Deno.test("webrtcCryptexInOffersAndAnswers", async () => {
  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection(
    config({ rtpHeaderEncryptionPolicy: "disable" }),
  );
  const pc3 = new RTCPeerConnection(
    config({ rtpHeaderEncryptionPolicy: "require" }),
  );
  try {
    pc1.addTransceiver("audio");
    const offer = await pc1.createOffer();
    assert(offer.sdp!.includes("a=cryptex"));
    await pc1.setLocalDescription(offer);
    await pc2.setRemoteDescription(offer);
    await pc2.setLocalDescription();
    assert(!pc2.localDescription!.sdp.includes("a=cryptex"));
    // "require" refuses a description without it.
    await assertRejects(
      () =>
        pc3.setRemoteDescription({
          type: "offer",
          sdp: offer.sdp!.replace("a=cryptex", "a=nocryptex"),
        }),
      DOMException,
    );
    await pc3.setRemoteDescription(offer);
    await pc3.setLocalDescription();
    assert(pc3.localDescription!.sdp.includes("a=cryptex"));
  } finally {
    pc1.close();
    pc2.close();
    pc3.close();
  }
});

Deno.test("webrtcAlwaysNegotiateDataChannels", async () => {
  const pc = new RTCPeerConnection(
    config({ alwaysNegotiateDataChannels: true }),
  );
  try {
    assertEquals(sections((await pc.createOffer()).sdp!).length, 1);
    pc.addTransceiver("audio");
    const found = sections((await pc.createOffer()).sdp!);
    assertEquals(found.length, 2);
    assert(found[0].startsWith("m=application "));
    assert(found[1].startsWith("m=audio "));
  } finally {
    pc.close();
  }
});

Deno.test("webrtcMungedLocalDescriptionIsRefused", async () => {
  const pc = new RTCPeerConnection();
  try {
    pc.addTransceiver("audio");
    const offer = await pc.createOffer();
    const error = await assertRejects(() =>
      pc.setLocalDescription({
        type: "offer",
        sdp: offer.sdp + "a=munging-is-not-good\r\n",
      })
    );
    assertEquals((error as DOMException).name, "InvalidModificationError");
  } finally {
    pc.close();
  }
});

Deno.test("webrtcRemoteDescriptionRules", async () => {
  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection();
  const pc3 = new RTCPeerConnection();
  try {
    pc1.addTransceiver("audio").direction = "sendonly";
    await pc1.setLocalDescription();
    // No DTLS fingerprint: SDES is not supported.
    const noFingerprint = pc1.localDescription!.sdp.split(/\r\n/)
      .filter((l) => !l.startsWith("a=fingerprint:")).join("\r\n");
    const error = await assertRejects(() =>
      pc3.setRemoteDescription({ type: "offer", sdp: noFingerprint })
    );
    assertEquals((error as DOMException).name, "InvalidAccessError");
    // An answer can't widen the offer's direction.
    await pc2.setRemoteDescription(pc1.localDescription!);
    await pc2.setLocalDescription();
    const widened = await assertRejects(() =>
      pc1.setRemoteDescription({
        type: "answer",
        sdp: pc2.localDescription!.sdp.replace("a=recvonly", "a=sendrecv"),
      })
    );
    assertEquals((widened as DOMException).name, "InvalidAccessError");
    await pc1.setRemoteDescription(pc2.localDescription!);
    await negotiate(pc1, pc2);
  } finally {
    pc1.close();
    pc2.close();
    pc3.close();
  }
});
