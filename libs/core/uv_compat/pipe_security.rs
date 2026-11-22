// Copyright 2018-2026 the Deno authors. MIT license.

//! The security descriptor of a listening Windows named pipe (DENEXT.md
//! item 484).
//!
//! A pipe created with no security attributes takes the default descriptor,
//! which lets every local account open it for reading. A `node:net` pipe
//! server is the local IPC of a Windows program, so its pipes are created
//! with an explicit, protected DACL instead: the account the process runs as
//! and LocalSystem, nobody else. `readableAll` / `writableAll`
//! (`uv_pipe_chmod`) add an ACE for Everyone on request.

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TOKEN_USER;
use windows_sys::Win32::Security::TokenUser;
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;

/// `readableAll`: Everyone may open the pipe for reading.
pub(crate) const EVERYONE_READ: u8 = 1;
/// `writableAll`: Everyone may open the pipe for writing.
pub(crate) const EVERYONE_WRITE: u8 = 2;

/// `SDDL_REVISION_1`.
const SDDL_REVISION: u32 = 1;

/// What the process's account and LocalSystem get: everything, which
/// includes creating further instances of the pipe and changing its DACL.
pub(crate) const OWNER_ACCESS: u32 = FILE_ALL_ACCESS;

/// The access mask of the Everyone ACE for `everyone` (a combination of
/// [`EVERYONE_READ`] and [`EVERYONE_WRITE`]), or `None` when there is no
/// such ACE. These are libuv's `uv_pipe_chmod` masks with the generic rights
/// spelled out: a read-only client also sets the pipe's read mode
/// (`FILE_WRITE_ATTRIBUTES`), a write-only one also reads its state
/// (`FILE_READ_ATTRIBUTES`).
pub(crate) fn everyone_access(everyone: u8) -> Option<u32> {
  let mut mask = 0;
  if everyone & EVERYONE_READ != 0 {
    mask |= FILE_GENERIC_READ | FILE_WRITE_ATTRIBUTES;
  }
  if everyone & EVERYONE_WRITE != 0 {
    mask |= FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES;
  }
  (mask != 0).then_some(mask)
}

/// The descriptor of a listening pipe, in SDDL: owned by `user_sid`, with a
/// protected DACL (no inherited ACEs) that allows [`OWNER_ACCESS`] to
/// `user_sid` and LocalSystem (`SY`), and [`everyone_access`] to Everyone
/// (`WD`) when asked.
pub(crate) fn pipe_sddl(user_sid: &str, everyone: u8) -> String {
  let mut sddl = format!(
    "O:{user_sid}D:P(A;;{OWNER_ACCESS:#x};;;{user_sid})(A;;{OWNER_ACCESS:#x};;;SY)"
  );
  if let Some(mask) = everyone_access(everyone) {
    sddl.push_str(&format!("(A;;{mask:#x};;;WD)"));
  }
  sddl
}

/// Copies a NUL-terminated UTF-16 string.
///
/// # Safety
/// `ptr` must point at a NUL-terminated UTF-16 string.
pub(crate) unsafe fn wide_to_string(ptr: *const u16) -> String {
  unsafe {
    let mut len = 0;
    while *ptr.add(len) != 0 {
      len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
  }
}

/// A SID as a string (`S-1-5-21-...`).
///
/// # Safety
/// `sid` must point at a valid SID.
pub(crate) unsafe fn sid_to_string(sid: *mut c_void) -> io::Result<String> {
  unsafe {
    let mut wide: *mut u16 = std::ptr::null_mut();
    if ConvertSidToStringSidW(sid, &mut wide) == 0 {
      return Err(io::Error::last_os_error());
    }
    let string = wide_to_string(wide);
    LocalFree(wide as _);
    Ok(string)
  }
}

/// Closes a token handle on drop.
struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
  fn drop(&mut self) {
    // SAFETY: the handle came from `OpenProcessToken` and is closed once.
    unsafe {
      CloseHandle(self.0);
    }
  }
}

/// The SID of the account the process runs as (its primary token's user).
pub(crate) fn current_user_sid() -> io::Result<String> {
  // SAFETY: plain Win32 calls; every buffer is sized by the call before it
  // and outlives its use.
  unsafe {
    let mut token: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
      return Err(io::Error::last_os_error());
    }
    let token = TokenHandle(token);

    let mut len = 0u32;
    if GetTokenInformation(
      token.0,
      TokenUser,
      std::ptr::null_mut(),
      0,
      &mut len,
    ) == 0
    {
      let err = io::Error::last_os_error();
      if err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(err);
      }
    }
    // u64 elements, so the buffer is aligned for TOKEN_USER.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    if GetTokenInformation(
      token.0,
      TokenUser,
      buf.as_mut_ptr() as *mut c_void,
      (buf.len() * 8) as u32,
      &mut len,
    ) == 0
    {
      return Err(io::Error::last_os_error());
    }
    let user = &*(buf.as_ptr() as *const TOKEN_USER);
    sid_to_string(user.User.Sid)
  }
}

/// A self-relative security descriptor built from SDDL, freed on drop, with
/// the `SECURITY_ATTRIBUTES` that hand it to `CreateNamedPipeW`.
pub(crate) struct PipeSecurity {
  descriptor: *mut c_void,
  attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
  /// The descriptor of a pipe this process listens on; see [`pipe_sddl`].
  pub(crate) fn for_current_user(everyone: u8) -> io::Result<Self> {
    Self::from_sddl(&pipe_sddl(&current_user_sid()?, everyone))
  }

  pub(crate) fn from_sddl(sddl: &str) -> io::Result<Self> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; on success `descriptor` is a
    // LocalAlloc'd descriptor that `Drop` frees.
    let ok = unsafe {
      ConvertStringSecurityDescriptorToSecurityDescriptorW(
        wide.as_ptr(),
        SDDL_REVISION,
        &mut descriptor,
        std::ptr::null_mut(),
      )
    };
    if ok == 0 {
      return Err(io::Error::last_os_error());
    }
    Ok(Self {
      descriptor,
      attributes: SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        // The pipe handle is never inherited by a child process.
        bInheritHandle: 0,
      },
    })
  }

  /// The descriptor (valid while `self` lives).
  #[cfg(test)]
  pub(crate) fn descriptor(&self) -> *mut c_void {
    self.descriptor
  }

  /// A `*mut SECURITY_ATTRIBUTES` for `CreateNamedPipeW` (valid while `self`
  /// lives and is not moved).
  pub(crate) fn attributes(&mut self) -> *mut c_void {
    &mut self.attributes as *mut SECURITY_ATTRIBUTES as *mut c_void
  }

  /// The descriptor's DACL (valid while `self` lives).
  fn dacl(&self) -> io::Result<*mut ACL> {
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `descriptor` is a valid descriptor.
    let ok = unsafe {
      GetSecurityDescriptorDacl(
        self.descriptor,
        &mut present,
        &mut dacl,
        &mut defaulted,
      )
    };
    if ok == 0 {
      return Err(io::Error::last_os_error());
    }
    if present == 0 || dacl.is_null() {
      // A missing DACL would mean "everyone, everything": never apply one.
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "the pipe's security descriptor has no DACL",
      ));
    }
    Ok(dacl)
  }
}

impl Drop for PipeSecurity {
  fn drop(&mut self) {
    // SAFETY: `descriptor` was allocated by
    // `ConvertStringSecurityDescriptorToSecurityDescriptorW` and is freed
    // once.
    unsafe {
      LocalFree(self.descriptor as _);
    }
  }
}

/// Replaces the DACL of the pipe `handle` is an instance of with the one
/// [`pipe_sddl`] describes for `everyone`. The handle needs `WRITE_DAC`. A
/// pipe's instances share one descriptor, so this covers the instances
/// created later as well.
///
/// # Safety
/// `handle` must be an open pipe handle.
pub(crate) unsafe fn set_pipe_dacl(
  handle: HANDLE,
  everyone: u8,
) -> io::Result<()> {
  let security = PipeSecurity::for_current_user(everyone)?;
  let dacl = security.dacl()?;
  // SAFETY: `handle` is open (caller) and `dacl` lives as long as
  // `security`.
  let status = unsafe {
    SetSecurityInfo(
      handle,
      SE_KERNEL_OBJECT,
      DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
      std::ptr::null_mut(),
      std::ptr::null_mut(),
      dacl,
      std::ptr::null(),
    )
  };
  if status != 0 {
    return Err(io::Error::from_raw_os_error(status as i32));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::os::windows::io::AsRawHandle;

  use windows_sys::Win32::Security::ACCESS_ALLOWED_ACE;
  use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
  use windows_sys::Win32::Security::GetAce;
  use windows_sys::Win32::Security::GetSecurityDescriptorControl;
  use windows_sys::Win32::Security::GetSecurityDescriptorOwner;
  use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
  use windows_sys::Win32::Security::SE_DACL_PROTECTED;

  use super::*;

  const EVERYONE_SID: &str = "S-1-1-0";
  const SYSTEM_SID: &str = "S-1-5-18";
  /// `ACCESS_ALLOWED_ACE_TYPE`.
  const ALLOWED: u8 = 0;

  /// What a descriptor says: its owner, whether its DACL is protected, and
  /// each ACE as (type, flags, SID, access mask), in order.
  #[derive(Debug, PartialEq)]
  struct Described {
    owner: String,
    protected: bool,
    aces: Vec<(u8, u8, String, u32)>,
  }

  /// # Safety
  /// `descriptor` must be a valid security descriptor with an owner and a
  /// DACL of access-allowed ACEs.
  unsafe fn describe(descriptor: *mut c_void) -> Described {
    unsafe {
      let mut owner: *mut c_void = std::ptr::null_mut();
      let mut defaulted = 0;
      assert_ne!(
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted),
        0
      );
      assert!(!owner.is_null(), "the descriptor has no owner");

      let mut control = 0u16;
      let mut revision = 0u32;
      assert_ne!(
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
        0
      );

      let mut present = 0;
      let mut dacl: *mut ACL = std::ptr::null_mut();
      assert_ne!(
        GetSecurityDescriptorDacl(
          descriptor,
          &mut present,
          &mut dacl,
          &mut defaulted
        ),
        0
      );
      assert_ne!(present, 0, "the descriptor has no DACL");
      assert!(!dacl.is_null(), "the descriptor has a NULL DACL");

      let mut aces = Vec::new();
      for index in 0..(*dacl).AceCount as u32 {
        let mut ace: *mut c_void = std::ptr::null_mut();
        assert_ne!(GetAce(dacl, index, &mut ace), 0);
        let ace = ace as *mut ACCESS_ALLOWED_ACE;
        let kind = (*ace).Header.AceType;
        assert_eq!(kind, ALLOWED, "only access-allowed ACEs are expected");
        let sid = std::ptr::addr_of_mut!((*ace).SidStart) as *mut c_void;
        aces.push((
          kind,
          (*ace).Header.AceFlags,
          sid_to_string(sid).unwrap(),
          (*ace).Mask,
        ));
      }
      Described {
        owner: sid_to_string(owner).unwrap(),
        protected: control & SE_DACL_PROTECTED != 0,
        aces,
      }
    }
  }

  /// The descriptor of the live pipe behind `handle`.
  fn describe_pipe(handle: HANDLE) -> Described {
    // SAFETY: `handle` is an open pipe handle with READ_CONTROL; the
    // descriptor GetSecurityInfo allocates is freed below.
    unsafe {
      let mut descriptor: *mut c_void = std::ptr::null_mut();
      let status = GetSecurityInfo(
        handle,
        SE_KERNEL_OBJECT,
        OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut descriptor,
      );
      assert_eq!(status, 0, "GetSecurityInfo failed");
      let described = describe(descriptor);
      LocalFree(descriptor as _);
      described
    }
  }

  fn expected(user: &str, everyone: Option<u32>) -> Described {
    let mut aces = vec![
      (ALLOWED, 0, user.to_string(), OWNER_ACCESS),
      (ALLOWED, 0, SYSTEM_SID.to_string(), OWNER_ACCESS),
    ];
    if let Some(mask) = everyone {
      aces.push((ALLOWED, 0, EVERYONE_SID.to_string(), mask));
    }
    Described {
      owner: user.to_string(),
      protected: true,
      aces,
    }
  }

  fn pipe_name() -> String {
    static NEXT: std::sync::atomic::AtomicU32 =
      std::sync::atomic::AtomicU32::new(0);
    format!(
      r"\\.\pipe\deno-core-pipe-security-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
  }

  /// The first instance of a pipe, created as `uv_pipe_listen` creates it.
  fn listen(
    name: &str,
    everyone: u8,
  ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut security = PipeSecurity::for_current_user(everyone)?;
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    opts.first_pipe_instance(true);
    opts.reject_remote_clients(true);
    opts.write_dac(true);
    // SAFETY: the attributes are valid for the call.
    unsafe {
      opts.create_with_security_attributes_raw(name, security.attributes())
    }
  }

  #[test]
  fn sddl_names_the_user_and_system_only() {
    assert_eq!(
      pipe_sddl("S-1-5-21-1-2-3-1001", 0),
      "O:S-1-5-21-1-2-3-1001D:P(A;;0x1f01ff;;;S-1-5-21-1-2-3-1001)(A;;0x1f01ff;;;SY)"
    );
  }

  #[test]
  fn sddl_adds_everyone_on_request() {
    let base = pipe_sddl("S-1-5-21-1-2-3-1001", 0);
    assert_eq!(
      pipe_sddl("S-1-5-21-1-2-3-1001", EVERYONE_READ),
      format!("{base}(A;;0x120189;;;WD)")
    );
    assert_eq!(
      pipe_sddl("S-1-5-21-1-2-3-1001", EVERYONE_WRITE),
      format!("{base}(A;;0x120196;;;WD)")
    );
    assert_eq!(
      pipe_sddl("S-1-5-21-1-2-3-1001", EVERYONE_READ | EVERYONE_WRITE),
      format!("{base}(A;;0x12019f;;;WD)")
    );
  }

  #[test]
  fn current_user_sid_is_a_sid() {
    let sid = current_user_sid().unwrap();
    assert!(sid.starts_with("S-1-"), "{sid}");
    assert_ne!(sid, EVERYONE_SID);
  }

  #[test]
  fn descriptor_holds_exactly_the_user_and_system() {
    let user = current_user_sid().unwrap();
    let security = PipeSecurity::for_current_user(0).unwrap();
    // SAFETY: the descriptor is valid while `security` lives.
    let described = unsafe { describe(security.descriptor()) };
    assert_eq!(described, expected(&user, None));
  }

  #[test]
  fn descriptor_adds_the_everyone_ace_on_request() {
    let user = current_user_sid().unwrap();
    for everyone in [
      EVERYONE_READ,
      EVERYONE_WRITE,
      EVERYONE_READ | EVERYONE_WRITE,
    ] {
      let security = PipeSecurity::for_current_user(everyone).unwrap();
      // SAFETY: the descriptor is valid while `security` lives.
      let described = unsafe { describe(security.descriptor()) };
      assert_eq!(described, expected(&user, everyone_access(everyone)));
    }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_listening_pipe_carries_the_descriptor() {
    let user = current_user_sid().unwrap();
    let name = pipe_name();
    let server = listen(&name, 0).unwrap();
    assert_eq!(
      describe_pipe(server.as_raw_handle() as HANDLE),
      expected(&user, None)
    );
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_second_first_instance_is_refused() {
    let name = pipe_name();
    let _server = listen(&name, 0).unwrap();
    let err = listen(&name, 0).unwrap_err();
    // ERROR_ACCESS_DENIED, which `uv_pipe_listen` reports as EADDRINUSE.
    assert_eq!(err.raw_os_error(), Some(5));
  }

  #[tokio::test(flavor = "current_thread")]
  async fn chmod_adds_and_removes_the_everyone_ace() {
    let user = current_user_sid().unwrap();
    let name = pipe_name();
    let server = listen(&name, 0).unwrap();
    let handle = server.as_raw_handle() as HANDLE;
    for everyone in [
      EVERYONE_READ,
      EVERYONE_WRITE,
      EVERYONE_READ | EVERYONE_WRITE,
      0,
    ] {
      // SAFETY: `handle` is the open server instance.
      unsafe { set_pipe_dacl(handle, everyone) }.unwrap();
      assert_eq!(
        describe_pipe(handle),
        expected(&user, everyone_access(everyone))
      );
    }
  }

  #[tokio::test(flavor = "current_thread")]
  async fn a_later_instance_shares_the_first_ones_descriptor() {
    let user = current_user_sid().unwrap();
    let name = pipe_name();
    let first = listen(&name, EVERYONE_READ).unwrap();
    let mut security = PipeSecurity::for_current_user(EVERYONE_READ).unwrap();
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    opts.first_pipe_instance(false);
    opts.reject_remote_clients(true);
    opts.write_dac(true);
    // SAFETY: the attributes are valid for the call.
    let second = unsafe {
      opts.create_with_security_attributes_raw(&name, security.attributes())
    }
    .unwrap();
    let want = expected(&user, everyone_access(EVERYONE_READ));
    assert_eq!(describe_pipe(first.as_raw_handle() as HANDLE), want);
    assert_eq!(describe_pipe(second.as_raw_handle() as HANDLE), want);
  }
}
