// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS confinement of a jailed lifecycle script on macOS: the script's
//! process tree runs under `sandbox-exec` (Seatbelt) with the deny-by-default
//! profile [`crate::seatbelt`] writes (DENEXT.md item 124).

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox and the hand-over act on the real file system and the process's own environment, before the runtime's virtual ones exist"
)]

use std::path::Path;
use std::sync::OnceLock;

pub use crate::seatbelt::profile;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Whether `sandbox-exec` works here (it is missing on some systems, and
/// refuses to run inside another sandbox).
#[allow(
  clippy::disallowed_methods,
  reason = "the real sandbox-exec binary, not a virtual file system"
)]
pub fn available() -> Result<(), String> {
  static AVAILABLE: OnceLock<Result<(), String>> = OnceLock::new();
  AVAILABLE
    .get_or_init(|| {
      if !Path::new(SANDBOX_EXEC).exists() {
        return Err(format!("{SANDBOX_EXEC} was not found"));
      }
      match std::process::Command::new(SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
      {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
          "{SANDBOX_EXEC} does not work here ({status}; is this already sandboxed?)"
        )),
        Err(err) => Err(format!("{SANDBOX_EXEC} failed to start: {err}")),
      }
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Enforcement (`--kernel-sandbox=enforce`, macOS Mode A): the re-executed
// image applies the profile with `sandbox_init`
// ---------------------------------------------------------------------------

use std::ffi::CString;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;

use crate::plan::EnforcePlan;
use crate::plan::MAX_PLAN_BYTES;
use crate::plan::PLAN_FD_ENV_VAR;

type SandboxInitFn = unsafe extern "C" fn(
  *const libc::c_char,
  u64,
  *mut *mut libc::c_char,
) -> libc::c_int;
type SandboxFreeErrorFn = unsafe extern "C" fn(*mut libc::c_char);
type SandboxCheckFn = unsafe extern "C" fn(
  libc::pid_t,
  *const libc::c_char,
  libc::c_int,
  ...
) -> libc::c_int;

/// `SANDBOX_FILTER_PATH` (sandbox.h).
const SANDBOX_FILTER_PATH: libc::c_int = 1;

fn symbol(name: &std::ffi::CStr) -> Option<*mut libc::c_void> {
  // SAFETY: a NUL-terminated name looked up in the loaded images
  let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
  (!ptr.is_null()).then_some(ptr)
}

/// `sandbox_init`, resolved at run time (deprecated since 10.8 but present):
/// its absence is a load failure, never a link failure.
fn sandbox_init_fn() -> Option<SandboxInitFn> {
  // SAFETY: the symbol's documented signature
  symbol(c"sandbox_init").map(|p| unsafe {
    std::mem::transmute::<*mut libc::c_void, SandboxInitFn>(p)
  })
}

fn sandbox_check_fn() -> Option<SandboxCheckFn> {
  // SAFETY: the symbol's signature (libsystem_sandbox)
  symbol(c"sandbox_check").map(|p| unsafe {
    std::mem::transmute::<*mut libc::c_void, SandboxCheckFn>(p)
  })
}

/// Whether `sandbox_init` can be called here.
pub fn sandbox_init_available() -> bool {
  sandbox_init_fn().is_some()
}

/// Whether this process already runs under a Seatbelt profile; `None` when
/// `sandbox_check` is missing.
pub fn is_sandboxed() -> Option<bool> {
  let check = sandbox_check_fn()?;
  // SAFETY: a null operation asks whether the process is sandboxed at all
  let res = unsafe { check(libc::getpid(), std::ptr::null(), 0) };
  Some(res != 0)
}

/// Whether this process's profile refuses reading `path`'s contents (the
/// measured check); `None` when it can't be asked.
pub fn refuses_read(path: &Path) -> Option<bool> {
  let check = sandbox_check_fn()?;
  let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
  // a refused check would log a violation; ask quietly where the flag
  // exists
  let no_report = symbol(c"SANDBOX_CHECK_NO_REPORT")
    // SAFETY: an exported `enum sandbox_filter_type` constant
    .map(|p| unsafe { *(p as *const libc::c_int) })
    .unwrap_or(0);
  // SAFETY: the documented variadic form: a path filter and its path
  let res = unsafe {
    check(
      libc::getpid(),
      c"file-read-data".as_ptr(),
      SANDBOX_FILTER_PATH | no_report,
      c_path.as_ptr(),
    )
  };
  Some(res != 0)
}

/// Applies `profile` to this process with `sandbox_init` (process-wide and
/// immediately, every thread included). A process that is already
/// sandboxed gets an error (Seatbelt refuses a second profile).
pub fn apply_profile(profile: &str) -> Result<(), String> {
  let Some(init) = sandbox_init_fn() else {
    return Err("sandbox_init is not available".to_string());
  };
  let c_profile = CString::new(profile)
    .map_err(|_| "the profile holds a NUL byte".to_string())?;
  let mut err: *mut libc::c_char = std::ptr::null_mut();
  // SAFETY: a valid profile string and out pointer; flags 0: an SBPL string
  let res = unsafe { init(c_profile.as_ptr(), 0, &mut err) };
  if res == 0 {
    return Ok(());
  }
  let errno = io::Error::last_os_error();
  let message = if err.is_null() {
    format!("sandbox_init failed: {errno}")
  } else {
    // SAFETY: a NUL-terminated message from sandbox_init
    let text = unsafe { std::ffi::CStr::from_ptr(err) }
      .to_string_lossy()
      .into_owned();
    if let Some(free) = symbol(c"sandbox_free_error") {
      // SAFETY: the documented way to release the message
      unsafe {
        std::mem::transmute::<*mut libc::c_void, SandboxFreeErrorFn>(free)(err)
      };
    }
    format!("sandbox_init failed: {text}")
  };
  Err(message)
}

/// Writes `plan` to a new temporary file, unlinked at once (the descriptor
/// keeps it), for the image after a re-exec to read
/// ([`take_inherited_plan`]). Not close-on-exec: it is the descriptor the
/// new image inherits.
fn plan_file(plan: &EnforcePlan) -> io::Result<OwnedFd> {
  let bytes = plan.to_bytes()?;
  let dir = std::env::temp_dir();
  let template = dir.join("deno-kernel-plan-XXXXXX");
  let Ok(c_template) = CString::new(template.as_os_str().as_bytes()) else {
    return Err(io::Error::other("the temp directory holds a NUL byte"));
  };
  let mut raw = c_template.into_bytes_with_nul();
  // SAFETY: a writable NUL-terminated template
  let fd = unsafe { libc::mkstemp(raw.as_mut_ptr() as *mut libc::c_char) };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just created
  let file = unsafe { OwnedFd::from_raw_fd(fd) };
  // SAFETY: the path mkstemp filled in; unlinked at once, the fd keeps it
  unsafe { libc::unlink(raw.as_ptr() as *const libc::c_char) };
  let mut written = 0;
  while written < bytes.len() {
    // SAFETY: a valid descriptor and buffer
    let n = unsafe {
      libc::write(
        file.as_raw_fd(),
        bytes[written..].as_ptr() as *const libc::c_void,
        bytes.len() - written,
      )
    };
    if n < 0 {
      let err = io::Error::last_os_error();
      if err.kind() == io::ErrorKind::Interrupted {
        continue;
      }
      return Err(err);
    }
    if n == 0 {
      return Err(io::Error::other("writing the plan made no progress"));
    }
    written += n as usize;
  }
  Ok(file)
}

/// Makes every descriptor but stdio, those `keep` names and `plan_fd`
/// close-on-exec, then `execve`s `program` with `argv` and `env` plus the
/// plan's variable. Returns only on failure.
fn exec_with_plan(
  program: &[u8],
  argv: Vec<Vec<u8>>,
  env: Vec<(OsString, OsString)>,
  plan_fd: i32,
  keep: impl Fn(i32) -> bool,
) -> io::Error {
  for fd in 3..open_max() {
    if fd == plan_fd || keep(fd) {
      continue;
    }
    // SAFETY: setting a flag; a closed number just fails
    unsafe {
      let flags = libc::fcntl(fd, libc::F_GETFD);
      if flags >= 0 {
        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
      }
    }
  }
  let mut envp: Vec<Vec<u8>> = env
    .iter()
    .filter(|(k, _)| k != PLAN_FD_ENV_VAR)
    .map(|(k, v)| {
      let mut kv = k.as_bytes().to_vec();
      kv.push(b'=');
      kv.extend_from_slice(v.as_bytes());
      kv
    })
    .collect();
  envp.push(format!("{PLAN_FD_ENV_VAR}={plan_fd}").into_bytes());
  let (Some(argv), Some(envp), Ok(program)) =
    (to_cstrings(argv), to_cstrings(envp), CString::new(program))
  else {
    return io::Error::other("an argument holds a NUL byte");
  };
  let mut argv_p: Vec<*const libc::c_char> =
    argv.iter().map(|a| a.as_ptr()).collect();
  argv_p.push(std::ptr::null());
  let mut envp_p: Vec<*const libc::c_char> =
    envp.iter().map(|e| e.as_ptr()).collect();
  envp_p.push(std::ptr::null());
  // SAFETY: NUL-terminated arrays of valid C strings that outlive the call
  unsafe {
    libc::execve(program.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr());
  }
  io::Error::last_os_error()
}

/// Re-executes this program (the same argv; the PID is unchanged) with
/// `plan` in an unlinked file and `env` as its environment; the new image
/// applies the plan with `sandbox_init` first thing in `main`, before it
/// opens anything ([`take_inherited_plan`]). Every descriptor but stdio and
/// those `keep` names is closed by the exec, so nothing this image opened
/// read-write (the caches) survives into the confined one. Returns only on
/// failure.
pub fn reexec_self(
  plan: &EnforcePlan,
  exe: &Path,
  env: Vec<(OsString, OsString)>,
  keep: impl Fn(i32) -> bool,
) -> io::Error {
  let file = match plan_file(plan) {
    Ok(file) => file,
    Err(err) => return err,
  };
  let argv: Vec<Vec<u8>> =
    std::env::args_os().map(|a| a.as_bytes().to_vec()).collect();
  let err = exec_with_plan(
    exe.as_os_str().as_bytes(),
    argv,
    env,
    file.as_raw_fd(),
    keep,
  );
  drop(file);
  err
}

/// The fallback where `sandbox_init` is missing: re-executes this program
/// under `sandbox-exec -p <profile>` with `plan` in an unlinked file the new
/// image reads ([`take_inherited_plan`]) and `env` as its environment. Every
/// descriptor but stdio, those `keep` names and the plan's is made
/// close-on-exec first. Returns only on failure.
pub fn reexec_under_sandbox_exec(
  profile: &str,
  plan: &EnforcePlan,
  env: Vec<(OsString, OsString)>,
  keep: impl Fn(i32) -> bool,
) -> io::Error {
  let file = match plan_file(plan) {
    Ok(file) => file,
    Err(err) => return err,
  };
  let exe = match std::env::current_exe() {
    Ok(exe) => exe,
    Err(err) => return err,
  };
  let mut argv: Vec<Vec<u8>> = vec![
    SANDBOX_EXEC.as_bytes().to_vec(),
    b"-p".to_vec(),
    profile.as_bytes().to_vec(),
    exe.as_os_str().as_bytes().to_vec(),
  ];
  argv.extend(std::env::args_os().skip(1).map(|a| a.as_bytes().to_vec()));
  let err =
    exec_with_plan(SANDBOX_EXEC.as_bytes(), argv, env, file.as_raw_fd(), keep);
  drop(file);
  err
}

fn to_cstrings(items: Vec<Vec<u8>>) -> Option<Vec<CString>> {
  items.into_iter().map(|b| CString::new(b).ok()).collect()
}

fn open_max() -> i32 {
  // SAFETY: sysconf has no preconditions
  let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
  if max <= 0 {
    1024
  } else {
    max.min(65536) as i32
  }
}

/// The plan the image before this one handed over through `sandbox-exec`
/// (the variable names an unlinked file), read, closed and the variable
/// removed. Call first thing in `main`. `Err`: refuse to start. The plan is
/// only ever compared with the one this image compiles; it never skips a
/// check by itself.
pub fn take_inherited_plan() -> Result<Option<EnforcePlan>, String> {
  let Some(value) = std::env::var_os(PLAN_FD_ENV_VAR) else {
    return Ok(None);
  };
  // SAFETY: called while the process has one thread
  unsafe { std::env::remove_var(PLAN_FD_ENV_VAR) };
  let fd: i32 = value
    .to_str()
    .and_then(|v| v.parse().ok())
    .filter(|fd| *fd > 2)
    .ok_or_else(|| {
      format!("{PLAN_FD_ENV_VAR}={value:?} is not a descriptor")
    })?;
  // SAFETY: checking the descriptor is open before owning it
  if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
    return Err(format!("{PLAN_FD_ENV_VAR}: descriptor {fd} is not open"));
  }
  // SAFETY: an open descriptor this process now owns
  let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
  let meta = file
    .metadata()
    .map_err(|err| format!("the plan descriptor: {err}"))?;
  if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_PLAN_BYTES as u64 {
    return Err("the plan descriptor is not a plan file".to_string());
  }
  let mut bytes = vec![0u8; meta.len() as usize];
  use std::os::unix::fs::FileExt;
  file
    .read_exact_at(&mut bytes, 0)
    .map_err(|err| format!("reading the plan: {err}"))?;
  drop(file);
  EnforcePlan::from_bytes(&bytes).map(Some)
}
