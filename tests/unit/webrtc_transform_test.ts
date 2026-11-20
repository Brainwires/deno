// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC Encoded Transform beyond the basics in webrtc_media_test.ts: a
// transform's owner, the write rules of a script transformer, key frame
// requests, frame metadata, and the legacy `createEncodedStreams()`. Runs
// with the fake camera and microphone (`DENO_FAKE_MEDIA_DEVICES=1`) and
// loopback gathering (tests/unit/mod.rs).

import { assert, assertEquals, assertRejects } from "./test_util.ts";

type AnyFrame = RTCEncodedAudioFrame | RTCEncodedVideoFrame;
interface EncodedStreams {
  readable: ReadableStream<AnyFrame>;
  writable: WritableStream<AnyFrame>;
}

// `createEncodedStreams()` is not in the DOM typings (Chrome's API).
function streamsOf(owner: RTCRtpSender | RTCRtpReceiver): EncodedStreams {
  return (owner as unknown as { createEncodedStreams(): EncodedStreams })
    .createEncodedStreams();
}

function errorName(fn: () => unknown): string {
  try {
    fn();
  } catch (e) {
    return (e as Error).name;
  }
  return "";
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

function moduleWorker(source: string): Worker {
  return new Worker(
    `data:application/javascript,${encodeURIComponent(source)}`,
    { type: "module" },
  );
}

Deno.test("transformStaysWithItsFirstOwner", () => {
  const worker = moduleWorker("self.onrtctransform = () => {};");
  const pc = new RTCPeerConnection();
  try {
    const transform = new RTCRtpScriptTransform(worker);
    const first = pc.addTransceiver("audio");
    const second = pc.addTransceiver("audio");
    first.sender.transform = transform;
    first.sender.transform = null;
    // Detached, it still belongs to the first sender.
    assertEquals(
      errorName(() => second.sender.transform = transform),
      "InvalidStateError",
    );
    assertEquals(
      errorName(() => second.receiver.transform = transform),
      "InvalidStateError",
    );
    first.sender.transform = transform;
    assert(first.sender.transform === transform);
  } finally {
    pc.close();
    worker.terminate();
  }
});

Deno.test("encodedStreamsAreCreatedOnce", async () => {
  const worker = moduleWorker("self.onrtctransform = () => {};");
  const pc = new RTCPeerConnection();
  try {
    const { sender, receiver } = pc.addTransceiver("audio");
    const streams = streamsOf(sender);
    assert(streams.readable instanceof ReadableStream);
    assert(streams.writable instanceof WritableStream);
    // The streams are no transform, and leave no room for one.
    assertEquals(sender.transform, null);
    assertEquals(errorName(() => streamsOf(sender)), "InvalidStateError");
    assertEquals(
      errorName(() => sender.transform = new RTCRtpScriptTransform(worker)),
      "InvalidStateError",
    );
    sender.transform = null;
    // A receiver with a transform has no streams.
    receiver.transform = new RTCRtpScriptTransform(worker);
    assertEquals(errorName(() => streamsOf(receiver)), "InvalidStateError");
    // Only frames can be written.
    await assertRejects(
      () => streams.writable.getWriter().write("x" as unknown as AnyFrame),
      TypeError,
    );
  } finally {
    pc.close();
    worker.terminate();
  }
});

Deno.test("encodedStreamsCarryWrittenFrames", async () => {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    const sender = a.addTrack(stream.getTracks()[0], stream);
    const out = streamsOf(sender);
    const received = new Promise<AnyFrame>((resolve) => {
      b.ontrack = (e) => {
        streamsOf(e.receiver).readable.getReader().read().then((r) =>
          resolve(r.value!)
        );
      };
    });
    await connect(a, b);
    const reader = out.readable.getReader();
    const writer = out.writable.getWriter();
    const { value: frame } = await timeout(reader.read(), 15_000, "a frame");
    assert(frame instanceof RTCEncodedAudioFrame);
    const metadata = frame.getMetadata();
    // The fake microphone's tone has a level.
    assert(typeof metadata.audioLevel === "number");
    assert(metadata.audioLevel > 0 && metadata.audioLevel < 1);
    const payload = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]);
    frame.data = payload.slice().buffer;
    await writer.write(frame);
    // Written once, a frame is consumed; its data stays readable.
    assertEquals(new Uint8Array(frame.data), payload);
    await assertRejects(() => writer.write(frame), DOMException);
    // The first frame written is the first the receiver gets, at its
    // timestamp.
    const got = await timeout(received, 15_000, "the received frame");
    assertEquals(new Uint8Array(got.data), payload);
    assertEquals(got.getMetadata().rtpTimestamp, metadata.rtpTimestamp);
    assertEquals(
      (got as unknown as { timestamp: number }).timestamp,
      metadata.rtpTimestamp,
    );
    assertEquals(typeof got.getMetadata().sequenceNumber, "number");
  } finally {
    a.close();
    b.close();
    stream.getTracks().forEach((t) => t.stop());
  }
});

// The sender writes a clone of its first frame (rejected), then the frame
// itself (accepted), then drops the rest; the receiver reports payloads.
const WRITE_RULES_WORKER = `
const hex = (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, "0")).join("");
self.onrtctransform = async ({ transformer }) => {
  const reader = transformer.readable.getReader();
  const writer = transformer.writable.getWriter();
  if (transformer.options.name === "receiver") {
    while (true) {
      const { value, done } = await reader.read();
      if (done) return;
      if (value.data.byteLength) self.postMessage(hex(value.data));
    }
  }
  const { value, done } = await reader.read();
  if (done) return;
  const clone = structuredClone(value);
  clone.data = new Uint8Array([0xba, 0xd0, 0xba, 0xd0]).buffer;
  writer.write(clone);
  value.data = new Uint8Array([0x60, 0x0d, 0xf0, 0x0d]).buffer;
  writer.write(value);
  writer.write(value);
  while (!(await reader.read()).done) {
    // dropped
  }
};
`;

Deno.test("scriptTransformerWritesOnlyItsOwnFrames", async () => {
  const worker = moduleWorker(WRITE_RULES_WORKER);
  const seen: string[] = [];
  const first = new Promise<void>((resolve) => {
    worker.onmessage = (e) => {
      seen.push(e.data);
      resolve();
    };
  });
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    a.addTrack(stream.getTracks()[0], stream).transform =
      new RTCRtpScriptTransform(worker, { name: "sender" });
    b.ontrack = (e) => {
      e.receiver.transform = new RTCRtpScriptTransform(worker, {
        name: "receiver",
      });
    };
    await connect(a, b);
    await timeout(first, 15_000, "the written frame");
    await new Promise((resolve) => setTimeout(resolve, 1000));
    assertEquals(seen, ["600df00d"]);
  } finally {
    a.close();
    b.close();
    worker.terminate();
    stream.getTracks().forEach((t) => t.stop());
  }
});

const KEY_FRAME_WORKER = `
self.onrtctransform = async ({ transformer }) => {
  const reader = transformer.readable.getReader();
  const { value } = await reader.read();
  const name = (p) => p.then(() => "resolved", (e) => e.name);
  const metadata = value.getMetadata();
  const pump = (async () => { while (!(await reader.read()).done) {} })();
  self.postMessage({
    type: value.type,
    frameId: typeof metadata.frameId,
    dependencies: metadata.dependencies,
    badRid: await name(transformer.generateKeyFrame("!?")),
    unknownRid: await name(transformer.generateKeyFrame("foo")),
    timestamp: typeof await transformer.generateKeyFrame(),
    sendKeyFrameRequest: await name(transformer.sendKeyFrameRequest()),
  });
  await pump;
};
`;

Deno.test("generateKeyFrameChecksRidsAndResolvesWithATimestamp", async () => {
  const worker = moduleWorker(KEY_FRAME_WORKER);
  const result = new Promise<Record<string, unknown>>((resolve) => {
    worker.onmessage = (e) => resolve(e.data);
  });
  const stream = await navigator.mediaDevices.getUserMedia({ video: true });
  const a = new RTCPeerConnection();
  const b = new RTCPeerConnection();
  try {
    a.addTrack(stream.getTracks()[0], stream).transform =
      new RTCRtpScriptTransform(worker);
    await connect(a, b);
    assertEquals(await timeout(result, 20_000, "generateKeyFrame"), {
      // The device's frames start with the connection, on its key frame.
      type: "key",
      frameId: "number",
      dependencies: [],
      badRid: "NotAllowedError",
      unknownRid: "NotFoundError",
      timestamp: "number",
      sendKeyFrameRequest: "InvalidStateError",
    });
  } finally {
    a.close();
    b.close();
    worker.terminate();
    stream.getTracks().forEach((t) => t.stop());
  }
});
