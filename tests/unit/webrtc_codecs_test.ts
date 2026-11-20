// Copyright 2018-2026 the Deno authors. MIT license.

// RTCRtpParameters: encodings[].codec, the dictionary members, the
// transaction rules, the negotiated codec lists, simulcast encodings and the
// SDP rules for payload types and unknown media types.

import {
  assert,
  assertEquals,
  assertRejects,
  assertThrows,
} from "./test_util.ts";

function codecOf(kind: string, mimeType: string) {
  const codec = RTCRtpSender.getCapabilities(kind)!.codecs.find((c) =>
    c.mimeType.toLowerCase() === mimeType.toLowerCase()
  );
  assert(codec !== undefined, `no ${mimeType} capability`);
  return codec;
}

Deno.test(function webrtcEncodingCodecIsKeptAndValidated() {
  const pc = new RTCPeerConnection();
  try {
    const opus = codecOf("audio", "audio/opus");
    const { sender } = pc.addTransceiver("audio", {
      sendEncodings: [{ codec: opus }],
    });
    const { codec } = sender.getParameters().encodings[0];
    assertEquals(codec?.mimeType, opus.mimeType);
    assertEquals(codec?.clockRate, opus.clockRate);
    assertEquals(codec?.sdpFmtpLine, opus.sdpFmtpLine);
    assertEquals(
      pc.addTransceiver("audio").sender.getParameters().encodings[0].codec,
      undefined,
    );
    assertThrows(
      () =>
        pc.addTransceiver("video", {
          sendEncodings: [{ codec: { mimeType: "video/nope", clockRate: 1 } }],
        }),
      DOMException,
      "",
    );
  } finally {
    pc.close();
  }
});

Deno.test(async function webrtcSetParametersRejectsAnUnknownCodec() {
  const pc = new RTCPeerConnection();
  try {
    const { sender } = pc.addTransceiver("video");
    const params = sender.getParameters();
    params.encodings[0].codec = { mimeType: "video/nope", clockRate: 90000 };
    const error = await assertRejects(() => sender.setParameters(params));
    assertEquals((error as DOMException).name, "InvalidModificationError");
  } finally {
    pc.close();
  }
});

Deno.test(async function webrtcEncodingComplexityAndScalabilityMode() {
  const pc = new RTCPeerConnection();
  try {
    assertThrows(
      () =>
        pc.addTransceiver("video", {
          sendEncodings: [
            { encodingComplexity: "nope" as RTCEncodingComplexity },
          ],
        }),
      TypeError,
    );
    const error = (() => {
      try {
        pc.addTransceiver("video", {
          sendEncodings: [{ scalabilityMode: "TotalNonsense" }],
        });
      } catch (e) {
        return e as DOMException;
      }
    })();
    assertEquals(error?.name, "OperationError");
    const { sender } = pc.addTransceiver("video", {
      sendEncodings: [{ scalabilityMode: "L1T3", encodingComplexity: "high" }],
    });
    const params = sender.getParameters();
    const encoding = params.encodings[0];
    assertEquals(encoding.scalabilityMode, "L1T3");
    assertEquals(encoding.encodingComplexity, "high");
    encoding.scalabilityMode = "L1T2";
    delete encoding.encodingComplexity;
    await sender.setParameters(params);
    const updated = sender.getParameters().encodings[0];
    assertEquals(updated.scalabilityMode, "L1T2");
    assert(!("encodingComplexity" in updated));
    // Audio has neither.
    const audio = pc.addTransceiver("audio", {
      sendEncodings: [{ encodingComplexity: "high" }],
    });
    assert(
      !("encodingComplexity" in audio.sender.getParameters().encodings[0]),
    );
  } finally {
    pc.close();
  }
});

Deno.test(async function webrtcTransactionIdLastsTheTask() {
  const pc = new RTCPeerConnection();
  try {
    const { sender } = pc.addTransceiver("audio");
    const first = sender.getParameters();
    await undefined;
    assertEquals(sender.getParameters().transactionId, first.transactionId);
    // The same parameters may be set twice before the task ends.
    const a = sender.setParameters(first);
    await sender.setParameters(first);
    await a;
    const unset: Partial<RTCRtpSendParameters> = { ...sender.getParameters() };
    delete unset.transactionId;
    await assertRejects(
      () => sender.setParameters(unset as RTCRtpSendParameters),
      TypeError,
    );
  } finally {
    pc.close();
  }
});

Deno.test(async function webrtcEncodingOptionsLengthIsChecked() {
  const pc = new RTCPeerConnection();
  try {
    const { sender } = pc.addTransceiver("video");
    const error = await assertRejects(() =>
      sender.setParameters(sender.getParameters(), {
        encodingOptions: [{ keyFrame: true }, { keyFrame: true }],
      })
    );
    assertEquals((error as DOMException).name, "InvalidModificationError");
  } finally {
    pc.close();
  }
});

const AUDIO_ANSWER = (mid: string) =>
  `v=0
o=- 166855176514521964 2 IN IP4 127.0.0.1
s=-
t=0 0
m=audio 9 UDP/TLS/RTP/SAVPF 121 111 0 101
c=IN IP4 0.0.0.0
a=rtcp:9 IN IP4 0.0.0.0
a=ice-ufrag:foobarba
a=ice-pwd:AAAA+BBBB+CCCC+DDDD+EE
a=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00
a=setup:passive
a=mid:${mid}
a=sendrecv
a=rtcp-mux
a=rtcp-rsize
a=rtpmap:101 telephone-event/8000/1
a=rtpmap:121 flarglblurp/8000/2
a=rtpmap:111 opus/48000/2
a=fmtp:111 maxaveragebitrate=20001;unknownparam=foo
`.replaceAll("\n", "\r\n");

Deno.test(async function webrtcSenderCodecsFollowTheRemoteDescription() {
  const pc = new RTCPeerConnection();
  try {
    const transceiver = pc.addTransceiver("audio");
    await pc.setLocalDescription();
    await pc.setRemoteDescription({
      type: "answer",
      sdp: AUDIO_ANSWER(transceiver.mid!),
    });
    const codecs = transceiver.sender.getParameters().codecs;
    assertEquals(
      codecs.map((c) => [c.payloadType, c.mimeType.toLowerCase()]),
      [[111, "audio/opus"], [0, "audio/pcmu"], [101, "audio/telephone-event"]],
    );
    assertEquals(
      codecs[0].sdpFmtpLine,
      "maxaveragebitrate=20001;unknownparam=foo",
    );
    assertEquals(codecs[1].sdpFmtpLine, undefined);
    assertEquals(codecs[1].channels, 1);
  } finally {
    pc.close();
  }
});

const OPUS_OFFER = (pt: number) =>
  `v=0
o=- 0 3 IN IP4 127.0.0.1
s=-
t=0 0
a=fingerprint:sha-256 A7:24:72:CA:6E:02:55:39:BA:66:DF:6E:CC:4C:D8:B0:1A:BF:1A:56:65:7D:F4:03:AD:7E:77:43:2A:29:EC:93
a=ice-ufrag:6HHHdzzeIhkE0CKj
a=ice-pwd:XYDGVpfvklQIEnZ6YnyLsAew
m=audio 9 RTP/SAVPF ${pt}
c=IN IP4 0.0.0.0
a=rtcp-mux
a=sendonly
a=mid:audio
a=rtpmap:${pt} opus/48000/2
a=setup:actpass
`.replaceAll("\n", "\r\n");

Deno.test(async function webrtcPayloadTypesThatCollideWithRtcpAreRejected() {
  const pc = new RTCPeerConnection();
  try {
    const error = await assertRejects(() =>
      pc.setRemoteDescription({ type: "offer", sdp: OPUS_OFFER(72) })
    );
    assertEquals((error as DOMException).name, "InvalidAccessError");
  } finally {
    pc.close();
  }
});

Deno.test(async function webrtcUnknownMediaTypeIsAnsweredRejected() {
  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection();
  try {
    pc1.addTransceiver("audio");
    const offer = await pc1.createOffer();
    await pc2.setRemoteDescription({
      type: "offer",
      sdp: offer.sdp!.replace("m=audio ", "m=unicorns "),
    });
    const answer = await pc2.createAnswer();
    const sections = answer.sdp!.split("\r\nm=");
    assertEquals(sections.length, 2);
    assert(sections[1].startsWith("unicorns 0 "), sections[1]);
    assertEquals(pc2.getTransceivers().length, 0);
  } finally {
    pc1.close();
    pc2.close();
  }
});

Deno.test(async function webrtcRemoteSimulcastOfferGivesEncodings() {
  const pc1 = new RTCPeerConnection();
  const pc2 = new RTCPeerConnection();
  try {
    const sender = pc1.addTransceiver("video").sender;
    // pc2 offers to receive two layers from pc1's transceiver.
    pc2.addTransceiver("video", { direction: "recvonly" });
    await pc2.setLocalDescription();
    const offer = pc2.localDescription!.sdp.replace(
      /(a=mid:[^\r\n]*\r\n)/,
      "$1a=rid:foo recv\r\na=rid:bar recv\r\na=simulcast:recv foo;bar\r\n",
    );
    await pc1.setRemoteDescription({ type: "offer", sdp: offer });
    // addTransceiver's transceiver can't take a remote offer's section: a
    // new one was made, with the offered layers.
    const remote = pc1.getTransceivers().find((t) => t.sender !== sender)!;
    assertEquals(
      remote.sender.getParameters().encodings.map((e) => e.rid),
      ["foo", "bar"],
    );
    assertEquals(
      remote.sender.getParameters().encodings.map((e) =>
        e.scaleResolutionDownBy
      ),
      [2, 1],
    );
    await pc1.setRemoteDescription({ type: "rollback" });
    for (const t of pc1.getTransceivers()) {
      assertEquals(
        t.sender.getParameters().encodings.map((e) => e.rid),
        [undefined],
      );
    }
  } finally {
    pc1.close();
    pc2.close();
  }
});
