// Copyright 2018-2026 the Deno authors. MIT license.

// WebRTC (DENEXT.md item 370): `RTCPeerConnection`, `RTCDataChannel`,
// `RTCRtpTransceiver` / `RTCRtpSender` / `RTCRtpReceiver`, `MediaStream`,
// `MediaStreamTrack`, `RTCCertificate`, the transports, their events and
// `RTCStatsReport`, as globals, and the `Deno.webrtc` server extension. The
// ops are in `lib.rs`, the engine around them in `peer.rs` and `media.rs`;
// this module is the W3C surface: argument handling, the state attributes,
// the event pump, the data channel and media objects.

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, primordials } = __bootstrap;
const { isDataView } = core;
const {
  op_base64_encode_from_buffer,
  op_webrtc_capabilities,
  op_webrtc_certificate_drop,
  op_webrtc_certificate_from_pem,
  op_webrtc_certificate_to_pem,
  op_webrtc_dc_close,
  op_webrtc_dc_id,
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
  op_webrtc_generate_certificate,
  op_webrtc_network_addresses,
  op_webrtc_network_close,
  op_webrtc_network_create,
  op_webrtc_network_tcp_addresses,
  op_webrtc_pc_add_transceiver,
  op_webrtc_pc_attach_track,
  op_webrtc_pc_selected_pair,
  op_webrtc_pc_set_ice_servers,
  op_webrtc_pc_transceivers,
  op_webrtc_receiver_parameters,
  op_webrtc_receiver_read_rtp,
  op_webrtc_receiver_request_key_frame,
  op_webrtc_receiver_sources,
  op_webrtc_sender_parameters,
  op_webrtc_sender_send_dtmf,
  op_webrtc_sender_set_paused,
  op_webrtc_sender_set_streams,
  op_webrtc_sender_write_frame,
  op_webrtc_sender_write_rtp,
  op_webrtc_set_log_level,
  op_webrtc_transceiver_set_codec_preferences,
  op_webrtc_transceiver_set_direction,
  op_webrtc_transceiver_stop,
  op_void_async_deferred,
} = core.ops;
const {
  ArrayBufferIsView,
  ArrayBufferPrototypeGetByteLength,
  ArrayFrom,
  ArrayIsArray,
  ArrayPrototypeIncludes,
  ArrayPrototypeJoin,
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
  ObjectDefineProperty,
  ObjectGetOwnPropertyDescriptor,
  ObjectFreeze,
  ObjectPrototypeIsPrototypeOf,
  Promise,
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
  ArrayPrototypeEvery,
  ArrayPrototypeFilter,
  ArrayPrototypeFind,
  ArrayPrototypeIndexOf,
  ArrayPrototypeSlice,
  ArrayPrototypeSome,
  ArrayPrototypeSplice,
  DateNow,
  JSONStringify,
  MathFloor,
  MathMax,
  MathMin,
  StringPrototypeCharCodeAt,
  MathRandom,
  RangeError,
  RegExpPrototypeTest,
  SafeRegExp,
  SafeSet,
  SetPrototypeAdd,
  SetPrototypeDelete,
  SetPrototypeForEach,
  SetPrototypeGetSize,
  SetPrototypeHas,
  StringPrototypeEndsWith,
  StringPrototypeIncludes,
  StringPrototypeLastIndexOf,
  StringPrototypeReplace,
  StringPrototypeToUpperCase,
  SafeFinalizationRegistry,
  SymbolDispose,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const {
  defineEventHandler,
  Event,
  EventTarget,
  MessageEvent,
  reportException,
  setIsTrusted,
} = core.loadExtScript("ext:deno_web/02_event.js");
const { Blob, BlobPrototype } = core.loadExtScript("ext:deno_web/09_file.js");
const { performance } = core.loadExtScript("ext:deno_web/15_performance.js");
const { clearTimeout, setTimeout } = core.loadExtScript(
  "ext:deno_web/02_timers.js",
);
const streams = core.loadExtScript("ext:deno_web/06_streams.js");

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

// An event handler IDL attribute (`onopen`, ...) that checks its `this`, as
// Web IDL has a regular attribute: read or set on the prototype itself, or
// on another kind of object, it throws a TypeError.
function defineHandler(prototype, name) {
  defineEventHandler(prototype, name);
  const key = `on${name}`;
  const { get, set } = ObjectGetOwnPropertyDescriptor(prototype, key);
  // Computed accessors, so the functions are named `get onopen` / `set
  // onopen`.
  const branded = {
    get [key]() {
      webidl.assertBranded(this, prototype);
      return FunctionPrototypeCall(get, this);
    },
    set [key](value) {
      webidl.assertBranded(this, prototype);
      FunctionPrototypeCall(set, this, value);
    },
  };
  ObjectDefineProperty(prototype, key, {
    __proto__: null,
    ...ObjectGetOwnPropertyDescriptor(branded, key),
    enumerable: true,
    configurable: true,
  });
}

function invalidState(message) {
  return new DOMException(message, "InvalidStateError");
}

// A promise-returning operation rejects (rather than throws) on a wrong
// `this`, as Web IDL has it: the error, or null when branded.
function brandError(self, prototype) {
  try {
    webidl.assertBranded(self, prototype);
    return null;
  } catch (error) {
    return error;
  }
}

// --- RTCSessionDescription -------------------------------------------------

const SDP_TYPES = ["offer", "pranswer", "answer", "rollback"];

class RTCSessionDescription {
  #type;
  #sdp;

  constructor(descriptionInitDict) {
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

// One of the four description slots ("currentLocal", "pendingLocal",
// "currentRemote", "pendingRemote"): the same RTCSessionDescription object
// while the description (its type and SDP, gathered candidates included)
// doesn't change; after close(), what the slot held then.
function descriptionSlot(pc, slot) {
  const state = pc[_state];
  const pair = state.closed
    ? state.last?.[slot]
    : op_webrtc_pc_descriptions(pc[_rid])[slot];
  if (pair === null || pair === undefined) return null;
  const cache = state.descriptions;
  const held = cache[slot];
  if (held !== null && held.type === pair[0] && held.sdp === pair[1]) {
    return held;
  }
  const description = descriptionFrom(pair);
  cache[slot] = description;
  return description;
}

// --- RTCIceCandidate -------------------------------------------------------

// `candidate:<foundation> <component> <transport> <priority> <address>
// <port> typ <type> [raddr <a> rport <p>] [tcptype <t>] ...` (RFC 8839
// section 5.1, RFC 6544 section 4.5), with no SDP framing ("a=") and no
// surrounding whitespace. A TCP candidate other than a relay carries its
// tcptype right after the related address. A string that doesn't parse
// leaves every derived attribute null.
const ICE_CHARS = new SafeRegExp("^[A-Za-z0-9+/]{1,32}$");
const DIGITS_3 = new SafeRegExp("^[0-9]{1,3}$");
const DIGITS_5 = new SafeRegExp("^[0-9]{1,5}$");
const DIGITS_10 = new SafeRegExp("^[0-9]{1,10}$");
const SPACES = new SafeRegExp("[ \t]+");

function nullCandidate() {
  return {
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
}

function parseCandidate(line) {
  if (!StringPrototypeStartsWith(line, "candidate:")) return nullCandidate();
  const value = StringPrototypeSlice(line, 10);
  const f = StringPrototypeSplit(value, SPACES);
  if (f.length < 8) return nullCandidate();
  const foundation = f[0];
  const address = f[4];
  if (
    !RegExpPrototypeTest(ICE_CHARS, foundation) ||
    !RegExpPrototypeTest(DIGITS_3, f[1]) ||
    !RegExpPrototypeTest(DIGITS_10, f[3]) ||
    !RegExpPrototypeTest(DIGITS_5, f[5]) ||
    address === "" ||
    StringPrototypeToLowerCase(f[6]) !== "typ"
  ) {
    return nullCandidate();
  }
  const component = NumberParseInt(f[1], 10);
  const priority = NumberParseInt(f[3], 10);
  const port = NumberParseInt(f[5], 10);
  if (
    component < 1 || component > 256 || priority < 1 ||
    priority > 2147483647 || port > 65535
  ) {
    return nullCandidate();
  }
  const out = {
    foundation,
    component: component === 1 ? "rtp" : component === 2 ? "rtcp" : null,
    priority,
    address,
    protocol: null,
    port,
    type: null,
    tcpType: null,
    relatedAddress: null,
    relatedPort: null,
  };
  const protocol = StringPrototypeToLowerCase(f[2]);
  if (protocol === "udp" || protocol === "tcp") out.protocol = protocol;
  const type = StringPrototypeToLowerCase(f[7]);
  if (ArrayPrototypeIncludes(["host", "srflx", "prflx", "relay"], type)) {
    out.type = type;
  }
  let hasRaddr = false;
  let hasRport = false;
  let i = 8;
  if (i + 1 < f.length && StringPrototypeToLowerCase(f[i]) === "raddr") {
    hasRaddr = true;
    out.relatedAddress = f[i + 1];
    i += 2;
  }
  if (i + 1 < f.length && StringPrototypeToLowerCase(f[i]) === "rport") {
    if (!RegExpPrototypeTest(DIGITS_5, f[i + 1])) return nullCandidate();
    const rport = NumberParseInt(f[i + 1], 10);
    if (rport > 65535) return nullCandidate();
    hasRport = true;
    out.relatedPort = rport;
    i += 2;
  }
  if (i < f.length && StringPrototypeToLowerCase(f[i]) === "tcptype") {
    const tcpType = i + 1 < f.length
      ? StringPrototypeToLowerCase(f[i + 1])
      : "";
    if (!ArrayPrototypeIncludes(["active", "passive", "so"], tcpType)) {
      return nullCandidate();
    }
    out.tcpType = tcpType;
  } else if (protocol === "tcp" && type !== "relay") {
    return nullCandidate();
  }
  if (hasRaddr !== hasRport) return nullCandidate();
  if (
    (type === "srflx" || type === "prflx" || type === "relay") && !hasRaddr
  ) {
    return nullCandidate();
  }
  return out;
}

class RTCIceCandidate {
  #init;
  #parsed;
  #relayProtocol;
  #url;

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
    const relayProtocol = init.relayProtocol ?? null;
    this.#relayProtocol = ArrayPrototypeIncludes(
        ["udp", "tcp", "tls"],
        relayProtocol,
      )
      ? relayProtocol
      : null;
    this.#url = init.url === undefined || init.url === null
      ? null
      : String(init.url);
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
  get relayProtocol() {
    return this.#relayProtocol;
  }
  get url() {
    return this.#url;
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
    const prefix = "Failed to construct 'RTCPeerConnectionIceEvent'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const candidate = eventInitDict?.candidate ?? null;
    if (
      candidate !== null &&
      !ObjectPrototypeIsPrototypeOf(RTCIceCandidatePrototype, candidate)
    ) {
      throw new TypeError(`${prefix}: 'candidate' is not an RTCIceCandidate`);
    }
    super(type, eventInitDict);
    this.#candidate = candidate;
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

  constructor(type, eventInitDict) {
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

  constructor(type, eventInitDict) {
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

// RTCErrorDetailType (W3C WebRTC 1.0 section 11.1.2).
const ERROR_DETAILS = [
  "data-channel-failure",
  "dtls-failure",
  "fingerprint-failure",
  "sctp-failure",
  "sdp-syntax-error",
  "hardware-encoder-not-available",
  "hardware-encoder-error",
];

class RTCError extends DOMException {
  #init;

  constructor(init, message = "") {
    super(message, "OperationError");
    const detail = init?.errorDetail;
    if (detail === undefined) {
      throw new TypeError(
        "Failed to construct 'RTCError': 'errorDetail' is required",
      );
    }
    const errorDetail = String(detail);
    if (!ArrayPrototypeIncludes(ERROR_DETAILS, errorDetail)) {
      throw new TypeError(
        `Failed to construct 'RTCError': '${errorDetail}' is not a valid value for 'errorDetail'`,
      );
    }
    this.#init = {
      errorDetail,
      sdpLineNumber: init.sdpLineNumber ?? null,
      sctpCauseCode: init.sctpCauseCode ?? null,
      receivedAlert: init.receivedAlert ?? null,
      sentAlert: init.sentAlert ?? null,
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
}
webidl.configureInterface(RTCError);

class RTCErrorEvent extends Event {
  #error;

  constructor(type, eventInitDict) {
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
}
// A maplike's @@iterator is its `entries` function itself.
ObjectDefineProperty(RTCStatsReport.prototype, SymbolIterator, {
  __proto__: null,
  value: RTCStatsReport.prototype.entries,
  writable: true,
  enumerable: false,
  configurable: true,
});
webidl.configureInterface(RTCStatsReport);

// --- RTCSctpTransport ------------------------------------------------------

// What this side's SCTP sends at most (the engine's bound, as browsers).
const LOCAL_MAX_MESSAGE_SIZE = 262144;

class RTCSctpTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[webidl.brand] = webidl.brand;
  }

  // The association's own state (the engine reports it): connecting until
  // the SCTP handshake completes, closed for good once it ends.
  get state() {
    webidl.assertBranded(this, RTCSctpTransportPrototype);
    const pc = this[_pc][_state];
    if (pc.closed) return "closed";
    return pc.sctpState ?? "connecting";
  }

  // The remote description's a=max-message-size (65536 when absent, no
  // limit when 0), capped by what this side can send.
  get maxMessageSize() {
    webidl.assertBranded(this, RTCSctpTransportPrototype);
    const pc = this[_pc];
    if (!pc[_state].closed) {
      const engine = op_webrtc_pc_max_message_size(pc[_rid]);
      if (engine > 0) return engine;
    }
    const remote = pc[_state].remoteMaxMessageSize ?? 65536;
    return remote === 0
      ? LOCAL_MAX_MESSAGE_SIZE
      : MathMin(remote, LOCAL_MAX_MESSAGE_SIZE);
  }

  get maxChannels() {
    webidl.assertBranded(this, RTCSctpTransportPrototype);
    return this.state === "connected" ? 65535 : null;
  }

  get transport() {
    webidl.assertBranded(this, RTCSctpTransportPrototype);
    return dtlsTransportOf(this[_pc]);
  }
}
defineHandler(RTCSctpTransport.prototype, "statechange");
webidl.configureInterface(RTCSctpTransport);
const RTCSctpTransportPrototype = RTCSctpTransport.prototype;

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
      // [[BufferedAmount]]: what send() accepted that SCTP hasn't taken from
      // its queue yet (`dcsent`).
      buffered: 0,
      // Bytes accepted by send() but still waiting behind a Blob read: they
      // count in bufferedAmount from the call on.
      queuedBytes: 0,
      // Sends waiting for a Blob ahead of them to be read.
      queue: [],
      reading: false,
      // close() was called with sends queued: the close follows them.
      closeAfterQueue: false,
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
    const state = this[_state];
    // The engine assigns an in-band channel's id once the DTLS role is
    // negotiated (an answer is applied); it doesn't change after that.
    if (state.id === null && state.readyState !== "closed") {
      const pc = this[_pc];
      if (!pc[_state].closed) {
        const id = op_webrtc_dc_id(pc[_rid], this[_channel]);
        if (id >= 0) state.id = id;
      }
    }
    return state.id;
  }
  get readyState() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].readyState;
  }
  get bufferedAmount() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_state].buffered + this[_state].queuedBytes;
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
      state.queuedBytes += data.size;
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
      state.queuedBytes += TypedArrayPrototypeGetByteLength(bytes);
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
    // Sends still waiting for a Blob to be read go out first (the closing
    // procedure finishes sending what send() accepted), then the close.
    if (state.queue.length > 0) {
      state.closeAfterQueue = true;
      return;
    }
    closeTransport(this);
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
defineHandler(RTCDataChannel.prototype, "open");
defineHandler(RTCDataChannel.prototype, "bufferedamountlow");
defineHandler(RTCDataChannel.prototype, "error");
defineHandler(RTCDataChannel.prototype, "closing");
defineHandler(RTCDataChannel.prototype, "close");
defineHandler(RTCDataChannel.prototype, "message");
webidl.configureInterface(RTCDataChannel);
const RTCDataChannelPrototype = RTCDataChannel.prototype;

function closeTransport(channel) {
  const pc = channel[_pc];
  if (!pc[_state].closed) {
    op_webrtc_dc_close(pc[_rid], channel[_channel]);
  }
}

function sendNow(channel, text, bytes) {
  const pc = channel[_pc];
  op_webrtc_dc_send(pc[_rid], channel[_channel], text, bytes);
  channel[_state].buffered += TypedArrayPrototypeGetByteLength(bytes);
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
        state.queuedBytes = MathMax(0, state.queuedBytes - head.size);
        state.reading = false;
        if (state.readyState === "open" || state.closeAfterQueue) {
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
        state.queuedBytes = 0;
        failChannel(channel, error);
      });
      return;
    }
    ArrayPrototypeShift(state.queue);
    state.queuedBytes = MathMax(
      0,
      state.queuedBytes - TypedArrayPrototypeGetByteLength(head.bytes),
    );
    try {
      sendNow(channel, head.text, head.bytes);
    } catch (error) {
      failChannel(channel, error);
      return;
    }
  }
  if (state.closeAfterQueue) {
    state.closeAfterQueue = false;
    closeTransport(channel);
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
  const state = channel[_state];
  state.queue = [];
  state.queuedBytes = 0;
  if (state.closeAfterQueue) {
    state.closeAfterQueue = false;
    closeTransport(channel);
  } else {
    channel.close();
  }
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

// --- Media: tracks and streams ---------------------------------------------
//
// There is no capture device in Deno. A remote track (a receiver's) carries
// what the peer sends, readable as RTP packets through `Deno.webrtc.readRtp`;
// a local track is a `Deno.webrtc.EncodedMediaTrack`, which the program feeds
// encoded frames or RTP packets. Both are MediaStreamTracks, so addTrack,
// addTransceiver, replaceTrack, MediaStream and the events work as in a
// browser.

const _source = Symbol("[[source]]");
const _receiver = Symbol("[[receiver]]");
const _trackState = Symbol("[[trackState]]");

// A version 4 UUID for track, stream and transaction ids (unique, not
// secret: they appear in the SDP).
function uuid() {
  const hex = "0123456789abcdef";
  let out = "";
  for (let i = 0; i < 36; ++i) {
    if (i === 8 || i === 13 || i === 18 || i === 23) {
      out += "-";
    } else if (i === 14) {
      out += "4";
    } else if (i === 19) {
      out += hex[8 + MathFloor(MathRandom() * 4)];
    } else {
      out += hex[MathFloor(MathRandom() * 16)];
    }
  }
  return out;
}

class MediaStreamTrack extends EventTarget {
  constructor(key = undefined, init = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_trackState] = {
      kind: init.kind,
      id: init.id ?? uuid(),
      label: init.label ?? "",
      enabled: true,
      muted: init.muted ?? false,
      readyState: "live",
      contentHint: "",
    };
    // A local track's source (what clones share); null for a remote track.
    this[_source] = init.source ?? null;
    // A remote track's receiver.
    this[_receiver] = init.receiver ?? null;
    if (this[_source] !== null) {
      SetPrototypeAdd(this[_source].tracks, this);
    }
    this[webidl.brand] = webidl.brand;
  }

  get kind() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].kind;
  }
  get id() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].id;
  }
  get label() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].label;
  }
  get enabled() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].enabled;
  }
  set enabled(value) {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    this[_trackState].enabled = !!value;
  }
  get muted() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].muted;
  }
  get readyState() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].readyState;
  }
  get contentHint() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return this[_trackState].contentHint;
  }
  set contentHint(value) {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const hint = String(value);
    const allowed = this[_trackState].kind === "audio"
      ? ["", "speech", "speech-recognition", "music"]
      : ["", "motion", "detail", "text"];
    if (ArrayPrototypeIncludes(allowed, hint)) {
      this[_trackState].contentHint = hint;
    }
  }

  clone() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const state = this[_trackState];
    const track =
      new (this[_source] !== null && this[_source].ctor
        ? this[_source].ctor
        : MediaStreamTrack)(illegalConstructorKey, {
        kind: state.kind,
        label: state.label,
        muted: state.muted,
        source: this[_source],
        receiver: this[_receiver],
      });
    track[_trackState].enabled = state.enabled;
    track[_trackState].contentHint = state.contentHint;
    if (state.readyState === "ended") track[_trackState].readyState = "ended";
    if (this[_receiver] !== null) {
      ArrayPrototypePush(this[_receiver][_state].clones, track);
    }
    // A capture device's track: the clone takes its settings and constraints.
    this[_source]?.device?.cloned(this, track);
    return track;
  }

  stop() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const state = this[_trackState];
    if (state.readyState === "ended") return;
    // No `ended` event: stop() is the application's own doing.
    state.readyState = "ended";
    if (this[_source] !== null) {
      SetPrototypeDelete(this[_source].tracks, this);
      // A capture device whose last track stopped stops capturing.
      if (this[_source].tracks.size === 0) this[_source].device?.ended();
    }
  }

  // A capture device's track (02_media_devices.js) answers these from its
  // device; any other track has no capabilities or constraints.
  getCapabilities() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const device = this[_source]?.device;
    return device ? device.capabilities(this) : {};
  }
  getConstraints() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const device = this[_source]?.device;
    return device ? device.constraints(this) : {};
  }
  getSettings() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const device = this[_source]?.device;
    return device ? device.settings(this) : { deviceId: "", groupId: "" };
  }
  applyConstraints(constraints = undefined) {
    const error = brandError(this, MediaStreamTrackPrototype);
    if (error !== null) return PromiseReject(error);
    const device = this[_source]?.device;
    return device
      ? device.applyConstraints(this, constraints)
      : PromiseResolve();
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `${this.constructor.name} ${
      inspect({
        kind: this.kind,
        id: this.id,
        label: this.label,
        enabled: this.enabled,
        muted: this.muted,
        readyState: this.readyState,
      }, inspectOptions)
    }`;
  }
}
defineHandler(MediaStreamTrack.prototype, "mute");
defineHandler(MediaStreamTrack.prototype, "unmute");
defineHandler(MediaStreamTrack.prototype, "ended");
webidl.configureInterface(MediaStreamTrack);
const MediaStreamTrackPrototype = MediaStreamTrack.prototype;

function setMuted(track, muted) {
  const state = track[_trackState];
  if (state.readyState === "ended" || state.muted === muted) return;
  state.muted = muted;
  fire(track, new Event(muted ? "mute" : "unmute"));
}

function endTrack(track) {
  const state = track[_trackState];
  if (state.readyState === "ended") return;
  state.readyState = "ended";
  fire(track, new Event("ended"));
}

class MediaStreamTrackEvent extends Event {
  #track;

  constructor(type, eventInitDict) {
    super(type, eventInitDict);
    if (
      !ObjectPrototypeIsPrototypeOf(
        MediaStreamTrackPrototype,
        eventInitDict?.track,
      )
    ) {
      throw new TypeError(
        "Failed to construct 'MediaStreamTrackEvent': 'track' is required",
      );
    }
    this.#track = eventInitDict.track;
  }

  get track() {
    return this.#track;
  }
}
webidl.configureInterface(MediaStreamTrackEvent);

const _tracks = Symbol("[[tracks]]");

class MediaStream extends EventTarget {
  #id;

  constructor(streamOrTracks = undefined) {
    super();
    this.#id = uuid();
    this[_tracks] = [];
    this[webidl.brand] = webidl.brand;
    if (streamOrTracks === illegalConstructorKey) {
      // A remote stream: its id comes from the remote description.
      this.#id = arguments[1];
      return;
    }
    let tracks = [];
    if (ObjectPrototypeIsPrototypeOf(MediaStreamPrototype, streamOrTracks)) {
      tracks = streamOrTracks.getTracks();
    } else if (streamOrTracks !== undefined) {
      if (
        streamOrTracks === null ||
        typeof streamOrTracks[SymbolIterator] !== "function"
      ) {
        throw new TypeError(
          "Failed to construct 'MediaStream': expected a MediaStream or a sequence of MediaStreamTrack",
        );
      }
      tracks = ArrayFrom(streamOrTracks);
    }
    for (let i = 0; i < tracks.length; ++i) {
      if (
        !ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, tracks[i])
      ) {
        throw new TypeError(
          "Failed to construct 'MediaStream': not a MediaStreamTrack",
        );
      }
      if (!ArrayPrototypeIncludes(this[_tracks], tracks[i])) {
        ArrayPrototypePush(this[_tracks], tracks[i]);
      }
    }
  }

  get id() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return this.#id;
  }
  get active() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return ArrayPrototypeSome(
      this[_tracks],
      (t) => t[_trackState].readyState !== "ended",
    );
  }
  getTracks() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return ArrayPrototypeSlice(this[_tracks]);
  }
  getAudioTracks() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return ArrayPrototypeFilter(
      this[_tracks],
      (t) => t[_trackState].kind === "audio",
    );
  }
  getVideoTracks() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return ArrayPrototypeFilter(
      this[_tracks],
      (t) => t[_trackState].kind === "video",
    );
  }
  getTrackById(trackId) {
    webidl.assertBranded(this, MediaStreamPrototype);
    const id = String(trackId);
    return ArrayPrototypeFind(
      this[_tracks],
      (t) => t[_trackState].id === id,
    ) ??
      null;
  }
  addTrack(track) {
    webidl.assertBranded(this, MediaStreamPrototype);
    if (!ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, track)) {
      throw new TypeError(
        "Failed to execute 'addTrack' on 'MediaStream': not a MediaStreamTrack",
      );
    }
    if (!ArrayPrototypeIncludes(this[_tracks], track)) {
      ArrayPrototypePush(this[_tracks], track);
    }
  }
  removeTrack(track) {
    webidl.assertBranded(this, MediaStreamPrototype);
    if (!ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, track)) {
      throw new TypeError(
        "Failed to execute 'removeTrack' on 'MediaStream': not a MediaStreamTrack",
      );
    }
    const i = ArrayPrototypeIndexOf(this[_tracks], track);
    if (i !== -1) ArrayPrototypeSplice(this[_tracks], i, 1);
  }
  clone() {
    webidl.assertBranded(this, MediaStreamPrototype);
    return new MediaStream(
      ArrayPrototypeMap(this[_tracks], (t) => t.clone()),
    );
  }
}
defineHandler(MediaStream.prototype, "addtrack");
defineHandler(MediaStream.prototype, "removetrack");
webidl.configureInterface(MediaStream);
const MediaStreamPrototype = MediaStream.prototype;

// Remote changes to a stream fire addtrack / removetrack.
function streamAddRemote(stream, track) {
  if (ArrayPrototypeIncludes(stream[_tracks], track)) return;
  ArrayPrototypePush(stream[_tracks], track);
  fire(stream, new MediaStreamTrackEvent("addtrack", { track }));
}

function streamRemoveRemote(stream, track) {
  const i = ArrayPrototypeIndexOf(stream[_tracks], track);
  if (i === -1) return;
  ArrayPrototypeSplice(stream[_tracks], i, 1);
  fire(stream, new MediaStreamTrackEvent("removetrack", { track }));
}

// --- RTCCertificate --------------------------------------------------------

const _certRid = Symbol("[[certRid]]");
const certificateRegistry = new SafeFinalizationRegistry((id) =>
  op_webrtc_certificate_drop(id)
);

class RTCCertificate {
  #expires;
  #fingerprints;

  constructor(key = undefined, info = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_certRid] = info.id;
    // The key goes with the object (a connection keeps its own copy).
    certificateRegistry.register(this, info.id);
    this.#expires = info.expires;
    this.#fingerprints = info.fingerprints;
    this[webidl.brand] = webidl.brand;
  }

  get expires() {
    webidl.assertBranded(this, RTCCertificatePrototype);
    return this.#expires;
  }

  getFingerprints() {
    webidl.assertBranded(this, RTCCertificatePrototype);
    return ArrayPrototypeMap(this.#fingerprints, (f) => ({
      algorithm: f.algorithm,
      value: f.value,
    }));
  }
}
webidl.configureInterface(RTCCertificate);
const RTCCertificatePrototype = RTCCertificate.prototype;

// WebCrypto's AlgorithmIdentifier as generateCertificate takes it.
function normalizeKeygen(keygenAlgorithm) {
  if (typeof keygenAlgorithm === "string") {
    keygenAlgorithm = { name: keygenAlgorithm };
  }
  if (keygenAlgorithm === null || typeof keygenAlgorithm !== "object") {
    throw new TypeError("generateCertificate: an algorithm is required");
  }
  const name = String(keygenAlgorithm.name ?? "");
  const upper = StringPrototypeToUpperCase(name);
  const hash = keygenAlgorithm.hash;
  const out = {
    name,
    namedCurve: keygenAlgorithm.namedCurve === undefined
      ? null
      : String(keygenAlgorithm.namedCurve),
    modulusLength: keygenAlgorithm.modulusLength === undefined
      ? null
      : webidl.converters["unsigned long"](keygenAlgorithm.modulusLength),
    publicExponent: keygenAlgorithm.publicExponent === undefined
      ? null
      : ArrayFrom(keygenAlgorithm.publicExponent),
    hash: hash === undefined
      ? null
      : typeof hash === "string"
      ? hash
      : String(hash?.name ?? ""),
  };
  if (upper !== "ECDSA" && upper !== "RSASSA-PKCS1-V1_5") {
    throw new DOMException(
      `'${name}' is not a supported certificate algorithm`,
      "NotSupportedError",
    );
  }
  return out;
}

// --- RTCDtlsTransport / RTCIceTransport -------------------------------------

const _transportState = Symbol("[[transportState]]");

class RTCIceTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[_transportState] = { state: "new", gatheringState: "new" };
    this[webidl.brand] = webidl.brand;
  }

  get role() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    // Fixed by the first answer applied: the offerer of that exchange is
    // controlling (an ICE-lite peer never is, but that is only the server
    // extension's case and it answers); an ICE restart keeps it.
    return this[_pc][_state].iceRole ?? "unknown";
  }
  get component() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return "rtp";
  }
  get state() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return this[_transportState].state;
  }
  get gatheringState() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return this[_transportState].gatheringState;
  }
  // The candidates gathered here and signalled with `icecandidate`, and the
  // remote ones received with addIceCandidate().
  getLocalCandidates() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return ArrayPrototypeSlice(this[_pc][_state].localCandidates);
  }
  getRemoteCandidates() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return ArrayPrototypeSlice(this[_pc][_state].remoteCandidates);
  }
  getSelectedCandidatePair() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return selectedPair(this[_pc]);
  }
  getLocalParameters() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return iceParameters(this[_pc].localDescription);
  }
  getRemoteParameters() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    return iceParameters(this[_pc].remoteDescription);
  }
}
defineHandler(RTCIceTransport.prototype, "statechange");
defineHandler(RTCIceTransport.prototype, "gatheringstatechange");
defineHandler(RTCIceTransport.prototype, "selectedcandidatepairchange");
webidl.configureInterface(RTCIceTransport);
const RTCIceTransportPrototype = RTCIceTransport.prototype;

class RTCIceCandidatePair {
  #local;
  #remote;

  constructor(key = undefined, local = undefined, remote = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this.#local = local;
    this.#remote = remote;
    this[webidl.brand] = webidl.brand;
  }

  get local() {
    webidl.assertBranded(this, RTCIceCandidatePairPrototype);
    return this.#local;
  }
  get remote() {
    webidl.assertBranded(this, RTCIceCandidatePairPrototype);
    return this.#remote;
  }
}
webidl.configureInterface(RTCIceCandidatePair);
const RTCIceCandidatePairPrototype = RTCIceCandidatePair.prototype;

// Whether a description signals "a=ice-options:trickle" (RFC 8840 section
// 4.1.3), at the session or a media level.
function offersTrickle(sdp) {
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (!StringPrototypeStartsWith(line, "a=ice-options:")) continue;
    const options = StringPrototypeSplit(
      StringPrototypeSlice(line, 14),
      SPACES,
    );
    if (ArrayPrototypeIncludes(options, "trickle")) return true;
  }
  return false;
}

function iceParameters(description) {
  if (description === null) return null;
  let usernameFragment = null;
  let password = null;
  const lines = StringPrototypeSplit(description.sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "a=ice-ufrag:")) {
      usernameFragment ??= StringPrototypeSlice(line, 12);
    } else if (StringPrototypeStartsWith(line, "a=ice-pwd:")) {
      password ??= StringPrototypeSlice(line, 10);
    }
  }
  if (usernameFragment === null) return null;
  return { usernameFragment, password };
}

class RTCDtlsTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[_transportState] = {
      state: "new",
      ice: new RTCIceTransport(illegalConstructorKey, pc),
    };
    this[webidl.brand] = webidl.brand;
  }

  get iceTransport() {
    webidl.assertBranded(this, RTCDtlsTransportPrototype);
    return this[_transportState].ice;
  }
  get state() {
    webidl.assertBranded(this, RTCDtlsTransportPrototype);
    return this[_transportState].state;
  }
  getRemoteCertificates() {
    webidl.assertBranded(this, RTCDtlsTransportPrototype);
    return [];
  }
}
defineHandler(RTCDtlsTransport.prototype, "statechange");
defineHandler(RTCDtlsTransport.prototype, "error");
webidl.configureInterface(RTCDtlsTransport);
const RTCDtlsTransportPrototype = RTCDtlsTransport.prototype;

// The connection's one (bundled) DTLS transport, from its first description.
function dtlsTransportOf(pc) {
  const state = pc[_state];
  if (
    pc.localDescription === null && pc.remoteDescription === null
  ) {
    return null;
  }
  if (state.dtls === null || state.dtls === undefined) {
    state.dtls = new RTCDtlsTransport(illegalConstructorKey, pc);
    // Created late (on first access): take the current states without
    // events.
    setTransportStates(pc, true);
  }
  return state.dtls;
}

function setTransportStates(pc, silent = false) {
  const notify = (target) => {
    if (!silent) fire(target, new Event("statechange"));
  };
  const sctp = pc[_state].sctp;
  if (sctp !== null && sctp !== undefined) {
    const now = sctp.state;
    if (sctp[_transportState] !== now) {
      const first = sctp[_transportState] === undefined;
      sctp[_transportState] = now;
      if (!first) notify(sctp);
    }
  }
  const dtls = pc[_state].dtls;
  if (dtls === undefined || dtls === null) return;
  const s = pc[_state];
  const ice = dtls[_transportState].ice;
  const iceState = s.closed ? "closed" : s.iceConnectionState;
  if (ice[_transportState].state !== iceState) {
    ice[_transportState].state = iceState;
    notify(ice);
  }
  const gathering = s.iceGatheringState;
  if (ice[_transportState].gatheringState !== gathering) {
    ice[_transportState].gatheringState = gathering;
    if (!silent) fire(ice, new Event("gatheringstatechange"));
  }
  let dtlsState = dtls[_transportState].state;
  // The peer's close_notify closes the DTLS transport itself (the engine
  // says so) while the connection, by its own rules, stays connected.
  if (
    s.closed || s.connectionState === "closed" || s.dtlsState === "closed"
  ) dtlsState = "closed";
  else if (s.connectionState === "failed") dtlsState = "failed";
  else if (s.connectionState === "connected") dtlsState = "connected";
  else if (s.connectionState === "connecting") dtlsState = "connecting";
  if (dtls[_transportState].state !== dtlsState) {
    dtls[_transportState].state = dtlsState;
    notify(dtls);
  }
}

// The selected pair as RTCIceCandidates, from the candidates this side
// signalled and received (matched by address); null before ICE connects.
function selectedPair(pc) {
  const state = pc[_state];
  if (state.closed) return null;
  const pair = op_webrtc_pc_selected_pair(pc[_rid]);
  if (pair === null) return null;
  const protocol = pair[2];
  const find = (list, addr) => {
    for (let i = 0; i < list.length; ++i) {
      const c = list[i];
      const host = StringPrototypeIncludes(c.address ?? "", ":")
        ? `[${c.address}]`
        : c.address;
      if (`${host}:${c.port}` === addr && c.protocol === protocol) return c;
    }
    return null;
  };
  const synth = (addr) => {
    const sep = StringPrototypeLastIndexOf(addr, ":");
    const host = StringPrototypeReplace(
      StringPrototypeReplace(StringPrototypeSlice(addr, 0, sep), "[", ""),
      "]",
      "",
    );
    return new RTCIceCandidate({
      // A candidate the strict parser accepts (priority 1 or more, and a
      // related address, which a peer-reflexive one must carry).
      candidate: `candidate:0 1 ${protocol} 1 ${host} ${
        StringPrototypeSlice(addr, sep + 1)
      } typ prflx raddr 0.0.0.0 rport 0${
        protocol === "tcp" ? " tcptype active" : ""
      }`,
      sdpMLineIndex: 0,
    });
  };
  return new RTCIceCandidatePair(
    illegalConstructorKey,
    find(state.localCandidates, pair[0]) ?? synth(pair[0]),
    find(state.remoteCandidates, pair[1]) ?? synth(pair[1]),
  );
}

// --- RTCRtpSender / RTCRtpReceiver / RTCRtpTransceiver ---------------------

const _key = Symbol("[[key]]");
const _transceiver = Symbol("[[transceiver]]");

const capabilitiesCache = new SafeMap();

function capabilities(kind, sender) {
  const k = `${sender ? "s" : "r"}:${kind}`;
  if (!MapPrototypeHas(capabilitiesCache, k)) {
    MapPrototypeSet(
      capabilitiesCache,
      k,
      op_webrtc_capabilities(String(kind), sender),
    );
  }
  const caps = MapPrototypeGet(capabilitiesCache, k);
  if (caps === null) return null;
  return {
    codecs: ArrayPrototypeMap(caps.codecs, (c) => ({ ...c })),
    headerExtensions: ArrayPrototypeMap(
      caps.headerExtensions,
      (h) => ({ ...h }),
    ),
  };
}

// getStats(): resolves in a later task (it is not on the operations
// chain, and never settles synchronously); after close(), the stats the
// connection had when it closed.
// The stats timestamps on the Performance timeline: the engine reports each
// relative to the moment the report was asked for, which is `base`.
const STATS_TIME_FIELDS = [
  "timestamp",
  "lastPacketSentTimestamp",
  "lastPacketReceivedTimestamp",
  "remoteTimestamp",
  "estimatedPlayoutTimestamp",
];

function engineStats(pc, key, sender) {
  const base = performance.timeOrigin + performance.now();
  const entries = op_webrtc_pc_get_stats(
    pc[_rid],
    key ?? 0,
    key !== null,
    sender,
  );
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    for (let f = 0; f < STATS_TIME_FIELDS.length; ++f) {
      const field = STATS_TIME_FIELDS[f];
      if (typeof stats[field] === "number") stats[field] += base;
    }
  }
  return refineStats(pc, entries, base);
}

// Members the engine fills with an empty string, `Unspecified` or a zero
// when it has no value: webrtc-stats leaves such a member out.
const STATS_OPTIONAL_STRINGS = [
  "url",
  "relayProtocol",
  "tcpType",
  "relatedAddress",
  "issuerCertificateId",
  "mediaSourceId",
  "rid",
  "codecId",
  "playoutId",
  "iceRole",
  "iceState",
  "dtlsState",
  "dtlsRole",
];
const HEX_ONLY = new SafeRegExp("^(?:[0-9a-fA-F]{2})+$");

function hexToBase64(hex) {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; ++i) {
    bytes[i] = NumberParseInt(StringPrototypeSlice(hex, 2 * i, 2 * i + 2), 16);
  }
  return op_base64_encode_from_buffer(bytes, 0, bytes.length);
}

// What the W3C report has and the engine's doesn't: references that resolve
// (a candidate pair names its candidates by their stats ids, a codec its
// transport), no member without a value, the certificate as base64 DER, the
// codec an outbound stream sends with, and a `media-source` for each sender
// with a track.
function refineStats(pc, entries, now) {
  const ids = new SafeSet();
  let transportId = null;
  for (let i = 0; i < entries.length; ++i) {
    SetPrototypeAdd(ids, entries[i].id);
    if (entries[i].type === "transport") transportId = entries[i].id;
  }
  const added = [];
  const state = pc[_state];
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    for (let f = 0; f < STATS_OPTIONAL_STRINGS.length; ++f) {
      const field = STATS_OPTIONAL_STRINGS[f];
      if (stats[field] === "" || stats[field] === "Unspecified") {
        delete stats[field];
      }
    }
    switch (stats.type) {
      case "candidate-pair": {
        const local = `RTCLocalIceCandidate_${stats.localCandidateId}`;
        if (SetPrototypeHas(ids, local)) stats.localCandidateId = local;
        const remote = `RTCRemoteIceCandidate_${stats.remoteCandidateId}`;
        if (SetPrototypeHas(ids, remote)) stats.remoteCandidateId = remote;
        break;
      }
      case "local-candidate":
      case "remote-candidate":
        if (stats.relatedAddress === undefined) delete stats.relatedPort;
        break;
      case "certificate":
        if (
          typeof stats.base64Certificate === "string" &&
          RegExpPrototypeTest(HEX_ONLY, stats.base64Certificate)
        ) {
          stats.base64Certificate = hexToBase64(stats.base64Certificate);
        }
        break;
      case "codec":
        if (stats.transportId === undefined && transportId !== null) {
          stats.transportId = transportId;
        }
        break;
      case "transport":
        if (stats.iceLocalUsernameFragment === "") {
          // The ufrag of the local description in use.
          const slots = op_webrtc_pc_descriptions(pc[_rid]);
          const local = slots.currentLocal ?? slots.pendingLocal;
          const ufrag = local === null
            ? null
            : mediaSectionsOf(local[1])[0]?.ufrag ?? null;
          if (ufrag === null) delete stats.iceLocalUsernameFragment;
          else stats.iceLocalUsernameFragment = ufrag;
        }
        if (stats.dtlsRole === undefined || stats.dtlsRole === "unknown") {
          const role = negotiatedDtlsRole(pc);
          if (role !== null) stats.dtlsRole = role;
        }
        break;
      case "outbound-rtp": {
        const t = ArrayPrototypeFind(
          state.transceivers,
          (t) => t[_state].mid !== null && t[_state].mid === stats.mid,
        );
        if (t === undefined) break;
        if (stats.codecId === undefined) {
          const codec = sendCodecStats(pc, t, transportId, now);
          if (codec !== null) {
            stats.codecId = codec.id;
            if (!SetPrototypeHas(ids, codec.id)) {
              SetPrototypeAdd(ids, codec.id);
              ArrayPrototypePush(added, codec);
            }
          }
        }
        const track = t[_state].sender[_state].track;
        if (track !== null) {
          const source = mediaSourceStats(track, stats, now);
          stats.mediaSourceId = source.id;
          if (!SetPrototypeHas(ids, source.id)) {
            SetPrototypeAdd(ids, source.id);
            ArrayPrototypePush(added, source);
          }
        }
        break;
      }
    }
  }
  // A remote-inbound stream is coded as the outbound stream it reports on.
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    if (stats.type !== "remote-inbound-rtp" || stats.codecId !== undefined) {
      continue;
    }
    const local = ArrayPrototypeFind(entries, (e) => e.id === stats.localId);
    if (local?.codecId !== undefined) stats.codecId = local.codecId;
  }
  for (let i = 0; i < added.length; ++i) ArrayPrototypePush(entries, added[i]);
  return entries;
}

// The DTLS role the current descriptions negotiated (RFC 5763 section 5: the
// `a=setup:active` side is the client), before the handshake starts and the
// engine reports it; null while there is no negotiated pair.
function negotiatedDtlsRole(pc) {
  const slots = op_webrtc_pc_descriptions(pc[_rid]);
  if (slots.currentLocal === null || slots.currentRemote === null) return null;
  const local = mediaSectionsOf(slots.currentLocal[1])[0]?.setup ?? null;
  const remote = mediaSectionsOf(slots.currentRemote[1])[0]?.setup ?? null;
  if (local === "active" || remote === "passive") return "client";
  if (local === "passive" || remote === "active") return "server";
  return null;
}

// The `codec` stats of the codec a transceiver's sender sends with (the
// first negotiated one), or null before negotiation.
function sendCodecStats(pc, transceiver, transportId, now) {
  let negotiated;
  try {
    negotiated = op_webrtc_sender_parameters(pc[_rid], transceiver[_key]);
  } catch {
    return null;
  }
  const codec = negotiated?.codecs?.[0];
  if (codec === undefined) return null;
  const stats = {
    timestamp: now,
    type: "codec",
    id: `RTCCodec_${transportId ?? "RTCTransport"}_send_PT${codec.payloadType}`,
    payloadType: codec.payloadType,
    codecType: "encode",
    mimeType: codec.mimeType,
    clockRate: codec.clockRate,
  };
  if (transportId !== null) stats.transportId = transportId;
  if (codec.channels !== undefined) stats.channels = codec.channels;
  if (codec.sdpFmtpLine !== undefined && codec.sdpFmtpLine !== "") {
    stats.sdpFmtpLine = codec.sdpFmtpLine;
  }
  return stats;
}

// The `media-source` stats of a sender's track: what the track's source says
// about itself (a capture device its dimensions and frame rate), and the
// frames the sender has sent.
function mediaSourceStats(track, outbound, now) {
  const stats = {
    timestamp: now,
    type: "media-source",
    id: `RTCMediaSource_${track.kind}_${track.id}`,
    trackIdentifier: track.id,
    kind: track.kind,
  };
  if (track.kind === "video") {
    const settings = track[_source]?.device?.settings(track);
    if (typeof settings?.width === "number") stats.width = settings.width;
    if (typeof settings?.height === "number") stats.height = settings.height;
    if (typeof settings?.frameRate === "number") {
      stats.framesPerSecond = settings.frameRate;
    }
    if (typeof outbound.framesSent === "number") {
      stats.frames = outbound.framesSent;
    }
  }
  return stats;
}

function statsFor(pc, key, sender) {
  const state = pc[_state];
  let entries;
  try {
    entries = state.closed
      ? (key === null ? state.lastStats ?? [] : [])
      : engineStats(pc, key, sender);
  } catch (error) {
    return PromiseReject(error);
  }
  return PromisePrototypeThen(
    nextTask(null),
    () => new RTCStatsReport(illegalConstructorKey, entries),
  );
}

// What a sender is fed from: the track's source, while the track is live and
// enabled (a disabled track sends nothing, as a muted one in a browser sends
// silence / black).
function updateSenderSource(sender, previous, next) {
  if (previous !== null && previous[_source] !== null) {
    SetPrototypeDelete(previous[_source].senders, sender);
  }
  if (next !== null && next[_source] !== null) {
    SetPrototypeAdd(next[_source].senders, sender);
    // A capture device starts sending (02_media_devices.js).
    next[_source].device?.attached();
  }
}

class RTCRtpSender {
  constructor(key = undefined, pc = undefined, transceiver = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_pc] = pc;
    this[_transceiver] = transceiver;
    this[_state] = {
      track: null,
      // Ever had a track (addTrack doesn't reuse such a transceiver).
      used: false,
      streams: [],
      lastParameters: null,
      encodings: [],
    };
    this[webidl.brand] = webidl.brand;
  }

  get track() {
    webidl.assertBranded(this, RTCRtpSenderPrototype);
    return this[_state].track;
  }
  get transport() {
    webidl.assertBranded(this, RTCRtpSenderPrototype);
    return dtlsTransportOf(this[_pc]);
  }
  get dtmf() {
    webidl.assertBranded(this, RTCRtpSenderPrototype);
    if (this[_transceiver][_state].kind !== "audio") return null;
    this[_state].dtmf ??= new RTCDTMFSender(illegalConstructorKey, this);
    return this[_state].dtmf;
  }

  static getCapabilities(kind) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'getCapabilities' on 'RTCRtpSender'",
    );
    return capabilities(kind, true);
  }

  getParameters() {
    webidl.assertBranded(this, RTCRtpSenderPrototype);
    const pc = this[_pc];
    const transceiver = this[_transceiver];
    let negotiated = { codecs: [], headerExtensions: [], encodings: [] };
    if (!pc[_state].closed) {
      try {
        negotiated = op_webrtc_sender_parameters(
          pc[_rid],
          transceiver[_key],
        ) ?? negotiated;
      } catch {
        // The transceiver is gone.
      }
    }
    const state = this[_state];
    const kind = transceiver[_state].kind;
    const encodings = ArrayPrototypeMap(state.encodings, (e) => {
      const out = { active: e.active };
      if (e.rid !== undefined) out.rid = e.rid;
      if (e.maxBitrate !== undefined) out.maxBitrate = e.maxBitrate;
      if (e.maxFramerate !== undefined) out.maxFramerate = e.maxFramerate;
      if (kind === "video") {
        out.scaleResolutionDownBy = e.scaleResolutionDownBy ?? 1;
      }
      out.priority = e.priority ?? "low";
      out.networkPriority = e.networkPriority ?? "low";
      // webrtc-extensions: whether the sender may vary its frame length (it
      // never does; the member is kept as set).
      out.adaptivePtime = e.adaptivePtime ?? false;
      return out;
    });
    const hasNegotiated = transceiver[_state].currentDirection !== null;
    const params = {
      transactionId: uuid(),
      encodings,
      headerExtensions: hasNegotiated
        ? ArrayPrototypeMap(negotiated.headerExtensions, (h) => ({ ...h }))
        : [],
      rtcp: { cname: pc[_state].cname, reducedSize: false },
      codecs: hasNegotiated
        ? ArrayPrototypeMap(negotiated.codecs, (c) => ({ ...c }))
        : [],
    };
    state.lastParameters = {
      transactionId: params.transactionId,
      snapshot: JSONStringify({
        e: ArrayPrototypeMap(params.encodings, (e) => e.rid ?? null),
        h: params.headerExtensions,
        r: params.rtcp,
        c: params.codecs,
      }),
    };
    // The transaction id is valid until the current task ends.
    const timers = pc[_state].timers;
    const timer = setTimeout(() => {
      SetPrototypeDelete(timers, timer);
      if (state.lastParameters?.transactionId === params.transactionId) {
        state.lastParameters = null;
      }
    }, 0);
    SetPrototypeAdd(timers, timer);
    return params;
  }

  setParameters(parameters, _setParameterOptions = undefined) {
    const branded = brandError(this, RTCRtpSenderPrototype);
    if (branded !== null) return PromiseReject(branded);
    const transceiver = this[_transceiver];
    const state = this[_state];
    try {
      if (transceiver[_state].stopping || transceiver[_state].stopped) {
        throw invalidState("The transceiver is stopped");
      }
      if (parameters === null || typeof parameters !== "object") {
        throw new TypeError("setParameters: parameters are required");
      }
      const last = state.lastParameters;
      if (last === null) {
        throw invalidState("getParameters() must be called first");
      }
      if (String(parameters.transactionId) !== last.transactionId) {
        throw new DOMException(
          "The transactionId doesn't match the last getParameters()",
          "InvalidModificationError",
        );
      }
      if (parameters.encodings === undefined) {
        throw new TypeError("setParameters: 'encodings' is required");
      }
      const encodings = ArrayFrom(parameters.encodings);
      const snapshot = JSONStringify({
        e: ArrayPrototypeMap(encodings, (e) => e?.rid ?? null),
        h: ArrayFrom(parameters.headerExtensions ?? []),
        r: parameters.rtcp ?? {},
        c: ArrayFrom(parameters.codecs ?? []),
      });
      if (
        encodings.length !== state.encodings.length ||
        snapshot !== last.snapshot
      ) {
        throw new DOMException(
          "Read-only parameters changed",
          "InvalidModificationError",
        );
      }
      const next = validateEncodings(
        encodings,
        transceiver[_state].kind,
        "setParameters",
        true,
      );
      state.lastParameters = null;
      state.encodings = next;
      const pc = this[_pc];
      if (!pc[_state].closed) {
        op_webrtc_sender_set_paused(
          pc[_rid],
          transceiver[_key],
          state.track === null || !next[0]?.active,
        );
      }
      return nextTask(null);
    } catch (error) {
      return PromiseReject(error);
    }
  }

  replaceTrack(withTrack) {
    const branded = brandError(this, RTCRtpSenderPrototype);
    if (branded !== null) return PromiseReject(branded);
    const pc = this[_pc];
    const transceiver = this[_transceiver];
    if (
      withTrack !== null &&
      !ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, withTrack)
    ) {
      return PromiseReject(
        new TypeError("replaceTrack: not a MediaStreamTrack or null"),
      );
    }
    if (withTrack !== null && withTrack.kind !== transceiver[_state].kind) {
      return PromiseReject(
        new TypeError(
          `replaceTrack: a ${withTrack.kind} track on a ${
            transceiver[_state].kind
          } sender`,
        ),
      );
    }
    return enqueue(pc, () => {
      if (transceiver[_state].stopping || transceiver[_state].stopped) {
        throw invalidState("The transceiver is stopped");
      }
      return inTask(pc, () => {
        setSenderTrack(this, withTrack);
      });
    });
  }

  setStreams(...streams) {
    webidl.assertBranded(this, RTCRtpSenderPrototype);
    const pc = this[_pc];
    if (pc[_state].closed) throw invalidState("RTCPeerConnection is closed");
    for (let i = 0; i < streams.length; ++i) {
      if (!ObjectPrototypeIsPrototypeOf(MediaStreamPrototype, streams[i])) {
        throw new TypeError("setStreams: not a MediaStream");
      }
    }
    this[_state].streams = ArrayPrototypeMap(streams, (s) => s.id);
    op_webrtc_sender_set_streams(
      pc[_rid],
      this[_transceiver][_key],
      this[_state].streams,
    );
    updateNegotiationNeeded(pc);
  }

  getStats() {
    const branded = brandError(this, RTCRtpSenderPrototype);
    if (branded !== null) return PromiseReject(branded);
    return statsFor(this[_pc], this[_transceiver][_key], true);
  }
}
webidl.configureInterface(RTCRtpSender);
const RTCRtpSenderPrototype = RTCRtpSender.prototype;

function setSenderTrack(sender, track) {
  const state = sender[_state];
  const previous = state.track;
  state.track = track;
  if (track !== null) state.used = true;
  updateSenderSource(sender, previous, track);
  const pc = sender[_pc];
  if (!pc[_state].closed) {
    op_webrtc_sender_set_paused(
      pc[_rid],
      sender[_transceiver][_key],
      track === null || !(state.encodings[0]?.active ?? true),
    );
  }
}

// --- RTCDTMFSender ------------------------------------------------------------

const DTMF_TONES = "0123456789ABCD#*,";

class RTCDTMFToneChangeEvent extends Event {
  #tone;

  constructor(type, eventInitDict = undefined) {
    super(type, eventInitDict);
    this.#tone = eventInitDict?.tone === undefined
      ? ""
      : String(eventInitDict.tone);
  }

  get tone() {
    return this.#tone;
  }
}
webidl.configureInterface(RTCDTMFToneChangeEvent);

// RFC 4733 events go out on the negotiated telephone-event payload type, in
// the sender's SSRC (`media.rs`).
class RTCDTMFSender extends EventTarget {
  constructor(key = undefined, sender = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_state] = { sender, toneBuffer: "", playing: false };
    this[webidl.brand] = webidl.brand;
  }

  get toneBuffer() {
    webidl.assertBranded(this, RTCDTMFSenderPrototype);
    return this[_state].toneBuffer;
  }

  get canInsertDTMF() {
    webidl.assertBranded(this, RTCDTMFSenderPrototype);
    return canInsertDTMF(this[_state].sender);
  }

  insertDTMF(tones, duration = 100, interToneGap = 70) {
    webidl.assertBranded(this, RTCDTMFSenderPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'insertDTMF' on 'RTCDTMFSender'",
    );
    const sender = this[_state].sender;
    const t = sender[_transceiver][_state];
    if (t.stopping || t.stopped || sender[_pc][_state].closed) {
      throw invalidState("insertDTMF: the transceiver is stopped");
    }
    if (
      t.currentDirection === "recvonly" || t.currentDirection === "inactive"
    ) {
      throw invalidState(
        `insertDTMF: the transceiver's direction is '${t.currentDirection}'`,
      );
    }
    const buffer = StringPrototypeToUpperCase(String(tones));
    for (let i = 0; i < buffer.length; ++i) {
      if (!StringPrototypeIncludes(DTMF_TONES, buffer[i])) {
        throw new DOMException(
          `insertDTMF: '${buffer[i]}' is not a DTMF tone`,
          "InvalidCharacterError",
        );
      }
    }
    const state = this[_state];
    state.toneBuffer = buffer;
    state.duration = MathMin(
      MathMax(webidl.converters["unsigned long"](duration), 40),
      6000,
    );
    state.gap = MathMax(webidl.converters["unsigned long"](interToneGap), 30);
    if (!state.playing && buffer !== "") {
      state.playing = true;
      dtmfLater(this, 0);
    }
  }
}
defineHandler(RTCDTMFSender.prototype, "tonechange");
webidl.configureInterface(RTCDTMFSender);
const RTCDTMFSenderPrototype = RTCDTMFSender.prototype;

function canInsertDTMF(sender) {
  const t = sender[_transceiver][_state];
  const pc = sender[_pc];
  if (
    pc[_state].closed || t.stopping || t.stopped ||
    sender[_state].track === null
  ) {
    return false;
  }
  if (
    t.currentDirection !== "sendrecv" && t.currentDirection !== "sendonly"
  ) {
    return false;
  }
  try {
    const params = op_webrtc_sender_parameters(
      pc[_rid],
      sender[_transceiver][_key],
    );
    return ArrayPrototypeSome(
      params?.codecs ?? [],
      (c) => StringPrototypeToLowerCase(c.mimeType) === "audio/telephone-event",
    );
  } catch {
    return false;
  }
}

function dtmfLater(dtmf, delay) {
  const pc = dtmf[_state].sender[_pc];
  const timers = pc[_state].timers;
  const timer = setTimeout(() => {
    SetPrototypeDelete(timers, timer);
    dtmfPlayout(dtmf);
  }, delay);
  SetPrototypeAdd(timers, timer);
}

// The spec's DTMF playout task: one tone per run, `tonechange` for each,
// and an empty `tonechange` when the buffer runs out.
function dtmfPlayout(dtmf) {
  const state = dtmf[_state];
  const sender = state.sender;
  const pc = sender[_pc];
  const t = sender[_transceiver][_state];
  if (pc[_state].closed || t.stopping || t.stopped) {
    state.playing = false;
    return;
  }
  if (state.toneBuffer === "") {
    state.playing = false;
    fire(dtmf, new RTCDTMFToneChangeEvent("tonechange", { tone: "" }));
    return;
  }
  const tone = state.toneBuffer[0];
  state.toneBuffer = StringPrototypeSlice(state.toneBuffer, 1);
  fire(dtmf, new RTCDTMFToneChangeEvent("tonechange", { tone }));
  if (tone === ",") {
    dtmfLater(dtmf, 2000);
    return;
  }
  const event = tone === "*"
    ? 10
    : tone === "#"
    ? 11
    : tone >= "A"
    ? 12 + (StringPrototypeCharCodeAt(tone, 0) - 65)
    : StringPrototypeCharCodeAt(tone, 0) - 48;
  if (!pc[_state].closed) {
    op_webrtc_sender_send_dtmf(
      pc[_rid],
      sender[_transceiver][_key],
      event,
      state.duration,
    );
  }
  dtmfLater(dtmf, state.duration + state.gap);
}

class RTCRtpReceiver {
  constructor(key = undefined, pc = undefined, transceiver = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_pc] = pc;
    this[_transceiver] = transceiver;
    this[_state] = {
      track: null,
      clones: [],
      jitterBufferTarget: null,
      // [[AssociatedRemoteMediaStreams]]
      streams: [],
      receiving: false,
    };
    this[_state].track = new MediaStreamTrack(illegalConstructorKey, {
      kind: transceiver[_state].kind,
      label: `remote ${transceiver[_state].kind}`,
      muted: true,
      receiver: this,
    });
    this[webidl.brand] = webidl.brand;
  }

  get track() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    return this[_state].track;
  }
  get transport() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    return dtlsTransportOf(this[_pc]);
  }
  get jitterBufferTarget() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    return this[_state].jitterBufferTarget;
  }
  set jitterBufferTarget(value) {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    if (value === null) {
      this[_state].jitterBufferTarget = null;
      return;
    }
    const target = webidl.converters["double"](value);
    if (target < 0 || target > 4000) {
      throw new RangeError("jitterBufferTarget must be between 0 and 4000");
    }
    this[_state].jitterBufferTarget = target;
  }

  static getCapabilities(kind) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'getCapabilities' on 'RTCRtpReceiver'",
    );
    return capabilities(kind, false);
  }

  getParameters() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    const pc = this[_pc];
    const transceiver = this[_transceiver];
    let negotiated = { codecs: [], headerExtensions: [] };
    if (!pc[_state].closed && transceiver[_state].currentDirection !== null) {
      try {
        negotiated = op_webrtc_receiver_parameters(
          pc[_rid],
          transceiver[_key],
        ) ?? negotiated;
      } catch {
        // The transceiver is gone.
      }
    }
    return {
      headerExtensions: ArrayPrototypeMap(
        negotiated.headerExtensions,
        (h) => ({ ...h }),
      ),
      rtcp: { reducedSize: false },
      codecs: ArrayPrototypeMap(negotiated.codecs, (c) => ({ ...c })),
    };
  }

  getContributingSources() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    return receiverSources(this, true);
  }

  getSynchronizationSources() {
    webidl.assertBranded(this, RTCRtpReceiverPrototype);
    return receiverSources(this, false);
  }

  getStats() {
    const branded = brandError(this, RTCRtpReceiverPrototype);
    if (branded !== null) return PromiseReject(branded);
    return statsFor(this[_pc], this[_transceiver][_key], false);
  }
}
webidl.configureInterface(RTCRtpReceiver);
const RTCRtpReceiverPrototype = RTCRtpReceiver.prototype;

// --- Encoded transforms: `sender.transform` / `receiver.transform` -----------
// (WebRTC Encoded Transform; the rest is 03_encoded_transform.js, loaded when
// a transform is first set.)

const _transform = Symbol("[[transform]]");
let encodedTransformMod;
const loadEncodedTransform = () =>
  encodedTransformMod ??
    (encodedTransformMod = core.loadExtScript(
      "ext:deno_webrtc/03_encoded_transform.js",
    ));

function defineTransformAttribute(prototype, side) {
  // An accessor literal, so the functions are named `get transform` /
  // `set transform` as Web IDL has them.
  const { get, set } = ObjectGetOwnPropertyDescriptor({
    get transform() {
      webidl.assertBranded(this, prototype);
      const transform = this[_transform] ?? null;
      // createEncodedStreams()'s internal transform is not one.
      if (
        transform !== null && loadEncodedTransform().isEncodedStreams(transform)
      ) {
        return null;
      }
      return transform;
    },
    set transform(value) {
      webidl.assertBranded(this, prototype);
      loadEncodedTransform().setTransform(this, side, value ?? null);
    },
  }, "transform");
  ObjectDefineProperty(prototype, "transform", {
    __proto__: null,
    configurable: true,
    enumerable: true,
    get,
    set,
  });
}
defineTransformAttribute(RTCRtpSenderPrototype, "sender");
defineTransformAttribute(RTCRtpReceiverPrototype, "receiver");

// `createEncodedStreams()`: the insertable streams API that preceded
// RTCRtpScriptTransform, as Chrome has it (03_encoded_transform.js).
function defineCreateEncodedStreams(prototype, side) {
  const { createEncodedStreams } = {
    createEncodedStreams() {
      webidl.assertBranded(this, prototype);
      return loadEncodedTransform().createEncodedStreams(this, side);
    },
  };
  ObjectDefineProperty(prototype, "createEncodedStreams", {
    __proto__: null,
    configurable: true,
    enumerable: true,
    writable: true,
    value: createEncodedStreams,
  });
}
defineCreateEncodedStreams(RTCRtpSenderPrototype, "sender");
defineCreateEncodedStreams(RTCRtpReceiverPrototype, "receiver");

function receiverSources(receiver, contributing) {
  const pc = receiver[_pc];
  if (pc[_state].closed) return [];
  try {
    const kind = receiver[_transceiver][_state].kind;
    return ArrayPrototypeMap(
      op_webrtc_receiver_sources(
        pc[_rid],
        receiver[_transceiver][_key],
        contributing,
      ),
      (s) => {
        const out = {
          timestamp: s.timestamp,
          source: s.source,
          rtpTimestamp: s.rtpTimestamp,
        };
        // No audio level: that needs the RFC 6464 header extension, which
        // isn't negotiated.
        if (kind === "audio" && s.audioLevel !== undefined) {
          out.audioLevel = s.audioLevel;
        }
        return out;
      },
    );
  } catch {
    return [];
  }
}

const DIRECTIONS = ["sendrecv", "sendonly", "recvonly", "inactive"];

class RTCRtpTransceiver {
  constructor(key = undefined, pc = undefined, init = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_pc] = pc;
    this[_key] = init.key;
    this[_state] = {
      kind: init.kind,
      mid: init.mid ?? null,
      direction: init.direction ?? "sendrecv",
      currentDirection: null,
      stopping: false,
      stopped: false,
    };
    this[_state].sender = new RTCRtpSender(illegalConstructorKey, pc, this);
    this[_state].receiver = new RTCRtpReceiver(
      illegalConstructorKey,
      pc,
      this,
    );
    this[webidl.brand] = webidl.brand;
  }

  get mid() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return this[_state].mid;
  }
  get sender() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return this[_state].sender;
  }
  get receiver() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return this[_state].receiver;
  }
  get direction() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const state = this[_state];
    return state.stopping || state.stopped ? "stopped" : state.direction;
  }
  set direction(value) {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const direction = String(value);
    if (!ArrayPrototypeIncludes(DIRECTIONS, direction)) {
      throw new TypeError(
        `'${direction}' is not a valid value for 'direction'`,
      );
    }
    const pc = this[_pc];
    const state = this[_state];
    if (pc[_state].closed) throw invalidState("RTCPeerConnection is closed");
    if (state.stopping || state.stopped) {
      throw invalidState("The transceiver is stopped");
    }
    if (state.direction === direction) return;
    state.direction = direction;
    op_webrtc_transceiver_set_direction(pc[_rid], this[_key], direction);
    updateNegotiationNeeded(pc);
  }
  get currentDirection() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const state = this[_state];
    return state.stopped ? "stopped" : state.currentDirection;
  }

  stop() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const pc = this[_pc];
    if (pc[_state].closed) throw invalidState("RTCPeerConnection is closed");
    const state = this[_state];
    if (state.stopping || state.stopped) return;
    stopSending(this);
    state.stopping = true;
    op_webrtc_transceiver_stop(pc[_rid], this[_key]);
    updateNegotiationNeeded(pc);
  }

  setCodecPreferences(codecs) {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'setCodecPreferences' on 'RTCRtpTransceiver'",
    );
    const pc = this[_pc];
    const list = codecs === undefined || codecs === null
      ? []
      : ArrayFrom(codecs);
    if (list.length > 0) {
      const kind = this[_state].kind;
      const send = capabilities(kind, true)?.codecs ?? [];
      const recv = capabilities(kind, false)?.codecs ?? [];
      const known = (c) => {
        const match = (k) =>
          StringPrototypeToLowerCase(k.mimeType) ===
            StringPrototypeToLowerCase(String(c.mimeType)) &&
          k.clockRate === c.clockRate &&
          (k.channels ?? null) === (c.channels ?? null) &&
          (k.sdpFmtpLine ?? null) === (c.sdpFmtpLine ?? null);
        return ArrayPrototypeSome(recv, match) ||
          ArrayPrototypeSome(send, match);
      };
      for (let i = 0; i < list.length; ++i) {
        const c = list[i];
        if (c === null || typeof c !== "object" || !known(c)) {
          throw new DOMException(
            `setCodecPreferences: ${c?.mimeType} is not a supported codec`,
            "InvalidModificationError",
          );
        }
      }
      // Only repair formats (RTX, RED, FEC) isn't a usable preference.
      const onlyRepair = ArrayPrototypeEvery(list, (c) => {
        const m = StringPrototypeToLowerCase(String(c.mimeType));
        return StringPrototypeEndsWith(m, "/rtx") ||
          StringPrototypeEndsWith(m, "/red") ||
          StringPrototypeIncludes(m, "fec");
      });
      if (onlyRepair) {
        throw new DOMException(
          "setCodecPreferences: only repair codecs",
          "InvalidModificationError",
        );
      }
    }
    if (pc[_state].closed) return;
    op_webrtc_transceiver_set_codec_preferences(
      pc[_rid],
      this[_key],
      ArrayPrototypeMap(list, (c) => ({
        mimeType: String(c.mimeType),
        clockRate: c.clockRate,
        channels: c.channels ?? null,
        sdpFmtpLine: c.sdpFmtpLine ?? null,
      })),
    );
  }
}
webidl.configureInterface(RTCRtpTransceiver);
const RTCRtpTransceiverPrototype = RTCRtpTransceiver.prototype;

function stopSending(transceiver) {
  const sender = transceiver[_state].sender;
  updateSenderSource(sender, sender[_state].track, null);
  const receiver = transceiver[_state].receiver;
  removeRemoteTrack(transceiver);
  closeReaders(receiver);
  endTrack(receiver[_state].track);
  for (let i = 0; i < receiver[_state].clones.length; ++i) {
    endTrack(receiver[_state].clones[i]);
  }
}

class RTCTrackEvent extends Event {
  #init;

  constructor(type, eventInitDict) {
    super(type, eventInitDict);
    const init = eventInitDict ?? { __proto__: null };
    if (
      !ObjectPrototypeIsPrototypeOf(RTCRtpReceiverPrototype, init.receiver) ||
      !ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, init.track) ||
      !ObjectPrototypeIsPrototypeOf(
        RTCRtpTransceiverPrototype,
        init.transceiver,
      )
    ) {
      throw new TypeError(
        "Failed to construct 'RTCTrackEvent': receiver, track and transceiver are required",
      );
    }
    this.#init = {
      receiver: init.receiver,
      track: init.track,
      streams: ObjectFreeze(ArrayFrom(init.streams ?? [])),
      transceiver: init.transceiver,
    };
  }

  get receiver() {
    return this.#init.receiver;
  }
  get track() {
    return this.#init.track;
  }
  get streams() {
    return this.#init.streams;
  }
  get transceiver() {
    return this.#init.transceiver;
  }
}
webidl.configureInterface(RTCTrackEvent);

// sendEncodings / setParameters encodings, validated as the spec says.
function validateEncodings(list, kind, method, keepRid) {
  const out = [];
  const rids = new SafeSet();
  for (let i = 0; i < list.length; ++i) {
    const e = list[i];
    if (e === null || typeof e !== "object") {
      throw new TypeError(`${method}: an encoding must be an object`);
    }
    const enc = { active: e.active === undefined ? true : !!e.active };
    if (e.rid !== undefined) {
      // At most 16 alphanumeric characters (the RtpStreamId header
      // extension's limit).
      const rid = String(e.rid);
      if (
        rid.length === 0 || rid.length > 16 ||
        !RegExpPrototypeTest(RID_RE, rid)
      ) {
        throw new TypeError(`${method}: '${rid}' is not a valid rid`);
      }
      if (SetPrototypeHas(rids, rid)) {
        throw new TypeError(`${method}: the rid '${rid}' is repeated`);
      }
      SetPrototypeAdd(rids, rid);
      enc.rid = rid;
    } else if (list.length > 1 && !keepRid) {
      throw new TypeError(`${method}: every encoding of several needs a rid`);
    }
    if (e.maxBitrate !== undefined) {
      enc.maxBitrate = webidl.converters["unsigned long"](e.maxBitrate);
    }
    // An audio encoding has no frame rate: its maxFramerate is dropped,
    // even an invalid one.
    if (e.maxFramerate !== undefined && kind === "video") {
      const f = webidl.converters["double"](e.maxFramerate);
      if (f < 0) throw new RangeError(`${method}: maxFramerate below 0`);
      enc.maxFramerate = f;
    }
    if (e.scaleResolutionDownBy !== undefined && kind === "video") {
      const s = webidl.converters["double"](e.scaleResolutionDownBy);
      if (s < 1) {
        throw new RangeError(`${method}: scaleResolutionDownBy below 1`);
      }
      enc.scaleResolutionDownBy = s;
    }
    if (e.priority !== undefined) enc.priority = String(e.priority);
    if (e.networkPriority !== undefined) {
      enc.networkPriority = String(e.networkPriority);
    }
    if (e.adaptivePtime !== undefined) enc.adaptivePtime = !!e.adaptivePtime;
    ArrayPrototypePush(out, enc);
  }
  if (!keepRid) {
    // addTransceiver(): at most MAX_ENCODINGS simulcast encodings, one for
    // audio; a lone encoding has no rid; and a video encoding with no
    // scaleResolutionDownBy gets 1 when another has one, else each a power of
    // two (the last is full size).
    const max = kind === "video" ? MAX_ENCODINGS : 1;
    if (out.length > max) ArrayPrototypeSplice(out, max, out.length - max);
    if (out.length === 1) delete out[0].rid;
    if (kind === "video") {
      const some = ArrayPrototypeSome(
        out,
        (enc) => enc.scaleResolutionDownBy !== undefined,
      );
      for (let i = 0; i < out.length; ++i) {
        out[i].scaleResolutionDownBy ??= some ? 1 : 2 ** (out.length - 1 - i);
      }
    }
  }
  return out;
}

// The simulcast encodings one sender may have.
const MAX_ENCODINGS = 16;
const RID_RE = new SafeRegExp("^[A-Za-z0-9]+$");

// Each transceiver's remote track state before a remote offer is applied, so
// that rolling the offer back can undo what it did (W3C "set the
// RTCSessionDescription", rollback: the remote streams are restored and a
// track the offer started receiving is removed).
function remoteTrackSnapshot(pc) {
  const snapshot = new SafeMap();
  const transceivers = pc[_state].transceivers;
  for (let i = 0; i < transceivers.length; ++i) {
    const rstate = transceivers[i][_state].receiver[_state];
    MapPrototypeSet(snapshot, transceivers[i], {
      receiving: rstate.receiving,
      streams: rstate.streams,
    });
  }
  return snapshot;
}

function restoreRemoteTracks(pc) {
  const state = pc[_state];
  const snapshot = state.remoteTracksBeforeOffer ?? new SafeMap();
  state.remoteTracksBeforeOffer = null;
  const transceivers = state.transceivers;
  const revived = [];
  for (let i = 0; i < transceivers.length; ++i) {
    const t = transceivers[i];
    const rstate = t[_state].receiver[_state];
    const before = MapPrototypeGet(snapshot, t);
    if (!rstate.receiving) {
      // The offer took the track away (its section went inactive or
      // recvonly): rolled back, the track is received again, in its streams,
      // and announced with a track event as when it first arrived.
      if (before !== undefined && before.receiving && !t[_state].stopped) {
        for (let j = 0; j < before.streams.length; ++j) {
          streamAddRemote(before.streams[j], rstate.track);
        }
        rstate.streams = before.streams;
        rstate.receiving = true;
        ArrayPrototypePush(revived, t);
      }
      continue;
    }
    if (before === undefined || !before.receiving) {
      removeRemoteTrack(t);
      continue;
    }
    const now = rstate.streams;
    for (let j = 0; j < now.length; ++j) {
      if (!ArrayPrototypeIncludes(before.streams, now[j])) {
        streamRemoveRemote(now[j], rstate.track);
      }
    }
    for (let j = 0; j < before.streams.length; ++j) {
      if (!ArrayPrototypeIncludes(now, before.streams[j])) {
        streamAddRemote(before.streams[j], rstate.track);
      }
    }
    rstate.streams = before.streams;
  }
  for (let i = 0; i < revived.length; ++i) {
    const receiver = revived[i][_state].receiver;
    fire(
      pc,
      new RTCTrackEvent("track", {
        receiver,
        track: receiver[_state].track,
        streams: receiver[_state].streams,
        transceiver: revived[i],
      }),
    );
  }
}

// The process of removing a remote track (the remote side stopped sending).
function removeRemoteTrack(transceiver) {
  const receiver = transceiver[_state].receiver;
  const rstate = receiver[_state];
  if (!rstate.receiving) return;
  rstate.receiving = false;
  const streams = rstate.streams;
  rstate.streams = [];
  for (let i = 0; i < streams.length; ++i) {
    streamRemoveRemote(streams[i], rstate.track);
  }
  setMuted(rstate.track, true);
}

// --- RTCPeerConnection -----------------------------------------------------

function convertIceServers(servers) {
  if (servers === undefined) return [];
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
    if (urls.length === 0) {
      throw new DOMException(
        "Failed to construct 'RTCPeerConnection': an RTCIceServer's 'urls' is empty",
        "SyntaxError",
      );
    }
    for (let j = 0; j < urls.length; ++j) {
      if (StringPrototypeIncludes(urls[j], "#")) {
        throw new DOMException(
          `Failed to construct 'RTCPeerConnection': '${
            urls[j]
          }' has a fragment`,
          "SyntaxError",
        );
      }
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

function convertConfiguration(configuration, prefix) {
  const config = configuration ?? { __proto__: null };
  if (typeof config !== "object") {
    throw new TypeError(`${prefix}: the configuration must be an object`);
  }
  const iceTransportPolicy = config.iceTransportPolicy === undefined
    ? "all"
    : String(config.iceTransportPolicy);
  if (iceTransportPolicy !== "all" && iceTransportPolicy !== "relay") {
    throw new TypeError(
      `${prefix}: '${iceTransportPolicy}' is not a valid value for 'iceTransportPolicy'`,
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
      `${prefix}: '${bundlePolicy}' is not a valid value for 'bundlePolicy'`,
    );
  }
  const rtcpMuxPolicy = config.rtcpMuxPolicy === undefined
    ? "require"
    : String(config.rtcpMuxPolicy);
  if (rtcpMuxPolicy !== "require") {
    throw new TypeError(
      `${prefix}: '${rtcpMuxPolicy}' is not a valid value for 'rtcpMuxPolicy'`,
    );
  }
  const iceCandidatePoolSize = config.iceCandidatePoolSize === undefined
    ? 0
    : webidl.converters["octet"](config.iceCandidatePoolSize, prefix, "", {
      enforceRange: true,
    });
  let certificates = [];
  if (config.certificates !== undefined) {
    if (
      config.certificates === null ||
      typeof config.certificates[SymbolIterator] !== "function"
    ) {
      throw new TypeError(`${prefix}: 'certificates' must be a sequence`);
    }
    certificates = ArrayFrom(config.certificates);
    for (let i = 0; i < certificates.length; ++i) {
      if (
        !ObjectPrototypeIsPrototypeOf(
          RTCCertificatePrototype,
          certificates[i],
        )
      ) {
        throw new TypeError(`${prefix}: not an RTCCertificate`);
      }
    }
  }
  const network = config.denoNetwork ?? null;
  if (
    network !== null &&
    !ObjectPrototypeIsPrototypeOf(RtcNetworkPrototype, network)
  ) {
    throw new TypeError(
      `${prefix}: 'denoNetwork' must be a Deno.webrtc.Network`,
    );
  }
  if (network !== null && network[_state].closed) {
    throw new DOMException(
      `${prefix}: the Deno.webrtc.Network is closed`,
      "InvalidStateError",
    );
  }
  return {
    iceServers: convertIceServers(config.iceServers),
    iceTransportPolicy,
    bundlePolicy,
    rtcpMuxPolicy,
    iceCandidatePoolSize,
    certificates,
    network,
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

// The legacy callback forms (successCallback, failureCallback): the promise
// form runs and settles the callbacks; the returned promise resolves to
// undefined.
function legacy(promise, success, failure) {
  return PromisePrototypeThen(
    promise,
    (value) => {
      FunctionPrototypeCall(success, undefined, value);
    },
    (error) => {
      if (typeof failure === "function") {
        FunctionPrototypeCall(failure, undefined, error);
      }
    },
  );
}

class RTCPeerConnection extends EventTarget {
  constructor(configuration = undefined) {
    super();
    const prefix = "Failed to construct 'RTCPeerConnection'";
    const config = convertConfiguration(configuration, prefix);
    const now = DateNow();
    for (let i = 0; i < config.certificates.length; ++i) {
      if (config.certificates[i].expires <= now) {
        throw new DOMException(
          `${prefix}: an RTCCertificate has expired`,
          "InvalidAccessError",
        );
      }
    }
    this[_rid] = op_webrtc_pc_create({
      iceServers: config.iceServers,
      iceTransportPolicy: config.iceTransportPolicy,
      bundlePolicy: config.bundlePolicy,
      certificates: ArrayPrototypeMap(
        config.certificates,
        (c) => c[_certRid],
      ),
      network: config.network === null ? null : config.network[_rid],
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
      sctpState: null,
      dtlsState: null,
      dtls: null,
      transceivers: [],
      // Remote MediaStreams by id.
      streams: new SafeMap(),
      localCandidates: [],
      remoteCandidates: [],
      remoteTracksBeforeOffer: null,
      cname: uuid(),
      // "controlling" / "controlled" once an answer is applied.
      iceRole: null,
      // The spec's [[NegotiationNeeded]] and
      // [[UpdateNegotiationNeededFlagOnEmptyChain]] slots.
      negotiationNeeded: false,
      updateNegotiationOnEmptyChain: false,
      // [[LocalIceCredentialsToReplace]]: "ufrag pwd" strings restartIce()
      // found in the local descriptions.
      iceCredentialsToReplace: new SafeSet(),
      // A changed ICE configuration restarts ICE in the next offer.
      configRestart: false,
      // Whether a data channel was ever created (it needs an m=application
      // section).
      hadDataChannel: false,
      // The type of the current local description.
      currentLocalType: null,
      // RTCSessionDescription objects by slot, kept while unchanged.
      descriptions: {
        __proto__: null,
        currentLocal: null,
        pendingLocal: null,
        currentRemote: null,
        pendingRemote: null,
      },
      // The four slots as close() found them.
      last: null,
      // getParameters() transaction timers, cleared by close().
      timers: new SafeSet(),
      // The spec's [[Operations]] chain: createOffer / createAnswer /
      // set*Description / addIceCandidate / replaceTrack run one at a
      // time, in call order.
      operations: [],
    };
    if (config.network !== null) {
      SetPrototypeAdd(config.network[_state].connections, this);
    }
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
    const remote = this.remoteDescription;
    return this[_state].closed || remote === null
      ? null
      : offersTrickle(remote.sdp);
  }
  get localDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "pendingLocal") ??
      descriptionSlot(this, "currentLocal");
  }
  get remoteDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "pendingRemote") ??
      descriptionSlot(this, "currentRemote");
  }
  get currentLocalDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "currentLocal");
  }
  get pendingLocalDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "pendingLocal");
  }
  get currentRemoteDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "currentRemote");
  }
  get pendingRemoteDescription() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return descriptionSlot(this, "pendingRemote");
  }
  get sctp() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return this[_state].sctp;
  }

  createOffer(
    options = undefined,
    failure = undefined,
    legacyOptions = undefined,
  ) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (typeof options === "function") {
      return legacy(this.createOffer(legacyOptions), options, failure);
    }
    return enqueue(this, () => {
      const state = this[_state];
      const signaling = state.signalingState;
      if (signaling !== "stable" && signaling !== "have-local-offer") {
        throw invalidState(
          `Cannot create an offer in signaling state '${signaling}'`,
        );
      }
      return inTask(this, () => {
        // Legacy offerToReceiveAudio / offerToReceiveVideo: a recvonly
        // transceiver of that kind when there is none to receive on.
        const kinds = ["audio", "video"];
        for (let k = 0; k < kinds.length; ++k) {
          const kind = kinds[k];
          const key = kind === "audio"
            ? "offerToReceiveAudio"
            : "offerToReceiveVideo";
          if (options?.[key] === undefined) continue;
          const want = !!options[key];
          const existing = ArrayPrototypeFilter(
            state.transceivers,
            (t) => t[_state].kind === kind && !t[_state].stopped,
          );
          if (want && existing.length === 0) {
            this.addTransceiver(kind, { direction: "recvonly" });
          } else if (!want) {
            for (let i = 0; i < existing.length; ++i) {
              const t = existing[i];
              if (t[_state].direction === "sendrecv") {
                t.direction = "sendonly";
              } else if (t[_state].direction === "recvonly") {
                t.direction = "inactive";
              }
            }
          }
        }
        // A plain RTCSessionDescriptionInit, as the spec has it (libraries
        // such as simple-peer edit its `sdp` before setLocalDescription).
        return {
          type: "offer",
          sdp: createOfferSdp(this, !!options?.iceRestart),
        };
      });
    });
  }

  createAnswer(options = undefined, failure = undefined) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (typeof options === "function") {
      return legacy(this.createAnswer(), options, failure);
    }
    return enqueue(this, () => {
      const state = this[_state].signalingState;
      if (state !== "have-remote-offer" && state !== "have-local-pranswer") {
        throw invalidState(
          `Cannot create an answer in signaling state '${state}'`,
        );
      }
      return inTask(this, () => ({
        type: "answer",
        sdp: op_webrtc_pc_create_answer(this[_rid]),
      }));
    });
  }

  setLocalDescription(
    description = undefined,
    success = undefined,
    failure = undefined,
  ) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (typeof success === "function") {
      return legacy(this.setLocalDescription(description), success, failure);
    }
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
      if (type === "rollback" && signaling === "stable") {
        throw invalidState(
          "Nothing to roll back in signaling state 'stable'",
        );
      }
      if (
        (type === "answer" || type === "pranswer") &&
        signaling !== "have-remote-offer" &&
        signaling !== "have-local-pranswer"
      ) {
        throw invalidState(
          `Cannot set a local ${type} in signaling state '${signaling}'`,
        );
      }
      if (
        type === "offer" && signaling !== "stable" &&
        signaling !== "have-local-offer"
      ) {
        throw invalidState(
          `Cannot set a local offer in signaling state '${signaling}'`,
        );
      }
      return inTask(this, () => {
        if (type !== "rollback" && (sdp === undefined || sdp === "")) {
          sdp = type === "offer"
            ? createOfferSdp(this, false)
            : op_webrtc_pc_create_answer(this[_rid]);
        }
        op_webrtc_pc_set_local_description(this[_rid], type, sdp ?? "");
        const state = this[_state];
        if (type === "answer" || type === "pranswer") {
          state.iceRole ??= "controlled";
          readChannelIds(this);
        }
        activate(this);
        descriptionApplied(this, true, type);
        syncTransceivers(this, { remote: false, type });
        setSignalingState(this, nextSignalingState(signaling, true, type));
        updateSctpTransport(this, type);
        setTransportStates(this);
        negotiationSettled(this);
      });
    });
  }

  setRemoteDescription(
    description,
    success = undefined,
    failure = undefined,
  ) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (typeof success === "function") {
      return legacy(this.setRemoteDescription(description), success, failure);
    }
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
      if (type === "rollback" && signaling === "stable") {
        throw invalidState(
          "Nothing to roll back in signaling state 'stable'",
        );
      }
      if (
        (type === "answer" || type === "pranswer") &&
        signaling !== "have-local-offer" &&
        signaling !== "have-remote-pranswer"
      ) {
        throw invalidState(
          `Cannot set a remote ${type} in signaling state '${signaling}'`,
        );
      }
      const apply = () => {
        const sdp = description.sdp === undefined
          ? ""
          : String(description.sdp);
        if (type !== "rollback" && StringPrototypeTrim(sdp) === "") {
          throw new DOMException(
            "setRemoteDescription: the description has no SDP",
            "InvalidAccessError",
          );
        }
        if (type !== "rollback") {
          requireSdpSyntax(sdp);
          requireRtcpMux(sdp);
        }
        const offerFromStable = type === "offer" &&
          this[_state].signalingState === "stable";
        const before = offerFromStable ? remoteTrackSnapshot(this) : null;
        if (type === "offer") syncDirections(this);
        op_webrtc_pc_set_remote_description(this[_rid], type, sdp);
        const state = this[_state];
        if (offerFromStable) state.remoteTracksBeforeOffer = before;
        if (type !== "rollback") {
          state.remoteMaxMessageSize = maxMessageSizeOf(sdp);
        }
        if (type === "answer" || type === "pranswer") {
          readChannelIds(this);
          state.iceRole ??= "controlling";
        }
        activate(this);
        const now = this[_state].signalingState;
        descriptionApplied(this, false, type);
        setSignalingState(this, nextSignalingState(now, false, type));
        if (type === "rollback") restoreRemoteTracks(this);
        syncTransceivers(this, { remote: true, type });
        updateSctpTransport(this, type);
        setTransportStates(this);
        negotiationSettled(this);
      };
      if (type === "offer" && signaling === "have-local-offer") {
        // Implicit rollback (perfect negotiation's polite peer), a task of
        // its own: signaling is stable, with its event, before the offer is
        // applied in the next one. The rollback stands if the offer fails.
        return PromisePrototypeThen(
          inTask(this, () => {
            op_webrtc_pc_set_local_description(this[_rid], "rollback", "");
            descriptionApplied(this, true, "rollback");
            syncTransceivers(this, { remote: false, type: "rollback" });
            setSignalingState(this, "stable");
          }),
          () => inTask(this, apply),
        );
      }
      return inTask(this, apply);
    });
  }

  addIceCandidate(
    candidate = undefined,
    success = undefined,
    failure = undefined,
  ) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (typeof success === "function") {
      return legacy(this.addIceCandidate(candidate), success, failure);
    }
    if (
      candidate !== undefined && candidate !== null &&
      candidate.candidate !== undefined && candidate.candidate !== "" &&
      (candidate.sdpMid ?? null) === null &&
      (candidate.sdpMLineIndex ?? null) === null
    ) {
      return PromiseReject(
        new TypeError(
          "Failed to execute 'addIceCandidate': sdpMid and sdpMLineIndex are both null",
        ),
      );
    }
    return enqueue(this, () => {
      // No candidate (undefined, null or `{}`) is the end-of-candidates
      // indication for every media description.
      candidate ??= { __proto__: null };
      const line = candidate.candidate === undefined
        ? ""
        : String(candidate.candidate);
      const sdpMid = candidate.sdpMid ?? null;
      const sdpMLineIndex = candidate.sdpMLineIndex ?? null;
      if (this.remoteDescription === null) {
        throw invalidState(
          "addIceCandidate needs a remote description first",
        );
      }
      return inTask(this, () => {
        if (sdpMid !== null) {
          const mids = midsOf(this.remoteDescription.sdp);
          if (!ArrayPrototypeIncludes(mids, String(sdpMid))) {
            throw new DOMException(
              `addIceCandidate: no media section with mid '${sdpMid}'`,
              "OperationError",
            );
          }
        } else if (sdpMLineIndex !== null) {
          const count = midsOf(this.remoteDescription.sdp).length;
          if (sdpMLineIndex >= count) {
            throw new DOMException(
              `addIceCandidate: no media section ${sdpMLineIndex}`,
              "OperationError",
            );
          }
        }
        // The engine checks a usernameFragment against its media description's
        // ice-ufrag (each m-section may have its own).
        const ufrag = candidate.usernameFragment ?? null;
        op_webrtc_pc_add_ice_candidate(this[_rid], {
          candidate: line,
          sdpMid: sdpMid === null ? null : String(sdpMid),
          sdpMLineIndex,
          usernameFragment: ufrag,
        });
        if (line !== "") {
          ArrayPrototypePush(
            this[_state].remoteCandidates,
            new RTCIceCandidate({
              candidate: line,
              sdpMid: sdpMid === null ? null : String(sdpMid),
              sdpMLineIndex,
              usernameFragment: ufrag,
            }),
          );
        }
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
    const prefix = "Failed to execute 'createDataChannel'";
    const dict = dataChannelDict ?? { __proto__: null };
    const init = {
      label: webidl.converters.USVString(label, prefix, "Argument 1"),
      ordered: dict.ordered === undefined ? true : !!dict.ordered,
      maxPacketLifeTime: dict.maxPacketLifeTime === undefined ||
          dict.maxPacketLifeTime === null
        ? null
        : webidl.converters["unsigned short"](
          dict.maxPacketLifeTime,
          "",
          "",
          {
            enforceRange: true,
          },
        ),
      maxRetransmits:
        dict.maxRetransmits === undefined || dict.maxRetransmits === null
          ? null
          : webidl.converters["unsigned short"](dict.maxRetransmits, "", "", {
            enforceRange: true,
          }),
      protocol: dict.protocol === undefined
        ? ""
        : webidl.converters.USVString(dict.protocol, prefix, "'protocol'"),
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
    // The id only counts for a negotiated channel; 65535 is reserved and
    // an id an open channel has is in use.
    if (init.negotiated && init.id === 65535) {
      throw new TypeError(`${prefix}: id 65535 is reserved`);
    }
    if (init.negotiated) {
      MapPrototypeForEach(this[_channels], (channel) => {
        if (channel.id === init.id && channel.readyState !== "closed") {
          throw new DOMException(
            `${prefix}: id ${init.id} is in use`,
            "OperationError",
          );
        }
      });
    }
    if (TypedArrayPrototypeGetByteLength(core.encode(init.label)) > 65535) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': label too long",
      );
    }
    if (
      TypedArrayPrototypeGetByteLength(core.encode(init.protocol)) > 65535
    ) {
      throw new TypeError(
        "Failed to execute 'createDataChannel': protocol too long",
      );
    }
    const created = op_webrtc_pc_create_data_channel(this[_rid], {
      label: init.label,
      ordered: init.ordered,
      maxPacketLifeTime: init.maxPacketLifeTime,
      maxRetransmits: init.maxRetransmits,
      protocol: init.protocol,
      negotiated: init.negotiated,
      id: init.negotiated ? init.id : null,
    });
    const channel = new RTCDataChannel(illegalConstructorKey, this, {
      ...init,
      channel: created.channel,
      id: created.id ?? (init.negotiated ? init.id : null),
    });
    MapPrototypeSet(this[_channels], created.channel, channel);
    if (!this[_state].hadDataChannel) {
      this[_state].hadDataChannel = true;
      updateNegotiationNeeded(this);
    }
    // A negotiated channel opens with the connection, without DCEP; its
    // open event comes from the engine like any other.
    return channel;
  }

  getStats(selector = null) {
    const branded = brandError(this, RTCPeerConnectionPrototype);
    if (branded !== null) return PromiseReject(branded);
    if (selector !== null && selector !== undefined) {
      if (
        !ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, selector)
      ) {
        return PromiseReject(
          new TypeError("getStats: the selector must be a MediaStreamTrack"),
        );
      }
      const matches = [];
      const transceivers = this[_state].transceivers;
      for (let i = 0; i < transceivers.length; ++i) {
        const t = transceivers[i];
        if (t[_state].sender[_state].track === selector) {
          ArrayPrototypePush(matches, [t, true]);
        }
        if (t[_state].receiver[_state].track === selector) {
          ArrayPrototypePush(matches, [t, false]);
        }
      }
      if (matches.length !== 1) {
        return PromiseReject(
          new DOMException(
            "getStats: the track is not sent or received by exactly one sender or receiver",
            "InvalidAccessError",
          ),
        );
      }
      return statsFor(this, matches[0][0][_key], matches[0][1]);
    }
    return statsFor(this, null, false);
  }

  getConfiguration() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const config = this[_state].config;
    const out = {
      iceServers: ArrayPrototypeMap(config.iceServers, (s) => {
        const server = { urls: ArrayPrototypeSlice(s.urls) };
        if (s.username !== undefined) server.username = s.username;
        if (s.credential !== undefined) server.credential = s.credential;
        return server;
      }),
      iceTransportPolicy: config.iceTransportPolicy,
      bundlePolicy: config.bundlePolicy,
      rtcpMuxPolicy: config.rtcpMuxPolicy,
      iceCandidatePoolSize: config.iceCandidatePoolSize,
      certificates: ArrayPrototypeSlice(config.certificates),
    };
    if (config.network !== null) out.denoNetwork = config.network;
    return out;
  }

  setConfiguration(configuration = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const prefix =
      "Failed to execute 'setConfiguration' on 'RTCPeerConnection'";
    if (this[_state].closed) {
      throw invalidState("RTCPeerConnection is closed");
    }
    const config = convertConfiguration(configuration, prefix);
    const current = this[_state].config;
    if (
      config.bundlePolicy !== current.bundlePolicy ||
      config.rtcpMuxPolicy !== current.rtcpMuxPolicy
    ) {
      throw new DOMException(
        `${prefix}: bundlePolicy and rtcpMuxPolicy can't change`,
        "InvalidModificationError",
      );
    }
    const sameCerts = config.certificates.length === 0 ||
      (config.certificates.length === current.certificates.length &&
        ArrayPrototypeEvery(
          config.certificates,
          (c, i) => c === current.certificates[i],
        ));
    if (!sameCerts) {
      throw new DOMException(
        `${prefix}: certificates can't change`,
        "InvalidModificationError",
      );
    }
    if (
      config.network !== current.network &&
      configuration?.denoNetwork !== undefined
    ) {
      throw new DOMException(
        `${prefix}: denoNetwork can't change`,
        "InvalidModificationError",
      );
    }
    if (
      config.iceCandidatePoolSize !== current.iceCandidatePoolSize &&
      this.localDescription !== null
    ) {
      throw new DOMException(
        `${prefix}: iceCandidatePoolSize can't change after setLocalDescription`,
        "InvalidModificationError",
      );
    }
    // Validates the ICE server URLs (SyntaxError / InvalidAccessError) and
    // the net permission for them; they apply to the next gathering (an
    // ICE restart).
    op_webrtc_pc_set_ice_servers(this[_rid], {
      iceServers: config.iceServers,
      iceTransportPolicy: config.iceTransportPolicy,
    });
    // RFC 9429 section 4.1.18: a new ICE candidate policy, or new STUN / TURN
    // servers, apply to the next gathering phase; once one has started,
    // they set the needs-ice-restart bit, so the next offer restarts ICE.
    const serverUrls = (servers) =>
      ArrayPrototypeJoin(
        ArrayPrototypeMap(servers, (s) => ArrayPrototypeJoin(s.urls, " ")),
        "\n",
      );
    if (
      (config.iceTransportPolicy !== current.iceTransportPolicy ||
        serverUrls(config.iceServers) !== serverUrls(current.iceServers)) &&
      this.localDescription !== null
    ) {
      this[_state].configRestart = true;
    }
    config.certificates = current.certificates;
    config.network = current.network;
    this[_state].config = config;
  }

  restartIce() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const state = this[_state];
    if (state.closed) return;
    // [[LocalIceCredentialsToReplace]]: every credential of the current and
    // the pending local description. Before the first negotiation there is
    // none, and nothing to restart: the first offer has fresh credentials.
    const replace = new SafeSet();
    const slots = op_webrtc_pc_descriptions(this[_rid]);
    const locals = [slots.currentLocal, slots.pendingLocal];
    for (let i = 0; i < locals.length; ++i) {
      if (locals[i] === null) continue;
      const credentials = iceCredentialsOf(locals[i][1]);
      for (let j = 0; j < credentials.length; ++j) {
        SetPrototypeAdd(replace, credentials[j]);
      }
    }
    state.iceCredentialsToReplace = replace;
    updateNegotiationNeeded(this);
  }

  getSenders() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return ArrayPrototypeMap(
      ArrayPrototypeFilter(
        this[_state].transceivers,
        (t) => !t[_state].stopped,
      ),
      (t) => t[_state].sender,
    );
  }
  getReceivers() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return ArrayPrototypeMap(
      ArrayPrototypeFilter(
        this[_state].transceivers,
        (t) => !t[_state].stopped,
      ),
      (t) => t[_state].receiver,
    );
  }
  getTransceivers() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    return ArrayPrototypeSlice(this[_state].transceivers);
  }

  addTrack(track, ...streams) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'addTrack' on 'RTCPeerConnection'",
    );
    if (!ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, track)) {
      throw new TypeError(
        "Failed to execute 'addTrack' on 'RTCPeerConnection': not a MediaStreamTrack",
      );
    }
    for (let i = 0; i < streams.length; ++i) {
      if (!ObjectPrototypeIsPrototypeOf(MediaStreamPrototype, streams[i])) {
        throw new TypeError(
          "Failed to execute 'addTrack' on 'RTCPeerConnection': not a MediaStream",
        );
      }
    }
    const state = this[_state];
    if (state.closed) throw invalidState("RTCPeerConnection is closed");
    const transceivers = state.transceivers;
    for (let i = 0; i < transceivers.length; ++i) {
      if (transceivers[i][_state].sender[_state].track === track) {
        throw new DOMException(
          "addTrack: the track already has a sender on this connection",
          "InvalidAccessError",
        );
      }
    }
    const streamIds = ArrayPrototypeMap(streams, (s) => s.id);
    // Reuse a transceiver of the same kind whose sender never had a track.
    const reuse = ArrayPrototypeFind(
      transceivers,
      (t) =>
        t[_state].kind === track.kind && !t[_state].stopping &&
        !t[_state].stopped && t[_state].sender[_state].track === null &&
        !t[_state].sender[_state].used,
    );
    if (reuse !== undefined) {
      op_webrtc_pc_attach_track(this[_rid], reuse[_key], {
        id: track.id,
        label: track.label,
      }, streamIds);
      const codec = track[_source]?.codec ?? null;
      if (codec !== null) {
        op_webrtc_transceiver_set_codec_preferences(
          this[_rid],
          reuse[_key],
          trackPreferences(codec),
        );
      }
      const sender = reuse[_state].sender;
      sender[_state].streams = streamIds;
      setSenderTrack(sender, track);
      const d = reuse[_state].direction;
      reuse[_state].direction = d === "recvonly"
        ? "sendrecv"
        : d === "inactive"
        ? "sendonly"
        : d;
      updateNegotiationNeeded(this);
      return sender;
    }
    const transceiver = addTransceiverInner(this, track, {
      direction: "sendrecv",
      streams,
      addTrack: true,
    });
    return transceiver[_state].sender;
  }

  removeTrack(sender) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'removeTrack' on 'RTCPeerConnection'",
    );
    if (!ObjectPrototypeIsPrototypeOf(RTCRtpSenderPrototype, sender)) {
      throw new TypeError(
        "Failed to execute 'removeTrack' on 'RTCPeerConnection': not an RTCRtpSender",
      );
    }
    if (this[_state].closed) {
      throw invalidState("RTCPeerConnection is closed");
    }
    if (sender[_pc] !== this) {
      throw new DOMException(
        "removeTrack: the sender belongs to another connection",
        "InvalidAccessError",
      );
    }
    const transceiver = sender[_transceiver];
    if (transceiver[_state].stopped || sender[_state].track === null) return;
    setSenderTrack(sender, null);
    const d = transceiver[_state].direction;
    const next = d === "sendrecv"
      ? "recvonly"
      : d === "sendonly"
      ? "inactive"
      : d;
    if (next !== d) {
      transceiver[_state].direction = next;
      op_webrtc_transceiver_set_direction(
        this[_rid],
        transceiver[_key],
        next,
      );
    }
    updateNegotiationNeeded(this);
  }

  addTransceiver(trackOrKind, init = undefined) {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'addTransceiver' on 'RTCPeerConnection'",
    );
    let track = null;
    if (typeof trackOrKind === "string") {
      if (trackOrKind !== "audio" && trackOrKind !== "video") {
        throw new TypeError(
          `Failed to execute 'addTransceiver': '${trackOrKind}' is not a valid media kind`,
        );
      }
    } else if (
      ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, trackOrKind)
    ) {
      track = trackOrKind;
    } else {
      throw new TypeError(
        "Failed to execute 'addTransceiver': expected a MediaStreamTrack or 'audio' / 'video'",
      );
    }
    const dict = init ?? { __proto__: null };
    const direction = dict.direction === undefined
      ? "sendrecv"
      : String(dict.direction);
    if (!ArrayPrototypeIncludes(DIRECTIONS, direction)) {
      throw new TypeError(
        `Failed to execute 'addTransceiver': '${direction}' is not a valid direction`,
      );
    }
    const streams = dict.streams === undefined ? [] : ArrayFrom(dict.streams);
    for (let i = 0; i < streams.length; ++i) {
      if (!ObjectPrototypeIsPrototypeOf(MediaStreamPrototype, streams[i])) {
        throw new TypeError(
          "Failed to execute 'addTransceiver': not a MediaStream",
        );
      }
    }
    const kind = track === null ? trackOrKind : track.kind;
    const sendEncodings = validateEncodings(
      dict.sendEncodings === undefined ? [] : ArrayFrom(dict.sendEncodings),
      kind,
      "addTransceiver",
      false,
    );
    if (this[_state].closed) {
      throw invalidState("RTCPeerConnection is closed");
    }
    return addTransceiverInner(this, track ?? kind, {
      direction,
      streams,
      sendEncodings,
    });
  }

  close() {
    webidl.assertBranded(this, RTCPeerConnectionPrototype);
    const state = this[_state];
    if (state.closed) return;
    try {
      state.last = op_webrtc_pc_descriptions(this[_rid]);
    } catch {
      // The resource is already gone.
    }
    try {
      state.lastStats = engineStats(this, null, false);
    } catch {
      state.lastStats = [];
    }
    state.closed = true;
    state.active = false;
    SetPrototypeForEach(state.timers, (timer) => clearTimeout(timer));
    state.timers = new SafeSet();
    state.signalingState = "closed";
    state.iceConnectionState = "closed";
    state.connectionState = "closed";
    op_webrtc_pc_close(this[_rid]);
    if (state.config.network !== null) {
      SetPrototypeDelete(state.config.network[_state].connections, this);
    }
    // Every transceiver stops; receiver tracks end without an event, as
    // close() is the application's own doing.
    for (let i = 0; i < state.transceivers.length; ++i) {
      const t = state.transceivers[i];
      const sender = t[_state].sender;
      updateSenderSource(sender, sender[_state].track, null);
      t[_state].stopped = true;
      t[_state].stopping = false;
      t[_state].currentDirection = null;
      const receiver = t[_state].receiver;
      receiver[_state].track[_trackState].readyState = "ended";
      receiver[_state].receiving = false;
      closeReaders(receiver);
      // Encoded transforms end with the connection (03_encoded_transform.js).
      if (sender[_transform] || receiver[_transform]) {
        loadEncodedTransform().ownerClosed(sender, receiver);
      }
    }
    // The transports report closed (no events after close()).
    if (state.dtls !== null) {
      state.dtls[_transportState].state = "closed";
      state.dtls[_transportState].ice[_transportState].state = "closed";
    }
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

  static generateCertificate(keygenAlgorithm) {
    const prefix =
      "Failed to execute 'generateCertificate' on 'RTCPeerConnection'";
    let alg;
    // `expires` (ms from now) is the spec's extension of the algorithm
    // dictionary ([EnforceRange] DOMTimeStamp); 30 days by default, as
    // browsers do. A bad value rejects the promise with a TypeError.
    let expires = 30 * 24 * 3600 * 1000;
    try {
      webidl.requiredArguments(arguments.length, 1, prefix);
      alg = normalizeKeygen(keygenAlgorithm);
      if (keygenAlgorithm?.expires !== undefined) {
        expires = webidl.converters["unsigned long long"](
          keygenAlgorithm.expires,
          prefix,
          "'expires'",
          { enforceRange: true },
        );
      }
    } catch (error) {
      return PromiseReject(error);
    }
    // The expiry instant on the JS clock (see generate_certificate).
    return PromisePrototypeThen(
      op_webrtc_generate_certificate(alg, DateNow() + expires),
      (info) => new RTCCertificate(illegalConstructorKey, info),
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
defineHandler(RTCPeerConnection.prototype, "negotiationneeded");
defineHandler(RTCPeerConnection.prototype, "icecandidate");
defineHandler(RTCPeerConnection.prototype, "icecandidateerror");
defineHandler(RTCPeerConnection.prototype, "signalingstatechange");
defineHandler(RTCPeerConnection.prototype, "iceconnectionstatechange");
defineHandler(RTCPeerConnection.prototype, "icegatheringstatechange");
defineHandler(RTCPeerConnection.prototype, "connectionstatechange");
defineHandler(RTCPeerConnection.prototype, "datachannel");
defineHandler(RTCPeerConnection.prototype, "track");
webidl.configureInterface(RTCPeerConnection);
const RTCPeerConnectionPrototype = RTCPeerConnection.prototype;

// W3C "set the RTCSessionDescription": a description (local or remote, an
// offer too) that negotiates an SCTP transport, an m=application section,
// creates the connection's RTCSctpTransport; a rollback to descriptions
// without one sets it back to null.
function updateSctpTransport(pc, type) {
  const state = pc[_state];
  const has = (d) =>
    d !== null && StringPrototypeIncludes(d.sdp, "\nm=application ");
  if (type === "rollback") {
    if (
      !has(pc.currentLocalDescription) && !has(pc.currentRemoteDescription)
    ) {
      state.sctp = null;
      state.sctpState = null;
    }
    return;
  }
  if (
    state.sctp === null &&
    (has(pc.localDescription) || has(pc.remoteDescription))
  ) {
    state.sctpState = null;
    state.sctp = new RTCSctpTransport(illegalConstructorKey, pc);
    state.sctp[_transportState] = state.sctp.state;
  }
}

function maxMessageSizeOf(sdp) {
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "a=max-message-size:")) {
      const n = NumberParseInt(StringPrototypeSlice(line, 19), 10);
      if (n >= 0) return n;
    }
  }
  return null;
}

// rtcpMuxPolicy "require": a remote description whose audio or video
// sections don't all multiplex RTCP is an InvalidAccessError.
// An SDP starts with its version line (RFC 8866 section 5.1); anything else
// is not SDP at all: an RTCError with `sdp-syntax-error`.
function requireSdpSyntax(sdp) {
  if (!StringPrototypeStartsWith(sdp, "v=")) {
    throw new RTCError(
      { errorDetail: "sdp-syntax-error", sdpLineNumber: 1 },
      "The description is not SDP: it has no version line",
    );
  }
}

function requireRtcpMux(sdp) {
  const sections = StringPrototypeSplit(sdp, "\nm=");
  for (let i = 1; i < sections.length; ++i) {
    const section = sections[i];
    const header = StringPrototypeSplit(section, "\n")[0];
    const fields = StringPrototypeSplit(StringPrototypeTrim(header), " ");
    if (fields[0] !== "audio" && fields[0] !== "video") continue;
    if (fields[1] === "0") continue;
    if (!StringPrototypeIncludes(section, "a=rtcp-mux")) {
      throw new DOMException(
        "setRemoteDescription: rtcpMuxPolicy is 'require', but a media section doesn't multiplex RTCP",
        "InvalidAccessError",
      );
    }
  }
}

function midsOf(sdp) {
  const mids = [];
  let inMedia = false;
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "m=")) {
      if (inMedia) ArrayPrototypePush(mids, null);
      inMedia = true;
    } else if (inMedia && StringPrototypeStartsWith(line, "a=mid:")) {
      ArrayPrototypePush(mids, StringPrototypeSlice(line, 6));
      inMedia = false;
    }
  }
  if (inMedia) ArrayPrototypePush(mids, null);
  return mids;
}

// An EncodedMediaTrack's codec, and for audio the telephone-event formats
// RTCDTMFSender needs (media.rs `track_preferences`).
function trackPreferences(codec) {
  const out = [codec];
  if (
    StringPrototypeStartsWith(
      StringPrototypeToLowerCase(codec.mimeType),
      "audio/",
    )
  ) {
    ArrayPrototypePush(
      out,
      {
        mimeType: "audio/telephone-event",
        clockRate: 48000,
        channels: null,
        sdpFmtpLine: "0-15",
      },
      {
        mimeType: "audio/telephone-event",
        clockRate: 8000,
        channels: null,
        sdpFmtpLine: "0-15",
      },
    );
  }
  return out;
}

function addTransceiverInner(pc, trackOrKind, init) {
  const track = typeof trackOrKind === "string" ? null : trackOrKind;
  const kind = track === null ? trackOrKind : track.kind;
  const streamIds = ArrayPrototypeMap(init.streams ?? [], (s) => s.id);
  const key = op_webrtc_pc_add_transceiver(pc[_rid], {
    kind,
    track: track === null ? null : { id: track.id, label: track.label },
    direction: init.direction,
    streams: streamIds,
    sendEncodings: ArrayPrototypeMap(init.sendEncodings ?? [], (e) => ({
      rid: e.rid ?? null,
      active: e.active,
    })),
    codec: track?.[_source]?.codec ?? null,
    addTrack: init.addTrack === true,
  });
  const transceiver = new RTCRtpTransceiver(illegalConstructorKey, pc, {
    key,
    kind,
    direction: init.direction,
  });
  const sender = transceiver[_state].sender;
  sender[_state].streams = streamIds;
  sender[_state].encodings = init.sendEncodings?.length > 0
    ? init.sendEncodings
    : [
      kind === "video"
        ? { active: true, scaleResolutionDownBy: 1 }
        : { active: true },
    ];
  if (track !== null) setSenderTrack(sender, track);
  ArrayPrototypePush(pc[_state].transceivers, transceiver);
  syncTransceivers(pc, null);
  updateNegotiationNeeded(pc);
  return transceiver;
}

// Reconciles the transceivers with the engine's after an operation: new
// ones (a remote offer's), mids, current directions, ones a rollback
// removed; after a remote description, the remote tracks (track events,
// stream membership, mute) as the spec's "process the addition / removal of
// a remote track".
function syncTransceivers(pc, phase) {
  const state = pc[_state];
  if (state.closed) return;
  const infos = op_webrtc_pc_transceivers(pc[_rid]);
  const known = new SafeMap();
  for (let i = 0; i < state.transceivers.length; ++i) {
    const t = state.transceivers[i];
    MapPrototypeSet(known, t[_key], t);
  }
  const next = [];
  const settled = phase !== null &&
    (phase.type === "answer" || phase.type === "rollback");
  const trackEvents = [];
  for (let i = 0; i < infos.length; ++i) {
    const info = infos[i];
    let t = MapPrototypeGet(known, info.key);
    if (t === undefined) {
      if (info.kind !== "audio" && info.kind !== "video") continue;
      t = new RTCRtpTransceiver(illegalConstructorKey, pc, {
        key: info.key,
        kind: info.kind,
        mid: info.mid,
        direction: info.direction ?? "recvonly",
      });
      t[_state].sender[_state].encodings = info.kind === "video"
        ? [{ active: true, scaleResolutionDownBy: 1 }]
        : [{ active: true }];
    }
    const ts = t[_state];
    if (phase !== null && phase.type === "rollback" && info.mid === null) {
      ts.mid = null;
    } else if (info.mid !== null) {
      ts.mid = info.mid;
    }
    if (phase !== null) {
      ts.currentDirection = info.currentDirection ?? ts.currentDirection;
      if (info.currentDirection === null && settled && info.mid === null) {
        ts.currentDirection = null;
      }
    }
    const remote = info.remote;
    if (settled && phase.type === "answer") {
      if (ts.stopping || (remote !== null && remote.rejected)) {
        if (!ts.stopped) {
          stopSending(t);
          ts.stopping = false;
          ts.stopped = true;
        }
      }
    }
    if (
      phase !== null && phase.remote && phase.type !== "rollback" &&
      remote !== null && !ts.stopped && !ts.stopping
    ) {
      const remoteSends = !remote.rejected &&
        (remote.direction === "sendrecv" || remote.direction === "sendonly");
      const receptive = ts.direction === "sendrecv" ||
        ts.direction === "recvonly";
      const receiver = ts.receiver;
      if (remoteSends && receptive) {
        const streams = ArrayPrototypeMap(remote.streams, (id) => {
          let stream = MapPrototypeGet(state.streams, id);
          if (stream === undefined) {
            stream = new MediaStream(illegalConstructorKey, id);
            MapPrototypeSet(state.streams, id, stream);
          }
          return stream;
        });
        const rstate = receiver[_state];
        const track = rstate.track;
        const old = rstate.streams;
        for (let j = 0; j < old.length; ++j) {
          if (!ArrayPrototypeIncludes(streams, old[j])) {
            streamRemoveRemote(old[j], track);
          }
        }
        for (let j = 0; j < streams.length; ++j) {
          if (!ArrayPrototypeIncludes(old, streams[j])) {
            streamAddRemote(streams[j], track);
          }
        }
        rstate.streams = streams;
        if (!rstate.receiving) {
          rstate.receiving = true;
          ArrayPrototypePush(trackEvents, [t, streams]);
        }
      } else if (receiver[_state].receiving && !remoteSends) {
        removeRemoteTrack(t);
      }
    }
    if (!(settled && ts.stopped && phase.type === "answer")) {
      ArrayPrototypePush(next, t);
    }
  }
  state.transceivers = next;
  for (let i = 0; i < trackEvents.length; ++i) {
    const { 0: t, 1: streams } = trackEvents[i];
    const receiver = t[_state].receiver;
    fire(
      pc,
      new RTCTrackEvent("track", {
        receiver,
        track: receiver[_state].track,
        streams,
        transceiver: t,
      }),
    );
  }
}

// A promise for a later task of the event loop (not a microtask): what the
// spec's "queue a task" is here. A timer, so tasks run in the order they
// were queued, among themselves and with the program's own `setTimeout(0)`.
// close() clears the connection's timers: a task queued before it never
// runs, as the spec's tasks abort on a closed connection. With no connection
// (getStats() and setParameters(), which settle on a closed one too) the
// task always runs.
function nextTask(pc) {
  return new Promise((resolve) => {
    if (pc === null) {
      setTimeout(resolve, 0);
      return;
    }
    const timers = pc[_state].timers;
    const timer = setTimeout(() => {
      SetPrototypeDelete(timers, timer);
      resolve();
    }, 0);
    SetPrototypeAdd(timers, timer);
  });
}

// Runs `work` in a later task and resolves with its result; never settles
// once the connection is closed (the spec's operations abort then).
function inTask(pc, work) {
  const state = pc[_state];
  return new Promise((resolve, reject) => {
    PromisePrototypeThen(nextTask(pc), () => {
      if (state.closed) return;
      try {
        resolve(work());
      } catch (error) {
        reject(error);
      }
    });
  });
}

// The spec's "chain an operation": `operation` runs once every operation
// before it has settled, at once (synchronously) when there is none. It
// returns a promise, or throws; either settles the returned promise. A
// closed connection rejects with InvalidStateError, and an operation that is
// pending when the connection closes never settles.
function enqueue(pc, operation) {
  const state = pc[_state];
  if (state.closed) {
    return PromiseReject(invalidState("RTCPeerConnection is closed"));
  }
  return new Promise((resolve, reject) => {
    const settle = (settler) => (value) => {
      if (state.closed) return;
      settler(value);
      ArrayPrototypeShift(state.operations);
      if (state.operations.length > 0) {
        state.operations[0]();
      } else if (state.updateNegotiationOnEmptyChain) {
        state.updateNegotiationOnEmptyChain = false;
        updateNegotiationNeeded(pc);
      }
    };
    const run = () => {
      let result;
      try {
        result = operation();
      } catch (error) {
        result = PromiseReject(error);
      }
      PromisePrototypeThen(
        PromiseResolve(result),
        settle(resolve),
        settle(reject),
      );
    };
    ArrayPrototypePush(state.operations, run);
    if (state.operations.length === 1) run();
  });
}

// An SDP's media sections, as far as negotiation looks at them: kind, mid,
// direction, whether the section is rejected (port 0 outside a bundle) and
// its ICE credentials.
function mediaSectionsOf(sdp) {
  const lines = StringPrototypeSplit(sdp, "\n");
  const sections = [];
  const session = { ufrag: null, pwd: null, setup: null };
  let current = null;
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "m=")) {
      const parts = StringPrototypeSplit(StringPrototypeSlice(line, 2), " ");
      current = {
        kind: parts[0],
        port: NumberParseInt(parts[1], 10),
        mid: null,
        direction: "sendrecv",
        bundleOnly: false,
        ufrag: null,
        pwd: null,
        setup: null,
      };
      ArrayPrototypePush(sections, current);
      continue;
    }
    const target = current ?? session;
    if (StringPrototypeStartsWith(line, "a=ice-ufrag:")) {
      target.ufrag = StringPrototypeSlice(line, 12);
    } else if (StringPrototypeStartsWith(line, "a=ice-pwd:")) {
      target.pwd = StringPrototypeSlice(line, 10);
    } else if (StringPrototypeStartsWith(line, "a=setup:")) {
      target.setup = StringPrototypeSlice(line, 8);
    } else if (current === null) {
      continue;
    } else if (StringPrototypeStartsWith(line, "a=mid:")) {
      current.mid = StringPrototypeSlice(line, 6);
    } else if (line === "a=bundle-only") {
      current.bundleOnly = true;
    } else if (
      line === "a=sendrecv" || line === "a=sendonly" ||
      line === "a=recvonly" || line === "a=inactive"
    ) {
      current.direction = StringPrototypeSlice(line, 2);
    }
  }
  for (let i = 0; i < sections.length; ++i) {
    const section = sections[i];
    section.ufrag ??= session.ufrag;
    section.pwd ??= session.pwd;
    section.setup ??= session.setup;
    section.rejected = section.port === 0 && !section.bundleOnly;
  }
  return sections;
}

// The ICE credentials of an SDP, each as "ufrag pwd".
function iceCredentialsOf(sdp) {
  const sections = mediaSectionsOf(sdp);
  const out = [];
  for (let i = 0; i < sections.length; ++i) {
    const { ufrag, pwd } = sections[i];
    if (ufrag === null) continue;
    const credential = `${ufrag} ${pwd}`;
    if (!ArrayPrototypeIncludes(out, credential)) {
      ArrayPrototypePush(out, credential);
    }
  }
  return out;
}

// createOffer's SDP: ICE restarts when asked for, when restartIce() has
// credentials to replace, or when the ICE configuration changed.
function createOfferSdp(pc, iceRestart) {
  const state = pc[_state];
  const restart = iceRestart || state.configRestart ||
    SetPrototypeGetSize(state.iceCredentialsToReplace) > 0;
  syncDirections(pc);
  const sdp = op_webrtc_pc_create_offer(pc[_rid], restart);
  state.configRestart = false;
  return sdp;
}

// The engine narrows a transceiver's direction to what a remote offer allows
// when it applies one (that is how it works out its answer), and keeps the
// narrowed one. The application's own direction is the one here: an offer
// states it, and each remote offer is answered from it.
function syncDirections(pc) {
  const transceivers = pc[_state].transceivers;
  for (let i = 0; i < transceivers.length; ++i) {
    const ts = transceivers[i][_state];
    if (ts.stopped || ts.stopping) continue;
    try {
      op_webrtc_transceiver_set_direction(
        pc[_rid],
        transceivers[i][_key],
        ts.direction,
      );
    } catch {
      // The engine's transceiver is gone.
    }
  }
}

function reverseDirection(direction) {
  return direction === "sendonly"
    ? "recvonly"
    : direction === "recvonly"
    ? "sendonly"
    : direction;
}

function sameIds(a, b) {
  for (let i = 0; i < a.length; ++i) {
    if (!ArrayPrototypeIncludes(b, a[i])) return false;
  }
  for (let i = 0; i < b.length; ++i) {
    if (!ArrayPrototypeIncludes(a, b[i])) return false;
  }
  return true;
}

// Bookkeeping when a description has been applied: the stream ids each
// sender has in the local description (what `a=msid` carries), the type of
// the current local description, and restartIce()'s credentials, which a
// completed negotiation with fresh ones retires.
function descriptionApplied(pc, local, type) {
  const state = pc[_state];
  const transceivers = state.transceivers;
  if (type === "rollback") {
    for (let i = 0; i < transceivers.length; ++i) {
      transceivers[i][_state].pendingStreams = null;
    }
    return;
  }
  if (local) {
    for (let i = 0; i < transceivers.length; ++i) {
      const ts = transceivers[i][_state];
      ts.pendingStreams = ArrayPrototypeSlice(ts.sender[_state].streams);
    }
  }
  if (type !== "answer") return;
  for (let i = 0; i < transceivers.length; ++i) {
    const ts = transceivers[i][_state];
    if (ts.pendingStreams !== null && ts.pendingStreams !== undefined) {
      ts.negotiatedStreams = ts.pendingStreams;
      ts.pendingStreams = null;
    }
  }
  state.currentLocalType = local ? "answer" : "offer";
  if (SetPrototypeGetSize(state.iceCredentialsToReplace) > 0) {
    const current = op_webrtc_pc_descriptions(pc[_rid]).currentLocal;
    const credentials = current === null ? [] : iceCredentialsOf(current[1]);
    if (
      !ArrayPrototypeSome(
        credentials,
        (c) => SetPrototypeHas(state.iceCredentialsToReplace, c),
      )
    ) {
      state.iceCredentialsToReplace = new SafeSet();
    }
  }
}

// The spec's "check if negotiation is needed".
function negotiationIsNeeded(pc) {
  const state = pc[_state];
  if (SetPrototypeGetSize(state.iceCredentialsToReplace) > 0) return true;
  const slots = op_webrtc_pc_descriptions(pc[_rid]);
  const local = slots.currentLocal === null
    ? []
    : mediaSectionsOf(slots.currentLocal[1]);
  const remote = slots.currentRemote === null
    ? []
    : mediaSectionsOf(slots.currentRemote[1]);
  if (
    state.hadDataChannel &&
    !ArrayPrototypeSome(local, (m) => m.kind === "application")
  ) {
    return true;
  }
  const transceivers = state.transceivers;
  for (let i = 0; i < transceivers.length; ++i) {
    const ts = transceivers[i][_state];
    if (ts.stopping && !ts.stopped) return true;
    const mine = ts.mid === null
      ? undefined
      : ArrayPrototypeFind(local, (m) => m.mid === ts.mid);
    const theirs = ts.mid === null
      ? undefined
      : ArrayPrototypeFind(remote, (m) => m.mid === ts.mid);
    if (ts.stopped) {
      if (
        mine !== undefined && !mine.rejected &&
        !(theirs !== undefined && theirs.rejected)
      ) {
        return true;
      }
      continue;
    }
    if (mine === undefined) return true;
    const direction = ts.direction;
    if (direction === "sendrecv" || direction === "sendonly") {
      if (!sameIds(ts.negotiatedStreams ?? [], ts.sender[_state].streams)) {
        return true;
      }
    }
    const offered = theirs === undefined
      ? "inactive"
      : reverseDirection(theirs.direction);
    if (state.currentLocalType === "offer") {
      if (mine.direction !== direction && offered !== direction) return true;
    } else {
      // An answer: the direction intersected with what was offered.
      const send = (direction === "sendrecv" || direction === "sendonly") &&
        (offered === "sendrecv" || offered === "sendonly");
      const recv = (direction === "sendrecv" || direction === "recvonly") &&
        (offered === "sendrecv" || offered === "recvonly");
      const expected = send
        ? (recv ? "sendrecv" : "sendonly")
        : (recv ? "recvonly" : "inactive");
      if (mine.direction !== expected) return true;
    }
  }
  return false;
}

// The spec's "update the negotiation-needed flag": `negotiationneeded` fires
// in a later task, once for any number of changes, only with an empty
// operations chain and in the stable state, and only if negotiation is
// still needed then.
function updateNegotiationNeeded(pc) {
  const state = pc[_state];
  if (state.closed) return;
  if (state.operations.length > 0) {
    state.updateNegotiationOnEmptyChain = true;
    return;
  }
  if (state.signalingState !== "stable") return;
  if (!negotiationIsNeeded(pc)) {
    state.negotiationNeeded = false;
    return;
  }
  if (state.negotiationNeeded) return;
  state.negotiationNeeded = true;
  PromisePrototypeThen(nextTask(pc), () => {
    if (state.closed) return;
    if (state.operations.length > 0) {
      state.updateNegotiationOnEmptyChain = true;
      return;
    }
    if (state.signalingState !== "stable") return;
    if (!negotiationIsNeeded(pc)) {
      state.negotiationNeeded = false;
      return;
    }
    if (!state.negotiationNeeded) return;
    fire(pc, new Event("negotiationneeded"));
  });
}

// After a description is applied: back in the stable state, the
// negotiation-needed flag is cleared and worked out again.
function negotiationSettled(pc) {
  const state = pc[_state];
  if (state.closed || state.signalingState !== "stable") return;
  state.negotiationNeeded = false;
  updateNegotiationNeeded(pc);
}

// An answer negotiates the DTLS role, and with it the ids of the channels
// created before it (the getter reads them too; this keeps them for a
// channel that closes before anything asks).
function readChannelIds(pc) {
  MapPrototypeForEach(pc[_channels], (channel) => channel.id);
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
    if (events.length === 0) pump(pc);
    else dispatchEngineEvents(pc, events, 0);
  }, () => {
    state.pending = null;
  });
}

// Each engine event is its own task, as a browser queues one per event: a
// listener's microtasks (an `await` that re-arms `onmessage`, say) run before
// the next event is dispatched.
function dispatchEngineEvents(pc, events, from) {
  const state = pc[_state];
  try {
    dispatchEngineEvent(pc, events[from]);
  } catch (error) {
    // Listener exceptions are reported by dispatchEvent itself; this is
    // anything else, reported without stopping the other events.
    reportException(error);
  }
  if (state.closed) return;
  if (from + 1 === events.length) {
    pump(pc);
    return;
  }
  const next = op_void_async_deferred();
  state.pending = next;
  if (!state.active) core.unrefOpPromise(next);
  PromisePrototypeThen(next, () => {
    state.pending = null;
    if (!state.closed) dispatchEngineEvents(pc, events, from + 1);
  });
}

function channelFor(pc, event) {
  return MapPrototypeGet(pc[_channels], event.channel);
}

function transceiverFor(pc, key) {
  return ArrayPrototypeFind(pc[_state].transceivers, (t) => t[_key] === key);
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
      ArrayPrototypePush(state.localCandidates, candidate);
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
      setTransportStates(pc);
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
      setTransportStates(pc);
      fire(pc, new Event("iceconnectionstatechange"));
      break;
    case "connectionstatechange":
      if (state.connectionState === event.state) break;
      state.connectionState = event.state;
      setTransportStates(pc);
      fire(pc, new Event("connectionstatechange"));
      if (event.state === "failed" || event.state === "closed") {
        deactivate(pc);
      }
      break;
    case "negotiationneeded":
      updateNegotiationNeeded(pc);
      break;
    case "dtlsstatechange":
      state.dtlsState = event.state;
      setTransportStates(pc);
      break;
    case "sctpstatechange":
      if (state.sctp === null) break;
      state.sctpState = event.state;
      setTransportStates(pc);
      break;
    case "trackunmute": {
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      const receiver = t[_state].receiver;
      setMuted(receiver[_state].track, false);
      for (let i = 0; i < receiver[_state].clones.length; ++i) {
        setMuted(receiver[_state].clones[i], false);
      }
      break;
    }
    case "rtp": {
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      deliverRtp(t[_state].receiver, event.data);
      break;
    }
    case "keyframerequest": {
      const t = transceiverFor(pc, event.transceiver);
      const sender = t?.[_state].sender;
      // An encoded transform's worker hears it too (03_encoded_transform.js).
      if (sender?.[_transform]) {
        loadEncodedTransform().keyFrameRequested(sender);
      }
      const track = sender?.[_state].track;
      if (track === null || track === undefined) break;
      fire(track, new Event("keyframerequest"));
      break;
    }
    case "encodedframe": {
      // A received frame for the receiver's encoded transform.
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      loadEncodedTransform().deliverReceived(t[_state].receiver, event);
      break;
    }
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
    case "dcsent": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      const s = channel[_state];
      const before = s.buffered;
      s.buffered = MathMax(0, before - event.bytes);
      if (before > s.lowThreshold && s.buffered <= s.lowThreshold) {
        fire(channel, new Event("bufferedamountlow"));
      }
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

// --- Deno.webrtc: the server-side extension ---------------------------------
//
// What a server needs and the standard doesn't have. Media: there is no
// capture device or codec here, so a local track is an EncodedMediaTrack the
// program feeds already-encoded frames or finished RTP packets, and a remote
// track's packets are read with readRtp(). Networking: a Network
// (createNetwork) fixes ports, shares one UDP port between connections,
// advertises 1:1 NAT addresses, does ICE over TCP (tcpPort), publishes host
// candidates under mDNS names (mdns) and runs ICE-lite; a connection uses one with
// the non-standard `RTCConfiguration.denoNetwork` member. Certificates:
// exportCertificate / importCertificate keep a DTLS fingerprint across
// restarts.

const MIME_KINDS = { __proto__: null, audio: "audio/", video: "video/" };

// Default clock rates and channels of the codecs browsers negotiate.
function defaultCodec(kind, init) {
  const mimeType = String(init.mimeType ?? "");
  const lower = StringPrototypeToLowerCase(mimeType);
  if (!StringPrototypeStartsWith(lower, MIME_KINDS[kind])) {
    throw new TypeError(
      `EncodedMediaTrack: '${mimeType}' is not a ${kind} codec`,
    );
  }
  const known = {
    __proto__: null,
    "audio/opus": [48000, 2, "minptime=10;useinbandfec=1"],
    "audio/g722": [8000, 0, null],
    "audio/pcmu": [8000, 0, null],
    "audio/pcma": [8000, 0, null],
    "video/vp8": [90000, 0, null],
    "video/vp9": [90000, 0, "profile-id=0"],
    "video/h264": [
      90000,
      0,
      "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
    ],
    "video/h265": [90000, 0, null],
    "video/av1": [90000, 0, null],
  };
  const d = known[lower] ?? [kind === "audio" ? 48000 : 90000, 0, null];
  return {
    mimeType,
    clockRate: init.clockRate === undefined
      ? d[0]
      : webidl.converters["unsigned long"](init.clockRate),
    channels: init.channels === undefined
      ? (d[1] === 0 ? null : d[1])
      : webidl.converters["unsigned short"](init.channels),
    sdpFmtpLine: init.sdpFmtpLine === undefined
      ? d[2]
      : String(init.sdpFmtpLine),
  };
}

function bytesOf(data, method) {
  if (ArrayBufferIsView(data)) {
    return isDataView(data)
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
  }
  if (core.isAnyArrayBuffer(data)) return new Uint8Array(data);
  throw new TypeError(`${method}: expected a BufferSource`);
}

// A sender currently fed by `source`: its track is one of the source's,
// live and enabled, its connection open and its transceiver sending.
function eachFedSender(source, fn) {
  let n = 0;
  SetPrototypeForEach(source.senders, (sender) => {
    const track = sender[_state].track;
    const pc = sender[_pc];
    const t = sender[_transceiver][_state];
    if (
      track === null || track[_trackState].readyState !== "live" ||
      !track[_trackState].enabled || pc[_state].closed || t.stopped ||
      t.stopping || (t.direction !== "sendrecv" && t.direction !== "sendonly")
    ) {
      return;
    }
    if (fn(pc, sender[_transceiver][_key], sender)) n++;
  });
  return n;
}

class EncodedMediaTrack extends MediaStreamTrack {
  constructor(init = undefined, cloneInit = undefined) {
    if (init === illegalConstructorKey) {
      super(illegalConstructorKey, cloneInit);
      return;
    }
    if (init === null || typeof init !== "object") {
      throw new TypeError(
        "Failed to construct 'EncodedMediaTrack': an init dictionary is required",
      );
    }
    const kind = String(init.kind);
    if (kind !== "audio" && kind !== "video") {
      throw new TypeError(
        `Failed to construct 'EncodedMediaTrack': '${kind}' is not a valid media kind`,
      );
    }
    const source = {
      tracks: new SafeSet(),
      senders: new SafeSet(),
      codec: defaultCodec(kind, init),
      ctor: EncodedMediaTrack,
    };
    super(illegalConstructorKey, {
      kind,
      id: init.id === undefined ? undefined : String(init.id),
      label: init.label === undefined ? "" : String(init.label),
      source,
    });
  }

  get codec() {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    return { ...this[_source].codec };
  }

  /** Sends one encoded frame on every connection this track is sent on,
   * cut into RTP packets for the negotiated codec. Returns how many senders
   * took it (0 before negotiation, or when stopped / disabled). */
  writeFrame(data, options = undefined) {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const bytes = bytesOf(data, "writeFrame");
    const duration = options?.duration === undefined
      ? (this[_trackState].kind === "audio" ? 20 : 1000 / 30)
      : webidl.converters["double"](options.duration);
    if (!(duration >= 0)) {
      throw new RangeError("writeFrame: duration must be 0 or more");
    }
    if (this[_trackState].readyState !== "live") return 0;
    return eachFedSender(
      this[_source],
      (pc, key) => op_webrtc_sender_write_frame(pc[_rid], key, bytes, duration),
    );
  }

  /** Sends one finished RTP packet (header included) on every connection
   * this track is sent on; its SSRC becomes each sender's, and an unknown
   * payload type the negotiated one. */
  writeRtp(packet) {
    webidl.assertBranded(this, MediaStreamTrackPrototype);
    const bytes = bytesOf(packet, "writeRtp");
    if (this[_trackState].readyState !== "live") return 0;
    return eachFedSender(
      this[_source],
      (pc, key) => op_webrtc_sender_write_rtp(pc[_rid], key, bytes),
    );
  }
}
defineHandler(EncodedMediaTrack.prototype, "keyframerequest");

// readRtp(receiver): the receiver's RTP packets as a stream. Real-time
// media doesn't wait: a packet that arrives while the stream's queue is
// full (`highWaterMark` packets, 256 by default) is dropped.
const _readers = Symbol("[[readers]]");

function readRtp(receiver, options = undefined) {
  if (!ObjectPrototypeIsPrototypeOf(RTCRtpReceiverPrototype, receiver)) {
    throw new TypeError("readRtp: expected an RTCRtpReceiver");
  }
  const highWaterMark = options?.highWaterMark === undefined
    ? 256
    : webidl.converters["unsigned long"](options.highWaterMark);
  const pc = receiver[_pc];
  receiver[_readers] ??= new SafeSet();
  const readers = receiver[_readers];
  let controller;
  const sync = () => {
    if (pc[_state].closed) return;
    op_webrtc_receiver_read_rtp(
      pc[_rid],
      receiver[_transceiver][_key],
      readers.size > 0,
    );
  };
  return new streams.ReadableStream({
    start(c) {
      controller = c;
      if (pc[_state].closed) {
        c.close();
        return;
      }
      SetPrototypeAdd(readers, c);
      sync();
    },
    cancel() {
      SetPrototypeDelete(readers, controller);
      sync();
    },
  }, { highWaterMark, size: () => 1 });
}

function deliverRtp(receiver, data) {
  const readers = receiver[_readers];
  if (readers === undefined) return;
  let first = true;
  SetPrototypeForEach(readers, (c) => {
    if (c.desiredSize <= 0) return;
    // Every reader gets its own copy but the first.
    c.enqueue(first ? data : TypedArrayPrototypeSlice(data));
    first = false;
  });
}

function closeReaders(receiver) {
  const readers = receiver[_readers];
  if (readers === undefined) return;
  SetPrototypeForEach(readers, (c) => {
    try {
      c.close();
    } catch {
      // Already closed.
    }
  });
  receiver[_readers] = undefined;
}

function requestKeyFrame(receiver) {
  if (!ObjectPrototypeIsPrototypeOf(RTCRtpReceiverPrototype, receiver)) {
    throw new TypeError("requestKeyFrame: expected an RTCRtpReceiver");
  }
  const pc = receiver[_pc];
  if (pc[_state].closed) return false;
  return op_webrtc_receiver_request_key_frame(
    pc[_rid],
    receiver[_transceiver][_key],
  );
}

function exportCertificate(certificate) {
  if (!ObjectPrototypeIsPrototypeOf(RTCCertificatePrototype, certificate)) {
    throw new TypeError("exportCertificate: expected an RTCCertificate");
  }
  return op_webrtc_certificate_to_pem(certificate[_certRid]);
}

function importCertificate(pem) {
  const info = op_webrtc_certificate_from_pem(String(pem));
  return new RTCCertificate(illegalConstructorKey, info);
}

// --- Deno.webrtc.Network -----------------------------------------------------

function convertNetworkOptions(options) {
  const o = options ?? { __proto__: null };
  const prefix = "Deno.webrtc.createNetwork";
  const out = {
    port: null,
    portRange: null,
    bindAddresses: null,
    interfaces: null,
    includeLoopback: true,
    publicAddresses: [],
    publicAddressCandidate: "host",
    iceLite: false,
    iceTimeouts: null,
    maxMessageSize: null,
    tcpPort: null,
    mdns: null,
  };
  if (o.port !== undefined) {
    out.port = webidl.converters["unsigned short"](o.port, prefix, "port", {
      enforceRange: true,
    });
  }
  if (o.portRange !== undefined) {
    const min = webidl.converters["unsigned short"](
      o.portRange?.min,
      prefix,
      "portRange.min",
      { enforceRange: true },
    );
    const max = webidl.converters["unsigned short"](
      o.portRange?.max,
      prefix,
      "portRange.max",
      { enforceRange: true },
    );
    if (min === 0 || min > max) {
      throw new RangeError(`${prefix}: portRange needs 0 < min <= max`);
    }
    out.portRange = { min, max };
  }
  if (out.port !== null && out.portRange !== null) {
    throw new TypeError(`${prefix}: port and portRange are exclusive`);
  }
  const strings = (v, name) => {
    if (v === undefined) return null;
    if (v === null || typeof v[SymbolIterator] !== "function") {
      throw new TypeError(
        `${prefix}: '${name}' must be a sequence of strings`,
      );
    }
    return ArrayPrototypeMap(ArrayFrom(v), String);
  };
  out.bindAddresses = strings(o.bindAddresses, "bindAddresses");
  out.interfaces = strings(o.interfaces, "interfaces");
  out.publicAddresses = strings(o.publicAddresses, "publicAddresses") ?? [];
  if (o.includeLoopback !== undefined) {
    out.includeLoopback = !!o.includeLoopback;
  }
  if (o.publicAddressCandidate !== undefined) {
    const t = String(o.publicAddressCandidate);
    if (t !== "host" && t !== "srflx") {
      throw new TypeError(
        `${prefix}: publicAddressCandidate is 'host' or 'srflx'`,
      );
    }
    out.publicAddressCandidate = t;
  }
  out.iceLite = !!o.iceLite;
  if (o.iceTimeouts !== undefined) {
    const t = o.iceTimeouts ?? {};
    const ms = (v) =>
      v === undefined ? null : webidl.converters["unsigned long"](v);
    out.iceTimeouts = {
      disconnected: ms(t.disconnected),
      failed: ms(t.failed),
      keepAlive: ms(t.keepAlive),
    };
  }
  if (o.maxMessageSize !== undefined) {
    out.maxMessageSize = webidl.converters["unsigned long"](o.maxMessageSize);
  }
  if (o.tcpPort !== undefined) {
    out.tcpPort = webidl.converters["unsigned short"](
      o.tcpPort,
      prefix,
      "tcpPort",
      { enforceRange: true },
    );
  }
  if (o.mdns !== undefined) {
    out.mdns = !!o.mdns;
  }
  return out;
}

class RtcNetwork {
  constructor(key = undefined, rid = undefined, options = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_rid] = rid;
    this[_state] = { options, closed: false, connections: new SafeSet() };
    this[webidl.brand] = webidl.brand;
  }

  /** The shared UDP port's local addresses (`port` networks), else []. */
  get localAddresses() {
    webidl.assertBranded(this, RtcNetworkPrototype);
    if (this[_state].closed) return [];
    return op_webrtc_network_addresses(this[_rid]);
  }

  /** The TCP listeners' local addresses (`tcpPort` networks), else []. */
  get tcpAddresses() {
    webidl.assertBranded(this, RtcNetworkPrototype);
    if (this[_state].closed) return [];
    return op_webrtc_network_tcp_addresses(this[_rid]);
  }

  /** Closes the network's shared sockets and every connection using it. */
  close() {
    webidl.assertBranded(this, RtcNetworkPrototype);
    const state = this[_state];
    if (state.closed) return;
    state.closed = true;
    SetPrototypeForEach(state.connections, (pc) => pc.close());
    op_webrtc_network_close(this[_rid]);
  }

  [SymbolDispose]() {
    this.close();
  }
}
webidl.configureInterface(RtcNetwork);
const RtcNetworkPrototype = RtcNetwork.prototype;

function createNetwork(options = undefined) {
  const converted = convertNetworkOptions(options);
  const rid = op_webrtc_network_create(converted);
  return new RtcNetwork(illegalConstructorKey, rid, converted);
}

// How much the WebRTC engine logs to stderr: "off" (the default; its
// outcomes are RTCPeerConnection events), "error", "warn", "info", "debug"
// or "trace". Process-wide.
function setLogLevel(level) {
  op_webrtc_set_log_level(
    webidl.converters.DOMString(
      level,
      "Deno.webrtc.setLogLevel",
      "Argument 1",
    ),
  );
}

const webrtcNs = ObjectFreeze({
  __proto__: null,
  EncodedMediaTrack,
  Network: RtcNetwork,
  createNetwork,
  readRtp,
  requestKeyFrame,
  exportCertificate,
  importCertificate,
  setLogLevel,
});

return {
  MediaStream,
  MediaStreamTrack,
  MediaStreamTrackEvent,
  RTCCertificate,
  RTCDataChannel,
  RTCDataChannelEvent,
  RTCDTMFSender,
  RTCDTMFToneChangeEvent,
  RTCDtlsTransport,
  RTCError,
  RTCErrorEvent,
  RTCIceCandidate,
  RTCIceCandidatePair,
  RTCIceTransport,
  RTCPeerConnection,
  RTCPeerConnectionIceErrorEvent,
  RTCPeerConnectionIceEvent,
  RTCRtpReceiver,
  RTCRtpSender,
  RTCRtpTransceiver,
  RTCSctpTransport,
  RTCSessionDescription,
  RTCStatsReport,
  RTCTrackEvent,
  webrtcNs,
  // For 02_media_devices.js: the track internals a capture device needs.
  mediaInternals: {
    illegalConstructorKey,
    _key,
    _pc,
    _rid,
    _source,
    _state,
    _trackState,
    _transceiver,
    _transform,
    eachFedSender,
  },
};
})();
