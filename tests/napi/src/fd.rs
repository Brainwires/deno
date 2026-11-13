// Copyright 2018-2026 the Deno authors. MIT license.

//! A descriptor the addon opens itself, as node-pty's `fork` opens its pty
//! master, for node:fs to read and close (denext, DENEXT.md item 370).

use std::ptr;

use napi_sys::*;

use crate::assert_napi_ok;
use crate::napi_new_property;

#[cfg(unix)]
unsafe extern "C" {
  fn pipe(fds: *mut std::ffi::c_int) -> std::ffi::c_int;
  fn fcntl(fd: std::ffi::c_int, cmd: std::ffi::c_int, ...) -> std::ffi::c_int;
}

#[cfg(unix)]
const F_SETFL: i32 = 4;
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(target_os = "linux")))]
const O_NONBLOCK: i32 = 0x4;

/// `openPipe(text)`: a pipe the addon creates, with `text` written to it and
/// a non-blocking read end (a read with the write end still open is `EAGAIN`,
/// not a hang); returns `[readFd, writeFd]` (`undefined` on Windows).
extern "C" fn open_pipe(env: napi_env, info: napi_callback_info) -> napi_value {
  let (args, argc, _) = crate::napi_get_callback_info!(env, info, 1);
  assert_eq!(argc, 1);
  let mut result: napi_value = ptr::null_mut();
  #[cfg(unix)]
  {
    let mut len = 0usize;
    assert_napi_ok!(napi_get_value_string_utf8(
      env,
      args[0],
      ptr::null_mut(),
      0,
      &mut len
    ));
    let mut buf = vec![0u8; len + 1];
    assert_napi_ok!(napi_get_value_string_utf8(
      env,
      args[0],
      buf.as_mut_ptr() as *mut _,
      buf.len(),
      &mut len
    ));
    let mut fds: [std::ffi::c_int; 2] = [-1; 2];
    assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0);
    {
      use std::io::Write;
      use std::os::fd::FromRawFd;
      use std::os::fd::IntoRawFd;
      // SAFETY: fds[1] is the pipe's write end, just created; the file is
      // turned back into the raw descriptor below, never closed here.
      let mut file = unsafe { std::fs::File::from_raw_fd(fds[1]) };
      file.write_all(&buf[..len]).unwrap();
      let _ = file.into_raw_fd();
    }
    assert_eq!(unsafe { fcntl(fds[0], F_SETFL, O_NONBLOCK) }, 0);
    assert_napi_ok!(napi_create_array_with_length(env, 2, &mut result));
    for (i, fd) in fds.iter().enumerate() {
      let mut value: napi_value = ptr::null_mut();
      assert_napi_ok!(napi_create_int32(env, *fd, &mut value));
      assert_napi_ok!(napi_set_element(env, result, i as u32, value));
    }
  }
  #[cfg(not(unix))]
  {
    let _ = args;
    assert_napi_ok!(napi_get_undefined(env, &mut result));
  }
  result
}

pub fn init(env: napi_env, exports: napi_value) {
  let properties = &[napi_new_property!(env, "openPipe", open_pipe)];

  assert_napi_ok!(napi_define_properties(
    env,
    exports,
    properties.len(),
    properties.as_ptr()
  ));
}
