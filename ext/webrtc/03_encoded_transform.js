// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC Encoded Transform: `RTCRtpScriptTransform` (window), and in its
// worker `RTCRtpScriptTransformer`, `RTCTransformEvent` and
// `KeyFrameRequestEvent`; `RTCEncodedVideoFrame` / `RTCEncodedAudioFrame`
// in both.
//
// `sender.transform = new RTCRtpScriptTransform(worker, options)` hands the
// sender's encoded frames to the worker before they are packetized; it
// writes back what goes out. A sender's frames are those of a fake capture
// device (02_media_devices.js); a receiver's are its RTP packets put back
// together (encoded_frames.rs), and what its transform writes back is
// dropped, as there is no decoder. The two sides talk over a MessageChannel
// whose far port goes to the worker in a message it takes before any
// `message` listener (`receiveTransform`, from 99_main.js).

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, primordials } = __bootstrap;
const {
  op_webrtc_receiver_request_key_frame,
  op_webrtc_receiver_transform,
  op_webrtc_sender_fake_frame,
  op_webrtc_sender_write_frame_at,
} = core.ops;
const {
  ArrayBufferPrototypeGetByteLength,
  ArrayBufferPrototypeSlice,
  ArrayIsArray,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  MapPrototypeDelete,
  MapPrototypeGet,
  MapPrototypeSet,
  ObjectDefineProperty,
  ObjectPrototypeIsPrototypeOf,
  PromisePrototypeThen,
  PromiseReject,
  PromiseWithResolvers,
  SafeMap,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  Symbol,
  SymbolFor,
  TypeError,
  TypedArrayPrototypeGetBuffer,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeGetByteOffset,
  Uint8Array,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { defineEventHandler, Event, EventTarget, setIsTrusted } = core
  .loadExtScript("ext:deno_web/02_event.js");
const { ReadableStream, WritableStream } = core.loadExtScript(
  "ext:deno_web/06_streams.js",
);
const { MessageChannel } = core.loadExtScript(
  "ext:deno_web/13_message_port.js",
);
const { performance } = core.loadExtScript("ext:deno_web/15_performance.js");

// The key of the message that takes a transform's port to its worker.
const TRANSFORM_KEY = "__denoRtcRtpScriptTransform";

const illegalConstructorKey = Symbol("illegalConstructorKey");
const _data = Symbol("[[data]]");
const _metadata = Symbol("[[metadata]]");
const _type = Symbol("[[type]]");
const _duration = Symbol("[[duration]]");
const _state = Symbol("[[state]]");
// The transformer a frame was enqueued by, and its place in that
// transformer's readable (writeEncodedData's [[owner]] and [[counter]]).
const _owner = Symbol("[[owner]]");
const _counter = Symbol("[[counter]]");

function fire(target, event) {
  setIsTrusted(event, true);
  target.dispatchEvent(event);
}

function invalidState(message) {
  return new DOMException(message, "InvalidStateError");
}

// --- RTCEncodedVideoFrame / RTCEncodedAudioFrame -----------------------------

function copyMetadata(metadata) {
  const out = { ...metadata };
  if (ArrayIsArray(metadata.contributingSources)) {
    out.contributingSources = ArrayPrototypeSlice(metadata.contributingSources);
  }
  if (ArrayIsArray(metadata.dependencies)) {
    out.dependencies = ArrayPrototypeSlice(metadata.dependencies);
  }
  return out;
}

function copyBuffer(buffer) {
  return ArrayBufferPrototypeSlice(buffer, 0);
}

// Metadata given to a copy constructor may change what describes the frame
// but not where it comes from.
function mergeMetadata(original, given, prefix) {
  if (given === undefined) return copyMetadata(original);
  if (given === null || typeof given !== "object") {
    throw new TypeError(`${prefix}: 'metadata' must be a dictionary`);
  }
  const out = copyMetadata(original);
  for (const key in given) {
    if (given[key] !== undefined) out[key] = given[key];
  }
  return copyMetadata(out);
}

function initFrame(frame, init) {
  frame[_type] = init.type;
  frame[_data] = init.data;
  frame[_metadata] = init.metadata;
  frame[_duration] = init.duration;
  frame[webidl.brand] = webidl.brand;
}

class RTCEncodedVideoFrame {
  constructor(originalFrame, options = undefined) {
    if (originalFrame === illegalConstructorKey) {
      initFrame(this, options);
      return;
    }
    const prefix = "Failed to construct 'RTCEncodedVideoFrame'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    if (
      !ObjectPrototypeIsPrototypeOf(
        RTCEncodedVideoFramePrototype,
        originalFrame,
      )
    ) {
      throw new TypeError(
        `${prefix}: Argument 1 is not an RTCEncodedVideoFrame`,
      );
    }
    initFrame(this, {
      type: originalFrame[_type],
      data: copyBuffer(originalFrame[_data]),
      metadata: mergeMetadata(
        originalFrame[_metadata],
        options?.metadata,
        prefix,
      ),
      duration: originalFrame[_duration],
    });
  }

  get type() {
    webidl.assertBranded(this, RTCEncodedVideoFramePrototype);
    return this[_type];
  }
  get data() {
    webidl.assertBranded(this, RTCEncodedVideoFramePrototype);
    return this[_data];
  }
  set data(value) {
    webidl.assertBranded(this, RTCEncodedVideoFramePrototype);
    this[_data] = webidl.converters.ArrayBuffer(
      value,
      "Failed to set 'data' on 'RTCEncodedVideoFrame'",
    );
  }
  getMetadata() {
    webidl.assertBranded(this, RTCEncodedVideoFramePrototype);
    return copyMetadata(this[_metadata]);
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `RTCEncodedVideoFrame ${
      inspect({
        type: this[_type],
        byteLength: ArrayBufferPrototypeGetByteLength(this[_data]),
        metadata: this[_metadata],
      }, inspectOptions)
    }`;
  }
}
webidl.configureInterface(RTCEncodedVideoFrame);
const RTCEncodedVideoFramePrototype = RTCEncodedVideoFrame.prototype;

class RTCEncodedAudioFrame {
  constructor(originalFrame, options = undefined) {
    if (originalFrame === illegalConstructorKey) {
      initFrame(this, options);
      return;
    }
    const prefix = "Failed to construct 'RTCEncodedAudioFrame'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    if (
      !ObjectPrototypeIsPrototypeOf(
        RTCEncodedAudioFramePrototype,
        originalFrame,
      )
    ) {
      throw new TypeError(
        `${prefix}: Argument 1 is not an RTCEncodedAudioFrame`,
      );
    }
    initFrame(this, {
      type: undefined,
      data: copyBuffer(originalFrame[_data]),
      metadata: mergeMetadata(
        originalFrame[_metadata],
        options?.metadata,
        prefix,
      ),
      duration: originalFrame[_duration],
    });
  }

  get data() {
    webidl.assertBranded(this, RTCEncodedAudioFramePrototype);
    return this[_data];
  }
  set data(value) {
    webidl.assertBranded(this, RTCEncodedAudioFramePrototype);
    this[_data] = webidl.converters.ArrayBuffer(
      value,
      "Failed to set 'data' on 'RTCEncodedAudioFrame'",
    );
  }
  getMetadata() {
    webidl.assertBranded(this, RTCEncodedAudioFramePrototype);
    return copyMetadata(this[_metadata]);
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `RTCEncodedAudioFrame ${
      inspect({
        byteLength: ArrayBufferPrototypeGetByteLength(this[_data]),
        metadata: this[_metadata],
      }, inspectOptions)
    }`;
  }
}
webidl.configureInterface(RTCEncodedAudioFrame);
const RTCEncodedAudioFramePrototype = RTCEncodedAudioFrame.prototype;

function isFrame(value) {
  return ObjectPrototypeIsPrototypeOf(RTCEncodedVideoFramePrototype, value) ||
    ObjectPrototypeIsPrototypeOf(RTCEncodedAudioFramePrototype, value);
}

// Serializable: a frame posted to another realm arrives as a frame (its
// data copied, or moved when it is in the transfer list).
function makeSerializable(ctor, name) {
  ObjectDefineProperty(ctor.prototype, core.hostObjectBrand, {
    __proto__: null,
    value: function () {
      return {
        type: name,
        frameType: this[_type],
        data: this[_data],
        metadata: this[_metadata],
        duration: this[_duration],
      };
    },
    enumerable: false,
    configurable: false,
    writable: false,
  });
}
makeSerializable(RTCEncodedVideoFrame, "RTCEncodedVideoFrame");
makeSerializable(RTCEncodedAudioFrame, "RTCEncodedAudioFrame");

// Registered in every realm by 98_global_scope_shared.js.
function deserializeFrame(serialized) {
  const init = {
    type: serialized.frameType,
    data: serialized.data,
    metadata: serialized.metadata,
    duration: serialized.duration,
  };
  return serialized.type === "RTCEncodedVideoFrame"
    ? new RTCEncodedVideoFrame(illegalConstructorKey, init)
    : new RTCEncodedAudioFrame(illegalConstructorKey, init);
}

// --- RTCRtpScriptTransform (window) ------------------------------------------

let webrtc;
function internals() {
  return webrtc ??= core.loadExtScript("ext:deno_webrtc/01_webrtc.js")
    .mediaInternals;
}

function isWorker(value) {
  const Worker = globalThis.Worker;
  return typeof Worker === "function" &&
    ObjectPrototypeIsPrototypeOf(Worker.prototype, value);
}

class RTCRtpScriptTransform {
  constructor(workerOrWorkerAndParameters, options = undefined, transfer = []) {
    const prefix = "Failed to construct 'RTCRtpScriptTransform'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    let worker = workerOrWorkerAndParameters;
    if (!isWorker(worker) && worker !== null && typeof worker === "object") {
      worker = worker.worker;
    }
    if (!isWorker(worker)) {
      throw new TypeError(`${prefix}: Argument 1 is not a Worker`);
    }
    const list = transfer === undefined
      ? []
      : webidl.converters["sequence<object>"](transfer, prefix, "Argument 3");
    const { port1, port2 } = new MessageChannel();
    const transferList = [port2];
    for (let i = 0; i < list.length; ++i) {
      ArrayPrototypePush(transferList, list[i]);
    }
    worker.postMessage(
      { [TRANSFORM_KEY]: { options, port: port2 } },
      transferList,
    );
    this[_state] = {
      port: port1,
      target: null,
      side: null,
      // The sender or receiver it was first attached to (its [[owner]]),
      // with that side and the media kind: a transform serves one owner
      // for good, and its worker's key frame calls answer for that owner
      // even after it is detached.
      owner: null,
      ownerSide: null,
      kind: null,
      // generateKeyFrame() calls waiting for the next key frame.
      keyFrameWaiters: [],
      // The last key frame's size, for the delta frames' metadata.
      size: null,
      closed: false,
    };
    port1.onmessage = (event) => onTransformerMessage(this, event.data);
    // An unused transform doesn't keep the program alive.
    port1.unref();
    this[webidl.brand] = webidl.brand;
  }
}
webidl.configureInterface(RTCRtpScriptTransform);
const RTCRtpScriptTransformPrototype = RTCRtpScriptTransform.prototype;

function reply(state, id, error) {
  state.port.postMessage(
    error === undefined
      ? { resolve: id }
      : { reject: id, name: error.name, message: error.message },
  );
}

function ownerParts(state) {
  const { _pc, _transceiver, _key } = internals();
  const owner = state.target;
  return { pc: owner[_pc], key: owner[_transceiver][_key] };
}

function onTransformerMessage(transform, message) {
  const state = transform[_state];
  if (message === null || typeof message !== "object") return;
  if (message.write !== undefined) {
    const frame = message.write;
    if (state.side !== "sender" || !isFrame(frame)) return;
    const data = frame[_data];
    if (ArrayBufferPrototypeGetByteLength(data) === 0) return;
    const { pc, key } = ownerParts(state);
    const { _rid, _state: pcState } = internals();
    if (pc[pcState].closed) return;
    try {
      op_webrtc_sender_write_frame_at(
        pc[_rid],
        key,
        new Uint8Array(data),
        frame[_duration] ?? 20,
        frame[_metadata].rtpTimestamp >>> 0,
      );
    } catch {
      // The sender went away.
    }
    return;
  }
  if (message.generateKeyFrame !== undefined) {
    const id = message.generateKeyFrame;
    const sender = state.side === "sender" ? state.target : null;
    const { _state: trackKey, _trackState } = internals();
    const track = sender?.[trackKey].track ?? null;
    if (track === null || track[_trackState].kind !== "video") {
      reply(
        state,
        id,
        invalidState("generateKeyFrame: not a video sender with a track"),
      );
      return;
    }
    if (message.rid !== undefined && message.rid !== null) {
      const rids = sender.getParameters().encodings;
      let found = false;
      for (let i = 0; i < rids.length; ++i) {
        if (rids[i].rid === message.rid) found = true;
      }
      if (!found) {
        reply(
          state,
          id,
          new DOMException(
            `generateKeyFrame: no encoding with rid '${message.rid}'`,
            "NotFoundError",
          ),
        );
        return;
      }
    }
    const { _source } = internals();
    if (track[_source]?.device === undefined) {
      // A track the program feeds (Deno.webrtc.EncodedMediaTrack): it is
      // asked for the key frame, as at a remote PLI.
      const event = new Event("keyframerequest");
      setIsTrusted(event, true);
      track.dispatchEvent(event);
      reply(state, id);
      return;
    }
    // Resolved when the next key frame goes to the worker (the fake
    // camera's clip starts with one every second).
    ArrayPrototypePush(state.keyFrameWaiters, id);
    return;
  }
  if (message.sendKeyFrameRequest !== undefined) {
    const id = message.sendKeyFrameRequest;
    if (state.side !== "receiver") {
      reply(state, id, invalidState("sendKeyFrameRequest: not a receiver"));
      return;
    }
    const receiver = state.target;
    const { _state: kindKey } = internals();
    if (receiver[kindKey].track.kind !== "video") {
      reply(state, id, invalidState("sendKeyFrameRequest: not video"));
      return;
    }
    const { pc, key } = ownerParts(state);
    const { _rid, _state: pcState } = internals();
    if (!pc[pcState].closed) {
      op_webrtc_receiver_request_key_frame(pc[_rid], key);
    }
    reply(state, id);
  }
}

// SFrame (04_sframe.js): loaded with the first SFrame transform.
let sframeMod;
const loadSFrame = () =>
  sframeMod ??
    (sframeMod = core.loadExtScript("ext:deno_webrtc/04_sframe.js"));

// An RTCRtpSFrameEncryptor / RTCRtpSFrameDecryptor's transform state.
function initRtpTransform(sframeTransform) {
  sframeTransform[_state] = {
    port: null,
    target: null,
    side: null,
    owner: null,
    ownerSide: null,
    kind: null,
    keyFrameWaiters: [],
    size: null,
    closed: false,
    sframe: true,
  };
}

// `sender.transform` / `receiver.transform` set (01_webrtc.js).
function setTransform(owner, side, transform) {
  const { _transform, _pc, _transceiver, _key, _rid, _state: s } = internals();
  if (transform !== null) {
    const isScript = ObjectPrototypeIsPrototypeOf(
      RTCRtpScriptTransformPrototype,
      transform,
    );
    if (
      !isScript &&
      !(transform?.[_state]?.sframe &&
        loadSFrame().isSFrameTransform(transform, side))
    ) {
      throw new TypeError(
        side === "sender"
          ? "Failed to set 'transform': not an RTCRtpScriptTransform, an RTCRtpSFrameEncryptor or null"
          : "Failed to set 'transform': not an RTCRtpScriptTransform, an RTCRtpSFrameDecryptor or null",
      );
    }
    // A transform once attached belongs to that sender or receiver, also
    // after it is detached from it (WebRTC Encoded Transform, the
    // `transform` setter: its [[owner]] is not reset).
    const state = transform[_state];
    if (state.owner !== null && state.owner !== owner) {
      throw invalidState("Failed to set 'transform': it is already in use");
    }
  }
  const previous = owner[_transform] ?? null;
  if (previous === transform) return;
  if (previous !== null) {
    previous[_state].target = null;
    previous[_state].side = null;
  }
  owner[_transform] = transform;
  const pc = owner[_pc];
  if (transform !== null) {
    const state = transform[_state];
    state.target = owner;
    state.side = side;
    if (state.owner === null) {
      state.owner = owner;
      state.ownerSide = side;
      state.kind = owner[_transceiver][s].kind;
    }
    state.port?.postMessage({ attach: side });
  }
  if (side === "receiver" && !pc[s].closed) {
    op_webrtc_receiver_transform(
      pc[_rid],
      owner[_transceiver][_key],
      transform !== null,
    );
  }
}

function postFrame(state, frame) {
  if (state.closed) return;
  state.port.postMessage({ frame }, [frame[_data]]);
}

// A fake device's frame for a sender with a transform (02_media_devices.js).
function feedFake(sender, pc, key, stereo) {
  const { _transform, _rid, _state: pcState } = internals();
  const state = sender[_transform][_state];
  // The device's frames start once the connection is up: before DTLS is
  // connected, a frame the transform writes back cannot be sent, and a
  // transform that writes only its first frame would send nothing (an
  // encoder starts with the transport too). The clip, and its RTP
  // timestamps, start there.
  const connection = pc[pcState].connectionState;
  if (connection !== "connected" && connection !== "disconnected") {
    return false;
  }
  const f = op_webrtc_sender_fake_frame(pc[_rid], key, stereo);
  if (f === null) return false;
  if (state.sframe) {
    sendThroughSFrame(sender, state, pc, key, f);
    return true;
  }
  const data = copyBytes(f.data);
  const info = f.info;
  const metadata = {
    synchronizationSource: info.synchronizationSource,
    payloadType: info.payloadType,
    contributingSources: [],
    rtpTimestamp: info.rtpTimestamp,
    captureTime: performance.now(),
    mimeType: info.mimeType,
  };
  if (
    StringPrototypeStartsWith(
      StringPrototypeToLowerCase(info.mimeType),
      "video/",
    )
  ) {
    if (f.widthHeight !== null) {
      state.size = f.widthHeight;
    }
    if (state.size !== null) {
      metadata.width = state.size[0];
      metadata.height = state.size[1];
    }
    metadata.spatialIndex = 0;
    metadata.temporalIndex = 0;
    const frame = new RTCEncodedVideoFrame(illegalConstructorKey, {
      type: f.keyFrame ? "key" : "delta",
      data,
      metadata,
      duration: f.durationMs,
    });
    postFrame(state, frame);
    if (f.keyFrame && state.keyFrameWaiters.length > 0) {
      const waiters = state.keyFrameWaiters;
      state.keyFrameWaiters = [];
      for (let i = 0; i < waiters.length; ++i) reply(state, waiters[i]);
    }
  } else {
    postFrame(
      state,
      new RTCEncodedAudioFrame(illegalConstructorKey, {
        type: undefined,
        data,
        metadata,
        duration: f.durationMs,
      }),
    );
  }
  return true;
}

// A sender's frame through its RTCRtpSFrameEncryptor, then out at the RTP
// timestamp it took. H.264 / H.265 frames are not sent: their payloaders
// read NAL units, which the ciphertext no longer has.
function sendThroughSFrame(sender, state, pc, key, f) {
  const mime = StringPrototypeToLowerCase(f.info.mimeType);
  if (mime === "video/h264" || mime === "video/h265") return;
  const { _rid, _state: pcState, _transform } = internals();
  const transform = sender[_transform];
  PromisePrototypeThen(
    loadSFrame().processRtpFrame(transform, "sender", f.data),
    (out) => {
      if (out === null || state.closed || pc[pcState].closed) return;
      if (sender[_transform] !== transform) return;
      try {
        op_webrtc_sender_write_frame_at(
          pc[_rid],
          key,
          out,
          f.durationMs,
          f.info.rtpTimestamp >>> 0,
        );
      } catch {
        // The sender went away.
      }
    },
    () => {},
  );
}

function copyBytes(view) {
  const out = new Uint8Array(TypedArrayPrototypeGetByteLength(view));
  out.set(
    new Uint8Array(
      TypedArrayPrototypeGetBuffer(view),
      TypedArrayPrototypeGetByteOffset(view),
      TypedArrayPrototypeGetByteLength(view),
    ),
  );
  return TypedArrayPrototypeGetBuffer(out);
}

// A receiver's frame (`encodedframe`, 01_webrtc.js).
function deliverReceived(receiver, event) {
  const { _transform } = internals();
  const transform = receiver[_transform] ?? null;
  if (transform === null) return;
  const state = transform[_state];
  const info = event.info;
  if (state.sframe) {
    // Decrypted for its errors; there is no decoder for the plaintext.
    PromisePrototypeThen(
      loadSFrame().processRtpFrame(transform, "receiver", event.data),
      () => {},
      () => {},
    );
    return;
  }
  const metadata = {
    synchronizationSource: info.synchronizationSource,
    payloadType: info.payloadType,
    contributingSources: info.contributingSources,
    rtpTimestamp: info.rtpTimestamp,
    receiveTime: performance.now(),
    mimeType: info.mimeType,
  };
  const data = copyBytes(event.data);
  if (
    StringPrototypeStartsWith(
      StringPrototypeToLowerCase(info.mimeType),
      "video/",
    )
  ) {
    if (event.width !== null && event.height !== null) {
      state.size = [event.width, event.height];
    }
    if (state.size !== null) {
      metadata.width = state.size[0];
      metadata.height = state.size[1];
    }
    metadata.spatialIndex = 0;
    metadata.temporalIndex = 0;
    postFrame(
      state,
      new RTCEncodedVideoFrame(illegalConstructorKey, {
        type: event.keyFrame ? "key" : "delta",
        data,
        metadata,
        duration: undefined,
      }),
    );
  } else {
    metadata.sequenceNumber = info.sequenceNumber;
    postFrame(
      state,
      new RTCEncodedAudioFrame(illegalConstructorKey, {
        type: undefined,
        data,
        metadata,
        duration: undefined,
      }),
    );
  }
}

// The remote asked a sender with a transform for a key frame.
function keyFrameRequested(sender) {
  const { _transform } = internals();
  const transform = sender[_transform] ?? null;
  if (transform === null || transform[_state].sframe) return;
  transform[_state].port.postMessage({ keyframerequest: null });
}

// The connection of a sender / receiver with a transform closed: the
// transform's readable ends in the worker, and its port closes.
function ownerClosed(...owners) {
  const { _transform } = internals();
  for (let i = 0; i < owners.length; ++i) {
    const transform = owners[i]?.[_transform] ?? null;
    if (transform === null) continue;
    const state = transform[_state];
    if (state.closed) continue;
    state.closed = true;
    if (state.port === null) continue;
    state.port.postMessage({ close: true });
    state.port.close();
  }
}

// --- the worker side ---------------------------------------------------------

class KeyFrameRequestEvent extends Event {
  #rid;

  constructor(type, rid = undefined) {
    const prefix = "Failed to construct 'KeyFrameRequestEvent'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    type = webidl.converters.DOMString(type, prefix, "Argument 1");
    super(type);
    this.#rid = rid === undefined
      ? null
      : webidl.converters.DOMString(rid, prefix, "Argument 2");
  }

  get rid() {
    return this.#rid;
  }
}
webidl.configureInterface(KeyFrameRequestEvent);

class RTCTransformEvent extends Event {
  #transformer;

  constructor(key = undefined, transformer = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super("rtctransform");
    this.#transformer = transformer;
  }

  get transformer() {
    if (!(#transformer in this)) {
      throw new TypeError("Illegal invocation");
    }
    return this.#transformer;
  }
}
webidl.configureInterface(RTCTransformEvent);

// Frames waiting in a transformer's readable beyond this are dropped:
// real-time media doesn't wait for a slow transform.
const READABLE_HIGH_WATER_MARK = 256;

class RTCRtpScriptTransformer extends EventTarget {
  constructor(key = undefined, port = undefined, options = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    const state = {
      port,
      options,
      side: null,
      controller: null,
      nextId: 0,
      pending: new SafeMap(),
      // The [[counter]] of the last frame enqueued in the readable, and of
      // the last one written back.
      enqueued: 0,
      written: 0,
    };
    this[_state] = state;
    this[webidl.brand] = webidl.brand;
    state.readable = new ReadableStream({
      start(controller) {
        state.controller = controller;
      },
    }, { highWaterMark: READABLE_HIGH_WATER_MARK, size: () => 1 });
    state.writable = new WritableStream({
      write(chunk) {
        if (!isFrame(chunk)) {
          throw new TypeError(
            "RTCRtpScriptTransformer: only RTCEncodedVideoFrame and RTCEncodedAudioFrame can be written",
          );
        }
        // writeEncodedData: only a frame this transformer's readable gave
        // out, and only in order and once. A clone, a constructed frame, a
        // frame of another transform or one written again is dropped (the
        // write still succeeds).
        if (chunk[_owner] !== state || chunk[_counter] <= state.written) {
          return;
        }
        state.written = chunk[_counter];
        const data = chunk[_data];
        if (ArrayBufferPrototypeGetByteLength(data) === 0) return;
        // The frame's data moves to the sender: it is detached here.
        port.postMessage({ write: chunk }, [data]);
      },
    });
    port.onmessage = (event) => onTransformMessage(this, event.data);
  }

  get readable() {
    webidl.assertBranded(this, RTCRtpScriptTransformerPrototype);
    return this[_state].readable;
  }
  get writable() {
    webidl.assertBranded(this, RTCRtpScriptTransformerPrototype);
    return this[_state].writable;
  }
  get options() {
    webidl.assertBranded(this, RTCRtpScriptTransformerPrototype);
    return this[_state].options;
  }

  generateKeyFrame(rid = undefined) {
    try {
      webidl.assertBranded(this, RTCRtpScriptTransformerPrototype);
      if (rid !== undefined) {
        rid = webidl.converters.DOMString(
          rid,
          "Failed to execute 'generateKeyFrame' on 'RTCRtpScriptTransformer'",
          "Argument 1",
        );
      }
    } catch (error) {
      return PromiseReject(error);
    }
    return request(this[_state], { generateKeyFrame: true, rid });
  }

  sendKeyFrameRequest() {
    try {
      webidl.assertBranded(this, RTCRtpScriptTransformerPrototype);
    } catch (error) {
      return PromiseReject(error);
    }
    return request(this[_state], { sendKeyFrameRequest: true });
  }
}
defineEventHandler(RTCRtpScriptTransformer.prototype, "keyframerequest");
webidl.configureInterface(RTCRtpScriptTransformer);
const RTCRtpScriptTransformerPrototype = RTCRtpScriptTransformer.prototype;

function request(state, message) {
  const id = state.nextId++;
  const { promise, resolve, reject } = PromiseWithResolvers();
  MapPrototypeSet(state.pending, id, { resolve, reject });
  if (message.generateKeyFrame) {
    state.port.postMessage({ generateKeyFrame: id, rid: message.rid });
  } else {
    state.port.postMessage({ sendKeyFrameRequest: id });
  }
  return promise;
}

function onTransformMessage(transformer, message) {
  const state = transformer[_state];
  if (message === null || typeof message !== "object") return;
  if (message.frame !== undefined) {
    const c = state.controller;
    if (c !== null && c.desiredSize > 0) {
      const frame = message.frame;
      frame[_owner] = state;
      frame[_counter] = ++state.enqueued;
      c.enqueue(frame);
    }
  } else if (message.resolve !== undefined || message.reject !== undefined) {
    const id = message.resolve ?? message.reject;
    const pending = MapPrototypeGet(state.pending, id);
    if (pending === undefined) return;
    MapPrototypeDelete(state.pending, id);
    if (message.resolve !== undefined) {
      pending.resolve(undefined);
    } else {
      pending.reject(new DOMException(message.message, message.name));
    }
  } else if (message.attach !== undefined) {
    state.side = message.attach;
  } else if (message.keyframerequest !== undefined) {
    fire(transformer, new KeyFrameRequestEvent("keyframerequest"));
  } else if (message.close !== undefined) {
    // The sender's or receiver's connection closed.
    try {
      state.controller?.close();
    } catch {
      // Already closed or errored.
    }
    state.port.close();
  }
}

// 99_main.js: a worker's message carrying a transform's port. Returns
// whether it was one (and was taken: no `message` event).
function receiveTransform(message, dispatch) {
  const init = message?.[TRANSFORM_KEY];
  if (init === undefined || init === null || typeof init !== "object") {
    return false;
  }
  const transformer = new RTCRtpScriptTransformer(
    illegalConstructorKey,
    init.port,
    init.options,
  );
  const event = new RTCTransformEvent(illegalConstructorKey, transformer);
  setIsTrusted(event, true);
  dispatch(event);
  return true;
}

return {
  KeyFrameRequestEvent,
  RTCEncodedAudioFrame,
  RTCEncodedVideoFrame,
  RTCRtpScriptTransform,
  RTCRtpScriptTransformer,
  RTCTransformEvent,
  TRANSFORM_KEY,
  deliverReceived,
  deserializeFrame,
  feedFake,
  initRtpTransform,
  isFrame,
  keyFrameRequested,
  ownerClosed,
  receiveTransform,
  setTransform,
};
})();
