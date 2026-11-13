# deno_webrtc

WebRTC data channels for Deno: `RTCPeerConnection`, `RTCDataChannel`,
`RTCSessionDescription`, `RTCIceCandidate`, their events and `RTCStatsReport`,
as globals (W3C WebRTC 1.0). Media tracks are not implemented.

The protocol stack (ICE, DTLS, SCTP, the data channel protocol) is webrtc-rs's
sans-I/O `rtc` crates, pinned from
[Brainwires/webrtc-rs-rtc](https://github.com/Brainwires/webrtc-rs-rtc) (our
fork, synced with upstream); `peer.rs` owns the UDP
sockets, the timers and the STUN/TURN clients around it, and gates every
datagram through the net permission. `01_webrtc.js` is the JavaScript surface.

Spec: https://www.w3.org/TR/webrtc/
