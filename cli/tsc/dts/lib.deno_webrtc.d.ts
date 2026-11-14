// Copyright 2018-2026 the Deno authors. MIT license.

// Documentation partially adapted from [MDN](https://developer.mozilla.org/),
// by Mozilla Contributors, which is licensed under CC-BY-SA 2.5.

// deno-lint-ignore-file no-explicit-any no-var

/// <reference no-default-lib="true" />
/// <reference lib="esnext" />

/**
 * The type of a session description: an offer, a provisional or final
 * answer, or a rollback to the last stable state.
 *
 * @category WebRTC
 */
type RTCSdpType = "answer" | "offer" | "pranswer" | "rollback";

/**
 * Where the connection is in the offer/answer exchange.
 *
 * @category WebRTC
 */
type RTCSignalingState =
  | "closed"
  | "have-local-offer"
  | "have-local-pranswer"
  | "have-remote-offer"
  | "have-remote-pranswer"
  | "stable";

/**
 * The aggregate state of the connection's ICE and DTLS transports.
 *
 * @category WebRTC
 */
type RTCPeerConnectionState =
  | "closed"
  | "connected"
  | "connecting"
  | "disconnected"
  | "failed"
  | "new";

/**
 * The state of the connection's ICE agent.
 *
 * @category WebRTC
 */
type RTCIceConnectionState =
  | "checking"
  | "closed"
  | "completed"
  | "connected"
  | "disconnected"
  | "failed"
  | "new";

/**
 * Whether the connection is still gathering local ICE candidates.
 *
 * @category WebRTC
 */
type RTCIceGatheringState = "complete" | "gathering" | "new";

/**
 * The state of an `RTCDataChannel`.
 *
 * @category WebRTC
 */
type RTCDataChannelState = "closed" | "closing" | "connecting" | "open";

/**
 * Which local candidates are signalled: all of them, or only TURN relay
 * candidates.
 *
 * @category WebRTC
 */
type RTCIceTransportPolicy = "all" | "relay";

/**
 * How media sections are bundled onto transports. Accepted for
 * compatibility; data channels always use one transport.
 *
 * @category WebRTC
 */
type RTCBundlePolicy = "balanced" | "max-bundle" | "max-compat";

/**
 * RTCP multiplexing; only `"require"` is supported, as in browsers.
 *
 * @category WebRTC
 */
type RTCRtcpMuxPolicy = "require";

/**
 * The transport protocol of an ICE candidate.
 *
 * @category WebRTC
 */
type RTCIceProtocol = "tcp" | "udp";

/**
 * The kind of an ICE candidate: a local interface address (`host`), the
 * address a STUN server saw (`srflx`), one learned from a peer's checks
 * (`prflx`), or a TURN relay address (`relay`).
 *
 * @category WebRTC
 */
type RTCIceCandidateType = "host" | "prflx" | "relay" | "srflx";

/**
 * The TCP role of a TCP ICE candidate.
 *
 * @category WebRTC
 */
type RTCIceTcpCandidateType = "active" | "passive" | "so";

/**
 * The ICE component a candidate belongs to.
 *
 * @category WebRTC
 */
type RTCIceComponent = "rtcp" | "rtp";

/**
 * The state of the connection's SCTP transport.
 *
 * @category WebRTC
 */
type RTCSctpTransportState = "closed" | "connected" | "connecting";

/**
 * The WebRTC-specific error kinds an `RTCError` reports.
 *
 * @category WebRTC
 */
type RTCErrorDetailType =
  | "data-channel-failure"
  | "dtls-failure"
  | "fingerprint-failure"
  | "hardware-encoder-error"
  | "hardware-encoder-not-available"
  | "sctp-failure"
  | "sdp-syntax-error";

/**
 * A STUN or TURN server for ICE. `urls` takes `stun:`, `turn:` and `turns:`
 * URLs (`?transport=udp|tcp` for TURN); TURN servers need `username` and
 * `credential`. The server's host and port need net permission.
 *
 * @category WebRTC
 */
interface RTCIceServer {
  urls: string | string[];
  username?: string;
  credential?: string;
}

/**
 * Options for `new RTCPeerConnection()`. Without `certificates`, each
 * connection generates its own ECDSA P-256 certificate.
 *
 * @category WebRTC
 */
interface RTCConfiguration {
  iceServers?: RTCIceServer[];
  iceTransportPolicy?: RTCIceTransportPolicy;
  bundlePolicy?: RTCBundlePolicy;
  rtcpMuxPolicy?: RTCRtcpMuxPolicy;
  iceCandidatePoolSize?: number;
  certificates?: RTCCertificate[];
}

/**
 * A certificate fingerprint, as `RTCCertificate.getFingerprints()` and the
 * SDP's `a=fingerprint` carry it.
 *
 * @category WebRTC
 */
interface RTCDtlsFingerprint {
  algorithm?: string;
  value?: string;
}

/**
 * A DTLS certificate and key for `RTCConfiguration.certificates`, from
 * `RTCPeerConnection.generateCertificate()` (ECDSA P-256 or
 * RSASSA-PKCS1-v1_5). Reusing one keeps the connection's fingerprint stable;
 * `Deno.webrtc.exportCertificate()` keeps it across restarts.
 *
 * @category WebRTC
 */
interface RTCCertificate {
  /** When the certificate stops being usable, in ms since the epoch. */
  readonly expires: number;
  getFingerprints(): RTCDtlsFingerprint[];
}

/**
 * A DTLS certificate (not constructible; see
 * `RTCPeerConnection.generateCertificate()`).
 *
 * @category WebRTC
 */
declare var RTCCertificate: {
  readonly prototype: RTCCertificate;
  new (): never;
};

/**
 * The algorithm for `RTCPeerConnection.generateCertificate()`:
 * `{ name: "ECDSA", namedCurve: "P-256" }` or `{ name: "RSASSA-PKCS1-v1_5",
 * modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash:
 * "SHA-256" }`, with an optional `expires` in milliseconds (30 days by
 * default).
 *
 * @category WebRTC
 */
interface RTCCertificateKeygenAlgorithm {
  name: string;
  namedCurve?: string;
  modulusLength?: number;
  publicExponent?: Uint8Array;
  hash?: string | { name: string };
  expires?: number;
}

/**
 * The success callback of the legacy `createOffer` / `createAnswer` form.
 *
 * @category WebRTC
 */
type RTCSessionDescriptionCallback = (
  description: RTCSessionDescriptionInit,
) => void;

/**
 * The failure callback of the legacy callback forms.
 *
 * @category WebRTC
 */
type RTCPeerConnectionErrorCallback = (error: DOMException) => void;

/**
 * Which way a transceiver sends and receives media.
 *
 * @category WebRTC
 */
type RTCRtpTransceiverDirection =
  | "inactive"
  | "recvonly"
  | "sendonly"
  | "sendrecv"
  | "stopped";

/**
 * The state of a DTLS transport.
 *
 * @category WebRTC
 */
type RTCDtlsTransportState =
  | "closed"
  | "connected"
  | "connecting"
  | "failed"
  | "new";

/**
 * The state of an ICE transport.
 *
 * @category WebRTC
 */
type RTCIceTransportState =
  | "checking"
  | "closed"
  | "completed"
  | "connected"
  | "disconnected"
  | "failed"
  | "new";

/**
 * The ICE gathering state of an ICE transport.
 *
 * @category WebRTC
 */
type RTCIceGathererState = "complete" | "gathering" | "new";

/**
 * Whether this side controls ICE nomination.
 *
 * @category WebRTC
 */
type RTCIceRole = "controlled" | "controlling" | "unknown";

/**
 * Whether a media track is live or has ended.
 *
 * @category WebRTC
 */
type MediaStreamTrackState = "ended" | "live";

/**
 * A codec, as `RTCRtpSender.getCapabilities()` lists it and
 * `setCodecPreferences()` takes it.
 *
 * @category WebRTC
 */
interface RTCRtpCodec {
  mimeType: string;
  clockRate: number;
  channels?: number;
  sdpFmtpLine?: string;
}

/**
 * A negotiated codec with its payload type.
 *
 * @category WebRTC
 */
interface RTCRtpCodecParameters extends RTCRtpCodec {
  payloadType: number;
}

/**
 * An RTP header extension the media stack supports.
 *
 * @category WebRTC
 */
interface RTCRtpHeaderExtensionCapability {
  uri: string;
}

/**
 * A negotiated RTP header extension.
 *
 * @category WebRTC
 */
interface RTCRtpHeaderExtensionParameters {
  uri: string;
  id: number;
  encrypted?: boolean;
}

/**
 * What `RTCRtpSender.getCapabilities()` / `RTCRtpReceiver.getCapabilities()`
 * return: Opus, G.722, PCMU, PCMA, VP8, VP9, H.264, AV1 and H.265 with RTX
 * and FEC.
 *
 * @category WebRTC
 */
interface RTCRtpCapabilities {
  codecs: RTCRtpCodec[];
  headerExtensions: RTCRtpHeaderExtensionCapability[];
}

/**
 * RTCP settings of a sender or receiver.
 *
 * @category WebRTC
 */
interface RTCRtcpParameters {
  cname?: string;
  reducedSize?: boolean;
}

/**
 * One encoding of a sender (one simulcast layer).
 *
 * @category WebRTC
 */
interface RTCRtpEncodingParameters {
  rid?: string;
  active?: boolean;
  maxBitrate?: number;
  maxFramerate?: number;
  scaleResolutionDownBy?: number;
  priority?: string;
  networkPriority?: string;
}

/**
 * A receiver's negotiated parameters.
 *
 * @category WebRTC
 */
interface RTCRtpParameters {
  codecs: RTCRtpCodecParameters[];
  headerExtensions: RTCRtpHeaderExtensionParameters[];
  rtcp: RTCRtcpParameters;
}

/**
 * A sender's parameters, read with `getParameters()` and changed with
 * `setParameters()` (encodings only).
 *
 * @category WebRTC
 */
interface RTCRtpSendParameters extends RTCRtpParameters {
  transactionId: string;
  encodings: RTCRtpEncodingParameters[];
}

/**
 * A source of received RTP, from `getSynchronizationSources()` /
 * `getContributingSources()`.
 *
 * @category WebRTC
 */
interface RTCRtpContributingSource {
  timestamp: number;
  source: number;
  audioLevel?: number;
  rtpTimestamp: number;
}

/**
 * Options for `addTransceiver()`.
 *
 * @category WebRTC
 */
interface RTCRtpTransceiverInit {
  direction?: RTCRtpTransceiverDirection;
  streams?: MediaStream[];
  sendEncodings?: RTCRtpEncodingParameters[];
}

/**
 * A media track. Deno has no capture devices: a remote track is a
 * receiver's (read its RTP with `Deno.webrtc.readRtp(receiver)`), and a
 * local one is a `Deno.webrtc.EncodedMediaTrack`.
 *
 * @category WebRTC
 */
interface MediaStreamTrack extends EventTarget {
  contentHint: string;
  enabled: boolean;
  readonly id: string;
  readonly kind: string;
  readonly label: string;
  readonly muted: boolean;
  readonly readyState: MediaStreamTrackState;
  onended: ((this: MediaStreamTrack, ev: Event) => any) | null;
  onmute: ((this: MediaStreamTrack, ev: Event) => any) | null;
  onunmute: ((this: MediaStreamTrack, ev: Event) => any) | null;
  applyConstraints(constraints?: Record<string, unknown>): Promise<void>;
  clone(): MediaStreamTrack;
  getCapabilities(): Record<string, unknown>;
  getConstraints(): Record<string, unknown>;
  getSettings(): Record<string, unknown>;
  stop(): void;
}

/**
 * A media track (not constructible).
 *
 * @category WebRTC
 */
declare var MediaStreamTrack: {
  readonly prototype: MediaStreamTrack;
  new (): MediaStreamTrack;
};

/**
 * The init dictionary of a `MediaStreamTrackEvent`.
 *
 * @category WebRTC
 */
interface MediaStreamTrackEventInit extends EventInit {
  track: MediaStreamTrack;
}

/**
 * A stream's `addtrack` / `removetrack` event.
 *
 * @category WebRTC
 */
interface MediaStreamTrackEvent extends Event {
  readonly track: MediaStreamTrack;
}

/**
 * A stream's `addtrack` / `removetrack` event.
 *
 * @category WebRTC
 */
declare var MediaStreamTrackEvent: {
  readonly prototype: MediaStreamTrackEvent;
  new (
    type: string,
    eventInitDict: MediaStreamTrackEventInit,
  ): MediaStreamTrackEvent;
};

/**
 * A group of tracks (what `a=msid` names in the SDP).
 *
 * @category WebRTC
 */
interface MediaStream extends EventTarget {
  readonly active: boolean;
  readonly id: string;
  onaddtrack: ((this: MediaStream, ev: MediaStreamTrackEvent) => any) | null;
  onremovetrack:
    | ((this: MediaStream, ev: MediaStreamTrackEvent) => any)
    | null;
  addTrack(track: MediaStreamTrack): void;
  clone(): MediaStream;
  getAudioTracks(): MediaStreamTrack[];
  getTrackById(trackId: string): MediaStreamTrack | null;
  getTracks(): MediaStreamTrack[];
  getVideoTracks(): MediaStreamTrack[];
  removeTrack(track: MediaStreamTrack): void;
}

/**
 * A group of tracks.
 *
 * @category WebRTC
 */
declare var MediaStream: {
  readonly prototype: MediaStream;
  new (): MediaStream;
  new (stream: MediaStream): MediaStream;
  new (tracks: MediaStreamTrack[]): MediaStream;
};

/**
 * The connection's ICE transport.
 *
 * @category WebRTC
 */
interface RTCIceTransport extends EventTarget {
  readonly component: RTCIceComponent;
  readonly gatheringState: RTCIceGathererState;
  readonly role: RTCIceRole;
  readonly state: RTCIceTransportState;
  ongatheringstatechange: ((this: RTCIceTransport, ev: Event) => any) | null;
  onselectedcandidatepairchange:
    | ((this: RTCIceTransport, ev: Event) => any)
    | null;
  onstatechange: ((this: RTCIceTransport, ev: Event) => any) | null;
  getLocalParameters(): RTCIceParameters | null;
  getRemoteParameters(): RTCIceParameters | null;
  getSelectedCandidatePair(): RTCIceCandidatePair | null;
}

/**
 * The connection's ICE transport (not constructible).
 *
 * @category WebRTC
 */
declare var RTCIceTransport: {
  readonly prototype: RTCIceTransport;
  new (): never;
};

/**
 * ICE credentials.
 *
 * @category WebRTC
 */
interface RTCIceParameters {
  usernameFragment?: string;
  password?: string;
}

/**
 * The selected candidate pair.
 *
 * @category WebRTC
 */
interface RTCIceCandidatePair {
  local?: RTCIceCandidate;
  remote?: RTCIceCandidate;
}

/**
 * The connection's DTLS transport (one, bundled), shared by every sender,
 * receiver and the SCTP transport.
 *
 * @category WebRTC
 */
interface RTCDtlsTransport extends EventTarget {
  readonly iceTransport: RTCIceTransport;
  readonly state: RTCDtlsTransportState;
  onerror: ((this: RTCDtlsTransport, ev: Event) => any) | null;
  onstatechange: ((this: RTCDtlsTransport, ev: Event) => any) | null;
  getRemoteCertificates(): ArrayBuffer[];
}

/**
 * The connection's DTLS transport (not constructible).
 *
 * @category WebRTC
 */
declare var RTCDtlsTransport: {
  readonly prototype: RTCDtlsTransport;
  new (): never;
};

/**
 * Sends one track's media to the peer.
 *
 * @category WebRTC
 */
interface RTCRtpSender {
  readonly dtmf: null;
  readonly track: MediaStreamTrack | null;
  readonly transport: RTCDtlsTransport | null;
  getParameters(): RTCRtpSendParameters;
  getStats(): Promise<RTCStatsReport>;
  replaceTrack(withTrack: MediaStreamTrack | null): Promise<void>;
  setParameters(parameters: RTCRtpSendParameters): Promise<void>;
  setStreams(...streams: MediaStream[]): void;
}

/**
 * Sends one track's media to the peer (not constructible).
 *
 * @category WebRTC
 */
declare var RTCRtpSender: {
  readonly prototype: RTCRtpSender;
  new (): never;
  getCapabilities(kind: string): RTCRtpCapabilities | null;
};

/**
 * Receives one track's media from the peer.
 *
 * @category WebRTC
 */
interface RTCRtpReceiver {
  jitterBufferTarget: number | null;
  readonly track: MediaStreamTrack;
  readonly transport: RTCDtlsTransport | null;
  getContributingSources(): RTCRtpContributingSource[];
  getParameters(): RTCRtpParameters;
  getStats(): Promise<RTCStatsReport>;
  getSynchronizationSources(): RTCRtpContributingSource[];
}

/**
 * Receives one track's media from the peer (not constructible).
 *
 * @category WebRTC
 */
declare var RTCRtpReceiver: {
  readonly prototype: RTCRtpReceiver;
  new (): never;
  getCapabilities(kind: string): RTCRtpCapabilities | null;
};

/**
 * A sender and a receiver sharing one media section (`mid`).
 *
 * @category WebRTC
 */
interface RTCRtpTransceiver {
  readonly currentDirection: RTCRtpTransceiverDirection | null;
  direction: RTCRtpTransceiverDirection;
  readonly mid: string | null;
  readonly receiver: RTCRtpReceiver;
  readonly sender: RTCRtpSender;
  setCodecPreferences(codecs: RTCRtpCodec[]): void;
  stop(): void;
}

/**
 * A sender and a receiver sharing one media section (not constructible).
 *
 * @category WebRTC
 */
declare var RTCRtpTransceiver: {
  readonly prototype: RTCRtpTransceiver;
  new (): never;
};

/**
 * The init dictionary of an `RTCTrackEvent`.
 *
 * @category WebRTC
 */
interface RTCTrackEventInit extends EventInit {
  receiver: RTCRtpReceiver;
  streams?: MediaStream[];
  track: MediaStreamTrack;
  transceiver: RTCRtpTransceiver;
}

/**
 * The `track` event: the peer started sending on a transceiver.
 *
 * @category WebRTC
 */
interface RTCTrackEvent extends Event {
  readonly receiver: RTCRtpReceiver;
  readonly streams: ReadonlyArray<MediaStream>;
  readonly track: MediaStreamTrack;
  readonly transceiver: RTCRtpTransceiver;
}

/**
 * The `track` event.
 *
 * @category WebRTC
 */
declare var RTCTrackEvent: {
  readonly prototype: RTCTrackEvent;
  new (type: string, eventInitDict: RTCTrackEventInit): RTCTrackEvent;
};

/**
 * Options for `createOffer()`.
 *
 * @category WebRTC
 */
interface RTCOfferOptions {
  iceRestart?: boolean;
  offerToReceiveAudio?: boolean;
  offerToReceiveVideo?: boolean;
}

/**
 * Options for `createAnswer()` (none are defined).
 *
 * @category WebRTC
 */
interface RTCAnswerOptions {}

/**
 * The fields of a session description.
 *
 * @category WebRTC
 */
interface RTCSessionDescriptionInit {
  sdp?: string;
  type: RTCSdpType;
}

/**
 * A description for `setLocalDescription()`: the type may be left out (it
 * is then inferred from the signaling state), and so may the SDP (it is
 * then created).
 *
 * @category WebRTC
 */
interface RTCLocalSessionDescriptionInit {
  sdp?: string;
  type?: RTCSdpType;
}

/**
 * The fields of an ICE candidate, as signalled.
 *
 * @category WebRTC
 */
interface RTCIceCandidateInit {
  candidate?: string;
  sdpMLineIndex?: number | null;
  sdpMid?: string | null;
  usernameFragment?: string | null;
}

/**
 * The fields of a locally constructed ICE candidate.
 *
 * @category WebRTC
 */
interface RTCLocalIceCandidateInit extends RTCIceCandidateInit {}

/**
 * Options for `createDataChannel()`. `maxPacketLifeTime` and
 * `maxRetransmits` make the channel partially reliable and are mutually
 * exclusive; `negotiated: true` with an `id` sets up the channel out of band
 * (both peers create it with the same id).
 *
 * @category WebRTC
 */
interface RTCDataChannelInit {
  ordered?: boolean;
  maxPacketLifeTime?: number;
  maxRetransmits?: number;
  protocol?: string;
  negotiated?: boolean;
  id?: number;
}

/**
 * A session description: an offer or answer in SDP.
 *
 * @category WebRTC
 */
interface RTCSessionDescription {
  readonly sdp: string;
  readonly type: RTCSdpType;
  toJSON(): RTCSessionDescriptionInit;
}

/**
 * A session description: an offer or answer in SDP.
 *
 * @category WebRTC
 */
declare var RTCSessionDescription: {
  readonly prototype: RTCSessionDescription;
  new (descriptionInitDict: RTCSessionDescriptionInit): RTCSessionDescription;
};

/**
 * An ICE candidate: a transport address the peer can try to reach this
 * connection on. The parsed fields are `null` when the candidate string is
 * empty or malformed.
 *
 * @category WebRTC
 */
interface RTCIceCandidate {
  readonly address: string | null;
  readonly candidate: string;
  readonly component: RTCIceComponent | null;
  readonly foundation: string | null;
  readonly port: number | null;
  readonly priority: number | null;
  readonly protocol: RTCIceProtocol | null;
  readonly relatedAddress: string | null;
  readonly relatedPort: number | null;
  readonly sdpMLineIndex: number | null;
  readonly sdpMid: string | null;
  readonly tcpType: RTCIceTcpCandidateType | null;
  readonly type: RTCIceCandidateType | null;
  readonly usernameFragment: string | null;
  toJSON(): RTCIceCandidateInit;
}

/**
 * An ICE candidate: a transport address the peer can try to reach this
 * connection on.
 *
 * @category WebRTC
 */
declare var RTCIceCandidate: {
  readonly prototype: RTCIceCandidate;
  new (candidateInitDict?: RTCLocalIceCandidateInit): RTCIceCandidate;
};

/**
 * The fields of an `RTCPeerConnectionIceEvent`.
 *
 * @category WebRTC
 */
interface RTCPeerConnectionIceEventInit extends EventInit {
  candidate?: RTCIceCandidate | null;
  url?: string | null;
}

/**
 * An `icecandidate` event: a new local candidate to signal to the peer, or
 * `candidate: null` once gathering is complete.
 *
 * @category WebRTC
 */
interface RTCPeerConnectionIceEvent extends Event {
  readonly candidate: RTCIceCandidate | null;
  readonly url: string | null;
}

/**
 * An `icecandidate` event.
 *
 * @category WebRTC
 */
declare var RTCPeerConnectionIceEvent: {
  readonly prototype: RTCPeerConnectionIceEvent;
  new (
    type: string,
    eventInitDict?: RTCPeerConnectionIceEventInit,
  ): RTCPeerConnectionIceEvent;
};

/**
 * The fields of an `RTCPeerConnectionIceErrorEvent`.
 *
 * @category WebRTC
 */
interface RTCPeerConnectionIceErrorEventInit extends EventInit {
  address?: string | null;
  errorCode: number;
  errorText?: string;
  port?: number | null;
  url?: string;
}

/**
 * An `icecandidateerror` event: a STUN or TURN server could not be used, a
 * local address could not be gathered on, or an address was refused by the
 * net permission (code 701, the permission error as `errorText`).
 *
 * @category WebRTC
 */
interface RTCPeerConnectionIceErrorEvent extends Event {
  readonly address: string | null;
  readonly errorCode: number;
  readonly errorText: string;
  readonly port: number | null;
  readonly url: string;
}

/**
 * An `icecandidateerror` event.
 *
 * @category WebRTC
 */
declare var RTCPeerConnectionIceErrorEvent: {
  readonly prototype: RTCPeerConnectionIceErrorEvent;
  new (
    type: string,
    eventInitDict: RTCPeerConnectionIceErrorEventInit,
  ): RTCPeerConnectionIceErrorEvent;
};

/**
 * The fields of an `RTCError`.
 *
 * @category WebRTC
 */
interface RTCErrorInit {
  errorDetail: RTCErrorDetailType;
  httpRequestStatusCode?: number;
  receivedAlert?: number;
  sctpCauseCode?: number;
  sdpLineNumber?: number;
  sentAlert?: number;
}

/**
 * A WebRTC error: a `DOMException` named `OperationError` with a WebRTC
 * `errorDetail`.
 *
 * @category WebRTC
 */
interface RTCError extends DOMException {
  readonly errorDetail: RTCErrorDetailType;
  readonly httpRequestStatusCode: number | null;
  readonly receivedAlert: number | null;
  readonly sctpCauseCode: number | null;
  readonly sdpLineNumber: number | null;
  readonly sentAlert: number | null;
}

/**
 * A WebRTC error.
 *
 * @category WebRTC
 */
declare var RTCError: {
  readonly prototype: RTCError;
  new (init: RTCErrorInit, message?: string): RTCError;
};

/**
 * The fields of an `RTCErrorEvent`.
 *
 * @category WebRTC
 */
interface RTCErrorEventInit extends EventInit {
  error: RTCError;
}

/**
 * An `error` event on an `RTCDataChannel`.
 *
 * @category WebRTC
 */
interface RTCErrorEvent extends Event {
  readonly error: RTCError;
}

/**
 * An `error` event on an `RTCDataChannel`.
 *
 * @category WebRTC
 */
declare var RTCErrorEvent: {
  readonly prototype: RTCErrorEvent;
  new (type: string, eventInitDict: RTCErrorEventInit): RTCErrorEvent;
};

/**
 * The events an `RTCDataChannel` fires.
 *
 * @category WebRTC
 */
interface RTCDataChannelEventMap {
  bufferedamountlow: Event;
  close: Event;
  closing: Event;
  error: RTCErrorEvent;
  message: MessageEvent;
  open: Event;
}

/**
 * A bidirectional channel for arbitrary messages between two peers, over
 * SCTP in the connection's DTLS. Messages are strings (`send(string)`,
 * received as strings) or binary (`ArrayBuffer`, typed arrays, `DataView`,
 * `Blob`; received as `ArrayBuffer` or, with `binaryType = "blob"`, `Blob`).
 * A message may be up to the connection's `sctp.maxMessageSize` bytes
 * (256 KiB when both peers allow it); a larger one throws a `TypeError`.
 *
 * @example
 * ```ts
 * const channel = pc.createDataChannel("chat");
 * channel.onopen = () => channel.send("hello");
 * channel.onmessage = (e) => console.log(e.data);
 * ```
 *
 * @category WebRTC
 */
interface RTCDataChannel extends EventTarget {
  binaryType: BinaryType;
  /** Bytes queued by `send()` that SCTP has not yet had acknowledged. */
  readonly bufferedAmount: number;
  /** `bufferedamountlow` fires when `bufferedAmount` falls to this or below. */
  bufferedAmountLowThreshold: number;
  readonly id: number | null;
  readonly label: string;
  readonly maxPacketLifeTime: number | null;
  readonly maxRetransmits: number | null;
  readonly negotiated: boolean;
  onbufferedamountlow: ((this: RTCDataChannel, ev: Event) => any) | null;
  onclose: ((this: RTCDataChannel, ev: Event) => any) | null;
  onclosing: ((this: RTCDataChannel, ev: Event) => any) | null;
  onerror: ((this: RTCDataChannel, ev: RTCErrorEvent) => any) | null;
  onmessage: ((this: RTCDataChannel, ev: MessageEvent) => any) | null;
  onopen: ((this: RTCDataChannel, ev: Event) => any) | null;
  readonly ordered: boolean;
  readonly protocol: string;
  readonly readyState: RTCDataChannelState;
  close(): void;
  send(data: string): void;
  send(data: Blob): void;
  send(data: ArrayBuffer): void;
  send(data: ArrayBufferView): void;
  addEventListener<K extends keyof RTCDataChannelEventMap>(
    type: K,
    listener: (this: RTCDataChannel, ev: RTCDataChannelEventMap[K]) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof RTCDataChannelEventMap>(
    type: K,
    listener: (this: RTCDataChannel, ev: RTCDataChannelEventMap[K]) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/**
 * A bidirectional message channel between two peers; created by
 * `RTCPeerConnection.createDataChannel()` or received in a `datachannel`
 * event (not constructible).
 *
 * @category WebRTC
 */
declare var RTCDataChannel: {
  readonly prototype: RTCDataChannel;
  new (): never;
};

/**
 * The fields of an `RTCDataChannelEvent`.
 *
 * @category WebRTC
 */
interface RTCDataChannelEventInit extends EventInit {
  channel: RTCDataChannel;
}

/**
 * A `datachannel` event: the peer opened a channel.
 *
 * @category WebRTC
 */
interface RTCDataChannelEvent extends Event {
  readonly channel: RTCDataChannel;
}

/**
 * A `datachannel` event.
 *
 * @category WebRTC
 */
declare var RTCDataChannelEvent: {
  readonly prototype: RTCDataChannelEvent;
  new (
    type: string,
    eventInitDict: RTCDataChannelEventInit,
  ): RTCDataChannelEvent;
};

/**
 * The connection's SCTP transport, which carries every data channel.
 * Available as `RTCPeerConnection.sctp` once the connection is connected.
 *
 * @category WebRTC
 */
interface RTCSctpTransport extends EventTarget {
  /** The largest message `RTCDataChannel.send()` accepts, in bytes. */
  readonly maxMessageSize: number;
  readonly maxChannels: number | null;
  readonly state: RTCSctpTransportState;
  /** The DTLS transport it runs over. */
  readonly transport: RTCDtlsTransport | null;
  onstatechange: ((this: RTCSctpTransport, ev: Event) => any) | null;
}

/**
 * The connection's SCTP transport (not constructible).
 *
 * @category WebRTC
 */
declare var RTCSctpTransport: {
  readonly prototype: RTCSctpTransport;
  new (): never;
};

/**
 * The statistics `getStats()` resolves to, by id. Entries follow
 * webrtc-stats (`peer-connection`, `transport`, `candidate-pair`,
 * `local-candidate`, `remote-candidate`, `certificate`, `data-channel`,
 * `codec`, `inbound-rtp`, `outbound-rtp`, `remote-inbound-rtp`,
 * `remote-outbound-rtp`).
 *
 * @category WebRTC
 */
interface RTCStatsReport extends ReadonlyMap<string, any> {}

/**
 * The statistics `getStats()` resolves to (not constructible).
 *
 * @category WebRTC
 */
declare var RTCStatsReport: {
  readonly prototype: RTCStatsReport;
  new (): never;
};

/**
 * The events an `RTCPeerConnection` fires.
 *
 * @category WebRTC
 */
interface RTCPeerConnectionEventMap {
  connectionstatechange: Event;
  datachannel: RTCDataChannelEvent;
  icecandidate: RTCPeerConnectionIceEvent;
  icecandidateerror: RTCPeerConnectionIceErrorEvent;
  iceconnectionstatechange: Event;
  icegatheringstatechange: Event;
  negotiationneeded: Event;
  signalingstatechange: Event;
  track: RTCTrackEvent;
}

/**
 * A WebRTC connection to a remote peer, so a Deno program can be a peer of
 * browsers and of other programs (W3C `RTCPeerConnection`): data channels,
 * and media transceivers whose local tracks are
 * `Deno.webrtc.EncodedMediaTrack`s (there are no capture devices or codecs;
 * the program sends encoded frames or RTP and reads received RTP).
 *
 * Signaling is the application's: exchange `localDescription` (or the
 * offer/answer plus each `icecandidate`) with the peer over any channel.
 *
 * Permissions: each ICE server's host and port need `--allow-net`
 * (`--allow-net-connect`); every datagram goes only to an address the net
 * permission allows (a remote candidate whose address is refused makes
 * `addIceCandidate()` / `setRemoteDescription()` reject); local sockets are
 * bound on `127.0.0.1`, or on `0.0.0.0` / `[::]` with the listen permission
 * for that address and `--allow-sys=networkInterfaces` (without the latter,
 * only loopback host candidates are gathered). A connection keeps the
 * process alive from its first description until `close()`.
 *
 * @example
 * ```ts
 * const pc = new RTCPeerConnection({
 *   iceServers: [{ urls: "stun:stun.l.google.com:19302" }],
 * });
 * const channel = pc.createDataChannel("data");
 * pc.onicecandidate = (e) => sendToPeer({ candidate: e.candidate });
 * await pc.setLocalDescription();
 * sendToPeer({ description: pc.localDescription });
 * ```
 *
 * @category WebRTC
 */
interface RTCPeerConnection extends EventTarget {
  readonly canTrickleIceCandidates: boolean | null;
  readonly connectionState: RTCPeerConnectionState;
  readonly currentLocalDescription: RTCSessionDescription | null;
  readonly currentRemoteDescription: RTCSessionDescription | null;
  readonly iceConnectionState: RTCIceConnectionState;
  readonly iceGatheringState: RTCIceGatheringState;
  readonly localDescription: RTCSessionDescription | null;
  readonly pendingLocalDescription: RTCSessionDescription | null;
  readonly pendingRemoteDescription: RTCSessionDescription | null;
  readonly remoteDescription: RTCSessionDescription | null;
  readonly sctp: RTCSctpTransport | null;
  readonly signalingState: RTCSignalingState;
  onconnectionstatechange: ((this: RTCPeerConnection, ev: Event) => any) | null;
  ondatachannel:
    | ((this: RTCPeerConnection, ev: RTCDataChannelEvent) => any)
    | null;
  onicecandidate:
    | ((this: RTCPeerConnection, ev: RTCPeerConnectionIceEvent) => any)
    | null;
  onicecandidateerror:
    | ((this: RTCPeerConnection, ev: RTCPeerConnectionIceErrorEvent) => any)
    | null;
  oniceconnectionstatechange:
    | ((this: RTCPeerConnection, ev: Event) => any)
    | null;
  onicegatheringstatechange:
    | ((this: RTCPeerConnection, ev: Event) => any)
    | null;
  onnegotiationneeded: ((this: RTCPeerConnection, ev: Event) => any) | null;
  onsignalingstatechange: ((this: RTCPeerConnection, ev: Event) => any) | null;
  ontrack: ((this: RTCPeerConnection, ev: RTCTrackEvent) => any) | null;
  addIceCandidate(candidate?: RTCIceCandidateInit | null): Promise<void>;
  addTrack(track: MediaStreamTrack, ...streams: MediaStream[]): RTCRtpSender;
  addTransceiver(
    trackOrKind: MediaStreamTrack | string,
    init?: RTCRtpTransceiverInit,
  ): RTCRtpTransceiver;
  getReceivers(): RTCRtpReceiver[];
  getSenders(): RTCRtpSender[];
  getTransceivers(): RTCRtpTransceiver[];
  removeTrack(sender: RTCRtpSender): void;
  close(): void;
  createAnswer(options?: RTCAnswerOptions): Promise<RTCSessionDescriptionInit>;
  createDataChannel(
    label: string,
    dataChannelDict?: RTCDataChannelInit,
  ): RTCDataChannel;
  createOffer(options?: RTCOfferOptions): Promise<RTCSessionDescriptionInit>;
  /** @deprecated The legacy callback form; use the promise form. */
  createOffer(
    successCallback: RTCSessionDescriptionCallback,
    failureCallback: RTCPeerConnectionErrorCallback,
    options?: RTCOfferOptions,
  ): Promise<void>;
  /** @deprecated The legacy callback form; use the promise form. */
  createAnswer(
    successCallback: RTCSessionDescriptionCallback,
    failureCallback: RTCPeerConnectionErrorCallback,
  ): Promise<void>;
  /** @deprecated The legacy callback form; use the promise form. */
  setLocalDescription(
    description: RTCLocalSessionDescriptionInit,
    successCallback: () => void,
    failureCallback: RTCPeerConnectionErrorCallback,
  ): Promise<void>;
  /** @deprecated The legacy callback form; use the promise form. */
  setRemoteDescription(
    description: RTCSessionDescriptionInit,
    successCallback: () => void,
    failureCallback: RTCPeerConnectionErrorCallback,
  ): Promise<void>;
  /** @deprecated The legacy callback form; use the promise form. */
  addIceCandidate(
    candidate: RTCIceCandidateInit,
    successCallback: () => void,
    failureCallback: RTCPeerConnectionErrorCallback,
  ): Promise<void>;
  getConfiguration(): RTCConfiguration;
  getStats(selector?: MediaStreamTrack | null): Promise<RTCStatsReport>;
  restartIce(): void;
  setConfiguration(configuration?: RTCConfiguration): void;
  setLocalDescription(
    description?: RTCLocalSessionDescriptionInit,
  ): Promise<void>;
  setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void>;
  addEventListener<K extends keyof RTCPeerConnectionEventMap>(
    type: K,
    listener: (
      this: RTCPeerConnection,
      ev: RTCPeerConnectionEventMap[K],
    ) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof RTCPeerConnectionEventMap>(
    type: K,
    listener: (
      this: RTCPeerConnection,
      ev: RTCPeerConnectionEventMap[K],
    ) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/**
 * A WebRTC connection to a remote peer.
 *
 * @category WebRTC
 */
declare var RTCPeerConnection: {
  readonly prototype: RTCPeerConnection;
  new (configuration?: RTCConfiguration): RTCPeerConnection;
  /** Generates a DTLS certificate (ECDSA P-256 or RSASSA-PKCS1-v1_5). */
  generateCertificate(
    keygenAlgorithm: RTCCertificateKeygenAlgorithm | string,
  ): Promise<RTCCertificate>;
};

/**
 * Deno's WebRTC extension: what a server needs that the standard doesn't
 * have.
 *
 * @category WebRTC
 */
declare namespace Deno {
  /**
   * Deno's WebRTC extension: sending already-encoded media and RTP,
   * reading received RTP, and keeping a DTLS certificate.
   *
   * @category WebRTC
   */
  export namespace webrtc {
    /**
     * What an `EncodedMediaTrack` sends.
     *
     * @category WebRTC
     */
    export interface EncodedMediaTrackInit {
      kind: "audio" | "video";
      /** The codec, e.g. `"video/VP8"`, `"video/H264"`, `"audio/opus"`. */
      mimeType: string;
      clockRate?: number;
      channels?: number;
      sdpFmtpLine?: string;
      id?: string;
      label?: string;
    }

    /**
     * A local media track fed with encoded media: it has no capture device
     * or codec. Add it with `addTrack()` / `addTransceiver()` /
     * `replaceTrack()` on any number of connections; each write goes to
     * every connection it is sent on (once negotiated), and the codec is
     * the transceiver's preferred one.
     *
     * @example
     * ```ts
     * const track = new Deno.webrtc.EncodedMediaTrack({
     *   kind: "video",
     *   mimeType: "video/VP8",
     * });
     * pc.addTrack(track);
     * track.onkeyframerequest = () => encoder.forceKeyFrame();
     * track.writeFrame(vp8Frame, { duration: 33 });
     * ```
     *
     * @category WebRTC
     */
    export class EncodedMediaTrack extends MediaStreamTrack {
      constructor(init: EncodedMediaTrackInit);
      /** The codec it sends. */
      readonly codec: RTCRtpCodec;
      /** Fired when a receiver asks for a key frame (RTCP PLI or FIR). */
      onkeyframerequest: ((this: EncodedMediaTrack, ev: Event) => any) | null;
      /**
       * Sends one encoded frame (Annex B for H.264 / H.265), cut into RTP
       * packets for the negotiated codec; `duration` (ms) advances the RTP
       * timestamp. Returns how many senders took it.
       */
      writeFrame(data: BufferSource, options?: { duration?: number }): number;
      /**
       * Sends one finished RTP packet (header included); its SSRC becomes
       * each sender's and an unknown payload type the negotiated one.
       * Returns how many senders took it.
       */
      writeRtp(packet: BufferSource): number;
    }

    /**
     * Reads the RTP packets a receiver gets (header included), one chunk
     * per packet. Real-time media doesn't wait: a packet that arrives while
     * `highWaterMark` packets (256) are queued is dropped.
     *
     * @category WebRTC
     */
    export function readRtp(
      receiver: RTCRtpReceiver,
      options?: { highWaterMark?: number },
    ): ReadableStream<Uint8Array>;

    /**
     * Asks the remote sender of a receiver for a key frame (RTCP PLI).
     * Returns whether it was sent.
     *
     * @category WebRTC
     */
    export function requestKeyFrame(receiver: RTCRtpReceiver): boolean;

    /**
     * A certificate's key, X.509 certificate and expiry as PEM, for
     * `importCertificate()`: a server that keeps it keeps its DTLS
     * fingerprint across restarts. The PEM holds the private key.
     *
     * @category WebRTC
     */
    export function exportCertificate(certificate: RTCCertificate): string;

    /**
     * An `RTCCertificate` from `exportCertificate()`'s PEM.
     *
     * @category WebRTC
     */
    export function importCertificate(pem: string): RTCCertificate;
  }
}
