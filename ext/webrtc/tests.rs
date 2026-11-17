// Copyright 2018-2026 the Deno authors. MIT license.

//! The driver end to end, without V8: two peer connections on loopback.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::rc::Rc;
use std::time::Duration;

use rtc::data_channel::RTCDataChannelInit;

use crate::IceServerSpec;
use crate::JsEvent;
use crate::NetGuard;
use crate::Peer;
use crate::PeerOptions;
use crate::ServerTransport;
use crate::WebRtcError;
use crate::peer::RemoteCandidate;
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
  assert_eq!(class(active, true, true), RemoteCandidate::Ignore);
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
  // peer-reflexive source of B's checks to the name it resolved.
  a.set_remote_description("answer", without(&sdp, |l| l.contains("typ host")))
    .unwrap();
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
