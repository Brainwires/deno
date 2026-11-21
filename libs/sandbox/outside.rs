// Copyright 2018-2026 the Deno authors. MIT license.

//! The outside helper of a kernel-confined desktop app (DENEXT.md item 483).
//!
//! A packaged desktop app's process confines itself in place
//! (`deno_lib::kernel_apply::start_desktop`), and everything it starts
//! afterwards inherits that confinement: on macOS the profile refuses
//! LaunchServices opens, so `open <url>` run as a child opens nothing, and
//! on both systems the updater's swap helper, a child too, could not
//! replace the install. Both are things the app is meant to do, and neither
//! can be allowed inside the sandbox without giving native code a way out
//! of it (a `.command` file written to a writable folder and opened through
//! LaunchServices runs unconfined).
//!
//! So the process forks this helper right before it confines itself. The
//! helper stays unconfined and does exactly what it was configured with
//! before the fork, nothing the confined side can choose:
//!
//! - [`Outside::open_url`]: run the system opener (`/usr/bin/open`,
//!   `xdg-open`) with one `http:`, `https:` or `mailto:` URL
//!   ([`valid_external_url`], checked in the helper);
//! - [`Outside::run_fixed`]: run one of the commands fixed at start (the
//!   updater's swap helper in its two modes), whose every argument, working
//!   directory and environment were set before the fork.
//!
//! The confined side, native code included, can send any bytes: a request
//! that isn't one of these is refused, and a malformed frame ends the
//! helper. The helper never reads a path, a program or an argument other
//! than the URL from the socket.
//!
//! After the fork the helper only makes system calls on buffers that exist
//! already (no allocation, no locks): the process may have other threads by
//! then (macOS applies its profile after AppKit started), and a forked
//! child of a threaded process must not take a lock another thread held.

#![allow(
  clippy::disallowed_methods,
  reason = "the helper acts on the real environment and file descriptors"
)]

use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Mutex;

/// The longest URL the helper opens.
pub const MAX_URL: usize = 2048;

const OP_OPEN: u8 = 1;
const OP_FIXED: u8 = 2;

const STATUS_OK: u8 = 0;
const STATUS_REFUSED: u8 = 1;
const STATUS_UNSUPPORTED: u8 = 2;
const STATUS_FAILED: u8 = 3;

/// Whether `url` is one the helper hands to the system opener: an `http:`,
/// `https:` or `mailto:` URL of at most [`MAX_URL`] bytes, made only of
/// printable ASCII with none of the characters a URL must percent-encode
/// (so it is one argument whatever reads it, and never an option: it starts
/// with its scheme). No allocation: the helper calls it after the fork.
pub fn valid_external_url(url: &[u8]) -> bool {
  fn has_prefix(url: &[u8], prefix: &[u8]) -> bool {
    url.len() > prefix.len() && url[..prefix.len()].eq_ignore_ascii_case(prefix)
  }
  if url.len() > MAX_URL {
    return false;
  }
  if !(has_prefix(url, b"http://")
    || has_prefix(url, b"https://")
    || has_prefix(url, b"mailto:"))
  {
    return false;
  }
  url.iter().all(|b| {
    matches!(b, 0x21..=0x7e)
      && !matches!(
        b,
        b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
      )
  })
}

/// A command the helper runs, fixed before the fork.
#[derive(Debug, Clone)]
pub struct FixedCommand {
  pub program: CString,
  /// The arguments after the program's own name.
  pub args: Vec<CString>,
  /// The working directory (`None`: the helper's).
  pub cwd: Option<CString>,
  /// The whole environment, `NAME=value` each.
  pub env: Vec<CString>,
}

/// What the helper may do, decided before the fork.
#[derive(Debug, Clone, Default)]
pub struct Config {
  /// The system opener (its path; the URL is its one argument). `None`:
  /// the app has no grant to run it, or the system has none.
  pub opener: Option<CString>,
  /// The commands [`Outside::run_fixed`] runs, by index.
  pub fixed: Vec<FixedCommand>,
}

/// A command as the helper's child execs it: every pointer array built.
struct Prepared {
  program: CString,
  /// `argv`, NUL-terminated; for the opener the slot before the terminator
  /// is the URL's.
  argv: Vec<*const libc::c_char>,
  envp: Vec<*const libc::c_char>,
  cwd: Option<CString>,
  // the strings `argv` and `envp` point into
  _keep: Vec<CString>,
}

impl Prepared {
  fn new(command: FixedCommand, url_slot: bool) -> Self {
    let mut keep = command.args;
    let env_at = keep.len();
    keep.extend(command.env);
    let mut argv: Vec<*const libc::c_char> = vec![command.program.as_ptr()];
    argv.extend(keep[..env_at].iter().map(|a| a.as_ptr()));
    if url_slot {
      argv.push(std::ptr::null());
    }
    argv.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> =
      keep[env_at..].iter().map(|e| e.as_ptr()).collect();
    envp.push(std::ptr::null());
    Self {
      program: command.program,
      argv,
      envp,
      cwd: command.cwd,
      _keep: keep,
    }
  }
}

/// The confined side's end.
#[derive(Debug)]
pub struct Outside {
  socket: Mutex<OwnedFd>,
  opener: bool,
  fixed: usize,
  helper: libc::pid_t,
}

impl Outside {
  /// The helper's process id.
  pub fn helper_pid(&self) -> i32 {
    self.helper
  }

  /// Whether the helper opens URLs (the app may run the system opener).
  pub fn opens_urls(&self) -> bool {
    self.opener
  }

  fn request(&self, op: u8, payload: &[u8]) -> io::Result<()> {
    let socket = self.socket.lock().unwrap_or_else(|e| e.into_inner());
    let fd = socket.as_raw_fd();
    let len = u16::try_from(payload.len())
      .map_err(|_| io::Error::other("the request is too long"))?;
    let mut frame = Vec::with_capacity(3 + payload.len());
    frame.push(op);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    if !write_all(fd, &frame) {
      return Err(io::Error::other("the outside helper is gone"));
    }
    let mut status = [0u8; 1];
    if !read_exact(fd, &mut status) {
      return Err(io::Error::other("the outside helper is gone"));
    }
    match status[0] {
      STATUS_OK => Ok(()),
      STATUS_REFUSED => Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "the outside helper refused the request",
      )),
      STATUS_UNSUPPORTED => Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the outside helper was not set up for this",
      )),
      _ => Err(io::Error::other("the outside helper could not start it")),
    }
  }

  /// Opens `url` with the system opener, outside the sandbox. Only an
  /// `http:`, `https:` or `mailto:` URL ([`valid_external_url`]).
  pub fn open_url(&self, url: &str) -> io::Result<()> {
    if !valid_external_url(url.as_bytes()) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "only http:, https: and mailto: URLs of printable ASCII open outside the sandbox",
      ));
    }
    if !self.opener {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the outside helper was not set up to open URLs",
      ));
    }
    self.request(OP_OPEN, url.as_bytes())
  }

  /// Runs the fixed command `index`, outside the sandbox.
  pub fn run_fixed(&self, index: usize) -> io::Result<()> {
    if index >= self.fixed {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no such fixed command",
      ));
    }
    self.request(OP_FIXED, &[index as u8])
  }
}

fn write_all(fd: i32, mut bytes: &[u8]) -> bool {
  while !bytes.is_empty() {
    // SAFETY: a buffer of the given length
    let n = unsafe {
      libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len())
    };
    if n > 0 {
      bytes = &bytes[n as usize..];
    } else if n < 0 && errno() == libc::EINTR {
      continue;
    } else {
      return false;
    }
  }
  true
}

fn read_exact(fd: i32, mut buf: &mut [u8]) -> bool {
  while !buf.is_empty() {
    // SAFETY: a buffer of the given length
    let n = unsafe {
      libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
    };
    if n > 0 {
      buf = &mut buf[n as usize..];
    } else if n < 0 && errno() == libc::EINTR {
      continue;
    } else {
      return false;
    }
  }
  true
}

fn errno() -> i32 {
  io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Starts `prepared` detached from the helper (its own session, the null
/// device as stdin), with `url` in the argument slot kept for it. Only
/// system calls. Returns whether the fork worked.
fn spawn(prepared: &mut Prepared, url: Option<*const libc::c_char>) -> bool {
  if let Some(url) = url {
    let slot = prepared.argv.len() - 2;
    prepared.argv[slot] = url;
  }
  // SAFETY: fork; the child makes system calls on memory that exists
  // already and execs (or exits)
  unsafe {
    let pid = libc::fork();
    if pid < 0 {
      return false;
    }
    if pid == 0 {
      libc::setsid();
      let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
      if null >= 0 {
        libc::dup2(null, 0);
        if null > 2 {
          libc::close(null);
        }
      }
      if let Some(cwd) = &prepared.cwd
        && libc::chdir(cwd.as_ptr()) != 0
      {
        libc::_exit(126);
      }
      libc::execve(
        prepared.program.as_ptr(),
        prepared.argv.as_ptr(),
        prepared.envp.as_ptr(),
      );
      libc::_exit(127);
    }
  }
  true
}

/// The helper's loop: one request, one status byte, until the socket
/// closes (the app ended) or a frame is malformed. Never returns.
fn serve(
  socket: i32,
  mut opener: Option<Prepared>,
  mut fixed: Vec<Prepared>,
  max_fd: i32,
) -> ! {
  // SAFETY: plain system calls on the helper's only thread
  unsafe {
    for fd in 3..max_fd {
      if fd != socket {
        libc::close(fd);
      }
    }
    // children are reaped by the kernel; a closed socket is an error return
    libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    #[cfg(target_os = "linux")]
    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
  }
  let mut header = [0u8; 3];
  // one more byte than the longest URL: its terminator
  let mut payload = [0u8; MAX_URL + 1];
  loop {
    if !read_exact(socket, &mut header) {
      break;
    }
    let len = u16::from_le_bytes([header[1], header[2]]) as usize;
    if len > MAX_URL || !read_exact(socket, &mut payload[..len]) {
      break;
    }
    let status = match header[0] {
      OP_OPEN => match &mut opener {
        None => STATUS_UNSUPPORTED,
        Some(_) if !valid_external_url(&payload[..len]) => STATUS_REFUSED,
        Some(opener) => {
          payload[len] = 0;
          if spawn(opener, Some(payload.as_ptr() as *const libc::c_char)) {
            STATUS_OK
          } else {
            STATUS_FAILED
          }
        }
      },
      OP_FIXED if len == 1 => match fixed.get_mut(payload[0] as usize) {
        None => STATUS_UNSUPPORTED,
        Some(command) => {
          if spawn(command, None) {
            STATUS_OK
          } else {
            STATUS_FAILED
          }
        }
      },
      _ => break,
    };
    if !write_all(socket, &[status]) {
      break;
    }
  }
  // SAFETY: the helper ends here
  unsafe { libc::_exit(0) }
}

/// The process's environment, `NAME=value` each (for a [`FixedCommand`]),
/// with `overrides` applied (`None` removes a variable).
pub fn environment(overrides: &[(&str, Option<&str>)]) -> Vec<CString> {
  use std::os::unix::ffi::OsStrExt;
  let mut env: Vec<CString> = std::env::vars_os()
    .filter(|(name, _)| {
      !overrides
        .iter()
        .any(|(o, _)| name.as_bytes() == o.as_bytes())
    })
    .filter_map(|(name, value)| {
      let mut entry = name.as_bytes().to_vec();
      entry.push(b'=');
      entry.extend_from_slice(value.as_bytes());
      CString::new(entry).ok()
    })
    .collect();
  for (name, value) in overrides {
    if let Some(value) = value
      && let Ok(entry) = CString::new(format!("{name}={value}"))
    {
      env.push(entry);
    }
  }
  env
}

/// Forks the helper. Call right before the process confines itself; the
/// helper keeps what the process could do until then. The returned end is
/// close-on-exec: programs the app starts don't inherit it.
pub fn start(config: Config) -> io::Result<Outside> {
  let mut fds = [0i32; 2];
  // SAFETY: an array of two descriptors, as socketpair writes
  if unsafe {
    libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr())
  } != 0
  {
    return Err(io::Error::last_os_error());
  }
  let has_opener = config.opener.is_some();
  let fixed_count = config.fixed.len();
  if fixed_count > u8::MAX as usize {
    return Err(io::Error::other("too many fixed commands"));
  }
  // everything the helper needs exists before the fork
  let opener = config.opener.map(|program| {
    Prepared::new(
      FixedCommand {
        program,
        args: Vec::new(),
        cwd: None,
        env: environment(&[]),
      },
      true,
    )
  });
  let fixed: Vec<Prepared> = config
    .fixed
    .into_iter()
    .map(|command| Prepared::new(command, false))
    .collect();
  // SAFETY: a plain query
  let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
  let max_fd = if max_fd <= 0 {
    1024
  } else {
    max_fd.min(65_536)
  } as i32;
  // SAFETY: fork; the child runs `serve`, which only makes system calls on
  // what was built above
  let pid = unsafe { libc::fork() };
  if pid < 0 {
    let err = io::Error::last_os_error();
    // SAFETY: the two descriptors made above
    unsafe {
      libc::close(fds[0]);
      libc::close(fds[1]);
    }
    return Err(err);
  }
  if pid == 0 {
    serve(fds[1], opener, fixed, max_fd);
  }
  // SAFETY: the helper's end is the child's; ours becomes close-on-exec
  let socket = unsafe {
    libc::close(fds[1]);
    libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
    OwnedFd::from_raw_fd(fds[0])
  };
  Ok(Outside {
    socket: Mutex::new(socket),
    opener: has_opener,
    fixed: fixed_count,
    helper: pid,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_web_and_mail_urls_of_plain_ascii_are_external() {
    for ok in [
      "https://example.com/a?b=c&d=%20e#f",
      "http://127.0.0.1:8080/",
      "HTTPS://EXAMPLE.COM",
      "mailto:someone@example.com?subject=hi",
    ] {
      assert!(valid_external_url(ok.as_bytes()), "{ok}");
    }
    for bad in [
      "",
      "https://",
      "mailto:",
      "file:///etc/passwd",
      "/Applications/Calculator.app",
      "-a Terminal",
      "javascript:alert(1)",
      "x-apple.systempreferences:",
      "ssh://host",
      "https://example.com/a b",
      "https://example.com/\n-a",
      "https://example.com/\0",
      "https://example.com/\"",
      "https://example.com/`id`",
      "https://example.com/{a|b}",
      "https://exämple.com/",
      " https://example.com/",
    ] {
      assert!(!valid_external_url(bad.as_bytes()), "{bad:?}");
    }
    let long = format!("https://example.com/{}", "a".repeat(MAX_URL));
    assert!(!valid_external_url(long.as_bytes()));
  }

  // the helper runs what it was configured with: the fixed command, the
  // opener with a URL it accepts, and nothing a request could name
  #[test]
  fn the_helper_runs_only_what_it_was_given() {
    let dir = std::env::temp_dir()
      .join(format!("deno_outside_helper_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let c = |s: &str| CString::new(s).unwrap();
    let fixed_out = dir.join("fixed");
    let outside = start(Config {
      // `echo <url>` stands in for the opener
      opener: Some(c("/bin/echo")),
      fixed: vec![FixedCommand {
        program: c("/bin/sh"),
        args: vec![
          c("-c"),
          c(&format!("echo \"$MARK\" > '{}'", fixed_out.display())),
        ],
        cwd: Some(c(&dir.to_string_lossy())),
        env: environment(&[("MARK", Some("fixed ran"))]),
      }],
    })
    .unwrap();
    assert!(outside.opens_urls());
    outside.open_url("https://example.com/").unwrap();
    assert_eq!(
      outside.open_url("file:///etc/passwd").unwrap_err().kind(),
      io::ErrorKind::InvalidInput
    );
    // a URL the helper itself refuses (sent past this side's check)
    assert_eq!(
      outside
        .request(OP_OPEN, b"file:///etc/passwd")
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidInput
    );
    assert_eq!(
      outside.run_fixed(1).unwrap_err().kind(),
      io::ErrorKind::Unsupported
    );
    assert_eq!(
      outside.request(OP_FIXED, &[9]).unwrap_err().kind(),
      io::ErrorKind::Unsupported
    );
    outside.run_fixed(0).unwrap();
    let mut text = String::new();
    for _ in 0..200 {
      text = std::fs::read_to_string(&fixed_out).unwrap_or_default();
      if !text.is_empty() {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(text.trim(), "fixed ran");
    // an unknown operation ends the helper: nothing answers after it
    assert!(outside.request(77, b"x").is_err());
    assert!(outside.open_url("https://example.com/").is_err());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
