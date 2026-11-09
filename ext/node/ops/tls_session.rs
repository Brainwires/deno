// Copyright 2018-2026 the Deno authors. MIT license.

//! TLS session resumption for `node:tls` over rustls.
//!
//! Node exposes a TLS session as an opaque buffer (`'session'` event,
//! `getSession()`, `setSession()`, `options.session`). OpenSSL serializes the
//! whole session into it. rustls offers no public serialization of its client
//! session values, and those values are tied to the verifier `Arc` that
//! accepted them, so they could not be resumed from bytes in another process
//! anyway. Instead, every session rustls hands to the client store is kept in
//! a per-isolate [`ClientSessionRegistry`] under a random 128-bit id, and the
//! buffer handed to JS names that id (`deno-tls-session:<hex id>`). Passing
//! the buffer back to `setSession()` (or `options.session`) offers exactly
//! that session again, and only to a connection whose server name matches the
//! one it was issued for.
//!
//! Limits, compared with OpenSSL:
//! - a session buffer resumes only inside the process (isolate) that
//!   received it;
//! - TLS 1.3 tickets are single-use in rustls, so a TLS 1.3 session buffer
//!   resumes while its lineage (the tickets issued by the handshake that
//!   produced it and by connections resumed from it) has a ticket left;
//!   each resumed connection refills it. A TLS 1.2 session resumes any number
//!   of times;
//! - TLS 1.3 external PSKs (`pskCallback`) are not offered: rustls has no
//!   external PSK support.
//!
//! The server half is [`NodeTicketer`]: session tickets encrypted under the
//! server's 48-byte `ticketKeys` (the first 16 bytes name the key, the rest
//! derive an AES-128-GCM key), so servers that share keys accept each other's
//! tickets and `setTicketKeys()` rotates them.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use aws_lc_rs::aead;
use deno_tls::rustls;
use rand::RngCore;
use rustls::pki_types::ServerName;

/// Prefix of the session buffers handed to JS.
pub(crate) const SESSION_PREFIX: &[u8] = b"deno-tls-session:";

/// The registry keeps at most this many sessions; the oldest go first.
const MAX_SESSIONS: usize = 4096;

pub(crate) type SessionId = [u8; 16];

/// TLS 1.3 tickets are single-use values in rustls (not clonable), while a
/// Node session buffer may be offered again and again. Every ticket of one
/// lineage (a full handshake, plus every ticket issued on a connection that
/// resumed from it) goes into one pool shared by all of the lineage's
/// buffers, and offering any of those buffers takes the newest ticket.
type Tls13Pool = Arc<Mutex<VecDeque<rustls::client::Tls13ClientSessionValue>>>;

/// Tickets kept per lineage.
const MAX_POOLED_TICKETS: usize = 8;

enum StoredValue {
  Tls12(rustls::client::Tls12ClientSessionValue),
  Tls13(Tls13Pool),
}

struct StoredSession {
  server_name: ServerName<'static>,
  value: StoredValue,
  ticket: Vec<u8>,
}

#[derive(Default)]
struct RegistryInner {
  sessions: HashMap<SessionId, StoredSession>,
  order: VecDeque<SessionId>,
}

/// Per-isolate store of every client session rustls produced, by id.
#[derive(Default)]
pub(crate) struct ClientSessionRegistry {
  inner: Mutex<RegistryInner>,
}

impl std::fmt::Debug for ClientSessionRegistry {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ClientSessionRegistry")
      .finish_non_exhaustive()
  }
}

impl ClientSessionRegistry {
  fn insert(&self, session: StoredSession) -> SessionId {
    let mut id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut id);
    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
    while inner.order.len() >= MAX_SESSIONS {
      if let Some(old) = inner.order.pop_front() {
        inner.sessions.remove(&old);
      }
    }
    inner.order.push_back(id);
    inner.sessions.insert(id, session);
    id
  }

  fn with<R>(
    &self,
    id: &SessionId,
    f: impl FnOnce(&mut StoredSession) -> R,
  ) -> Option<R> {
    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
    inner.sessions.get_mut(id).map(f)
  }

  #[cfg(test)]
  fn len(&self) -> usize {
    self.inner.lock().unwrap().sessions.len()
  }
}

/// Encode a session id as the buffer handed to JS.
pub(crate) fn encode_session_buffer(id: &SessionId) -> Vec<u8> {
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let mut out = Vec::with_capacity(SESSION_PREFIX.len() + 32);
  out.extend_from_slice(SESSION_PREFIX);
  for b in id {
    out.push(HEX[(b >> 4) as usize]);
    out.push(HEX[(b & 0xf) as usize]);
  }
  out
}

/// Parse a buffer produced by [`encode_session_buffer`]. Anything else
/// (including an OpenSSL-serialized session from another runtime) is `None`,
/// which Node treats as a session that simply does not resume.
pub(crate) fn decode_session_buffer(buf: &[u8]) -> Option<SessionId> {
  let hex = buf.strip_prefix(SESSION_PREFIX)?;
  if hex.len() != 32 {
    return None;
  }
  let mut id = [0u8; 16];
  for (i, pair) in hex.chunks(2).enumerate() {
    let hi = (pair[0] as char).to_digit(16)?;
    let lo = (pair[1] as char).to_digit(16)?;
    id[i] = (hi * 16 + lo) as u8;
  }
  Some(id)
}

/// The session ticket inside a rustls client session value.
///
/// rustls keeps the ticket bytes crate-private, but its `Debug` output of
/// `ClientSessionCommon` prints them as hex (`ticket: <hex>`) and never prints
/// the secret. `getTLSTicket()` needs the bytes, so read them from there; the
/// unit test below fails if a rustls update changes that format.
fn ticket_from_debug(debug: &str) -> Vec<u8> {
  const ANCHOR: &str = "ClientSessionCommon { ticket: ";
  let Some(start) = debug.find(ANCHOR) else {
    return Vec::new();
  };
  let hex: Vec<u8> = debug[start + ANCHOR.len()..]
    .bytes()
    .take_while(|b| b.is_ascii_hexdigit())
    .collect();
  hex
    .chunks_exact(2)
    .filter_map(|pair| {
      let hi = (pair[0] as char).to_digit(16)?;
      let lo = (pair[1] as char).to_digit(16)?;
      Some((hi * 16 + lo) as u8)
    })
    .collect()
}

/// A session rustls stored during this connection, waiting to be delivered
/// to JS as a `'session'` event.
pub(crate) struct IssuedSession {
  pub(crate) buffer: Vec<u8>,
  pub(crate) tls12: bool,
}

#[derive(Default)]
struct LinkState {
  /// The session `setSession()` asked this connection to offer.
  offered: Option<SessionId>,
  /// The ticket of the offered session, once rustls took it for the
  /// ClientHello.
  offered_ticket: Option<Vec<u8>>,
  /// The ticket of the newest session issued on this connection.
  last_ticket: Option<Vec<u8>>,
  /// The TLS 1.3 lineage this connection resumed from, if any.
  pool: Option<Tls13Pool>,
  issued: Vec<IssuedSession>,
}

/// Per-connection view of the registry: which session to offer, and which
/// new sessions arrived.
pub(crate) struct ClientSessionLink {
  registry: Arc<ClientSessionRegistry>,
  state: Mutex<LinkState>,
}

impl std::fmt::Debug for ClientSessionLink {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ClientSessionLink").finish_non_exhaustive()
  }
}

impl ClientSessionLink {
  pub(crate) fn new(registry: Arc<ClientSessionRegistry>) -> Self {
    Self {
      registry,
      state: Mutex::new(LinkState::default()),
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, LinkState> {
    self.state.lock().unwrap_or_else(|e| e.into_inner())
  }

  /// Offer the session named by `buf` on the next handshake. Returns whether
  /// the buffer names a session this isolate still holds.
  pub(crate) fn set_offered(&self, buf: Option<&[u8]>) -> bool {
    let id = buf.and_then(decode_session_buffer);
    let known = id
      .as_ref()
      .and_then(|id| self.registry.with(id, |_| ()))
      .is_some();
    self.lock().offered = if known { id } else { None };
    known
  }

  /// Sessions issued since the last call, oldest first.
  pub(crate) fn take_issued(&self) -> Vec<IssuedSession> {
    std::mem::take(&mut self.lock().issued)
  }

  /// The ticket `getTLSTicket()` reports: the newest issued one, or on a
  /// resumed TLS 1.2 connection (where OpenSSL keeps the offered session)
  /// the one that was offered.
  pub(crate) fn ticket(&self, resumed_tls12: bool) -> Option<Vec<u8>> {
    let state = self.lock();
    if resumed_tls12 {
      return state.offered_ticket.clone();
    }
    state
      .last_ticket
      .clone()
      .or_else(|| state.offered_ticket.clone())
  }

  fn record_tls13(
    &self,
    server_name: ServerName<'static>,
    value: rustls::client::Tls13ClientSessionValue,
    ticket: Vec<u8>,
  ) {
    let pool = self
      .lock()
      .pool
      .get_or_insert_with(Default::default)
      .clone();
    {
      let mut tickets = pool.lock().unwrap_or_else(|e| e.into_inner());
      if tickets.len() >= MAX_POOLED_TICKETS {
        tickets.pop_front();
      }
      tickets.push_back(value);
    }
    self.record_issued(server_name, StoredValue::Tls13(pool), ticket);
  }

  fn record_issued(
    &self,
    server_name: ServerName<'static>,
    value: StoredValue,
    ticket: Vec<u8>,
  ) {
    let tls12 = matches!(value, StoredValue::Tls12(_));
    let id = self.registry.insert(StoredSession {
      server_name,
      value,
      ticket: ticket.clone(),
    });
    let mut state = self.lock();
    if !ticket.is_empty() {
      state.last_ticket = Some(ticket);
    }
    state.issued.push(IssuedSession {
      buffer: encode_session_buffer(&id),
      tls12,
    });
  }

  fn offered_matching(
    &self,
    server_name: &ServerName<'_>,
  ) -> Option<SessionId> {
    let id = self.lock().offered?;
    self
      .registry
      .with(&id, |s| s.server_name == *server_name)
      .filter(|matches| *matches)
      .map(|_| id)
  }
}

/// The `ClientSessionStore` installed on every `node:tls` client config.
/// It saves each new session to the registry (reported to JS through the
/// link) and offers only the session `setSession()` selected, which is how
/// Node behaves: a client resumes only when given `options.session`.
#[derive(Debug)]
pub(crate) struct NodeClientSessionStore {
  /// Only used for key-exchange group hints.
  pub(crate) kx_hints: Arc<dyn rustls::client::ClientSessionStore>,
  pub(crate) link: Arc<ClientSessionLink>,
}

impl rustls::client::ClientSessionStore for NodeClientSessionStore {
  fn set_kx_hint(
    &self,
    server_name: ServerName<'static>,
    group: rustls::NamedGroup,
  ) {
    self.kx_hints.set_kx_hint(server_name, group);
  }

  fn kx_hint(
    &self,
    server_name: &ServerName<'_>,
  ) -> Option<rustls::NamedGroup> {
    self.kx_hints.kx_hint(server_name)
  }

  fn set_tls12_session(
    &self,
    server_name: ServerName<'static>,
    value: rustls::client::Tls12ClientSessionValue,
  ) {
    let ticket = ticket_from_debug(&format!("{value:?}"));
    self
      .link
      .record_issued(server_name, StoredValue::Tls12(value), ticket);
  }

  fn tls12_session(
    &self,
    server_name: &ServerName<'_>,
  ) -> Option<rustls::client::Tls12ClientSessionValue> {
    let id = self.link.offered_matching(server_name)?;
    let (value, ticket) =
      self.link.registry.with(&id, |s| match &s.value {
        StoredValue::Tls12(v) => Some((v.clone(), s.ticket.clone())),
        StoredValue::Tls13(_) => None,
      })??;
    self.link.lock().offered_ticket = Some(ticket);
    Some(value)
  }

  fn remove_tls12_session(&self, _server_name: &ServerName<'static>) {
    // A session the server refused stays in the registry: the application
    // still holds its buffer, and Node leaves dropping it to the
    // application (it simply does not resume).
  }

  fn insert_tls13_ticket(
    &self,
    server_name: ServerName<'static>,
    value: rustls::client::Tls13ClientSessionValue,
  ) {
    let ticket = ticket_from_debug(&format!("{value:?}"));
    self.link.record_tls13(server_name, value, ticket);
  }

  fn take_tls13_ticket(
    &self,
    server_name: &ServerName<'static>,
  ) -> Option<rustls::client::Tls13ClientSessionValue> {
    let id = self.link.offered_matching(server_name)?;
    let (pool, ticket) =
      self.link.registry.with(&id, |s| match &s.value {
        StoredValue::Tls13(pool) => Some((pool.clone(), s.ticket.clone())),
        StoredValue::Tls12(_) => None,
      })??;
    let value = pool.lock().unwrap_or_else(|e| e.into_inner()).pop_back()?;
    let mut state = self.link.lock();
    state.offered_ticket = Some(ticket);
    state.pool = Some(pool);
    Some(value)
  }
}

/// Session tickets under a Node `ticketKeys` value: 16 bytes of key name,
/// 16 bytes of HMAC secret, 16 bytes of AES key. The ticket is
/// `name || nonce || AES-128-GCM(key, nonce, aad = name)` where `key` is
/// derived from the last 32 bytes, so a ticket decrypts only under the same
/// 48 bytes. The format is not OpenSSL's: tickets are only shared between
/// servers in this runtime.
pub(crate) struct NodeTicketer {
  name: [u8; 16],
  key: aead::LessSafeKey,
  lifetime: u32,
}

impl std::fmt::Debug for NodeTicketer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NodeTicketer")
      .field("lifetime", &self.lifetime)
      .finish_non_exhaustive()
  }
}

impl NodeTicketer {
  pub(crate) fn new(keys: &[u8; 48], lifetime: u32) -> Option<Self> {
    use sha2::Digest;
    let mut name = [0u8; 16];
    name.copy_from_slice(&keys[..16]);
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"deno node:tls ticket key");
    hasher.update(&keys[16..48]);
    let digest = hasher.finalize();
    let unbound =
      aead::UnboundKey::new(&aead::AES_128_GCM, &digest[..16]).ok()?;
    Some(Self {
      name,
      key: aead::LessSafeKey::new(unbound),
      lifetime,
    })
  }
}

const TICKET_NONCE_LEN: usize = 12;

impl rustls::server::ProducesTickets for NodeTicketer {
  fn enabled(&self) -> bool {
    true
  }

  fn lifetime(&self) -> u32 {
    self.lifetime
  }

  fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
    let mut nonce = [0u8; TICKET_NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let mut sealed = plain.to_vec();
    self
      .key
      .seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(&self.name),
        &mut sealed,
      )
      .ok()?;
    let mut out = Vec::with_capacity(16 + TICKET_NONCE_LEN + sealed.len());
    out.extend_from_slice(&self.name);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Some(out)
  }

  fn decrypt(&self, cipher: &[u8]) -> Option<Vec<u8>> {
    if cipher.len() < 16 + TICKET_NONCE_LEN || cipher[..16] != self.name {
      return None;
    }
    let mut nonce = [0u8; TICKET_NONCE_LEN];
    nonce.copy_from_slice(&cipher[16..16 + TICKET_NONCE_LEN]);
    let mut sealed = cipher[16 + TICKET_NONCE_LEN..].to_vec();
    let plain = self
      .key
      .open_in_place(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(&self.name),
        &mut sealed,
      )
      .ok()?;
    Some(plain.to_vec())
  }
}

/// `NodeTicketer`s by (keys, lifetime), so every connection of a server (a
/// fresh `ServerConfig` each) and every server sharing `ticketKeys` uses one
/// ticketer.
#[derive(Default, Clone)]
pub(crate) struct TicketerCache {
  ticketers: HashMap<([u8; 48], u32), Arc<NodeTicketer>>,
}

impl std::fmt::Debug for TicketerCache {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TicketerCache")
      .field("len", &self.ticketers.len())
      .finish()
  }
}

impl TicketerCache {
  pub(crate) fn get(
    &mut self,
    keys: &[u8; 48],
    lifetime: u32,
  ) -> Option<Arc<NodeTicketer>> {
    if let Some(t) = self.ticketers.get(&(*keys, lifetime)) {
      return Some(t.clone());
    }
    // Ticketers are cheap to rebuild; bound the cache instead of tracking
    // which servers still use which keys.
    if self.ticketers.len() >= 256 {
      self.ticketers.clear();
    }
    let t = Arc::new(NodeTicketer::new(keys, lifetime)?);
    self.ticketers.insert((*keys, lifetime), t.clone());
    Some(t)
  }
}

#[cfg(test)]
mod tests {
  use rustls::server::ProducesTickets;

  use super::*;

  #[test]
  fn session_buffer_round_trip() {
    let id = [0xab; 16];
    let buf = encode_session_buffer(&id);
    assert!(buf.starts_with(SESSION_PREFIX));
    assert_eq!(decode_session_buffer(&buf), Some(id));
    assert_eq!(decode_session_buffer(b"deno-tls-session:zz"), None);
    assert_eq!(decode_session_buffer(b"\x30\x82\x01\x00"), None);
  }

  #[test]
  fn ticket_from_debug_reads_hex() {
    let dbg = "Tls12ClientSessionValue { suite: X, session_id: Y, \
               extended_ms: true, common: ClientSessionCommon { ticket: \
               00ff10, epoch: 1, lifetime_secs: 2 } }";
    assert_eq!(ticket_from_debug(dbg), vec![0x00, 0xff, 0x10]);
    assert!(ticket_from_debug("nothing here").is_empty());
  }

  #[test]
  fn ticketer_round_trip_and_key_separation() {
    let a = NodeTicketer::new(&[1u8; 48], 300).unwrap();
    let a2 = NodeTicketer::new(&[1u8; 48], 300).unwrap();
    let mut other = [1u8; 48];
    other[40] = 2;
    let b = NodeTicketer::new(&other, 300).unwrap();
    let ticket = a.encrypt(b"session state").unwrap();
    assert_eq!(a.decrypt(&ticket).as_deref(), Some(&b"session state"[..]));
    // Same 48 bytes (another server sharing ticketKeys) decrypts.
    assert_eq!(a2.decrypt(&ticket).as_deref(), Some(&b"session state"[..]));
    // Rotated keys with the same name do not.
    assert_eq!(b.decrypt(&ticket), None);
    // Tampering is detected.
    let mut bad = ticket.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(a.decrypt(&bad), None);
    assert_ne!(a.encrypt(b"x"), a.encrypt(b"x"));
  }

  #[test]
  fn link_offers_only_matching_known_sessions() {
    let registry = Arc::new(ClientSessionRegistry::default());
    let link = ClientSessionLink::new(registry.clone());
    assert!(!link.set_offered(Some(b"garbage")));
    let forged = encode_session_buffer(&[7u8; 16]);
    assert!(!link.set_offered(Some(&forged)));
    assert_eq!(registry.len(), 0);
  }
}
