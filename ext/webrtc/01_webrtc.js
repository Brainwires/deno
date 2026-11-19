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
  op_webrtc_pc_ice_selected_pair,
  op_webrtc_ice_add_remote_candidate,
  op_webrtc_ice_create,
  op_webrtc_ice_gather,
  op_webrtc_ice_local_parameters,
  op_webrtc_ice_next_events,
  op_webrtc_ice_selected_pair,
  op_webrtc_ice_start,
  op_webrtc_ice_stop,
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
  // wpt100:hdrext
  op_webrtc_pc_set_negotiation_options,
  op_webrtc_transceiver_set_header_extensions,
  op_void_async_deferred,
  // wpt100:mediasink
  op_webrtc_receiver_muted,
  op_webrtc_receiver_read_frames,
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
  StringPrototypeIndexOf,
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
  let pair = state.closed
    ? state.last?.[slot]
    : op_webrtc_pc_descriptions(pc[_rid])[slot];
  if (pair === null || pair === undefined) return null;
  if (slot === "currentLocal" || slot === "pendingLocal") {
    pair = [pair[0], announcedSdp(state, pair[1])];
  }
  // wpt100:identity — a local description shows the a=identity line the
  // engine was given without (06_identity.js).
  const sdp = identityHooks.reportedSdp(pc, slot, pair[1]);
  const cache = state.descriptions;
  const held = cache[slot];
  if (held !== null && held.type === pair[0] && held.sdp === sdp) {
    return held;
  }
  const description = descriptionFrom([pair[0], sdp]);
  cache[slot] = description;
  return description;
}

// The key a gathered candidate is announced under: the local ICE ufrag it was
// gathered for and the candidate's first eight fields (foundation to type),
// which the SDP line and the event's string share whatever extensions follow.
function announcedKey(ufrag, candidate) {
  const fields = StringPrototypeSplit(StringPrototypeTrim(candidate), SPACES);
  return `${ufrag ?? ""} ${
    ArrayPrototypeJoin(ArrayPrototypeSlice(fields, 0, 8), " ")
  }`;
}

// A local description as the application has been told of it. The engine
// writes each candidate into its local description as it gathers it, but W3C
// webrtc-pc 5.6 ("When an ICE candidate is gathered") adds the candidate to
// [[CurrentLocalDescription]] / [[PendingLocalDescription]] in the same task
// that fires its `icecandidate` event: before that event the description
// lacks it, so a description read right after setLocalDescription() has no
// candidate line, and each announced candidate makes a new description object
// (descriptionSlot compares the text). `a=end-of-candidates` likewise waits
// for the `complete` gathering state.
function announcedSdp(state, sdp) {
  if (
    !StringPrototypeIncludes(sdp, "a=candidate:") &&
    !StringPrototypeIncludes(sdp, "a=end-of-candidates")
  ) {
    return sdp;
  }
  const lines = StringPrototypeSplit(sdp, "\n");
  const out = [];
  let ufrag = "";
  for (let i = 0; i < lines.length; ++i) {
    const line = lines[i];
    const trimmed = StringPrototypeTrim(line);
    if (StringPrototypeStartsWith(trimmed, "a=ice-ufrag:")) {
      ufrag = StringPrototypeSlice(trimmed, 12);
    } else if (StringPrototypeStartsWith(trimmed, "a=candidate:")) {
      const candidate = StringPrototypeSlice(trimmed, 2);
      if (
        !SetPrototypeHas(state.announced, announcedKey(ufrag, candidate)) &&
        !SetPrototypeHas(state.announced, announcedKey(null, candidate))
      ) {
        continue;
      }
    } else if (
      trimmed === "a=end-of-candidates" &&
      state.iceGatheringState !== "complete"
    ) {
      continue;
    }
    ArrayPrototypePush(out, line);
  }
  return ArrayPrototypeJoin(out, "\n");
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

// Makes an RTCIceCandidate whose derived attributes are given rather than
// parsed (see RTCIceCandidate's static block).
let candidateWithFields;

class RTCIceCandidate {
  #init;
  #parsed;
  #relayProtocol;
  #url;

  static {
    // A peer-reflexive remote candidate the application never signalled is
    // exposed with an empty candidate string and no address (W3C webrtc-pc
    // 4.8.1, "the address is not exposed"), but with the fields the agent
    // learned: those can't come from parsing a string.
    candidateWithFields = (init, parsed) => {
      const candidate = new RTCIceCandidate(init);
      candidate.#parsed = parsed;
      return candidate;
    };
  }

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
  // wpt100:identity — RTCErrorDetailTypeIdp (W3C WebRTC Identity section
  // 10), the partial enum WebIDL can't write.
  "idp-bad-script-failure",
  "idp-execution-failure",
  "idp-load-failure",
  "idp-need-login",
  "idp-timeout",
  "idp-tls-failure",
  "idp-token-expired",
  "idp-token-invalid",
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
      // wpt100:identity — RTCErrorInit.httpRequestStatusCode (`long`,
      // W3C WebRTC Identity section 10).
      httpRequestStatusCode: init.httpRequestStatusCode === undefined
        ? null
        : webidl.converters.long(
          init.httpRequestStatusCode,
          "Failed to construct 'RTCError'",
          "httpRequestStatusCode",
        ),
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
  // wpt100:identity
  get httpRequestStatusCode() {
    return this.#init.httpRequestStatusCode;
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
// The remote max-message-size of the last answer applied (null before one).
const _maxMessageSize = Symbol("[[MaxMessageSize]]");

class RTCSctpTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[_maxMessageSize] = null;
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

  // [[MaxMessageSize]] (W3C "update the data max message size"): set when
  // an answer or pranswer is applied, from the remote description's
  // a=max-message-size (65536 when absent, no limit when 0), capped by what
  // this side can send. Until then the remote limit is RFC 8841's default.
  get maxMessageSize() {
    webidl.assertBranded(this, RTCSctpTransportPrototype);
    const remote = this[_maxMessageSize] ?? 65536;
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

// webrtc-priority 4: RTCPriorityType, and the DCEP priority each is sent as
// (RFC 8831 6.4 / RFC 8832 5.1).
const PRIORITY_VALUES = {
  __proto__: null,
  "very-low": 128,
  low: 256,
  medium: 512,
  high: 1024,
};

// webrtc-priority 4.3: an incoming channel's priority from the DCEP value.
function priorityFromDcep(value) {
  if (typeof value !== "number") return "low";
  if (value <= 128) return "very-low";
  if (value <= 256) return "low";
  if (value <= 512) return "medium";
  return "high";
}

// Transferable RTCDataChannel (W3C WebRTC 6.2): the transfer and the channel's
// life in the receiving realm are 05_transfer.js. On this side a transferred
// channel's engine events go to `_transferPort`; a received channel (its
// [[pc]] in another realm) sends through `_proxyPort`.
const _transferPort = Symbol("[[transferPort]]");
const _proxyPort = Symbol("[[proxyPort]]");
let transferMod;
const loadTransfer = () =>
  transferMod ??
    (transferMod = core.loadExtScript("ext:deno_webrtc/05_transfer.js"));

class RTCDataChannel extends EventTarget {
  constructor(key = undefined, pc = undefined, init = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[_pc] = pc;
    this[_channel] = init.channel;
    this[_init] = init;
    const state = {
      readyState: init.readyState ?? "connecting",
      id: init.id ?? null,
      binaryType: "arraybuffer",
      // [[IsTransferable]]: true until a task queued at creation runs, send()
      // is called or the channel is transferred.
      transferable: pc !== null,
      // A received channel's maxMessageSize (its connection is elsewhere).
      maxMessageSize: init.maxMessageSize ?? null,
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
    this[_state] = state;
    this[webidl.brand] = webidl.brand;
    // W3C "create an RTCDataChannel": [[IsTransferable]] turns false in a
    // task queued now, ahead of any task that delivers the channel's
    // messages, so a channel is transferred in the task that made it (the
    // `datachannel` event's, or the createDataChannel() caller's). A system
    // timer: ordered with the program's own `setTimeout(0)`, it keeps
    // nothing alive and is no test-sanitizer resource.
    if (state.transferable) {
      core.createSystemTimer(
        () => {
          state.transferable = false;
        },
        0,
        false,
      );
    }
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
  // webrtc-priority 4.1: [[DataChannelPriority]].
  get priority() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    return this[_init].priority ?? "low";
  }
  get id() {
    webidl.assertBranded(this, RTCDataChannelPrototype);
    const state = this[_state];
    // The engine assigns an in-band channel's id once the DTLS role is
    // negotiated (an answer is applied); it doesn't change after that. A
    // received channel learns it from the realm that has the connection.
    if (state.id === null && state.readyState !== "closed") {
      const pc = this[_pc];
      if (pc !== null && !pc[_state].closed) {
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
    if (pc === null) {
      loadTransfer().proxySetThreshold(this, threshold);
    } else if (!pc[_state].closed) {
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
    // W3C send() step 2: a channel that sent can no longer be transferred.
    state.transferable = false;
    if (state.readyState !== "open") {
      throw invalidState(
        `RTCDataChannel.readyState is '${state.readyState}', not 'open'`,
      );
    }
    if (ObjectPrototypeIsPrototypeOf(BlobPrototype, data)) {
      // The byte size of a Blob is known synchronously (send() step 4).
      checkMessageSize(this, data.size);
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
    checkMessageSize(this, TypedArrayPrototypeGetByteLength(bytes));
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
// [Transferable] (W3C WebRTC 6.2): postMessage() takes a channel in its
// transfer list; the steps are registered for every realm by
// 98_global_scope_shared.js.
ObjectDefineProperty(RTCDataChannelPrototype, core.hostObjectBrand, {
  __proto__: null,
  value: "RTCDataChannel",
  enumerable: false,
  configurable: false,
  writable: false,
});

// W3C send() step 5: a message larger than the SCTP transport's
// maxMessageSize is a TypeError, thrown by send() itself.
function checkMessageSize(channel, size) {
  let limit;
  const pc = channel[_pc];
  if (pc === null) {
    limit = channel[_state].maxMessageSize;
  } else {
    const sctp = pc[_state].sctp;
    limit = sctp === null || sctp === undefined ? null : sctp.maxMessageSize;
  }
  if (limit !== null && size > limit) {
    throw new TypeError(
      `Failed to execute 'send' on 'RTCDataChannel': the message (${size} bytes) is larger than the connection's maxMessageSize (${limit})`,
    );
  }
}

function closeTransport(channel) {
  const pc = channel[_pc];
  if (pc === null) {
    loadTransfer().proxyClose(channel);
  } else if (!pc[_state].closed) {
    op_webrtc_dc_close(pc[_rid], channel[_channel]);
  }
}

function sendNow(channel, text, bytes) {
  const pc = channel[_pc];
  if (pc === null) {
    loadTransfer().proxySend(channel, text, bytes);
  } else {
    op_webrtc_dc_send(pc[_rid], channel[_channel], text, bytes);
  }
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

// W3C "announce the RTCDataChannel as open" for a channel the remote peer
// opened: a task after the `datachannel` event's. A channel transferred in
// that event hears it in its new realm.
function announceOpen(pc, channel, event) {
  const next = op_void_async_deferred();
  if (!pc[_state].active) core.unrefOpPromise(next);
  PromisePrototypeThen(next, () => {
    if (pc[_state].closed) return;
    if (channel[_transferPort] !== undefined) {
      loadTransfer().forwardEvent(channel, event);
      return;
    }
    const state = channel[_state];
    if (state.readyState !== "open") return;
    fire(channel, new Event("open"));
  });
}

// SCTP took `bytes` of the channel's data from its queue: [[BufferedAmount]]
// goes down, and crossing the threshold fires `bufferedamountlow`.
function channelSent(channel, bytes) {
  const s = channel[_state];
  const before = s.buffered;
  s.buffered = MathMax(0, before - bytes);
  if (before > s.lowThreshold && s.buffered <= s.lowThreshold) {
    fire(channel, new Event("bufferedamountlow"));
  }
}

// W3C "receiving messages on a data channel" (what arrives while the channel
// is closing still reaches it, as before).
function deliverChannelMessage(channel, text, bytes) {
  if (channel[_state].readyState === "closed") return;
  const data = text ? core.decode(bytes) : toBinary(channel, bytes);
  fire(channel, new MessageEvent("message", { data }));
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
    closeFrameReaders(this);
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
    if (device) return device.settings(this);
    // A remote video track: the coded size of the frames it receives, as
    // their bitstream names it (0 until one does).
    const receiver = this[_receiver];
    if (receiver !== null && this[_trackState].kind === "video") {
      const size = receiver[_frameSize];
      return {
        deviceId: "",
        groupId: "",
        width: size?.width ?? 0,
        height: size?.height ?? 0,
      };
    }
    return { deviceId: "", groupId: "" };
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
  // A remote track: the next packet that arrives unmutes it again.
  if (muted) rearmUnmute(track[_receiver]);
  fire(track, new Event(muted ? "mute" : "unmute"));
}

// The receiver's next RTP packet sends a `trackunmute` event again.
function rearmUnmute(receiver) {
  if (receiver === null || receiver === undefined) return;
  const pc = receiver[_pc];
  if (pc[_state].closed) return;
  try {
    op_webrtc_receiver_muted(pc[_rid], receiver[_transceiver][_key]);
  } catch {
    // The transceiver is gone.
  }
}

// A track the user agent ends (anything but its own stop()): mediacapture-
// main 4.3.1.2 queues a task that sets its readyState and fires `ended`,
// and does nothing if the track ended meanwhile (stop(), or close()).
// webrtc-pc's "stop sending and receiving" ends the receiver's track so.
function endTrack(track) {
  if (track[_trackState].readyState === "ended") return;
  setTimeout(() => {
    const state = track[_trackState];
    if (state.readyState === "ended") return;
    state.readyState = "ended";
    closeFrameReaders(track);
    fire(track, new Event("ended"));
  }, 0);
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

// [Serializable] (W3C WebRTC 4.9.2): the serialized certificate carries its
// key, fingerprints and expiry; it deserializes only where RTCCertificate is
// exposed ([Exposed=Window]), so a worker gets `messageerror`.
ObjectDefineProperty(RTCCertificatePrototype, core.hostObjectBrand, {
  __proto__: null,
  value: function () {
    return {
      type: "RTCCertificate",
      pem: op_webrtc_certificate_to_pem(this[_certRid]),
      expires: this.expires,
    };
  },
  enumerable: false,
  configurable: false,
  writable: false,
});

// Registered in every realm by 98_global_scope_shared.js.
function deserializeCertificate(serialized) {
  const { WorkerGlobalScope } = core.loadExtScript(
    "ext:deno_web/04_global_interfaces.js",
  );
  if (ObjectPrototypeIsPrototypeOf(WorkerGlobalScope.prototype, globalThis)) {
    throw new DOMException(
      "RTCCertificate is not exposed in a worker",
      "DataCloneError",
    );
  }
  const info = op_webrtc_certificate_from_pem(String(serialized.pem));
  return new RTCCertificate(illegalConstructorKey, {
    ...info,
    expires: serialized.expires,
  });
}

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
// wpt100:ice: the state of a standalone transport (`new RTCIceTransport()`,
// W3C webrtc-ice); undefined on a peer connection's transport.
const _ice = Symbol("[[iceTransport]]");

class RTCIceTransport extends EventTarget {
  constructor(key = undefined, pc = undefined) {
    super();
    this[webidl.brand] = webidl.brand;
    if (key !== illegalConstructorKey) {
      // W3C webrtc-ice: `new RTCIceTransport()` is a transport of its own,
      // driven by gather() / start() / addRemoteCandidate() (ice_transport.rs).
      const rid = op_webrtc_ice_create();
      const params = op_webrtc_ice_local_parameters(rid);
      this[_ice] = {
        rid,
        role: null,
        state: "new",
        gatheringState: "new",
        local: [],
        remote: [],
        // Generated with the transport (RFC 8445 section 5.3), kept after
        // stop().
        localParameters: { usernameFragment: params[0], password: params[1] },
        remoteParameters: null,
        closed: false,
        active: false,
        pending: null,
      };
      // One never stopped is stopped when it is collected.
      iceTransportRegistry.register(this, rid);
      return;
    }
    this[_pc] = pc;
    this[_transportState] = { state: "new", gatheringState: "new" };
  }

  get role() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    if (this[_ice] !== undefined) return this[_ice].role;
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
    if (this[_ice] !== undefined) return this[_ice].state;
    return this[_transportState].state;
  }
  get gatheringState() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    if (this[_ice] !== undefined) return this[_ice].gatheringState;
    return this[_transportState].gatheringState;
  }
  // The candidates gathered here and signalled with `icecandidate`, and the
  // remote ones received with addIceCandidate().
  getLocalCandidates() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    if (this[_ice] !== undefined) return ArrayPrototypeSlice(this[_ice].local);
    return ArrayPrototypeSlice(this[_pc][_state].localCandidates);
  }
  getRemoteCandidates() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    if (this[_ice] !== undefined) {
      return ArrayPrototypeSlice(this[_ice].remote);
    }
    return ArrayPrototypeSlice(this[_pc][_state].remoteCandidates);
  }
  getSelectedCandidatePair() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    if (this[_ice] !== undefined) return iceSelectedPair(this[_ice]);
    return selectedPair(this[_pc]);
  }
  getLocalParameters() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = this[_ice];
    if (ice !== undefined) return { ...ice.localParameters };
    return iceParameters(this[_pc].localDescription);
  }
  getRemoteParameters() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = this[_ice];
    if (ice !== undefined) {
      return ice.remoteParameters === null ? null : { ...ice.remoteParameters };
    }
    return iceParameters(this[_pc].remoteDescription);
  }

  // --- W3C webrtc-ice: the standalone transport's methods ---

  gather(options = undefined) {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = requireStandalone(this, "gather");
    const opts = options ?? { __proto__: null };
    if (typeof opts !== "object") {
      throw new TypeError(
        "Failed to execute 'gather' on 'RTCIceTransport': the options must be an object",
      );
    }
    const gatherPolicy = opts.gatherPolicy === undefined
      ? "all"
      : String(opts.gatherPolicy);
    if (gatherPolicy !== "all" && gatherPolicy !== "relay") {
      throw new TypeError(
        `Failed to execute 'gather' on 'RTCIceTransport': '${gatherPolicy}' is not a valid value for 'gatherPolicy'`,
      );
    }
    // `sequence<RTCIceServer>`: null is not a sequence (a TypeError).
    const servers = convertIceServers(opts.iceServers);
    if (ice.closed) {
      throw invalidState("The RTCIceTransport is closed");
    }
    if (ice.gatheringState !== "new") {
      throw invalidState("gather() was already called");
    }
    op_webrtc_ice_gather(ice.rid, servers, gatherPolicy === "relay");
    // The candidates and the `complete` state follow as events; the change
    // to "gathering" itself is not one.
    ice.gatheringState = "gathering";
    iceActivate(ice);
    icePump(this);
  }

  start(remoteParameters = undefined, role = "controlled") {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = requireStandalone(this, "start");
    const prefix = "Failed to execute 'start' on 'RTCIceTransport'";
    const params = remoteParameters ?? { __proto__: null };
    if (typeof params !== "object") {
      throw new TypeError(`${prefix}: remoteParameters must be an object`);
    }
    role = String(role);
    if (role !== "controlling" && role !== "controlled") {
      throw new TypeError(
        `${prefix}: '${role}' is not a valid value for 'role'`,
      );
    }
    if (ice.closed) {
      throw invalidState("The RTCIceTransport is closed");
    }
    if (
      params.usernameFragment === undefined || params.password === undefined
    ) {
      throw new TypeError(
        `${prefix}: remoteParameters needs a usernameFragment and a password`,
      );
    }
    const usernameFragment = String(params.usernameFragment);
    const password = String(params.password);
    // RFC 8839 section 5.4: ice-ufrag is 4 to 256 ice-chars, ice-pwd 22 to
    // 256.
    if (!RegExpPrototypeTest(ICE_UFRAG, usernameFragment)) {
      throw new DOMException(
        `${prefix}: '${usernameFragment}' is not a valid usernameFragment`,
        "SyntaxError",
      );
    }
    if (!RegExpPrototypeTest(ICE_PWD, password)) {
      throw new DOMException(
        `${prefix}: the password is not valid`,
        "SyntaxError",
      );
    }
    if (ice.role !== null && ice.role !== role) {
      throw invalidState(
        `${prefix}: the role is '${ice.role}' and cannot change`,
      );
    }
    const previous = ice.remoteParameters;
    if (
      previous !== null && (previous.usernameFragment !== usernameFragment ||
        previous.password !== password)
    ) {
      // The remote side restarted: its candidates are flushed, and the
      // transport is "new" until one of the new generation arrives.
      ice.remote = [];
      if (ice.state === "checking") ice.state = "new";
    }
    op_webrtc_ice_start(
      ice.rid,
      usernameFragment,
      password,
      role === "controlling",
    );
    ice.role = role;
    ice.remoteParameters = { usernameFragment, password };
    if (ice.remote.length > 0 && ice.state === "new") ice.state = "checking";
    iceActivate(ice);
    icePump(this);
  }

  stop() {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = requireStandalone(this, "stop");
    if (ice.closed) return;
    ice.closed = true;
    ice.state = "closed";
    if (ice.active && ice.pending) core.unrefOpPromise(ice.pending);
    ice.active = false;
    op_webrtc_ice_stop(ice.rid);
  }

  addRemoteCandidate(remoteCandidate = undefined) {
    webidl.assertBranded(this, RTCIceTransportPrototype);
    const ice = requireStandalone(this, "addRemoteCandidate");
    const candidate = ObjectPrototypeIsPrototypeOf(
        RTCIceCandidatePrototype,
        remoteCandidate,
      )
      ? remoteCandidate
      : new RTCIceCandidate(remoteCandidate ?? { sdpMid: "" });
    if (ice.closed) {
      throw invalidState("The RTCIceTransport is closed");
    }
    // An empty candidate is the remote end-of-candidates.
    if (candidate.candidate === "") return;
    if (candidate.foundation === null || candidate.type === null) {
      throw new DOMException(
        `Failed to execute 'addRemoteCandidate' on 'RTCIceTransport': '${candidate.candidate}' is not a valid candidate`,
        "OperationError",
      );
    }
    op_webrtc_ice_add_remote_candidate(ice.rid, candidate.candidate);
    ArrayPrototypePush(ice.remote, candidate);
    if (ice.remoteParameters !== null && ice.state === "new") {
      ice.state = "checking";
    }
  }
}
defineHandler(RTCIceTransport.prototype, "statechange");
defineHandler(RTCIceTransport.prototype, "gatheringstatechange");
defineHandler(RTCIceTransport.prototype, "selectedcandidatepairchange");
defineHandler(RTCIceTransport.prototype, "error");
defineHandler(RTCIceTransport.prototype, "icecandidate");
webidl.configureInterface(RTCIceTransport);
const RTCIceTransportPrototype = RTCIceTransport.prototype;

const iceTransportRegistry = new SafeFinalizationRegistry((rid) => {
  try {
    op_webrtc_ice_stop(rid);
  } catch {
    // Already gone.
  }
});

// RFC 8839 section 5.4 (ice-char = ALPHA / DIGIT / "+" / "/").
const ICE_UFRAG = new SafeRegExp("^[A-Za-z0-9+/]{4,256}$");
const ICE_PWD = new SafeRegExp("^[A-Za-z0-9+/]{22,256}$");

// The webrtc-ice methods exist only on a transport of its own: a peer
// connection's transport is driven by its descriptions.
function requireStandalone(transport, method) {
  const ice = transport[_ice];
  if (ice === undefined) {
    throw invalidState(
      `RTCIceTransport.${method}() is only for a transport made with new RTCIceTransport()`,
    );
  }
  return ice;
}

// A gathering or started transport keeps the process alive, as an open
// socket does, until stop().
function iceActivate(ice) {
  if (ice.closed || ice.active) return;
  ice.active = true;
  if (ice.pending) core.refOpPromise(ice.pending);
}

function icePump(transport) {
  const ice = transport[_ice];
  if (ice.closed || ice.pending) return;
  const promise = op_webrtc_ice_next_events(ice.rid);
  ice.pending = promise;
  if (!ice.active) core.unrefOpPromise(promise);
  PromisePrototypeThen(promise, (events) => {
    ice.pending = null;
    if (events === null || ice.closed) return;
    iceDispatchEvents(transport, events, 0);
  }, () => {
    ice.pending = null;
  });
}

// Each event is its own task, as for a peer connection.
function iceDispatchEvents(transport, events, from) {
  const ice = transport[_ice];
  try {
    iceDispatchEvent(transport, events, from);
  } catch (error) {
    reportException(error);
  }
  if (ice.closed) return;
  if (from + 1 >= events.length) {
    icePump(transport);
    return;
  }
  const next = op_void_async_deferred();
  ice.pending = next;
  if (!ice.active) core.unrefOpPromise(next);
  PromisePrototypeThen(next, () => {
    ice.pending = null;
    if (!ice.closed) iceDispatchEvents(transport, events, from + 1);
  });
}

function iceDispatchEvent(transport, events, from) {
  const ice = transport[_ice];
  const event = events[from];
  switch (event.type) {
    case "icecandidate": {
      const candidate = new RTCIceCandidate({
        candidate: event.candidate,
        sdpMid: "",
        sdpMLineIndex: 0,
        usernameFragment: ice.localParameters.usernameFragment,
      });
      ArrayPrototypePush(ice.local, candidate);
      fire(
        transport,
        new RTCPeerConnectionIceEvent("icecandidate", { candidate }),
      );
      break;
    }
    case "gatheringcomplete":
      // W3C webrtc-ice: the null candidate, then (a task of its own) the
      // gathering state "complete".
      ArrayPrototypeSplice(events, from + 1, 0, { type: "gatheringstate" });
      fire(
        transport,
        new RTCPeerConnectionIceEvent("icecandidate", { candidate: null }),
      );
      break;
    case "gatheringstate":
      if (ice.gatheringState === "complete") break;
      ice.gatheringState = "complete";
      fire(transport, new Event("gatheringstatechange"));
      break;
    case "statechange":
      // `checking` is set by start() / addRemoteCandidate() themselves (the
      // agent reports it as soon as it starts, before any candidate).
      if (event.state === "checking" || ice.state === event.state) break;
      ice.state = event.state;
      fire(transport, new Event("statechange"));
      break;
    case "selectedcandidatepairchange":
      fire(transport, new Event("selectedcandidatepairchange"));
      break;
    case "rolechange":
      ice.role = event.controlling ? "controlling" : "controlled";
      break;
  }
}

// The standalone transport's selected pair, its candidates matched with the
// ones gathered and added (a peer-reflexive remote one hides its address).
function iceSelectedPair(ice) {
  if (ice.closed) return null;
  const pair = op_webrtc_ice_selected_pair(ice.rid);
  if (pair === null) return null;
  return new RTCIceCandidatePair(
    illegalConstructorKey,
    findPairCandidate(ice.local, pair.local) ?? synthPairCandidate(pair.local),
    findPairCandidate(ice.remote, pair.remote) ??
      (pair.remote.type === "prflx"
        ? hiddenPairCandidate(pair.remote)
        : synthPairCandidate(pair.remote)),
  );
}

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

// The candidate of `list` (signalled or received by the application) at a
// selected pair candidate's address, port and protocol.
function findPairCandidate(list, cand) {
  for (let i = 0; i < list.length; ++i) {
    const c = list[i];
    const host = StringPrototypeIncludes(c.address ?? "", ":")
      ? `[${c.address}]`
      : c.address;
    if (`${host}:${c.port}` === cand.addr && c.protocol === cand.protocol) {
      return c;
    }
  }
  return null;
}

// A selected pair candidate the application doesn't have (gathered or
// learned by the agent itself), written out from its fields.
function synthPairCandidate(cand) {
  const related = cand.relatedAddress !== ""
    ? ` raddr ${cand.relatedAddress} rport ${cand.relatedPort}`
    : cand.type === "host"
    ? ""
    : " raddr 0.0.0.0 rport 0";
  const foundation = RegExpPrototypeTest(ICE_CHARS, cand.foundation)
    ? cand.foundation
    : "0";
  return new RTCIceCandidate({
    candidate: `candidate:${foundation} 1 ${cand.protocol} ${
      MathMax(1, cand.priority)
    } ${cand.address} ${cand.port} typ ${cand.type}${related}${
      cand.protocol === "tcp" && cand.type !== "relay" ? " tcptype active" : ""
    }`,
    sdpMLineIndex: 0,
  });
}

// A peer-reflexive remote candidate the application never signalled: W3C
// webrtc-pc 4.8.1 exposes it with an empty candidate string and a null
// address (its address came from the network, not from the application).
function hiddenPairCandidate(cand) {
  return candidateWithFields({ candidate: "", sdpMLineIndex: 0 }, {
    foundation: cand.foundation,
    component: "rtp",
    priority: cand.priority,
    address: null,
    protocol: cand.protocol === "tcp" ? "tcp" : "udp",
    port: cand.port,
    type: "prflx",
    tcpType: null,
    relatedAddress: null,
    relatedPort: cand.relatedPort,
  });
}

// The selected pair as RTCIceCandidates, from the candidates this side
// signalled and received (matched by address); null before ICE connects.
function selectedPair(pc) {
  const state = pc[_state];
  if (state.closed) return null;
  const ice = op_webrtc_pc_ice_selected_pair(pc[_rid]);
  if (ice !== null) {
    return new RTCIceCandidatePair(
      illegalConstructorKey,
      findPairCandidate(state.localCandidates, ice.local) ??
        synthPairCandidate(ice.local),
      findPairCandidate(state.remoteCandidates, ice.remote) ??
        (ice.remote.type === "prflx"
          ? hiddenPairCandidate(ice.remote)
          : synthPairCandidate(ice.remote)),
    );
  }
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
  const entries = ArrayPrototypeMap(
    op_webrtc_pc_get_stats(
      pc[_rid],
      key ?? 0,
      key !== null,
      sender,
    ),
    plainStats,
  );
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    for (let f = 0; f < STATS_TIME_FIELDS.length; ++f) {
      const field = STATS_TIME_FIELDS[f];
      if (typeof stats[field] === "number") stats[field] += base;
    }
  }
  return refineStats(pc, entries, base, key === null);
}

// A stats object is a dictionary (webrtc-stats 5, `RTCStats`): an ordinary
// object, as its records (`qualityLimitationDurations`) are. The op hands
// over null-prototype objects, which have no `hasOwnProperty`.
function plainStats(value) {
  if (value === null || typeof value !== "object") return value;
  if (ArrayIsArray(value)) return ArrayPrototypeMap(value, plainStats);
  const out = {};
  // The op's objects have no prototype: this sees their own members only.
  for (const key in value) out[key] = plainStats(value[key]);
  return out;
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
  // A reference with nothing to name: no pair selected yet, no certificate
  // exchanged, a stream not yet associated with a media section.
  "selectedCandidatePairId",
  "localCertificateId",
  "remoteCertificateId",
  "mid",
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
function refineStats(pc, engineEntries, now, whole) {
  const entries = withoutRetiredStreams(
    pc,
    ArrayPrototypeFilter(engineEntries, remoteStreamReported),
  );
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
        // wpt100:ice: a peer-reflexive remote candidate's address came from
        // the network, not from the application: webrtc-stats 8.13 leaves it
        // null unless the application signalled that address itself.
        if (
          stats.type === "remote-candidate" &&
          stats.candidateType === "prflx" &&
          !ArrayPrototypeSome(
            state.remoteCandidates,
            (c) => c.address === stats.address,
          )
        ) {
          stats.address = null;
        }
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
        transportStateStats(pc, stats);
        break;
      case "outbound-rtp": {
        const t = ArrayPrototypeFind(
          state.transceivers,
          (t) => t[_state].mid !== null && t[_state].mid === stats.mid,
        );
        // An encoding is "active" as the application configured it
        // (webrtc-stats `RTCOutboundRtpStreamStats.active`:
        // `RTCRtpEncodingParameters.active`).
        const encodings = t?.[_state].sender[_state].encodings;
        const encoding = encodings?.[stats.encodingIndex ?? 0];
        if (encoding !== undefined) stats.active = encoding.active !== false;
        // `encodingIndex` exists for video streams only.
        if (stats.kind === "audio") delete stats.encodingIndex;
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
      case "inbound-rtp": {
        // `trackIdentifier` is the receiver's track's id (webrtc-stats
        // `RTCInboundRtpStreamStats.trackIdentifier`), which the msid gave
        // it, not the engine's own name for the track.
        const t = transceiverOfStream(state, stats);
        const track = t?.[_state].receiver[_state].track;
        if (track !== undefined && track !== null) {
          stats.trackIdentifier = track.id;
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
  if (whole) {
    addSenderSources(state, entries, ids, now);
    addChannelStats(pc, entries, ids, now);
  }
  // Before the first description there is no RTCDtlsTransport (and so no
  // RTCIceTransport) for a `transport` object to describe.
  const report = pc.localDescription === null &&
      pc.remoteDescription === null
    ? ArrayPrototypeFilter(entries, (e) => e.type !== "transport")
    : entries;
  dropUnresolvedReferences(report);
  return report;
}

// The transport's ICE and DTLS state and the ICE role before the engine has
// any to report (webrtc-stats `RTCTransportStats`: the RTCIceTransport's
// `state` and `role`, the RTCDtlsTransport's `state`). The role is the one
// this side takes in the offer / answer exchange under way (RFC 8445
// section 6.1.1: the offerer is controlling) until an answer fixes it.
function transportStateStats(pc, stats) {
  const s = pc[_state];
  const dtls = s.dtls;
  if (stats.iceState === undefined) {
    stats.iceState = dtls !== null && dtls !== undefined
      ? dtls[_transportState].ice[_transportState].state
      : (s.closed ? "closed" : s.iceConnectionState);
  }
  if (stats.dtlsState === undefined) {
    let dtlsState = dtls !== null && dtls !== undefined
      ? dtls[_transportState].state
      : "new";
    if (dtls === null || dtls === undefined) {
      if (s.closed || s.connectionState === "closed") dtlsState = "closed";
      else if (s.connectionState === "failed") dtlsState = "failed";
      else if (s.connectionState === "connected") dtlsState = "connected";
      else if (s.connectionState === "connecting") dtlsState = "connecting";
    }
    stats.dtlsState = dtlsState;
  }
  if (stats.iceRole === undefined) {
    let role = s.iceRole;
    if (role === null || role === undefined) {
      role = s.signalingState === "have-local-offer" ||
          s.signalingState === "have-local-pranswer"
        ? "controlling"
        : s.signalingState === "have-remote-offer" ||
            s.signalingState === "have-remote-pranswer"
        ? "controlled"
        : "unknown";
    }
    stats.iceRole = role;
  }
}

// webrtc-stats 6.1: the remote RTP stream objects are created when their
// RTCP packet is first received. The engine has one beside each local stream
// from the start: a `remote-inbound-rtp` counts the receiver reports it was
// made from (each one a round-trip measurement), a `remote-outbound-rtp` the
// sender reports.
function remoteStreamReported(stats) {
  if (stats.type === "remote-inbound-rtp") {
    return stats.roundTripTimeMeasurements !== 0;
  }
  if (stats.type === "remote-outbound-rtp") return stats.reportsSent !== 0;
  return true;
}

// A `media-source` for each track attached to a sender (webrtc-stats
// `RTCStatsType` "media-source": created when a track is attached to any
// RTCRtpSender), also before the sender has an outbound stream.
function addSenderSources(state, entries, ids, now) {
  for (let i = 0; i < state.transceivers.length; ++i) {
    const t = state.transceivers[i];
    if (t[_state].stopped) continue;
    const track = t[_state].sender[_state].track;
    if (track === null || track === undefined) continue;
    const source = mediaSourceStats(track, {}, now);
    if (SetPrototypeHas(ids, source.id)) continue;
    SetPrototypeAdd(ids, source.id);
    ArrayPrototypePush(entries, source);
  }
}

// A `data-channel` object for each channel from its creation (webrtc-stats
// `RTCStatsType` "data-channel": created when the RTCDataChannel is), which
// the engine has only once the channel opens. Before that the channel has
// sent and received nothing; `dataChannelIdentifier` is there once the
// channel has an id.
function addChannelStats(pc, entries, ids, now) {
  MapPrototypeForEach(pc[_channels], (channel, handle) => {
    const id = `RTCDataChannel_${handle}`;
    if (SetPrototypeHas(ids, id)) return;
    SetPrototypeAdd(ids, id);
    const stats = {
      timestamp: now,
      type: "data-channel",
      id,
      label: channel.label,
      protocol: channel.protocol,
      state: channel.readyState,
      messagesSent: 0,
      bytesSent: 0,
      messagesReceived: 0,
      bytesReceived: 0,
    };
    const streamId = channel.id;
    if (streamId !== null) stats.dataChannelIdentifier = streamId;
    ArrayPrototypePush(entries, stats);
  });
}

// webrtc-stats 5 (`RTCStats`): a member that references another object
// names one in the same report; a reference with nothing to name is left out.
function dropUnresolvedReferences(entries) {
  const ids = new SafeSet();
  for (let i = 0; i < entries.length; ++i) SetPrototypeAdd(ids, entries[i].id);
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    for (const key in stats) {
      if (
        StringPrototypeEndsWith(key, "Id") && typeof stats[key] === "string" &&
        !SetPrototypeHas(ids, stats[key])
      ) {
        delete stats[key];
      }
    }
  }
}

// The transceiver an RTP stream's `mid` names, if this connection still
// has it.
function transceiverOfStream(state, stats) {
  if (typeof stats.mid !== "string" || stats.mid === "") return undefined;
  return ArrayPrototypeFind(
    state.transceivers,
    (t) => t[_state].mid === stats.mid,
  );
}

// The RTP stream stats whose lifetime ended (webrtc-stats 6.1, "lifetime
// considerations"): those of a transceiver that is stopped (webrtc-pc 5.4,
// "stop the RTCRtpTransceiver": a negotiation completed after stop()) or
// gone, with the remote stream objects and the codecs only they named.
// stop() alone keeps them until that negotiation.
function withoutRetiredStreams(pc, entries) {
  const state = pc[_state];
  const retired = new SafeSet();
  const codecs = new SafeSet();
  for (let i = 0; i < entries.length; ++i) {
    const stats = entries[i];
    if (stats.type !== "inbound-rtp" && stats.type !== "outbound-rtp") {
      continue;
    }
    if (typeof stats.mid !== "string" || stats.mid === "") continue;
    const t = transceiverOfStream(state, stats);
    if (t !== undefined && !t[_state].stopped) continue;
    SetPrototypeAdd(retired, stats.id);
    if (typeof stats.codecId === "string") {
      SetPrototypeAdd(codecs, stats.codecId);
    }
  }
  if (SetPrototypeGetSize(retired) === 0) return entries;
  const kept = ArrayPrototypeFilter(
    entries,
    (e) =>
      !SetPrototypeHas(retired, e.id) && !SetPrototypeHas(retired, e.localId),
  );
  return ArrayPrototypeFilter(
    kept,
    (e) =>
      e.type !== "codec" || !SetPrototypeHas(codecs, e.id) ||
      ArrayPrototypeSome(kept, (o) => o.codecId === e.id),
  );
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
    return rtpTransportOf(this[_transceiver]);
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
    const state = this[_state];
    const kind = this[_transceiver][_state].kind;
    const negotiated = senderNegotiated(this);
    const codecs = negotiated.codecs;
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
      if (e.codec !== undefined) out.codec = { ...e.codec };
      if (e.encodingComplexity !== undefined) {
        out.encodingComplexity = e.encodingComplexity;
      }
      if (e.scalabilityMode !== undefined) {
        // Once a codec is negotiated, the mode it can send.
        const sending = e.codec ??
          ArrayPrototypeFind(codecs, (c) => !isRepairCodec(c.mimeType));
        out.scalabilityMode = sending === undefined
          ? e.scalabilityMode
          : negotiatedScalabilityMode(e.scalabilityMode, sending.mimeType);
      }
      return out;
    });
    // [[LastReturnedParameters]]: until the current task ends, the same
    // transaction (webrtc-pc 5.2 getParameters()).
    const previous = this[_pc][_state].closed ? null : state.lastParameters;
    const params = {
      transactionId: previous?.transactionId ?? uuid(),
      encodings,
      headerExtensions: negotiated.headerExtensions,
      rtcp: { cname: this[_pc][_state].cname, reducedSize: false },
      codecs,
    };
    state.lastParameters = {
      transactionId: params.transactionId,
      codecs: params.codecs,
      snapshot: JSONStringify({
        e: ArrayPrototypeMap(params.encodings, (e) => e.rid ?? null),
        h: params.headerExtensions,
        r: params.rtcp,
        c: params.codecs,
      }),
    };
    if (previous === null) {
      // The transaction id is valid until the current task ends.
      const pc = this[_pc];
      const timers = pc[_state].timers;
      const last = state.lastParameters;
      const timer = setTimeout(() => {
        SetPrototypeDelete(timers, timer);
        if (state.lastParameters?.transactionId === last.transactionId) {
          state.lastParameters = null;
        }
      }, 0);
      SetPrototypeAdd(timers, timer);
    }
    return params;
  }

  setParameters(parameters, setParameterOptions = undefined) {
    const branded = brandError(this, RTCRtpSenderPrototype);
    if (branded !== null) return PromiseReject(branded);
    const transceiver = this[_transceiver];
    const state = this[_state];
    try {
      // The dictionary's required members (WebIDL conversion comes first).
      if (parameters === null || typeof parameters !== "object") {
        throw new TypeError("setParameters: parameters are required");
      }
      if (parameters.transactionId === undefined) {
        throw new TypeError("setParameters: 'transactionId' is required");
      }
      if (parameters.encodings === undefined) {
        throw new TypeError("setParameters: 'encodings' is required");
      }
      const encodingOptions = setParameterOptions?.encodingOptions === undefined
        ? []
        : ArrayFrom(setParameterOptions.encodingOptions);
      if (transceiver[_state].stopping || transceiver[_state].stopped) {
        throw invalidState("The transceiver is stopped");
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
      const kind = transceiver[_state].kind;
      // webrtc-extensions: encodingOptions, when given, has one entry per
      // encoding (audio senders ignore it).
      if (
        kind === "video" && encodingOptions.length !== 0 &&
        encodingOptions.length !== encodings.length
      ) {
        throw new DOMException(
          "setParameters: encodingOptions doesn't match the encodings",
          "InvalidModificationError",
        );
      }
      const next = validateEncodings(encodings, kind, "setParameters", true);
      // webrtc-pc 5.2 setParameters(): an encoding's codec must be one the
      // sender can choose, the negotiated codecs, else what it implements.
      let choosable = last.codecs ?? [];
      if (choosable.length === 0) {
        choosable = capabilities(kind, true)?.codecs ?? [];
      }
      for (let i = 0; i < next.length; ++i) {
        if (
          next[i].codec !== undefined &&
          !ArrayPrototypeSome(choosable, (c) => codecMatches(c, next[i].codec))
        ) {
          throw new DOMException(
            `setParameters: ${next[i].codec.mimeType} is not a codec the sender can use`,
            "InvalidModificationError",
          );
        }
      }
      state.encodings = next;
      const pc = this[_pc];
      if (!pc[_state].closed) {
        op_webrtc_sender_set_paused(
          pc[_rid],
          transceiver[_key],
          state.track === null || !next[0]?.active,
        );
      }
      // [[LastReturnedParameters]] is cleared by the task that resolves the
      // promise, so the same parameters may be set again before it runs.
      return PromisePrototypeThen(nextTask(null), () => {
        if (state.lastParameters?.transactionId === last.transactionId) {
          state.lastParameters = null;
        }
      });
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

// The sender's negotiated codecs and header extensions (none before the
// transceiver has negotiated a direction).
function senderNegotiated(sender) {
  const pc = sender[_pc];
  const transceiver = sender[_transceiver];
  let negotiated = { codecs: [], headerExtensions: [] };
  if (!pc[_state].closed && transceiver[_state].currentDirection !== null) {
    try {
      negotiated = op_webrtc_sender_parameters(
        pc[_rid],
        transceiver[_key],
      ) ?? negotiated;
    } catch {
      // The transceiver is gone.
    }
  }
  return {
    codecs: ArrayPrototypeMap(negotiated.codecs, (c) => ({ ...c })),
    headerExtensions: ArrayPrototypeMap(
      negotiated.headerExtensions,
      (h) => ({ ...h }),
    ),
  };
}

// Retransmission, redundancy and FEC formats (RFC 4588, 2198, 5109).
function isRepairCodec(mimeType) {
  const m = StringPrototypeToLowerCase(String(mimeType));
  return StringPrototypeEndsWith(m, "/rtx") ||
    StringPrototypeEndsWith(m, "/red") ||
    StringPrototypeIncludes(m, "fec");
}

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
  // Playout task, step 1-2: a stopping transceiver, or one negotiated to
  // send nothing (currentDirection recvonly / inactive), plays no more tones.
  if (
    pc[_state].closed || t.stopping || t.stopped ||
    t.currentDirection === "recvonly" || t.currentDirection === "inactive"
  ) {
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
    return rtpTransportOf(this[_transceiver]);
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

// wpt100:hdrext — RTP header extension control (webrtc-extensions).
const EXTENSION_DIRECTIONS = [
  "sendrecv",
  "sendonly",
  "recvonly",
  "inactive",
  "stopped",
];
// Mandatory to use, both ways: the MID extension with BUNDLE (RFC 9143
// section 9.1, RFC 8834 section 5.2.4).
const MANDATORY_HEADER_EXTENSIONS = ["urn:ietf:params:rtp-hdrext:sdes:mid"];
// Implemented, but not in an initial offer unless the application asks (an
// answer still takes them when offered).
const UNOFFERED_HEADER_EXTENSIONS = [
  "http://www.webrtc.org/experiments/rtp-hdrext/abs-capture-time",
];

function copyHeaderExtensions(list) {
  return ArrayPrototypeMap(
    list,
    (e) => ({ uri: e.uri, direction: e.direction }),
  );
}

// [[HeaderExtensionsToNegotiate]] of a new transceiver: the first
// non-stopping transceiver's of the same kind, else the implemented ones;
// [[NegotiatedHeaderExtensions]]: each of them, "stopped".
function initHeaderExtensions(transceiver, pc, kind) {
  const state = transceiver[_state];
  const first = ArrayPrototypeFind(
    pc[_state].transceivers,
    (t) =>
      t[_state].kind === kind && !t[_state].stopping && !t[_state].stopped,
  );
  if (first !== undefined) {
    state.headerExtensionsToNegotiate = copyHeaderExtensions(
      first[_state].headerExtensionsToNegotiate,
    );
    state.userStoppedExtensions = new SafeSet();
    SetPrototypeForEach(
      first[_state].userStoppedExtensions,
      (uri) => SetPrototypeAdd(state.userStoppedExtensions, uri),
    );
  } else {
    const implemented = capabilities(kind, true)?.headerExtensions ?? [];
    state.headerExtensionsToNegotiate = ArrayPrototypeMap(
      implemented,
      (h) => ({
        uri: h.uri,
        direction: ArrayPrototypeIncludes(UNOFFERED_HEADER_EXTENSIONS, h.uri)
          ? "stopped"
          : "sendrecv",
      }),
    );
    state.userStoppedExtensions = new SafeSet();
  }
  state.negotiatedHeaderExtensions = ArrayPrototypeMap(
    state.headerExtensionsToNegotiate,
    (e) => ({ uri: e.uri, direction: "stopped" }),
  );
}

// Before an offer or answer: the extensions each transceiver's media section
// may carry. An offer has the ones not "stopped"; an answer the offered ones
// the application didn't stop itself (one only the last negotiation left
// "stopped" comes back when the peer offers it again).
function pushHeaderExtensions(pc, offer) {
  const transceivers = pc[_state].transceivers;
  for (let i = 0; i < transceivers.length; ++i) {
    const ts = transceivers[i][_state];
    if (ts.stopped) continue;
    const uris = [];
    const list = ts.headerExtensionsToNegotiate;
    for (let j = 0; j < list.length; ++j) {
      const e = list[j];
      const keep = offer
        ? e.direction !== "stopped"
        : !SetPrototypeHas(ts.userStoppedExtensions, e.uri);
      if (keep) ArrayPrototypePush(uris, e.uri);
    }
    try {
      op_webrtc_transceiver_set_header_extensions(
        pc[_rid],
        transceivers[i][_key],
        uris,
      );
    } catch {
      // The engine's transceiver is gone.
    }
  }
}

// The `a=extmap` lines of each media section, by mid: id and direction
// (sendrecv when unstated) per URI.
function extmapsOf(sdp) {
  const out = new SafeMap();
  const lines = StringPrototypeSplit(sdp, "\n");
  let current = null;
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "m=")) {
      const port = StringPrototypeSplit(line, " ")[1];
      current = { mid: null, rejected: port === "0", extensions: [] };
      continue;
    }
    if (current === null) continue;
    if (line === "a=bundle-only") {
      current.rejected = false;
    } else if (StringPrototypeStartsWith(line, "a=mid:")) {
      current.mid = StringPrototypeSlice(line, 6);
      MapPrototypeSet(out, current.mid, current);
    } else if (StringPrototypeStartsWith(line, "a=extmap:")) {
      const fields = StringPrototypeSplit(StringPrototypeSlice(line, 9), " ");
      if (fields.length < 2) continue;
      const idDir = StringPrototypeSplit(fields[0], "/");
      ArrayPrototypePush(current.extensions, {
        id: NumberParseInt(idDir[0], 10),
        direction: idDir[1] ?? "sendrecv",
        uri: fields[1],
      });
    }
  }
  return out;
}

// webrtc-extensions, "set a session description" for an answer: each
// extension's direction as the answer has it (reversed for a remote one,
// "stopped" when absent) becomes both [[NegotiatedHeaderExtensions]] and
// [[HeaderExtensionsToNegotiate]].
function headerExtensionsNegotiated(pc, sdp, remote) {
  const sections = extmapsOf(sdp);
  const transceivers = pc[_state].transceivers;
  for (let i = 0; i < transceivers.length; ++i) {
    const ts = transceivers[i][_state];
    if (ts.mid === null) continue;
    const section = MapPrototypeGet(sections, ts.mid);
    if (section === undefined) continue;
    const negotiated = ArrayPrototypeMap(
      ts.headerExtensionsToNegotiate,
      (e) => {
        const found = section.rejected
          ? undefined
          : ArrayPrototypeFind(section.extensions, (x) => x.uri === e.uri);
        let direction = found === undefined ? "stopped" : found.direction;
        if (found !== undefined && remote) {
          direction = reverseDirection(direction);
        }
        return { uri: e.uri, direction };
      },
    );
    ts.negotiatedHeaderExtensions = negotiated;
    ts.headerExtensionsToNegotiate = copyHeaderExtensions(negotiated);
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
      // [[SenderTransport]] / [[ReceiverTransport]] are set (true) and their
      // values at the last stable state, which a rollback restores.
      transport: false,
      stableTransport: false,
    };
    initHeaderExtensions(this, pc, init.kind);
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
    // wpt100:transceiver: a remote offer's transceiver made to send has a
    // sender in the engine now (media.rs ensure_sender), paused until its
    // RTCRtpSender has a track.
    if (direction === "sendrecv" || direction === "sendonly") {
      const sender = state.sender;
      op_webrtc_sender_set_paused(
        pc[_rid],
        this[_key],
        sender[_state].track === null ||
          !(sender[_state].encodings[0]?.active ?? true),
      );
    }
    invalidateCreated(pc);
    updateNegotiationNeeded(pc);
  }
  get currentDirection() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const state = this[_state];
    return state.stopped ? "stopped" : state.currentDirection;
  }
  // wpt100:transceiver: `stopped` left webrtc-pc for `currentDirection ==
  // "stopped"`, but every browser still has it and the WPT transceiver tests
  // read it: the transceiver's [[Stopped]] slot.
  get stopped() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return this[_state].stopped;
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
    invalidateCreated(pc);
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
    invalidateCreated(pc);
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

  // wpt100:hdrext — webrtc-extensions RTP header extension control.
  getHeaderExtensionsToNegotiate() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return copyHeaderExtensions(this[_state].headerExtensionsToNegotiate);
  }

  setHeaderExtensionsToNegotiate(extensions) {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    const prefix =
      "Failed to execute 'setHeaderExtensionsToNegotiate' on 'RTCRtpTransceiver'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    if (
      extensions === null || typeof extensions !== "object" ||
      typeof extensions[SymbolIterator] !== "function"
    ) {
      throw new TypeError(`${prefix}: the argument is not a sequence`);
    }
    const list = ArrayFrom(extensions);
    const converted = [];
    for (let i = 0; i < list.length; ++i) {
      const e = list[i];
      if (e === null || typeof e !== "object") {
        throw new TypeError(
          `${prefix}: an RTCRtpHeaderExtensionCapability must be an object`,
        );
      }
      // `uri` is required; an empty one names no extension.
      if (e.uri === undefined || String(e.uri) === "") {
        throw new TypeError(`${prefix}: 'uri' is required`);
      }
      if (e.direction === undefined) {
        throw new TypeError(`${prefix}: 'direction' is required`);
      }
      const direction = String(e.direction);
      if (!ArrayPrototypeIncludes(EXTENSION_DIRECTIONS, direction)) {
        throw new TypeError(
          `${prefix}: '${direction}' is not a valid RTCRtpTransceiverDirection`,
        );
      }
      ArrayPrototypePush(converted, { uri: String(e.uri), direction });
    }
    const current = this[_state].headerExtensionsToNegotiate;
    if (converted.length !== current.length) {
      throw new DOMException(
        `${prefix}: the list must have the same extensions as getHeaderExtensionsToNegotiate()`,
        "InvalidModificationError",
      );
    }
    for (let i = 0; i < converted.length; ++i) {
      const { uri, direction } = converted[i];
      if (uri !== current[i].uri) {
        throw new DOMException(
          `${prefix}: '${uri}' is not the extension at index ${i}`,
          "InvalidModificationError",
        );
      }
      if (
        ArrayPrototypeIncludes(MANDATORY_HEADER_EXTENSIONS, uri) &&
        direction !== "sendrecv"
      ) {
        throw new DOMException(
          `${prefix}: '${uri}' is mandatory and must be 'sendrecv'`,
          "InvalidModificationError",
        );
      }
    }
    const stopped = new SafeSet();
    for (let i = 0; i < converted.length; ++i) {
      if (converted[i].direction === "stopped") {
        SetPrototypeAdd(stopped, converted[i].uri);
      }
    }
    this[_state].headerExtensionsToNegotiate = converted;
    this[_state].userStoppedExtensions = stopped;
    // A new offer or answer would carry other a=extmap lines: the last
    // created ones are stale (wpt100:transceiver's generation counter).
    invalidateCreated(this[_pc]);
  }

  getNegotiatedHeaderExtensions() {
    webidl.assertBranded(this, RTCRtpTransceiverPrototype);
    return copyHeaderExtensions(this[_state].negotiatedHeaderExtensions);
  }
}
webidl.configureInterface(RTCRtpTransceiver);
const RTCRtpTransceiverPrototype = RTCRtpTransceiver.prototype;

// webrtc-pc 5.4 "stop sending and receiving". The receiver's track ends as the
// user agent's own doing (not track.stop()), which mediacapture-main does in a
// queued task: the stop() call or the description that caused it returns
// first, with the track still live (WPT RTCRtpTransceiver-stopping).
// The sender's and receiver's `transport` (one object for both): null until a
// description gives the transceiver one (webrtc-pc 4.4.1.6: a local offer or
// answer, or a remote answer; never a remote offer), and null again when a
// rollback takes it back. The engine bundles everything on its one DTLS
// transport, which is the object every transceiver shares.
function rtpTransportOf(transceiver) {
  return transceiver[_state].transport
    ? dtlsTransportOf(transceiver[_pc])
    : null;
}

function stopSending(transceiver) {
  const sender = transceiver[_state].sender;
  updateSenderSource(sender, sender[_state].track, null);
  const receiver = transceiver[_state].receiver;
  removeRemoteTrack(transceiver);
  closeReaders(receiver);
  endTrackSoon(receiver[_state].track);
  for (let i = 0; i < receiver[_state].clones.length; ++i) {
    endTrackSoon(receiver[_state].clones[i]);
  }
}

// A plain timer, not one of the connection's: close() ends the track at once
// without an event, and the task then finds it ended.
function endTrackSoon(track) {
  if (track[_trackState].readyState === "ended") return;
  setTimeout(() => endTrack(track), 0);
}

// webrtc-pc 5.4 "stop the RTCRtpTransceiver": [[Stopping]] and [[Stopped]]
// (currentDirection reads "stopped"). `engine` stops the engine's transceiver
// too (a remote description rejected its m-section); a rolled-back remote
// offer's is gone from the engine already.
function stopTransceiverNow(pc, transceiver, engine) {
  const ts = transceiver[_state];
  if (ts.stopped) return;
  if (!ts.stopping) stopSending(transceiver);
  ts.stopping = true;
  ts.stopped = true;
  if (engine) {
    try {
      op_webrtc_transceiver_stop(pc[_rid], transceiver[_key]);
    } catch {
      // The engine's transceiver is gone.
    }
  }
}

// A stopped transceiver leaves the connection's set of transceivers and loses
// its mid; the engine keeps its own, which the next syncs leave out.
function retireTransceiver(pc, transceiver) {
  transceiver[_state].mid = null;
  SetPrototypeAdd(pc[_state].retired, transceiver[_key]);
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
    // webrtc-pc 5.2: the codec an encoding is sent with (an RTCRtpCodec).
    if (e.codec !== undefined) enc.codec = convertCodec(e.codec, method);
    // webrtc-extensions: an enum, so an unknown value is a TypeError for any
    // kind; an audio encoding has no encoder complexity and drops it.
    if (e.encodingComplexity !== undefined) {
      const complexity = String(e.encodingComplexity);
      if (!ArrayPrototypeIncludes(ENCODING_COMPLEXITIES, complexity)) {
        throw new TypeError(
          `${method}: '${complexity}' is not a valid RTCEncodingComplexity`,
        );
      }
      if (kind === "video") enc.encodingComplexity = complexity;
    }
    // webrtc-svc 4.2.1: a mode no codec supports is an OperationError;
    // audio has no scalability modes and drops it.
    if (e.scalabilityMode !== undefined && kind === "video") {
      const mode = String(e.scalabilityMode);
      if (!ArrayPrototypeIncludes(SCALABILITY_MODES, mode)) {
        throw new DOMException(
          `${method}: '${mode}' is not a supported scalability mode`,
          "OperationError",
        );
      }
      enc.scalabilityMode = mode;
    }
    ArrayPrototypePush(out, enc);
  }
  if (!keepRid) {
    // webrtc-pc 5.1 addTransceiver(): a codec that is not one of
    // RTCRtpSender.getCapabilities(kind).codecs is an OperationError.
    const send = capabilities(kind, true)?.codecs ?? [];
    for (let i = 0; i < out.length; ++i) {
      if (
        out[i].codec !== undefined &&
        !ArrayPrototypeSome(send, (c) => codecMatches(c, out[i].codec))
      ) {
        throw new DOMException(
          `${method}: ${out[i].codec.mimeType} is not a codec this sender implements`,
          "OperationError",
        );
      }
    }
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

// A sender's [[SendEncodings]] when a description has been applied
// (webrtc-pc 4.4.1.6 "set the RTCSessionDescription"): `remote` is the
// transceiver's m-section in the remote description.
// - A remote offer asking to receive simulcast gives a sender that has one
//   encoding without a rid one encoding per offered layer (the first of each
//   layer's alternatives, each once), scaled down by powers of two; the
//   encodings it replaced come back on a rollback.
// - An answer (local or remote) keeps the encodings whose layer the remote
//   description still has, in the sender's order, or only the first when it
//   has none of them (no simulcast, or every layer gone).
// - An answer also drops an encoding's codec that is no longer negotiated.
// Each success task clears [[LastReturnedParameters]].
function negotiateEncodings(transceiver, remote, phase) {
  const sender = transceiver[_state].sender;
  const s = sender[_state];
  if (phase.type === "rollback") {
    if (s.ridlessEncodings !== undefined && s.ridlessEncodings !== null) {
      s.encodings = s.ridlessEncodings;
      s.ridlessEncodings = null;
    }
    s.lastParameters = null;
    return;
  }
  s.lastParameters = null;
  const video = transceiver[_state].kind === "video";
  const layers = remote?.simulcastRecv ?? [];
  if (phase.remote && phase.type === "offer" && video && layers.length > 0) {
    const encodings = s.encodings;
    if (encodings.length === 1 && encodings[0].rid === undefined) {
      const n = MathMin(layers.length, MAX_ENCODINGS);
      const next = [];
      for (let i = 0; i < n; ++i) {
        ArrayPrototypePush(next, {
          active: true,
          rid: layers[i],
          scaleResolutionDownBy: 2 ** (n - 1 - i),
        });
      }
      s.ridlessEncodings = encodings;
      s.encodings = next;
    }
  }
  if (phase.type !== "answer" && phase.type !== "pranswer") return;
  if (phase.type === "answer") s.ridlessEncodings = null;
  if (s.encodings.length > 1 && remote !== null && remote !== undefined) {
    const kept = ArrayPrototypeFilter(
      s.encodings,
      (e) => e.rid !== undefined && ArrayPrototypeIncludes(layers, e.rid),
    );
    s.encodings = kept.length > 0 ? kept : [s.encodings[0]];
  }
  if (ArrayPrototypeSome(s.encodings, (e) => e.codec !== undefined)) {
    const negotiated = senderNegotiated(sender).codecs;
    if (negotiated.length > 0) {
      for (let i = 0; i < s.encodings.length; ++i) {
        const e = s.encodings[i];
        if (
          e.codec !== undefined &&
          !ArrayPrototypeSome(negotiated, (c) => codecMatches(c, e.codec))
        ) {
          delete e.codec;
        }
      }
    }
  }
}

// RFC 5761 section 4: with RTP and RTCP multiplexed (which WebRTC requires),
// payload types 64 to 95 would be taken for RTCP packet types, so an RTP
// m-section may not use them (InvalidAccessError, as browsers reject it).
function requirePayloadTypes(sdp) {
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (!StringPrototypeStartsWith(line, "m=")) continue;
    const fields = ArrayPrototypeFilter(
      StringPrototypeSplit(StringPrototypeSlice(line, 2), " "),
      (f) => f !== "",
    );
    if (fields.length < 4 || !StringPrototypeIncludes(fields[2], "RTP")) {
      continue;
    }
    for (let j = 3; j < fields.length; ++j) {
      const pt = NumberParseInt(fields[j], 10);
      if (pt >= 64 && pt <= 95) {
        throw new DOMException(
          `setRemoteDescription: payload type ${pt} collides with RTCP (RFC 5761)`,
          "InvalidAccessError",
        );
      }
    }
  }
}

// RFC 9429 section 5.8.3 (RFC 3264 section 6): each m-section of an answer
// has the media type of the offer's at the same index, known to this
// endpoint or not.
function requireAnswerMediaTypes(pc, sdp) {
  const offer = op_webrtc_pc_descriptions(pc[_rid]).pendingLocal;
  if (offer === null || offer === undefined) return;
  const offered = mediaSectionsOf(offer[1]);
  const answered = mediaSectionsOf(sdp);
  const n = MathMin(offered.length, answered.length);
  for (let i = 0; i < n; ++i) {
    if (answered[i].kind !== offered[i].kind) {
      throw new DOMException(
        `setRemoteDescription: the answer's m-section ${i} is '${
          answered[i].kind
        }', the offer's '${offered[i].kind}'`,
        "InvalidAccessError",
      );
    }
  }
}

// The simulcast encodings one sender may have.
const MAX_ENCODINGS = 16;
const RID_RE = new SafeRegExp("^[A-Za-z0-9]+$");

// webrtc-extensions RTCEncodingComplexity.
const ENCODING_COMPLEXITIES = [
  "low",
  "normal",
  "high",
  "very-high",
  "maximum",
];

// webrtc-svc section 6: the scalability modes.
const SCALABILITY_MODES = [
  "L1T1",
  "L1T2",
  "L1T3",
  "L2T1",
  "L2T1h",
  "L2T1_KEY",
  "L2T2",
  "L2T2h",
  "L2T2_KEY",
  "L2T2_KEY_SHIFT",
  "L2T3",
  "L2T3h",
  "L2T3_KEY",
  "L2T3_KEY_SHIFT",
  "L3T1",
  "L3T1h",
  "L3T1_KEY",
  "L3T2",
  "L3T2h",
  "L3T2_KEY",
  "L3T2_KEY_SHIFT",
  "L3T3",
  "L3T3h",
  "L3T3_KEY",
  "L3T3_KEY_SHIFT",
  "S2T1",
  "S2T1h",
  "S2T2",
  "S2T2h",
  "S2T3",
  "S2T3h",
  "S3T1",
  "S3T1h",
  "S3T2",
  "S3T2h",
  "S3T3",
  "S3T3h",
];

// The temporal layer count of a scalability mode ("L3T2_KEY" -> 2).
function temporalLayers(mode) {
  const i = StringPrototypeIndexOf(mode, "T");
  return i === -1 ? 1 : NumberParseInt(StringPrototypeSlice(mode, i + 1), 10);
}

// The scalability mode a sender reports once a codec is negotiated: what was
// asked for when the codec has it, else the codec's mode with the same
// temporal layers and one spatial layer (VP8 and H.264 have no spatial
// scalability, webrtc-svc section 4.2.1).
function negotiatedScalabilityMode(mode, mimeType) {
  const lower = StringPrototypeToLowerCase(mimeType);
  if (lower !== "video/vp8" && lower !== "video/h264") return mode;
  if (StringPrototypeStartsWith(mode, "L1T")) return mode;
  return `L1T${MathMin(temporalLayers(mode), 3)}`;
}

// An RTCRtpCodec dictionary (mimeType and clockRate required).
function convertCodec(codec, method) {
  if (codec === null || typeof codec !== "object") {
    throw new TypeError(`${method}: a codec must be an object`);
  }
  if (codec.mimeType === undefined || codec.clockRate === undefined) {
    throw new TypeError(`${method}: a codec needs mimeType and clockRate`);
  }
  const out = {
    mimeType: String(codec.mimeType),
    clockRate: webidl.converters["unsigned long"](codec.clockRate),
  };
  if (codec.channels !== undefined) {
    out.channels = webidl.converters["unsigned short"](codec.channels);
  }
  if (codec.sdpFmtpLine !== undefined) {
    out.sdpFmtpLine = String(codec.sdpFmtpLine);
  }
  return out;
}

// webrtc-pc 5.2 "codec dictionary match" (payload types aside).
function codecMatches(a, b) {
  return StringPrototypeToLowerCase(String(a.mimeType)) ===
      StringPrototypeToLowerCase(String(b.mimeType)) &&
    a.clockRate === b.clockRate &&
    (a.channels ?? null) === (b.channels ?? null) &&
    (a.sdpFmtpLine ?? null) === (b.sdpFmtpLine ?? null);
}

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
    let added = false;
    for (let j = 0; j < now.length; ++j) {
      if (!ArrayPrototypeIncludes(before.streams, now[j])) {
        streamRemoveRemote(now[j], rstate.track);
      }
    }
    for (let j = 0; j < before.streams.length; ++j) {
      if (!ArrayPrototypeIncludes(now, before.streams[j])) {
        streamAddRemote(before.streams[j], rstate.track);
        added = true;
      }
    }
    rstate.streams = before.streams;
    // The streams the rollback gave back are announced as new ones are.
    if (added) ArrayPrototypePush(revived, t);
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
    // wpt100:hdrext — webrtc-extensions RTCIceServer.credentialType and
    // RTCOAuthCredential (set the configuration, step 11.6).
    const credentialType = server.credentialType === undefined
      ? "password"
      : String(server.credentialType);
    if (credentialType !== "password" && credentialType !== "oauth") {
      throw new TypeError(
        `Failed to construct 'RTCPeerConnection': '${credentialType}' is not a valid value for 'credentialType'`,
      );
    }
    let credential = server.credential === undefined
      ? undefined
      : String(server.credential);
    if (credentialType === "oauth") {
      const c = server.credential;
      if (c !== null && typeof c === "object") {
        if (c.macKey === undefined || c.accessToken === undefined) {
          throw new TypeError(
            "Failed to construct 'RTCPeerConnection': an RTCOAuthCredential needs 'macKey' and 'accessToken'",
          );
        }
        credential = {
          macKey: String(c.macKey),
          accessToken: String(c.accessToken),
        };
      } else if (ArrayPrototypeSome(urls, isTurnUrl)) {
        throw new DOMException(
          "Failed to construct 'RTCPeerConnection': credentialType is 'oauth' but the credential is not an RTCOAuthCredential",
          "InvalidAccessError",
        );
      }
    }
    ArrayPrototypePush(out, {
      urls,
      username: server.username === undefined
        ? undefined
        : String(server.username),
      credential,
      credentialType,
    });
  }
  return out;
}

function isTurnUrl(url) {
  const u = StringPrototypeToLowerCase(url);
  return StringPrototypeStartsWith(u, "turn:") ||
    StringPrototypeStartsWith(u, "turns:");
}

// wpt100:hdrext — the ICE servers the engine gets: not the OAuth ones (RFC
// 7635 needs an ACCESS-TOKEN attribute the engine's TURN client doesn't
// send; they are kept in the configuration, unused).
function engineIceServers(servers) {
  const out = [];
  for (let i = 0; i < servers.length; ++i) {
    const s = servers[i];
    if (s.credentialType === "oauth") continue;
    ArrayPrototypePush(out, {
      urls: s.urls,
      username: s.username,
      credential: s.credential,
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
  // wpt100:hdrext — webrtc-extensions members.
  const rtpHeaderEncryptionPolicy = config.rtpHeaderEncryptionPolicy ===
      undefined
    ? "negotiate"
    : String(config.rtpHeaderEncryptionPolicy);
  if (
    !ArrayPrototypeIncludes(
      ["disable", "negotiate", "require"],
      rtpHeaderEncryptionPolicy,
    )
  ) {
    throw new TypeError(
      `${prefix}: '${rtpHeaderEncryptionPolicy}' is not a valid value for 'rtpHeaderEncryptionPolicy'`,
    );
  }
  const alwaysNegotiateDataChannels = !!config.alwaysNegotiateDataChannels;
  const targetLatency = config.targetLatency === undefined
    ? "lowest"
    : String(config.targetLatency);
  if (targetLatency !== "lowest" && targetLatency !== "none") {
    throw new TypeError(
      `${prefix}: '${targetLatency}' is not a valid value for 'targetLatency'`,
    );
  }
  // wpt100:identity — RTCConfiguration.peerIdentity (W3C WebRTC Identity
  // section 8.1): the target peer identity, a DOMString.
  const peerIdentity = config.peerIdentity === undefined
    ? null
    : webidl.converters.DOMString(config.peerIdentity, prefix, "peerIdentity");
  return {
    iceServers: convertIceServers(config.iceServers),
    iceTransportPolicy,
    bundlePolicy,
    rtcpMuxPolicy,
    iceCandidatePoolSize,
    certificates,
    network,
    rtpHeaderEncryptionPolicy,
    alwaysNegotiateDataChannels,
    targetLatency,
    peerIdentity,
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
      iceServers: engineIceServers(config.iceServers),
      iceTransportPolicy: config.iceTransportPolicy,
      bundlePolicy: config.bundlePolicy,
      certificates: ArrayPrototypeMap(
        config.certificates,
        (c) => c[_certRid],
      ),
      network: config.network === null ? null : config.network[_rid],
    });
    // wpt100:hdrext — rtpHeaderEncryptionPolicy, alwaysNegotiateDataChannels.
    op_webrtc_pc_set_negotiation_options(
      this[_rid],
      config.rtpHeaderEncryptionPolicy,
      config.alwaysNegotiateDataChannels,
    );
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
      // wpt100:transceiver: keys of the stopped transceivers that left
      // `transceivers` (the engine keeps them).
      retired: new SafeSet(),
      // wpt100:transceiver: [[LastCreatedOffer]] / [[LastCreatedAnswer]]
      // ({ sdp, generation }), valid while `generation` is unchanged.
      lastCreatedOffer: null,
      lastCreatedAnswer: null,
      generation: 0,
      // Remote MediaStreams by id.
      streams: new SafeMap(),
      localCandidates: [],
      // announcedKey()s of the gathered candidates whose icecandidate event
      // was dispatched: only those are in the local description.
      announced: new SafeSet(),
      // A rolled-back first offer discarded its gathering (discardGathering).
      gatheringDiscarded: false,
      // The gathering state the engine last reported.
      engineGatheringState: "new",
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
      const created = inTask(this, () => {
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
        const sdp = createOfferSdp(this, !!options?.iceRestart);
        state.lastCreatedOffer = { sdp, generation: state.generation };
        return { type: "offer", sdp };
      });
      // wpt100:identity — the a=identity assertion (06_identity.js).
      return identityHooks.withAssertion(this, created);
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
      const created = inTask(this, () => {
        const sdp = createAnswerSdp(this);
        this[_state].lastCreatedAnswer = {
          sdp,
          generation: this[_state].generation,
        };
        return { type: "answer", sdp };
      });
      // wpt100:identity — the a=identity assertion (06_identity.js).
      return identityHooks.withAssertion(this, created);
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
          // webrtc-pc 4.4.1.5: [[LastCreatedOffer]] / [[LastCreatedAnswer]]
          // while still valid, else a new one.
          const offer = type === "offer";
          sdp = lastCreated(this, offer);
          if (sdp === null) {
            // The engine compares a description with the last one it made.
            sdp = offer ? createOfferSdp(this, false) : createAnswerSdp(this);
            const created = { sdp, generation: this[_state].generation };
            if (offer) this[_state].lastCreatedOffer = created;
            else this[_state].lastCreatedAnswer = created;
          }
        }
        // wpt100:identity — the engine gets the description without the
        // a=identity line createOffer / createAnswer added (06_identity.js).
        op_webrtc_pc_set_local_description(
          this[_rid],
          type,
          identityHooks.engineSdp(this, sdp ?? ""),
        );
        const state = this[_state];
        if (type === "rollback") discardGathering(this);
        else if (state.gatheringDiscarded) resumeGathering(this);
        if (type === "answer" || type === "pranswer") {
          state.iceRole ??= "controlled";
          readChannelIds(this);
        }
        activate(this);
        descriptionApplied(this, true, type);
        syncTransceivers(this, { remote: false, type });
        if (type === "answer") headerExtensionsNegotiated(this, sdp, false);
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
          requireUniqueMsids(sdp);
          // wpt100:codecs
          requirePayloadTypes(sdp);
          // wpt100:hdrext
          requireFingerprint(sdp);
          requireCryptex(this, sdp);
          requireStableExtensionIds(this, sdp);
          if (type === "answer" || type === "pranswer") {
            // wpt100:codecs
            requireAnswerMediaTypes(this, sdp);
            // wpt100:hdrext
            requireAnswerDirections(this, sdp);
          }
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
        // The transceivers are in place (a remote offer's new ones included)
        // when signalingstatechange fires; track events come after it
        // (webrtc-pc 4.4.1.6).
        const trackEvents = syncTransceivers(this, { remote: true, type });
        // wpt100:hdrext
        if (type === "answer") headerExtensionsNegotiated(this, sdp, true);
        setSignalingState(this, nextSignalingState(now, false, type));
        if (type === "rollback") restoreRemoteTracks(this);
        fireTrackEvents(this, trackEvents);
        updateSctpTransport(this, type);
        setTransportStates(this);
        negotiationSettled(this);
      };
      // wpt100:identity — a=identity validation (06_identity.js): before
      // `apply` with a target peer identity, after it without.
      const applyRemote = identityHooks.withRemoteIdentity(
        this,
        type,
        description.sdp,
        apply,
      );
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
          () => inTask(this, applyRemote),
        );
      }
      return inTask(this, applyRemote);
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
      // webrtc-priority 4.2: RTCPriorityType priority = "low".
      priority: dict.priority === undefined
        ? "low"
        : webidl.converters.DOMString(dict.priority, prefix, "'priority'"),
    };
    if (PRIORITY_VALUES[init.priority] === undefined) {
      throw new TypeError(
        `${prefix}: '${init.priority}' is not a valid value for enumeration RTCPriorityType`,
      );
    }
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
      priority: PRIORITY_VALUES[init.priority],
    });
    const channel = new RTCDataChannel(illegalConstructorKey, this, {
      ...init,
      channel: created.channel,
      id: created.id ?? (init.negotiated ? init.id : null),
    });
    MapPrototypeSet(this[_channels], created.channel, channel);
    if (!this[_state].hadDataChannel) {
      this[_state].hadDataChannel = true;
      // A new offer would carry the data m= section: the last created offer
      // / answer are stale (webrtc-pc 4.4.1.5, wpt100:transceiver).
      invalidateCreated(this);
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
        if (s.credential !== undefined) {
          server.credential = typeof s.credential === "object"
            ? { ...s.credential }
            : s.credential;
        }
        server.credentialType = s.credentialType;
        return server;
      }),
      iceTransportPolicy: config.iceTransportPolicy,
      bundlePolicy: config.bundlePolicy,
      rtcpMuxPolicy: config.rtcpMuxPolicy,
      iceCandidatePoolSize: config.iceCandidatePoolSize,
      certificates: ArrayPrototypeSlice(config.certificates),
      // wpt100:hdrext
      rtpHeaderEncryptionPolicy: config.rtpHeaderEncryptionPolicy,
      alwaysNegotiateDataChannels: config.alwaysNegotiateDataChannels,
      targetLatency: config.targetLatency,
    };
    if (config.network !== null) out.denoNetwork = config.network;
    // wpt100:identity
    if (config.peerIdentity !== null) out.peerIdentity = config.peerIdentity;
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
    // wpt100:hdrext — webrtc-extensions: set only at construction.
    const policy = config.rtpHeaderEncryptionPolicy;
    const always = config.alwaysNegotiateDataChannels;
    if (
      policy !== current.rtpHeaderEncryptionPolicy ||
      always !== current.alwaysNegotiateDataChannels
    ) {
      throw new DOMException(
        `${prefix}: rtpHeaderEncryptionPolicy and alwaysNegotiateDataChannels can't change`,
        "InvalidModificationError",
      );
    }
    if (config.targetLatency !== current.targetLatency) {
      throw new TypeError(`${prefix}: targetLatency can't change`);
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
    // wpt100:identity — the target peer identity can't change (W3C WebRTC
    // 1.0's 2017 setConfiguration step; Firefox has it).
    if (
      config.peerIdentity !== null &&
      config.peerIdentity !== current.peerIdentity
    ) {
      throw new DOMException(
        `${prefix}: peerIdentity can't change`,
        "InvalidModificationError",
      );
    }
    // Validates the ICE server URLs (SyntaxError / InvalidAccessError) and
    // the net permission for them; they apply to the next gathering (an
    // ICE restart).
    op_webrtc_pc_set_ice_servers(this[_rid], {
      iceServers: engineIceServers(config.iceServers),
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
    config.peerIdentity = current.peerIdentity;
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
    // The next offer restarts ICE: the last created offer / answer are stale
    // (webrtc-pc 4.4.1.5, wpt100:transceiver).
    invalidateCreated(this);
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
      invalidateCreated(this);
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
    // A stopping transceiver is left alone too (WPT "Calling removeTrack on a
    // stopped transceiver should be a no-op"; w3c/webrtc-pc#2874).
    if (
      transceiver[_state].stopping || transceiver[_state].stopped ||
      sender[_state].track === null
    ) {
      return;
    }
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
    invalidateCreated(this);
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
      // A transferred channel closes in the realm it went to.
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, { type: "dcclose" });
        return;
      }
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
  // W3C "set the RTCSessionDescription" 4.4.1.6: an answer or pranswer (local
  // or remote) runs "update the data max message size" with the remote
  // description of that moment, a renegotiation included.
  if (state.sctp !== null && (type === "answer" || type === "pranswer")) {
    const remote = pc.remoteDescription;
    if (remote !== null) {
      state.sctp[_maxMessageSize] = maxMessageSizeOf(remote.sdp) ?? 65536;
    }
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

// wpt100:hdrext — RFC 8827 section 6.5 / RFC 8826 section 4.3.1: no SDES,
// DTLS-SRTP only; a description whose media has no DTLS fingerprint is
// refused (RFC 9429 section 5.8) as InvalidAccessError.
function requireFingerprint(sdp) {
  if (StringPrototypeIncludes(sdp, "a=fingerprint:")) return;
  const sections = mediaSectionsOf(sdp);
  if (ArrayPrototypeSome(sections, (m) => !m.rejected)) {
    throw new DOMException(
      "setRemoteDescription: the description has no DTLS fingerprint (SDES is not supported)",
      "InvalidAccessError",
    );
  }
}

// Whether `a=cryptex` (RFC 9335) applies to every RTP section of a
// description: at the session level or on each live audio / video section;
// `null` when it has no such section.
function cryptexOf(sdp) {
  const parts = StringPrototypeSplit(sdp, "\nm=");
  const lines = (part) =>
    ArrayPrototypeMap(StringPrototypeSplit(part, "\n"), StringPrototypeTrim);
  const session = ArrayPrototypeIncludes(lines(parts[0]), "a=cryptex");
  let rtp = 0;
  let all = true;
  for (let i = 1; i < parts.length; ++i) {
    const section = lines(parts[i]);
    const fields = StringPrototypeSplit(section[0], " ");
    if (fields[0] !== "audio" && fields[0] !== "video") continue;
    if (
      fields[1] === "0" && !ArrayPrototypeIncludes(section, "a=bundle-only")
    ) {
      continue;
    }
    rtp++;
    if (!ArrayPrototypeIncludes(section, "a=cryptex")) all = false;
  }
  if (rtp === 0) return null;
  return session || all;
}

// webrtc-extensions: with rtpHeaderEncryptionPolicy "require", a remote
// description with RTP media that doesn't signal cryptex is refused.
function requireCryptex(pc, sdp) {
  if (pc[_state].config.rtpHeaderEncryptionPolicy !== "require") return;
  if (cryptexOf(sdp) === false) {
    throw new DOMException(
      "setRemoteDescription: rtpHeaderEncryptionPolicy is 'require', but the description doesn't signal a=cryptex",
      "InvalidAccessError",
    );
  }
}

// RFC 8285 section 6: once negotiated, a media section's extension ids are
// not remapped. A remote description that gives a URI another id, or an id
// another URI, than the current descriptions of the same mid is refused.
function requireStableExtensionIds(pc, sdp) {
  const incoming = extmapsOf(sdp);
  const previous = [];
  const remote = pc.currentRemoteDescription;
  const local = pc.currentLocalDescription;
  if (remote !== null) ArrayPrototypePush(previous, extmapsOf(remote.sdp));
  if (local !== null) ArrayPrototypePush(previous, extmapsOf(local.sdp));
  MapPrototypeForEach(incoming, (section, mid) => {
    if (section.rejected) return;
    for (let p = 0; p < previous.length; ++p) {
      const before = MapPrototypeGet(previous[p], mid);
      if (before === undefined || before.rejected) continue;
      for (let i = 0; i < section.extensions.length; ++i) {
        const e = section.extensions[i];
        const clash = ArrayPrototypeSome(
          before.extensions,
          (b) => (b.uri === e.uri) !== (b.id === e.id),
        );
        if (clash) {
          throw new DOMException(
            `setRemoteDescription: the extension '${e.uri}' of mid '${mid}' was remapped`,
            "InvalidAccessError",
          );
        }
      }
    }
  });
}

// RFC 3264 section 6.1 / RFC 9429 section 5.10: an answer's direction can't
// exceed what the offer allows (it sends only what the offerer receives,
// receives only what it sends).
function requireAnswerDirections(pc, sdp) {
  const offer = pc.pendingLocalDescription;
  if (offer === null) return;
  const offered = mediaSectionsOf(offer.sdp);
  const answered = mediaSectionsOf(sdp);
  for (let i = 0; i < answered.length; ++i) {
    const a = answered[i];
    if (a.rejected || a.mid === null) continue;
    const o = ArrayPrototypeFind(offered, (m) => m.mid === a.mid);
    if (o === undefined || o.rejected) continue;
    const sends = (d) => d === "sendrecv" || d === "sendonly";
    const receives = (d) => d === "sendrecv" || d === "recvonly";
    if (
      (sends(a.direction) && !receives(o.direction)) ||
      (receives(a.direction) && !sends(o.direction))
    ) {
      throw new DOMException(
        `setRemoteDescription: the answer's '${a.direction}' for mid '${a.mid}' exceeds the offer's '${o.direction}'`,
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
  // wpt100:codecs: video keeps retransmission and FEC, which a preference
  // list leaves out unless it lists them (media.rs track_preferences()).
  if (
    StringPrototypeStartsWith(
      StringPrototypeToLowerCase(codec.mimeType),
      "video/",
    )
  ) {
    ArrayPrototypePush(
      out,
      { mimeType: "video/rtx", clockRate: 90000, channels: null },
      { mimeType: "video/ulpfec", clockRate: 90000, channels: null },
    );
  }
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
  invalidateCreated(pc);
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
  if (state.closed) return [];
  const infos = op_webrtc_pc_transceivers(pc[_rid]);
  const known = new SafeMap();
  for (let i = 0; i < state.transceivers.length; ++i) {
    const t = state.transceivers[i];
    MapPrototypeSet(known, t[_key], t);
  }
  const seen = new SafeSet();
  const next = [];
  const settled = phase !== null &&
    (phase.type === "answer" || phase.type === "rollback");
  const trackEvents = [];
  for (let i = 0; i < infos.length; ++i) {
    const info = infos[i];
    if (SetPrototypeHas(state.retired, info.key)) continue;
    let t = MapPrototypeGet(known, info.key);
    if (t === undefined) {
      if (info.kind !== "audio" && info.kind !== "video") continue;
      // A remote offer's new transceiver is recvonly (webrtc-pc 4.4.1.6,
      // "create an RTCRtpTransceiver ... direction recvonly"), whatever the
      // engine narrowed its own to; the answer narrows it (createAnswerSdp).
      t = new RTCRtpTransceiver(illegalConstructorKey, pc, {
        key: info.key,
        kind: info.kind,
        mid: info.mid,
        direction: "recvonly",
      });
      t[_state].sender[_state].encodings = info.kind === "video"
        ? [{ active: true, scaleResolutionDownBy: 1 }]
        : [{ active: true }];
    }
    SetPrototypeAdd(seen, info.key);
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
    // webrtc-pc 4.4.1.6: a media description the remote side rejected stops
    // its transceiver at once (an offer: [[Stopped]] while the answer is
    // pending; WPT "Remotely stopped transceiver goes directly to stopped").
    // In an answer this is also how a stopping transceiver becomes stopped:
    // the offer that followed stop() rejected its m-section, and so does the
    // answer. A stopping transceiver whose m-section the answer did not
    // reject (stopped after the offer came in: answered inactive) stays
    // stopping until the next offer.
    if (
      phase !== null && phase.type !== "rollback" && remote !== null &&
      remote.rejected && !ts.stopped
    ) {
      stopTransceiverNow(pc, t, true);
    }
    if (
      phase !== null && phase.remote && phase.type !== "rollback" &&
      remote !== null && !ts.stopped
    ) {
      // A stopping transceiver still takes part (WPT checkStopAfterCreateOffer:
      // the answer to an offer made before stop() fires its track event).
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
        let added = false;
        for (let j = 0; j < old.length; ++j) {
          if (!ArrayPrototypeIncludes(streams, old[j])) {
            streamRemoveRemote(old[j], track);
          }
        }
        for (let j = 0; j < streams.length; ++j) {
          if (!ArrayPrototypeIncludes(old, streams[j])) {
            streamAddRemote(streams[j], track);
            added = true;
          }
        }
        rstate.streams = streams;
        // A track event when the track starts being received, and when the
        // description adds streams to it ("set the associated remote
        // streams" grew addList).
        if (!rstate.receiving || added) {
          rstate.receiving = true;
          ArrayPrototypePush(trackEvents, [t, streams]);
        }
      } else if (receiver[_state].receiving && !remoteSends) {
        removeRemoteTrack(t);
      }
    }
    if (phase !== null) {
      if (phase.type === "rollback") {
        ts.transport = ts.stableTransport;
      } else if (
        ts.mid !== null && (!phase.remote || phase.type !== "offer")
      ) {
        ts.transport = true;
      }
      if (settled) ts.stableTransport = ts.transport;
      // wpt100:codecs
      negotiateEncodings(t, remote, phase);
    }
    if (settled && phase.type === "answer" && ts.stopped) {
      // Negotiated stopped: out of the set, without a mid (webrtc-pc 4.4.1.6).
      retireTransceiver(pc, t);
      continue;
    }
    ArrayPrototypePush(next, t);
  }
  // Transceivers the engine no longer has: a rolled-back remote offer created
  // them, and nothing kept them (webrtc-pc 4.4.1.6 rollback: "stop the
  // RTCRtpTransceiver ... and remove it from the set of transceivers"). Their
  // receiver track ends (WPT "Rollback when removing transceiver does end the
  // track").
  for (let i = 0; i < state.transceivers.length; ++i) {
    const t = state.transceivers[i];
    if (SetPrototypeHas(seen, t[_key])) continue;
    stopTransceiverNow(pc, t, false);
    retireTransceiver(pc, t);
  }
  state.transceivers = next;
  return trackEvents;
}

// The track events a remote description queued (webrtc-pc 4.4.1.6: after
// signalingstatechange).
function fireTrackEvents(pc, trackEvents) {
  if (trackEvents === undefined) return;
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
  pushHeaderExtensions(pc, true);
  const sdp = op_webrtc_pc_create_offer(pc[_rid], restart);
  state.configRestart = false;
  return sdp;
}

// RFC 8830 section 2: one MediaStreamTrack is in one m-section, so an
// `a=msid:<stream> <track>` value two m-sections both carry is an invalid
// description, which setRemoteDescription rejects with an OperationError.
// (An msid with no track part, and rejected sections, say nothing here.)
function requireUniqueMsids(sdp) {
  const seen = new SafeSet();
  let mine = null;
  const flush = () => {
    if (mine === null) return;
    const values = ArrayFrom(new SafeSet(mine));
    for (let i = 0; i < values.length; ++i) {
      if (SetPrototypeHas(seen, values[i])) {
        throw new DOMException(
          `setRemoteDescription: the msid '${values[i]}' is in two media sections`,
          "OperationError",
        );
      }
      SetPrototypeAdd(seen, values[i]);
    }
  };
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (StringPrototypeStartsWith(line, "m=")) {
      flush();
      const parts = StringPrototypeSplit(line, " ");
      mine = parts[1] === "0" ? null : [];
    } else if (mine !== null && StringPrototypeStartsWith(line, "a=msid:")) {
      const value = StringPrototypeTrim(StringPrototypeSlice(line, 7));
      const parts = ArrayPrototypeFilter(
        StringPrototypeSplit(value, " "),
        (part) => part !== "",
      );
      if (parts.length >= 2) {
        ArrayPrototypePush(mine, `${parts[0]} ${parts[1]}`);
      }
    }
  }
  flush();
}

// createAnswer's SDP. JSEP (RFC 9429 5.3.1): each answered m= section's
// direction is the transceiver's own direction intersected with the reverse
// of the offered one, and a stopping transceiver answers inactive (it is
// rejected by the next offer). The engine writes its own direction into the
// answer, and only half narrows it when it applies the offer (a sendonly offer
// left a sendrecv transceiver answering sendrecv; a remote offer's transceiver
// answered sendonly with nothing to send), so the directions are set here.
function createAnswerSdp(pc) {
  const state = pc[_state];
  const byKey = new SafeMap();
  for (let i = 0; i < state.transceivers.length; ++i) {
    const t = state.transceivers[i];
    MapPrototypeSet(byKey, t[_key], t);
  }
  const infos = op_webrtc_pc_transceivers(pc[_rid]);
  for (let i = 0; i < infos.length; ++i) {
    const info = infos[i];
    const remote = info.remote;
    if (remote === null || remote.rejected) continue;
    const t = MapPrototypeGet(byKey, info.key);
    if (t === undefined) continue;
    const ts = t[_state];
    if (ts.stopped) continue;
    const want = ts.stopping
      ? "inactive"
      : intersectDirections(ts.direction, reverseDirection(remote.direction));
    if (info.direction === want) continue;
    try {
      op_webrtc_transceiver_set_direction(pc[_rid], t[_key], want);
    } catch {
      // The engine's transceiver is gone.
    }
  }
  // wpt100:hdrext
  pushHeaderExtensions(pc, false);
  return op_webrtc_pc_create_answer(pc[_rid]);
}

function intersectDirections(a, b) {
  const send = (a === "sendrecv" || a === "sendonly") &&
    (b === "sendrecv" || b === "sendonly");
  const recv = (a === "sendrecv" || a === "recvonly") &&
    (b === "sendrecv" || b === "recvonly");
  return send
    ? (recv ? "sendrecv" : "sendonly")
    : (recv ? "recvonly" : "inactive");
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

// [[LastCreatedOffer]] / [[LastCreatedAnswer]]'s SDP if nothing that goes into
// a description changed since it was created (invalidateCreated), else null.
function lastCreated(pc, offer) {
  const state = pc[_state];
  const last = offer ? state.lastCreatedOffer : state.lastCreatedAnswer;
  return last !== null && last.generation === state.generation
    ? last.sdp
    : null;
}

// A change that a new offer or answer would show (a transceiver, a direction,
// a track, codec preferences, an applied description): the last created ones
// are stale.
function invalidateCreated(pc) {
  pc[_state].generation++;
}

// Bookkeeping when a description has been applied: the stream ids each
// sender has in the local description (what `a=msid` carries), the type of
// the current local description, and restartIce()'s credentials, which a
// completed negotiation with fresh ones retires.
function descriptionApplied(pc, local, type) {
  const state = pc[_state];
  invalidateCreated(pc);
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
    const event = events[from];
    if (
      event.type === "icegatheringstatechange" && event.state === "complete" &&
      !event.endOfCandidates && state.iceGatheringState === "gathering"
    ) {
      // W3C webrtc-pc 5.6 (as amended by w3c/webrtc-pc#2894): gathering
      // that finishes queues two tasks. The first fires `icecandidate` with
      // an empty candidate string for the media description (its
      // end-of-candidates), the second the gathering state changes and the
      // global null candidate. The same event comes back as the next task.
      event.endOfCandidates = true;
      ArrayPrototypeSplice(events, from + 1, 0, event);
      fireEndOfCandidates(pc);
    } else {
      dispatchEngineEvent(pc, event);
    }
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

// setLocalDescription(rollback) of the first offer leaves no local
// description: the ICE transports that offer created are discarded, and with
// no transport the ICE gathering state is "new" again (W3C webrtc-pc 4.4.1.6
// rollback, 4.4.4 "update the ICE gathering state"). Gathering still running
// for it is not reported any more.
function discardGathering(pc) {
  const state = pc[_state];
  if (
    pc.currentLocalDescription !== null || pc.pendingLocalDescription !== null
  ) {
    return;
  }
  if (state.iceGatheringState === "new") return;
  state.gatheringDiscarded = true;
  state.iceGatheringState = "new";
  setTransportStates(pc);
  fire(pc, new Event("icegatheringstatechange"));
}

// The next local description after such a rollback: the engine keeps the
// sockets and candidates it gathered (it gathers once per connection), so
// the gathering state goes where the engine's is again, through "gathering"
// (and, if the engine had finished, "complete" with its null candidate, in a
// task of its own).
function resumeGathering(pc) {
  const state = pc[_state];
  state.gatheringDiscarded = false;
  const engine = state.engineGatheringState;
  if (engine === "new" || state.iceGatheringState !== "new") return;
  state.iceGatheringState = "gathering";
  setTransportStates(pc);
  fire(pc, new Event("icegatheringstatechange"));
  if (engine !== "complete") return;
  PromisePrototypeThen(op_void_async_deferred(), () => {
    if (
      state.closed || state.gatheringDiscarded ||
      state.iceGatheringState !== "gathering"
    ) {
      return;
    }
    state.iceGatheringState = "complete";
    setTransportStates(pc);
    fire(pc, new Event("icegatheringstatechange"));
    fire(
      pc,
      new RTCPeerConnectionIceEvent("icecandidate", { candidate: null }),
    );
  });
}

// The STUN / TURN server URL an icecandidateerror names: the URL as the
// ICE agent uses it, so a `turns:` URL carries its implied TCP transport
// (RFC 7065 section 3: TURN over TLS runs over TCP).
function iceErrorUrl(url) {
  if (
    typeof url === "string" && StringPrototypeStartsWith(url, "turns:") &&
    !StringPrototypeIncludes(url, "?")
  ) {
    return `${url}?transport=tcp`;
  }
  return url;
}

// The `icecandidate` event that ends a media description's candidates: an
// RTCIceCandidate whose candidate is "" (W3C webrtc-pc 5.6).
function fireEndOfCandidates(pc) {
  const state = pc[_state];
  // The last gathered candidate is of the generation that just ended.
  const first = state.localCandidates[state.localCandidates.length - 1];
  let sdpMid = first?.sdpMid ?? null;
  if (sdpMid === null) {
    const local = pc.localDescription;
    sdpMid = local === null ? null : midsOf(local.sdp)[0] ?? null;
  }
  const candidate = new RTCIceCandidate({
    candidate: "",
    sdpMid,
    sdpMLineIndex: first?.sdpMLineIndex ?? 0,
    usernameFragment: first?.usernameFragment ?? null,
  });
  fire(
    pc,
    new RTCPeerConnectionIceEvent("icecandidate", { candidate, url: null }),
  );
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
      SetPrototypeAdd(
        state.announced,
        announcedKey(event.usernameFragment ?? null, event.candidate),
      );
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
          // No local address was used (the server's name didn't resolve, or
          // no socket could reach it): port 0 with the null address, as
          // browsers report it.
          port: event.port ?? (event.address == null ? 0 : null),
          url: iceErrorUrl(event.url),
          errorCode: event.errorCode,
          errorText: event.errorText,
        }),
      );
      break;
    case "signalingstatechange":
      // The JS side sets the signaling state as each description is applied.
      break;
    case "icegatheringstatechange":
      state.engineGatheringState = event.state;
      // A rolled-back first offer discarded what was being gathered.
      if (state.gatheringDiscarded) break;
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
      // wpt100:mediasink: a packet still in flight after the remote side
      // stopped sending (by negotiation) leaves the track muted; the next
      // packet asks again.
      if (!receiver[_state].receiving) {
        rearmUnmute(receiver);
        break;
      }
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
    // wpt100:mediasink
    case "trackmute": {
      // The receiver's RTP stopped (media_sink.rs's SILENCE).
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      const receiver = t[_state].receiver;
      setMuted(receiver[_state].track, true);
      for (let i = 0; i < receiver[_state].clones.length; ++i) {
        setMuted(receiver[_state].clones[i], true);
      }
      break;
    }
    case "trackresize": {
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      t[_state].receiver[_frameSize] = {
        width: event.width,
        height: event.height,
      };
      break;
    }
    case "mediaframe": {
      const t = transceiverFor(pc, event.transceiver);
      if (t === undefined) break;
      deliverFrame(t[_state].receiver, event);
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
        // Opened by the remote peer: announced with `datachannel`. Its
        // readyState is already "open" there (so it can send), and `open`
        // follows in a task of its own (W3C "announce the channel as open"):
        // a channel closed or transferred in the `datachannel` handler
        // reports no `open` here.
        channel = new RTCDataChannel(illegalConstructorKey, pc, {
          channel: event.channel,
          id: event.id,
          label: event.label,
          protocol: event.protocol,
          ordered: event.ordered,
          maxPacketLifeTime: event.maxPacketLifeTime ?? null,
          maxRetransmits: event.maxRetransmits ?? null,
          negotiated: event.negotiated,
          priority: priorityFromDcep(event.priority),
          readyState: "open",
        });
        MapPrototypeSet(pc[_channels], event.channel, channel);
        fire(pc, new RTCDataChannelEvent("datachannel", { channel }));
        announceOpen(pc, channel, event);
        break;
      }
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
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
      if (channel === undefined) break;
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
        break;
      }
      if (channel[_state].readyState === "closed") break;
      channel[_state].readyState = "closing";
      fire(channel, new Event("closing"));
      break;
    }
    case "dcclose": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      MapPrototypeDelete(pc[_channels], event.channel);
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
        break;
      }
      if (channel[_state].readyState === "closed") break;
      channel[_state].readyState = "closed";
      fire(channel, new Event("close"));
      break;
    }
    case "dcerror": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
        break;
      }
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
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
        break;
      }
      channelSent(channel, event.bytes);
      break;
    }
    case "dcmessage": {
      const channel = channelFor(pc, event);
      if (channel === undefined) break;
      if (channel[_transferPort] !== undefined) {
        loadTransfer().forwardEvent(channel, event);
        break;
      }
      deliverChannelMessage(channel, event.text, event.data);
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
  closeAllFrameReaders(receiver);
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

// readFrames(track): the frames a remote track receives, put back together
// from RTP, still encoded (there is no decoder), as a stream. Like readRtp,
// it doesn't wait: a frame that arrives while `highWaterMark` frames (64)
// are queued is dropped. A disabled track delivers none; the stream closes
// when the track ends. This is what the WPT runner's media elements play.
const _frameReaders = Symbol("[[frameReaders]]");
const _frameSize = Symbol("[[frameSize]]");

function readFrames(track, options = undefined) {
  if (!ObjectPrototypeIsPrototypeOf(MediaStreamTrackPrototype, track)) {
    throw new TypeError("readFrames: expected a MediaStreamTrack");
  }
  const receiver = track[_receiver];
  if (receiver === null) {
    throw new TypeError(
      "readFrames: expected a remote track (an RTCRtpReceiver's)",
    );
  }
  const highWaterMark = options?.highWaterMark === undefined
    ? 64
    : webidl.converters["unsigned long"](options.highWaterMark);
  const pc = receiver[_pc];
  let entry;
  return new streams.ReadableStream({
    start(controller) {
      entry = { track, controller };
      if (pc[_state].closed || track[_trackState].readyState === "ended") {
        controller.close();
        return;
      }
      receiver[_frameReaders] ??= new SafeSet();
      SetPrototypeAdd(receiver[_frameReaders], entry);
      syncFrameSink(receiver, true);
    },
    cancel() {
      const readers = receiver[_frameReaders];
      if (readers === undefined) return;
      SetPrototypeDelete(readers, entry);
      syncFrameSink(receiver, false);
    },
  }, { highWaterMark, size: () => 1 });
}

// Tells the engine whether the receiver's frames are wanted; a new reader
// of a video track asks the sender for a key frame, which it needs first.
function syncFrameSink(receiver, started) {
  const pc = receiver[_pc];
  if (pc[_state].closed) return;
  const on = (receiver[_frameReaders]?.size ?? 0) > 0;
  const key = receiver[_transceiver][_key];
  try {
    op_webrtc_receiver_read_frames(pc[_rid], key, on);
    if (started && on && receiver[_state].track[_trackState].kind === "video") {
      op_webrtc_receiver_request_key_frame(pc[_rid], key);
    }
  } catch {
    // The transceiver is gone.
  }
}

function deliverFrame(receiver, event) {
  const readers = receiver[_frameReaders];
  if (readers === undefined || readers.size === 0) return;
  const timestamp = performance.now();
  let first = true;
  SetPrototypeForEach(readers, (entry) => {
    const state = entry.track[_trackState];
    if (state.readyState !== "live" || !state.enabled) return;
    if (entry.controller.desiredSize <= 0) return;
    // Every reader gets its own copy but the first.
    entry.controller.enqueue({
      data: first ? event.data : TypedArrayPrototypeSlice(event.data),
      codec: event.mimeType,
      keyFrame: event.keyFrame,
      width: event.width ?? null,
      height: event.height ?? null,
      rtpTimestamp: event.rtpTimestamp,
      timestamp,
    });
    first = false;
  });
}

// The track ended: its readers' streams close.
function closeFrameReaders(track) {
  const receiver = track[_receiver];
  const readers = receiver?.[_frameReaders];
  if (readers === undefined) return;
  SetPrototypeForEach(readers, (entry) => {
    if (entry.track !== track) return;
    SetPrototypeDelete(readers, entry);
    try {
      entry.controller.close();
    } catch {
      // Already closed.
    }
  });
  syncFrameSink(receiver, false);
}

function closeAllFrameReaders(receiver) {
  const readers = receiver[_frameReaders];
  if (readers === undefined) return;
  receiver[_frameReaders] = undefined;
  SetPrototypeForEach(readers, (entry) => {
    try {
      entry.controller.close();
    } catch {
      // Already closed.
    }
  });
  syncFrameSink(receiver, false);
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
  // wpt100:mediasink
  readFrames,
  requestKeyFrame,
  exportCertificate,
  importCertificate,
  setLogLevel,
});

// wpt100:identity — WebRTC Identity (06_identity.js): the RTCPeerConnection
// members, RTCIdentityAssertion, MediaStreamTrack's isolation attributes and
// the hooks createOffer / createAnswer / setRemoteDescription call.
const identityHooks = core.loadExtScript("ext:deno_webrtc/06_identity.js")
  .installIdentity({
    _rid,
    _state,
    brandError,
    defineHandler,
    invalidState,
    MediaStreamTrackPrototype,
    RTCError,
    RTCPeerConnectionPrototype,
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
  // wpt100:identity
  RTCIdentityAssertion: identityHooks.RTCIdentityAssertion,
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
  // wpt100:S2 for 98_global_scope_shared.js (RTCCertificate deserialization)
  // and 05_transfer.js (transferred RTCDataChannels).
  deserializeCertificate,
  dataChannelInternals: {
    illegalConstructorKey,
    _channel,
    _init,
    _pc,
    _proxyPort,
    _rid,
    _state,
    _transferPort,
    RTCDataChannel,
    RTCError,
    RTCErrorEvent,
    channelSent,
    deliverChannelMessage,
    failChannel,
    fire,
  },
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
