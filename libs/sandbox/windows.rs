// Copyright 2018-2026 the Deno authors. MIT license.

//! Windows, the small tier (DENEXT.md item 902): what a process can apply
//! to itself, in place, with no relaunch and no administrator rights.
//!
//! - [`apply_job`]: a job object the process assigns itself to. No
//!   breakaway (a child can't leave it), kill on close (the tree ends with
//!   the process), and with no `--allow-run` an active-process limit of 1
//!   (the kernel refuses every `CreateProcess`).
//! - [`label_low`] and [`lower_integrity`]: write confinement. The process
//!   lowers its own token to Low integrity; Windows then refuses writes to
//!   every object labelled Medium or higher, which is everything a user
//!   owns unless it was labelled Low. The granted write roots are labelled
//!   Low first (a persistent change to those folders, which this module
//!   never undoes: another instance may be running).
//!
//! Not held here, by design (the published matrix says so): reads and the
//! network stay Deno-only on Windows, and a folder that was already labelled
//! Low before the program ran (`AppData\LocalLow`) stays writable.

#![allow(
  clippy::disallowed_methods,
  reason = "the sandbox acts on the real file system and environment, never a virtual one"
)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
use windows_sys::Win32::Security::CreateWellKnownSid;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetSecurityDescriptorSacl;
use windows_sys::Win32::Security::GetSidSubAuthority;
use windows_sys::Win32::Security::GetSidSubAuthorityCount;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::LABEL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;
use windows_sys::Win32::Security::SID_AND_ATTRIBUTES;
use windows_sys::Win32::Security::SetTokenInformation;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_MANDATORY_LABEL;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TokenIntegrityLevel;
use windows_sys::Win32::Security::WinLowLabelSid;
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;
use windows_sys::Win32::System::JobObjects::SetInformationJobObject;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;

use crate::plan::WindowsPlan;

/// `SECURITY_MANDATORY_LOW_RID`.
pub const LOW_RID: u32 = 0x1000;
/// `SECURITY_MANDATORY_MEDIUM_RID`.
pub const MEDIUM_RID: u32 = 0x2000;
/// `SE_GROUP_INTEGRITY`.
const SE_GROUP_INTEGRITY: u32 = 0x20;
/// `SYSTEM_MANDATORY_LABEL_ACE_TYPE`.
const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
/// `SDDL_REVISION_1`.
const SDDL_REVISION_1: u32 = 1;

fn wide(s: &OsStr) -> Vec<u16> {
  s.encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> String {
  format!("{what}: {}", io::Error::last_os_error())
}

/// What the job this process was put in holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobState {
  pub in_job: bool,
  pub kill_on_close: bool,
  /// `Some(n)`: at most `n` processes may be active in the job.
  pub active_process_limit: Option<u32>,
}

/// Creates a job object (no breakaway, kill on close, and with
/// `no_children` an active-process limit of 1), assigns this process to it,
/// and reads the limits back. The job handle is kept open for the life of
/// the process (never inheritable, so no child can hold the job open or
/// change it); closing it, which only the process's end does, ends every
/// process still in the job.
///
/// Fails where the process can't be put in a job (a parent's job on a
/// Windows without nested jobs): the caller refuses to start.
pub fn apply_job(no_children: bool) -> Result<JobState, String> {
  // SAFETY: null attributes (a handle that isn't inheritable) and a null
  // name (an anonymous job nobody else can open by name)
  let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
  if job.is_null() {
    return Err(last_error("CreateJobObject"));
  }
  // SAFETY: integers only: all zeroes is valid
  let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
    unsafe { std::mem::zeroed() };
  // no BREAKAWAY_OK / SILENT_BREAKAWAY_OK: nothing leaves the job
  info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
  if no_children {
    info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    info.BasicLimitInformation.ActiveProcessLimit = 1;
  }
  // SAFETY: `info` outlives the call; the length is its size
  let set = unsafe {
    SetInformationJobObject(
      job,
      JobObjectExtendedLimitInformation,
      (&raw const info).cast(),
      std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
    )
  };
  if set == 0 {
    let err = last_error("SetInformationJobObject");
    // SAFETY: an open handle this function owns
    unsafe { CloseHandle(job) };
    return Err(err);
  }
  // SAFETY: both handles are valid (the pseudo-handle of this process)
  if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
    let err = last_error("AssignProcessToJobObject");
    // SAFETY: as above
    unsafe { CloseHandle(job) };
    return Err(err);
  }
  let state = job_state(job)?;
  if !state.in_job
    || !state.kill_on_close
    || (no_children && state.active_process_limit != Some(1))
  {
    return Err(format!(
      "the job object was assigned but doesn't hold: {state:?}"
    ));
  }
  // the handle is leaked on purpose: it lives as long as the process
  Ok(state)
}

/// Whether this process is in `job`, and the job's limits.
fn job_state(job: HANDLE) -> Result<JobState, String> {
  let mut in_job = 0;
  // SAFETY: valid handles and out pointer
  if unsafe { IsProcessInJob(GetCurrentProcess(), job, &mut in_job) } == 0 {
    return Err(last_error("IsProcessInJob"));
  }
  // SAFETY: integers only
  let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
    unsafe { std::mem::zeroed() };
  // SAFETY: `info` is the size passed
  let ok = unsafe {
    QueryInformationJobObject(
      job,
      JobObjectExtendedLimitInformation,
      (&raw mut info).cast(),
      std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
      std::ptr::null_mut(),
    )
  };
  if ok == 0 {
    return Err(last_error("QueryInformationJobObject"));
  }
  let flags = info.BasicLimitInformation.LimitFlags;
  Ok(JobState {
    in_job: in_job != 0,
    kill_on_close: flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
    active_process_limit: (flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0)
      .then_some(info.BasicLimitInformation.ActiveProcessLimit),
  })
}

struct Token(HANDLE);

impl Drop for Token {
  fn drop(&mut self) {
    // SAFETY: an open token handle this value owns
    unsafe { CloseHandle(self.0) };
  }
}

fn process_token(access: u32) -> Result<Token, String> {
  let mut token: HANDLE = std::ptr::null_mut();
  // SAFETY: the pseudo-handle of this process and a valid out pointer
  if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
    return Err(last_error("OpenProcessToken"));
  }
  Ok(Token(token))
}

/// This process's integrity level (the RID of its token's mandatory label:
/// [`LOW_RID`], [`MEDIUM_RID`], 0x3000 for an elevated process).
pub fn integrity_level() -> Result<u32, String> {
  let token = process_token(TOKEN_QUERY)?;
  // aligned for TOKEN_MANDATORY_LABEL (a pointer and a u32), whose SID the
  // call writes behind it in the same buffer
  let mut buf = [0u64; 32];
  let mut len = 0u32;
  // SAFETY: a buffer of the size passed
  let ok = unsafe {
    GetTokenInformation(
      token.0,
      TokenIntegrityLevel,
      buf.as_mut_ptr().cast(),
      std::mem::size_of_val(&buf) as u32,
      &mut len,
    )
  };
  if ok == 0 {
    return Err(last_error("GetTokenInformation(TokenIntegrityLevel)"));
  }
  // SAFETY: the call filled in a TOKEN_MANDATORY_LABEL whose SID lies in
  // the same buffer
  unsafe {
    let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    Ok(sid_last_rid(label.Label.Sid))
  }
}

/// The last sub-authority of `sid` (an integrity SID's level).
///
/// # Safety
/// `sid` must point to a valid SID.
unsafe fn sid_last_rid(sid: *mut core::ffi::c_void) -> u32 {
  // SAFETY: the caller's contract
  unsafe {
    let count = *GetSidSubAuthorityCount(sid);
    if count == 0 {
      return 0;
    }
    *GetSidSubAuthority(sid, u32::from(count) - 1)
  }
}

/// Lowers this process's token to Low integrity, for the whole process and
/// every process it starts, and reads the level back. It can't be raised
/// again. Handles opened before the call keep the access they were opened
/// with, so the caller applies it before it opens anything of its own.
pub fn lower_integrity() -> Result<(), String> {
  let token = process_token(TOKEN_ADJUST_DEFAULT | TOKEN_QUERY)?;
  // SECURITY_MAX_SID_SIZE is 68 bytes; aligned as a SID is
  let mut sid = [0u32; 17];
  let mut sid_len = std::mem::size_of_val(&sid) as u32;
  // SAFETY: a buffer of the size passed
  if unsafe {
    CreateWellKnownSid(
      WinLowLabelSid,
      std::ptr::null_mut(),
      sid.as_mut_ptr().cast(),
      &mut sid_len,
    )
  } == 0
  {
    return Err(last_error("CreateWellKnownSid(WinLowLabelSid)"));
  }
  let label = TOKEN_MANDATORY_LABEL {
    Label: SID_AND_ATTRIBUTES {
      Sid: sid.as_mut_ptr().cast(),
      Attributes: SE_GROUP_INTEGRITY,
    },
  };
  // SAFETY: `label` and the SID it points to outlive the call
  let ok = unsafe {
    SetTokenInformation(
      token.0,
      TokenIntegrityLevel,
      (&raw const label).cast(),
      std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32
        + GetLengthSid(sid.as_mut_ptr().cast()),
    )
  };
  if ok == 0 {
    return Err(last_error("SetTokenInformation(TokenIntegrityLevel)"));
  }
  drop(token);
  match integrity_level()? {
    level if level <= LOW_RID => Ok(()),
    level => Err(format!(
      "the token was lowered but reads back as integrity level {level:#x}"
    )),
  }
}

/// The integrity level `path` is labelled with (`None`: no label, which
/// Windows treats as Medium).
pub fn path_label(path: &Path) -> Result<Option<u32>, String> {
  let wide_path = wide(path.as_os_str());
  let mut sacl: *mut ACL = std::ptr::null_mut();
  let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
  // SAFETY: a NUL-terminated path and valid out pointers
  let rc = unsafe {
    GetNamedSecurityInfoW(
      wide_path.as_ptr(),
      SE_FILE_OBJECT,
      LABEL_SECURITY_INFORMATION,
      std::ptr::null_mut(),
      std::ptr::null_mut(),
      std::ptr::null_mut(),
      &mut sacl,
      &mut descriptor,
    )
  };
  if rc != 0 {
    return Err(format!(
      "reading the integrity label of {}: {}",
      path.display(),
      io::Error::from_raw_os_error(rc as i32)
    ));
  }
  let mut level = None;
  if !sacl.is_null() {
    // SAFETY: `sacl` points into `descriptor`, which is freed below
    unsafe {
      for index in 0..u32::from((*sacl).AceCount) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        if GetAce(sacl, index, &mut ace) == 0 {
          continue;
        }
        // ACE_HEADER { AceType: u8, AceFlags: u8, AceSize: u16 }, then for
        // a mandatory label ACE: Mask: u32, SidStart
        if *(ace as *const u8) == SYSTEM_MANDATORY_LABEL_ACE_TYPE {
          let sid = (ace as *mut u8).add(8).cast();
          level = Some(sid_last_rid(sid));
        }
      }
    }
  }
  if !descriptor.is_null() {
    // SAFETY: allocated by GetNamedSecurityInfoW
    unsafe { LocalFree(descriptor.cast()) };
  }
  Ok(level)
}

/// Labels `path` Low integrity (a folder: inherited by everything beneath
/// it, existing or new), so a Low process may write it. A path that already
/// carries a Low label is left alone. A persistent change, visible with
/// `icacls`; `icacls <path> /setintegritylevel (OI)(CI)M` undoes it.
pub fn label_low(path: &Path) -> Result<(), String> {
  if path_label(path)?.is_some_and(|level| level <= LOW_RID) {
    return Ok(());
  }
  // a mandatory label ACE, no-write-up, Low; object + container inherit
  // for a folder
  let sddl = if path.is_dir() {
    "S:(ML;OICI;NW;;;LW)"
  } else {
    "S:(ML;;NW;;;LW)"
  };
  let wide_sddl = wide(OsStr::new(sddl));
  let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
  // SAFETY: a NUL-terminated string and a valid out pointer
  if unsafe {
    ConvertStringSecurityDescriptorToSecurityDescriptorW(
      wide_sddl.as_ptr(),
      SDDL_REVISION_1,
      &mut descriptor,
      std::ptr::null_mut(),
    )
  } == 0
  {
    return Err(last_error("the Low label's security descriptor"));
  }
  let mut present = 0;
  let mut defaulted = 0;
  let mut sacl: *mut ACL = std::ptr::null_mut();
  // SAFETY: `descriptor` is valid until LocalFree below
  let got = unsafe {
    GetSecurityDescriptorSacl(
      descriptor,
      &mut present,
      &mut sacl,
      &mut defaulted,
    )
  };
  let result = if got == 0 || present == 0 || sacl.is_null() {
    Err(last_error("the Low label's SACL"))
  } else {
    let wide_path = wide(path.as_os_str());
    // SAFETY: a NUL-terminated path and a SACL valid until LocalFree
    let rc = unsafe {
      SetNamedSecurityInfoW(
        wide_path.as_ptr(),
        SE_FILE_OBJECT,
        LABEL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
        sacl,
      )
    };
    if rc == 0 {
      Ok(())
    } else {
      Err(format!(
        "labelling {} Low integrity: {}",
        path.display(),
        io::Error::from_raw_os_error(rc as i32)
      ))
    }
  };
  // SAFETY: allocated by the conversion
  unsafe { LocalFree(descriptor.cast()) };
  result
}

/// Why `path` may not be labelled Low, if it may not: labelling is
/// persistent and makes the folder writable to every Low process of the
/// user, so the roots of the system and of the user's whole profile are
/// refused (a write grant that wide can't be held by this tier; the caller
/// refuses to start, naming it).
pub fn refuses_label(path: &Path) -> Option<&'static str> {
  // lower case, backslashes, and without the verbatim prefix a
  // canonicalized path carries (`\\?\C:\x`, `\\?\UNC\server\share`)
  let norm = |p: &Path| {
    let text = p.to_string_lossy().to_lowercase().replace('/', "\\");
    match text.strip_prefix(r"\\?\unc\") {
      Some(rest) => format!(r"\\{rest}"),
      None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    }
  };
  let target = norm(path);
  let target = target.trim_end_matches('\\');
  // a drive or share root: `c:`, `\\server\share`
  let is_root = (target.len() == 2 && target.ends_with(':'))
    || (target.starts_with(r"\\") && target[2..].matches('\\').count() <= 1);
  if is_root {
    return Some("a drive root");
  }
  let holds = |var: &str| {
    std::env::var_os(var)
      .filter(|v| !v.is_empty())
      .map(|v| norm(Path::new(&v)))
      .is_some_and(|dir| {
        let dir = dir.trim_end_matches('\\');
        // the folder itself, or one that holds it
        dir == target || dir.starts_with(&format!("{target}\\"))
      })
  };
  let inside = |var: &str| {
    std::env::var_os(var)
      .filter(|v| !v.is_empty())
      .map(|v| norm(Path::new(&v)))
      .is_some_and(|dir| {
        let dir = dir.trim_end_matches('\\');
        target == dir || target.starts_with(&format!("{dir}\\"))
      })
  };
  if holds("USERPROFILE") {
    return Some("the user's profile folder (or a folder that holds it)");
  }
  if holds("SystemRoot") || inside("SystemRoot") {
    return Some("the Windows folder");
  }
  for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramData"] {
    if holds(var) || inside(var) {
      return Some("a system-wide program folder");
    }
  }
  None
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
  pub job: JobState,
  /// The process now runs at Low integrity.
  pub low_integrity: bool,
  /// The paths labelled Low (or found labelled) for the write grant.
  pub labelled: Vec<PathBuf>,
}

/// Applies `plan` to this process, in place: the job object first (it
/// can't fail half-way into a state that holds less), then the write
/// roots' labels while the process is still at its own level (a Low
/// process couldn't label them), then the token. Any failure is an error:
/// the caller refuses to start rather than run with part of it.
pub fn apply(plan: &WindowsPlan) -> Result<Applied, String> {
  let mut labelled = Vec::new();
  if let Some(roots) = &plan.write_roots {
    for root in roots {
      if let Some(why) = refuses_label(root) {
        return Err(format!(
          "the write grant {} is {why}: holding it would label it Low integrity for every program, so it can't be held here",
          root.display()
        ));
      }
    }
  }
  let job = apply_job(plan.no_children)?;
  let low_integrity = match &plan.write_roots {
    None => false,
    Some(roots) => {
      for root in roots {
        // a root that doesn't exist yet gets no label: creating it then
        // fails (the caller reported that gap)
        if root.exists() {
          label_low(root)?;
          labelled.push(root.clone());
        }
      }
      lower_integrity()?;
      true
    }
  };
  Ok(Applied {
    job,
    low_integrity,
    labelled,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  // The tests that change the process (a job, the token) run in a child:
  // `cargo test` re-runs this binary with one test selected and a variable
  // set, so the harness's own process stays as it was.
  fn in_child(test: &str, env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut command = std::process::Command::new(exe);
    command
      .args(["--exact", test, "--nocapture", "--test-threads=1"])
      .env("DENO_SANDBOX_WINDOWS_CHILD", "1");
    for (k, v) in env {
      command.env(k, v);
    }
    command.output().unwrap()
  }

  fn is_child() -> bool {
    std::env::var_os("DENO_SANDBOX_WINDOWS_CHILD").is_some()
  }

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
      .join(format!("deno_sandbox_win_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }

  #[test]
  fn the_job_refuses_children_and_reads_back() {
    if !is_child() {
      let out = in_child(
        "windows::tests::the_job_refuses_children_and_reads_back",
        &[],
      );
      let text = String::from_utf8_lossy(&out.stdout);
      assert!(out.status.success(), "{text}");
      assert!(text.contains("child-ok"), "{text}");
      return;
    }
    let state = apply_job(true).unwrap();
    assert_eq!(
      state,
      JobState {
        in_job: true,
        kill_on_close: true,
        active_process_limit: Some(1),
      }
    );
    // the kernel refuses a second process in the job
    let spawned = std::process::Command::new("cmd")
      .args(["/c", "exit"])
      .spawn();
    assert!(spawned.is_err(), "a child started under the process limit");
    println!("child-ok");
  }

  #[test]
  fn a_job_with_children_keeps_them_in_it() {
    if !is_child() {
      let out =
        in_child("windows::tests::a_job_with_children_keeps_them_in_it", &[]);
      let text = String::from_utf8_lossy(&out.stdout);
      assert!(out.status.success(), "{text}");
      assert!(text.contains("child-ok"), "{text}");
      return;
    }
    let state = apply_job(false).unwrap();
    assert!(state.in_job && state.kill_on_close);
    assert_eq!(state.active_process_limit, None);
    // children start; one that asks to break away is refused
    assert!(
      std::process::Command::new("cmd")
        .args(["/c", "exit"])
        .status()
        .unwrap()
        .success()
    );
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let breakaway = std::process::Command::new("cmd")
      .args(["/c", "exit"])
      .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
      .spawn();
    assert!(breakaway.is_err(), "a child broke away from the job");
    println!("child-ok");
  }

  #[test]
  fn low_integrity_writes_only_labelled_folders() {
    if !is_child() {
      let dir = scratch("low");
      let granted = dir.join("granted");
      let outside = dir.join("outside");
      std::fs::create_dir_all(granted.join("sub")).unwrap();
      std::fs::create_dir_all(&outside).unwrap();
      std::fs::write(granted.join("sub").join("old.txt"), "old").unwrap();
      std::fs::write(outside.join("old.txt"), "old").unwrap();
      let out = in_child(
        "windows::tests::low_integrity_writes_only_labelled_folders",
        &[("DENO_SANDBOX_WINDOWS_DIR", dir.to_str().unwrap())],
      );
      let text = String::from_utf8_lossy(&out.stdout);
      assert!(out.status.success(), "{text}");
      assert!(text.contains("child-ok"), "{text}");
      // the label persists, and the folder outside kept its own
      assert_eq!(path_label(&granted).unwrap(), Some(LOW_RID));
      assert_ne!(path_label(&outside).unwrap(), Some(LOW_RID));
      assert_eq!(
        std::fs::read_to_string(outside.join("old.txt")).unwrap(),
        "old"
      );
      std::fs::remove_dir_all(&dir).unwrap();
      return;
    }
    let dir =
      PathBuf::from(std::env::var_os("DENO_SANDBOX_WINDOWS_DIR").unwrap());
    let granted = dir.join("granted");
    let outside = dir.join("outside");
    assert!(integrity_level().unwrap() >= MEDIUM_RID);
    let applied = apply(&WindowsPlan {
      no_children: false,
      write_roots: Some(vec![granted.clone()]),
    })
    .unwrap();
    assert!(applied.low_integrity);
    assert_eq!(applied.labelled, vec![granted.clone()]);
    assert_eq!(integrity_level().unwrap(), LOW_RID);
    // inside the grant: new files, existing files beneath it
    std::fs::write(granted.join("new.txt"), "new").unwrap();
    std::fs::write(granted.join("sub").join("old.txt"), "changed").unwrap();
    // outside it: create, overwrite, delete and rename are refused
    assert!(std::fs::write(outside.join("new.txt"), "x").is_err());
    assert!(std::fs::write(outside.join("old.txt"), "x").is_err());
    assert!(std::fs::remove_file(outside.join("old.txt")).is_err());
    assert!(
      std::fs::rename(outside.join("old.txt"), outside.join("moved.txt"))
        .is_err()
    );
    // reads are not confined on Windows (the matrix says so)
    assert_eq!(
      std::fs::read_to_string(outside.join("old.txt")).unwrap(),
      "old"
    );
    // a child inherits the level
    let child = std::process::Command::new("cmd")
      .args(["/c", "echo x>"])
      .arg(outside.join("child.txt"))
      .output()
      .unwrap();
    assert!(!outside.join("child.txt").exists(), "{child:?}");
    println!("child-ok");
  }

  #[test]
  fn wide_roots_are_never_labelled() {
    for (path, refused) in [
      (r"C:\", true),
      (r"C:", true),
      (r"C:\Windows", true),
      (r"C:\Windows\Temp", true),
      (r"C:\Program Files\App", true),
      (r"C:\Users", true),
      (r"\\?\C:\", true),
      (r"\\?\C:\Windows", true),
      (r"\\?\C:\Users", true),
      (r"\\server\share", true),
      (r"\\?\UNC\server\share", true),
    ] {
      assert_eq!(refuses_label(Path::new(path)).is_some(), refused, "{path}");
    }
    let profile = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    assert!(refuses_label(&profile).is_some());
    let verbatim = PathBuf::from(format!(r"\\?\{}", profile.display()));
    assert!(refuses_label(&verbatim).is_some());
    assert!(refuses_label(&profile.join("Documents")).is_none());
    assert!(refuses_label(&std::env::temp_dir().join("x")).is_none());
  }
}
