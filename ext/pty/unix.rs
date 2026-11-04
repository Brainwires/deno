// Copyright 2018-2026 the Deno authors. MIT license.

//! The Unix pseudo-terminal: a `/dev/ptmx` master and its slave, both
//! opened close-on-exec, the child started by `std::process::Command` with
//! the slave as its stdio and, in `pre_exec`, a new session that takes the
//! slave as its controlling terminal. The master is non-blocking and read
//! and written through tokio's `AsyncFd`; the child is reaped by tokio's
//! process reaper (no thread of our own).

use std::ffi::CStr;
use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::pin::Pin;
use std::process::Stdio;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;

use deno_process::PreparedRun;
use deno_process::ProcessError;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::ReadBuf;
use tokio::io::unix::AsyncFd;

use crate::Exit;
use crate::PtyError;

/// The master, shared by the two halves and the resizer.
type Master = Rc<AsyncFd<OwnedFd>>;

pub struct Pty {
  master: Master,
}

pub struct ReadHalf(Master);
pub struct WriteHalf(Master);

/// The child, reaped by tokio.
pub struct Waiter(tokio::process::Child);

/// Signals the child by pid; only while it has not been reaped (the pid may
/// name another process afterwards), which `lib.rs` guarantees.
pub struct Killer {
  pid: u32,
}

pub struct Spawned {
  pub pid: u32,
  pub pty: Pty,
  pub reader: ReadHalf,
  pub writer: WriteHalf,
  pub waiter: Waiter,
  pub killer: Killer,
}

fn last_os_error() -> io::Error {
  io::Error::last_os_error()
}

pub fn spawn(
  run: PreparedRun,
  args: &[String],
  cols: u16,
  rows: u16,
) -> Result<Spawned, PtyError> {
  let (master, slave) = open_pty(cols, rows)?;

  let mut command = std::process::Command::new(&run.program);
  command.arg0(&run.argv0);
  command.args(args);
  command.env_clear();
  command.envs(run.envs.iter().map(|(k, v)| (k, v)));
  command.current_dir(&run.cwd);
  // `std` dup2s these onto 0-2 in the child before `pre_exec` runs; the
  // copies (and `slave` itself) are close-on-exec, so the child keeps only
  // its stdio.
  command.stdin(Stdio::from(slave.try_clone()?));
  command.stdout(Stdio::from(slave.try_clone()?));
  command.stderr(Stdio::from(slave.try_clone()?));
  // SAFETY: only async-signal-safe calls, between fork and exec: a new
  // session, then the slave (already fd 0) as its controlling terminal.
  unsafe {
    command.pre_exec(|| {
      if libc::setsid() < 0 {
        return Err(last_os_error());
      }
      if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
        return Err(last_os_error());
      }
      Ok(())
    });
  }

  let mut child = match tokio::process::Command::from(command).spawn() {
    Ok(child) => child,
    Err(err) => return Err(spawn_error(&run, err).into()),
  };
  drop(slave);
  let pid = child.id().expect("a freshly spawned child has a pid");
  run.verify_after_spawn(|| {
    let _ = child.start_kill();
  })?;

  // SAFETY: the descriptor is owned by the `AsyncFd` for its whole life and
  // closed by nothing else.
  let master =
    unsafe { AsyncFd::register(master) }.map_err(|err| err.into_parts().1)?;
  let master = Rc::new(master);
  Ok(Spawned {
    pid,
    pty: Pty {
      master: master.clone(),
    },
    reader: ReadHalf(master.clone()),
    writer: WriteHalf(master),
    waiter: Waiter(child),
    killer: Killer { pid },
  })
}

/// `Deno.Command`'s spawn error: the command as the app named it, and a
/// missing working directory called out.
fn spawn_error(run: &PreparedRun, err: io::Error) -> ProcessError {
  let command = run.argv0.to_string_lossy().into_owned();
  // launching a sub process always depends on the real file system
  #[allow(clippy::disallowed_methods, reason = "requires real fs")]
  if !run.cwd.exists() {
    return ProcessError::Io(io::Error::new(
      io::ErrorKind::NotFound,
      format!(
        "Failed to spawn '{}': No such cwd '{}'",
        command,
        run.cwd.to_string_lossy()
      ),
    ));
  }
  ProcessError::SpawnFailed {
    command,
    error: Box::new(ProcessError::Io(err)),
  }
}

/// Opens a master / slave pair, both close-on-exec from the moment they
/// exist (no window for another thread's fork to inherit them), the slave
/// set up with the attributes `node-pty` gives a new terminal and the
/// requested window size.
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
  // SAFETY: libc calls on descriptors this function owns.
  unsafe {
    let fd = libc::open(
      c"/dev/ptmx".as_ptr(),
      libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
    );
    if fd < 0 {
      return Err(last_os_error());
    }
    let master = OwnedFd::from_raw_fd(fd);
    if libc::grantpt(master.as_raw_fd()) != 0 {
      return Err(last_os_error());
    }
    if libc::unlockpt(master.as_raw_fd()) != 0 {
      return Err(last_os_error());
    }
    let name = slave_name(&master)?;
    let fd = libc::open(
      name.as_ptr(),
      libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
    );
    if fd < 0 {
      return Err(last_os_error());
    }
    let slave = OwnedFd::from_raw_fd(fd);

    let term = default_termios();
    if libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &term) != 0 {
      return Err(last_os_error());
    }
    set_winsize(slave.as_raw_fd(), cols, rows)?;

    let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
    if flags < 0
      || libc::fcntl(
        master.as_raw_fd(),
        libc::F_SETFL,
        flags | libc::O_NONBLOCK,
      ) < 0
    {
      return Err(last_os_error());
    }
    Ok((master, slave))
  }
}

/// The slave's path, from the master.
fn slave_name(master: &OwnedFd) -> io::Result<CString> {
  let mut buf = [0 as libc::c_char; 128];
  #[cfg(any(target_os = "linux", target_os = "android"))]
  {
    // SAFETY: the buffer is as long as stated.
    let rc = unsafe {
      libc::ptsname_r(master.as_raw_fd(), buf.as_mut_ptr(), buf.len())
    };
    if rc != 0 {
      return Err(io::Error::from_raw_os_error(rc));
    }
  }
  #[cfg(target_vendor = "apple")]
  {
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes.
    if unsafe {
      libc::ioctl(
        master.as_raw_fd(),
        libc::TIOCPTYGNAME as _,
        buf.as_mut_ptr(),
      )
    } < 0
    {
      return Err(last_os_error());
    }
  }
  #[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  )))]
  {
    // SAFETY: `ptsname` returns a static buffer, copied before any other
    // call on this thread.
    let name = unsafe { libc::ptsname(master.as_raw_fd()) };
    if name.is_null() {
      return Err(last_os_error());
    }
    // SAFETY: a NUL-terminated string from libc.
    let name = unsafe { CStr::from_ptr(name) };
    let bytes = name.to_bytes();
    if bytes.len() >= buf.len() {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "pty name"));
    }
    for (dst, src) in buf.iter_mut().zip(bytes) {
      *dst = *src as libc::c_char;
    }
  }
  // SAFETY: NUL-terminated by the zeroed buffer.
  Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

/// The attributes `node-pty` gives a new terminal (its `pty_openpty`):
/// canonical mode, echo, signals, UTF-8, 38400 baud.
fn default_termios() -> libc::termios {
  // SAFETY: a plain C struct, every field set below or left zero.
  let mut term: libc::termios = unsafe { std::mem::zeroed() };
  term.c_iflag = libc::ICRNL
    | libc::IXON
    | libc::IXANY
    | libc::IMAXBEL
    | libc::BRKINT
    | libc::IUTF8;
  term.c_oflag = libc::OPOST | libc::ONLCR;
  term.c_cflag = libc::CREAD | libc::CS8 | libc::HUPCL;
  term.c_lflag = libc::ICANON
    | libc::ISIG
    | libc::IEXTEN
    | libc::ECHO
    | libc::ECHOE
    | libc::ECHOK
    | libc::ECHOKE
    | libc::ECHOCTL;
  const DISABLED: libc::cc_t = libc::cc_t::MAX;
  term.c_cc[libc::VEOF] = 4;
  term.c_cc[libc::VEOL] = DISABLED;
  term.c_cc[libc::VEOL2] = DISABLED;
  term.c_cc[libc::VERASE] = 0x7f;
  term.c_cc[libc::VWERASE] = 23;
  term.c_cc[libc::VKILL] = 21;
  term.c_cc[libc::VREPRINT] = 18;
  term.c_cc[libc::VINTR] = 3;
  term.c_cc[libc::VQUIT] = 0x1c;
  term.c_cc[libc::VSUSP] = 26;
  term.c_cc[libc::VSTART] = 17;
  term.c_cc[libc::VSTOP] = 19;
  term.c_cc[libc::VLNEXT] = 22;
  term.c_cc[libc::VDISCARD] = 15;
  term.c_cc[libc::VMIN] = 1;
  term.c_cc[libc::VTIME] = 0;
  #[cfg(target_vendor = "apple")]
  {
    term.c_cc[libc::VDSUSP] = 25;
    term.c_cc[libc::VSTATUS] = 20;
  }
  // SAFETY: `term` is a valid termios.
  unsafe {
    libc::cfsetispeed(&mut term, libc::B38400);
    libc::cfsetospeed(&mut term, libc::B38400);
  }
  term
}

fn set_winsize(fd: libc::c_int, cols: u16, rows: u16) -> io::Result<()> {
  let size = libc::winsize {
    ws_row: rows,
    ws_col: cols,
    ws_xpixel: 0,
    ws_ypixel: 0,
  };
  // SAFETY: TIOCSWINSZ reads a winsize.
  if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) } < 0 {
    return Err(last_os_error());
  }
  Ok(())
}

impl Pty {
  pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
    set_winsize(self.master.as_raw_fd(), cols, rows)
  }

  /// The foreground process group's leader, by name: `tcgetpgrp` on the
  /// master, then the process's own name from the kernel (Linux: `/proc`;
  /// macOS: `proc_name`).
  pub fn foreground_process(&self) -> Option<String> {
    // SAFETY: tcgetpgrp on an open terminal.
    let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
    if pgrp <= 0 {
      return None;
    }
    process_name(pgrp)
  }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_name(pid: libc::pid_t) -> Option<String> {
  use std::io::Read;
  // the process's own name is only in /proc
  #[allow(clippy::disallowed_methods, reason = "reads procfs")]
  let mut file = std::fs::File::open(format!("/proc/{pid}/comm")).ok()?;
  let mut name = String::new();
  file.read_to_string(&mut name).ok()?;
  let name = name.trim();
  (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_vendor = "apple")]
fn process_name(pid: libc::pid_t) -> Option<String> {
  let mut buf = [0u8; 256];
  // SAFETY: libproc writes at most `buf.len()` bytes.
  let len = unsafe {
    libc::proc_name(
      pid,
      buf.as_mut_ptr() as *mut libc::c_void,
      buf.len() as u32,
    )
  };
  if len <= 0 {
    return None;
  }
  Some(String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

#[cfg(not(any(
  target_os = "linux",
  target_os = "android",
  target_vendor = "apple"
)))]
fn process_name(_pid: libc::pid_t) -> Option<String> {
  None
}

/// A read of the master. On Linux, once the last slave descriptor closes,
/// reads fail with `EIO`: that is the terminal's end-of-file.
fn read_master(fd: libc::c_int, buf: &mut [u8]) -> io::Result<usize> {
  // SAFETY: reads at most `buf.len()` bytes into `buf`.
  let n =
    unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
  if n < 0 {
    let err = last_os_error();
    if err.raw_os_error() == Some(libc::EIO) {
      return Ok(0);
    }
    return Err(err);
  }
  Ok(n as usize)
}

fn write_master(fd: libc::c_int, buf: &[u8]) -> io::Result<usize> {
  // SAFETY: writes at most `buf.len()` bytes from `buf`.
  let n =
    unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
  if n < 0 {
    return Err(last_os_error());
  }
  Ok(n as usize)
}

impl AsyncRead for ReadHalf {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    loop {
      let mut guard = std::task::ready!(self.0.poll_read_ready(cx))?;
      let unfilled = buf.initialize_unfilled();
      match guard
        .try_io(|inner| read_master(inner.get_ref().as_raw_fd(), unfilled))
      {
        Ok(Ok(n)) => {
          buf.advance(n);
          return Poll::Ready(Ok(()));
        }
        Ok(Err(err)) => return Poll::Ready(Err(err)),
        Err(_would_block) => continue,
      }
    }
  }
}

impl AsyncWrite for WriteHalf {
  fn poll_write(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<io::Result<usize>> {
    loop {
      let mut guard = std::task::ready!(self.0.poll_write_ready(cx))?;
      match guard.try_io(|inner| write_master(inner.get_ref().as_raw_fd(), buf))
      {
        Ok(result) => return Poll::Ready(result),
        Err(_would_block) => continue,
      }
    }
  }

  fn poll_flush(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
  ) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }

  fn poll_shutdown(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
  ) -> Poll<io::Result<()>> {
    // A terminal has no half-close; the master closes with the resource.
    Poll::Ready(Ok(()))
  }
}

impl Waiter {
  pub async fn wait(&mut self) -> io::Result<Exit> {
    let status = self.0.wait().await?;
    Ok(Exit {
      code: status.code(),
      signal: status.signal(),
    })
  }
}

impl Killer {
  /// Sends `signal` to the child (0 only checks that it exists).
  pub fn kill(&self, signal: i32) -> Result<(), PtyError> {
    // SAFETY: kill on the child's pid, which lib.rs has not reaped yet.
    if unsafe { libc::kill(self.pid as libc::pid_t, signal) } < 0 {
      return Err(last_os_error().into());
    }
    Ok(())
  }

  /// The runtime is ending with the child running: end it, as
  /// `Deno.Command` does.
  pub fn kill_on_drop(&self) {
    // SAFETY: best-effort kill of the child's pid; an error (it exited in
    // the meantime) is ignored.
    unsafe {
      libc::kill(self.pid as libc::pid_t, libc::SIGKILL);
    }
  }
}
