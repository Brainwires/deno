// Copyright 2018-2026 the Deno authors. MIT license.

//! The network half of the supervising broker (Mode B of
//! `--kernel-sandbox`): the operations a confined process asks for through
//! `deno_net_route`, carried out here, outside the sandbox, by the process
//! that holds the grant.
//!
//! A [`NetAuthority`] is an authority, not a relay. The confined side can
//! send anything (native code can write to the broker's socket directly),
//! so nothing a request says is believed: every operation is decided by the
//! [`NetDecider`] (Deno's own permission checks, built by the CLI from the
//! grant the run started with), names are resolved here, and a socket is
//! connected to exactly the address that was checked.
//!
//! What "the grant reaches an address" means:
//!
//! - an address the grant names itself (an IP literal, a subnet, an
//!   unscoped grant) is reached as written;
//! - a NAME is allowed, not where it leads. A granted name is resolved
//!   here, and an answer on this machine or its networks (loopback, private,
//!   link-local with `169.254.169.254`, the other internal ranges of
//!   [`crate::proxy`]) is refused unless the grant also names that address,
//!   so a granted public name can't be rebound to `127.0.0.1` or a cloud
//!   metadata endpoint. `localhost` (and `*.localhost`) is the one name that
//!   may lead to loopback, and only there;
//! - a connect request carries an address, because most callers resolve and
//!   connect in two steps (`node:net`, the Postgres client, Happy Eyeballs).
//!   The address must be one a granted name resolved to HERE: every answer
//!   [`NetAuthority::resolve`] gives is remembered for [`ANSWER_TTL`], and a
//!   request that says which name it resolved (`host`) is resolved again on
//!   a miss. An address the confined side claims for a name is never taken
//!   on its word;
//! - deny rules hold for the name and for the address connected to.
//!
//! UDP: a socket handed to the confined process can't be held to a peer on
//! Linux (`sendto` with a destination works on a connected UDP socket, and
//! `connect` can be called again), so under a host-scoped grant no UDP
//! socket is handed out at all ([`NetAuthority::open_udp`] needs an
//! unscoped connect grant). A DNS query to a name server the grant names
//! is an operation instead ([`NetAuthority::udp_exchange`]): the socket
//! never leaves this process.

use std::collections::HashMap;
use std::io;
use std::io::Read;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use deno_net_route::NetRoute;
use deno_net_route::TcpBind;
use deno_net_route::TcpConnect;
use deno_net_route::UdpOpen;
use deno_net_route::refused;
use socket2::Domain;
use socket2::Protocol;
use socket2::Socket;
use socket2::Type;

use crate::proxy::ProxyHost;
use crate::proxy::is_internal;

/// How long an answer of [`NetAuthority::resolve`] lets its addresses be
/// connected to without the name.
pub const ANSWER_TTL: Duration = Duration::from_secs(300);

/// The most names whose answers are remembered (the oldest go first).
const MAX_ANSWERS: usize = 4096;

/// The longest a connect waits (a refused or unreachable address answers
/// at once; this bounds a silent one).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(130);

/// The longest [`NetAuthority::udp_exchange`] waits for its answer.
pub const MAX_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest datagram [`NetAuthority::udp_exchange`] sends or returns.
pub const MAX_DATAGRAM: usize = 65_535;

/// Deno's rule, as the broker asks it. The CLI implements it over the
/// permissions the run was started with (no prompting: every question has
/// an answer), so the broker and the confined process's own checks can't
/// disagree about what the grant says.
pub trait NetDecider: Send + Sync + 'static {
  /// Deno's connect check for `host` as written (a name, or an IP
  /// literal's text) and `port`: allowed by the grant and not denied.
  fn connect(&self, host: &str, port: u16) -> bool;
  /// Deno's check of an address a connection is about to be made to (the
  /// deny rules, by address).
  fn connect_resolved(&self, ip: IpAddr, port: u16) -> bool;
  /// Deno's listen check for `host` as written and `port`.
  fn listen(&self, host: &str, port: u16) -> bool;
  /// Deno's check of an address about to be bound.
  fn listen_resolved(&self, ip: IpAddr, port: u16) -> bool;
  /// Whether the grant names `host` (a name, or an IP literal's text) for
  /// some port, in either direction, and no deny rule covers it whole.
  fn names(&self, host: &str) -> bool;
  /// Whether a socket that can send anywhere holds the grant: connecting
  /// is unscoped and nothing is denied.
  fn open_datagrams(&self) -> bool;
  /// Deno's check for connecting to the Unix socket at `path` (read and
  /// write on it, and the connect direction of the network grant).
  fn unix(&self, path: &Path) -> bool;
  /// Deno's check for binding the Unix socket at `path` (read and write on
  /// it, and the listen direction of the network grant:
  /// `--allow-net-listen=unix:<path>`). A decider that doesn't tell the
  /// directions apart answers as for a connect.
  fn unix_listen(&self, path: &Path) -> bool {
    self.unix(path)
  }
  /// Deno's file check alone (read and write) for the place a socket's
  /// name really is: the folder that holds it as the kernel names it, with
  /// the name, or the place a link leads to. The network grant named the
  /// socket by the path as it was given; the file grant holds where the
  /// socket is. A decider that doesn't tell the two apart answers as for a
  /// connect.
  fn unix_place(&self, path: &Path) -> bool {
    self.unix(path)
  }
}

/// Resolves a name to its addresses (the system's resolver, or a test's).
pub type Resolve =
  Arc<dyn Fn(&str) -> io::Result<Vec<IpAddr>> + Send + Sync + 'static>;

fn system_resolve() -> Resolve {
  Arc::new(|host| {
    let mut addrs: Vec<IpAddr> =
      (host, 0).to_socket_addrs()?.map(|addr| addr.ip()).collect();
    addrs.dedup();
    Ok(addrs)
  })
}

struct Answer {
  addrs: Vec<IpAddr>,
  at: Instant,
}

/// The broker's network operations (see the module docs).
pub struct NetAuthority {
  decider: Arc<dyn NetDecider>,
  resolve: Resolve,
  /// name → what it resolved to here, and when
  answers: Mutex<HashMap<String, Answer>>,
  ttl: Duration,
}

/// `localhost` and its subdomains (RFC 6761): the name that names loopback.
fn is_localhost(name: &str) -> bool {
  name == "localhost" || name.ends_with(".localhost")
}

/// An IP address as Deno's checks take it (no brackets).
fn ip_text(ip: IpAddr) -> String {
  ip.to_string()
}

impl NetAuthority {
  pub fn new(decider: Arc<dyn NetDecider>) -> Self {
    Self::with_resolver(decider, system_resolve())
  }

  /// With a resolver of the caller's (tests: no DNS).
  pub fn with_resolver(decider: Arc<dyn NetDecider>, resolve: Resolve) -> Self {
    Self {
      decider,
      resolve,
      answers: Mutex::new(HashMap::new()),
      ttl: ANSWER_TTL,
    }
  }

  /// How long an answer is remembered (tests).
  pub fn with_answer_ttl(mut self, ttl: Duration) -> Self {
    self.ttl = ttl;
    self
  }

  /// The addresses of `host`, resolved here: an IP literal as itself, a
  /// name only when the grant names it, none of them internal unless the
  /// grant names that address (or the name is `localhost`, which may lead
  /// to loopback only). One internal answer refuses the name: a rebinding
  /// answer mixes them.
  pub fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
    let name = match parse_host(host)? {
      ProxyHost::Ip(ip) => return Ok(vec![ip]),
      ProxyHost::Domain(name) => name,
    };
    self.resolve_name(&name)
  }

  fn resolve_name(&self, name: &str) -> io::Result<Vec<IpAddr>> {
    if !self.decider.names(name) {
      return Err(refused(format_args!("{name} is not in the network grant")));
    }
    let mut addrs = (self.resolve)(name)?;
    addrs.dedup();
    if addrs.is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{name} has no addresses"),
      ));
    }
    for ip in &addrs {
      if is_localhost(name) {
        if !ip.is_loopback() {
          return Err(refused(format_args!(
            "{name} resolves to {ip}, which is not a loopback address"
          )));
        }
      } else if is_internal(*ip) && !self.decider.names(&ip_text(*ip)) {
        return Err(refused(format_args!(
          "{name} resolves to the internal address {ip}, which the network grant does not name"
        )));
      }
    }
    let mut answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
    if answers.len() >= MAX_ANSWERS && !answers.contains_key(name) {
      let ttl = self.ttl;
      answers.retain(|_, answer| answer.at.elapsed() < ttl);
      if answers.len() >= MAX_ANSWERS
        && let Some(oldest) = answers
          .iter()
          .min_by_key(|(_, answer)| answer.at)
          .map(|(name, _)| name.clone())
      {
        answers.remove(&oldest);
      }
    }
    answers.insert(
      name.to_string(),
      Answer {
        addrs: addrs.clone(),
        at: Instant::now(),
      },
    );
    Ok(addrs)
  }

  /// Whether `name` resolved to `ip` here within the TTL.
  fn answered(&self, name: &str, ip: IpAddr) -> bool {
    let answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
    answers
      .get(name)
      .is_some_and(|a| a.at.elapsed() < self.ttl && a.addrs.contains(&ip))
  }

  /// The names that resolved to `ip` here within the TTL.
  fn names_answering(&self, ip: IpAddr) -> Vec<String> {
    let answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
    let mut names: Vec<String> = answers
      .iter()
      .filter(|(_, a)| a.at.elapsed() < self.ttl && a.addrs.contains(&ip))
      .map(|(name, _)| name.clone())
      .collect();
    names.sort();
    names
  }

  /// Whether the grant reaches `addr` in the direction `allowed` asks
  /// (connect or listen), given the name the caller says it resolved.
  fn reaches(
    &self,
    addr: SocketAddr,
    host: Option<&str>,
    allowed: &dyn Fn(&str, u16) -> bool,
  ) -> io::Result<()> {
    let (ip, port) = (addr.ip(), addr.port());
    let not_granted =
      || refused(format_args!("{addr} is not in the network grant"));
    // the address a name leads to must be nameable through it for this port
    let through = |name: &str| -> io::Result<()> {
      if !allowed(name, port) {
        return Err(refused(format_args!(
          "{name}:{port} is not in the network grant"
        )));
      }
      if is_localhost(name) {
        if !ip.is_loopback() {
          return Err(not_granted());
        }
      } else if is_internal(ip) && !allowed(&ip_text(ip), port) {
        return Err(refused(format_args!(
          "{name} resolves to the internal address {ip}, which the network grant does not name"
        )));
      }
      Ok(())
    };
    match host.map(parse_host).transpose()? {
      Some(ProxyHost::Ip(named)) if named != ip => Err(refused(format_args!(
        "{named} and {ip} are not the same address"
      ))),
      Some(ProxyHost::Domain(name)) => {
        if !self.answered(&name, ip) {
          // not an answer given here (or an old one): ask again
          if !allowed(&name, port) {
            return Err(refused(format_args!(
              "{name}:{port} is not in the network grant"
            )));
          }
          if !self.resolve_name(&name)?.contains(&ip) {
            return Err(refused(format_args!(
              "{ip} is not an address of {name}"
            )));
          }
        }
        through(&name)
      }
      Some(ProxyHost::Ip(_)) | None => {
        if allowed(&ip_text(ip), port) {
          return Ok(());
        }
        // an address with no name of its own: one a granted name led to
        let mut last = None;
        for name in self.names_answering(ip) {
          match through(&name) {
            Ok(()) => return Ok(()),
            Err(err) => last = Some(err),
          }
        }
        Err(last.unwrap_or_else(not_granted))
      }
    }
  }

  /// A TCP socket connected to exactly `request.addr`.
  pub fn connect_tcp(&self, request: &TcpConnect) -> io::Result<OwnedFd> {
    let addr = request.addr;
    self.reaches(addr, request.host.as_deref(), &|host, port| {
      self.decider.connect(host, port)
    })?;
    if !self.decider.connect_resolved(addr.ip(), addr.port()) {
      return Err(refused(format_args!("{addr} is denied")));
    }
    if let Some(local) = request.local {
      self.may_bind(local, None)?;
    }
    let socket = Socket::new(
      Domain::for_address(addr),
      Type::STREAM,
      Some(Protocol::TCP),
    )?;
    if let Some(local) = request.local {
      socket.set_reuse_address(true)?;
      socket.bind(&local.into())?;
    }
    socket.connect_timeout(&addr.into(), CONNECT_TIMEOUT)?;
    Ok(socket.into())
  }

  fn may_bind(&self, addr: SocketAddr, host: Option<&str>) -> io::Result<()> {
    self.reaches(addr, host, &|host, port| self.decider.listen(host, port))?;
    if !self.decider.listen_resolved(addr.ip(), addr.port()) {
      return Err(refused(format_args!("listening on {addr} is denied")));
    }
    Ok(())
  }

  /// A TCP socket bound to `request.addr` and listening.
  pub fn bind_tcp(&self, request: &TcpBind) -> io::Result<OwnedFd> {
    let addr = request.addr;
    self.may_bind(addr, request.host.as_deref())?;
    let socket = Socket::new(
      Domain::for_address(addr),
      Type::STREAM,
      Some(Protocol::TCP),
    )?;
    if let Some(only_v6) = request.ipv6_only
      && addr.is_ipv6()
    {
      socket.set_only_v6(only_v6)?;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if request.reuse_port {
      socket.set_reuse_port(true)?;
    }
    // as every listener of the runtime: a port is usable again right after
    // its last listener closed
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    socket.listen(request.backlog.max(0))?;
    Ok(socket.into())
  }

  /// A UDP socket. Only when a socket that can send anywhere holds the
  /// grant (see the module docs); the bind is checked as a listen unless it
  /// asks for no address and no port of its own.
  pub fn open_udp(&self, request: &UdpOpen) -> io::Result<OwnedFd> {
    if !self.decider.open_datagrams() {
      return Err(refused(
        "a UDP socket can send to any address, so a host-scoped network grant has none (see the kernel sandbox's network notes)",
      ));
    }
    let ephemeral = request.bind.ip().is_unspecified()
      && request.bind.port() == 0
      && request.connect.is_some();
    if !ephemeral {
      self.may_bind(request.bind, None)?;
    }
    if let Some(peer) = request.connect {
      self.reaches(peer, request.host.as_deref(), &|host, port| {
        self.decider.connect(host, port)
      })?;
      if !self.decider.connect_resolved(peer.ip(), peer.port()) {
        return Err(refused(format_args!("{peer} is denied")));
      }
    }
    let socket = Socket::new(
      Domain::for_address(request.bind),
      Type::DGRAM,
      Some(Protocol::UDP),
    )?;
    if let Some(only_v6) = request.ipv6_only
      && request.bind.is_ipv6()
    {
      socket.set_only_v6(only_v6)?;
    }
    if request.reuse_address {
      socket.set_reuse_address(true)?;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if request.reuse_port {
      socket.set_reuse_port(true)?;
    }
    socket.bind(&request.bind.into())?;
    if let Some(peer) = request.connect {
      socket.connect(&peer.into())?;
    }
    Ok(socket.into())
  }

  /// Sends `payload` to `server` (an address the grant names itself) and
  /// returns the first datagram that comes back from it.
  pub fn udp_exchange(
    &self,
    server: SocketAddr,
    payload: &[u8],
    timeout: Duration,
  ) -> io::Result<Vec<u8>> {
    if payload.len() > MAX_DATAGRAM {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "the datagram is too large",
      ));
    }
    if !self.decider.connect(&ip_text(server.ip()), server.port())
      || !self.decider.connect_resolved(server.ip(), server.port())
    {
      return Err(refused(format_args!(
        "{server} is not in the network grant"
      )));
    }
    let socket = Socket::new(
      Domain::for_address(server),
      Type::DGRAM,
      Some(Protocol::UDP),
    )?;
    // connected: only this server's datagrams arrive, and an unreachable
    // port reports ECONNREFUSED
    socket.connect(&server.into())?;
    socket.set_read_timeout(Some(
      timeout
        .min(MAX_EXCHANGE_TIMEOUT)
        .max(Duration::from_millis(1)),
    ))?;
    socket.send(payload)?;
    let mut answer = vec![0u8; MAX_DATAGRAM];
    let n = (&socket)
      .read(&mut answer)
      .map_err(|err| match err.kind() {
        io::ErrorKind::WouldBlock => {
          io::Error::new(io::ErrorKind::TimedOut, "no answer in time")
        }
        _ => err,
      })?;
    answer.truncate(n);
    Ok(answer)
  }

  /// A Unix stream socket connected to `path`.
  ///
  /// The check names a path and a connect follows the links on it, so the
  /// socket that is connected is the one that was checked (Linux, macOS):
  /// the folder that holds the name is opened first and the path the kernel
  /// gives that folder, with the name, must be in the file grant too
  /// ([`NetDecider::unix_place`]); a name
  /// that is a link is followed here, one hop at a time, each place it
  /// leads to checked the same way. On Linux the socket is then pinned
  /// (`O_PATH | O_NOFOLLOW`) and connected through its descriptor, so
  /// nothing swapped in afterwards is followed. macOS has no such connect:
  /// there the name is connected by the folder's real path and must still
  /// be the same socket afterwards (a swap that puts the same socket back
  /// in between is not seen).
  pub fn connect_unix(&self, path: &Path) -> io::Result<OwnedFd> {
    if !self.decider.unix(path) {
      return Err(not_granted(path));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if !is_abstract(path) {
      let checked = path;
      let mut path = path.to_path_buf();
      for _ in 0..MAX_SOCKET_LINKS {
        let held = held_name::HeldName::open(&path)?;
        if held.real() != checked && !self.decider.unix_place(held.real()) {
          return Err(not_granted(held.real()));
        }
        match held.link_target()? {
          // a relative target is the folder's (`join` takes an absolute
          // one as it is)
          Some(target) => path = held.folder().join(target),
          None => return Ok(held.connect()?.into()),
        }
      }
      return Err(refused(format_args!(
        "the socket {} is behind too many links",
        path.display()
      )));
    }
    Ok(std::os::unix::net::UnixStream::connect(path)?.into())
  }

  /// A Unix socket bound to `path`: a listening stream socket, or a
  /// datagram one.
  ///
  /// As for a connect, the folder that holds the name is opened first and
  /// its real path, with the name, must be in the file grant (Linux, macOS). The
  /// socket is bound by the path as given (the address it reports), and
  /// must then be in that folder: a folder swapped in between leaves the
  /// request refused and the socket closed (its file stays where the swap
  /// put it).
  pub fn bind_unix(&self, path: &Path, datagram: bool) -> io::Result<OwnedFd> {
    if !self.decider.unix_listen(path) {
      return Err(not_granted(path));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let held = if is_abstract(path) {
      None
    } else {
      let held = held_name::HeldName::open(path)?;
      if held.real() != path && !self.decider.unix_place(held.real()) {
        return Err(not_granted(held.real()));
      }
      Some(held)
    };
    let socket: OwnedFd = if datagram {
      std::os::unix::net::UnixDatagram::bind(path)?.into()
    } else {
      std::os::unix::net::UnixListener::bind(path)?.into()
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(held) = held
      && !held.is_socket()
    {
      return Err(refused(format_args!(
        "the socket {} was not made in the folder that was checked",
        path.display()
      )));
    }
    Ok(socket)
  }
}

fn not_granted(path: &Path) -> io::Error {
  refused(format_args!(
    "the socket {} is not in the grant",
    path.display()
  ))
}

/// The most links a socket's name is followed through.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_SOCKET_LINKS: usize = 8;

/// Whether `path` is a name in Linux's abstract namespace (a leading NUL):
/// no file, nothing to follow.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_abstract(path: &Path) -> bool {
  use std::os::unix::ffi::OsStrExt;
  path.as_os_str().as_bytes().first() == Some(&0)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod held_name {
  //! A socket's name in the folder that holds it, with the folder open:
  //! what was checked is what is connected or bound.

  use std::ffi::CString;
  use std::fs::File;
  use std::io;
  use std::os::fd::AsRawFd;
  use std::os::unix::ffi::OsStrExt;
  use std::os::unix::net::UnixStream;
  use std::path::Path;
  use std::path::PathBuf;

  use deno_net_route::refused;

  pub struct HeldName {
    dir: File,
    name: CString,
    /// The folder as the kernel names the open descriptor.
    folder: PathBuf,
    /// `folder` with the name.
    real: PathBuf,
  }

  /// Where the open folder is, as the kernel names it.
  #[cfg_attr(
    target_os = "linux",
    allow(
      clippy::disallowed_methods,
      reason = "the kernel's name for a descriptor this process holds"
    )
  )]
  fn folder_path(dir: &File) -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
      let path =
        std::fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
      // a folder removed meanwhile reads back as `<path> (deleted)`, and
      // nothing that isn't absolute is a place
      if !path.is_absolute()
        || path.as_os_str().as_bytes().ends_with(b" (deleted)")
      {
        return Err(io::Error::from(io::ErrorKind::NotFound));
      }
      Ok(path)
    }
    #[cfg(target_os = "macos")]
    {
      use std::os::unix::ffi::OsStringExt;
      let mut buf = vec![0u8; libc::PATH_MAX as usize];
      // SAFETY: F_GETPATH writes at most PATH_MAX bytes into the buffer
      if unsafe {
        libc::fcntl(dir.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr())
      } != 0
      {
        return Err(io::Error::last_os_error());
      }
      let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
      buf.truncate(len);
      Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
    }
  }

  impl HeldName {
    pub fn open(path: &Path) -> io::Result<Self> {
      let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
      let name = path.file_name().ok_or_else(invalid)?;
      let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
      };
      // the folder, links on the way followed: the descriptor is the place
      let dir = File::open(parent)?;
      if !dir.metadata()?.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
      }
      let folder = folder_path(&dir)?;
      let real = folder.join(name);
      let name = CString::new(name.as_bytes()).map_err(|_| invalid())?;
      Ok(Self {
        dir,
        name,
        folder,
        real,
      })
    }

    pub fn real(&self) -> &Path {
      &self.real
    }

    pub fn folder(&self) -> &Path {
      &self.folder
    }

    /// `lstat` of the name in the open folder.
    fn stat(&self) -> io::Result<libc::stat> {
      // SAFETY: a zeroed stat is a valid out parameter
      let mut st: libc::stat = unsafe { std::mem::zeroed() };
      // SAFETY: an open descriptor, a C string and an out parameter
      let res = unsafe {
        libc::fstatat(
          self.dir.as_raw_fd(),
          self.name.as_ptr(),
          &mut st,
          libc::AT_SYMLINK_NOFOLLOW,
        )
      };
      if res != 0 {
        return Err(io::Error::last_os_error());
      }
      Ok(st)
    }

    pub fn is_socket(&self) -> bool {
      self
        .stat()
        .is_ok_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFSOCK)
    }

    /// The target of the name when it is a symlink.
    pub fn link_target(&self) -> io::Result<Option<PathBuf>> {
      use std::os::unix::ffi::OsStringExt;
      if self.stat()?.st_mode & libc::S_IFMT != libc::S_IFLNK {
        return Ok(None);
      }
      let mut buf = vec![0u8; libc::PATH_MAX as usize];
      // SAFETY: an open descriptor, a C string and a buffer of that length
      let len = unsafe {
        libc::readlinkat(
          self.dir.as_raw_fd(),
          self.name.as_ptr(),
          buf.as_mut_ptr() as *mut libc::c_char,
          buf.len(),
        )
      };
      if len < 0 {
        return Err(io::Error::last_os_error());
      }
      buf.truncate(len as usize);
      Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(buf))))
    }

    fn changed(&self) -> io::Error {
      refused(format_args!(
        "the socket {} changed while it was being connected",
        self.real.display()
      ))
    }

    /// Connects to the socket the name is, never through a link put in
    /// its place: the object is pinned and connected by its descriptor.
    #[cfg(target_os = "linux")]
    pub fn connect(&self) -> io::Result<UnixStream> {
      use std::os::fd::FromRawFd;
      use std::os::fd::OwnedFd;
      // SAFETY: an open descriptor and a C string; owned once valid
      let pinned = unsafe {
        let fd = libc::openat(
          self.dir.as_raw_fd(),
          self.name.as_ptr(),
          libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        );
        if fd < 0 {
          return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(fd)
      };
      // a link swapped in since the name was looked at is pinned as a link
      if File::from(pinned.try_clone()?)
        .metadata()?
        .file_type()
        .is_symlink()
      {
        return Err(self.changed());
      }
      UnixStream::connect(format!("/proc/self/fd/{}", pinned.as_raw_fd()))
    }

    /// Connects by the folder's real path, and checks that the name is
    /// still the socket it was.
    #[cfg(target_os = "macos")]
    pub fn connect(&self) -> io::Result<UnixStream> {
      let identity = |st: &libc::stat| (st.st_dev, st.st_ino, st.st_mode);
      let before = self.stat()?;
      let stream = UnixStream::connect(&self.real)?;
      match self.stat() {
        Ok(after) if identity(&after) == identity(&before) => Ok(stream),
        _ => Err(self.changed()),
      }
    }
  }
}

/// A host as a request names it; anything that isn't one is refused.
fn parse_host(host: &str) -> io::Result<ProxyHost> {
  let host = host.trim_start_matches('[').trim_end_matches(']');
  ProxyHost::parse(host)
    .map(|host| match host {
      ProxyHost::Domain(name) => ProxyHost::Domain(name.to_ascii_lowercase()),
      ip => ip,
    })
    .ok_or_else(|| refused(format_args!("{host:?} is not a host")))
}

/// The authority as a route of its own process: no boundary between the
/// two, so it confines nothing. It is how the routing of every socket path
/// is tested without a supervisor, and what the supervisor's protocol
/// stands in for.
pub struct InProcessRoute(pub Arc<NetAuthority>);

impl NetRoute for InProcessRoute {
  fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
    self.0.resolve(host)
  }

  fn connect_tcp(&self, request: &TcpConnect) -> io::Result<OwnedFd> {
    self.0.connect_tcp(request)
  }

  fn bind_tcp(&self, request: &TcpBind) -> io::Result<OwnedFd> {
    self.0.bind_tcp(request)
  }

  fn open_udp(&self, request: &UdpOpen) -> io::Result<OwnedFd> {
    self.0.open_udp(request)
  }

  fn udp_exchange(
    &self,
    server: SocketAddr,
    payload: &[u8],
    timeout: Duration,
  ) -> io::Result<Vec<u8>> {
    self.0.udp_exchange(server, payload, timeout)
  }

  fn connect_unix(&self, path: &Path) -> io::Result<OwnedFd> {
    self.0.connect_unix(path)
  }

  fn bind_unix(&self, path: &Path, datagram: bool) -> io::Result<OwnedFd> {
    self.0.bind_unix(path, datagram)
  }
}

#[cfg(test)]
mod tests {
  use std::net::Ipv4Addr;
  use std::net::Ipv6Addr;
  use std::net::TcpListener;
  use std::net::UdpSocket;

  use super::*;

  /// A grant as lists of `host` / `host:port` (names and IP literals,
  /// matched as written), for both directions.
  #[derive(Default)]
  struct Lists {
    allow: Vec<(String, Option<u16>)>,
    deny_ips: Vec<IpAddr>,
    all: bool,
  }

  impl Lists {
    fn of(entries: &[&str]) -> Self {
      Self {
        allow: entries
          .iter()
          .map(|e| match e.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => {
              (host.to_string(), Some(port.parse().unwrap()))
            }
            _ => (e.to_string(), None),
          })
          .collect(),
        ..Default::default()
      }
    }

    fn allows(&self, host: &str, port: u16) -> bool {
      self.all
        || self
          .allow
          .iter()
          .any(|(h, p)| h == host && p.is_none_or(|p| p == port))
    }
  }

  impl NetDecider for Lists {
    fn connect(&self, host: &str, port: u16) -> bool {
      self.allows(host, port)
        && host
          .parse::<IpAddr>()
          .map_or(true, |ip| !self.deny_ips.contains(&ip))
    }
    fn connect_resolved(&self, ip: IpAddr, _port: u16) -> bool {
      !self.deny_ips.contains(&ip)
    }
    fn listen(&self, host: &str, port: u16) -> bool {
      self.allows(host, port)
    }
    fn listen_resolved(&self, ip: IpAddr, _port: u16) -> bool {
      !self.deny_ips.contains(&ip)
    }
    fn names(&self, host: &str) -> bool {
      self.all || self.allow.iter().any(|(h, _)| h == host)
    }
    fn open_datagrams(&self) -> bool {
      self.all && self.deny_ips.is_empty()
    }
    fn unix(&self, path: &Path) -> bool {
      self.allow.iter().any(|(h, _)| Path::new(h) == path)
    }
  }

  fn fixed(table: &[(&str, &[IpAddr])]) -> Resolve {
    let table: HashMap<String, Vec<IpAddr>> = table
      .iter()
      .map(|(name, addrs)| (name.to_string(), addrs.to_vec()))
      .collect();
    Arc::new(move |host| {
      table
        .get(host)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such name"))
    })
  }

  fn authority(lists: Lists, table: &[(&str, &[IpAddr])]) -> NetAuthority {
    NetAuthority::with_resolver(Arc::new(lists), fixed(table))
  }

  const PUBLIC: IpAddr = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
  const LOOP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
  const META: IpAddr = IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254));

  fn connect(
    authority: &NetAuthority,
    addr: SocketAddr,
    host: Option<&str>,
  ) -> io::Result<OwnedFd> {
    authority.connect_tcp(&TcpConnect {
      addr,
      host: host.map(str::to_string),
      local: None,
    })
  }

  fn refused_with(result: io::Result<impl Sized>, what: &str) {
    let err = result.err().expect("refused");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    assert!(err.to_string().contains(what), "{err}");
  }

  #[test]
  fn only_granted_names_resolve() {
    let a = authority(
      Lists::of(&["example.com:443"]),
      &[("example.com", &[PUBLIC]), ("other.test", &[PUBLIC])],
    );
    assert_eq!(a.resolve("example.com").unwrap(), vec![PUBLIC]);
    // the name as a URL writes it
    assert_eq!(a.resolve("EXAMPLE.com.").unwrap(), vec![PUBLIC]);
    refused_with(a.resolve("other.test"), "other.test is not in");
    // a literal is its own answer, in every spelling a URL takes
    assert_eq!(a.resolve("10.0.0.1").unwrap().len(), 1);
    assert_eq!(a.resolve("0x7f.1").unwrap(), vec![LOOP]);
    assert_eq!(
      a.resolve("[::1]").unwrap(),
      vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
    );
    refused_with(a.resolve("not a host"), "is not a host");
  }

  #[test]
  fn a_granted_name_cannot_be_rebound_inside() {
    for inside in [
      LOOP,
      META,
      IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
      IpAddr::V6(Ipv6Addr::LOCALHOST),
      "::ffff:127.0.0.1".parse().unwrap(),
      "fd00::1".parse().unwrap(),
    ] {
      let a = authority(
        Lists::of(&["example.com"]),
        // mixed with a public answer, as a rebinding record set is
        &[("example.com", &[PUBLIC, inside])],
      );
      refused_with(a.resolve("example.com"), "internal address");
      // nothing was remembered: neither answer is connectable
      refused_with(
        connect(&a, SocketAddr::new(PUBLIC, 443), None),
        "not in the network grant",
      );
      refused_with(
        connect(&a, SocketAddr::new(inside, 443), Some("example.com")),
        "internal address",
      );
    }
  }

  #[test]
  fn an_internal_answer_needs_the_address_in_the_grant() {
    let db = IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3));
    // the name alone: refused; with the address: honoured
    let a = authority(
      Lists::of(&["db.internal:5432", "10.1.2.3:5432"]),
      &[("db.internal", &[db])],
    );
    assert_eq!(a.resolve("db.internal").unwrap(), vec![db]);
    // the address is named for 5432 only
    let a = authority(
      Lists::of(&["db.internal", "10.1.2.3:5432"]),
      &[("db.internal", &[db])],
    );
    a.resolve("db.internal").unwrap();
    refused_with(
      connect(&a, SocketAddr::new(db, 80), Some("db.internal")),
      "internal address",
    );
  }

  #[test]
  fn localhost_leads_to_loopback_only() {
    let a = authority(
      Lists::of(&["localhost", "app.localhost"]),
      &[
        ("localhost", &[LOOP]),
        ("app.localhost", &[IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]),
      ],
    );
    assert_eq!(a.resolve("localhost").unwrap(), vec![LOOP]);
    refused_with(a.resolve("app.localhost"), "not a loopback");
  }

  #[test]
  fn connects_to_exactly_what_was_checked() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let other = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let other_port = other.local_addr().unwrap().port();
    let a = authority(
      Lists::of(&[&format!("localhost:{port}")]),
      &[("localhost", &[LOOP])],
    );
    let here = SocketAddr::new(LOOP, port);

    // an address nothing here resolved: refused, even on the granted port
    refused_with(connect(&a, here, None), "not in the network grant");
    // with the name it came from: resolved here, then connected
    let fd = connect(&a, here, Some("localhost")).unwrap();
    assert_eq!(std::net::TcpStream::from(fd).peer_addr().unwrap(), here);
    // and now the answer is remembered: the two-step callers work
    connect(&a, here, None).unwrap();
    // the port is part of the grant
    refused_with(
      connect(&a, SocketAddr::new(LOOP, other_port), Some("localhost")),
      "not in the network grant",
    );
    refused_with(
      connect(&a, SocketAddr::new(LOOP, other_port), None),
      "not in the network grant",
    );
    // a name that doesn't lead to the address
    refused_with(
      connect(&a, SocketAddr::new(PUBLIC, port), Some("localhost")),
      "is not an address of localhost",
    );
    // a claimed literal that isn't the address
    refused_with(
      connect(&a, here, Some("93.184.216.34")),
      "not the same address",
    );
  }

  #[test]
  fn an_answer_is_forgotten() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let a = authority(
      Lists::of(&[&format!("localhost:{port}")]),
      &[("localhost", &[LOOP])],
    )
    .with_answer_ttl(Duration::ZERO);
    a.resolve("localhost").unwrap();
    let here = SocketAddr::new(LOOP, port);
    refused_with(connect(&a, here, None), "not in the network grant");
    // the name still gets there: it is resolved again
    connect(&a, here, Some("localhost")).unwrap();
  }

  #[test]
  fn a_literal_in_the_grant_is_reached_as_written() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let a = authority(Lists::of(&[&format!("127.0.0.1:{port}")]), &[]);
    connect(&a, SocketAddr::new(LOOP, port), None).unwrap();
    connect(&a, SocketAddr::new(LOOP, port), Some("127.0.0.1")).unwrap();
    // another loopback address is another address
    refused_with(
      connect(
        &a,
        SocketAddr::new("127.0.0.2".parse().unwrap(), port),
        None,
      ),
      "not in the network grant",
    );
  }

  #[test]
  fn deny_rules_hold_for_the_address() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut lists = Lists::of(&["localhost"]);
    lists.deny_ips.push(LOOP);
    let a = authority(lists, &[("localhost", &[LOOP])]);
    refused_with(
      connect(&a, SocketAddr::new(LOOP, port), Some("localhost")),
      "is denied",
    );
  }

  #[test]
  fn listeners_are_bound_here() {
    let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let a = authority(
      Lists::of(&[&format!("127.0.0.1:{port}")]),
      &[("localhost", &[LOOP])],
    );
    let bind = |addr: SocketAddr, host: Option<&str>| {
      a.bind_tcp(&TcpBind {
        addr,
        host: host.map(str::to_string),
        reuse_port: false,
        backlog: 16,
        ipv6_only: None,
      })
    };
    let addr = SocketAddr::new(LOOP, port);
    let listener = TcpListener::from(bind(addr, None).unwrap());
    assert_eq!(listener.local_addr().unwrap(), addr);
    // it listens already: a connection completes without another call
    std::net::TcpStream::connect(addr).unwrap();
    // another port, every interface, a name the grant doesn't hold
    refused_with(bind(SocketAddr::new(LOOP, 0), None), "not in the network");
    refused_with(
      bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port), None),
      "not in the network",
    );
    refused_with(bind(addr, Some("localhost")), "localhost:");
  }

  #[test]
  fn no_udp_socket_under_a_scoped_grant() {
    let peer = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let open = |a: &NetAuthority, connect: Option<SocketAddr>| {
      a.open_udp(&UdpOpen {
        bind: SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
        connect,
        host: None,
        reuse_address: false,
        reuse_port: false,
        ipv6_only: None,
      })
    };
    let scoped = authority(Lists::of(&[&peer_addr.to_string()]), &[]);
    // even connected to the one granted peer
    refused_with(open(&scoped, Some(peer_addr)), "can send to any address");
    let open_grant = authority(
      Lists {
        all: true,
        ..Default::default()
      },
      &[],
    );
    let socket = UdpSocket::from(open(&open_grant, Some(peer_addr)).unwrap());
    assert_eq!(socket.peer_addr().unwrap(), peer_addr);
    // an unscoped grant with a deny is scoped again
    let denied = authority(
      Lists {
        all: true,
        deny_ips: vec![META],
        ..Default::default()
      },
      &[],
    );
    refused_with(open(&denied, Some(peer_addr)), "can send to any address");
  }

  #[test]
  fn a_datagram_exchange_stays_here() {
    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let server_addr = server.local_addr().unwrap();
    let echo = std::thread::spawn(move || {
      let mut buf = [0u8; 64];
      let (n, from) = server.recv_from(&mut buf).unwrap();
      buf[..n].reverse();
      server.send_to(&buf[..n], from).unwrap();
    });
    let a = authority(Lists::of(&[&server_addr.to_string()]), &[]);
    let answer = a
      .udp_exchange(server_addr, b"query", Duration::from_secs(10))
      .unwrap();
    assert_eq!(answer, b"yreuq");
    echo.join().unwrap();
    // a server the grant doesn't name
    let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    refused_with(
      a.udp_exchange(
        silent.local_addr().unwrap(),
        b"query",
        Duration::from_secs(1),
      ),
      "not in the network grant",
    );
    // a granted server that never answers
    let b =
      authority(Lists::of(&[&silent.local_addr().unwrap().to_string()]), &[]);
    let err = b
      .udp_exchange(
        silent.local_addr().unwrap(),
        b"query",
        Duration::from_millis(50),
      )
      .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
  }

  #[test]
  fn the_answer_table_is_bounded() {
    let names: Vec<String> =
      (0..MAX_ANSWERS + 8).map(|i| format!("n{i}.test")).collect();
    let lists = Lists {
      all: true,
      ..Default::default()
    };
    let a = NetAuthority::with_resolver(
      Arc::new(lists),
      Arc::new(|_| Ok(vec![PUBLIC])),
    );
    for name in &names {
      a.resolve(name).unwrap();
    }
    assert!(a.answers.lock().unwrap().len() <= MAX_ANSWERS);
  }

  #[test]
  fn the_in_process_route_is_the_authority() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let route = InProcessRoute(Arc::new(authority(
      Lists::of(&[&format!("localhost:{port}")]),
      &[("localhost", &[LOOP])],
    )));
    assert_eq!(route.resolve("localhost").unwrap(), vec![LOOP]);
    route
      .connect_tcp(&TcpConnect {
        addr: SocketAddr::new(LOOP, port),
        host: None,
        local: None,
      })
      .unwrap();
    refused_with(route.resolve("example.com"), "not in the network grant");
  }

  /// A grant of Unix socket paths, each matched as written.
  #[cfg(any(target_os = "linux", target_os = "macos"))]
  fn sockets(paths: &[&Path]) -> NetAuthority {
    let lists = Lists {
      allow: paths
        .iter()
        .map(|p| (p.to_str().unwrap().to_string(), None))
        .collect(),
      ..Default::default()
    };
    authority(lists, &[])
  }

  #[cfg(any(target_os = "linux", target_os = "macos"))]
  #[test]
  #[allow(
    clippy::disallowed_methods,
    reason = "a real folder with real sockets and links"
  )]
  fn a_unix_socket_is_connected_where_it_was_checked() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let granted = base.join("granted");
    let outside = base.join("outside");
    std::fs::create_dir(&granted).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let inside = UnixListener::bind(granted.join("in.sock")).unwrap();
    let other = UnixListener::bind(outside.join("out.sock")).unwrap();
    inside.set_nonblocking(true).unwrap();
    other.set_nonblocking(true).unwrap();
    // a name in the granted folder that is a link to the socket outside,
    // a folder in it that is a link to the folder outside, and a link that
    // stays with a granted name
    symlink(outside.join("out.sock"), granted.join("link.sock")).unwrap();
    symlink(&outside, granted.join("dir")).unwrap();
    symlink("in.sock", granted.join("alias.sock")).unwrap();
    symlink("alias.sock", granted.join("alias2.sock")).unwrap();

    let a = sockets(&[
      &granted.join("in.sock"),
      &granted.join("link.sock"),
      &granted.join("dir/out.sock"),
      &granted.join("alias.sock"),
      &granted.join("alias2.sock"),
    ]);
    // the socket itself, and through links to a granted place
    for name in ["in.sock", "alias.sock", "alias2.sock"] {
      a.connect_unix(&granted.join(name)).unwrap();
      inside.accept().unwrap();
    }
    // a granted name that leads to a socket outside the grant: through a
    // link as the name, and through a link as its folder
    for name in ["link.sock", "dir/out.sock"] {
      refused_with(a.connect_unix(&granted.join(name)), "not in the grant");
    }
    assert_eq!(
      other.accept().unwrap_err().kind(),
      io::ErrorKind::WouldBlock,
      "nothing reached the socket outside"
    );
    // a name the grant doesn't hold, and one that isn't there
    refused_with(
      a.connect_unix(&outside.join("out.sock")),
      "not in the grant",
    );
    let missing = granted.join("missing.sock");
    assert_eq!(
      sockets(&[&missing])
        .connect_unix(&missing)
        .unwrap_err()
        .kind(),
      io::ErrorKind::NotFound
    );
    // a link to itself ends
    symlink("loop.sock", granted.join("loop.sock")).unwrap();
    let looped = granted.join("loop.sock");
    refused_with(sockets(&[&looped]).connect_unix(&looped), "too many links");
  }

  #[cfg(any(target_os = "linux", target_os = "macos"))]
  #[test]
  #[allow(
    clippy::disallowed_methods,
    reason = "a real folder with real sockets and links"
  )]
  fn a_unix_socket_is_bound_where_it_was_checked() {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let granted = base.join("granted");
    let outside = base.join("outside");
    std::fs::create_dir(&granted).unwrap();
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, granted.join("dir")).unwrap();

    let a =
      sockets(&[&granted.join("new.sock"), &granted.join("dir/new.sock")]);
    // a granted name whose folder is a link out of the grant: nothing is
    // made there
    for datagram in [false, true] {
      refused_with(
        a.bind_unix(&granted.join("dir/new.sock"), datagram),
        "not in the grant",
      );
      assert!(std::fs::symlink_metadata(outside.join("new.sock")).is_err());
    }
    // the granted name itself
    let _listener = a.bind_unix(&granted.join("new.sock"), false).unwrap();
    assert!(
      std::fs::symlink_metadata(granted.join("new.sock"))
        .unwrap()
        .file_type()
        .is_socket()
    );
    refused_with(
      a.bind_unix(&granted.join("other.sock"), false),
      "not in the grant",
    );
  }
}
