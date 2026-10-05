// Copyright 2018-2026 the Deno authors. MIT license.

//! When a desktop app's executable runs as a headless worker.
//!
//! A framework dev server (Next.js) forks child processes with
//! `child_process.fork()`; in a desktop app that re-executes the app's own
//! executable as `<exe> run [flags…] <script> …`, with a Node IPC channel
//! (`NODE_CHANNEL_FD`), and the runtime runs the script headless (no
//! window) instead of starting the app. Anyone could start the executable
//! that way, though: a shell, `open --args`, a shortcut, another program.
//! The worker then ran the script with the app's permissions, its code
//! signature, and on macOS its privacy (TCC) grants.
//!
//! Which launches are worker launches ([`is_worker_launch`]) is the laufey
//! host's own decision, mirrored: the host runs a launch it classifies as a
//! headless worker with no web engine, no window and no single-instance
//! check, so the runtime must take its worker path for every such launch
//! (and never start the app with no backend). That covers the `run <script>`
//! command, a compiled binary's `fork()` (`<exe> <module>` with the module in
//! `DENO_INTERNAL_CHILD_ENTRYPOINT`), and
//! `spawn(process.execPath, [script], { stdio: [..., "ipc"] })` (`<exe>
//! <script>` with `NODE_CHANNEL_FD` and nothing else to tell it apart).
//!
//! A worker launch is admitted only when ALL of these hold ([`authorize`]):
//!
//! * [`WORKER_TOKEN_ENV`] holds a token naming the parent process
//!   (`v1.<parent pid>.<128 random bits>`): the runtime issues it once per
//!   launch ([`issue_token`]) and hands it only to children it forks of its
//!   own executable with an IPC channel
//!   (`deno_process::set_self_fork_env_var`), never to other programs;
//! * the pid in the token is this process's parent, and the parent runs the
//!   same executable file;
//! * `NODE_CHANNEL_FD` names an inherited IPC endpoint (a Unix-domain socket
//!   on macOS / Linux, a pipe handle on Windows), which only a parent that
//!   set up a fork passes.
//!
//! Anything else shaped like a worker launch is refused and the process
//! exits without starting the app.
//!
//! And in a packaged app, a forked script must be one the app ships: a path
//! inside the embedded file system's root ([`module_allowed`]), not an
//! arbitrary file on disk or a UNC share. Only a development run (`deno
//! desktop --hmr`, which runs the framework's dev server from the source
//! directory) forks scripts from disk.
//!
//! What this is not: a boundary against native code already running as the
//! same user, which can start a process with the app as its parent
//! (re-executing a pre-forked process on Unix, `PROC_THREAD_ATTRIBUTE_
//! PARENT_PROCESS` on Windows) and read a live child's environment. Such
//! code can only fork scripts the packaged app itself contains, which is what
//! starting the app normally lets it run anyway.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;

use deno_core::url::Url;

/// The environment variable carrying the worker token from a desktop
/// runtime to the workers it forks.
pub const WORKER_TOKEN_ENV: &str = "DENO_DESKTOP_WORKER_TOKEN";

/// Whether a launch is a worker launch: `args` is argv without the program,
/// `env` reads this process's environment.
///
/// It mirrors laufey's `laufey_common::IsHeadlessWorkerLaunch`
/// (backend-common/src/launch_args.cc), which every host checks before it
/// loads a web engine or takes the single-instance lock: `IsCliWorkerCommand`
/// (`args[0]` is `run` and a non-flag argument follows) or
/// `IsForkedWorkerEnvironment` (`NODE_CHANNEL_FD` or `NEXT_PRIVATE_WORKER` is
/// set; the Windows hosts skip an empty one, this counts it, which only
/// widens the worker path). A launch the host runs headless therefore never
/// reaches the app's startup here with no backend: it is admitted or refused
/// by [`authorize`]. Keep the two in step.
///
/// Two runtime-only shapes are worker launches too, though the host gives
/// them a backend; they are refused unless [`authorize`] admits them, never
/// started as the app: `run` with no script, and
/// `DENO_INTERNAL_CHILD_ENTRYPOINT` (a compiled binary's `fork()` names its
/// module there; the app started with it set would run that module as its
/// main module).
pub fn is_worker_launch(
  args: &[OsString],
  env: impl Fn(&str) -> Option<OsString>,
) -> bool {
  let run = args.first().is_some_and(|a| a == OsStr::new("run"));
  let host_cli_worker = run
    && args[1..]
      .iter()
      .any(|a| !a.as_encoded_bytes().starts_with(b"-"));
  let host_forked_worker =
    env("NODE_CHANNEL_FD").is_some() || env("NEXT_PRIVATE_WORKER").is_some();
  let fork_child_entrypoint =
    env(denort::run::INTERNAL_CHILD_ENTRYPOINT_ENV_VAR)
      .is_some_and(|v| !v.is_empty());
  host_cli_worker || host_forked_worker || run || fork_child_entrypoint
}

/// Why a worker launch was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
  NoToken,
  MalformedToken,
  /// The token names a process that is not our parent.
  NotTheParent {
    token_pid: u32,
    parent_pid: Option<u32>,
  },
  /// The parent runs another executable.
  ParentIsAnotherExecutable,
  NoIpcChannel,
  NotAnIpcChannel,
}

impl std::fmt::Display for Refusal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Refusal::NoToken => write!(f, "no {WORKER_TOKEN_ENV}"),
      Refusal::MalformedToken => write!(f, "malformed {WORKER_TOKEN_ENV}"),
      Refusal::NotTheParent {
        token_pid,
        parent_pid,
      } => write!(
        f,
        "the worker token names process {token_pid}, the parent is {}",
        parent_pid.map_or_else(|| "unknown".to_string(), |p| p.to_string())
      ),
      Refusal::ParentIsAnotherExecutable => {
        write!(f, "the parent process runs another executable")
      }
      Refusal::NoIpcChannel => write!(f, "no NODE_CHANNEL_FD"),
      Refusal::NotAnIpcChannel => {
        write!(f, "NODE_CHANNEL_FD is not an inherited IPC channel")
      }
    }
  }
}

/// A fresh token for this process's forks: `v1.<our pid>.<32 hex digits>`.
pub fn issue_token() -> String {
  let mut nonce = [0u8; 16];
  fill_random(&mut nonce);
  let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
  format!("v1.{}.{hex}", std::process::id())
}

fn fill_random(buf: &mut [u8]) {
  let provider = rustls::crypto::aws_lc_rs::default_provider();
  if provider.secure_random.fill(buf).is_err() {
    // Never reached in practice (AWS-LC's RNG does not fail); a token with
    // a predictable nonce still names the parent, which is the check that
    // matters.
    let t = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_nanos())
      .unwrap_or_default();
    for (i, b) in buf.iter_mut().enumerate() {
      *b = (t >> ((i % 16) * 8)) as u8;
    }
  }
}

/// The pid a well-formed token names, or `None`.
pub fn parse_token(token: &str) -> Option<u32> {
  let rest = token.strip_prefix("v1.")?;
  let (pid, nonce) = rest.split_once('.')?;
  if pid.is_empty()
    || !pid.bytes().all(|b| b.is_ascii_digit())
    || nonce.len() != 32
    || !nonce
      .bytes()
      .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
  {
    return None;
  }
  pid.parse().ok()
}

/// The checks of the module docs, with the OS queries injected (see
/// [`authorize`]).
pub fn authorize_with(
  token: Option<&str>,
  channel_fd: Option<&str>,
  parent_pid: Option<u32>,
  parent_runs_this_executable: impl FnOnce(u32) -> bool,
  is_inherited_ipc_channel: impl FnOnce(&str) -> bool,
) -> Result<(), Refusal> {
  let token = token.ok_or(Refusal::NoToken)?;
  let token_pid = parse_token(token).ok_or(Refusal::MalformedToken)?;
  if parent_pid != Some(token_pid) {
    return Err(Refusal::NotTheParent {
      token_pid,
      parent_pid,
    });
  }
  if !parent_runs_this_executable(token_pid) {
    return Err(Refusal::ParentIsAnotherExecutable);
  }
  let fd = channel_fd.ok_or(Refusal::NoIpcChannel)?;
  if !is_inherited_ipc_channel(fd) {
    return Err(Refusal::NotAnIpcChannel);
  }
  Ok(())
}

/// Whether this process may run as a headless worker (see the module docs).
pub fn authorize() -> Result<(), Refusal> {
  let token = std::env::var(WORKER_TOKEN_ENV).ok();
  let fd = std::env::var("NODE_CHANNEL_FD").ok();
  authorize_with(
    token.as_deref(),
    fd.as_deref(),
    parent_pid(),
    parent_runs_this_executable,
    is_inherited_ipc_channel,
  )
}

/// Whether a forked module may run: in a development run any file, else
/// only a file under the embedded file system's root (`vfs_root`). The path
/// is normalized first, so `..` can't climb out of the root.
pub fn module_allowed(module: &Url, vfs_root: &Path, dev: bool) -> bool {
  if module.scheme() != "file" {
    return false;
  }
  if dev {
    return true;
  }
  let Ok(path) = module.to_file_path() else {
    return false;
  };
  path_starts_with(&normalize(&path), &normalize(vfs_root))
}

/// `path` with `.` dropped and `..` applied lexically (never above the
/// root).
fn normalize(path: &Path) -> std::path::PathBuf {
  use std::path::Component;
  let mut out = std::path::PathBuf::new();
  for c in path.components() {
    match c {
      Component::CurDir => {}
      Component::ParentDir => {
        out.pop();
      }
      c => out.push(c.as_os_str()),
    }
  }
  out
}

/// `path` is `root` or below it; on Windows compared case-insensitively.
fn path_starts_with(path: &Path, root: &Path) -> bool {
  #[cfg(windows)]
  {
    let lower =
      |p: &Path| std::path::PathBuf::from(p.to_string_lossy().to_lowercase());
    lower(path).starts_with(lower(root))
  }
  #[cfg(not(windows))]
  {
    path.starts_with(root)
  }
}

// --- OS queries -------------------------------------------------------------

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
  // SAFETY: getppid has no preconditions.
  let ppid = unsafe { libc::getppid() };
  // Reparented to init / launchd: the process that forked us is gone.
  (ppid > 1).then_some(ppid as u32)
}

#[cfg(windows)]
fn parent_pid() -> Option<u32> {
  windows_proc::parent_pid()
}

/// Whether process `pid` runs the same executable file as this process.
fn parent_runs_this_executable(pid: u32) -> bool {
  let Some(parent) = executable_of(pid) else {
    return false;
  };
  let Ok(own) = std::env::current_exe() else {
    return false;
  };
  same_file(&parent, &own)
}

#[allow(
  clippy::disallowed_methods,
  reason = "compares two executables on the real file system, before any runtime sys exists"
)]
fn same_file(a: &Path, b: &Path) -> bool {
  match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
    (Ok(a), Ok(b)) => {
      #[cfg(windows)]
      {
        a.to_string_lossy()
          .eq_ignore_ascii_case(&b.to_string_lossy())
      }
      #[cfg(not(windows))]
      {
        a == b
      }
    }
    _ => false,
  }
}

#[cfg(target_os = "macos")]
fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
  use std::os::unix::ffi::OsStrExt;
  let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
  // SAFETY: the buffer is valid for its length.
  let n = unsafe {
    libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32)
  };
  if n <= 0 {
    return None;
  }
  buf.truncate(n as usize);
  Some(std::ffi::OsStr::from_bytes(&buf).into())
}

#[cfg(all(unix, not(target_os = "macos")))]
#[allow(
  clippy::disallowed_methods,
  reason = "reads /proc before any runtime sys exists"
)]
fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
  std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(windows)]
fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
  windows_proc::executable_of(pid)
}

/// Whether `fd` (the `NODE_CHANNEL_FD` value) is an open Unix-domain socket
/// this process inherited, which is what `child_process.fork()` passes.
#[cfg(unix)]
fn is_inherited_ipc_channel(fd: &str) -> bool {
  let Ok(fd) = fd.parse::<libc::c_int>() else {
    return false;
  };
  if fd < 0 {
    return false;
  }
  // SAFETY: `stat` is plain data; all-zero is a valid value.
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: `st` is a valid out-pointer; an invalid fd fails with EBADF.
  let stat_failed = unsafe { libc::fstat(fd, &mut st) } != 0;
  if stat_failed || (st.st_mode & libc::S_IFMT) != libc::S_IFSOCK {
    return false;
  }
  // SAFETY: `sockaddr_storage` is plain data; all-zero is a valid value.
  let mut addr: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
  let mut len =
    std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
  // SAFETY: `addr` holds `len` bytes; getsockname writes at most that.
  let name_failed = unsafe {
    libc::getsockname(
      fd,
      (&mut addr as *mut libc::sockaddr_storage).cast(),
      &mut len,
    )
  } != 0;
  if name_failed {
    return false;
  }
  addr.ss_family as libc::c_int == libc::AF_UNIX
}

/// Whether `handle` (the `NODE_CHANNEL_FD` value) is an inherited pipe
/// handle, which is what `child_process.fork()` passes on Windows.
#[cfg(windows)]
fn is_inherited_ipc_channel(handle: &str) -> bool {
  windows_proc::is_pipe_handle(handle)
}

#[cfg(windows)]
mod windows_proc {
  use std::os::windows::ffi::OsStringExt;

  use windows_sys::Win32::Foundation::CloseHandle;
  use windows_sys::Win32::Foundation::FILETIME;
  use windows_sys::Win32::Foundation::HANDLE;
  use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
  use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_PIPE;
  use windows_sys::Win32::Storage::FileSystem::GetFileType;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32NextW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
  use windows_sys::Win32::System::Threading::GetCurrentProcess;
  use windows_sys::Win32::System::Threading::GetProcessTimes;
  use windows_sys::Win32::System::Threading::OpenProcess;
  use windows_sys::Win32::System::Threading::PROCESS_NAME_WIN32;
  use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
  use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

  /// The parent recorded at our creation, if it is still the process that
  /// created us: a pid can be reused once its process exits, and a process
  /// created after us can't be our creator.
  pub fn parent_pid() -> Option<u32> {
    let own = std::process::id();
    // SAFETY: a process snapshot; the handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
      return None;
    }
    // SAFETY: PROCESSENTRY32W is plain data; dwSize is set as required.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut parent = None;
    // SAFETY: `snap` is a valid snapshot and `entry` is initialized.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
    while ok != 0 {
      if entry.th32ProcessID == own {
        parent = Some(entry.th32ParentProcessID);
        break;
      }
      // SAFETY: as above.
      ok = unsafe { Process32NextW(snap, &mut entry) };
    }
    // SAFETY: the snapshot handle is owned here.
    unsafe { CloseHandle(snap) };
    let parent = parent.filter(|p| *p != 0)?;
    let parent_created = with_process(parent, created_at)?;
    // SAFETY: the pseudo handle of this process needs no closing.
    let own_created = created_at(unsafe { GetCurrentProcess() })?;
    (parent_created <= own_created).then_some(parent)
  }

  pub fn executable_of(pid: u32) -> Option<std::path::PathBuf> {
    with_process(pid, |h| {
      let mut buf = vec![0u16; 32 * 1024];
      let mut len = buf.len() as u32;
      // SAFETY: `buf` holds `len` UTF-16 units.
      let ok = unsafe {
        QueryFullProcessImageNameW(
          h,
          PROCESS_NAME_WIN32,
          buf.as_mut_ptr(),
          &mut len,
        )
      };
      if ok == 0 {
        return None;
      }
      buf.truncate(len as usize);
      Some(std::ffi::OsString::from_wide(&buf).into())
    })
  }

  fn with_process<T>(
    pid: u32,
    f: impl FnOnce(HANDLE) -> Option<T>,
  ) -> Option<T> {
    // SAFETY: a query-only handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
      return None;
    }
    let out = f(h);
    // SAFETY: `h` is owned here.
    unsafe { CloseHandle(h) };
    out
  }

  fn created_at(h: HANDLE) -> Option<u64> {
    let zero = FILETIME {
      dwLowDateTime: 0,
      dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) =
      (zero, zero, zero, zero);
    // SAFETY: four valid FILETIME out-pointers.
    let ok = unsafe {
      GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user)
    };
    if ok == 0 {
      return None;
    }
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
  }

  pub fn is_pipe_handle(handle: &str) -> bool {
    let Ok(raw) = handle.parse::<i64>() else {
      return false;
    };
    if raw <= 0 {
      return false;
    }
    // SAFETY: GetFileType only inspects the handle; an invalid one answers
    // FILE_TYPE_UNKNOWN.
    unsafe { GetFileType(raw as usize as HANDLE) == FILE_TYPE_PIPE }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const NONCE: &str = "0123456789abcdef0123456789abcdef";

  #[test]
  fn issued_tokens_name_this_process_and_differ() {
    let a = issue_token();
    let b = issue_token();
    assert_ne!(a, b);
    assert_eq!(parse_token(&a), Some(std::process::id()));
    assert_eq!(parse_token(&b), Some(std::process::id()));
  }

  #[test]
  fn malformed_tokens_are_refused() {
    for t in [
      "",
      "1",
      "v1",
      "v1.",
      "v1.123",
      "v1.123.",
      "v2.123.0123456789abcdef0123456789abcdef",
      "v1..0123456789abcdef0123456789abcdef",
      "v1.-1.0123456789abcdef0123456789abcdef",
      "v1.+1.0123456789abcdef0123456789abcdef",
      "v1.12a.0123456789abcdef0123456789abcdef",
      "v1.123.0123456789ABCDEF0123456789ABCDEF",
      "v1.123.0123456789abcdef0123456789abcde",
      "v1.123.0123456789abcdef0123456789abcdef0",
      "v1.123.0123456789abcdef0123456789abcdeg",
      "v1.99999999999.0123456789abcdef0123456789abcdef",
      " v1.123.0123456789abcdef0123456789abcdef",
      "v1.123.0123456789abcdef0123456789abcdé",
    ] {
      assert_eq!(parse_token(t), None, "{t:?}");
    }
    assert_eq!(parse_token(&format!("v1.123.{NONCE}")), Some(123));
  }

  #[test]
  fn argv_alone_is_never_a_worker() {
    let ok = |_: u32| true;
    let ok_fd = |_: &str| true;
    // No token: an `<App> run /tmp/x.js` from a shell, `open --args`, …
    assert_eq!(
      authorize_with(None, Some("3"), Some(10), ok, ok_fd),
      Err(Refusal::NoToken)
    );
    assert_eq!(
      authorize_with(Some("1"), Some("3"), Some(10), ok, ok_fd),
      Err(Refusal::MalformedToken)
    );
    // A token for another parent (copied from another process's
    // environment, or the parent exited and we were reparented).
    let t = format!("v1.10.{NONCE}");
    assert_eq!(
      authorize_with(Some(&t), Some("3"), Some(11), ok, ok_fd),
      Err(Refusal::NotTheParent {
        token_pid: 10,
        parent_pid: Some(11)
      })
    );
    assert_eq!(
      authorize_with(Some(&t), Some("3"), None, ok, ok_fd),
      Err(Refusal::NotTheParent {
        token_pid: 10,
        parent_pid: None
      })
    );
    // The right pid, but another program.
    assert_eq!(
      authorize_with(Some(&t), Some("3"), Some(10), |_| false, ok_fd),
      Err(Refusal::ParentIsAnotherExecutable)
    );
    // No IPC channel, or a value that is no channel.
    assert_eq!(
      authorize_with(Some(&t), None, Some(10), ok, ok_fd),
      Err(Refusal::NoIpcChannel)
    );
    assert_eq!(
      authorize_with(Some(&t), Some("0"), Some(10), ok, |_| false),
      Err(Refusal::NotAnIpcChannel)
    );
    // Everything holds.
    assert_eq!(
      authorize_with(Some(&t), Some("3"), Some(10), ok, ok_fd),
      Ok(())
    );
  }

  /// How a launch with `argv` (without the program) and `env` starts, for a
  /// process whose parent is `PARENT` running this executable, and whose
  /// `NODE_CHANNEL_FD` (if any) is an inherited IPC channel: the decision
  /// the runtime makes at startup, with the OS queries answered.
  fn launch(argv: &[&str], env: &[(&str, &str)]) -> Result<bool, Refusal> {
    const PARENT: u32 = 10;
    let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let var = |name: &str| {
      env
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| OsString::from(v))
    };
    if !is_worker_launch(&args, var) {
      return Ok(false);
    }
    let get =
      |name: &str| env.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
    authorize_with(
      get(WORKER_TOKEN_ENV),
      get("NODE_CHANNEL_FD"),
      Some(PARENT),
      |pid| pid == PARENT,
      |_| true,
    )
    .map(|()| true)
  }

  #[test]
  fn every_launch_the_host_runs_headless_is_a_worker_launch() {
    let token = format!("v1.10.{NONCE}");
    let token = token.as_str();
    let entry = denort::run::INTERNAL_CHILD_ENTRYPOINT_ENV_VAR;
    // The app itself: no worker path at all.
    assert_eq!(launch(&[], &[]), Ok(false));
    assert_eq!(launch(&["--", "acme://open?x=1"], &[]), Ok(false));
    assert_eq!(launch(&["acme://run"], &[]), Ok(false));
    assert_eq!(launch(&["serve", "x.js"], &[]), Ok(false));
    // A worker token alone (inherited by a program the app started, which
    // started the app) does not make a launch a worker launch.
    assert_eq!(launch(&[], &[(WORKER_TOKEN_ENV, token)]), Ok(false));

    // `<exe> run <script>`: the update helper, `fork()` from `deno desktop`.
    let ipc = [(WORKER_TOKEN_ENV, token), ("NODE_CHANNEL_FD", "3")];
    assert_eq!(launch(&["run", "-A", "worker.js"], &ipc), Ok(true));
    assert_eq!(
      launch(&["run", "denext-update-helper", "apply", "42"], &[]),
      Err(Refusal::NoToken)
    );
    // A compiled binary's `fork()`.
    assert_eq!(
      launch(
        &["/app/fork_child.js"],
        &[
          (entry, "/app/fork_child.js"),
          (WORKER_TOKEN_ENV, token),
          ("NODE_CHANNEL_FD", "3")
        ]
      ),
      Ok(true)
    );
    // `spawn(process.execPath, [script], { stdio: [..., "ipc"] })`: argv is
    // `<exe> <script>`, only the IPC channel (and the token this runtime
    // gave the child) mark it. The host runs it headless.
    assert_eq!(launch(&["child.js"], &ipc), Ok(true));
    assert_eq!(launch(&[], &ipc), Ok(true));
    // The same without a token: not this app's runtime's child.
    assert_eq!(
      launch(&["child.js"], &[("NODE_CHANNEL_FD", "3")]),
      Err(Refusal::NoToken)
    );
    // An empty NODE_CHANNEL_FD still sends the host down its headless path.
    assert_eq!(
      launch(&["child.js"], &[("NODE_CHANNEL_FD", "")]),
      Err(Refusal::NoToken)
    );
    // A forged NEXT_PRIVATE_WORKER, with or without a stolen token.
    assert_eq!(
      launch(&[], &[("NEXT_PRIVATE_WORKER", "1")]),
      Err(Refusal::NoToken)
    );
    assert_eq!(
      launch(
        &["x.js"],
        &[("NEXT_PRIVATE_WORKER", "1"), (WORKER_TOKEN_ENV, token)]
      ),
      Err(Refusal::NoIpcChannel)
    );
    // The runtime-only shapes, which the host starts as the app: refused,
    // never run as the app.
    assert_eq!(launch(&["run"], &[]), Err(Refusal::NoToken));
    assert_eq!(launch(&["run", "--quiet"], &[]), Err(Refusal::NoToken));
    assert_eq!(launch(&["x.js"], &[(entry, "x.js")]), Err(Refusal::NoToken));
    assert_eq!(launch(&[], &[(entry, "")]), Ok(false));
  }

  #[test]
  fn worker_launches_mirror_the_host_classifier() {
    // laufey's own cases (backend-common/tests/launch_args_test.cc,
    // TestHeadlessWorkerLaunch): the host's headless launches are worker
    // launches here.
    let no_env = |_: &str| None;
    let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
    for headless in [
      &["run", "denext-update-helper", "apply", "42"][..],
      &["run", "-A", "--quiet", "worker.ts"],
      &["run", ""],
    ] {
      assert!(is_worker_launch(&args(headless), no_env), "{headless:?}");
    }
    for name in ["NODE_CHANNEL_FD", "NEXT_PRIVATE_WORKER"] {
      let env = |n: &str| (n == name).then(|| OsString::from("1"));
      assert!(is_worker_launch(&args(&["x.js"]), env), "{name}");
    }
    for app in [
      &[][..],
      &["serve", "worker.ts"],
      &["--", "acme://run"],
      &["--runtime", "/rt.so", "run", "x"],
    ] {
      assert!(!is_worker_launch(&args(app), no_env), "{app:?}");
    }
    let single =
      |n: &str| (n == "LAUFEY_SINGLE_INSTANCE").then(|| OsString::from("0"));
    assert!(!is_worker_launch(&args(&[]), single));
  }

  #[test]
  fn the_os_checks_refuse_this_test_process() {
    // The test harness was started by cargo, not by itself, and has no IPC
    // channel: every real check refuses.
    if let Some(ppid) = parent_pid() {
      assert!(!parent_runs_this_executable(ppid));
    }
    assert!(!parent_runs_this_executable(u32::MAX - 1));
    assert!(!is_inherited_ipc_channel("not a number"));
    assert!(!is_inherited_ipc_channel("-1"));
    assert!(!is_inherited_ipc_channel("987654"));
  }

  #[cfg(unix)]
  #[test]
  fn only_a_unix_socket_is_an_ipc_channel() {
    use std::os::fd::AsRawFd;
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(is_inherited_ipc_channel(&a.as_raw_fd().to_string()));
    let file = tempfile::tempfile().unwrap();
    assert!(!is_inherited_ipc_channel(&file.as_raw_fd().to_string()));
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(!is_inherited_ipc_channel(&tcp.as_raw_fd().to_string()));
  }

  #[test]
  fn a_child_of_this_executable_is_recognized() {
    // Re-run this test binary as a child that checks its parent.
    if std::env::var_os("DENO_RT_DESKTOP_WORKER_LAUNCH_CHILD").is_some() {
      let ppid = parent_pid().expect("a parent");
      assert!(parent_runs_this_executable(ppid));
      return;
    }
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
      .args([
        "--exact",
        "worker_launch::tests::a_child_of_this_executable_is_recognized",
        "--nocapture",
      ])
      .env("DENO_RT_DESKTOP_WORKER_LAUNCH_CHILD", "1")
      .status()
      .unwrap();
    assert!(status.success());
  }

  #[test]
  fn packaged_apps_fork_only_their_own_modules() {
    let root = if cfg!(windows) {
      Path::new("C:\\Users\\u\\AppData\\Local\\app\\root")
    } else {
      Path::new("/tmp/deno-compile-app/root")
    };
    let url = |p: &str| Url::from_file_path(p).unwrap();
    let inside = if cfg!(windows) {
      "C:\\Users\\u\\AppData\\Local\\app\\root\\node_modules\\next\\worker.js"
    } else {
      "/tmp/deno-compile-app/root/node_modules/next/worker.js"
    };
    assert!(module_allowed(&url(inside), root, false));
    let outside = [
      if cfg!(windows) {
        "C:\\Temp\\x.js"
      } else {
        "/tmp/x.js"
      },
      if cfg!(windows) {
        "C:\\Users\\u\\AppData\\Local\\app\\root\\..\\..\\x.js"
      } else {
        "/tmp/deno-compile-app/root/../../x.js"
      },
      if cfg!(windows) {
        "C:\\Users\\u\\AppData\\Local\\app\\rootx\\x.js"
      } else {
        "/tmp/deno-compile-app/rootx/x.js"
      },
    ];
    for p in outside {
      let u = Url::parse(&format!("file://{}", p.replace('\\', "/")))
        .unwrap_or_else(|_| url(p));
      assert!(!module_allowed(&u, root, false), "{p}");
      // A development run forks the dev server's scripts from disk.
      assert!(module_allowed(&u, root, true), "{p}");
    }
    #[cfg(windows)]
    {
      // A UNC share is never inside the root.
      let unc = Url::parse("file://server/share/x.js").unwrap();
      assert!(!module_allowed(&unc, root, false));
      // Case differences are the same path on Windows.
      let upper = url(&inside.to_uppercase());
      assert!(module_allowed(&upper, root, false));
    }
    // Not a file at all.
    let remote = Url::parse("https://example.com/x.js").unwrap();
    assert!(!module_allowed(&remote, root, true));
  }
}
