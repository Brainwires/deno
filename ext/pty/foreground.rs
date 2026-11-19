// Copyright 2018-2026 the Deno authors. MIT license.

//! What runs in a terminal right now: the foreground process of a pty, with
//! its name, arguments and working directory (`Deno.Pty.foreground()`,
//! DENEXT.md item 461).
//!
//! This file asks the kernel about ONE process by id; which process is the
//! terminal's foreground one is decided by `unix.rs` (`tcgetpgrp` on the
//! master) and `windows.rs` (the console's process tree). It depends on
//! nothing but `libc` / `windows-sys`, so its tests run against real
//! processes without the rest of the runtime.
//!
//! Every field but the name is best-effort: a process of another user (a
//! `sudo` in the terminal), a zombie, or one that exits between two reads
//! gives `None` for what the kernel refuses, never an error.

/// A terminal's foreground process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
  /// The foreground process group (`tcgetpgrp`); `None` on Windows, which
  /// has no process groups of this kind.
  pub pgid: Option<i32>,
  /// The process described: the group's leader, or when the leader is gone
  /// another member of the group.
  pub pid: u32,
  /// Its name (`vim`; on Windows the image name, `vim.exe`).
  pub name: String,
  /// Its arguments, the program first.
  pub argv: Option<Vec<String>>,
  /// Its working directory.
  pub cwd: Option<String>,
}

/// One process, as far as the kernel tells.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
  pub name: String,
  pub argv: Option<Vec<String>>,
  pub cwd: Option<String>,
}

#[cfg(unix)]
pub use unix_impl::*;

#[cfg(unix)]
mod unix_impl {
  use super::Foreground;
  use super::ProcessInfo;

  /// The foreground process of the group `pgrp`: the leader when it is
  /// still a live process, else the first live member (a pipeline whose
  /// first command has exited keeps its group).
  pub fn foreground_of_group(pgrp: libc::pid_t) -> Option<Foreground> {
    if pgrp <= 0 {
      return None;
    }
    let describe = |pid: libc::pid_t| {
      process_info(pid).map(|info| Foreground {
        pgid: Some(pgrp),
        pid: pid as u32,
        name: info.name,
        argv: info.argv,
        cwd: info.cwd,
      })
    };
    describe(pgrp).or_else(|| {
      group_members(pgrp)
        .into_iter()
        .filter(|pid| *pid != pgrp)
        .find_map(describe)
    })
  }

  /// A live (not zombie) process's name, arguments and working directory;
  /// `None` when there is no such process or it is a zombie.
  #[cfg(any(target_os = "linux", target_os = "android"))]
  pub fn process_info(pid: libc::pid_t) -> Option<ProcessInfo> {
    let (name, state, _) = read_stat(pid)?;
    if state == b'Z' || state == b'X' {
      return None;
    }
    Some(ProcessInfo {
      name,
      argv: read_cmdline(pid),
      cwd: read_cwd(pid),
    })
  }

  /// `/proc/<pid>/stat`: the name (`comm`), the state letter and the
  /// process group. The name is between the first `(` and the LAST `)` (a
  /// name may hold both).
  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn read_stat(pid: libc::pid_t) -> Option<(String, u8, libc::pid_t)> {
    let bytes = read_proc(pid, "stat")?;
    parse_stat(&bytes)
  }

  #[cfg(any(target_os = "linux", target_os = "android", test))]
  pub(super) fn parse_stat(bytes: &[u8]) -> Option<(String, u8, libc::pid_t)> {
    let open = bytes.iter().position(|b| *b == b'(')?;
    let close = bytes.iter().rposition(|b| *b == b')')?;
    if close < open {
      return None;
    }
    let name = String::from_utf8_lossy(&bytes[open + 1..close]).into_owned();
    let rest = std::str::from_utf8(bytes.get(close + 1..)?).ok()?;
    // state ppid pgrp …
    let mut fields = rest.split_ascii_whitespace();
    let state = *fields.next()?.as_bytes().first()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    if name.is_empty() {
      return None;
    }
    Some((name, state, pgrp))
  }

  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn read_proc(pid: libc::pid_t, file: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    // a process's own facts are only in procfs
    #[allow(clippy::disallowed_methods, reason = "reads procfs")]
    let mut f = std::fs::File::open(format!("/proc/{pid}/{file}")).ok()?;
    let mut bytes = Vec::new();
    // a command line is bounded by ARG_MAX; never read without a limit
    f.by_ref()
      .take(MAX_ARGS_BYTES as u64)
      .read_to_end(&mut bytes)
      .ok()?;
    Some(bytes)
  }

  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn read_cmdline(pid: libc::pid_t) -> Option<Vec<String>> {
    let bytes = read_proc(pid, "cmdline")?;
    split_nul_args(&bytes)
  }

  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn read_cwd(pid: libc::pid_t) -> Option<String> {
    #[allow(clippy::disallowed_methods, reason = "reads procfs")]
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some(path.to_string_lossy().into_owned())
  }

  /// The members of a process group, by scanning procfs.
  #[cfg(any(target_os = "linux", target_os = "android"))]
  fn group_members(pgrp: libc::pid_t) -> Vec<libc::pid_t> {
    #[allow(clippy::disallowed_methods, reason = "reads procfs")]
    let Ok(dir) = std::fs::read_dir("/proc") else {
      return Vec::new();
    };
    let mut members: Vec<libc::pid_t> = dir
      .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
      .filter(|pid| matches!(read_stat(*pid), Some((_, _, g)) if g == pgrp))
      .collect();
    members.sort_unstable();
    members
  }

  /// The most bytes of arguments read for one process.
  const MAX_ARGS_BYTES: usize = 4 * 1024 * 1024;

  /// NUL-separated arguments (`/proc/<pid>/cmdline`, the tail of
  /// `KERN_PROCARGS2`): `None` when there are none (a kernel thread, a
  /// zombie).
  #[cfg(any(target_os = "linux", target_os = "android", test))]
  pub(super) fn split_nul_args(bytes: &[u8]) -> Option<Vec<String>> {
    let bytes = match bytes.last() {
      Some(0) => &bytes[..bytes.len() - 1],
      _ => bytes,
    };
    if bytes.is_empty() {
      return None;
    }
    Some(
      bytes
        .split(|b| *b == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect(),
    )
  }

  #[cfg(target_vendor = "apple")]
  pub fn process_info(pid: libc::pid_t) -> Option<ProcessInfo> {
    // `proc_pidinfo` answers nothing for a zombie, which is the liveness
    // check; a process of another user answers its short info all the same.
    // SAFETY: zeroed plain data, filled by libproc up to the size given.
    let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes long.
    let got = unsafe {
      libc::proc_pidinfo(
        pid,
        libc::PROC_PIDTBSDINFO,
        0,
        &mut bsd as *mut _ as *mut libc::c_void,
        size,
      )
    };
    const SZOMB: u32 = 5;
    let name = if got == size {
      if bsd.pbi_status == SZOMB {
        return None;
      }
      // `pbi_name` is the full name (up to 32 bytes), `pbi_comm` the short
      // one (16)
      let name = c_chars(&bsd.pbi_name);
      if name.is_empty() {
        c_chars(&bsd.pbi_comm)
      } else {
        name
      }
    } else {
      // another user's process: the name alone
      short_name(pid)?
    };
    if name.is_empty() {
      return None;
    }
    Some(ProcessInfo {
      name,
      argv: read_procargs(pid),
      cwd: read_cwd(pid),
    })
  }

  #[cfg(target_vendor = "apple")]
  fn c_chars(chars: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = chars
      .iter()
      .take_while(|c| **c != 0)
      .map(|c| *c as u8)
      .collect();
    String::from_utf8_lossy(&bytes).into_owned()
  }

  #[cfg(target_vendor = "apple")]
  fn short_name(pid: libc::pid_t) -> Option<String> {
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

  #[cfg(target_vendor = "apple")]
  fn read_cwd(pid: libc::pid_t) -> Option<String> {
    // SAFETY: zeroed plain data, filled by libproc up to the size given.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes long.
    let got = unsafe {
      libc::proc_pidinfo(
        pid,
        libc::PROC_PIDVNODEPATHINFO,
        0,
        &mut info as *mut _ as *mut libc::c_void,
        size,
      )
    };
    if got != size {
      return None;
    }
    let path: Vec<u8> = info
      .pvi_cdir
      .vip_path
      .iter()
      .flatten()
      .take_while(|c| **c != 0)
      .map(|c| *c as u8)
      .collect();
    (!path.is_empty()).then(|| String::from_utf8_lossy(&path).into_owned())
  }

  /// `sysctl(KERN_PROCARGS2)`: the kernel's copy of the process's argument
  /// area. Refused (`EINVAL`) for a process of another user.
  #[cfg(target_vendor = "apple")]
  fn read_procargs(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut argmax: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: `argmax` is `len` bytes long.
    let rc = unsafe {
      libc::sysctl(
        mib.as_mut_ptr(),
        mib.len() as libc::c_uint,
        &mut argmax as *mut _ as *mut libc::c_void,
        &mut len,
        std::ptr::null_mut(),
        0,
      )
    };
    if rc != 0 || argmax <= 0 {
      return None;
    }
    let mut buf = vec![0u8; (argmax as usize).min(MAX_ARGS_BYTES)];
    let mut len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: `buf` is `len` bytes long; the kernel writes at most that and
    // stores the count in `len`.
    let rc = unsafe {
      libc::sysctl(
        mib.as_mut_ptr(),
        mib.len() as libc::c_uint,
        buf.as_mut_ptr() as *mut libc::c_void,
        &mut len,
        std::ptr::null_mut(),
        0,
      )
    };
    if rc != 0 {
      return None;
    }
    parse_procargs2(buf.get(..len)?)
  }

  /// The `KERN_PROCARGS2` layout: `argc` (an int), the executable's path,
  /// NUL padding, then `argc` NUL-terminated arguments (the environment
  /// follows and is never read).
  #[cfg(any(target_vendor = "apple", test))]
  pub(super) fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc <= 0 {
      return None;
    }
    let rest = &buf[4..];
    // the executable's path, then the padding
    let path_end = rest.iter().position(|b| *b == 0)?;
    let args_start =
      path_end + rest[path_end..].iter().position(|b| *b != 0)?;
    let mut args = Vec::with_capacity(argc as usize);
    let mut at = args_start;
    for _ in 0..argc {
      let tail = rest.get(at..)?;
      let end = tail.iter().position(|b| *b == 0)?;
      args.push(String::from_utf8_lossy(&tail[..end]).into_owned());
      at += end + 1;
    }
    Some(args)
  }

  #[cfg(target_vendor = "apple")]
  fn group_members(pgrp: libc::pid_t) -> Vec<libc::pid_t> {
    let mut pids = vec![0 as libc::pid_t; 1024];
    let bytes =
      (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer is `bytes` long.
    let count = unsafe {
      libc::proc_listpgrppids(
        pgrp,
        pids.as_mut_ptr() as *mut libc::c_void,
        bytes,
      )
    };
    if count <= 0 {
      return Vec::new();
    }
    pids.truncate((count as usize).min(pids.len()));
    pids.retain(|pid| *pid > 0);
    pids.sort_unstable();
    pids
  }

  #[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  )))]
  pub fn process_info(_pid: libc::pid_t) -> Option<ProcessInfo> {
    None
  }

  #[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  )))]
  fn group_members(_pgrp: libc::pid_t) -> Vec<libc::pid_t> {
    Vec::new()
  }
}

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
  use std::collections::HashMap;
  use std::ffi::c_void;
  use std::os::windows::io::AsRawHandle;
  use std::os::windows::io::FromRawHandle;
  use std::os::windows::io::OwnedHandle;

  use windows_sys::Win32::Foundation::FILETIME;
  use windows_sys::Win32::Foundation::HANDLE;
  use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
  use windows_sys::Win32::Foundation::LocalFree;
  use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32NextW;
  use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
  use windows_sys::Win32::System::Threading::GetExitCodeProcess;
  use windows_sys::Win32::System::Threading::GetProcessTimes;
  use windows_sys::Win32::System::Threading::OpenProcess;
  use windows_sys::Win32::System::Threading::PROCESS_QUERY_INFORMATION;
  use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
  use windows_sys::Win32::System::Threading::PROCESS_VM_READ;
  use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

  use super::Foreground;

  #[link(name = "ntdll")]
  unsafe extern "system" {
    fn NtQueryInformationProcess(
      process: HANDLE,
      class: u32,
      info: *mut c_void,
      len: u32,
      returned: *mut u32,
    ) -> i32;
  }

  /// `PROCESS_BASIC_INFORMATION` (class 0) on a 64-bit Windows.
  #[repr(C)]
  struct BasicInformation {
    exit_status: i32,
    peb: usize,
    affinity: usize,
    base_priority: i32,
    pid: usize,
    parent_pid: usize,
  }

  /// `UNICODE_STRING` in a 64-bit process.
  #[repr(C)]
  #[derive(Clone, Copy)]
  struct UnicodeString {
    /// Bytes, not characters.
    length: u16,
    maximum_length: u16,
    buffer: usize,
  }

  // The documented-by-use layout of a 64-bit PEB and its
  // RTL_USER_PROCESS_PARAMETERS (stable since Windows Vista, the same on
  // x64 and arm64; a 32-bit process under WOW64 has this native PEB too).
  const PEB_PROCESS_PARAMETERS: usize = 0x20;
  const PARAMS_CURRENT_DIRECTORY: usize = 0x38;
  const PARAMS_COMMAND_LINE: usize = 0x70;
  /// The longest command line or path read from another process, in bytes
  /// (a `UNICODE_STRING` holds at most 65534).
  const MAX_STRING_BYTES: usize = 0xfffe;
  const STILL_ACTIVE: u32 = 259;

  /// The image names a shell runs under: a terminal's foreground process is
  /// looked for below these (see [`foreground_of_tree`]).
  const SHELLS: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "bash.exe",
    "sh.exe",
    "zsh.exe",
    "fish.exe",
    "nu.exe",
    "wsl.exe",
    "elvish.exe",
    "xonsh.exe",
  ];
  /// The console host itself (a child of some shells' launchers).
  const CONSOLE_HOSTS: &[&str] = &["conhost.exe", "openconsole.exe"];

  fn is_shell(name: &str) -> bool {
    SHELLS.iter().any(|s| name.eq_ignore_ascii_case(s))
  }

  struct Entry {
    pid: u32,
    parent: u32,
    name: String,
  }

  fn snapshot() -> Vec<Entry> {
    // SAFETY: a snapshot of the process list; closed by the OwnedHandle.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
      return Vec::new();
    }
    // SAFETY: a handle this function owns.
    let snap = unsafe { OwnedHandle::from_raw_handle(raw as _) };
    // SAFETY: zeroed plain data; dwSize is set as the API requires.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut entries = Vec::new();
    // SAFETY: `entry` is a PROCESSENTRY32W with its size set.
    let mut more =
      unsafe { Process32FirstW(snap.as_raw_handle() as HANDLE, &mut entry) };
    while more != 0 {
      let len = entry
        .szExeFile
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(entry.szExeFile.len());
      entries.push(Entry {
        pid: entry.th32ProcessID,
        parent: entry.th32ParentProcessID,
        name: String::from_utf16_lossy(&entry.szExeFile[..len]),
      });
      // SAFETY: as above.
      more =
        unsafe { Process32NextW(snap.as_raw_handle() as HANDLE, &mut entry) };
    }
    entries
  }

  fn open(pid: u32, access: u32) -> Option<OwnedHandle> {
    // SAFETY: opens a process by id; null on failure.
    let handle = unsafe { OpenProcess(access, 0, pid) };
    if handle.is_null() {
      return None;
    }
    // SAFETY: a handle this function owns.
    Some(unsafe { OwnedHandle::from_raw_handle(handle as _) })
  }

  /// When a process started (100 ns ticks), and whether it still runs.
  fn start_time(process: HANDLE) -> Option<(u64, bool)> {
    let zero = FILETIME {
      dwLowDateTime: 0,
      dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) =
      (zero, zero, zero, zero);
    // SAFETY: four FILETIMEs to fill.
    if unsafe {
      GetProcessTimes(
        process,
        &mut created,
        &mut exited,
        &mut kernel,
        &mut user,
      )
    } == 0
    {
      return None;
    }
    let mut code = 0u32;
    // SAFETY: an out parameter.
    let running = unsafe { GetExitCodeProcess(process, &mut code) } != 0
      && code == STILL_ACTIVE;
    Some((
      ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
      running,
    ))
  }

  /// The foreground process of the console whose first process is `root`
  /// (`root_process` is a handle to it, which also keeps its id from being
  /// reused).
  ///
  /// Windows has no foreground process group, and a console's process list
  /// can only be read from inside that console. What a terminal shows as
  /// "running" is found in the process tree instead: starting at the root,
  /// while the process is a shell with a live child, the child started last
  /// is taken (a shell waits for the command it started last); the first
  /// process that is not a shell, or a shell with no child, is the answer.
  /// A child is only believed when it started after its parent (a parent id
  /// outlives its process in the snapshot and may name a newer one).
  pub fn foreground_of_tree(
    root: u32,
    root_process: HANDLE,
  ) -> Option<Foreground> {
    let (root_started, root_running) = start_time(root_process)?;
    let entries = snapshot();
    let mut children: HashMap<u32, Vec<&Entry>> = HashMap::new();
    for entry in &entries {
      children.entry(entry.parent).or_default().push(entry);
    }
    let root_name = entries
      .iter()
      .find(|e| e.pid == root)
      .map(|e| e.name.clone());

    let mut current: Option<(u32, String, u64)> =
      match (root_running, root_name) {
        (true, Some(name)) => Some((root, name, root_started)),
        _ => None,
      };
    // The root gone (a shell that exited and left a program behind): its
    // children are still found by its id, which the handle keeps reserved.
    let mut parent = (root, root_started);
    let mut parent_is_shell = match &current {
      Some((_, name, _)) => is_shell(name),
      None => true,
    };
    // a tree deeper than this is not a terminal's
    for _ in 0..64 {
      if !parent_is_shell {
        break;
      }
      let newest = children
        .get(&parent.0)
        .into_iter()
        .flatten()
        .filter(|e| {
          e.pid != parent.0
            && !CONSOLE_HOSTS.iter().any(|h| e.name.eq_ignore_ascii_case(h))
        })
        .filter_map(|e| {
          let process = open(e.pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
          let (started, running) =
            start_time(process.as_raw_handle() as HANDLE)?;
          (running && started >= parent.1).then_some((e, started))
        })
        .max_by_key(|(e, started)| (*started, e.pid));
      let Some((entry, started)) = newest else {
        break;
      };
      current = Some((entry.pid, entry.name.clone(), started));
      parent = (entry.pid, started);
      parent_is_shell = is_shell(&entry.name);
    }

    let (pid, name, _) = current?;
    let (argv, cwd) = if pid == root {
      read_parameters(root_process)
    } else {
      match open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ) {
        Some(process) => read_parameters(process.as_raw_handle() as HANDLE),
        None => (None, None),
      }
    };
    Some(Foreground {
      pgid: None,
      pid,
      name,
      argv,
      cwd,
    })
  }

  fn read_bytes(
    process: HANDLE,
    address: usize,
    len: usize,
  ) -> Option<Vec<u8>> {
    if address == 0 {
      return None;
    }
    let mut buf = vec![0u8; len];
    let mut read = 0usize;
    // SAFETY: `buf` is `len` bytes long; the kernel copies at most that.
    let ok = unsafe {
      ReadProcessMemory(
        process,
        address as *const c_void,
        buf.as_mut_ptr() as *mut c_void,
        len,
        &mut read,
      )
    };
    (ok != 0 && read == len).then_some(buf)
  }

  fn read_usize(process: HANDLE, address: usize) -> Option<usize> {
    let bytes = read_bytes(process, address, std::mem::size_of::<usize>())?;
    Some(usize::from_ne_bytes(bytes.try_into().ok()?))
  }

  fn read_unicode_string(process: HANDLE, address: usize) -> Option<String> {
    let bytes =
      read_bytes(process, address, std::mem::size_of::<UnicodeString>())?;
    // SAFETY: `bytes` is exactly one UnicodeString of plain data.
    let string: UnicodeString =
      unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const _) };
    let len = (string.length as usize).min(MAX_STRING_BYTES) & !1;
    if len == 0 {
      return None;
    }
    let raw = read_bytes(process, string.buffer, len)?;
    let wide: Vec<u16> = raw
      .as_chunks::<2>()
      .0
      .iter()
      .map(|pair| u16::from_ne_bytes(*pair))
      .collect();
    Some(String::from_utf16_lossy(&wide))
  }

  /// A process's command line (split as the C runtime splits it) and its
  /// current directory, read from its process environment block. `None`
  /// each when the process cannot be read (another user's, a protected
  /// process, one that exited meanwhile).
  fn read_parameters(process: HANDLE) -> (Option<Vec<String>>, Option<String>) {
    // SAFETY: zeroed plain data the kernel fills.
    let mut basic: BasicInformation = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is as long as stated.
    let status = unsafe {
      NtQueryInformationProcess(
        process,
        0,
        &mut basic as *mut _ as *mut c_void,
        std::mem::size_of::<BasicInformation>() as u32,
        std::ptr::null_mut(),
      )
    };
    if status < 0 || basic.peb == 0 {
      return (None, None);
    }
    let Some(params) = read_usize(process, basic.peb + PEB_PROCESS_PARAMETERS)
    else {
      return (None, None);
    };
    if params == 0 {
      return (None, None);
    }
    let argv = read_unicode_string(process, params + PARAMS_COMMAND_LINE)
      .and_then(|line| split_command_line(&line));
    let cwd = read_unicode_string(process, params + PARAMS_CURRENT_DIRECTORY)
      .map(|dir| trim_directory(&dir));
    (argv, cwd)
  }

  /// `C:\Users\me\` → `C:\Users\me`; a drive's root keeps its separator.
  pub(super) fn trim_directory(dir: &str) -> String {
    let trimmed = dir.trim_end_matches('\\');
    if trimmed.ends_with(':') || trimmed.is_empty() {
      format!("{trimmed}\\")
    } else {
      trimmed.to_string()
    }
  }

  pub(super) fn split_command_line(line: &str) -> Option<Vec<String>> {
    if line.trim().is_empty() {
      return None;
    }
    let wide: Vec<u16> = line.encode_utf16().chain(Some(0)).collect();
    let mut argc = 0i32;
    // SAFETY: a NUL-terminated string; the result is freed below.
    let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv.is_null() {
      return None;
    }
    let mut args = Vec::with_capacity(argc.max(0) as usize);
    for i in 0..argc.max(0) as usize {
      // SAFETY: `argv` holds `argc` NUL-terminated strings.
      let arg = unsafe {
        let ptr = *argv.add(i);
        let mut len = 0;
        while *ptr.add(len) != 0 {
          len += 1;
        }
        std::slice::from_raw_parts(ptr, len)
      };
      args.push(String::from_utf16_lossy(arg));
    }
    // SAFETY: allocated by CommandLineToArgvW with LocalAlloc.
    unsafe { LocalFree(argv as _) };
    Some(args)
  }
}

#[cfg(all(test, unix))]
mod tests {
  use std::process::Command;
  use std::process::Stdio;

  use super::unix_impl::*;

  /// A child that sleeps, killed and reaped when dropped.
  struct Sleeper(std::process::Child);

  impl Sleeper {
    fn start(dir: &std::path::Path) -> Sleeper {
      Sleeper(
        #[allow(clippy::disallowed_methods, reason = "a test's child")]
        Command::new("sleep")
          .arg("300")
          .current_dir(dir)
          .stdin(Stdio::null())
          .stdout(Stdio::null())
          .stderr(Stdio::null())
          .spawn()
          .unwrap(),
      )
    }

    fn pid(&self) -> libc::pid_t {
      self.0.id() as libc::pid_t
    }
  }

  impl Drop for Sleeper {
    fn drop(&mut self) {
      let _ = self.0.kill();
      let _ = self.0.wait();
    }
  }

  /// The child, once it has become `sleep` (between `fork` and `exec` it is
  /// still a copy of this test).
  fn info_after_exec(pid: libc::pid_t) -> super::ProcessInfo {
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
      if let Some(info) = process_info(pid)
        && info.name == "sleep"
        && info.argv.is_some()
      {
        return info;
      }
      assert!(
        std::time::Instant::now() < deadline,
        "the child never became sleep"
      );
      std::thread::sleep(std::time::Duration::from_millis(5));
    }
  }

  #[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  ))]
  #[test]
  fn a_child_is_described_by_name_arguments_and_directory() {
    #[allow(clippy::disallowed_methods, reason = "a test's directory")]
    let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let child = Sleeper::start(&dir);
    let info = info_after_exec(child.pid());
    assert_eq!(info.name, "sleep");
    assert_eq!(
      info.argv.as_deref(),
      Some(&["sleep".to_string(), "300".to_string()][..])
    );
    assert_eq!(info.cwd.as_deref(), dir.to_str());
  }

  #[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  ))]
  #[test]
  fn a_zombie_and_a_missing_process_are_not_described() {
    #[allow(clippy::disallowed_methods, reason = "a test's directory")]
    let dir = std::env::temp_dir();
    let mut child = Sleeper::start(&dir);
    let pid = child.pid();
    info_after_exec(pid);
    child.0.kill().unwrap();
    // killed and not yet reaped: a zombie
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(30);
    while process_info(pid).is_some() {
      assert!(std::time::Instant::now() < deadline, "still described");
      std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.0.wait().unwrap();
    assert_eq!(process_info(pid), None);
    assert_eq!(foreground_of_group(0), None);
    assert_eq!(foreground_of_group(-1), None);
  }

  #[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
  ))]
  #[test]
  fn a_group_whose_leader_is_gone_is_described_by_a_member() {
    use std::os::unix::process::CommandExt;
    #[allow(clippy::disallowed_methods, reason = "a test's directory")]
    let dir = std::env::temp_dir();
    // a leader of its own group, and a second process put in that group
    #[allow(clippy::disallowed_methods, reason = "a test's child")]
    let mut leader = Command::new("sleep")
      .arg("300")
      .current_dir(&dir)
      .stdin(Stdio::null())
      .process_group(0)
      .spawn()
      .unwrap();
    let pgrp = leader.id() as libc::pid_t;
    #[allow(clippy::disallowed_methods, reason = "a test's child")]
    let member = Sleeper(
      Command::new("sleep")
        .arg("301")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .process_group(pgrp)
        .spawn()
        .unwrap(),
    );
    info_after_exec(member.pid());

    let fg = foreground_of_group(pgrp).unwrap();
    assert_eq!((fg.pgid, fg.pid), (Some(pgrp), pgrp as u32));

    // the leader exits; while it is a zombie and after it is reaped, the
    // group is described by its other member
    leader.kill().unwrap();
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
      let fg = foreground_of_group(pgrp).unwrap();
      assert_eq!(fg.pgid, Some(pgrp));
      if fg.pid == member.pid() as u32 {
        assert_eq!(fg.name, "sleep");
        assert_eq!(
          fg.argv.as_deref(),
          Some(&["sleep".to_string(), "301".to_string()][..])
        );
        break;
      }
      assert!(
        std::time::Instant::now() < deadline,
        "leader still described"
      );
      std::thread::sleep(std::time::Duration::from_millis(5));
    }
    leader.wait().unwrap();
    assert_eq!(
      foreground_of_group(pgrp).map(|fg| fg.pid),
      Some(member.pid() as u32)
    );
    drop(member);
    // nobody left
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(30);
    while foreground_of_group(pgrp).is_some() {
      assert!(
        std::time::Instant::now() < deadline,
        "group still described"
      );
      std::thread::sleep(std::time::Duration::from_millis(5));
    }
  }

  #[test]
  fn nul_separated_arguments() {
    assert_eq!(split_nul_args(b""), None);
    assert_eq!(split_nul_args(b"\0"), None);
    assert_eq!(
      split_nul_args(b"vim\0-u\0\0file name\0"),
      Some(vec![
        "vim".to_string(),
        "-u".to_string(),
        String::new(),
        "file name".to_string()
      ])
    );
    // no trailing NUL (a process that rewrote its title)
    assert_eq!(
      split_nul_args(b"nginx: worker"),
      Some(vec!["nginx: worker".to_string()])
    );
  }

  #[test]
  fn stat_names_may_hold_parentheses_and_spaces() {
    assert_eq!(
      parse_stat(b"42 (a) b (c)) S 1 77 77 0 -1"),
      Some(("a) b (c)".to_string(), b'S', 77))
    );
    assert_eq!(
      parse_stat(b"7 (sh) Z 1 7 7"),
      Some(("sh".to_string(), b'Z', 7))
    );
    assert_eq!(parse_stat(b"7 sh S 1 7 7"), None);
    assert_eq!(parse_stat(b"7 (sh) S 1"), None);
    assert_eq!(parse_stat(b"7 () S 1 7 7"), None);
    assert_eq!(parse_stat(b""), None);
  }

  #[test]
  fn procargs2_layout() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&2i32.to_ne_bytes());
    buf.extend_from_slice(b"/bin/sleep\0\0\0\0\0\0");
    buf.extend_from_slice(b"sleep\0");
    buf.extend_from_slice(b"300\0");
    buf.extend_from_slice(b"SECRET=1\0");
    assert_eq!(
      parse_procargs2(&buf),
      Some(vec!["sleep".to_string(), "300".to_string()])
    );
    // an empty argument is kept; the environment is never read
    let mut buf = Vec::new();
    buf.extend_from_slice(&2i32.to_ne_bytes());
    buf.extend_from_slice(b"/bin/x\0\0x\0\0ENV=1\0");
    assert_eq!(
      parse_procargs2(&buf),
      Some(vec!["x".to_string(), String::new()])
    );
    // truncated, or claiming more arguments than there are
    assert_eq!(parse_procargs2(&[]), None);
    assert_eq!(parse_procargs2(&0i32.to_ne_bytes()), None);
    let mut buf = Vec::new();
    buf.extend_from_slice(&3i32.to_ne_bytes());
    buf.extend_from_slice(b"/bin/x\0\0x\0y");
    assert_eq!(parse_procargs2(&buf), None);
    let mut buf = Vec::new();
    buf.extend_from_slice(&1i32.to_ne_bytes());
    buf.extend_from_slice(b"/bin/x\0\0\0");
    assert_eq!(parse_procargs2(&buf), None);
  }
}

#[cfg(all(test, windows))]
mod tests {
  use std::os::windows::io::AsRawHandle;
  use std::process::Command;
  use std::process::Stdio;

  use super::windows_impl::*;

  #[test]
  fn directories_and_command_lines() {
    assert_eq!(trim_directory("C:\\Users\\me\\"), "C:\\Users\\me");
    assert_eq!(trim_directory("C:\\"), "C:\\");
    assert_eq!(trim_directory("C:\\Users\\me"), "C:\\Users\\me");
    assert_eq!(split_command_line("   "), None);
    assert_eq!(
      split_command_line("\"C:\\Program Files\\x.exe\" -a \"b c\" d"),
      Some(vec![
        "C:\\Program Files\\x.exe".to_string(),
        "-a".to_string(),
        "b c".to_string(),
        "d".to_string()
      ])
    );
  }

  /// `cmd /c ping` as a terminal's shell would run it: the foreground
  /// process is the ping, described with its arguments and directory.
  #[test]
  fn a_shell_s_newest_child_is_the_foreground_process() {
    #[allow(clippy::disallowed_methods, reason = "a test's directory")]
    let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    #[allow(clippy::disallowed_methods, reason = "a test's child")]
    let mut shell = Command::new("cmd.exe")
      .args(["/d", "/c", "ping -n 300 127.0.0.1"])
      .current_dir(&dir)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .unwrap();
    let handle = shell.as_raw_handle() as _;
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(60);
    let fg = loop {
      let fg = foreground_of_tree(shell.id(), handle).unwrap();
      if fg.name.eq_ignore_ascii_case("ping.exe") && fg.argv.is_some() {
        break fg;
      }
      assert!(std::time::Instant::now() < deadline, "no ping: {fg:?}");
      std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(fg.pgid, None);
    assert_ne!(fg.pid, shell.id());
    let argv = fg.argv.clone().unwrap();
    assert!(argv[0].to_ascii_lowercase().starts_with("ping"), "{argv:?}");
    assert_eq!(&argv[1..], ["-n", "300", "127.0.0.1"]);
    // \\?\C:\… from canonicalize against the plain path the process holds
    let want = dir
      .to_string_lossy()
      .trim_start_matches("\\\\?\\")
      .to_string();
    assert!(
      fg.cwd.as_deref().unwrap().eq_ignore_ascii_case(&want),
      "{:?} vs {want}",
      fg.cwd
    );

    // the ping killed: the shell ends too, and nothing is described
    #[allow(clippy::disallowed_methods, reason = "a test's child")]
    let _ = Command::new("taskkill")
      .args(["/f", "/pid", &fg.pid.to_string()])
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .status();
    let _ = shell.kill();
    shell.wait().unwrap();
    let deadline =
      std::time::Instant::now() + std::time::Duration::from_secs(60);
    while let Some(fg) = foreground_of_tree(shell.id(), handle) {
      assert!(std::time::Instant::now() < deadline, "still: {fg:?}");
      std::thread::sleep(std::time::Duration::from_millis(20));
    }
  }

  /// A program that is not a shell is the answer itself, whatever it
  /// started.
  #[test]
  fn a_program_that_is_not_a_shell_is_not_descended() {
    #[allow(clippy::disallowed_methods, reason = "a test's child")]
    let mut ping = Command::new("ping.exe")
      .args(["-n", "300", "127.0.0.1"])
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .unwrap();
    let handle = ping.as_raw_handle() as _;
    let fg = foreground_of_tree(ping.id(), handle).unwrap();
    assert_eq!(fg.pid, ping.id());
    assert!(fg.name.eq_ignore_ascii_case("ping.exe"));
    assert_eq!(&fg.argv.unwrap()[1..], ["-n", "300", "127.0.0.1"]);
    ping.kill().unwrap();
    ping.wait().unwrap();
    assert_eq!(foreground_of_tree(ping.id(), handle), None);
  }
}
