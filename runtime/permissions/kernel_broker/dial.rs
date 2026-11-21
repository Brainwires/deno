// Copyright 2018-2026 the Deno authors. MIT license.

//! The dial: how a `deno` or `denort` process started inside a brokered run
//! reaches the run's broker (denext).
//!
//! A broker channel carries one request at a time and belongs to one
//! process, so a child can't share its parent's. The confined program
//! instead leaves three descriptors open for everything it starts, named in
//! [`DIAL_ENV_VAR`]:
//!
//! - a channel of the broker that nobody uses for anything but asking for
//!   another channel ([`Request::Channel`](super::protocol::Request));
//! - the two ends of a pipe that holds one byte, the token: whoever reads
//!   it may use that channel, and writes it back when done.
//!
//! A child reads the token, asks for a channel of its own, gives the token
//! back, and from then on talks to the broker on its own channel
//! ([`Dial::connect`]). Nothing new travels toward the broker and the
//! broker learns nothing about who asks: it holds the one policy of the run
//! it supervises and answers every channel against it, so a child gets
//! through the dial exactly what native code in its parent could already
//! ask for. A process that confines itself further must not keep the dial
//! (its own, narrower grant would not hold against the broker's answers):
//! it closes it before it applies its sandbox ([`Dial::close`]).
//!
//! The variable is never trusted on its own. Each descriptor must be of the
//! right kind and, where the OS numbers them, the very object the variable
//! names (a program in between may have closed the descriptors and opened
//! something else under the same numbers). A dial that doesn't check, a
//! token that doesn't come, or a broker that doesn't answer leaves the
//! process without a broker: the kernel still refuses what it refused.

use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::time::Duration;
use std::time::Instant;

use super::client::BrokerClient;

/// Names the dial's descriptors for the processes a brokered run starts:
/// `<channel>:<inode>,<token read end>:<inode>,<token write end>:<inode>`.
pub const DIAL_ENV_VAR: &str = "DENO_INTERNAL_KERNEL_BROKER_DIAL";

/// The longest a process waits for the token. It is held for one request
/// and its answer; one that never comes back was lost with its holder.
pub const TOKEN_WAIT: Duration = Duration::from_secs(2);

/// The dial's three descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dial {
  channel: i32,
  token_read: i32,
  token_write: i32,
}

/// The number the OS gives the object behind a descriptor (0 where it
/// gives none).
fn inode_of(st: &libc::stat) -> u64 {
  #[allow(clippy::unnecessary_cast, reason = "ino_t widths differ by platform")]
  {
    st.st_ino as u64
  }
}

fn stat(fd: i32) -> io::Result<libc::stat> {
  // SAFETY: a zeroed stat is a valid out parameter
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a descriptor number and an out parameter
  if unsafe { libc::fstat(fd, &mut st) } != 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(st)
}

fn set_cloexec(fd: i32, on: bool) -> io::Result<()> {
  // SAFETY: flags of a descriptor number
  unsafe {
    let flags = libc::fcntl(fd, libc::F_GETFD);
    if flags < 0 {
      return Err(io::Error::last_os_error());
    }
    let flags = if on {
      flags | libc::FD_CLOEXEC
    } else {
      flags & !libc::FD_CLOEXEC
    };
    if libc::fcntl(fd, libc::F_SETFD, flags) < 0 {
      return Err(io::Error::last_os_error());
    }
  }
  Ok(())
}

/// The lowest number a dial's descriptor gets.
const FIRST_DIAL_FD: libc::c_int = 100;

/// `fd` again at [`FIRST_DIAL_FD`] or above, not close-on-exec (the
/// original is closed).
fn move_up(fd: OwnedFd) -> io::Result<OwnedFd> {
  // SAFETY: duplicating a descriptor this function owns; owned once valid
  let moved = unsafe {
    let moved = libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD, FIRST_DIAL_FD);
    if moved < 0 {
      return Err(io::Error::last_os_error());
    }
    OwnedFd::from_raw_fd(moved)
  };
  set_cloexec(moved.as_raw_fd(), false)?;
  Ok(moved)
}

impl Dial {
  /// Makes the dial for the processes this one starts: a fresh channel of
  /// `client`'s broker and the token's pipe, none of them close-on-exec.
  /// Call while the process has one thread (the descriptors are meant to be
  /// inherited by every child; a child started meanwhile would get half).
  pub fn create(client: &BrokerClient) -> io::Result<Self> {
    let channel = client.channel()?;
    let mut pipe = [0 as libc::c_int; 2];
    // SAFETY: an array of two descriptors for the kernel to fill
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
      return Err(io::Error::last_os_error());
    }
    // SAFETY: just created, owned from here
    let (read, write) =
      unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    // the read end never blocks: two processes may see the token, one
    // gets it (the flag is the pipe's, so every inheritor has it)
    // SAFETY: flags of a descriptor this function owns; one byte written
    // from a valid buffer
    unsafe {
      let flags = libc::fcntl(read.as_raw_fd(), libc::F_GETFL);
      if flags < 0
        || libc::fcntl(
          read.as_raw_fd(),
          libc::F_SETFL,
          flags | libc::O_NONBLOCK,
        ) < 0
        || libc::write(write.as_raw_fd(), b"t".as_ptr() as *const _, 1) != 1
      {
        return Err(io::Error::last_os_error());
      }
    }
    // out of the way of the low numbers a program gives its children's
    // own descriptors (`stdio: [..., "pipe"]` puts them at 3 and up, over
    // whatever is there), and inherited from here on
    let channel = move_up(channel)?;
    let read = move_up(read)?;
    let write = move_up(write)?;
    Ok(Self {
      channel: channel.into_raw_fd(),
      token_read: read.into_raw_fd(),
      token_write: write.into_raw_fd(),
    })
  }

  pub fn fds(&self) -> [i32; 3] {
    [self.channel, self.token_read, self.token_write]
  }

  /// The value of [`DIAL_ENV_VAR`] for this dial.
  pub fn env_value(&self) -> String {
    self
      .fds()
      .iter()
      .map(|fd| {
        let inode = stat(*fd).map(|st| inode_of(&st)).unwrap_or(0);
        format!("{fd}:{inode}")
      })
      .collect::<Vec<_>>()
      .join(",")
  }

  /// The descriptor numbers a [`DIAL_ENV_VAR`] value names, unchecked (to
  /// leave them out of what a process counts as its inherited
  /// descriptors).
  pub fn named_fds(value: &str) -> Vec<i32> {
    value
      .split(',')
      .filter_map(|part| part.split(':').next()?.parse::<i32>().ok())
      .filter(|fd| *fd > 2)
      .collect()
  }

  /// The dial a [`DIAL_ENV_VAR`] value names, checked: three distinct open
  /// descriptors past stdio, a socket and two ends of a pipe, each the
  /// object the value names where the OS numbers it.
  pub fn from_env_value(value: &str) -> Result<Self, String> {
    let parts: Vec<(i32, u64)> = value
      .split(',')
      .map(|part| {
        let (fd, inode) = part.split_once(':')?;
        Some((fd.parse::<i32>().ok()?, inode.parse::<u64>().ok()?))
      })
      .collect::<Option<_>>()
      .ok_or_else(|| format!("{DIAL_ENV_VAR}={value:?} is not a dial"))?;
    let [channel, token_read, token_write] = parts[..] else {
      return Err(format!("{DIAL_ENV_VAR}={value:?} is not a dial"));
    };
    if channel.0 == token_read.0
      || channel.0 == token_write.0
      || token_read.0 == token_write.0
    {
      return Err(format!("{DIAL_ENV_VAR}={value:?} names a descriptor twice"));
    }
    for ((fd, inode), kind, what) in [
      (channel, libc::S_IFSOCK, "a socket"),
      (token_read, libc::S_IFIFO, "a pipe"),
      (token_write, libc::S_IFIFO, "a pipe"),
    ] {
      if fd <= 2 {
        return Err(format!("{DIAL_ENV_VAR}: {fd} is not a descriptor"));
      }
      let st = stat(fd)
        .map_err(|_| format!("{DIAL_ENV_VAR}: descriptor {fd} is not open"))?;
      if st.st_mode & libc::S_IFMT != kind {
        return Err(format!("{DIAL_ENV_VAR}: descriptor {fd} is not {what}"));
      }
      // macOS gives sockets no inode number: the kind is all there is
      if inode != 0 && inode_of(&st) != 0 && inode_of(&st) != inode {
        return Err(format!(
          "{DIAL_ENV_VAR}: descriptor {fd} is not the one that was handed down"
        ));
      }
    }
    Ok(Self {
      channel: channel.0,
      token_read: token_read.0,
      token_write: token_write.0,
    })
  }

  /// Reads the token, waiting at most `wait`.
  fn take_token(&self, wait: Duration) -> io::Result<()> {
    let deadline = Instant::now() + wait;
    loop {
      let mut byte = 0u8;
      // SAFETY: one byte into a valid buffer; the read end never blocks
      let n = unsafe {
        libc::read(self.token_read, &mut byte as *mut u8 as *mut _, 1)
      };
      match n {
        1 => return Ok(()),
        // every write end is closed: no token will ever come
        0 => {
          return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the broker dial's token is gone",
          ));
        }
        _ => {
          let err = io::Error::last_os_error();
          if !matches!(
            err.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
          ) {
            return Err(err);
          }
        }
      }
      let left = deadline.saturating_duration_since(Instant::now());
      if left.is_zero() {
        return Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "the broker dial's token didn't come back",
        ));
      }
      let mut poll = libc::pollfd {
        fd: self.token_read,
        events: libc::POLLIN,
        revents: 0,
      };
      // SAFETY: one pollfd
      unsafe { libc::poll(&mut poll, 1, left.as_millis().min(100) as i32) };
    }
  }

  fn give_token(&self) {
    // SAFETY: one byte from a valid buffer; a pipe that holds no byte never
    // blocks a one-byte write
    unsafe { libc::write(self.token_write, b"t".as_ptr() as *const _, 1) };
  }

  /// A channel of this process's own to the broker behind the dial: takes
  /// the token, asks for a channel, gives the token back.
  pub fn connect(&self) -> io::Result<OwnedFd> {
    self.connect_within(TOKEN_WAIT)
  }

  pub fn connect_within(&self, wait: Duration) -> io::Result<OwnedFd> {
    // a copy of our own, so the shared descriptor is never closed here
    // SAFETY: duplicating a descriptor number; owned once valid
    let channel = unsafe {
      let fd = libc::fcntl(self.channel, libc::F_DUPFD_CLOEXEC, 3);
      if fd < 0 {
        return Err(io::Error::last_os_error());
      }
      OwnedFd::from_raw_fd(fd)
    };
    self.take_token(wait)?;
    let result = BrokerClient::new(channel).channel();
    // given back whatever the answer was: a channel that is out of step
    // fails the next asker too, and a held token would only make it wait
    self.give_token();
    result
  }

  /// Closes the dial's descriptors in this process (a process that applies
  /// a sandbox of its own, before it does).
  pub fn close(self) {
    for fd in self.fds() {
      // SAFETY: the dial's descriptors, which this process gives up
      unsafe { libc::close(fd) };
    }
  }
}
