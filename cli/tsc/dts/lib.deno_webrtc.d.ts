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
 * Options for `new RTCPeerConnection()`. `certificates` is not supported
 * (each connection generates its own ECDSA P-256 certificate).
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
 * A certificate for a connection's DTLS. Deno does not create these
 * (`RTCPeerConnection.generateCertificate()` rejects); the type exists for
 * `RTCConfiguration.certificates`.
 *
 * @category WebRTC
 */
interface RTCCertificate {
  readonly expires: number;
}

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
 * `local-candidate`, `remote-candidate`, `certificate`, `data-channel`).
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
  track: Event;
}

/**
 * A WebRTC connection to a remote peer, for data channels, so a Deno
 * program can be a peer of browsers and of other programs (W3C
 * `RTCPeerConnection`; media tracks are not supported: `addTrack()` and
 * `addTransceiver()` throw `NotSupportedError`).
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
  ontrack: ((this: RTCPeerConnection, ev: Event) => any) | null;
  addIceCandidate(candidate?: RTCIceCandidateInit | null): Promise<void>;
  close(): void;
  createAnswer(options?: RTCAnswerOptions): Promise<RTCSessionDescriptionInit>;
  createDataChannel(
    label: string,
    dataChannelDict?: RTCDataChannelInit,
  ): RTCDataChannel;
  createOffer(options?: RTCOfferOptions): Promise<RTCSessionDescriptionInit>;
  getConfiguration(): RTCConfiguration;
  getStats(selector?: null): Promise<RTCStatsReport>;
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
 * A WebRTC connection to a remote peer, for data channels.
 *
 * @category WebRTC
 */
declare var RTCPeerConnection: {
  readonly prototype: RTCPeerConnection;
  new (configuration?: RTCConfiguration): RTCPeerConnection;
  /** Not supported: rejects with `NotSupportedError`. */
  generateCertificate(
    keygenAlgorithm: AlgorithmIdentifier,
  ): Promise<RTCCertificate>;
};
