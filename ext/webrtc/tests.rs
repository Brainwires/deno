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
      .stats()
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

  let stats = a.stats().unwrap();
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
  assert!(max >= 65_536, "maxMessageSize {max}");
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
  assert!(matches!(
    classify_remote_candidate("candidate:1 1 udp 1 abc.local 5 typ host"),
    Ok(RemoteCandidate::Ignore)
  ));
  assert!(matches!(
    classify_remote_candidate("candidate:1 1 udp 1 10.0.0.1 5 typ host"),
    Ok(RemoteCandidate::Udp(a)) if a == "10.0.0.1:5".parse::<SocketAddr>().unwrap()
  ));
}

#[tokio::test]
async fn buffered_amount_drains_to_zero() {
  let (a, b, id, mut ea, _eb) = connect(reliable()).await;
  a.set_buffered_amount_low_threshold(id, 1024);
  for _ in 0..64 {
    a.send(id, false, &[7u8; 16 * 1024]).unwrap();
  }
  assert!(a.buffered_amount(id) > 1024);
  wait_for(&a, &mut ea, |e| matches!(e, JsEvent::DataChannelLow { .. })).await;
  tokio::time::timeout(Duration::from_secs(20), async {
    while a.buffered_amount(id) > 0 {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .expect("bufferedAmount drains");
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
