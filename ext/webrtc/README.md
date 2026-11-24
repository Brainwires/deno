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

## Embedding for web pages

A runtime that runs untrusted page script (no `Deno` namespace, `Deno.core`
removed after bootstrap) can load this extension with three things in mind.

**Extensions.** `deno_webrtc` needs `deno_web`'s extension only. `deno_net` is a
crate dependency (its resolver, its routed TCP connect and the type of the TLS
options), not an extension one, so none of `deno_net`'s ops has to be
registered. Without `deno_net::DefaultTlsOptions` in the `OpState` a `turns:`
server is verified against the default root store. The JavaScript loads
`deno_webidl`, `deno_web`, and on first use `deno_crypto` (`SFrameTransform`)
and, for `peerIdentity` / `setIdentityProvider` only, `deno_fetch` and the
runtime's worker module (`ext:runtime/11_workers.js`), which a page runtime
doesn't have: an identity provider call fails there. The ops behind
`Deno.webrtc` (`op_webrtc_network_*`, which bind listeners on chosen ports,
`op_webrtc_certificate_*_pem`, `op_webrtc_sender_write_*`,
`op_webrtc_fake_media_devices`, `op_webrtc_set_log_level`) are registered with
the extension; nothing in the page-facing interfaces reaches them, and an
embedder that lists its ops can disable them with an op middleware.

**The address deny list.** With a `deno_fetch::deny_addresses::EmbedderDenyList`
in the `OpState` (the list a page runtime gives its `fetch()` client):

- a remote candidate whose address is denied, in a description or given to
  `addIceCandidate` / `RTCIceTransport.addRemoteCandidate`, is dropped before
  the ICE agent sees it, and the call succeeds, as for any candidate that can't
  be used. No pair is formed, so no connectivity check goes there;
- every mDNS `.local` candidate is dropped unresolved (the default is to resolve
  them): resolving asks the local network about a name the remote description
  chose, the answer is a LAN address, and a name says nothing about where it
  leads. Such a peer is still reached, as a peer-reflexive candidate, when its
  own checks arrive from an address that is not denied;
- a STUN or TURN server is refused (an `icecandidateerror` whose text starts
  with `ERR_ADDRESS_DENIED`) when any address its name resolves to is denied;
  the client then talks to the one address that was checked, and a TCP or TLS
  connection's peer address is checked again;
- no datagram is sent to a denied address at all (the send-side gate every
  datagram already passes for the net permission), which covers checks to a
  peer-reflexive address, an ICE-TCP dial and data sent through a TURN relay.

IPv4-mapped IPv6 addresses are compared as the IPv4 address they carry. The list
governs where this machine sends; what a TURN server relays to is checked
against it too (as an address), although the relay reaches it from its own
network.

**Local addresses.** Host candidates are gathered on the machine's interface
addresses when the permissions allow listing them (`sys: networkInterfaces`) and
listening on the wildcard address; without those the only host candidate is
`127.0.0.1`, and nothing outside the machine is reachable. With them, the page
sees the interface addresses, private ones included, in candidates, the local
description and `getStats()`: nothing hides them by default (a browser publishes
mDNS names instead; here that is `Deno.webrtc.createNetwork({ mdns:
true })`,
which a page can't ask for). An embedder that must not expose them puts
`deno_webrtc::EmbedderIceOptions { force_relay: true }` in the `OpState`: every
connection then gathers as with `iceTransportPolicy: "relay"` whatever its
configuration says (no host or server-reflexive candidate), a relay candidate's
related address is `0.0.0.0` / `::`, and `icecandidateerror` names no local
address. A connection then needs a TURN server.
