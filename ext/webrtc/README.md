# deno_webrtc

WebRTC for Deno, as globals (W3C WebRTC 1.0): `RTCPeerConnection`,
`RTCDataChannel`, `RTCRtpTransceiver` / `RTCRtpSender` / `RTCRtpReceiver`,
`MediaStream`, `MediaStreamTrack`, `RTCCertificate`, `RTCDtlsTransport`,
`RTCIceTransport`, `RTCSessionDescription`, `RTCIceCandidate`, their events and
`RTCStatsReport`; and `Deno.webrtc`, the server-side extension (sending encoded
media and RTP, reading received RTP, keeping a DTLS certificate). There is no
capture device or codec: a local track is a `Deno.webrtc.EncodedMediaTrack` the
program feeds.

The protocol stack (ICE, DTLS, SCTP, the data channel protocol, SRTP, RTP / RTCP
and its interceptors) is webrtc-rs's sans-I/O `rtc` crates, pinned from
[Brainwires/webrtc-rs-rtc](https://github.com/Brainwires/webrtc-rs-rtc) (our
fork, synced with upstream); `peer.rs` owns the UDP sockets, the ICE-TCP
connections (RFC 6544, on a `Deno.webrtc.createNetwork({ tcpPort })` network),
the mDNS socket that resolves (and, with `mdns: true`, publishes) `.local` host
candidates, the timers and the STUN/TURN clients around it, and gates every
datagram through the net permission; `net.rs` holds the networks' shared sockets
and TCP listeners; `media.rs` maps transceivers, senders and receivers onto the
engine's. `01_webrtc.js` is the JavaScript surface. `Deno.webrtc.setLogLevel()`
shows the engine's own log on stderr (off by default).

Spec: https://www.w3.org/TR/webrtc/
