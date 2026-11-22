// Copyright 2018-2026 the Deno authors. MIT license.

//! The kernel-sandbox broker protocol (denext): what a process confined
//! under `--kernel-sandbox=broker` says to the supervising broker, the
//! unconfined process that performs the operations the kernel can't hold
//! exactly for the confined one. Pure encoding and decoding, no I/O and no
//! `unsafe`: the broker parses bytes written by a process it does not trust
//! (native code in the confined process can write anything to the socket),
//! so this module is the fuzzed surface.
//!
//! Not the permission broker of `DENO_PERMISSION_BROKER_PATH`
//! ([`crate::broker`]): that one decides (yes / no, a prompt of its own) for
//! a process that then acts; this one holds a fixed policy, never asks and
//! never widens, and acts itself.
//!
//! # Transport
//!
//! A connected `AF_UNIX` `SOCK_STREAM` socket pair (a channel). The confined
//! side sends one request frame and reads one response frame; a channel
//! carries one request at a time, in order. Concurrency is more channels:
//! [`Request::Channel`] asks the broker for a new one, which arrives as a
//! descriptor (the broker caps how many it serves). A descriptor travels as
//! `SCM_RIGHTS` ancillary data with the first byte of the response frame
//! that announces it; a response carries at most one. The requester
//! acknowledges each descriptor with a [`Request::Received`] frame (which
//! has no response) before anything else on that channel, and the broker
//! keeps its own copy open until then: on macOS a descriptor closed by its
//! sender before the receiver took it out of the socket can arrive dead.
//!
//! # Frame
//!
//! ```text
//! offset  size  field
//!      0     4  magic    "DKB2"
//!      4     2  kind     request: the operation; response: 0x8000 | status
//!      6     2  flags    zero (reserved)
//!      8     4  id       chosen by the requester, echoed in the response
//!     12     4  len      payload bytes that follow, at most MAX_PAYLOAD
//! ```
//!
//! Integers are little-endian. A payload is the operation's fields in order
//! with nothing after them: `u32`, `u64`, `i64`, and byte strings (a `u32`
//! length, then the bytes). A **path** is a byte string of 1 to
//! [`MAX_PATH`] bytes that is absolute and already normal: it starts with
//! `/`, has no NUL, no empty, `.` or `..` component and no trailing `/`
//! (the root is `/`). The requester normalizes; the broker refuses anything
//! else as malformed rather than guess what was meant.
//!
//! # Malformed and unknown
//!
//! Anything that isn't a well-formed frame of a known shape is malformed:
//! the broker answers [`Response::Invalid`] where it still can and closes
//! the channel (the requester treats a closed channel as every later
//! operation refused). A well-formed frame whose operation this broker
//! doesn't know (a newer requester, an operation not built in) gets
//! [`Response::Unsupported`] and the channel stays open: its payload was
//! skipped by its length.
//!
//! # Operations
//!
//! | kind | request | fields | on success |
//! | --- | --- | --- | --- |
//! | 0x0001 | `Hello` | `version: u32`, `features: u64` | `Hello { version, features, max_payload }` |
//! | 0x0002 | `Channel` | | `Ok` + a descriptor (a new channel) |
//! | 0x0003 | `Received` | | (no response: the last response's descriptor arrived) |
//! | 0x0010 | `Open` | `path`, `flags: u32` ([`OpenFlags`]), `mode: u32` | `Ok` + a descriptor |
//! | 0x0011 | `Mkdir` | `path`, `mode: u32` | `Ok` |
//! | 0x0012 | `Remove` | `path`, `what: u32` (0 a file or link, 1 an empty directory, 2 a directory and everything in it) | `Ok` |
//! | 0x0013 | `Rename` | `from: path`, `to: path`, `flags: u32` (1: don't replace) | `Ok` |
//! | 0x0014 | `Symlink` | `target: bytes`, `link: path` | `Ok` |
//! | 0x0015 | `Link` | `existing: path`, `new: path` | `Ok` |
//! | 0x0016 | `Chmod` | `path`, `mode: u32`, `flags: u32` (1: don't follow) | `Ok` |
//! | 0x0017 | `Chown` | `path`, `uid: u32`, `gid: u32` (`u32::MAX`: keep), `flags: u32` | `Ok` |
//! | 0x0018 | `Utime` | `path`, `atime`, `mtime` (each `i64` s, `u32` ns), `flags: u32` | `Ok` |
//! | 0x0019 | `Truncate` | `path`, `len: u64` | `Ok` |
//! | 0x0100–0x01ff | reserved: network (connect, bind, resolve, Unix sockets) | | |
//!
//! Every operation names its objects by path and nothing else: no
//! descriptor ever travels toward the broker, and no request refers to an
//! earlier one, so the broker keeps no per-requester state to confuse. The
//! broker resolves each path itself, in its own view of the file system,
//! against its own copy of the policy.
//!
//! # Responses
//!
//! | status | response | payload |
//! | --- | --- | --- |
//! | 0 | `Ok` | `fds: u32` (0 or 1: the descriptors sent with this frame) |
//! | 1 | `Denied` | (the policy refuses it) |
//! | 2 | `Errno` | `errno: i32` (the OS refused it; 1..=4095) |
//! | 3 | `Unsupported` | |
//! | 4 | `Invalid` | (malformed; the channel closes) |
//! | 5 | `Hello` | `version: u32`, `features: u64`, `max_payload: u32` |
//! | 6 | `Data` | `bytes` (an operation's answer; none of the file operations has one) |
//!
//! # Versions
//!
//! `Hello` is optional and may be sent at any time; both sides then use the
//! lower `version` and the features both named. Version 1 is everything
//! above. The requester and the broker are the same executable, so `Hello`
//! mostly guards a channel handed to the wrong program.

use std::fmt;

/// The first four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"DKB2";
/// The protocol version this module speaks.
pub const VERSION: u32 = 1;
/// The size of a frame's header.
pub const HEADER_LEN: usize = 16;
/// The longest path a request names (Linux's `PATH_MAX`).
pub const MAX_PATH: usize = 4096;
/// The largest payload: two paths and a few integers fit, with room for
/// the network operations' host names.
pub const MAX_PAYLOAD: usize = 16 * 1024;
/// The largest errno a response carries (the kernel's `MAX_ERRNO`).
pub const MAX_ERRNO: i32 = 4095;
/// The network operations' kinds (reserved; none is defined here).
pub const NET_KINDS: std::ops::RangeInclusive<u16> = 0x0100..=0x01ff;

/// Set in a response's kind.
const RESPONSE_BIT: u16 = 0x8000;

/// What a broker does ([`Request::Hello`]).
pub mod features {
  /// The file operations (`Open` .. `Link`, `Truncate`).
  pub const FS: u64 = 1 << 0;
  /// `Chmod`, `Chown`, `Utime`.
  pub const FS_METADATA: u64 = 1 << 1;
  /// Reserved: TCP / UDP connect and bind.
  pub const NET: u64 = 1 << 8;
  /// Reserved: name resolution.
  pub const DNS: u64 = 1 << 9;
  /// Reserved: Unix sockets by path.
  pub const UNIX: u64 = 1 << 10;
}

/// How [`Request::Open`] opens. Not the OS's `O_*` bits: the broker builds
/// those itself from the flags it knows, so no flag it hasn't considered
/// (`O_PATH`, `O_TMPFILE`, `O_DIRECT`, ...) reaches `open(2)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenFlags(u32);

impl OpenFlags {
  pub const READ: Self = Self(1 << 0);
  pub const WRITE: Self = Self(1 << 1);
  pub const APPEND: Self = Self(1 << 2);
  pub const TRUNCATE: Self = Self(1 << 3);
  pub const CREATE: Self = Self(1 << 4);
  /// `O_CREAT | O_EXCL`.
  pub const CREATE_NEW: Self = Self(1 << 5);
  /// The object must be a directory.
  pub const DIRECTORY: Self = Self(1 << 6);
  /// A symlink as the last component is refused (`ELOOP`).
  pub const NOFOLLOW: Self = Self(1 << 7);
  const ALL: u32 = (1 << 8) - 1;

  pub const fn empty() -> Self {
    Self(0)
  }

  pub const fn bits(self) -> u32 {
    self.0
  }

  /// `None` for a bit this version doesn't define, and for combinations
  /// that mean nothing: no access at all, a directory opened for anything
  /// but reading.
  pub fn from_bits(bits: u32) -> Option<Self> {
    if bits & !Self::ALL != 0 {
      return None;
    }
    let flags = Self(bits);
    let changes = flags.changes();
    if !flags.contains(Self::READ) && !changes {
      return None;
    }
    if flags.contains(Self::DIRECTORY) && changes {
      return None;
    }
    Some(flags)
  }

  pub const fn contains(self, other: Self) -> bool {
    self.0 & other.0 == other.0
  }

  #[must_use]
  pub const fn with(self, other: Self) -> Self {
    Self(self.0 | other.0)
  }

  /// Whether the open can change the file system (so the write scope
  /// decides it).
  pub const fn changes(self) -> bool {
    self.0
      & (Self::WRITE.0
        | Self::APPEND.0
        | Self::TRUNCATE.0
        | Self::CREATE.0
        | Self::CREATE_NEW.0)
      != 0
  }
}

impl fmt::Debug for OpenFlags {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "OpenFlags({:#x})", self.0)
  }
}

/// Seconds and nanoseconds since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
  pub sec: i64,
  /// Below 1,000,000,000.
  pub nsec: u32,
}

/// [`Request::Rename`]'s flag: fail with `EEXIST` rather than replace.
pub const RENAME_NOREPLACE: u32 = 1;
/// The metadata operations' flag: a symlink as the last component is the
/// object (not what it points at).
pub const META_NOFOLLOW: u32 = 1;
/// [`Request::Chown`]: leave the owner or the group as it is.
pub const CHOWN_KEEP: u32 = u32::MAX;

/// An absolute, normal path as bytes (see the module docs).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct WirePath(Vec<u8>);

impl WirePath {
  /// `None` unless `bytes` is an absolute, normal path of at most
  /// [`MAX_PATH`] bytes.
  pub fn new(bytes: Vec<u8>) -> Option<Self> {
    is_wire_path(&bytes).then_some(Self(bytes))
  }

  pub fn as_bytes(&self) -> &[u8] {
    &self.0
  }

  pub fn into_bytes(self) -> Vec<u8> {
    self.0
  }
}

impl fmt::Debug for WirePath {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}", String::from_utf8_lossy(&self.0))
  }
}

fn is_wire_path(bytes: &[u8]) -> bool {
  if bytes.is_empty() || bytes.len() > MAX_PATH || bytes[0] != b'/' {
    return false;
  }
  if bytes.contains(&0) {
    return false;
  }
  if bytes == b"/" {
    return true;
  }
  bytes[1..]
    .split(|b| *b == b'/')
    .all(|c| !c.is_empty() && c != b"." && c != b"..")
}

/// What [`Request::Remove`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remove {
  /// A file, or a symlink itself.
  File,
  /// An empty directory.
  Dir,
  /// A directory and everything in it (no link in it is followed).
  Tree,
}

/// What the confined side asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
  Hello {
    version: u32,
    features: u64,
  },
  Channel,
  /// The descriptor of the response before this arrived. Sent after every
  /// response that carried one, and only then; never answered.
  Received,
  Open {
    path: WirePath,
    flags: OpenFlags,
    /// The new file's permission bits (at most `0o7777`).
    mode: u32,
  },
  Mkdir {
    path: WirePath,
    mode: u32,
  },
  Remove {
    path: WirePath,
    what: Remove,
  },
  Rename {
    from: WirePath,
    to: WirePath,
    noreplace: bool,
  },
  Symlink {
    /// The link's text: 1 to [`MAX_PATH`] bytes with no NUL, otherwise
    /// anything (it is resolved only when the link is followed).
    target: Vec<u8>,
    link: WirePath,
  },
  Link {
    existing: WirePath,
    new: WirePath,
  },
  Chmod {
    path: WirePath,
    mode: u32,
    nofollow: bool,
  },
  Chown {
    path: WirePath,
    /// [`CHOWN_KEEP`]: unchanged.
    uid: u32,
    gid: u32,
    nofollow: bool,
  },
  Utime {
    path: WirePath,
    atime: Time,
    mtime: Time,
    nofollow: bool,
  },
  Truncate {
    path: WirePath,
    len: u64,
  },
}

impl Request {
  /// The frame kind.
  pub fn kind(&self) -> u16 {
    match self {
      Request::Hello { .. } => 0x0001,
      Request::Channel => 0x0002,
      Request::Received => 0x0003,
      Request::Open { .. } => 0x0010,
      Request::Mkdir { .. } => 0x0011,
      Request::Remove { .. } => 0x0012,
      Request::Rename { .. } => 0x0013,
      Request::Symlink { .. } => 0x0014,
      Request::Link { .. } => 0x0015,
      Request::Chmod { .. } => 0x0016,
      Request::Chown { .. } => 0x0017,
      Request::Utime { .. } => 0x0018,
      Request::Truncate { .. } => 0x0019,
    }
  }

  /// Whether a successful response brings a descriptor.
  pub fn returns_fd(&self) -> bool {
    matches!(self, Request::Channel | Request::Open { .. })
  }
}

/// What the broker answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
  /// Done. `fd`: one descriptor came with the frame.
  Ok { fd: bool },
  /// The policy refuses it.
  Denied,
  /// The OS refused it.
  Errno(i32),
  /// The broker doesn't have this operation.
  Unsupported,
  /// The request was malformed; the channel is closed.
  Invalid,
  Hello {
    version: u32,
    features: u64,
    max_payload: u32,
  },
  /// An operation's answer in bytes (at most [`MAX_PAYLOAD`] - 4).
  Data(Vec<u8>),
}

impl Response {
  fn status(&self) -> u16 {
    match self {
      Response::Ok { .. } => 0,
      Response::Denied => 1,
      Response::Errno(_) => 2,
      Response::Unsupported => 3,
      Response::Invalid => 4,
      Response::Hello { .. } => 5,
      Response::Data(_) => 6,
    }
  }

  /// How many descriptors come with the frame.
  pub fn fds(&self) -> usize {
    usize::from(matches!(self, Response::Ok { fd: true }))
  }
}

/// A frame's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
  pub kind: u16,
  pub id: u32,
  pub len: u32,
}

impl Header {
  pub fn is_response(&self) -> bool {
    self.kind & RESPONSE_BIT != 0
  }
}

/// Why bytes are not a frame of this protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
  /// Not this protocol's magic.
  Magic,
  /// A reserved field isn't zero.
  Reserved,
  /// The payload is longer than [`MAX_PAYLOAD`].
  TooLong(u32),
  /// A request where a response belongs, or the reverse.
  Direction,
  /// The payload ended before a field did.
  Truncated,
  /// Bytes follow the last field.
  Trailing,
  /// A field's value is outside what the operation takes.
  Field(&'static str),
  /// A well-formed frame of a kind this version doesn't know: not
  /// malformed (see the module docs).
  UnknownKind(u16),
}

impl ProtocolError {
  /// Whether the frame was well-formed but of an unknown kind (answered
  /// `Unsupported`; everything else closes the channel).
  pub fn is_unknown_kind(&self) -> bool {
    matches!(self, ProtocolError::UnknownKind(_))
  }
}

impl fmt::Display for ProtocolError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ProtocolError::Magic => write!(f, "not a broker frame"),
      ProtocolError::Reserved => write!(f, "a reserved field is set"),
      ProtocolError::TooLong(len) => {
        write!(f, "a payload of {len} bytes, more than {MAX_PAYLOAD}")
      }
      ProtocolError::Direction => write!(f, "a frame in the wrong direction"),
      ProtocolError::Truncated => write!(f, "the payload ends early"),
      ProtocolError::Trailing => write!(f, "bytes after the last field"),
      ProtocolError::Field(name) => write!(f, "a bad value for {name}"),
      ProtocolError::UnknownKind(kind) => {
        write!(f, "an unknown kind {kind:#06x}")
      }
    }
  }
}

impl std::error::Error for ProtocolError {}

/// Parses a header; `len` is checked against [`MAX_PAYLOAD`], so the caller
/// can read exactly that many payload bytes.
pub fn decode_header(
  bytes: &[u8; HEADER_LEN],
) -> Result<Header, ProtocolError> {
  if bytes[0..4] != MAGIC {
    return Err(ProtocolError::Magic);
  }
  let kind = u16::from_le_bytes([bytes[4], bytes[5]]);
  let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
  let id = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
  let len = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
  if flags != 0 {
    return Err(ProtocolError::Reserved);
  }
  if len as usize > MAX_PAYLOAD {
    return Err(ProtocolError::TooLong(len));
  }
  Ok(Header { kind, id, len })
}

/// A frame of `kind` around `payload` (the operations outside this module:
/// the network kinds of [`super::net`]). `payload` must be at most
/// [`MAX_PAYLOAD`] bytes.
pub fn encode_frame(kind: u16, id: u32, payload: &[u8]) -> Vec<u8> {
  assert!(payload.len() <= MAX_PAYLOAD, "a payload within the cap");
  let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
  out.extend_from_slice(&MAGIC);
  out.extend_from_slice(&kind.to_le_bytes());
  out.extend_from_slice(&0u16.to_le_bytes());
  out.extend_from_slice(&id.to_le_bytes());
  out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
  out.extend_from_slice(payload);
  out
}

struct Writer(Vec<u8>);

impl Writer {
  fn u32(&mut self, v: u32) {
    self.0.extend_from_slice(&v.to_le_bytes());
  }
  fn u64(&mut self, v: u64) {
    self.0.extend_from_slice(&v.to_le_bytes());
  }
  fn i64(&mut self, v: i64) {
    self.0.extend_from_slice(&v.to_le_bytes());
  }
  fn bytes(&mut self, v: &[u8]) {
    self.u32(v.len() as u32);
    self.0.extend_from_slice(v);
  }
  fn path(&mut self, v: &WirePath) {
    self.bytes(v.as_bytes());
  }
  fn time(&mut self, v: Time) {
    self.i64(v.sec);
    self.u32(v.nsec);
  }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
  fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
    if self.0.len() < n {
      return Err(ProtocolError::Truncated);
    }
    let (head, rest) = self.0.split_at(n);
    self.0 = rest;
    Ok(head)
  }
  fn u32(&mut self) -> Result<u32, ProtocolError> {
    let b = self.take(4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
  }
  fn u64(&mut self) -> Result<u64, ProtocolError> {
    let b = self.take(8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    Ok(u64::from_le_bytes(a))
  }
  fn i64(&mut self) -> Result<i64, ProtocolError> {
    Ok(self.u64()? as i64)
  }
  fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
    let len = self.u32()? as usize;
    self.take(len)
  }
  fn path(&mut self, name: &'static str) -> Result<WirePath, ProtocolError> {
    WirePath::new(self.bytes()?.to_vec()).ok_or(ProtocolError::Field(name))
  }
  fn flag(
    &mut self,
    name: &'static str,
    bit: u32,
  ) -> Result<bool, ProtocolError> {
    match self.u32()? {
      0 => Ok(false),
      v if v == bit => Ok(true),
      _ => Err(ProtocolError::Field(name)),
    }
  }
  fn mode(&mut self) -> Result<u32, ProtocolError> {
    let mode = self.u32()?;
    if mode > 0o7777 {
      return Err(ProtocolError::Field("mode"));
    }
    Ok(mode)
  }
  fn time(&mut self, name: &'static str) -> Result<Time, ProtocolError> {
    let sec = self.i64()?;
    let nsec = self.u32()?;
    if nsec >= 1_000_000_000 {
      return Err(ProtocolError::Field(name));
    }
    Ok(Time { sec, nsec })
  }
  fn end(self) -> Result<(), ProtocolError> {
    if self.0.is_empty() {
      Ok(())
    } else {
      Err(ProtocolError::Trailing)
    }
  }
}

/// A request as one frame.
pub fn encode_request(id: u32, request: &Request) -> Vec<u8> {
  let mut w = Writer(Vec::new());
  match request {
    Request::Hello { version, features } => {
      w.u32(*version);
      w.u64(*features);
    }
    Request::Channel | Request::Received => {}
    Request::Open { path, flags, mode } => {
      w.path(path);
      w.u32(flags.bits());
      w.u32(*mode);
    }
    Request::Mkdir { path, mode } => {
      w.path(path);
      w.u32(*mode);
    }
    Request::Remove { path, what } => {
      w.path(path);
      w.u32(match what {
        Remove::File => 0,
        Remove::Dir => 1,
        Remove::Tree => 2,
      });
    }
    Request::Rename {
      from,
      to,
      noreplace,
    } => {
      w.path(from);
      w.path(to);
      w.u32(if *noreplace { RENAME_NOREPLACE } else { 0 });
    }
    Request::Symlink { target, link } => {
      w.bytes(target);
      w.path(link);
    }
    Request::Link { existing, new } => {
      w.path(existing);
      w.path(new);
    }
    Request::Chmod {
      path,
      mode,
      nofollow,
    } => {
      w.path(path);
      w.u32(*mode);
      w.u32(if *nofollow { META_NOFOLLOW } else { 0 });
    }
    Request::Chown {
      path,
      uid,
      gid,
      nofollow,
    } => {
      w.path(path);
      w.u32(*uid);
      w.u32(*gid);
      w.u32(if *nofollow { META_NOFOLLOW } else { 0 });
    }
    Request::Utime {
      path,
      atime,
      mtime,
      nofollow,
    } => {
      w.path(path);
      w.time(*atime);
      w.time(*mtime);
      w.u32(if *nofollow { META_NOFOLLOW } else { 0 });
    }
    Request::Truncate { path, len } => {
      w.path(path);
      w.u64(*len);
    }
  }
  encode_frame(request.kind(), id, &w.0)
}

/// Parses a request's payload (`header.len` bytes). An unknown kind is
/// [`ProtocolError::UnknownKind`]; anything else wrong is malformed.
pub fn decode_request(
  header: &Header,
  payload: &[u8],
) -> Result<Request, ProtocolError> {
  if header.is_response() {
    return Err(ProtocolError::Direction);
  }
  if payload.len() != header.len as usize {
    return Err(ProtocolError::Truncated);
  }
  let mut r = Reader(payload);
  let request = match header.kind {
    0x0001 => Request::Hello {
      version: r.u32()?,
      features: r.u64()?,
    },
    0x0002 => Request::Channel,
    0x0003 => Request::Received,
    0x0010 => Request::Open {
      path: r.path("path")?,
      flags: OpenFlags::from_bits(r.u32()?)
        .ok_or(ProtocolError::Field("flags"))?,
      mode: r.mode()?,
    },
    0x0011 => Request::Mkdir {
      path: r.path("path")?,
      mode: r.mode()?,
    },
    0x0012 => Request::Remove {
      path: r.path("path")?,
      what: match r.u32()? {
        0 => Remove::File,
        1 => Remove::Dir,
        2 => Remove::Tree,
        _ => return Err(ProtocolError::Field("what")),
      },
    },
    0x0013 => Request::Rename {
      from: r.path("from")?,
      to: r.path("to")?,
      noreplace: r.flag("flags", RENAME_NOREPLACE)?,
    },
    0x0014 => {
      let target = r.bytes()?;
      if target.is_empty() || target.len() > MAX_PATH || target.contains(&0) {
        return Err(ProtocolError::Field("target"));
      }
      Request::Symlink {
        target: target.to_vec(),
        link: r.path("link")?,
      }
    }
    0x0015 => Request::Link {
      existing: r.path("existing")?,
      new: r.path("new")?,
    },
    0x0016 => Request::Chmod {
      path: r.path("path")?,
      mode: r.mode()?,
      nofollow: r.flag("flags", META_NOFOLLOW)?,
    },
    0x0017 => Request::Chown {
      path: r.path("path")?,
      uid: r.u32()?,
      gid: r.u32()?,
      nofollow: r.flag("flags", META_NOFOLLOW)?,
    },
    0x0018 => Request::Utime {
      path: r.path("path")?,
      atime: r.time("atime")?,
      mtime: r.time("mtime")?,
      nofollow: r.flag("flags", META_NOFOLLOW)?,
    },
    0x0019 => Request::Truncate {
      path: r.path("path")?,
      len: {
        let len = r.u64()?;
        // a length a file offset can hold
        if len > i64::MAX as u64 {
          return Err(ProtocolError::Field("len"));
        }
        len
      },
    },
    kind => return Err(ProtocolError::UnknownKind(kind)),
  };
  r.end()?;
  Ok(request)
}

/// A response as one frame.
pub fn encode_response(id: u32, response: &Response) -> Vec<u8> {
  let mut w = Writer(Vec::new());
  match response {
    Response::Ok { fd } => w.u32(u32::from(*fd)),
    Response::Denied | Response::Unsupported | Response::Invalid => {}
    Response::Errno(errno) => w.u32(*errno as u32),
    Response::Hello {
      version,
      features,
      max_payload,
    } => {
      w.u32(*version);
      w.u64(*features);
      w.u32(*max_payload);
    }
    Response::Data(bytes) => w.bytes(bytes),
  }
  encode_frame(RESPONSE_BIT | response.status(), id, &w.0)
}

/// Parses a response's payload (`header.len` bytes).
pub fn decode_response(
  header: &Header,
  payload: &[u8],
) -> Result<Response, ProtocolError> {
  if !header.is_response() {
    return Err(ProtocolError::Direction);
  }
  if payload.len() != header.len as usize {
    return Err(ProtocolError::Truncated);
  }
  let mut r = Reader(payload);
  let response = match header.kind & !RESPONSE_BIT {
    0 => Response::Ok {
      fd: r.flag("fds", 1)?,
    },
    1 => Response::Denied,
    2 => {
      let errno = r.u32()? as i32;
      if !(1..=MAX_ERRNO).contains(&errno) {
        return Err(ProtocolError::Field("errno"));
      }
      Response::Errno(errno)
    }
    3 => Response::Unsupported,
    4 => Response::Invalid,
    5 => Response::Hello {
      version: r.u32()?,
      features: r.u64()?,
      max_payload: r.u32()?,
    },
    6 => Response::Data(r.bytes()?.to_vec()),
    kind => return Err(ProtocolError::UnknownKind(kind)),
  };
  r.end()?;
  Ok(response)
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  fn path(s: &str) -> WirePath {
    WirePath::new(s.as_bytes().to_vec()).unwrap()
  }

  fn split(frame: &[u8]) -> (Header, &[u8]) {
    let header =
      decode_header(frame[..HEADER_LEN].try_into().unwrap()).unwrap();
    (header, &frame[HEADER_LEN..])
  }

  pub(crate) fn sample_requests() -> Vec<Request> {
    let t = |sec, nsec| Time { sec, nsec };
    vec![
      Request::Hello {
        version: VERSION,
        features: features::FS | features::FS_METADATA,
      },
      Request::Channel,
      Request::Received,
      Request::Open {
        path: path("/a/b"),
        flags: OpenFlags::READ,
        mode: 0,
      },
      Request::Open {
        path: path("/"),
        flags: OpenFlags::READ.with(OpenFlags::DIRECTORY),
        mode: 0,
      },
      Request::Open {
        path: path("/a/new"),
        flags: OpenFlags::WRITE
          .with(OpenFlags::CREATE_NEW)
          .with(OpenFlags::NOFOLLOW),
        mode: 0o644,
      },
      Request::Mkdir {
        path: path("/a/dir"),
        mode: 0o755,
      },
      Request::Remove {
        path: path("/a/b"),
        what: Remove::File,
      },
      Request::Remove {
        path: path("/a/dir"),
        what: Remove::Dir,
      },
      Request::Remove {
        path: path("/a/dir"),
        what: Remove::Tree,
      },
      Request::Rename {
        from: path("/a/b"),
        to: path("/a/c"),
        noreplace: true,
      },
      Request::Symlink {
        target: b"../x".to_vec(),
        link: path("/a/l"),
      },
      Request::Link {
        existing: path("/a/b"),
        new: path("/a/h"),
      },
      Request::Chmod {
        path: path("/a/b"),
        mode: 0o600,
        nofollow: false,
      },
      Request::Chown {
        path: path("/a/b"),
        uid: 1000,
        gid: CHOWN_KEEP,
        nofollow: true,
      },
      Request::Utime {
        path: path("/a/b"),
        atime: t(1, 2),
        mtime: t(-5, 999_999_999),
        nofollow: false,
      },
      Request::Truncate {
        path: path("/a/b"),
        len: 12,
      },
    ]
  }

  fn sample_responses() -> Vec<Response> {
    vec![
      Response::Ok { fd: false },
      Response::Ok { fd: true },
      Response::Denied,
      Response::Errno(2),
      Response::Errno(MAX_ERRNO),
      Response::Unsupported,
      Response::Invalid,
      Response::Hello {
        version: VERSION,
        features: features::FS,
        max_payload: MAX_PAYLOAD as u32,
      },
      Response::Data(vec![1, 2, 3]),
      Response::Data(Vec::new()),
    ]
  }

  #[test]
  fn requests_round_trip() {
    for (i, request) in sample_requests().into_iter().enumerate() {
      let frame = encode_request(i as u32 + 7, &request);
      let (header, payload) = split(&frame);
      assert_eq!(header.id, i as u32 + 7);
      assert_eq!(header.kind, request.kind());
      assert!(!header.is_response());
      assert_eq!(decode_request(&header, payload).unwrap(), request);
      // a request is never read as a response
      assert_eq!(
        decode_response(&header, payload),
        Err(ProtocolError::Direction)
      );
    }
  }

  #[test]
  fn responses_round_trip() {
    for response in sample_responses() {
      let frame = encode_response(9, &response);
      let (header, payload) = split(&frame);
      assert!(header.is_response());
      assert_eq!(decode_response(&header, payload).unwrap(), response);
      assert_eq!(
        decode_request(&header, payload),
        Err(ProtocolError::Direction)
      );
    }
  }

  #[test]
  fn paths_must_be_absolute_and_normal() {
    for good in ["/", "/a", "/a/b", "/a/...", "/a/.b", "/a b/\u{e9}"] {
      assert!(WirePath::new(good.as_bytes().to_vec()).is_some(), "{good}");
    }
    for bad in [
      "", "a", "a/b", "./a", "//", "/a/", "/a//b", "/.", "/..", "/a/./b",
      "/a/../b", "/a/..", "/a\0b",
    ] {
      assert!(WirePath::new(bad.as_bytes().to_vec()).is_none(), "{bad:?}");
    }
    let mut long = vec![b'/'];
    long.extend(std::iter::repeat_n(b'a', MAX_PATH - 1));
    assert!(WirePath::new(long.clone()).is_some());
    long.push(b'a');
    assert!(WirePath::new(long).is_none());
  }

  #[test]
  fn open_flags_refuse_unknown_bits_and_empty_access() {
    assert!(OpenFlags::from_bits(0).is_none());
    assert!(OpenFlags::from_bits(1 << 8).is_none());
    assert!(OpenFlags::from_bits(u32::MAX).is_none());
    // nofollow or directory alone: no access
    assert!(OpenFlags::from_bits(OpenFlags::NOFOLLOW.bits()).is_none());
    assert!(OpenFlags::from_bits(OpenFlags::DIRECTORY.bits()).is_none());
    // a directory is only read
    assert!(
      OpenFlags::from_bits(OpenFlags::DIRECTORY.with(OpenFlags::WRITE).bits())
        .is_none()
    );
    assert!(
      OpenFlags::from_bits(
        OpenFlags::DIRECTORY
          .with(OpenFlags::READ)
          .with(OpenFlags::CREATE)
          .bits()
      )
      .is_none()
    );
    let rw = OpenFlags::from_bits(3).unwrap();
    assert!(rw.contains(OpenFlags::READ) && rw.changes());
    assert!(!OpenFlags::READ.changes());
  }

  #[test]
  fn headers_refuse_other_protocols_and_oversize() {
    let frame = encode_request(1, &Request::Channel);
    let mut header: [u8; HEADER_LEN] = frame[..HEADER_LEN].try_into().unwrap();
    assert!(decode_header(&header).is_ok());
    let mut bad = header;
    bad[0] = b'X';
    assert_eq!(decode_header(&bad), Err(ProtocolError::Magic));
    let mut bad = header;
    bad[6] = 1;
    assert_eq!(decode_header(&bad), Err(ProtocolError::Reserved));
    header[12..16].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_le_bytes());
    assert_eq!(
      decode_header(&header),
      Err(ProtocolError::TooLong(MAX_PAYLOAD as u32 + 1))
    );
    header[12..16].copy_from_slice(&(MAX_PAYLOAD as u32).to_le_bytes());
    assert!(decode_header(&header).is_ok());
  }

  #[test]
  fn field_values_outside_the_operation_are_malformed() {
    let reframe = |request: &Request, edit: &dyn Fn(&mut Vec<u8>)| {
      let frame = encode_request(1, request);
      let (header, payload) = split(&frame);
      let mut payload = payload.to_vec();
      edit(&mut payload);
      let header = Header {
        len: payload.len() as u32,
        ..header
      };
      decode_request(&header, &payload)
    };
    let open = Request::Open {
      path: path("/a"),
      flags: OpenFlags::READ,
      mode: 0,
    };
    // a trailing byte
    assert_eq!(reframe(&open, &|p| p.push(0)), Err(ProtocolError::Trailing));
    // a missing byte
    assert_eq!(
      reframe(&open, &|p| {
        p.pop();
      }),
      Err(ProtocolError::Truncated)
    );
    // mode above 0o7777 (the last four bytes)
    assert_eq!(
      reframe(&open, &|p| {
        let n = p.len();
        p[n - 4..].copy_from_slice(&0o10000u32.to_le_bytes());
      }),
      Err(ProtocolError::Field("mode"))
    );
    // a path length that runs past the payload
    assert_eq!(
      reframe(&open, &|p| p[0..4].copy_from_slice(&u32::MAX.to_le_bytes())),
      Err(ProtocolError::Truncated)
    );
    // a relative path
    assert_eq!(
      reframe(&open, &|p| p[4] = b'a'),
      Err(ProtocolError::Field("path"))
    );
    // a boolean that is neither 0 nor its bit
    let remove = Request::Remove {
      path: path("/a"),
      what: Remove::Dir,
    };
    assert_eq!(
      reframe(&remove, &|p| {
        let n = p.len();
        p[n - 4] = 3;
      }),
      Err(ProtocolError::Field("what"))
    );
    // a boolean that is neither 0 nor its bit
    let rename = Request::Rename {
      from: path("/a"),
      to: path("/b"),
      noreplace: true,
    };
    assert_eq!(
      reframe(&rename, &|p| {
        let n = p.len();
        p[n - 4] = 2;
      }),
      Err(ProtocolError::Field("flags"))
    );
    // nanoseconds of a second or more
    let utime = Request::Utime {
      path: path("/a"),
      atime: Time { sec: 0, nsec: 0 },
      mtime: Time { sec: 0, nsec: 0 },
      nofollow: false,
    };
    assert_eq!(
      reframe(&utime, &|p| {
        let n = p.len();
        p[n - 8..n - 4].copy_from_slice(&1_000_000_000u32.to_le_bytes());
      }),
      Err(ProtocolError::Field("mtime"))
    );
    // an empty symlink target
    let frame = {
      let mut w = Writer(Vec::new());
      w.bytes(b"");
      w.path(&path("/a/l"));
      encode_frame(0x0014, 1, &w.0)
    };
    let (header, payload) = split(&frame);
    assert_eq!(
      decode_request(&header, payload),
      Err(ProtocolError::Field("target"))
    );
    // a truncate length past a file offset
    let frame = {
      let mut w = Writer(Vec::new());
      w.path(&path("/a"));
      w.u64(u64::MAX);
      encode_frame(0x0019, 1, &w.0)
    };
    let (header, payload) = split(&frame);
    assert_eq!(
      decode_request(&header, payload),
      Err(ProtocolError::Field("len"))
    );
  }

  #[test]
  fn unknown_kinds_are_not_malformed() {
    for kind in [0u16, 0x0004, 0x001a, 0x0100, 0x01ff, 0x7fff] {
      let frame = encode_frame(kind, 1, &[1, 2, 3]);
      let (header, payload) = split(&frame);
      let err = decode_request(&header, payload).unwrap_err();
      assert_eq!(err, ProtocolError::UnknownKind(kind));
      assert!(err.is_unknown_kind());
    }
    assert!(NET_KINDS.contains(&0x0100));
  }

  #[test]
  fn response_errno_stays_in_range() {
    for errno in [0i32, -1, MAX_ERRNO + 1] {
      let frame =
        encode_frame(RESPONSE_BIT | 2, 1, &(errno as u32).to_le_bytes());
      let (header, payload) = split(&frame);
      assert_eq!(
        decode_response(&header, payload),
        Err(ProtocolError::Field("errno"))
      );
    }
    // two descriptors are never announced
    let frame = encode_frame(RESPONSE_BIT, 1, &2u32.to_le_bytes());
    let (header, payload) = split(&frame);
    assert_eq!(
      decode_response(&header, payload),
      Err(ProtocolError::Field("fds"))
    );
  }

  /// A small deterministic generator (xorshift64*), so a failure names its
  /// seed.
  pub(crate) struct Rng(pub u64);

  impl Rng {
    pub fn next(&mut self) -> u64 {
      self.0 ^= self.0 >> 12;
      self.0 ^= self.0 << 25;
      self.0 ^= self.0 >> 27;
      self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
      (self.next() % n as u64) as usize
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
      (0..n).map(|_| self.next() as u8).collect()
    }
  }

  #[allow(
    clippy::disallowed_methods,
    reason = "a test knob read from the test process's environment"
  )]
  pub(crate) fn fuzz_iterations(default: usize) -> usize {
    std::env::var("DENO_BROKER_FUZZ_ITERS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(default)
  }

  /// One mutation of a valid frame: flipped bits, a spliced integer, a cut,
  /// an insertion, a rewritten length.
  pub(crate) fn mutate(rng: &mut Rng, frame: &mut Vec<u8>) {
    const INTERESTING: [u32; 10] = [
      0,
      1,
      2,
      0x7fff_ffff,
      0x8000_0000,
      u32::MAX,
      MAX_PATH as u32,
      MAX_PATH as u32 + 1,
      MAX_PAYLOAD as u32,
      MAX_PAYLOAD as u32 + 1,
    ];
    match rng.below(6) {
      0 if !frame.is_empty() => {
        let i = rng.below(frame.len());
        frame[i] ^= 1 << rng.below(8);
      }
      1 if frame.len() >= 4 => {
        let i = rng.below(frame.len() - 3);
        let v = INTERESTING[rng.below(INTERESTING.len())];
        frame[i..i + 4].copy_from_slice(&v.to_le_bytes());
      }
      2 if !frame.is_empty() => {
        let n = rng.below(frame.len());
        frame.truncate(n);
      }
      3 => {
        let i = rng.below(frame.len() + 1);
        let n = rng.below(9);
        let extra = rng.bytes(n);
        frame.splice(i..i, extra);
      }
      4 if frame.len() >= HEADER_LEN => {
        // keep the frame's length field honest after other edits
        let len = (frame.len() - HEADER_LEN) as u32;
        frame[12..16].copy_from_slice(&len.to_le_bytes());
      }
      _ => {
        let i = rng.below(frame.len() + 1);
        let c = [b'/', b'.', 0, b'a'][rng.below(4)];
        frame.insert(i, c);
      }
    }
  }

  /// Decodes whatever `bytes` are as a request frame, the way the server
  /// reads one; never panics.
  fn decode_any(bytes: &[u8]) -> Result<Request, ProtocolError> {
    let Some(head) = bytes.get(..HEADER_LEN) else {
      return Err(ProtocolError::Truncated);
    };
    let header = decode_header(head.try_into().unwrap())?;
    let payload = &bytes[HEADER_LEN..];
    if payload.len() < header.len as usize {
      return Err(ProtocolError::Truncated);
    }
    decode_request(&header, &payload[..header.len as usize])
  }

  /// The parser fuzz: random bytes and mutated valid frames never panic,
  /// and whatever decodes re-encodes to a frame that decodes to the same
  /// request (so nothing the parser accepts is ambiguous).
  #[test]
  fn fuzz_request_parser() {
    let iterations = fuzz_iterations(200_000);
    let samples: Vec<Vec<u8>> = sample_requests()
      .iter()
      .map(|r| encode_request(3, r))
      .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut accepted = 0usize;
    for i in 0..iterations {
      let mut frame = if i % 4 == 0 {
        let n = rng.below(64);
        let mut f = rng.bytes(n);
        if i % 8 == 0 && f.len() >= 4 {
          f[..4].copy_from_slice(&MAGIC);
        }
        f
      } else {
        samples[rng.below(samples.len())].clone()
      };
      for _ in 0..1 + rng.below(4) {
        mutate(&mut rng, &mut frame);
      }
      if let Ok(request) = decode_any(&frame) {
        accepted += 1;
        let again = encode_request(1, &request);
        assert_eq!(decode_any(&again).as_ref(), Ok(&request), "seed step {i}");
        // every accepted path is absolute and normal
        let paths: Vec<&WirePath> = match &request {
          Request::Open { path, .. }
          | Request::Mkdir { path, .. }
          | Request::Remove { path, .. }
          | Request::Chmod { path, .. }
          | Request::Chown { path, .. }
          | Request::Utime { path, .. }
          | Request::Truncate { path, .. } => vec![path],
          Request::Rename { from, to, .. } => vec![from, to],
          Request::Link { existing, new } => vec![existing, new],
          Request::Symlink { link, .. } => vec![link],
          Request::Hello { .. } | Request::Channel | Request::Received => {
            vec![]
          }
        };
        for p in paths {
          assert!(is_wire_path(p.as_bytes()));
        }
      }
    }
    // the mutations keep a share of the frames valid: the fuzz reaches the
    // field checks, not only the header's
    assert!(accepted > iterations / 100, "{accepted} of {iterations}");
  }

  #[test]
  fn fuzz_response_parser() {
    let iterations = fuzz_iterations(100_000);
    let samples: Vec<Vec<u8>> = sample_responses()
      .iter()
      .map(|r| encode_response(3, r))
      .collect();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..iterations {
      let mut frame = samples[rng.below(samples.len())].clone();
      for _ in 0..1 + rng.below(4) {
        mutate(&mut rng, &mut frame);
      }
      let Some(head) = frame.get(..HEADER_LEN) else {
        continue;
      };
      let Ok(header) = decode_header(head.try_into().unwrap()) else {
        continue;
      };
      let payload = &frame[HEADER_LEN..];
      if payload.len() < header.len as usize {
        continue;
      }
      if let Ok(response) =
        decode_response(&header, &payload[..header.len as usize])
      {
        assert!(response.fds() <= 1);
        let again = encode_response(1, &response);
        let (h, p) = split(&again);
        assert_eq!(decode_response(&h, p), Ok(response));
      }
    }
  }
}
