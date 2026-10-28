// Copyright 2018-2026 the Deno authors. MIT license.

use std::ffi::OsStr;
use std::io;
use std::os::windows::io::IntoRawHandle;

use crate::Command;

#[test]
fn spawn_rejects_nuls_in_arguments() {
  for verbatim_arguments in [false, true] {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
      .arg(OsStr::new("invalid\0argument"))
      .arg("must-not-be-dropped")
      .verbatim_arguments(verbatim_arguments);

    let error = command.spawn().err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(error.to_string(), "nul byte found in provided data");
  }
}

/// A handle passed with `inherit_handle` is inheritable for its own spawn
/// only: afterwards a spawn with no handle list (Rust's `Command`) must not
/// pick it up, so its inherit flag is back to what it was.
#[test]
fn inherit_handle_is_inheritable_only_during_its_spawn() {
  use windows_sys::Win32::Foundation::CloseHandle;
  use windows_sys::Win32::Foundation::GetHandleInformation;
  use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;

  // Both ends are created non-inheritable.
  let pipes = crate::anon_pipe::anon_pipe(true, false).unwrap();
  let handle = pipes.theirs.into_handle().into_raw_handle();

  let mut command = Command::new(comspec());
  command
    .args(["/d", "/c", "exit 0"])
    .stdin(crate::Stdio::Null)
    .stdout(crate::Stdio::Null)
    .stderr(crate::Stdio::Null)
    .inherit_handle(handle);
  let status = command.spawn().unwrap().wait_blocking().unwrap();
  assert!(status.success());

  let mut flags = 0u32;
  // SAFETY: `handle` is open until the CloseHandle below.
  assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
  assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "still inheritable");
  // SAFETY: we own `handle`.
  unsafe { CloseHandle(handle) };
}

/// Children spawned through this crate, each with piped stdout, must see
/// end-of-file when they exit even while other threads keep starting
/// long-lived programs through Rust's `std::process::Command`, which inherits
/// every inheritable handle in the process. Run through
/// `with_exclusive_inheritance`, such a spawn never overlaps the window in
/// which this crate holds a child's inheritable pipe ends; without it, a
/// `ping` that picked up one kept that spawn's stdout open for its 30 s life.
#[test]
fn std_spawns_take_no_pipe_ends_meant_for_another_child() {
  use std::sync::Arc;
  use std::sync::atomic::AtomicBool;
  use std::sync::atomic::Ordering;
  use std::time::Duration;
  use std::time::Instant;

  const READERS: usize = 4;
  const SPAWNS_PER_READER: usize = 25;
  const HOLDERS: usize = 2;

  let done = Arc::new(AtomicBool::new(false));
  let holders = (0..HOLDERS)
    .map(|_| {
      let done = done.clone();
      std::thread::spawn(move || {
        let mut children = Vec::new();
        while !done.load(Ordering::Relaxed) && children.len() < 40 {
          let child = crate::with_exclusive_inheritance(|| {
            std::process::Command::new("ping")
              .args(["-n", "31", "127.0.0.1"])
              .stdin(std::process::Stdio::null())
              .stdout(std::process::Stdio::null())
              .stderr(std::process::Stdio::null())
              .spawn()
          })
          .unwrap();
          children.push(child);
          std::thread::sleep(Duration::from_millis(20));
        }
        children
      })
    })
    .collect::<Vec<_>>();

  let readers = (0..READERS)
    .map(|_| {
      std::thread::spawn(|| {
        let mut slowest = Duration::ZERO;
        for _ in 0..SPAWNS_PER_READER {
          let start = Instant::now();
          let mut command = Command::new(comspec());
          command
            .args(["/d", "/c", "echo ok"])
            .stdin(crate::Stdio::Null)
            .stdout(crate::Stdio::Pipe)
            .stderr(crate::Stdio::Null);
          let output = command.spawn().unwrap().wait_with_output().unwrap();
          assert!(output.status.success());
          assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
          slowest = slowest.max(start.elapsed());
        }
        slowest
      })
    })
    .collect::<Vec<_>>();

  let slowest = readers
    .into_iter()
    .map(|reader| reader.join().unwrap())
    .max()
    .unwrap();
  done.store(true, Ordering::Relaxed);
  for holder in holders {
    for mut child in holder.join().unwrap() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
  assert!(
    slowest < Duration::from_secs(15),
    "a piped child's output took {slowest:?}: its stdout stayed open in a \
     concurrently spawned long-lived process"
  );
}

fn comspec() -> std::ffi::OsString {
  std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into())
}
