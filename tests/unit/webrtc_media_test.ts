// Copyright 2018-2026 the Deno authors. MIT license.

// `navigator.mediaDevices` with its fake camera and microphone
// (`DENO_FAKE_MEDIA_DEVICES=1`, set by tests/unit/mod.rs), the media they
// send, and WebRTC Encoded Transform (`RTCRtpScriptTransform`).

import { assert, assertEquals, assertRejects } from "./test_util.ts";

// deno-lint-ignore no-explicit-any
function assertInstanceOf(value: unknown, ctor: new (...args: any[]) => unknown) {
  assert(value instanceof ctor, `expected an instance of ${ctor.name}`);
}

function withoutFakeDevices<T>(fn: () => Promise<T>): Promise<T> {
  const previous = Deno.env.get("DENO_FAKE_MEDIA_DEVICES");
  Deno.env.set("DENO_FAKE_MEDIA_DEVICES", "0");
  return fn().finally(() => {
    if (previous === undefined) Deno.env.delete("DENO_FAKE_MEDIA_DEVICES");
    else Deno.env.set("DENO_FAKE_MEDIA_DEVICES", previous);
  });
}

async function connect(a: RTCPeerConnection, b: RTCPeerConnection) {
  a.onicecandidate = (e) => e.candidate && b.addIceCandidate(e.candidate);
  b.onicecandidate = (e) => e.candidate && a.addIceCandidate(e.candidate);
  await a.setLocalDescription();
  await b.setRemoteDescription(a.localDescription!);
  await b.setLocalDescription();
  await a.setRemoteDescription(b.localDescription!);
}

function timeout<T>(p: Promise<T>, ms = 15_000, what = "event"): Promise<T> {
  let timer: number;
  return Promise.race([
    p.finally(() => clearTimeout(timer)),
    new Promise<T>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`timed out: ${what}`)), ms);
    }),
  ]);
}

Deno.test("mediaDevicesHasNoDevicesWithoutTheFlag", async () => {
  await withoutFakeDevices(async () => {
    assertEquals(await navigator.mediaDevices.enumerateDevices(), []);
    const error = await assertRejects(
      () => navigator.mediaDevices.getUserMedia({ audio: true }),
      DOMException,
    );
    assertEquals(error.name, "NotFoundError");
  });
});

Deno.test("mediaDevicesIsSameObject", () => {
  assert(navigator.mediaDevices === navigator.mediaDevices);
  assertInstanceOf(navigator.mediaDevices, MediaDevices);
  assertInstanceOf(navigator.mediaDevices, EventTarget);
  const supported = navigator.mediaDevices.getSupportedConstraints();
  assertEquals(supported.width, true);
  assertEquals(supported.channelCount, true);
});

Deno.test("mediaDevicesEnumeratesTheFakeDevices", async () => {
  const devices = await navigator.mediaDevices.enumerateDevices();
  assertEquals(devices.map((d) => d.kind), ["audioinput", "videoinput"]);
  for (const d of devices) {
    assertInstanceOf(d, InputDeviceInfo);
    assert(d.deviceId.length > 0);
    assertEquals(d.toJSON().deviceId, d.deviceId);
  }
  const camera = (devices[1] as InputDeviceInfo).getCapabilities();
  assertEquals(camera.width, { min: 640, max: 640 });
  assertEquals(camera.frameRate, { min: 30, max: 30 });
  const mic = (devices[0] as InputDeviceInfo).getCapabilities();
  assertEquals(mic.channelCount, { min: 1, max: 2 });
});

Deno.test("getUserMediaGivesFakeTracks", async () => {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: true,
    video: true,
  });
  const [audio, video] = stream.getTracks();
  assertEquals(audio.kind, "audio");
  assertEquals(video.kind, "video");
  assertEquals(audio.readyState, "live");
  const a = audio.getSettings();
  assertEquals(a.sampleRate, 48000);
  assertEquals(a.channelCount, 1);
  assertEquals(a.deviceId, "fake-microphone");
  const v = video.getSettings();
  assertEquals([v.width, v.height, v.frameRate], [640, 480, 30]);
  // A clone keeps the settings; stopping one leaves the other live.
  const clone = video.clone();
  assertEquals(clone.getSettings().width, 640);
  video.stop();
  assertEquals(clone.readyState, "live");
  clone.stop();
  audio.stop();
});

Deno.test("getUserMediaChecksItsConstraints", async () => {
  await assertRejects(
    () => navigator.mediaDevices.getUserMedia({}),
    TypeError,
  );
  const error = await assertRejects(
    () =>
      navigator.mediaDevices.getUserMedia({
        video: { width: { exact: 1280 } },
      }),
    OverconstrainedError,
  );
  assertEquals(error.constraint, "width");
  assertEquals(error.name, "OverconstrainedError");
  // Ideal values the device can't meet are fine.
  const stream = await navigator.mediaDevices.getUserMedia({
    video: { width: 1280, height: { ideal: 720 } },
    audio: { channelCount: { exact: 2 }, echoCancellation: false },
  });
  const [audio, video] = stream.getTracks();
  assertEquals(audio.getSettings().channelCount, 2);
  assertEquals(audio.getSettings().echoCancellation, false);
  assertEquals(video.getSettings().width, 640);
  assertEquals(audio.getConstraints().channelCount, { exact: 2 });
  await audio.applyConstraints({ channelCount: 1 });
  assertEquals(audio.getSettings().channelCount, 1);
  await assertRejects(
    () => audio.applyConstraints({ sampleRate: { exact: 8000 } }),
    OverconstrainedError,
  );
  audio.stop();
  video.stop();
});

Deno.test("fakeTracksSendMedia", async () => {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: true,
    video: true,
  });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    for (const track of stream.getTracks()) a.addTrack(track, stream);
    const tracks: RTCRtpReceiver[] = [];
    const both = new Promise<void>((resolve) => {
      b.ontrack = (e) => {
        tracks.push(e.receiver);
        if (tracks.length === 2) resolve();
      };
    });
    await connect(a, b);
    await timeout(both, 15_000, "ontrack");
    for (const receiver of tracks) {
      const reader = Deno.webrtc.readRtp(receiver).getReader();
      const { value } = await timeout(reader.read(), 15_000, "rtp");
      assert(value && value.byteLength > 12);
      await reader.cancel();
    }
    const sources = tracks[0].getSynchronizationSources();
    assertEquals(sources.length, 1);
  } finally {
    a.close();
    b.close();
    stream.getTracks().forEach((t) => t.stop());
  }
});

const TRANSFORM_WORKER = `
self.onrtctransform = async (event) => {
  const t = event.transformer;
  const reader = t.readable.getReader();
  const writer = t.writable.getWriter();
  let first = true;
  while (true) {
    const { value, done } = await reader.read();
    if (done) return;
    if (first) {
      first = false;
      self.postMessage({ name: t.options.name, frame: value });
    }
    await writer.write(value);
  }
};
self.postMessage("ready");
`;

Deno.test("scriptTransformSeesSentAndReceivedFrames", async () => {
  const url = `data:application/javascript,${
    encodeURIComponent(TRANSFORM_WORKER)
  }`;
  const worker = new Worker(url, { type: "module" });
  const messages: MessageEvent[] = [];
  const firstFrames = new Promise<void>((resolve) => {
    worker.onmessage = (e) => {
      if (e.data === "ready") return;
      messages.push(e);
      if (messages.length === 2) resolve();
    };
  });
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    const sender = a.addTrack(stream.getTracks()[0], stream);
    const senderTransform = new RTCRtpScriptTransform(worker, {
      name: "sender",
    });
    sender.transform = senderTransform;
    assert(sender.transform === senderTransform);
    b.ontrack = (e) => {
      e.receiver.transform = new RTCRtpScriptTransform(worker, {
        name: "receiver",
      });
    };
    await connect(a, b);
    await timeout(firstFrames, 20_000, "transformed frames");
    const byName = Object.fromEntries(
      messages.map((m) => [m.data.name, m.data.frame]),
    );
    const sent = byName.sender as RTCEncodedAudioFrame;
    const received = byName.receiver as RTCEncodedAudioFrame;
    // Frames are Serializable: they come back as frames.
    assertInstanceOf(sent, RTCEncodedAudioFrame);
    assertInstanceOf(received, RTCEncodedAudioFrame);
    assert(sent.data.byteLength > 0);
    const s = sent.getMetadata();
    const r = received.getMetadata();
    assertEquals(s.mimeType?.toLowerCase(), "audio/opus");
    assertEquals(r.mimeType?.toLowerCase(), "audio/opus");
    assertEquals(typeof s.captureTime, "number");
    assertEquals(typeof r.receiveTime, "number");
    assertEquals(typeof r.sequenceNumber, "number");
    // What the worker wrote back went out: the receiver saw the sender's
    // stream, at its timestamps.
    assertEquals(r.synchronizationSource, s.synchronizationSource);
    assertEquals(r.payloadType, s.payloadType);
    // A copy keeps the metadata, and takes new values for it.
    const copy = new RTCEncodedAudioFrame(sent, {
      metadata: { rtpTimestamp: 7 },
    });
    assertEquals(copy.getMetadata().rtpTimestamp, 7);
    assertEquals(copy.getMetadata().synchronizationSource, s.synchronizationSource);
    // A transform is used by one sender or receiver at a time.
    const other = a.addTransceiver("audio").sender;
    let threw = false;
    try {
      other.transform = senderTransform;
    } catch (e) {
      threw = (e as DOMException).name === "InvalidStateError";
    }
    assert(threw);
  } finally {
    a.close();
    b.close();
    worker.terminate();
    stream.getTracks().forEach((t) => t.stop());
  }
});
