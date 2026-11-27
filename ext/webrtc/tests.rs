// Copyright 2018-2026 the Deno authors. MIT license.

//! The driver end to end, without V8: two peer connections on loopback.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use rtci::data_channel::RTCDataChannelInit;

use crate::IceServerSpec;
use crate::JsEvent;
use crate::NetGuard;
use crate::Peer;
use crate::PeerOptions;
use crate::ServerTransport;
use crate::WebRtcError;
use crate::peer::Datagram;
use crate::peer::MAX_ROUTES;
use crate::peer::MAX_SDP_BYTES;
use crate::peer::RemoteCandidate;
use crate::peer::Via;
use crate::peer::candidate_address;
use crate::peer::classify_remote_candidate;
use crate::peer::epoch_fields_relative;

/// Loopback only: interface enumeration is refused, so host candidates are
/// `127.0.0.1` alone and the test is the same on every machine.
struct LoopbackGuard {
  refuse: Option<SocketAddr>,
}

impl NetGuard for LoopbackGuard {
  fn connect(&self, addr: SocketAddr) -> Result<(), String> {
    if Some(addr) == self.refuse {
      return Err(format!("refused {addr}"));
    }
    Ok(())
  }
  fn listen(&self, _ip: IpAddr) -> Result<(), String> {
    Ok(())
  }
  fn interfaces(&self) -> Result<(), String> {
    Err("no interface enumeration in tests".to_string())
  }
}

fn new_peer() -> Rc<Peer> {
  Peer::new(
    PeerOptions::default(),
    Rc::new(LoopbackGuard { refuse: None }),
  )
  .unwrap()
}

/// Collects events from `peer` until `done` returns true for one of them.
async fn wait_for(
  peer: &Rc<Peer>,
  seen: &mut Vec<JsEvent>,
  done: impl Fn(&JsEvent) -> bool,
) {
  if seen.iter().any(&done) {
    return;
  }
  let result = tokio::time::timeout(Duration::from_secs(20), async {
    loop {
      let batch = peer.next_events().await.expect("peer closed");
      let hit = batch.iter().any(&done);
      seen.extend(batch);
      if hit {
        return;
      }
    }
  })
  .await;
  if result.is_err() {
    let stats: Vec<_> = peer
      .stats(None)
      .unwrap_or_default()
      .into_iter()
      .filter(|s| {
        let t = s.get("type").and_then(|t| t.as_str()).unwrap_or("");
        t == "transport" || t.contains("candidate")
      })
      .collect();
    panic!(
      "timed out waiting for an event; selected {:?}; stats {stats:#?}; saw {seen:#?}",
      peer.selected_for_tests()
    );
  }
}

fn gathered(e: &JsEvent) -> bool {
  matches!(e, JsEvent::IceGatheringState { state } if state == "complete")
}

fn opened(e: &JsEvent) -> bool {
  matches!(e, JsEvent::DataChannelOpen { .. })
}

/// Offer/answer with full (non-trickle) descriptions; returns both peers
/// once a channel is open on each side.
async fn connect(
  init: RTCDataChannelInit,
) -> (Rc<Peer>, Rc<Peer>, u32, Vec<JsEvent>, Vec<JsEvent>) {
  let a = new_peer();
  let b = new_peer();
  let mut ea = Vec::new();
  let mut eb = Vec::new();
  let (id, _) = a.create_data_channel("chat", init).unwrap();
  let offer = a.create_offer(false).unwrap();
  a.set_local_description("offer", offer).unwrap();
  wait_for(&a, &mut ea, gathered).await;
  let (kind, sdp) = a.local_description().unwrap();
  assert_eq!(kind, "offer");
  assert!(sdp.contains("a=candidate:"), "offer has candidates: {sdp}");
  b.set_remote_description("offer", sdp).unwrap();
  let answer = b.create_answer().unwrap();
  b.set_local_description("answer", answer).unwrap();
  wait_for(&b, &mut eb, gathered).await;
  let (kind, sdp) = b.local_description().unwrap();
  assert_eq!(kind, "answer");
  a.set_remote_description("answer", sdp).unwrap();
  wait_for(&a, &mut ea, opened).await;
  wait_for(&b, &mut eb, opened).await;
  (a, b, id, ea, eb)
}

fn reliable() -> RTCDataChannelInit {
  RTCDataChannelInit {
    ordered: true,
    ..Default::default()
  }
}

fn messages(events: &[JsEvent]) -> Vec<(bool, Vec<u8>)> {
  events
    .iter()
    .filter_map(|e| match e {
      JsEvent::DataChannelMessage { text, data, .. } => {
        Some((*text, data.bytes()))
      }
      _ => None,
    })
    .collect()
}

async fn wait_messages(
  peer: &Rc<Peer>,
  seen: &mut Vec<JsEvent>,
  count: usize,
) -> Vec<(bool, Vec<u8>)> {
  tokio::time::timeout(Duration::from_secs(20), async {
    loop {
      if messages(seen).len() >= count {
        return;
      }
      seen.extend(peer.next_events().await.expect("peer closed"));
    }
  })
  .await
  .expect("timed out waiting for messages");
  messages(seen)
}

#[tokio::test]
async fn loopback_data_channel_both_ways() {
  let (a, b, id, mut ea, mut eb) = connect(reliable()).await;
  let remote = eb
    .iter()
    .find_map(|e| match e {
      JsEvent::DataChannelOpen {
        channel,
        id,
        label,
        ordered,
        ..
      } => Some((*channel, *id, label.clone(), *ordered)),
      _ => None,
    })
    .unwrap();
  // The remote side's handle is its own; the SCTP stream id is shared.
  let local_stream = ea
    .iter()
    .find_map(|e| match e {
      JsEvent::DataChannelOpen { id, .. } => *id,
      _ => None,
    })
    .unwrap();
  assert_eq!(remote.1, Some(local_stream));
  assert_eq!((remote.2.as_str(), remote.3), ("chat", true));
  let rid = remote.0;

  a.send(id, true, "hello from a".as_bytes()).unwrap();
  a.send(id, false, &[1, 2, 3]).unwrap();
  b.send(rid, true, "hello from b".as_bytes()).unwrap();

  let got_b = wait_messages(&b, &mut eb, 2).await;
  assert_eq!(got_b[0], (true, b"hello from a".to_vec()));
  assert_eq!(got_b[1], (false, vec![1, 2, 3]));
  let got_a = wait_messages(&a, &mut ea, 1).await;
  assert_eq!(got_a[0], (true, b"hello from b".to_vec()));

  // Both connections reached "connected".
  let connected = |e: &JsEvent| matches!(e, JsEvent::ConnectionState { state } if state == "connected");
  wait_for(&a, &mut ea, connected).await;
  wait_for(&b, &mut eb, connected).await;

  let stats = a.stats(None).unwrap();
  assert!(
    stats
      .iter()
      .any(|s| s.get("type").and_then(|t| t.as_str()) == Some("data-channel")),
    "stats has a data-channel entry: {stats:?}"
  );

  a.close();
  b.close();
  assert!(a.next_events().await.is_none() || a.next_events().await.is_none());
}

#[tokio::test]
async fn loopback_large_and_ordered_messages() {
  let (a, b, id, _ea, mut eb) = connect(reliable()).await;
  // The largest message the connection takes (both sides offer 256 KiB),
  // then 200 small ones whose order must survive; one byte more is a
  // TypeError and is never buffered.
  let max = a.max_message_size().expect("SCTP is up") as usize;
  assert_eq!(max, 262_144, "both sides take 256 KiB messages");
  let big: Vec<u8> = (0..max as u32).map(|i| (i % 251) as u8).collect();
  let too_big = vec![0u8; max + 1];
  assert!(matches!(
    a.send(id, false, &too_big),
    Err(WebRtcError::Type(_))
  ));
  let buffered = a.send(id, false, &big).unwrap();
  assert!(buffered as usize >= big.len(), "bufferedAmount {buffered}");
  for i in 0..200u32 {
    a.send(id, true, i.to_string().as_bytes()).unwrap();
  }
  let got = wait_messages(&b, &mut eb, 201).await;
  assert_eq!(got[0].1.len(), big.len());
  assert_eq!(got[0].1, big);
  for (i, (text, data)) in got[1..].iter().enumerate() {
    assert!(text);
    assert_eq!(data, i.to_string().as_bytes());
  }
  a.close();
  b.close();
}

#[tokio::test]
async fn loopback_unordered_unreliable_channel() {
  let (a, b, id, _ea, mut eb) = connect(RTCDataChannelInit {
    ordered: false,
    max_retransmits: Some(0),
    ..Default::default()
  })
  .await;
  let remote = eb
    .iter()
    .find_map(|e| match e {
      JsEvent::DataChannelOpen {
        ordered,
        max_retransmits,
        ..
      } => Some((*ordered, *max_retransmits)),
      _ => None,
    })
    .unwrap();
  // Both come through DCEP's channel type and reliability parameter.
  assert_eq!(remote, (false, Some(0)));
  for i in 0..50u32 {
    a.send(id, false, &i.to_be_bytes()).unwrap();
  }
  // Loopback loses nothing; arrival order is not asserted.
  let mut got: Vec<u32> = wait_messages(&b, &mut eb, 50)
    .await
    .into_iter()
    .map(|(_, d)| u32::from_be_bytes(d.try_into().unwrap()))
    .collect();
  got.sort_unstable();
  assert_eq!(got, (0..50).collect::<Vec<_>>());
  a.close();
  b.close();
}

#[tokio::test]
async fn close_releases_sockets_and_ends_events() {
  let (a, b, id, _ea, mut eb) = connect(reliable()).await;
  a.close();
  // The remote side sees its channel close once the association goes.
  wait_for(&b, &mut eb, |e| {
    matches!(e, JsEvent::DataChannelClose { .. })
      || matches!(e, JsEvent::ConnectionState { state } if state == "closed" || state == "failed" || state == "disconnected")
  })
  .await;
  assert!(a.is_closed());
  // Drained, then None for good.
  while a.next_events().await.is_some() {}
  assert!(a.next_events().await.is_none());
  assert!(matches!(
    a.send(id, true, b"late"),
    Err(WebRtcError::InvalidState(_)) | Err(WebRtcError::Operation(_))
  ));
  b.close();
  b.close(); // idempotent
}

#[tokio::test]
async fn refused_remote_candidate_rejects() {
  let refused: SocketAddr = "127.0.0.1:40000".parse().unwrap();
  let a = Peer::new(
    PeerOptions::default(),
    Rc::new(LoopbackGuard {
      refuse: Some(refused),
    }),
  )
  .unwrap();
  let b = new_peer();
  b.create_data_channel("x", reliable()).unwrap();
  let offer = b.create_offer(false).unwrap();
  b.set_local_description("offer", offer.clone()).unwrap();
  a.set_remote_description("offer", offer).unwrap();
  let err = a
    .add_ice_candidate(
      "candidate:1 1 udp 2130706431 127.0.0.1 40000 typ host".to_string(),
      Some("0".to_string()),
      Some(0),
      None,
    )
    .unwrap_err();
  assert!(matches!(err, WebRtcError::Permission(_)), "{err:?}");
  // mDNS and TCP candidates are accepted and skipped.
  a.add_ice_candidate(
    "candidate:2 1 udp 2130706431 0a1b2c3d.local 40001 typ host".to_string(),
    Some("0".to_string()),
    Some(0),
    None,
  )
  .unwrap();
  a.add_ice_candidate(
    "candidate:3 1 tcp 2130706431 127.0.0.1 9 typ host tcptype active"
      .to_string(),
    Some("0".to_string()),
    Some(0),
    None,
  )
  .unwrap();
  a.close();
  b.close();
}

#[test]
fn candidate_parsing() {
  assert_eq!(
    candidate_address(
      "candidate:842163049 1 udp 1677729535 1.2.3.4 46154 typ srflx raddr 0.0.0.0 rport 0"
    ),
    Some(("udp".to_string(), "1.2.3.4".to_string(), 46154))
  );
  assert_eq!(
    candidate_address("a=candidate:1 1 UDP 1 ::1 5000 typ host"),
    Some(("udp".to_string(), "::1".to_string(), 5000))
  );
  assert!(candidate_address("candidate:garbage").is_none());
}

#[test]
fn remote_candidate_classes() {
  let class = |c: &str, tcp: bool, mdns: bool| {
    classify_remote_candidate(c, tcp, mdns).unwrap()
  };
  let addr = |a: &str| a.parse::<SocketAddr>().unwrap();
  let udp = "candidate:1 1 udp 1 10.0.0.1 5 typ host";
  let named =
    "candidate:1 1 udp 1 0a1b2c3d-0000-4000-8000-000000000001.local 5 typ host";
  let passive = "candidate:1 1 tcp 1 10.0.0.1 5000 typ host tcptype passive";
  let so = "candidate:1 1 TCP 1 10.0.0.1 5001 typ host tcptype so";
  let active = "candidate:1 1 tcp 1 10.0.0.1 9 typ host tcptype active";
  let untyped = "candidate:1 1 tcp 1 10.0.0.1 5002 typ host";
  let named_passive = "candidate:1 1 tcp 1 0a1b2c3d-0000-4000-8000-000000000001.local 5000 typ host tcptype passive";

  assert_eq!(
    class(udp, false, false),
    RemoteCandidate::Udp(addr("10.0.0.1:5"))
  );
  // mDNS names: resolved only when the connection resolves them.
  assert_eq!(class(named, false, false), RemoteCandidate::Ignore);
  assert_eq!(class(named, false, true), RemoteCandidate::Mdns);
  assert_eq!(
    class(
      "candidate:1 1 udp 1 not_a_name.local 5 typ host",
      false,
      true
    ),
    RemoteCandidate::Ignore
  );
  // TCP: only with ICE over TCP, only passive / simultaneous-open.
  assert_eq!(class(passive, false, false), RemoteCandidate::Ignore);
  assert_eq!(
    class(passive, true, false),
    RemoteCandidate::Tcp(addr("10.0.0.1:5000"))
  );
  assert_eq!(
    class(so, true, false),
    RemoteCandidate::Tcp(addr("10.0.0.1:5001"))
  );
  assert_eq!(class(active, true, true), RemoteCandidate::Active);
  assert_eq!(class(active, false, true), RemoteCandidate::Ignore);
  assert_eq!(class(untyped, true, true), RemoteCandidate::Ignore);
  assert_eq!(class(named_passive, true, true), RemoteCandidate::Mdns);
  assert_eq!(class(named_passive, false, true), RemoteCandidate::Ignore);
  assert!(classify_remote_candidate("candidate:garbage", true, true).is_err());
}

#[test]
fn related_addresses_are_redacted() {
  use crate::peer::redact_related_address as redact;
  assert_eq!(
    redact(
      "candidate:1 1 udp 1677729535 203.0.113.7 46154 typ srflx raddr 192.168.1.3 rport 50000"
    ),
    "candidate:1 1 udp 1677729535 203.0.113.7 46154 typ srflx raddr 0.0.0.0 rport 0"
  );
  assert_eq!(
    redact(
      "candidate:1 1 udp 1 2001:db8::7 5 typ relay raddr 2001:db8::3 rport 6 generation 0"
    ),
    "candidate:1 1 udp 1 2001:db8::7 5 typ relay raddr :: rport 0 generation 0"
  );
  let host = "candidate:1 1 udp 1 x.local 5 typ host";
  assert_eq!(redact(host), host);
}

// W3C bufferedAmount falls as SCTP takes data from its send queue: every
// byte sent is reported by `dcsent` once transmitted (before the peer's
// acknowledgement, which the engine's back-pressure then waits for).
#[tokio::test]
async fn sent_bytes_are_reported_as_sctp_transmits_them() {
  let (a, b, id, mut ea, _eb) = connect(reliable()).await;
  const TOTAL: usize = 64 * 16 * 1024;
  for _ in 0..64 {
    a.send(id, false, &[7u8; 16 * 1024]).unwrap();
  }
  assert!(a.buffered_amount(id) > 1024);
  let reported = |events: &[JsEvent]| -> f64 {
    events
      .iter()
      .map(|e| match e {
        JsEvent::DataChannelSent { channel, bytes } if *channel == id => *bytes,
        _ => 0.0,
      })
      .sum()
  };
  ea.clear();
  tokio::time::timeout(Duration::from_secs(20), async {
    while reported(&ea) < TOTAL as f64 {
      ea.extend(a.next_events().await.expect("peer closed"));
    }
  })
  .await
  .expect("every byte is reported sent");
  assert_eq!(reported(&ea), TOTAL as f64, "not more than was sent");
  tokio::time::timeout(Duration::from_secs(20), async {
    while a.buffered_amount(id) > 0 {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .expect("the engine's back-pressure drains");
  a.close();
  b.close();
}

// A close right after sends follows them: the peer receives every message,
// then the channel's close (RFC 8831 section 6.7; the reset went out ahead
// of data still in the engine's pipeline).
#[tokio::test]
async fn close_after_send_delivers_the_data_first() {
  let (a, b, id, _ea, mut eb) = connect(reliable()).await;
  for _ in 0..16 {
    a.send(id, false, &[7u8; 16 * 1024]).unwrap();
  }
  a.close_data_channel(id);
  eb.clear();
  tokio::time::timeout(Duration::from_secs(20), async {
    while !eb
      .iter()
      .any(|e| matches!(e, JsEvent::DataChannelClose { .. }))
    {
      eb.extend(b.next_events().await.expect("peer closed"));
    }
  })
  .await
  .expect("the channel closes");
  let close_at = eb
    .iter()
    .position(|e| matches!(e, JsEvent::DataChannelClose { .. }))
    .unwrap();
  let received: usize = messages(&eb[..close_at])
    .iter()
    .map(|(_, data)| data.len())
    .sum();
  assert_eq!(received, 16 * 16 * 1024);
  a.close();
  b.close();
}

#[test]
fn stream_frames() {
  use crate::peer::frame_len_for_tests as len;
  // STUN: 20-byte header + length.
  let mut stun = vec![0u8, 1, 0, 8];
  stun.resize(28, 0);
  assert_eq!(len(&stun), Some(28));
  assert_eq!(len(&stun[..27]), Some(0));
  // ChannelData: 4-byte header + length padded to 4.
  let chan = [0x40u8, 0, 0, 5, 1, 2, 3, 4, 5, 0, 0, 0];
  assert_eq!(len(&chan), Some(12));
  assert_eq!(len(&chan[..9]), Some(0));
  assert_eq!(len(&[0xc0u8, 0, 0, 0]), None);
}

#[test]
fn ice_server_urls() {
  let stun = IceServerSpec::parse("stun:stun.example.org", None, None).unwrap();
  assert!(!stun.turn);
  assert_eq!((stun.host.as_str(), stun.port), ("stun.example.org", 3478));
  assert_eq!(stun.transport, ServerTransport::Udp);

  let turn = IceServerSpec::parse(
    "turn:turn.example.org:3479?transport=udp",
    Some("u"),
    Some("p"),
  )
  .unwrap();
  assert!(turn.turn);
  assert_eq!(turn.port, 3479);

  assert!(matches!(
    IceServerSpec::parse("turn:turn.example.org", None, None),
    Err(WebRtcError::InvalidAccess(_))
  ));
  assert!(matches!(
    IceServerSpec::parse("http://example.org", None, None),
    Err(WebRtcError::Syntax(_))
  ));
  // No userinfo or path after the scheme (a SyntaxError before the TURN
  // credential check), an empty credential or a username over 509 bytes.
  for url in [
    "stun:user@example.org",
    "turn:example.org/",
    "stun:a.org\\p",
  ] {
    assert!(
      matches!(
        IceServerSpec::parse(url, Some("u"), Some("p")),
        Err(WebRtcError::Syntax(_))
      ),
      "{url}"
    );
  }
  assert!(matches!(
    IceServerSpec::parse("turn:turn.example.org", Some("u"), Some("")),
    Err(WebRtcError::InvalidAccess(_))
  ));
  let long = "u".repeat(510);
  assert!(matches!(
    IceServerSpec::parse("turns:turn.example.org", Some(&long), Some("p")),
    Err(WebRtcError::InvalidAccess(_))
  ));
  assert!(
    IceServerSpec::parse("turn:turn.example.org", Some(&long[1..]), Some("p"))
      .is_ok()
  );
  let tcp = IceServerSpec::parse(
    "turn:turn.example.org?transport=tcp",
    Some("u"),
    Some("p"),
  )
  .unwrap();
  assert_eq!(tcp.transport, ServerTransport::Tcp);
  let tls =
    IceServerSpec::parse("turns:turn.example.org", Some("u"), Some("p"))
      .unwrap();
  assert_eq!((tls.transport, tls.port), (ServerTransport::Tls, 5349));
}

#[test]
fn shared_port_stun_routing_info() {
  // A binding request with USERNAME "srv1:cli1": routed by "srv1".
  let mut msg = vec![0x00, 0x01, 0x00, 0x10, 0x21, 0x12, 0xa4, 0x42];
  msg.extend_from_slice(&[7u8; 12]);
  msg.extend_from_slice(&[0x00, 0x06, 0x00, 0x09]);
  msg.extend_from_slice(b"srv1:cli1\0\0\0");
  let (typ, tid, ufrag) = crate::net::stun_info_for_tests(&msg).unwrap();
  assert_eq!(typ, 0x0001);
  assert_eq!(tid, [7u8; 12]);
  assert_eq!(ufrag.as_deref(), Some("srv1"));
  // A success response: routed by transaction id, no ufrag.
  let mut resp = msg[..20].to_vec();
  resp[0..4].copy_from_slice(&[0x01, 0x01, 0x00, 0x00]);
  let (typ, _, ufrag) = crate::net::stun_info_for_tests(&resp).unwrap();
  assert_eq!(typ & 0x0110, 0x0100);
  assert_eq!(ufrag, None);
  // Not STUN (DTLS, RTP): nothing.
  assert!(crate::net::stun_info_for_tests(&[22, 254, 253, 0]).is_none());
  // A truncated attribute is refused, not read past the end.
  let mut bad = msg.clone();
  bad[2..4].copy_from_slice(&[0x00, 0x40]);
  assert!(crate::net::stun_info_for_tests(&bad).is_none());
}

#[test]
fn network_options() {
  use crate::net::NetworkArg;
  use crate::net::NetworkOptions;
  let opts = NetworkOptions::parse(NetworkArg {
    public_addresses: vec![
      "203.0.113.7/10.0.0.5".into(),
      "198.51.100.2/10.0.0.6".into(),
    ],
    ..Default::default()
  })
  .unwrap();
  assert_eq!(
    opts.public_for("10.0.0.6".parse().unwrap()),
    Some("198.51.100.2".parse().unwrap())
  );
  assert_eq!(opts.public_for("10.0.0.9".parse().unwrap()), None);
  // One public address stands for every private one of its family.
  let one = NetworkOptions::parse(NetworkArg {
    public_addresses: vec!["203.0.113.7".into()],
    ..Default::default()
  })
  .unwrap();
  assert_eq!(
    one.public_for("10.1.2.3".parse().unwrap()),
    Some("203.0.113.7".parse().unwrap())
  );
  assert_eq!(one.public_for("::1".parse().unwrap()), None);
  // Several unpaired public addresses are ambiguous.
  assert!(
    NetworkOptions::parse(NetworkArg {
      public_addresses: vec!["203.0.113.7".into(), "203.0.113.8".into()],
      ..Default::default()
    })
    .is_err()
  );
  assert!(
    NetworkOptions::parse(NetworkArg {
      bind_addresses: Some(vec!["nope".into()]),
      ..Default::default()
    })
    .is_err()
  );
  // ICE over TCP and mDNS.
  use crate::net::MdnsMode;
  let defaults = NetworkOptions::parse(NetworkArg::default()).unwrap();
  assert_eq!(
    (defaults.tcp_port, defaults.mdns),
    (None, MdnsMode::Resolve)
  );
  let set = |mdns| {
    NetworkOptions::parse(NetworkArg {
      tcp_port: Some(0),
      mdns,
      ..Default::default()
    })
    .unwrap()
  };
  assert_eq!(set(Some(true)).mdns, MdnsMode::Gather);
  assert_eq!(set(Some(false)).mdns, MdnsMode::Off);
  assert_eq!(set(None).tcp_port, Some(0));
}

#[test]
fn port_range_binds_inside_it() {
  let ip: IpAddr = "127.0.0.1".parse().unwrap();
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let a = crate::net::bind_in_range(ip, (47300, 47301)).unwrap();
    let b = crate::net::bind_in_range(ip, (47300, 47301)).unwrap();
    let pa = a.local_addr().unwrap().port();
    let pb = b.local_addr().unwrap().port();
    assert!((47300..=47301).contains(&pa) && (47300..=47301).contains(&pb));
    assert_ne!(pa, pb);
    // The range is full.
    assert!(crate::net::bind_in_range(ip, (47300, 47301)).is_err());
  });
}

/// Loopback only, like [`LoopbackGuard`], for networks.
fn loopback_network(tcp_port: Option<u16>) -> Rc<crate::net::Network> {
  use crate::net::NetworkArg;
  use crate::net::NetworkOptions;
  let options = NetworkOptions::parse(NetworkArg {
    tcp_port,
    include_loopback: true,
    ..Default::default()
  })
  .unwrap();
  crate::net::Network::new(options, &LoopbackGuard { refuse: None }).unwrap()
}

fn peer_on(network: &Rc<crate::net::Network>) -> Rc<Peer> {
  Peer::new(
    PeerOptions {
      network: Some(network.clone()),
      ..Default::default()
    },
    Rc::new(LoopbackGuard { refuse: None }),
  )
  .unwrap()
}

/// The description without the candidate lines `drop` matches.
fn without(sdp: &str, drop: impl Fn(&str) -> bool) -> String {
  sdp
    .split_inclusive('\n')
    .filter(|l| !(l.starts_with("a=candidate:") && drop(l)))
    .collect()
}

fn is_udp(line: &str) -> bool {
  line.split_ascii_whitespace().nth(2) == Some("udp")
}

/// Offer/answer between two ICE-TCP connections, each description keeping
/// only the candidate lines `keep_a` / `keep_b` allow (so the test decides
/// which side listens and which dials), then a message each way.
async fn ice_tcp_round_trip(
  keep_a: impl Fn(&str) -> bool,
  keep_b: impl Fn(&str) -> bool,
) {
  let net_a = loopback_network(Some(0));
  let net_b = loopback_network(Some(0));
  assert_eq!(net_a.tcp_addresses().len(), 1);
  let a = peer_on(&net_a);
  let b = peer_on(&net_b);
  let (mut ea, mut eb) = (Vec::new(), Vec::new());
  let (id, _) = a.create_data_channel("tcp", reliable()).unwrap();
  let offer = a.create_offer(false).unwrap();
  a.set_local_description("offer", offer).unwrap();
  wait_for(&a, &mut ea, gathered).await;
  let (_, sdp) = a.local_description().unwrap();
  assert!(
    sdp.contains(" tcp ") && sdp.contains("tcptype passive"),
    "{sdp}"
  );
  assert!(sdp.contains("tcptype active"), "{sdp}");
  b.set_remote_description("offer", without(&sdp, |l| !keep_a(l)))
    .unwrap();
  let answer = b.create_answer().unwrap();
  b.set_local_description("answer", answer).unwrap();
  wait_for(&b, &mut eb, gathered).await;
  let (_, sdp) = b.local_description().unwrap();
  a.set_remote_description("answer", without(&sdp, |l| !keep_b(l)))
    .unwrap();
  wait_for(&a, &mut ea, opened).await;
  wait_for(&b, &mut eb, opened).await;
  let remote = eb
    .iter()
    .find_map(|e| match e {
      JsEvent::DataChannelOpen { channel, .. } => Some(*channel),
      _ => None,
    })
    .unwrap();
  a.send(id, true, b"over tcp").unwrap();
  b.send(remote, false, &[9u8; 50_000]).unwrap();
  assert_eq!(
    wait_messages(&b, &mut eb, 1).await[0].1,
    b"over tcp".to_vec()
  );
  assert_eq!(wait_messages(&a, &mut ea, 1).await[0].1, vec![9u8; 50_000]);
  for peer in [&a, &b] {
    assert!(peer.selected_is_tcp_for_tests(), "the selected pair is TCP");
  }
  a.close();
  b.close();
  net_a.close();
  net_b.close();
}

#[tokio::test]
async fn ice_tcp_offerer_listens_answerer_dials() {
  // A (offering, controlling) keeps only its passive candidate, B only its
  // active one: B dials A's listener and A's agent learns B as a
  // peer-reflexive active candidate.
  ice_tcp_round_trip(
    |l| !is_udp(l) && l.contains("tcptype passive"),
    |l| !is_udp(l) && l.contains("tcptype active"),
  )
  .await;
}

#[tokio::test]
async fn ice_tcp_offerer_dials_answerer_listens() {
  ice_tcp_round_trip(
    |l| !is_udp(l) && l.contains("tcptype active"),
    |l| !is_udp(l) && l.contains("tcptype passive"),
  )
  .await;
}

#[tokio::test]
async fn ice_tcp_listener_closes_streams_that_are_not_ice() {
  use tokio::io::AsyncReadExt;
  use tokio::io::AsyncWriteExt;
  let net = loopback_network(Some(0));
  let addr: SocketAddr = net.tcp_addresses()[0].parse().unwrap();
  let closed = |mut s: tokio::net::TcpStream| async move {
    let mut buf = [0u8; 16];
    tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
      .await
      .map(|r| matches!(r, Ok(0) | Err(_)))
      .unwrap_or(false)
  };
  // Not STUN.
  let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
  s.write_all(&[0, 4, 22, 254, 253, 0]).await.unwrap();
  assert!(
    closed(s).await,
    "a DTLS-looking first frame closes the stream"
  );
  // A binding request for a ufrag no connection has.
  let mut req = vec![0x00, 0x01, 0x00, 0x10, 0x21, 0x12, 0xa4, 0x42];
  req.extend_from_slice(&[7u8; 12]);
  req.extend_from_slice(&[0x00, 0x06, 0x00, 0x09]);
  req.extend_from_slice(b"nope:peer\0\0\0");
  let mut framed = (req.len() as u16).to_be_bytes().to_vec();
  framed.extend_from_slice(&req);
  let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
  s.write_all(&framed).await.unwrap();
  assert!(closed(s).await, "an unknown ufrag closes the stream");
  // A frame longer than a binding request could be.
  let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
  s.write_all(&[0xff, 0xff]).await.unwrap();
  s.write_all(&[0u8; 4096]).await.unwrap();
  assert!(
    closed(s).await,
    "an oversized first frame closes the stream"
  );
  net.close();
}

#[tokio::test]
async fn ice_tcp_needs_a_tcp_network() {
  // Without `tcpPort` nothing about TCP changes: no TCP candidates, and the
  // peer's are skipped.
  let net = loopback_network(None);
  let a = peer_on(&net);
  let mut ea = Vec::new();
  a.create_data_channel("x", reliable()).unwrap();
  let offer = a.create_offer(false).unwrap();
  a.set_local_description("offer", offer).unwrap();
  wait_for(&a, &mut ea, gathered).await;
  let (_, sdp) = a.local_description().unwrap();
  assert!(!sdp.contains(" tcp "), "{sdp}");
  a.close();
  net.close();
}

/// mDNS between two connections on this machine's LAN address: A publishes
/// its host candidates under `.local` names, B resolves them. It binds
/// `0.0.0.0:5353` and sends to the IPv4 mDNS group, which a plain
/// `cargo test` on a developer's machine shouldn't do, so it is ignored by
/// default and CI's Linux leg runs it with `--ignored`
/// (`.github/workflows/denext_tests_os.yml`, job `webrtc-mdns`, our Linux
/// pool only).
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "binds 0.0.0.0:5353 and uses IPv4 multicast; CI's webrtc-mdns job runs it with --ignored"]
async fn mdns_host_candidates_resolve_between_peers() {
  use crate::net::NetworkArg;
  use crate::net::NetworkOptions;
  struct LanGuard;
  impl NetGuard for LanGuard {
    fn connect(&self, _addr: SocketAddr) -> Result<(), String> {
      Ok(())
    }
    fn listen(&self, _ip: IpAddr) -> Result<(), String> {
      Ok(())
    }
    fn interfaces(&self) -> Result<(), String> {
      Ok(())
    }
  }
  let options = |mdns| {
    NetworkOptions::parse(NetworkArg {
      include_loopback: false,
      mdns,
      ..Default::default()
    })
    .unwrap()
  };
  let net_a = crate::net::Network::new(options(Some(true)), &LanGuard).unwrap();
  let net_b = crate::net::Network::new(options(None), &LanGuard).unwrap();
  let new = |net: &Rc<crate::net::Network>| {
    Peer::new(
      PeerOptions {
        network: Some(net.clone()),
        ..Default::default()
      },
      Rc::new(LanGuard),
    )
    .unwrap()
  };
  let (a, b) = (new(&net_a), new(&net_b));
  let (mut ea, mut eb) = (Vec::new(), Vec::new());
  let (id, _) = a.create_data_channel("mdns", reliable()).unwrap();
  let offer = a.create_offer(false).unwrap();
  a.set_local_description("offer", offer).unwrap();
  wait_for(&a, &mut ea, gathered).await;
  let (_, sdp) = a.local_description().unwrap();
  let host_lines: Vec<&str> = sdp
    .lines()
    .filter(|l| l.starts_with("a=candidate:") && l.contains("typ host"))
    .collect();
  assert!(!host_lines.is_empty(), "{sdp}");
  for line in &host_lines {
    let address = line.split_ascii_whitespace().nth(4).unwrap();
    assert!(address.ends_with(".local"), "published by name: {line}");
  }
  b.set_remote_description("offer", sdp).unwrap();
  let answer = b.create_answer().unwrap();
  b.set_local_description("answer", answer).unwrap();
  wait_for(&b, &mut eb, gathered).await;
  let (_, sdp) = b.local_description().unwrap();
  // B's literal host candidates are left out, so A only reaches B as the
  // peer-reflexive source of B's checks to the name it resolved. So is the
  // answer's `a=end-of-candidates`, as from a peer that trickles its
  // candidates and hasn't finished: told that a peer with no candidate is
  // done, A fails its empty check list at once (the driver's
  // `fail_without_pairs`, and the engine's checklist failure at
  // end-of-candidates, RFC 8838 section 8), and whether B's first check
  // reaches A before that is a race (won with the 0.22.0+brainwires.5
  // engine, which had no checklist failure; rtci 0.1's RFC 8445 check list
  // added it).
  let answer: String = without(&sdp, |l| l.contains("typ host"))
    .split_inclusive('\n')
    .filter(|l| l.trim_end() != "a=end-of-candidates")
    .collect();
  a.set_remote_description("answer", answer).unwrap();
  wait_for(&a, &mut ea, opened).await;
  wait_for(&b, &mut eb, opened).await;
  a.send(id, true, b"via mdns").unwrap();
  assert_eq!(
    wait_messages(&b, &mut eb, 1).await[0].1,
    b"via mdns".to_vec()
  );
  a.close();
  b.close();
  net_a.close();
  net_b.close();
}

#[test]
fn mdns_is_off_without_its_permissions() {
  // The unit tests' guard refuses interface enumeration: `.local` candidates
  // are skipped, as before, and no mDNS socket is opened.
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  rt.block_on(async {
    let a = new_peer();
    let b = new_peer();
    b.create_data_channel("x", reliable()).unwrap();
    let offer = b.create_offer(false).unwrap();
    b.set_local_description("offer", offer.clone()).unwrap();
    a.set_remote_description("offer", offer).unwrap();
    a.add_ice_candidate(
      "candidate:2 1 udp 2130706431 0a1b2c3d-0000-4000-8000-000000000001.local 40001 typ host"
        .to_string(),
      Some("0".to_string()),
      Some(0),
      None,
    )
    .unwrap();
    assert!(!a.mdns_open_for_tests());
    a.close();
    b.close();
  });
}

#[test]
fn stats_epoch_fields_are_relative_to_the_request() {
  let stats = epoch_fields_relative(
    serde_json::json!({
      "id": "PC",
      "type": "peer-connection",
      "timestamp": 1794904159643.25,
      "lastPacketReceivedTimestamp": 1794904157643.0,
      "bytesSent": 7,
    }),
    1794904159643.0,
  );
  assert!((stats["timestamp"].as_f64().unwrap() - 0.25).abs() < 1e-6);
  assert_eq!(stats["lastPacketReceivedTimestamp"], -2000.0);
  assert_eq!(stats["bytesSent"], 7);
}

// A report's own timestamp is the moment it was asked for, to well under a
// millisecond (the engine used to truncate it to whole milliseconds).
#[test]
fn stats_timestamp_is_the_request_time() {
  let peer = new_peer();
  let stats = peer.stats(None).unwrap();
  let pc = stats
    .iter()
    .find(|s| s["type"] == "peer-connection")
    .expect("a peer-connection entry");
  let offset = pc["timestamp"].as_f64().unwrap();
  assert!((0.0..50.0).contains(&offset), "{offset}");
}

#[test]
fn gathering_needs_a_live_media_section() {
  use crate::peer::has_live_media_section;
  let head = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=-\r\nt=0 0\r\n";
  assert!(!has_live_media_section(head));
  assert!(has_live_media_section(&format!(
    "{head}m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n"
  )));
  assert!(!has_live_media_section(&format!(
    "{head}m=audio 0 UDP/TLS/RTP/SAVPF 0\r\na=mid:0\r\n"
  )));
  assert!(has_live_media_section(&format!(
    "{head}m=audio 0 UDP/TLS/RTP/SAVPF 0\r\na=bundle-only\r\n"
  )));
}

// The interceptor chain `media_stack` builds delivers an inbound key-frame
// request (PLI) to the application, past every interceptor: no slot holds the
// RTCP terminus (a nested interceptor wrapped around `NoopInterceptor` would
// end all inbound RTCP in its slot, NACKs and receiver reports included).
#[test]
fn media_chain_delivers_key_frame_requests() {
  use rtci::interceptor::AttributedPacket;
  use rtci::interceptor::Packet;
  use rtci::interceptor::TaggedPacket;
  use rtci::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
  use rtci::sansio::Protocol;
  let (_engine, registry) = crate::media::media_stack().unwrap();
  for (slot, name) in registry.slots() {
    assert!(!name.contains("Noop"), "{slot:?} holds {name}");
  }
  let mut chain = registry.build();
  chain
    .handle_read(TaggedPacket {
      now: std::time::Instant::now(),
      transport: rtci::shared::TransportContext::default(),
      message: AttributedPacket::new(Packet::Rtcp(vec![Box::new(
        PictureLossIndication {
          sender_ssrc: 1,
          media_ssrc: 2,
        },
      )])),
    })
    .unwrap();
  let mut delivered = 0;
  while let Some(read) = chain.poll_read() {
    if let Packet::Rtcp(packets) = &read.message.packet
      && packets
        .iter()
        .any(|p| p.as_any().downcast_ref::<PictureLossIndication>().is_some())
    {
      delivered += 1;
    }
  }
  assert_eq!(delivered, 1, "the PLI reached the application");
}

/// The four description slots: a current description stays while a new offer
/// is pending, and a rollback leaves it in place.
#[tokio::test]
async fn description_slots_keep_the_current_pair_during_renegotiation() {
  let (a, _b, _, _, _) = connect(reliable()).await;
  let slots = a.descriptions();
  assert_eq!(slots.current_local.as_ref().unwrap().0, "offer");
  assert_eq!(slots.current_remote.as_ref().unwrap().0, "answer");
  assert!(slots.pending_local.is_none());
  assert!(slots.pending_remote.is_none());
  let current = slots.current_local.unwrap().1;

  let offer = a.create_offer(true).unwrap();
  a.set_local_description("offer", offer).unwrap();
  let slots = a.descriptions();
  assert_eq!(slots.pending_local.as_ref().unwrap().0, "offer");
  assert_eq!(slots.current_remote.as_ref().unwrap().0, "answer");
  // `localDescription` is the pending one; the current one is still there.
  assert_eq!(a.local_description(), slots.pending_local);
  let ufrag = |sdp: &str| {
    sdp
      .lines()
      .find_map(|l| l.strip_prefix("a=ice-ufrag:"))
      .unwrap()
      .to_string()
  };
  assert_eq!(ufrag(&slots.current_local.unwrap().1), ufrag(&current));
  assert_ne!(ufrag(&slots.pending_local.unwrap().1), ufrag(&current));

  a.set_local_description("rollback", String::new()).unwrap();
  let slots = a.descriptions();
  assert!(slots.pending_local.is_none());
  assert_eq!(ufrag(&slots.current_local.unwrap().1), ufrag(&current));
}

/// The selected pair's candidates are stats objects of the same report, also
/// when the remote one was learned from a connectivity check.
#[tokio::test]
async fn stats_selected_pair_candidates_are_in_the_report() {
  let (a, _b, _, _, _) = connect(reliable()).await;
  let stats = a.stats(None).unwrap();
  let ids: Vec<&str> = stats
    .iter()
    .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
    .collect();
  let pair = stats
    .iter()
    .find(|s| {
      s.get("type").and_then(|v| v.as_str()) == Some("candidate-pair")
        && s.get("nominated").and_then(|v| v.as_bool()) == Some(true)
    })
    .expect("a nominated pair");
  for (member, prefix) in [
    ("localCandidateId", "RTCLocalIceCandidate_"),
    ("remoteCandidateId", "RTCRemoteIceCandidate_"),
  ] {
    let id = pair.get(member).and_then(|v| v.as_str()).unwrap();
    let full = format!("{prefix}{id}");
    assert!(
      ids.contains(&id) || ids.contains(&full.as_str()),
      "{member} {id} is not in the report: {ids:?}"
    );
  }
}

/// A remote media section's streams: its `a=msid` stream ids, none for the
/// id `-`, and one default stream when the section has no `a=msid` at all.
#[test]
fn remote_sections_streams() {
  let sdp = "v=0\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
a=msid:one track\r\n\
a=msid:two track\r\n\
a=msid:one track\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
a=msid:- track\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:2\r\n\
a=sendonly\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=mid:3\r\n";
  let sections = crate::media::sections(sdp);
  assert_eq!(sections["0"].streams, vec!["one", "two"]);
  assert!(sections["1"].streams.is_empty());
  assert_eq!(sections["2"].streams, vec!["default"]);
  assert_eq!(sections["2"].direction, "sendonly");
  assert!(sections["3"].streams.is_empty());
}

/// A datagram from `from` to `local`: a STUN-looking one (a first
/// byte below 4, not a valid message) or not.
fn datagram(local: SocketAddr, from: SocketAddr, stun: bool) -> Datagram {
  let first = if stun { 0u8 } else { 0x80 };
  Datagram {
    local,
    from,
    stream: None,
    via: Via::Socket,
    data: bytes::BytesMut::from(&[first, 1, 0, 0, 0, 0, 0, 0][..]),
    closed: None,
    ecn: None,
  }
}

/// On a wildcard socket, datagrams from addresses nobody vouched for (no
/// signalled candidate, no permitted send) cache no route, STUN-looking or
/// not; a vouched-for address gets one, and the cache stays bounded however
/// many vouched-for addresses there are.
#[tokio::test]
async fn route_cache_is_bounded_and_only_for_known_remotes() {
  let peer = new_peer();
  let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
  let local = socket.local_addr().unwrap();
  // Two host addresses, so the route would be probed.
  let hosts: Vec<IpAddr> =
    vec!["127.0.0.1".parse().unwrap(), "127.0.0.2".parse().unwrap()];
  let mut core = peer.core.borrow_mut();
  core.add_endpoint_for_test(socket, hosts);

  for i in 0..(MAX_ROUTES as u32 * 4) {
    let from = SocketAddr::new(
      IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + i)),
      40000,
    );
    core.handle_datagram_for_test(datagram(local, from, i % 2 == 0));
  }
  assert_eq!(core.route_count_for_test(), 0);

  let known: SocketAddr = "127.0.0.1:40001".parse().unwrap();
  core.mark_allowed(known);
  core.handle_datagram_for_test(datagram(local, known, true));
  assert_eq!(core.route_count_for_test(), 1);

  for i in 0..(MAX_ROUTES as u32 + 100) {
    let from = SocketAddr::new(
      IpAddr::from(std::net::Ipv4Addr::from(0x7f01_0000 + i)),
      40000,
    );
    core.mark_allowed(from);
    core.handle_datagram_for_test(datagram(local, from, true));
    assert!(core.route_count_for_test() <= MAX_ROUTES);
  }
}

/// A description longer than the engine's 1 MiB limit is refused as an
/// OperationError (what an unparsable one gets) by both setters, before the
/// engine sees it; the limit itself, a rollback and a bad type are unchanged.
#[tokio::test]
async fn oversized_descriptions_are_refused_before_the_engine() {
  let a = new_peer();
  a.create_data_channel("x", reliable()).unwrap();
  let offer = a.create_offer(false).unwrap();
  let mut big = offer.clone();
  big.push_str(&"a=x-pad:".repeat(MAX_SDP_BYTES / 8 + 1));
  assert!(big.len() > MAX_SDP_BYTES);

  for kind in ["offer", "answer", "pranswer"] {
    let err = a.set_local_description(kind, big.clone()).unwrap_err();
    assert!(matches!(err, WebRtcError::Operation(_)), "{kind}: {err:?}");
    let b = new_peer();
    let err = b.set_remote_description(kind, big.clone()).unwrap_err();
    assert!(matches!(err, WebRtcError::Operation(_)), "{kind}: {err:?}");
    assert!(err.to_string().contains("bytes"), "{err}");
  }
  // Nothing was applied: the real offer still sets.
  a.set_local_description("offer", offer).unwrap();

  // A bad type is still a TypeError, whatever the text's size.
  let err = new_peer().set_remote_description("bogus", big).unwrap_err();
  assert!(matches!(err, WebRtcError::Type(_)), "{err:?}");

  // Exactly the limit passes the size check (and fails only as an SDP).
  let exact = "v".repeat(MAX_SDP_BYTES);
  assert!(crate::peer::check_sdp_size("offer", &exact).is_ok());
  assert!(crate::peer::check_sdp_size("rollback", &(exact + "v")).is_ok());
}

fn rtp_header(seq: u16, ts: u32, marker: bool) -> rtci::rtp::Header {
  rtci::rtp::Header {
    sequence_number: seq,
    timestamp: ts,
    marker,
    ..Default::default()
  }
}

#[test]
fn received_video_counts_frames_whose_packets_all_arrived() {
  let mut video = crate::media::ReceivedVideo::default();
  // The first frame's start can't be told: not counted.
  assert!(!video.packet(&rtp_header(10, 100, true)));
  // A two-packet frame, then a one-packet frame.
  assert!(!video.packet(&rtp_header(11, 200, false)));
  assert!(video.packet(&rtp_header(12, 200, true)));
  assert!(video.packet(&rtp_header(13, 300, true)));
  // A packet lost inside a frame, and the frame after a loss.
  assert!(!video.packet(&rtp_header(14, 400, false)));
  assert!(!video.packet(&rtp_header(16, 400, true)));
  assert!(video.packet(&rtp_header(17, 500, true)));
  // A frame whose first packet was lost.
  assert!(!video.packet(&rtp_header(19, 600, true)));
  assert!(video.packet(&rtp_header(20, 700, true)));
  assert_eq!(video.frames_received, 4);
  // Sequence numbers wrap.
  let mut wrap = crate::media::ReceivedVideo::default();
  assert!(!wrap.packet(&rtp_header(u16::MAX - 1, 100, true)));
  assert!(!wrap.packet(&rtp_header(u16::MAX, 200, false)));
  assert!(wrap.packet(&rtp_header(0, 200, true)));
  assert_eq!(wrap.frames_received, 1);
}

#[test]
fn received_video_takes_packets_without_a_payload_as_present() {
  // Padding, or a RED stream's ULPFEC packet (rtci 0.2 delivers it
  // emptied): a sequence number of the stream, no frame.
  let mut video = crate::media::ReceivedVideo::default();
  assert!(!video.packet(&rtp_header(10, 100, true)));
  video.empty(&rtp_header(11, 100, false));
  assert!(!video.packet(&rtp_header(12, 200, false)));
  video.empty(&rtp_header(13, 9000, false));
  assert!(video.packet(&rtp_header(14, 200, true)));
  // One after a gap: the next frame's start may be what was lost.
  video.empty(&rtp_header(16, 200, false));
  assert!(!video.packet(&rtp_header(17, 300, true)));
  assert!(video.packet(&rtp_header(18, 400, true)));
  assert_eq!(video.frames_received, 2);
}

#[test]
fn sent_video_counts_what_the_frames_show() {
  let mut video = crate::media::SentVideo::default();
  let now = std::time::Instant::now();
  // A VP8 key frame (frame tag bit 0 clear, start code, 320x240) and two
  // small delta frames.
  let mut key =
    vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];
  key.resize(100, 0);
  let delta = vec![0x31u8; 10];
  video.frame("video/VP8", &delta, true, now);
  video.frame("video/VP8", &delta, true, now);
  video.frame("video/VP8", &key, true, now);
  video.frame("video/VP8", &delta, false, now);
  assert_eq!(video.frames_encoded, 4);
  assert_eq!(video.key_frames_encoded, 1);
  assert_eq!(video.frames_sent, 3);
  assert_eq!(video.huge_frames_sent, 1);
  assert_eq!(video.size, Some((320, 240)));
  assert_eq!(video.frames_per_second(now), 4.0);
  assert_eq!(
    video.frames_per_second(now + std::time::Duration::from_millis(1001)),
    0.0
  );
}

// The packets a connection's pacer refused (a full queue) are the selected
// candidate pair's `packetsDiscardedOnSend` / `bytesDiscardedOnSend`, added to
// what the engine counted; another pair's are left alone.
#[test]
fn pacer_refusals_are_the_selected_pairs_discards() {
  use serde_json::json;
  let mut stats = vec![
    json!({ "type": "transport", "id": "T", "selectedCandidatePairId": "P1" }),
    json!({
      "type": "candidate-pair",
      "id": "P1",
      "packetsDiscardedOnSend": 1,
      "bytesDiscardedOnSend": 100,
    }),
    json!({
      "type": "candidate-pair",
      "id": "P2",
      "packetsDiscardedOnSend": 0,
      "bytesDiscardedOnSend": 0,
    }),
  ];
  crate::peer::add_pacer_refusals(&mut stats, (0, 0));
  assert_eq!(stats[1]["packetsDiscardedOnSend"], 1);
  crate::peer::add_pacer_refusals(&mut stats, (3, 3036));
  assert_eq!(stats[1]["packetsDiscardedOnSend"], 4);
  assert_eq!(stats[1]["bytesDiscardedOnSend"], 3136);
  assert_eq!(stats[2]["packetsDiscardedOnSend"], 0);
  assert_eq!(stats[2]["bytesDiscardedOnSend"], 0);
}

fn connected(e: &JsEvent) -> bool {
  matches!(e, JsEvent::ConnectionState { state } if state == "connected")
}

/// `a` sends video to `b` (one sendonly transceiver with a track), full
/// (non-trickle) descriptions; returns both peers once both are connected,
/// with `a`'s and `b`'s transceiver keys and the negotiated VP8 payload type.
async fn connect_video() -> (Rc<Peer>, Rc<Peer>, u32, u32, u8) {
  let a = new_peer();
  let b = new_peer();
  let mut ea = Vec::new();
  let mut eb = Vec::new();
  let a_key = a
    .media(|core| {
      core.add_transceiver(crate::media::TransceiverArg {
        kind: "video".to_string(),
        track: Some(crate::media::TrackArg {
          id: "video-track".to_string(),
          label: "video".to_string(),
        }),
        direction: "sendonly".to_string(),
        streams: vec!["stream".to_string()],
        send_encodings: vec![],
        codec: None,
        add_track: true,
      })
    })
    .unwrap();
  let offer = a.create_offer(false).unwrap();
  a.set_local_description("offer", offer).unwrap();
  wait_for(&a, &mut ea, gathered).await;
  let (_, sdp) = a.local_description().unwrap();
  b.set_remote_description("offer", sdp).unwrap();
  let answer = b.create_answer().unwrap();
  b.set_local_description("answer", answer).unwrap();
  wait_for(&b, &mut eb, gathered).await;
  let (_, sdp) = b.local_description().unwrap();
  let vp8 = sdp
    .lines()
    .find_map(|l| {
      let rest = l.strip_prefix("a=rtpmap:")?;
      let (pt, codec) = rest.split_once(' ')?;
      codec
        .eq_ignore_ascii_case("VP8/90000")
        .then(|| pt.parse::<u8>().ok())?
    })
    .expect("the answer has VP8");
  a.set_remote_description("answer", sdp).unwrap();
  wait_for(&a, &mut ea, connected).await;
  wait_for(&b, &mut eb, connected).await;
  let b_key = b
    .media(|core| Ok(core.transceivers()))
    .unwrap()
    .into_iter()
    .find(|t| t.kind == "video")
    .expect("b has the video transceiver")
    .key;
  (a, b, a_key, b_key, vp8)
}

/// One-packet VP8 frames written to a sender, in RTP sequence.
struct Vp8Writer {
  key: u32,
  payload_type: u8,
  seq: u16,
  timestamp: u32,
}

impl Vp8Writer {
  fn write(&mut self, peer: &Rc<Peer>, key_frame: bool) {
    // The VP8 payload descriptor (S=1), then a key frame's tag and start
    // code (640x480) or a delta frame's tag (bit 0 set).
    let payload: &[u8] = if key_frame {
      &[0x10, 0, 0, 0, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01]
    } else {
      &[0x10, 0x01, 0x02, 0x03, 0x04]
    };
    let packet = rtci::rtp::packet::Packet {
      header: rtci::rtp::Header {
        version: 2,
        marker: true,
        payload_type: self.payload_type,
        sequence_number: self.seq,
        timestamp: self.timestamp,
        ..Default::default()
      },
      payload: bytes::Bytes::copy_from_slice(payload),
    };
    let bytes = rtci::shared::marshal::Marshal::marshal(&packet).unwrap();
    let key = self.key;
    peer.media(|core| core.write_rtp(key, &bytes)).unwrap();
    self.seq = self.seq.wrapping_add(1);
    self.timestamp = self.timestamp.wrapping_add(3000);
  }

  /// The next packet's sequence number skips `n`: they are lost on the way.
  fn lose(&mut self, n: u16) {
    self.seq = self.seq.wrapping_add(n);
    self.timestamp = self.timestamp.wrapping_add(3000 * u32::from(n));
  }
}

/// The PLIs a receiver sent for its video stream (`inbound-rtp` `pliCount`).
fn plis_sent(peer: &Rc<Peer>) -> u64 {
  peer
    .stats(None)
    .unwrap()
    .iter()
    .find(|s| s["type"] == "inbound-rtp" && s["kind"] == "video")
    .and_then(|s| s["pliCount"].as_u64())
    .unwrap_or(0)
}

/// Collects `peer`'s events for `duration`.
async fn events_for(
  peer: &Rc<Peer>,
  seen: &mut Vec<JsEvent>,
  duration: Duration,
) {
  let _ = tokio::time::timeout(duration, async {
    loop {
      match peer.next_events().await {
        Some(batch) => seen.extend(batch),
        None => return,
      }
    }
  })
  .await;
}

fn media_frames(events: &[JsEvent], key: u32) -> Vec<bool> {
  events
    .iter()
    .filter_map(|e| match e {
      JsEvent::MediaFrame {
        transceiver,
        key_frame,
        ..
      } if *transceiver == key => Some(*key_frame),
      _ => None,
    })
    .collect()
}

// A receiver whose frames lose one to a sequence gap asks the sender for a
// key frame (a PLI), the sender hears it as `keyframerequest`, the requests
// repeat at most once per KEY_FRAME_REQUEST_INTERVAL while the stream stays
// broken, and stop once a key frame repairs it.
#[tokio::test]
async fn a_lost_frame_asks_for_a_key_frame_until_one_comes() {
  let (a, b, a_key, b_key, pt) = connect_video().await;
  b.media(|core| core.receiver_read_frames(b_key, true))
    .unwrap();
  let mut w = Vp8Writer {
    key: a_key,
    payload_type: pt,
    seq: 1000,
    timestamp: 90_000,
  };
  let mut ea = Vec::new();
  let mut eb = Vec::new();
  // A key frame reaches the receiver (the first packets may beat the
  // receiver's binding: write until one does).
  for _ in 0..50 {
    w.write(&a, true);
    events_for(&b, &mut eb, Duration::from_millis(40)).await;
    if media_frames(&eb, b_key).contains(&true) {
      break;
    }
  }
  assert!(
    media_frames(&eb, b_key).contains(&true),
    "a key frame arrived"
  );
  // Frames in sequence: nothing is asked for.
  for _ in 0..5 {
    w.write(&a, false);
    events_for(&b, &mut eb, Duration::from_millis(20)).await;
  }
  assert_eq!(plis_sent(&b), 0, "no PLI while every frame arrives");
  // Two packets lost: the next delta frame can't be decoded.
  w.lose(2);
  w.write(&a, false);
  let lost_at = std::time::Instant::now();
  // However long the packet takes to arrive (a coarse timer on Windows).
  let deadline = lost_at + Duration::from_secs(5);
  while plis_sent(&b) == 0 && std::time::Instant::now() < deadline {
    events_for(&b, &mut eb, Duration::from_millis(20)).await;
  }
  assert_eq!(plis_sent(&b), 1, "one PLI for the loss");
  // The sender hears it.
  wait_for(&a, &mut ea, |e| {
    matches!(e, JsEvent::KeyFrameRequest { transceiver } if *transceiver == a_key)
  })
  .await;
  // More delta frames, still broken: the requests are paced.
  for _ in 0..10 {
    w.write(&a, false);
    events_for(&b, &mut eb, Duration::from_millis(20)).await;
  }
  let paced = 1
    + (lost_at.elapsed().as_millis()
      / crate::media::KEY_FRAME_REQUEST_INTERVAL.as_millis()) as u64;
  let sent = plis_sent(&b);
  assert!(
    (1..=paced).contains(&sent),
    "{sent} PLIs, at most {paced} by the interval"
  );
  // The key frame repairs the stream; later delta frames ask for nothing,
  // past the interval too.
  let frames = media_frames(&eb, b_key).len();
  w.write(&a, true);
  let deadline = std::time::Instant::now() + Duration::from_secs(5);
  while media_frames(&eb, b_key).len() == frames
    && std::time::Instant::now() < deadline
  {
    events_for(&b, &mut eb, Duration::from_millis(20)).await;
  }
  assert_eq!(media_frames(&eb, b_key)[frames..], [true], "the key frame");
  let after_key = plis_sent(&b);
  let interval = crate::media::KEY_FRAME_REQUEST_INTERVAL;
  let until = std::time::Instant::now() + interval + interval / 2;
  while std::time::Instant::now() < until {
    w.write(&a, false);
    events_for(&b, &mut eb, Duration::from_millis(30)).await;
  }
  assert_eq!(plis_sent(&b), after_key, "no PLI once repaired");
  a.close();
  b.close();
}

// Measures how long after ICE connects the DTLS handshake completes, on
// loopback (a few round trips of well under a millisecond each). Run it
// with `--ignored --nocapture`; it prints the delay per side over 20
// connections. Without the engine's first-flight timeout (rtci's
// `DtlsHandler::poll_timeout`) the DTLS client's ClientHello waits for the
// ICE agent's next check (200 ms).
#[tokio::test]
#[ignore = "a measurement, not a check"]
#[allow(clippy::print_stdout, reason = "the measurement is the output")]
async fn dtls_after_ice_connected_delay() {
  let mut delays: Vec<(u128, u128)> = Vec::new();
  for _ in 0..20 {
    let a = new_peer();
    let b = new_peer();
    a.create_data_channel("chat", reliable()).unwrap();
    let mut ea = Vec::new();
    let mut eb = Vec::new();
    let offer = a.create_offer(false).unwrap();
    a.set_local_description("offer", offer).unwrap();
    wait_for(&a, &mut ea, gathered).await;
    let (_, sdp) = a.local_description().unwrap();
    b.set_remote_description("offer", sdp).unwrap();
    let answer = b.create_answer().unwrap();
    b.set_local_description("answer", answer).unwrap();
    wait_for(&b, &mut eb, gathered).await;
    let (_, sdp) = b.local_description().unwrap();
    a.set_remote_description("answer", sdp).unwrap();
    // Each side's events, stamped as they arrive.
    let mut ice: [Option<std::time::Instant>; 2] = [None, None];
    let mut dtls: [Option<std::time::Instant>; 2] = [None, None];
    let ice_up = |e: &JsEvent| matches!(e, JsEvent::IceConnectionState { state } if state == "connected" || state == "completed");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while dtls.iter().any(Option::is_none)
      && std::time::Instant::now() < deadline
    {
      let (side, batch) = tokio::select! {
        batch = a.next_events() => (0, batch),
        batch = b.next_events() => (1, batch),
      };
      let now = std::time::Instant::now();
      for e in batch.unwrap_or_default() {
        if ice_up(&e) && ice[side].is_none() {
          ice[side] = Some(now);
        }
        if connected(&e) && dtls[side].is_none() {
          dtls[side] = Some(now);
        }
      }
    }
    let delay = |side: usize| {
      dtls[side]
        .zip(ice[side])
        .map(|(d, i)| d.saturating_duration_since(i).as_millis())
        .unwrap_or(u128::MAX)
    };
    delays.push((delay(0), delay(1)));
    a.close();
    b.close();
  }
  let mut offerer: Vec<u128> = delays.iter().map(|d| d.0).collect();
  let mut answerer: Vec<u128> = delays.iter().map(|d| d.1).collect();
  offerer.sort_unstable();
  answerer.sort_unstable();
  println!("ICE connected -> DTLS connected, ms (min / median / max):");
  println!(
    "  offerer  {} / {} / {}",
    offerer[0],
    offerer[offerer.len() / 2],
    offerer[offerer.len() - 1]
  );
  println!(
    "  answerer {} / {} / {}",
    answerer[0],
    answerer[answerer.len() / 2],
    answerer[answerer.len() - 1]
  );
}

/// The format tokens of each m= line of `sdp`, in order.
fn section_formats(sdp: &str) -> Vec<Vec<String>> {
  sdp
    .lines()
    .filter_map(|l| l.trim_end().strip_prefix("m="))
    .map(|m| {
      m.split_ascii_whitespace()
        .skip(3)
        .map(str::to_string)
        .collect()
    })
    .collect()
}

/// The payload types of `a=<attr>:<pt>` lines of each m-section of `sdp`.
fn section_attribute_formats(sdp: &str, attr: &str) -> Vec<Vec<String>> {
  let prefix = format!("a={attr}:");
  let mut out: Vec<Vec<String>> = Vec::new();
  for line in sdp.lines() {
    let line = line.trim_end();
    if line.starts_with("m=") {
      out.push(Vec::new());
    } else if let Some(rest) = line.strip_prefix(prefix.as_str())
      && let Some(last) = out.last_mut()
      && let Some(pt) = rest.split_ascii_whitespace().next()
    {
      last.push(pt.to_string());
    }
  }
  out
}

/// `sdp` with only the formats named in `keep` (codec names, any case) in
/// its audio and video sections, plus the retransmission formats that repair
/// them, as an offer from a browser that supports fewer codecs is.
fn offer_with_only(sdp: &str, keep: &[&str]) -> String {
  // Pass 1: per section, the kept payload types.
  let mut kept: Vec<Vec<String>> = Vec::new();
  let mut rtx: Vec<Vec<(String, String)>> = Vec::new();
  for line in sdp.lines() {
    let line = line.trim_end();
    if line.starts_with("m=") {
      kept.push(Vec::new());
      rtx.push(Vec::new());
    } else if let Some(rest) = line.strip_prefix("a=rtpmap:")
      && let Some((pt, codec)) = rest.split_once(' ')
      && let Some(name) = codec.split('/').next()
      && keep.iter().any(|k| k.eq_ignore_ascii_case(name))
    {
      kept.last_mut().unwrap().push(pt.to_string());
    } else if let Some(rest) = line.strip_prefix("a=fmtp:")
      && let Some((pt, params)) = rest.split_once(' ')
      && let Some(apt) = params
        .split(';')
        .find_map(|p| p.trim().strip_prefix("apt="))
    {
      rtx
        .last_mut()
        .unwrap()
        .push((pt.to_string(), apt.trim().to_string()));
    }
  }
  for (kept, rtx) in kept.iter_mut().zip(&rtx) {
    let repairs: Vec<String> = rtx
      .iter()
      .filter(|(_, apt)| kept.contains(apt))
      .map(|(pt, _)| pt.clone())
      .collect();
    kept.extend(repairs);
  }
  // Pass 2: rewrite the audio / video sections.
  let mut out = String::new();
  let mut index: Option<usize> = None;
  let mut media = false;
  for line in sdp.split_inclusive('\n') {
    let text = line.trim_end();
    if let Some(m) = text.strip_prefix("m=") {
      let i = index.map_or(0, |i| i + 1);
      index = Some(i);
      let fields: Vec<&str> = m.split_ascii_whitespace().collect();
      media = matches!(fields[0], "audio" | "video");
      if media {
        let formats: Vec<&str> = fields[3..]
          .iter()
          .copied()
          .filter(|f| kept[i].iter().any(|k| k == f))
          .collect();
        out.push_str(&format!(
          "m={} {} {} {}\r\n",
          fields[0],
          fields[1],
          fields[2],
          formats.join(" ")
        ));
        continue;
      }
    } else if media
      && let Some(i) = index
      && let Some((_, rest)) = text
        .split_once(':')
        .filter(|(a, _)| matches!(*a, "a=rtpmap" | "a=fmtp" | "a=rtcp-fb"))
      && let Some(pt) = rest.split_ascii_whitespace().next()
      && pt != "*"
      && !kept[i].iter().any(|k| k == pt)
    {
      continue;
    }
    out.push_str(line);
  }
  out
}

// DENEXT.md item 565, the WebKit case: an offer with VP8 alone, which the
// peer later leaves for VP9 (payload type 98) if our answer listed it. The
// engine's answer appended VP9 (and its RTX) and PCMU, which its receiver
// would drop as not negotiated; the answer as signalled lists only what the
// offer had, so the peer can never switch to them.
#[test]
fn answer_lists_only_the_offered_formats() {
  let offer = "v=0\r\n\
o=- 1 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1 2\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
a=mid:1\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtcp-fb:96 nack\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=mid:2\r\n\
a=sctp-port:5000\r\n";
  let answer = "v=0\r\n\
o=- 3 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1 2\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111 0\r\n\
a=mid:0\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
a=rtpmap:0 PCMU/8000\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97 98 99\r\n\
a=mid:1\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtcp-fb:96 nack\r\n\
a=rtcp-fb:* ack ccfb\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n\
a=rtpmap:98 VP9/90000\r\n\
a=fmtp:98 profile-id=0\r\n\
a=rtcp-fb:98 nack pli\r\n\
a=rtpmap:99 rtx/90000\r\n\
a=fmtp:99 apt=98\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=mid:2\r\n\
a=sctp-port:5000\r\n";
  let filtered = crate::peer::answer_with_offered_formats(answer, offer);
  let expected = "v=0\r\n\
o=- 3 2 IN IP4 127.0.0.1\r\n\
s=-\r\n\
t=0 0\r\n\
a=group:BUNDLE 0 1 2\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
a=rtpmap:111 opus/48000/2\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
a=mid:1\r\n\
a=rtpmap:96 VP8/90000\r\n\
a=rtcp-fb:96 nack\r\n\
a=rtcp-fb:* ack ccfb\r\n\
a=rtpmap:97 rtx/90000\r\n\
a=fmtp:97 apt=96\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
a=mid:2\r\n\
a=sctp-port:5000\r\n";
  assert_eq!(filtered, expected);
  // Nothing to drop: the answer is returned as it is.
  assert_eq!(
    crate::peer::answer_with_offered_formats(expected, offer),
    expected
  );
  // An RTX format the offer listed whose primary it did not goes too.
  let offer_rtx_only = offer.replace(
    "m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 96 97 99\r\n",
  );
  assert_eq!(
    section_formats(&crate::peer::answer_with_offered_formats(
      answer,
      &offer_rtx_only
    ))[1],
    vec!["96", "97"]
  );
  // A section count that does not match the offer's: left as it is.
  let fewer = offer.split("m=application").next().unwrap();
  assert_eq!(
    crate::peer::answer_with_offered_formats(answer, fewer),
    answer
  );
}

// DENEXT.md item 565 end to end: a VP8-only, Opus-only offer gets an answer,
// a local description and receiver parameters with those formats (and VP8's
// RTX) alone; the engine applies the answer handed out, and a later offer
// from the answerer is not filtered.
#[tokio::test]
async fn answer_to_a_narrow_offer_lists_only_its_formats() {
  let a = new_peer();
  let b = new_peer();
  for kind in ["audio", "video"] {
    a.media(|core| {
      core.add_transceiver(crate::media::TransceiverArg {
        kind: kind.to_string(),
        track: Some(crate::media::TrackArg {
          id: format!("{kind}-track"),
          label: kind.to_string(),
        }),
        direction: "sendrecv".to_string(),
        streams: vec!["stream".to_string()],
        send_encodings: vec![],
        codec: None,
        add_track: true,
      })
    })
    .unwrap();
  }
  let full = a.create_offer(false).unwrap();
  assert!(
    full.contains(" VP9/90000"),
    "the engine offers more than VP8: {full}"
  );
  let offer = offer_with_only(&full, &["opus", "VP8"]);
  let offered = section_formats(&offer);
  assert_eq!(offered.len(), 2, "{offer}");
  assert!(offered.iter().all(|f| !f.is_empty()), "{offer}");
  b.set_remote_description("offer", offer.clone()).unwrap();
  let answer = b.create_answer().unwrap();
  let check = |sdp: &str| {
    let formats = section_formats(sdp);
    assert_eq!(formats.len(), offered.len(), "{sdp}");
    for (i, section) in formats.iter().enumerate() {
      assert!(!section.is_empty(), "{sdp}");
      for f in section {
        assert!(offered[i].contains(f), "{f} was not offered: {sdp}");
      }
      for attr in ["rtpmap", "fmtp", "rtcp-fb"] {
        for pt in &section_attribute_formats(sdp, attr)[i] {
          assert!(
            pt == "*" || offered[i].contains(pt),
            "a={attr}:{pt} was not offered: {sdp}"
          );
        }
      }
    }
    for codec in ["VP9/", "H264/", "AV1/", "PCMU/", "PCMA/", "G722/"] {
      assert!(!sdp.contains(codec), "{codec} in {sdp}");
    }
    assert!(sdp.contains(" VP8/90000"), "{sdp}");
    assert!(sdp.contains(" opus/48000"), "{sdp}");
  };
  check(&answer);
  // The engine compares the answer it is given with the one it made.
  b.set_local_description("answer", answer.clone()).unwrap();
  let (kind, local) = b.local_description().unwrap();
  assert_eq!(kind, "answer");
  check(&local);
  assert_eq!(section_formats(&local), section_formats(&answer));
  let current = b.descriptions().current_local.unwrap().1;
  check(&current);
  // The receivers are prepared for the answer's formats.
  let keys: Vec<u32> = b
    .media(|core| Ok(core.transceivers()))
    .unwrap()
    .into_iter()
    .map(|t| t.key)
    .collect();
  assert_eq!(keys.len(), 2);
  for (i, key) in keys.into_iter().enumerate() {
    let params = b.media(|core| core.receiver_parameters(key)).unwrap();
    let codecs = params["codecs"].as_array().cloned().unwrap_or_default();
    assert!(!codecs.is_empty(), "{params}");
    for c in codecs {
      let pt = c["payloadType"].as_u64().unwrap().to_string();
      assert!(
        offered.iter().any(|s| s.contains(&pt)),
        "receiver {i} lists {pt}, not offered: {params}"
      );
    }
  }
  // Offers are not filtered: the answerer's own offer is the engine's, and
  // the engine takes it.
  let reoffer = b.create_offer(false).unwrap();
  b.set_local_description("offer", reoffer).unwrap();
  a.close();
  b.close();
}

// DENEXT.md item 572: an answer that lists formats beyond the offer (as
// rtci's own answers do, RFC 9429 5.3.1) does not widen the codecs a sender
// reports. The offerer set VP8 alone as its codec preference; the answer it
// is given adds VP9, and sender getParameters() lists the engine's
// negotiated codecs (VP8, with the answer's payload type), not VP9 (WPT
// protocol/h265-level-id "Offer to send H265, answer to receive level-id=30
// results in level-id=30", which counts them).
#[tokio::test]
async fn sender_codecs_are_the_offered_ones_the_answer_accepted() {
  let a = new_peer();
  let b = new_peer();
  let vp8 = crate::media::capabilities("video", true)
    .unwrap()
    .expect("capabilities")
    .codecs
    .into_iter()
    .find(|c| c.mime_type.eq_ignore_ascii_case("video/VP8"))
    .expect("VP8");
  let key = a
    .media(|core| {
      let key = core.add_transceiver(crate::media::TransceiverArg {
        kind: "video".to_string(),
        track: Some(crate::media::TrackArg {
          id: "video-track".to_string(),
          label: "video".to_string(),
        }),
        direction: "sendonly".to_string(),
        streams: vec!["stream".to_string()],
        send_encodings: vec![],
        codec: None,
        add_track: true,
      })?;
      core.set_codec_preferences(key, vec![vp8])?;
      Ok(key)
    })
    .unwrap();
  let offer = a.create_offer(false).unwrap();
  assert!(!offer.contains(" VP9/90000"), "{offer}");
  a.set_local_description("offer", offer.clone()).unwrap();
  b.set_remote_description("offer", offer).unwrap();
  let answer = b.create_answer().unwrap();
  b.set_local_description("answer", answer.clone()).unwrap();
  assert!(!answer.contains(" VP9/90000"), "{answer}");
  // The answer as another stack writes it: VP9 on a payload type of its own.
  let pt = (96u8..=127)
    .find(|pt| {
      !answer.contains(&format!("a=rtpmap:{pt} "))
        && !answer.contains(&format!("a=fmtp:{pt} "))
    })
    .expect("a free payload type");
  let mut widened = String::new();
  let mut in_video = false;
  for line in answer.split_inclusive("\r\n") {
    let bare = line.trim_end();
    if bare.starts_with("m=") {
      in_video = bare.starts_with("m=video");
      if in_video {
        widened.push_str(&format!("{bare} {pt}\r\n"));
        continue;
      }
    }
    widened.push_str(line);
    if in_video && bare.starts_with("a=mid:") {
      widened.push_str(&format!(
        "a=rtpmap:{pt} VP9/90000\r\na=fmtp:{pt} profile-id=0\r\n"
      ));
    }
  }
  assert!(
    widened.contains(&format!("a=rtpmap:{pt} VP9/90000")),
    "{widened}"
  );
  a.set_remote_description("answer", widened.clone()).unwrap();
  let answered = section_formats(&widened)[0].clone();
  let params = a.media(|core| core.sender_parameters(key)).unwrap();
  let codecs = params["codecs"].as_array().cloned().unwrap_or_default();
  assert!(
    codecs.iter().any(|c| c["mimeType"]
      .as_str()
      .is_some_and(|m| m.eq_ignore_ascii_case("video/VP8"))),
    "{params}"
  );
  for c in &codecs {
    let mime = c["mimeType"].as_str().unwrap_or_default();
    assert!(!mime.eq_ignore_ascii_case("video/VP9"), "{params}");
    let codec_pt = c["payloadType"].as_u64().unwrap().to_string();
    assert!(answered.contains(&codec_pt), "{codec_pt}: {params}");
  }
  a.close();
  b.close();
}

// setCodecPreferences() takes what getCapabilities() lists, for audio too:
// a mono codec there has `channels: 1`, which the engine (whose rtpmap has
// no encoding parameter for it) holds as 0 (WPT
// RTCRtpTransceiver-setCodecPreferences "on audio transceiver with codecs
// returned from RTCRtpReceiver.getCapabilities('audio')").
#[test]
fn audio_codec_preferences_take_the_capabilities() {
  let peer = new_peer();
  for (kind, sender) in [
    ("audio", false),
    ("audio", true),
    ("video", false),
    ("video", true),
  ] {
    let codecs = crate::media::capabilities(kind, sender)
      .unwrap()
      .expect("capabilities")
      .codecs;
    assert!(
      codecs
        .iter()
        .any(|c| kind == "video" || c.channels == Some(1)),
      "{kind} lists a mono codec"
    );
    let mut reordered = codecs.clone();
    reordered.reverse();
    peer
      .media(|core| {
        let key = core.add_transceiver(crate::media::TransceiverArg {
          kind: kind.to_string(),
          track: None,
          direction: "sendrecv".to_string(),
          streams: vec![],
          send_encodings: vec![],
          codec: None,
          add_track: false,
        })?;
        core.set_codec_preferences(key, codecs)?;
        core.set_codec_preferences(key, reordered)
      })
      .unwrap_or_else(|e| panic!("{kind} (sender {sender}): {e:?}"));
  }
  peer.close();
}

// getCapabilities() lists each registered codec, the H.264 High profile
// levels included, though rtci 0.2 offers those with level 5.0's fmtp line
// (WPT protocol/codecs-filtered-by-direction: a sendonly or recvonly offer
// lists as many formats as getCapabilities() has codecs).
#[test]
fn capabilities_list_every_registered_video_codec() {
  let codecs = crate::media::capabilities("video", true)
    .unwrap()
    .expect("capabilities")
    .codecs;
  for level in ["640028", "640029", "64002a", "640032", "640033"] {
    assert!(
      codecs.iter().any(|c| c
        .sdp_fmtp_line
        .as_deref()
        .is_some_and(|f| f.contains(&format!("profile-level-id={level}")))),
      "H.264 {level}: {codecs:?}"
    );
  }
  let rtx: Vec<_> = codecs
    .iter()
    .filter(|c| c.mime_type.eq_ignore_ascii_case("video/rtx"))
    .collect();
  assert_eq!(rtx.len(), 1, "RTX once: {codecs:?}");
  assert_eq!(rtx[0].sdp_fmtp_line, None, "RTX without apt");
  // An offer lists one format per non-repair capability.
  let peer = new_peer();
  peer
    .media(|core| {
      core.add_transceiver(crate::media::TransceiverArg {
        kind: "video".to_string(),
        track: None,
        direction: "sendonly".to_string(),
        streams: vec![],
        send_encodings: vec![],
        codec: None,
        add_track: false,
      })
    })
    .unwrap();
  let offer = peer.create_offer(false).unwrap();
  let offered = offer
    .lines()
    .filter(|l| l.starts_with("a=rtpmap:") && !l.contains(" rtx/"))
    .count();
  let primaries = codecs
    .iter()
    .filter(|c| !c.mime_type.eq_ignore_ascii_case("video/rtx"))
    .count();
  assert_eq!(offered, primaries, "{offer}");
  peer.close();
}

// The audio level extensions are offered in the clear only, as browsers
// offer them, and getCapabilities() never lists RFC 6904's `encrypt` URI as
// an extension of its own.
#[test]
fn audio_level_extensions_are_offered_in_the_clear() {
  let peer = new_peer();
  peer
    .media(|core| {
      core.add_transceiver(crate::media::TransceiverArg {
        kind: "audio".to_string(),
        track: None,
        direction: "sendrecv".to_string(),
        streams: vec![],
        send_encodings: vec![],
        codec: None,
        add_track: false,
      })
    })
    .unwrap();
  let offer = peer.create_offer(false).unwrap();
  assert!(
    !offer.contains("urn:ietf:params:rtp-hdrext:encrypt"),
    "{offer}"
  );
  assert!(offer.contains("urn:ietf:params:rtp-hdrext:ssrc-audio-level"));
  peer.close();
  let caps = serde_json::to_value(
    crate::media::capabilities("audio", false)
      .unwrap()
      .expect("capabilities"),
  )
  .unwrap();
  let uris: Vec<&str> = caps["headerExtensions"]
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|h| h["uri"].as_str())
    .collect();
  assert!(
    !uris.contains(&"urn:ietf:params:rtp-hdrext:encrypt"),
    "{uris:?}"
  );
  assert_eq!(
    crate::media::extmap_uri(
      "4 urn:ietf:params:rtp-hdrext:encrypt urn:ietf:params:rtp-hdrext:ssrc-audio-level vad=on"
    ),
    Some("urn:ietf:params:rtp-hdrext:ssrc-audio-level")
  );
  assert_eq!(
    crate::media::extmap_uri("1/sendonly urn:ietf:params:rtp-hdrext:sdes:mid"),
    Some("urn:ietf:params:rtp-hdrext:sdes:mid")
  );
}

#[test]
fn repeated_simulcast_rids_are_dropped() {
  use crate::peer::simulcast_without_repeated_rids as dedupe;
  assert_eq!(dedupe("recv foo;bar;baz"), None);
  assert_eq!(dedupe("recv foo;bar;foo").as_deref(), Some("recv foo;bar"));
  assert_eq!(dedupe("recv a,~b;b;c").as_deref(), Some("recv a,~b;c"));
  assert_eq!(
    dedupe("send x;y recv z;x").as_deref(),
    Some("send x;y recv z")
  );
}

#[test]
fn attribute_names_are_tokens() {
  use crate::peer::is_attribute_name;
  assert!(is_attribute_name("ssrc"));
  assert!(is_attribute_name("x-google-flag"));
  assert!(!is_attribute_name("notssrc2526472737 cname"));
  assert!(!is_attribute_name(""));
}

// A remote offer whose attribute name is not a token (WPT
// RTCPeerConnection-connectionState hides the SSRCs as `a=notssrc<n>
// cname:...`) is applied, the line left out, as browsers ignore it; a
// repeated simulcast rid is ignored too, and remoteDescription keeps the
// line as signalled, while the answer sends each rid once, in the offer's
// order (WPT simulcast/negotiation-encodings "Duplicate rids in sRD(offer)
// are ignored": one `a=rid` line per `a=simulcast` entry, the repeat
// included).
#[tokio::test]
async fn remote_offer_with_unknown_lines_and_repeated_rids_applies() {
  let a = new_peer();
  a.media(|core| {
    core.add_transceiver(crate::media::TransceiverArg {
      kind: "video".to_string(),
      track: Some(crate::media::TrackArg {
        id: "video-track".to_string(),
        label: "video".to_string(),
      }),
      direction: "sendrecv".to_string(),
      streams: vec!["stream".to_string()],
      send_encodings: vec![],
      codec: None,
      add_track: true,
    })
  })
  .unwrap();
  let offer = a.create_offer(false).unwrap();
  a.close();
  let munged = offer.replace("a=ssrc:", "a=notssrc");
  assert!(munged.contains("a=notssrc"), "{offer}");
  let b = new_peer();
  b.set_remote_description("offer", munged).unwrap();
  b.close();
  let simulcast = offer.replace(
    "a=rtcp-mux\r\n",
    "a=rtcp-mux\r\na=rid:foo recv\r\na=rid:bar recv\r\na=rid:foo recv\r\na=simulcast:recv foo;bar;foo\r\n",
  );
  assert!(simulcast.contains("a=simulcast:"), "{offer}");
  let c = new_peer();
  c.media(|core| {
    core.add_transceiver(crate::media::TransceiverArg {
      kind: "video".to_string(),
      track: Some(crate::media::TrackArg {
        id: "answer-track".to_string(),
        label: "video".to_string(),
      }),
      direction: "sendrecv".to_string(),
      streams: vec!["answer-stream".to_string()],
      send_encodings: vec![],
      codec: None,
      add_track: true,
    })
  })
  .unwrap();
  c.set_remote_description("offer", simulcast).unwrap();
  let shown = c.remote_description().expect("remote description").1;
  assert!(shown.contains("a=simulcast:recv foo;bar;foo"), "{shown}");
  let answer = c.create_answer().unwrap();
  assert!(answer.contains("a=simulcast:send foo;bar\r\n"), "{answer}");
  assert_eq!(answer.matches("a=rid:foo send").count(), 1, "{answer}");
  c.close();
}

// RFC 8285 section 5: an extension is named by an absolute URI; a remote
// `a=extmap` line that names it otherwise (WPT protocol/rtp-headerextensions
// offers `bogus`) is not given to the engine.
#[test]
fn extmap_lines_name_absolute_uris() {
  use crate::peer::extmap_names_absolute_uris as absolute;
  assert!(absolute("1 urn:ietf:params:rtp-hdrext:sdes:mid"));
  assert!(absolute(
    "3/recvonly http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time"
  ));
  assert!(absolute(
    "4 urn:ietf:params:rtp-hdrext:encrypt urn:ietf:params:rtp-hdrext:ssrc-audio-level vad=on"
  ));
  assert!(!absolute("2 bogus"));
  assert!(!absolute("4 urn:ietf:params:rtp-hdrext:encrypt bogus"));
  assert!(!absolute("5 1bad:scheme"));
}
