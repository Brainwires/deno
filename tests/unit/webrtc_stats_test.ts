// Copyright 2018-2026 the Deno authors. MIT license.

// getStats() as webrtc-stats defines the report: which objects exist at
// each step of a negotiation, their references, and the members derived
// from the connection's own state. Nothing here connects: the descriptions
// are exchanged without candidates. tests/unit/mod.rs gives this file the
// fake camera and microphone.

import { assert, assertEquals } from "./test_util.ts";

function typesOf(report: RTCStatsReport): string[] {
  return [...new Set([...report.values()].map((s) => s.type))].sort();
}

function ofType(report: RTCStatsReport, type: string) {
  return [...report.values()].filter((s) => s.type === type);
}

function assertReferencesResolve(report: RTCStatsReport) {
  for (const stats of report.values()) {
    for (const [key, value] of Object.entries(stats)) {
      if (key.endsWith("Id")) {
        assert(
          typeof value === "string" && report.has(value),
          `${stats.type}.${key} (${value}) resolves`,
        );
      }
    }
  }
}

async function negotiate(a: RTCPeerConnection, b: RTCPeerConnection) {
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription!);
}

Deno.test("webrtcStatsBeforeAnyDescription", async () => {
  const pc = new RTCPeerConnection();
  pc.createDataChannel("wpt");
  const report = await pc.getStats();
  // No RTCDtlsTransport yet, so no `transport`; the channel's object exists
  // from createDataChannel().
  assertEquals(typesOf(report), ["data-channel", "peer-connection"]);
  const [channel] = ofType(report, "data-channel");
  assertEquals(channel.label, "wpt");
  assertEquals(channel.state, "connecting");
  assertEquals(channel.messagesSent, 0);
  assert(!("dataChannelIdentifier" in channel));
  // Stats objects are ordinary dictionaries.
  for (const stats of report.values()) {
    assertEquals(Object.getPrototypeOf(stats), Object.prototype);
    assert(Object.prototype.hasOwnProperty.call(stats, "id"));
    // What WPT's assert_own_property calls.
    // deno-lint-ignore no-prototype-builtins
    assert(stats.hasOwnProperty("type"));
  }
  pc.close();
});

Deno.test("webrtcStatsTransportAfterLocalOffer", async () => {
  const pc = new RTCPeerConnection();
  pc.createDataChannel("wpt");
  await pc.setLocalDescription();
  const report = await pc.getStats();
  const [transport] = ofType(report, "transport");
  assert(transport !== undefined);
  assertEquals(transport.iceRole, "controlling");
  assertEquals(transport.iceState, "new");
  assertEquals(transport.dtlsState, "new");
  assertEquals(transport.dtlsRole, "unknown");
  assertEquals(transport.selectedCandidatePairChanges, 0);
  assert(!("selectedCandidatePairId" in transport));
  assertReferencesResolve(report);
  pc.close();
});

Deno.test("webrtcStatsMediaSourceOnAttach", async () => {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const [track] = stream.getTracks();
  const pc = new RTCPeerConnection();
  pc.addTrack(track, stream);
  const report = await pc.getStats();
  const [source] = ofType(report, "media-source");
  assert(source !== undefined, typesOf(report).join(","));
  assertEquals(source.trackIdentifier, track.id);
  assertEquals(source.kind, "audio");
  assertEquals(ofType(report, "outbound-rtp").length, 0);
  assertEquals(ofType(report, "codec").length, 0);
  assertReferencesResolve(report);
  pc.close();
  track.stop();
});

Deno.test("webrtcStatsNoRemoteStreamsBeforeRtcp", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  a.addTransceiver("audio");
  await negotiate(a, b);
  const report = await a.getStats();
  const [outbound] = ofType(report, "outbound-rtp");
  assert(outbound !== undefined, typesOf(report).join(","));
  // webrtc-stats: encodingIndex is for video streams only.
  assertEquals(outbound.encodingIndex, undefined);
  assertEquals(outbound.active, true);
  // No RTCP has been received: no remote stream objects.
  assertEquals(ofType(report, "remote-inbound-rtp").length, 0);
  assertEquals(ofType(report, "remote-outbound-rtp").length, 0);
  assertReferencesResolve(report);
  a.close();
  b.close();
});

Deno.test("webrtcStatsOutboundActiveFollowsParameters", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const { sender } = a.addTransceiver("video");
  await negotiate(a, b);
  const params = sender.getParameters();
  params.encodings[0].active = false;
  await sender.setParameters(params);
  const [outbound] = ofType(await a.getStats(), "outbound-rtp");
  assert(outbound !== undefined);
  assertEquals(outbound.active, false);
  assertEquals(outbound.encodingIndex, 0);
  a.close();
  b.close();
});

Deno.test("webrtcStatsStoppedTransceiverAfterRenegotiation", async () => {
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  const transceiver = a.addTransceiver("audio");
  await negotiate(a, b);
  const first = await transceiver.sender.getStats();
  assertEquals(ofType(first, "outbound-rtp").length, 1);
  transceiver.stop();
  // stop() alone keeps the stream's stats until the negotiation completes.
  const second = await transceiver.sender.getStats();
  assertEquals(ofType(second, "outbound-rtp").length, 1);
  await negotiate(a, b);
  assertEquals(transceiver.currentDirection, "stopped");
  assertEquals(ofType(await transceiver.sender.getStats(), "outbound-rtp"), []);
  assertEquals(ofType(await a.getStats(), "outbound-rtp"), []);
  a.close();
  b.close();
});
