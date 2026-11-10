// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the native sections of `process.report` (Node's diagnostic
//! report): `userLimits` (getrlimit) and `sharedObjects` (the libraries
//! loaded into the process).

use deno_core::OpState;
use deno_core::op2;
use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_permissions::PermissionCheckError;
use deno_permissions::PermissionsContainer;

/// The report's `userLimits` section, Node's `PrintSystemInformation`:
/// `{ <name>: { soft, hard } }` with `"unlimited"` for `RLIM_INFINITY`.
#[cfg(unix)]
fn user_limits() -> serde_json::Value {
  #[cfg(any(target_os = "linux", target_os = "android"))]
  type Resource = libc::__rlimit_resource_t;
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  type Resource = libc::c_int;

  let limits: &[(&str, Resource)] = &[
    ("core_file_size_blocks", libc::RLIMIT_CORE as Resource),
    ("data_seg_size_bytes", libc::RLIMIT_DATA as Resource),
    ("file_size_blocks", libc::RLIMIT_FSIZE as Resource),
    ("max_locked_memory_bytes", libc::RLIMIT_MEMLOCK as Resource),
    ("max_memory_size_bytes", libc::RLIMIT_RSS as Resource),
    ("open_files", libc::RLIMIT_NOFILE as Resource),
    ("stack_size_bytes", libc::RLIMIT_STACK as Resource),
    ("cpu_time_seconds", libc::RLIMIT_CPU as Resource),
    ("max_user_processes", libc::RLIMIT_NPROC as Resource),
    ("virtual_memory_bytes", libc::RLIMIT_AS as Resource),
  ];
  let value = |v: libc::rlim_t| {
    if v == libc::RLIM_INFINITY {
      json!("unlimited")
    } else {
      json!(v)
    }
  };
  let mut out = serde_json::Map::new();
  for (name, resource) in limits {
    let mut limit = libc::rlimit {
      rlim_cur: 0,
      rlim_max: 0,
    };
    // SAFETY: getrlimit only writes to `limit`.
    if unsafe { libc::getrlimit(*resource, &mut limit) } == 0 {
      out.insert(
        (*name).to_string(),
        json!({ "soft": value(limit.rlim_cur), "hard": value(limit.rlim_max) }),
      );
    }
  }
  serde_json::Value::Object(out)
}

/// Node omits `userLimits` on Windows.
#[cfg(not(unix))]
fn user_limits() -> serde_json::Value {
  serde_json::Value::Null
}

#[cfg(target_os = "macos")]
fn shared_objects() -> Vec<String> {
  unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(image_index: u32) -> *const libc::c_char;
  }
  let mut out = Vec::new();
  // SAFETY: dyld's image list is safe to read from any thread; a name is a
  // NUL-terminated string owned by dyld (or null for an index removed while
  // iterating).
  unsafe {
    for i in 0.._dyld_image_count() {
      let name = _dyld_get_image_name(i);
      if !name.is_null() {
        out.push(
          std::ffi::CStr::from_ptr(name)
            .to_string_lossy()
            .into_owned(),
        );
      }
    }
  }
  out
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn shared_objects() -> Vec<String> {
  unsafe extern "C" fn callback(
    info: *mut libc::dl_phdr_info,
    _size: libc::size_t,
    data: *mut libc::c_void,
  ) -> libc::c_int {
    // SAFETY: `data` is the `Vec` passed to dl_iterate_phdr below, and
    // `info` is valid for the duration of the callback.
    unsafe {
      let out = &mut *(data as *mut Vec<String>);
      let name = (*info).dlpi_name;
      if !name.is_null() {
        let name = std::ffi::CStr::from_ptr(name).to_string_lossy();
        if !name.is_empty() {
          out.push(name.into_owned());
        }
      }
    }
    0
  }
  let mut out: Vec<String> = Vec::new();
  // SAFETY: the callback only touches `out`, which outlives the call.
  unsafe {
    libc::dl_iterate_phdr(
      Some(callback),
      &mut out as *mut Vec<String> as *mut libc::c_void,
    );
  }
  out
}

/// Other platforms (Windows included) report an empty list for now.
#[cfg(not(any(
  target_os = "macos",
  target_os = "linux",
  target_os = "android"
)))]
fn shared_objects() -> Vec<String> {
  Vec::new()
}

/// `{ userLimits, sharedObjects }` as JSON. Like the report's header (the
/// OS release, the CPUs, the network interfaces), this is system
/// information, gated on the `osRelease` sys permission.
#[op2(stack_trace)]
#[string]
pub fn op_node_report_native_info(
  state: &mut OpState,
) -> Result<String, PermissionCheckError> {
  state
    .borrow_mut::<PermissionsContainer>()
    .check_sys("osRelease", "process.report.getReport()")?;
  let mut info = serde_json::Map::new();
  let limits = user_limits();
  if !limits.is_null() {
    info.insert("userLimits".to_string(), limits);
  }
  info.insert("sharedObjects".to_string(), json!(shared_objects()));
  Ok(serde_json::Value::Object(info).to_string())
}
