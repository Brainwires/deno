// Copyright 2018-2026 the Deno authors. MIT license.

// `navigator.mediaDevices` with its fake camera and microphone
// (`DENO_FAKE_MEDIA_DEVICES=1`, set by tests/unit/mod.rs), the media they
// send, and WebRTC Encoded Transform (`RTCRtpScriptTransform`).

import { assert, assertEquals, assertRejects } from "./test_util.ts";

function assertInstanceOf(
  value: unknown,
  ctor: new (...args: never[]) => unknown,
) {
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
  let timer: ReturnType<typeof setTimeout>;
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
    assertEquals(
      copy.getMetadata().synchronizationSource,
      s.synchronizationSource,
    );
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

function hex(s: string): Uint8Array<ArrayBuffer> {
  const clean = s.replace(/\s+/g, "");
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; ++i) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function baseKey(raw: Uint8Array<ArrayBuffer>): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", raw, "HKDF", false, ["deriveBits"]);
}

// RFC 9605 Appendix C.3: SFrame encryption and decryption.
const SFRAME_VECTORS: [string, string][] = [
  [
    "AES_128_CTR_HMAC_SHA256_80",
    "9901234567449408b6f490086165b9d6f62b24ae1a59a56486b4ae8ed036b88912e24f11",
  ],
  [
    "AES_128_CTR_HMAC_SHA256_64",
    "99012345673f31438db4d09434e43afa0f8a2f00867a2be085046a9f5cb4f101d607",
  ],
  [
    "AES_128_CTR_HMAC_SHA256_32",
    "990123456717fc8af28a5a695afcfc6c8df6358a17e26b2fcb3bae32e443",
  ],
  [
    "AES_128_GCM_SHA256_128",
    "9901234567b7412c2513a1b66dbb48841bbaf17f598751176ad847681a69c6d0b091c07018ce4adb34eb",
  ],
  [
    "AES_256_GCM_SHA512_128",
    "990123456794f509d36e9beacb0e261d99c7d1e972f1fed787d4049f17ca21353c1cc24d56ceabced279",
  ],
];

Deno.test("sframeMatchesRfc9605Vectors", async () => {
  // Loads 04_sframe.js, which installs the test hook.
  assertEquals(typeof SFrameEncryptorStream, "function");
  // @ts-ignore internal test hook
  const vector = Deno[Deno.internal].sframeVector;
  const key = await baseKey(hex("000102030405060708090a0b0c0d0e0f"));
  const metadata = hex("4945544620534672616d65205747");
  const pt = hex("64726166742d696574662d736672616d652d656e63");
  for (const [suite, ct] of SFRAME_VECTORS) {
    const out = await vector(suite, key, 0x123n, 0x4567n, metadata, pt);
    assertEquals(toHex(out.ct), ct, suite);
    assertEquals(toHex(out.pt), toHex(pt), suite);
  }
});

Deno.test("sframeStreamsRoundTrip", async () => {
  const raw = crypto.getRandomValues(new Uint8Array(16));
  const key = await baseKey(raw);
  for (
    const cipherSuite of [
      "AES_128_CTR_HMAC_SHA256_80",
      "AES_128_GCM_SHA256_128",
    ] as const
  ) {
    const encryptor = new SFrameEncryptorStream({ cipherSuite });
    const decryptor = new SFrameDecryptorStream({ cipherSuite });
    await encryptor.setEncryptionKey(key, 300n);
    await decryptor.addDecryptionKey(key, 300);
    const writer = encryptor.writable.getWriter();
    const reader = decryptor.readable.getReader();
    encryptor.readable.pipeTo(decryptor.writable);
    const sent = Uint8Array.from({ length: 64 }, (_, i) => i);
    writer.write(sent);
    writer.write(sent.subarray(8));
    const a = await reader.read();
    const b = await reader.read();
    assertEquals(new Uint8Array(a.value as ArrayBuffer), sent);
    assertEquals(new Uint8Array(b.value as ArrayBuffer), sent.subarray(8));
    await writer.close();
    await reader.cancel();
  }
});

Deno.test("sframeDecryptorReportsErrors", async () => {
  const cipherSuite = "AES_128_CTR_HMAC_SHA256_80";
  const key = await baseKey(new Uint8Array(16));
  const other = await baseKey(new Uint8Array(16).fill(1));
  const encryptor = new SFrameEncryptorStream({ cipherSuite });
  await encryptor.setEncryptionKey(key, 5);
  const ew = encryptor.writable.getWriter();
  const er = encryptor.readable.getReader();
  const [, first] = await Promise.all([
    ew.write(new Uint8Array([1, 2, 3, 4])),
    er.read(),
  ]);
  const encrypted = first.value as ArrayBuffer;

  const decryptor = new SFrameDecryptorStream({ cipherSuite });
  const errors: SFrameTransformErrorEvent[] = [];
  decryptor.onerror = (e) => errors.push(e as SFrameTransformErrorEvent);
  const dw = decryptor.writable.getWriter();
  const dr = decryptor.readable.getReader();
  // The readable has no queue: each write is taken while a read waits.
  const roundTrip = async (chunk: BufferSource) =>
    (await Promise.all([dw.write(chunk), dr.read()]))[1].value;
  // No key for KID 5.
  assertEquals(await roundTrip(encrypted), new ArrayBuffer(0));
  // The wrong key: authentication fails.
  await decryptor.addDecryptionKey(other, 5);
  await roundTrip(encrypted);
  // An empty frame has no header.
  await roundTrip(new Uint8Array(0));
  assertEquals(errors.map((e) => e.errorType), [
    "keyID",
    "authentication",
    "syntax",
  ]);
  assertEquals(errors[0].keyID, 5);
  // A chunk of another type errors the stream.
  const read = dr.read().catch((e) => e);
  await assertRejects(() => dw.write({} as ArrayBuffer), TypeError);
  assert((await read) instanceof TypeError);

  let typeError = false;
  try {
    new SFrameEncryptorStream({ cipherSuite: "nope" as never });
  } catch (e) {
    typeError = e instanceof TypeError;
  }
  assert(typeError);
  let notSupported = false;
  try {
    new RTCRtpSFrameEncryptor({ cipherSuite: "AES_256_CTR_HMAC_SHA512_80" });
  } catch (e) {
    notSupported = (e as DOMException).name === "NotSupportedError";
  }
  assert(notSupported);
  await ew.close();
  await er.cancel();
});

Deno.test("sframeTransformsAttachOnce", async () => {
  const pc = new RTCPeerConnection();
  try {
    const encryptor = new RTCRtpSFrameEncryptor({
      cipherSuite: "AES_128_GCM_SHA256_128",
    });
    const decryptor = new RTCRtpSFrameDecryptor({
      cipherSuite: "AES_128_GCM_SHA256_128",
    });
    const t1 = pc.addTransceiver("audio");
    const t2 = pc.addTransceiver("video");
    t1.sender.transform = encryptor;
    t1.receiver.transform = decryptor;
    assert(t1.sender.transform === encryptor);
    for (
      const set of [
        () => t2.sender.transform = encryptor,
        () => t2.receiver.transform = decryptor,
      ]
    ) {
      let name = "";
      try {
        set();
      } catch (e) {
        name = (e as DOMException).name;
      }
      assertEquals(name, "InvalidStateError");
    }
    // An encryptor is no receiver transform.
    let typeError = false;
    try {
      // @ts-ignore deliberately the wrong kind
      t2.receiver.transform = encryptor;
    } catch (e) {
      typeError = e instanceof TypeError;
    }
    assert(typeError);
    t1.sender.transform = null;
    assertEquals(t1.sender.transform, null);
    await Promise.resolve();
  } finally {
    pc.close();
  }
});

Deno.test("sframeEncryptsWhatASenderSends", async () => {
  const cipherSuite = "AES_128_CTR_HMAC_SHA256_80";
  const key = await baseKey(crypto.getRandomValues(new Uint8Array(16)));
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    const sender = a.addTrack(stream.getTracks()[0], stream);
    const encryptor = new RTCRtpSFrameEncryptor({ cipherSuite });
    await encryptor.setEncryptionKey(key, 9);
    sender.transform = encryptor;
    const decryptor = new RTCRtpSFrameDecryptor({ cipherSuite });
    const errors: string[] = [];
    // Without its key the receiver can't decrypt: the frames are SFrames
    // with KID 9.
    const keyError = new Promise<SFrameTransformErrorEvent>((resolve) => {
      decryptor.onerror = (e) => {
        errors.push((e as SFrameTransformErrorEvent).errorType);
        resolve(e as SFrameTransformErrorEvent);
      };
    });
    b.ontrack = (e) => e.receiver.transform = decryptor;
    await connect(a, b);
    const error = await timeout(keyError, 15_000, "SFrame error");
    assertEquals(error.errorType, "keyID");
    assertEquals(error.keyID, 9);
    // With it, they decrypt.
    await decryptor.addDecryptionKey(key, 9);
    const count = errors.length;
    await new Promise((r) => setTimeout(r, 300));
    assert(errors.length <= count + 2, `errors after the key: ${errors}`);
  } finally {
    a.close();
    b.close();
    stream.getTracks().forEach((t) => t.stop());
  }
});

const KEY_FRAME_WORKER = `
self.onrtctransform = async (event) => {
  const t = event.transformer;
  const reader = t.readable.getReader();
  const writer = t.writable.getWriter();
  if (t.options.name === "receiver") {
    t.generateKeyFrame().catch((e) => self.postMessage(e.name));
  } else {
    t.generateKeyFrame().then(() => self.postMessage("generated"));
  }
  while (true) {
    const { value, done } = await reader.read();
    if (done) return;
    await writer.write(value);
  }
};
`;

Deno.test("scriptTransformGeneratesKeyFrames", async () => {
  const url = `data:application/javascript,${
    encodeURIComponent(KEY_FRAME_WORKER)
  }`;
  const worker = new Worker(url, { type: "module" });
  const messages: string[] = [];
  const both = new Promise<void>((resolve) => {
    worker.onmessage = (e) => {
      messages.push(e.data);
      if (messages.length === 2) resolve();
    };
  });
  const stream = await navigator.mediaDevices.getUserMedia({ video: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    const sender = a.addTrack(stream.getTracks()[0], stream);
    sender.transform = new RTCRtpScriptTransform(worker, { name: "sender" });
    b.ontrack = (e) => {
      e.receiver.transform = new RTCRtpScriptTransform(worker, {
        name: "receiver",
      });
    };
    await connect(a, b);
    await timeout(both, 20_000, "generateKeyFrame");
    // A sender's resolves at its next key frame; a receiver's rejects.
    assertEquals(messages.sort(), ["InvalidStateError", "generated"]);
  } finally {
    a.close();
    b.close();
    worker.terminate();
    stream.getTracks().forEach((t) => t.stop());
  }
});
