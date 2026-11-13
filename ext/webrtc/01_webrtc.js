// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC data channels (DENEXT.md item 364): `RTCPeerConnection`,
// `RTCDataChannel`, `RTCSessionDescription`, `RTCIceCandidate`, their events
// and `RTCStatsReport`, as globals. The ops are in `lib.rs` and the engine
// around them in `peer.rs`; this module is the W3C surface: argument
// handling, the state attributes, the event pump and the data channel
// objects. Media (tracks, transceivers) is not implemented: `addTrack` and
// `addTransceiver` throw `NotSupportedError`.

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, primordials } = __bootstrap;
const { isDataView } = core;
const {
  op_webrtc_dc_buffered_amount,
  op_webrtc_dc_close,
  op_webrtc_dc_send,
  op_webrtc_dc_set_low_threshold,
  op_webrtc_pc_add_ice_candidate,
  op_webrtc_pc_close,
  op_webrtc_pc_create,
  op_webrtc_pc_create_answer,
  op_webrtc_pc_create_data_channel,
  op_webrtc_pc_create_offer,
  op_webrtc_pc_descriptions,
  op_webrtc_pc_get_stats,
  op_webrtc_pc_max_message_size,
  op_webrtc_pc_next_events,
  op_webrtc_pc_set_local_description,
  op_webrtc_pc_set_remote_description,
} = core.ops;
const {
  ArrayBufferIsView,
  ArrayBufferPrototypeGetByteLength,
  ArrayFrom,
  ArrayIsArray,
  ArrayPrototypeIncludes,
  ArrayPrototypeMap,
  ArrayPrototypePush,
  ArrayPrototypeShift,
  MapPrototypeDelete,
  MapPrototypeEntries,
  MapPrototypeForEach,
  MapPrototypeGet,
  MapPrototypeGetSize,
  MapPrototypeHas,
  MapPrototypeKeys,
  MapPrototypeSet,
  MapPrototypeValues,
  NumberParseInt,
  DataViewPrototypeGetBuffer,
  DataViewPrototypeGetByteLength,
  DataViewPrototypeGetByteOffset,
  FunctionPrototypeCall,
  ObjectFreeze,
  ObjectPrototypeIsPrototypeOf,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  SafeMap,
  String,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  StringPrototypeTrim,
  Symbol,
  SymbolFor,
  SymbolIterator,
  TypeError,
  TypedArrayPrototypeGetBuffer,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeGetByteOffset,
  TypedArrayPrototypeSlice,
  Uint8Array,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const {
  defineEventHandler,
  Event,
  EventTarget,
  MessageEvent,
  reportException,
  setIsTrusted,
} = core.loadExtScript("ext:deno_web/02_event.js");
const { Blob, BlobPrototype } = core.loadExtScript("ext:deno_web/09_file.js");

const _rid = Symbol("[[rid]]");
const _state = Symbol("[[state]]");
const _channels = Symbol("[[channels]]");
const _channel = Symbol("[[channel]]");
const _pc = Symbol("[[pc]]");
const _init = Symbol("[[init]]");
const illegalConstructorKey = Symbol("illegalConstructorKey");

function fire(target, event) {
  setIsTrusted(event, true);
  target.dispatchEvent(event);
}

function invalidState(message) {
  return new DOMException(message, "InvalidStateError");
}

// --- RTCSessionDescription -------------------------------------------------

const SDP_TYPES = ["offer", "pranswer", "answer", "rollback"];

class RTCSessionDescription {
  #type;
  #sdp;

  constructor(descriptionInitDict = undefined) {
    const prefix = "Failed to construct 'RTCSessionDescription'";
    const init = descriptionInitDict ?? { __proto__: null };
    const type = init.type === undefined ? undefined : String(init.type);
    if (type === undefined || !ArrayPrototypeIncludes(SDP_TYPES, type)) {
      throw new TypeError(
        `${prefix}: '${type}' is not a valid value for 'type'`,
      );
    }
    this.#type = type;
    this.#sdp = init.sdp === undefined ? "" : String(init.sdp);
    this[webidl.brand] = webidl.brand;
  }

  get type() {
    webidl.assertBranded(this, RTCSessionDescriptionPrototype);
    return this.#type;
  }

  get sdp() {
    webidl.assertBranded(this, RTCSessionDescriptionPrototype);
    return this.#sdp;
  }

  toJSON() {
    webidl.assertBranded(this, RTCSessionDescriptionPrototype);
    return { type: this.#type, sdp: this.#sdp };
  }
}
webidl.configureInterface(RTCSessionDescription);
const RTCSessionDescriptionPrototype = RTCSessionDescription.prototype;

function descriptionFrom(pair) {
  return pair === null || pair === undefined
    ? null
    : new RTCSessionDescription({ type: pair[0], sdp: pair[1] });
}

// --- RTCIceCandidate -------------------------------------------------------

// `candidate:<foundation> <component> <protocol> <priority> <address> <port>
// typ <type> [raddr <a>] [rport <p>] [tcptype <t>] ...` (RFC 8839).
function parseCandidate(line) {
  const out = {
    foundation: null,
    component: null,
    priority: null,
    address: null,
    protocol: null,
    port: null,
    type: null,
    tcpType: null,
    relatedAddress: null,
    relatedPort: null,
  };
  let value = StringPrototypeTrim(line);
  if (StringPrototypeStartsWith(value, "a=")) {
    value = StringPrototypeSlice(value, 2);
  }
  if (StringPrototypeStartsWith(value, "candidate:")) {
    value = StringPrototypeSlice(value, 10);
  }
  const f = StringPrototypeSplit(value, " ");
  if (f.length < 8) return out;
  out.foundation = f[0];
  out.component = f[1] === "1" ? "rtp" : f[1] === "2" ? "rtcp" : null;
  out.protocol = StringPrototypeToLowerCase(f[2]);
  out.priority = NumberParseInt(f[3], 10);
  out.address = f[4];
  out.port = NumberParseInt(f[5], 10);
  out.type = f[7];
  for (let i = 8; i + 1 < f.length; i += 2) {
    if (f[i] === "raddr") out.relatedAddress = f[i + 1];
    else if (f[i] === "rport") out.relatedPort = NumberParseInt(f[i + 1], 10);
    else if (f[i] === "tcptype") out.tcpType = f[i + 1];
  }
  return out;
}

class RTCIceCandidate {
  #init;
  #parsed;

  constructor(candidateInitDict = undefined) {
    const init = candidateInitDict ?? { __proto__: null };
    const candidate = init.candidate === undefined
      ? ""
      : String(init.candidate);
    const sdpMid = init.sdpMid === undefined || init.sdpMid === null
      ? null
      : String(init.sdpMid);
    const sdpMLineIndex =
      init.sdpMLineIndex === undefined || init.sdpMLineIndex === null
        ? null
        : webidl.converters["unsigned short"](init.sdpMLineIndex);
    if (sdpMid === null && sdpMLineIndex === null) {
      throw new TypeError(
        "Failed to construct 'RTCIceCandidate': sdpMid and sdpMLineIndex are both null",
      );
    }
    const usernameFragment =
      init.usernameFragment === undefined || init.usernameFragment === null
        ? null
        : String(init.usernameFragment);
    this.#init = { candidate, sdpMid, sdpMLineIndex, usernameFragment };
    this.#parsed = parseCandidate(candidate);
    this[webidl.brand] = webidl.brand;
  }

  get candidate() {
    webidl.assertBranded(this, RTCIceCandidatePrototype);
    return this.#init.candidate;
  }
  get sdpMid() {
    webidl.assertBranded(this, RTCIceCandidatePrototype);
    return this.#init.sdpMid;
  }
  get sdpMLineIndex() {
    webidl.assertBranded(this, RTCIceCandidatePrototype);
    return this.#init.sdpMLineIndex;
  }
  get usernameFragment() {
    webidl.assertBranded(this, RTCIceCandidatePrototype);
    return this.#init.usernameFragment;
  }
  get foundation() {
    return this.#parsed.foundation;
  }
  get component() {
    return this.#parsed.component;
  }
  get priority() {
    return this.#parsed.priority;
  }
  get address() {
    return this.#parsed.address;
  }
  get protocol() {
    return this.#parsed.protocol;
  }
  get port() {
    return this.#parsed.port;
  }
  get type() {
    return this.#parsed.type;
  }
  get tcpType() {
    return this.#parsed.tcpType;
  }
  get relatedAddress() {
    return this.#parsed.relatedAddress;
  }
  get relatedPort() {
    return this.#parsed.relatedPort;
  }

  toJSON() {
    webidl.assertBranded(this, RTCIceCandidatePrototype);
    return { ...this.#init };
  }
}
webidl.configureInterface(RTCIceCandidate);
const RTCIceCandidatePrototype = RTCIceCandidate.prototype;

// --- Events and errors -----------------------------------------------------

class RTCPeerConnectionIceEvent extends Event {
  #candidate;
  #url;

  constructor(type, eventInitDict = undefined) {
    super(type, eventInitDict);
    this.#candidate = eventInitDict?.candidate ?? null;
    this.#url = eventInitDict?.url ?? null;
  }

  get candidate() {
    return this.#candidate;
  }

  get url() {
    return this.#url;
  }
}
webidl.configureInterface(RTCPeerConnectionIceEvent);

class RTCPeerConnectionIceErrorEvent extends Event {
  #init;

  constructor(type, eventInitDict = undefined) {
    super(type, eventInitDict);
    this.#init = {
      address: eventInitDict?.address ?? null,
      port: eventInitDict?.port ?? null,
      url: eventInitDict?.url ?? "",
      errorCode: eventInitDict?.errorCode ?? 0,
      errorText: eventInitDict?.errorText ?? "",
    };
  }

  get address() {
    return this.#init.address;
  }
  get port() {
    return this.#init.port;
  }
  get url() {
    return this.#init.url;
  }
  get errorCode() {
    return this.#init.errorCode;
  }
  get errorText() {
    return this.#init.errorText;
  }
}
webidl.configureInterface(RTCPeerConnectionIceErrorEvent);

class RTCDataChannelEvent extends Event {
  #channel;

  constructor(type, eventInitDict = undefined) {
    super(type, eventInitDict);
    if (
      !ObjectPrototypeIsPrototypeOf(
        RTCDataChannelPrototype,
        eventInitDict?.channel,
      )
    ) {
      throw new TypeError(
        "Failed to construct 'RTCDataChannelEvent': 'channel' is required",
      );
    }
    this.#channel = eventInitDict.channel;
  }

  get channel() {
    return this.#channel;
  }
}
webidl.configureInterface(RTCDataChannelEvent);

class RTCError extends DOMException {
  #init;

  constructor(init = undefined, message = "") {
    super(message, "OperationError");
    const detail = init?.errorDetail;
    if (detail === undefined) {
      throw new TypeError(
        "Failed to construct 'RTCError': 'errorDetail' is required",
      );
    }
    this.#init = {
      errorDetail: String(detail),
      sdpLineNumber: init.sdpLineNumber ?? null,
      sctpCauseCode: init.sctpCauseCode ?? null,
      receivedAlert: init.receivedAlert ?? null,
      sentAlert: init.sentAlert ?? null,
      httpRequestStatusCode: init.httpRequestStatusCode ?? null,
    };
  }

  get errorDetail() {
    return this.#init.errorDetail;
  }
  get sdpLineNumber() {
    return this.#init.sdpLineNumber;
  }
  get sctpCauseCode() {
    return this.#init.sctpCauseCode;
  }
  get receivedAlert() {
    return this.#init.receivedAlert;
  }
  get sentAlert() {
    return this.#init.sentAlert;
  }
  get httpRequestStatusCode() {
    return this.#init.httpRequestStatusCode;
  }
}

class RTCErrorEvent extends Event {
  #error;

  constructor(type, eventInitDict = undefined) {
    super(type, eventInitDict);
    this.#error = eventInitDict?.error ?? null;
  }

  get error() {
    return this.#error;
  }
}
webidl.configureInterface(RTCErrorEvent);

// --- RTCStatsReport --------------------------------------------------------

class RTCStatsReport {
  #map;

  constructor(key = undefined, entries = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this.#map = new SafeMap();
    for (let i = 0; i < entries.length; ++i) {
      const entry = entries[i];
      MapPrototypeSet(this.#map, entry.id, ObjectFreeze(entry));
    }
    this[webidl.brand] = webidl.brand;
  }

  get size() {
    return MapPrototypeGetSize(this.#map);
  }
  get(id) {
    return MapPrototypeGet(this.#map, id);
  }
  has(id) {
    return MapPrototypeHas(this.#map, id);
  }
  keys() {
    return MapPrototypeKeys(this.#map);
  }
  values() {
    return MapPrototypeValues(this.#map);
  }
  entries() {
    return MapPrototypeEntries(this.#map);
  }
  forEach(callback, thisArg = undefined) {
    MapPrototypeForEach(this.#map, (value, key) => {
      FunctionPrototypeCall(callback, thisArg, value, key, this);
    });
  }
  [SymbolIterator]() {
    return MapPrototypeEntries(this.#map);
  }
}
webidl.configureInterface(RTCStatsReport);

// --- RTCSctpTransport ------------------------------------------------------

class RTCSctpTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
  }

  get state() {
    return this[_pc][_state].connectionState === "closed"
      ? "closed"
      : "connected";
  }

  get maxMessageSize() {
    const pc = this[_pc];
    if (pc[_state].closed) return 0;
    return op_webrtc_pc_max_message_size(pc[_rid]) || 65536;
  }

  get maxChannels() {
    return 65535;
  }

  get transport() {
    return null;
  }
}
defineEventHandler(RTCSctpTransport.prototype, "statechange");
webidl.configureInterface(RTCSctpTransport);

// --- RTCDataChannel --------------------------------------------------------

class RTCDataChannel extends EventTarget {
  constructor(key = undefined, pc = undefined, init = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[_channel] = init.channel;
    this[_init] = init;
    this[_state] = {
      readyState: init.readyState ?? "connecting",
      id: init.id ?? null,
      binaryType: "arraybuffer",
      lowThreshold: 0,
      lastBuffered: 0,
      // Sends waiting for a Blob ahead of them to be read.
      queue: [],
      reading: false,
    };
    this[webidl.brand] = webidl.brand;
  }

  get label() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].label;
  }
  get ordered() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].ordered;
  }
  get maxPacketLifeTime() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].maxPacketLifeTime ?? null;
  }
  get maxRetransmits() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].maxRetransmits ?? null;
  }
  get protocol() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].protocol;
  }
  get negotiated() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].negotiated;
  }
  get id() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].id;
  }
  get readyState() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].readyState;
  }
  get bufferedAmount() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    const pc = this[_pc];
    if (!pc[_state].closed && this[_state].readyState !== "closed") {
      this[_state].lastBuffered = op_webrtc_dc_buffered_amount(
        pc[_rid],
        this[_channel],
      );
    }
    return this[_state].lastBuffered;
  }
  get bufferedAmountLowThreshold() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].lowThreshold;
  }
  set bufferedAmountLowThreshold(value) {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    const threshold = webidl.converters["unsigned long"](value);
    this[_state].lowThreshold = threshold;
    const pc = this[_pc];
    if (!pc[_state].closed) {
      op_webrtc_dc_set_low_threshold(pc[_rid], this[_channel], threshold);
    }
  }
  get binaryType() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].binaryType;
  }
  set binaryType(value) {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    const type = String(value);
    if (type === "arraybuffer" || type === "blob") {
      this[_state].binaryType = type;
    }
  }

  send(data) {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'send' on 'RTCDataChannel'",
    );
    const state = this[_state];
    if (state.readyState !== "open") {
      throw invalidState(
        `RTCDataChannel.readyState is '${state.readyState}', not 'open'`,
      );
    }
    if (ObjectPrototypeIsPrototypeOf(BlobPrototype, data)) {
      // A Blob is read first; later sends wait behind it so the order on the
      // wire is the order of the calls.
      ArrayPrototypePush(state.queue, data);
      drainQueue(this);
      return;
    }
    let text = false;
    let bytes;
    if (typeof data === "string") {
      text = true;
      bytes = core.encode(data);
    } else if (ArrayBufferIsView(data)) {
      bytes = isDataView(data)
        ? new Uint8Array(
          DataViewPrototypeGetBuffer(data),
          DataViewPrototypeGetByteOffset(data),
          DataViewPrototypeGetByteLength(data),
        )
        : new Uint8Array(
          TypedArrayPrototypeGetBuffer(data),
          TypedArrayPrototypeGetByteOffset(data),
          TypedArrayPrototypeGetByteLength(data),
        );
    } else if (core.isAnyArrayBuffer(data)) {
      bytes = new Uint8Array(data);
    } else {
      text = true;
      bytes = core.encode(String(data));
    }
    if (state.queue.length > 0) {
      ArrayPrototypePush(state.queue, { text, bytes });
      return;
    }
    sendNow(this, text, bytes);
  }

  close() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    const state = this[_state];
    if (state.readyState === "closing" || state.readyState === "closed") {
      return;
    }
    state.readyState = "closing";
    const pc = this[_pc];
    if (!pc[_state].closed) {
      op_webrtc_dc_close(pc[_rid], this[_channel]);
    }
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `${this.constructor.name} ${
      inspect({
        label: this.label,
        id: this.id,
        readyState: this.readyState,
        ordered: this.ordered,
        protocol: this.protocol,
        bufferedAmount: this.bufferedAmount,
      }, inspectOptions)
    }`;
  }
}
defineEventHandler(RTCDataChannel.prototype, "open");
defineEventHandler(RTCDataChannel.prototype, "bufferedamountlow");
defineEventHandler(RTCDataChannel.prototype, "error");
defineEventHandler(RTCDataChannel.prototype, "closing");
defineEventHandler(RTCDataChannel.prototype, "close");
defineEventHandler(RTCDataChannel.prototype, "message");
webidl.configureInterface(RTCDataChannel);
const RTCDataChannelPrototype = RTCDataChannel.prototype;

function sendNow(channel, text, bytes) {
  const pc = channel[_pc];
  channel[_state].lastBuffered = op_webrtc_dc_send(
    pc[_rid],
    channel[_channel],
    text,
    bytes,
  );
}

function drainQueue(channel) {
  const state = channel[_state];
  if (state.reading) return;
  while (state.queue.length > 0) {
    const head = state.queue[0];
    if (ObjectPrototypeIsPrototypeOf(BlobPrototype, head)) {
      state.reading = true;
      PromisePrototypeThen(head.arrayBuffer(), (buffer) => {
        ArrayPrototypeShift(state.queue);
        state.reading = false;
        if (state.readyState === "open") {
          try {
            sendNow(channel, false, new Uint8Array(buffer));
          } catch (error) {
            failChannel(channel, error);
            return;
          }
          drainQueue(channel);
        }
      }, (error) => {
        state.reading = false;
        state.queue = [];
        failChannel(channel, error);
      });
      return;
    }
    ArrayPrototypeShift(state.queue);
    try {
      sendNow(channel, head.text, head.bytes);
    } catch (error) {
      failChannel(channel, error);
      return;
    }
  }
}

// A queued send that could not go out (a Blob that failed to read, or one
// over maxMessageSize): `error`, then the channel closes, as a browser does
// when it cannot send what it already accepted.
function failChannel(channel, cause) {
  const error = new RTCError(
    { errorDetail: "data-channel-failure" },
    cause?.message ?? String(cause),
  );
  fire(channel, new RTCErrorEvent("error", { error }));
  channel.close();
}

function toBinary(channel, data) {
  if (channel[_state].binaryType === "blob") {
    return new Blob([data]);
  }
  if (
    TypedArrayPrototypeGetByteOffset(data) === 0 &&
    TypedArrayPrototypeGetByteLength(data) ===
      ArrayBufferPrototypeGetByteLength(TypedArrayPrototypeGetBuffer(data))
  ) {
    return TypedArrayPrototypeGetBuffer(data);
  }
  return TypedArrayPrototypeGetBuffer(TypedArrayPrototypeSlice(data));
}

// --- RTCPeerConnection -----------------------------------------------------

function convertIceServers(servers) {
  if (servers === undefined || servers === null) return [];
  if (
    !ArrayIsArray(servers) && typeof servers?.[SymbolIterator] !== "function"
  ) {
    throw new TypeError(
      "Failed to construct 'RTCPeerConnection': 'iceServers' must be a sequence",
    );
  }
  const list = ArrayFrom(servers);
  const out = [];
  for (let i = 0; i < list.length; ++i) {
    const server = list[i];
    if (server === null || typeof server !== "object") {
      throw new TypeError(
        "Failed to construct 'RTCPeerConnection': an RTCIceServer must be an object",
      );
    }
    const urls = typeof server.urls === "string"
      ? [server.urls]
      : server.urls === undefined
      ? undefined
      : ArrayPrototypeMap(ArrayFrom(server.urls), String);
    if (urls === undefined) {
      throw new TypeError(
        "Failed to construct 'RTCPeerConnection': 'urls' is required in RTCIceServer",
      );
    }
    ArrayPrototypePush(out, {
      urls,
      username: server.username === undefined
        ? undefined
        : String(server.username),
      credential: server.credential === undefined
        ? undefined
        : String(server.credential),
    });
  }
  return out;
}

function convertConfiguration(configuration) {
  const config = configuration ?? { __proto__: null };
  const iceTransportPolicy = config.iceTransportPolicy === undefined
    ? "all"
    : String(config.iceTransportPolicy);
  if (iceTransportPolicy !== "all" && iceTransportPolicy !== "relay") {
    throw new TypeError(
      `Failed to construct 'RTCPeerConnection': '${iceTransportPolicy}' is not a valid value for 'iceTransportPolicy'`,
    );
  }
  const bundlePolicy = config.bundlePolicy === undefined
    ? "balanced"
    : String(config.bundlePolicy);
  if (
    !ArrayPrototypeIncludes(
      ["balanced", "max-compat", "max-bundle"],
      bundlePolicy,
    )
  ) {
    throw new TypeError(
      `Failed to construct 'RTCPeerConnection': '${bundlePolicy}' is not a valid value for 'bundlePolicy'`,
    );
  }
  const rtcpMuxPolicy = config.rtcpMuxPolicy === undefined
    ? "require"
    : String(config.rtcpMuxPolicy);
  if (rtcpMuxPolicy !== "require") {
    throw new TypeError(
      `Failed to construct 'RTCPeerConnection': '${rtcpMuxPolicy}' is not a valid value for 'rtcpMuxPolicy'`,
    );
  }
  if (config.certificates !== undefined && config.certificates.length > 0) {
    throw new DOMException(
      "RTCConfiguration.certificates is not supported",
      "NotSupportedError",
    );
  }
  return {
    iceServers: convertIceServers(config.iceServers),
    iceTransportPolicy,
    bundlePolicy,
    rtcpMuxPolicy,
    iceCandidatePoolSize: config.iceCandidatePoolSize ?? 0,
  };
}

// The signaling state after a description is applied (JSEP, RFC 8829).
function nextSignalingState(_current, local, type) {
  if (type === "rollback") return "stable";
  if (local) {
    if (type === "offer") return "have-local-offer";
    if (type === "pranswer") return "have-local-pranswer";
    return "stable";
  }
  if (type === "offer") return "have-remote-offer";
  if (type === "pranswer") return "have-remote-pranswer";
  return "stable";
}

class RTCPeerConnection extends EventTarget {
  constructor(configuration = undefined) {
    super();
    const config = convertConfiguration(configuration);
    this[_rid] = op_webrtc_pc_create({
      iceServers: config.iceServers,
      iceTransportPolicy: config.iceTransportPolicy,
    });
    this[_channels] = new SafeMap();
    this[_state] = {
      config,
      closed: false,
      active: false,
      signalingState: "stable",
      iceGatheringState: "new",
      iceConnectionState: "new",
      connectionState: "new",
      sctp: null,
      // Operations chain: createOffer / createAnswer / set*Description /
      // addIceCandidate run in call order, as the spec's operations chain.
      chain: PromiseResolve(),
    };
    this[webidl.brand] = webidl.brand;
    pump(this);
  }

  get signalingState() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].signalingState;
  }
  get iceGatheringState() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].iceGatheringState;
  }
  get iceConnectionState() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].iceConnectionState;
  }
  get connectionState() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].connectionState;
  }
  get canTrickleIceCandidates() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].closed || this.remoteDescription === null ? null : true;
  }
  get localDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    if (this[_state].closed) return this[_state].lastLocal ?? null;
    return descriptionFrom(op_webrtc_pc_descriptions(this[_rid]).local);
  }
  get remoteDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    if (this[_state].closed) return this[_state].lastRemote ?? null;
    return descriptionFrom(op_webrtc_pc_descriptions(this[_rid]).remote);
  }
  get currentLocalDescription() {
    return this.signalingState === "stable" ? this.localDescription : null;
  }
  get pendingLocalDescription() {
    return this.signalingState === "stable" ? null : this.localDescription;
  }
  get currentRemoteDescription() {
    return this.signalingState === "stable" ? this.remoteDescription : null;
  }
  get pendingRemoteDescription() {
    return this.signalingState === "stable" ? null : this.remoteDescription;
  }
  get sctp() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const state = this[_state];
    if (state.closed || state.connectionState !== "connected") {
      return state.sctp;
    }
    state.sctp ??= new RTCSctpTransport(illegalConstructorKey, this);
    return state.sctp;
  }

  createOffer(options = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return enqueue(this, () => {
      const iceRestart = !!options?.iceRestart || !!this[_state].restartIce;
      this[_state].restartIce = false;
      return new RTCSessionDescription({
        type: "offer",
        sdp: op_webrtc_pc_create_offer(this[_rid], iceRestart),
      });
    });
  }

  createAnswer(_options = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return enqueue(this, () => {
      const state = this[_state].signalingState;
      if (state !== "have-remote-offer" && state !== "have-local-pranswer") {
        throw invalidState(
          `Cannot create an answer in signaling state '${state}'`,
        );
      }
      return new RTCSessionDescription({
        type: "answer",
        sdp: op_webrtc_pc_create_answer(this[_rid]),
      });
    });
  }

  setLocalDescription(description = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return enqueue(this, () => {
      let type = description?.type;
      let sdp = description?.sdp;
      const signaling = this[_state].signalingState;
      if (type === undefined) {
        type = signaling === "have-remote-offer" ||
            signaling === "have-local-pranswer"
          ? "answer"
          : "offer";
      }
      type = String(type);
      if (!ArrayPrototypeIncludes(SDP_TYPES, type)) {
        throw new TypeError(`'${type}' is not a valid RTCSdpType`);
      }
      if (type !== "rollback" && (sdp === undefined || sdp === "")) {
        sdp = type === "offer"
          ? op_webrtc_pc_create_offer(this[_rid], false)
          : op_webrtc_pc_create_answer(this[_rid]);
      }
      op_webrtc_pc_set_local_description(this[_rid], type, sdp ?? "");
      activate(this);
      setSignalingState(this, nextSignalingState(signaling, true, type));
    });
  }

  setRemoteDescription(description) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return enqueue(this, () => {
      if (description === undefined || description === null) {
        throw new TypeError(
          "Failed to execute 'setRemoteDescription': 1 argument required",
        );
      }
      const type = String(description.type);
      if (!ArrayPrototypeIncludes(SDP_TYPES, type)) {
        throw new TypeError(`'${type}' is not a valid RTCSdpType`);
      }
      const signaling = this[_state].signalingState;
      op_webrtc_pc_set_remote_description(
        this[_rid],
        type,
        description.sdp === undefined ? "" : String(description.sdp),
      );
      activate(this);
      setSignalingState(this, nextSignalingState(signaling, false, type));
    });
  }

  addIceCandidate(candidate = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return enqueue(this, () => {
      if (candidate === undefined || candidate === null) return;
      const line = candidate.candidate === undefined
        ? ""
        : String(candidate.candidate);
      const sdpMid = candidate.sdpMid ?? null;
      const sdpMLineIndex = candidate.sdpMLineIndex ?? null;
      if (line !== "" && sdpMid === null && sdpMLineIndex === null) {
        throw new TypeError(
          "Failed to execute 'addIceCandidate': sdpMid and sdpMLineIndex are both null",
        );
      }
      if (this.remoteDescription === null) {
        throw invalidState(
          "addIceCandidate needs a remote description first",
        );
      }
      op_webrtc_pc_add_ice_candidate(this[_rid], {
        candidate: line,
        sdpMid: sdpMid === null ? null : String(sdpMid),
        sdpMLineIndex,
        usernameFragment: candidate.usernameFragment ?? null,
      });
    });
  }

  createDataChannel(label, dataChannelDict = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'createDataChannel' on 'RTCPeerConnection'",
    );
    if (this[_state].closed) {
      throw invalidState("RTCPeerConnection is closed");
    }
    const dict = dataChannelDict ?? { __proto__: null };
    const init = {
      label: String(label),
      ordered: dict.ordered === undefined ? true : !!dict.ordered,
      maxPacketLifeTime: dict.maxPacketLifeTime === undefined ||
          dict.maxPacketLifeTime === null
        ? null
        : webidl.converters["unsigned short"](dict.maxPacketLifeTime, "", "", {
          clamp: true,
        }),
      maxRetransmits:
        dict.maxRetransmits === undefined || dict.maxRetransmits === null
          ? null
          : webidl.converters["unsigned short"](dict.maxRetransmits, "", "", {
            clamp: true,
          }),
      protocol: dict.protocol === undefined ? "" : String(dict.protocol),
      negotiated: !!dict.negotiated,
      id: dict.id === undefined || dict.id === null
        ? null
        : webidl.converters["unsigned short"](dict.id, "", "", {
          enforceRange: true,
        }),
    };
    if (init.maxPacketLifeTime !== null && init.maxRetransmits !== null) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': maxPacketLifeTime and maxRetransmits are mutually exclusive",
      );
    }
    if (init.negotiated && init.id === null) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': a negotiated channel needs an id",
      );
    }
    if (init.id === 65535) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': id 65535 is reserved",
      );
    }
    if (TypedArrayPrototypeGetByteLength(core.encode(init.label)) > 65535) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': label too long",
      );
    }
    const created = op_webrtc_pc_create_data_channel(this[_rid], {
      label: init.label,
      ordered: init.ordered,
      maxPacketLifeTime: init.maxPacketLifeTime,
      maxRetransmits: init.maxRetransmits,
      protocol: init.protocol,
      negotiated: init.negotiated,
      id: init.id,
    });
    const channel = new RTCDataChannel(illegalConstructorKey, this, {
      ...init,
      channel: created.channel,
      id: created.id ?? init.id,
    });
    MapPrototypeSet(this[_channels], created.channel, channel);
    // A negotiated channel opens with the connection, without DCEP; its
    // open event comes from the engine like any other.
    return channel;
  }

  getStats(_selector = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    if (this[_state].closed) {
      return PromiseReject(invalidState("RTCPeerConnection is closed"));
    }
    try {
      const entries = op_webrtc_pc_get_stats(this[_rid]);
      return PromiseResolve(
        new RTCStatsReport(illegalConstructorKey, entries),
      );
    } catch (error) {
      return PromiseReject(error);
    }
  }

  getConfiguration() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const config = this[_state].config;
    return {
      iceServers: ArrayPrototypeMap(config.iceServers, (s) => ({ ...s })),
      iceTransportPolicy: config.iceTransportPolicy,
      bundlePolicy: config.bundlePolicy,
      rtcpMuxPolicy: config.rtcpMuxPolicy,
      iceCandidatePoolSize: config.iceCandidatePoolSize,
      certificates: [],
    };
  }

  setConfiguration(configuration = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    if (this[_state].closed) {
      throw invalidState("RTCPeerConnection is closed");
    }
    const config = convertConfiguration(configuration);
    const current = this[_state].config;
    if (
      config.bundlePolicy !== current.bundlePolicy ||
      config.rtcpMuxPolicy !== current.rtcpMuxPolicy
    ) {
      throw new DOMException(
        "bundlePolicy and rtcpMuxPolicy can't change",
        "InvalidModificationError",
      );
    }
    // The ICE servers and policy apply to the next gathering, which this
    // implementation starts once per connection: a change is recorded only.
    this[_state].config = config;
  }

  restartIce() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    if (this[_state].closed) return;
    this[_state].restartIce = true;
    fire(this, new Event("negotiationneeded"));
  }

  getSenders() {
    return [];
  }
  getReceivers() {
    return [];
  }
  getTransceivers() {
    return [];
  }
  addTrack() {
    throw new DOMException(
      "Media tracks are not supported by Deno's RTCPeerConnection (data channels only)",
      "NotSupportedError",
    );
  }
  removeTrack() {
    throw new DOMException(
      "Media tracks are not supported by Deno's RTCPeerConnection (data channels only)",
      "NotSupportedError",
    );
  }
  addTransceiver() {
    throw new DOMException(
      "Media tracks are not supported by Deno's RTCPeerConnection (data channels only)",
      "NotSupportedError",
    );
  }

  close() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const state = this[_state];
    if (state.closed) return;
    try {
      const descriptions = op_webrtc_pc_descriptions(this[_rid]);
      state.lastLocal = descriptionFrom(descriptions.local);
      state.lastRemote = descriptionFrom(descriptions.remote);
    } catch {
      // The resource is already gone.
    }
    state.closed = true;
    state.active = false;
    state.signalingState = "closed";
    state.iceConnectionState = "closed";
    state.connectionState = "closed";
    op_webrtc_pc_close(this[_rid]);
    // Every channel is closed at once; like browsers, each still reports
    // `close` (in a later task, after close() returns).
    const channels = [];
    MapPrototypeForEach(this[_channels], (channel) => {
      if (channel[_state].readyState !== "closed") {
        channel[_state].readyState = "closed";
        ArrayPrototypePush(channels, channel);
      }
    });
    this[_channels] = new SafeMap();
    if (channels.length > 0) {
      PromisePrototypeThen(PromiseResolve(), () => {
        for (let i = 0; i < channels.length; ++i) {
          fire(channels[i], new Event("close"));
        }
      });
    }
  }

  static generateCertificate(_keygenAlgorithm) {
    return PromiseReject(
      new DOMException(
        "RTCPeerConnection.generateCertificate is not supported",
        "NotSupportedError",
      ),
    );
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `${this.constructor.name} ${
      inspect({
        signalingState: this.signalingState,
        iceGatheringState: this.iceGatheringState,
        iceConnectionState: this.iceConnectionState,
        connectionState: this.connectionState,
      }, inspectOptions)
    }`;
  }
}
defineEventHandler(RTCPeerConnection.prototype, "negotiationneeded");
defineEventHandler(RTCPeerConnection.prototype, "icecandidate");
defineEventHandler(RTCPeerConnection.prototype, "icecandidateerror");
defineEventHandler(RTCPeerConnection.prototype, "signalingstatechange");
defineEventHandler(RTCPeerConnection.prototype, "iceconnectionstatechange");
defineEventHandler(RTCPeerConnection.prototype, "icegatheringstatechange");
defineEventHandler(RTCPeerConnection.prototype, "connectionstatechange");
defineEventHandler(RTCPeerConnection.prototype, "datachannel");
defineEventHandler(RTCPeerConnection.prototype, "track");
webidl.configureInterface(RTCPeerConnection);
const RTCPeerConnectionPrototype = RTCPeerConnection.prototype;

// Runs `fn` on the operations chain; its result (or throw) settles the
// returned promise. A closed connection rejects with InvalidStateError.
function enqueue(pc, fn) {
  const state = pc[_state];
  if (state.closed) {
    return PromiseReject(invalidState("RTCPeerConnection is closed"));
  }
  const run = () => {
    if (state.closed) throw invalidState("RTCPeerConnection is closed");
    return fn();
  };
  const result = PromisePrototypeThen(state.chain, run);
  state.chain = PromisePrototypeThen(result, () => {}, () => {});
  return result;
}

function setSignalingState(pc, next) {
  const state = pc[_state];
  if (state.closed || state.signalingState === next) return;
  state.signalingState = next;
  fire(pc, new Event("signalingstatechange"));
}

// The connection keeps the process alive from its first description until
// it is closed or has failed, as an open socket does.
function activate(pc) {
  const state = pc[_state];
  if (state.closed || state.active) return;
  state.active = true;
  if (state.pending) core.refOpPromise(state.pending);
}

function deactivate(pc) {
  const state = pc[_state];
  if (!state.active) return;
  state.active = false;
  if (state.pending) core.unrefOpPromise(state.pending);
}

function pump(pc) {
  const state = pc[_state];
  const promise = op_webrtc_pc_next_events(pc[_rid]);
  state.pending = promise;
  if (!state.active) core.unrefOpPromise(promise);
  PromisePrototypeThen(promise, (events) => {
    state.pending = null;
    if (events === null || state.closed) return;
    for (let i = 0; i < events.length; ++i) {
      try {
        dispatchEngineEvent(pc, events[i]);
      } catch (error) {
        // Listener exceptions are reported by dispatchEvent itself; this is
        // anything else, reported without stopping the other events.
        reportException(error);
      }
      if (state.closed) return;
    }
    pump(pc);
  }, () => {
    state.pending = null;
  });
}

function channelFor(pc, event) {
  return MapPrototypeGet(pc[_channels], event.channel);
}

function dispatchEngineEvent(pc, event) {
  const state = pc[_state];
  switch (event.type) {
    case "icecandidate": {
      const candidate = new RTCIceCandidate({
        candidate: event.candidate,
        sdpMid: event.sdpMid,
        sdpMLineIndex: event.sdpMLineIndex,
        usernameFragment: event.usernameFragment,
      });
      fire(
        pc,
        new RTCPeerConnectionIceEvent("icecandidate", {
          candidate,
          url: event.url ?? null,
        }),
      );
      break;
    }
    case "icecandidateerror":
      fire(
        pc,
        new RTCPeerConnectionIceErrorEvent("icecandidateerror", {
          address: event.address ?? null,
          port: event.port ?? null,
          url: event.url,
          errorCode: event.errorCode,
          errorText: event.errorText,
        }),
      );
      break;
    case "signalingstatechange":
      // The JS side sets the signaling state as each description is applied.
      break;
    case "icegatheringstatechange":
      if (state.iceGatheringState === event.state) break;
      state.iceGatheringState = event.state;
      fire(pc, new Event("icegatheringstatechange"));
      if (event.state === "complete") {
        // End of candidates: an icecandidate event with a null candidate.
        fire(
          pc,
          new RTCPeerConnectionIceEvent("icecandidate", { candidate: null }),
        );
      }
      break;
    case "iceconnectionstatechange":
      if (state.iceConnectionState === event.state) break;
      state.iceConnectionState = event.state;
      fire(pc, new Event("iceconnectionstatechange"));
      break;
    case "connectionstatechange":
      if (state.connectionState === event.state) break;
      state.connectionState = event.state;
      fire(pc, new Event("connectionstatechange"));
      if (event.state === "failed" || event.state === "closed") {
        deactivate(pc);
      }
      break;
    case "negotiationneeded":
      if (state.signalingState === "stable") {
        fire(pc, new Event("negotiationneeded"));
      }
      break;
    case "dcopen": {
      let channel = channelFor(pc, event);
      if (channel === undefined) {
        // Opened by the remote peer: announced with `datachannel`.
        channel = new RTCDataChannel(illegalConstructorKey, pc, {
          channel: event.channel,
          id: event.id,
          label: event.label,
          protocol: event.protocol,
          ordered: event.ordered,
          maxPacketLifeTime: event.maxPacketLifeTime ?? null,
          maxRetransmits: event.maxRetransmits ?? null,
          negotiated: event.negotiated,
          readyState: "open",
        });
        MapPrototypeSet(pc[_channels], event.channel, channel);
        fire(pc, new RTCDataChannelEvent("datachannel", { channel }));
        fire(channel, new Event("open"));
        break;
      }
      if (channel[_state].readyState !== "connecting") break;
      channel[_state].id = event.id ?? channel[_state].id;
      channel[_state].readyState = "open";
      fire(channel, new Event("open"));
      break;
    }
    case "dcclosing": {
      const channel = channelFor(pc, event);
      if (channel === undefined || channel[_state].readyState === "closed") {
        break;
      }
      channel[_state].readyState = "closing";
      fire(channel, new Event("closing"));
      break;
    }
    case "dcclose": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      MapPrototypeDelete(pc[_channels], event.channel);
      if (channel[_state].readyState === "closed") break;
      channel[_state].readyState = "closed";
      fire(channel, new Event("close"));
      break;
    }
    case "dcerror": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      fire(
        channel,
        new RTCErrorEvent("error", {
          error: new RTCError(
            { errorDetail: "sctp-failure" },
            "The data channel failed",
          ),
        }),
      );
      break;
    }
    case "dclow": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      fire(channel, new Event("bufferedamountlow"));
      break;
    }
    case "dcmessage": {
      const channel = channelFor(pc, event);
      if (channel === undefined || channel[_state].readyState === "closed") {
        break;
      }
      const data = event.text ? core.decode(event.data) : toBinary(
        channel,
        event.data,
      );
      fire(channel, new MessageEvent("message", { data }));
      break;
    }
  }
}

return {
  RTCDataChannel,
  RTCDataChannelEvent,
  RTCError,
  RTCErrorEvent,
  RTCIceCandidate,
  RTCPeerConnection,
  RTCPeerConnectionIceErrorEvent,
  RTCPeerConnectionIceEvent,
  RTCSctpTransport,
  RTCSessionDescription,
  RTCStatsReport,
};
})();
