// Copyright 2018-2026 the Deno authors. MIT license.

//! A `JsRuntime` must be created inside a tokio runtime context: V8's
//! delayed foreground tasks are scheduled on it, and without one deno_core
//! aborts the process when V8 posts the first (`setup::spawn_delayed_task`).
//! When that happens depends on the GC, so a test that broke the rule used to
//! pass or die with a bare SIGABRT at random.

use crate::JsRuntime;
use crate::RuntimeOptions;
use crate::runtime::setup;
use crate::v8;

/// Makes V8 post a delayed foreground task now (the same task the GC posts
/// on its own under load).
fn post_a_delayed_v8_task(runtime: &mut JsRuntime) {
  runtime
    .v8_isolate()
    .memory_pressure_notification(v8::MemoryPressureLevel::Critical);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(
  expected = "A `JsRuntime` was created outside of a tokio runtime context"
)]
fn a_js_runtime_outside_tokio_fails_at_creation() {
  JsRuntime::new(RuntimeOptions::default());
}

#[test]
fn a_js_runtime_in_a_tokio_context_survives_a_delayed_task() {
  let tokio = tokio::runtime::Builder::new_current_thread()
    .enable_time()
    .build()
    .unwrap();
  let _guard = tokio.enter();
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  post_a_delayed_v8_task(&mut runtime);
  runtime.execute_script("<none>", "1 + 1").unwrap();
}

const ABORT_CHILD_ENV: &str = "DENO_CORE_TEST_DELAYED_TASK_ABORT_CHILD";

/// Run by `the_delayed_task_abort_is_never_silent` in a child process: an
/// isolate with no tokio handle (what a release build creates outside tokio)
/// receives a delayed task, and the process aborts.
#[tokio::test]
#[ignore = "run in a child process by the_delayed_task_abort_is_never_silent"]
async fn delayed_task_abort_child() {
  #[allow(clippy::disallowed_methods, reason = "test code")]
  let is_child = std::env::var_os(ABORT_CHILD_ENV).is_some();
  if !is_child {
    return;
  }
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  // SAFETY: the pointer only identifies the isolate's registry entry.
  let isolate_ptr = unsafe { runtime.v8_isolate().as_raw_isolate_ptr() };
  setup::forget_tokio_handle_for_test(setup::isolate_ptr_to_key(isolate_ptr));
  post_a_delayed_v8_task(&mut runtime);
  // Give V8 a chance to post it from a background thread, too.
  for _ in 0..50 {
    runtime
      .execute_script("<none>", "new Array(1e5).fill(0)")
      .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
  }
  unreachable!("V8 posted no delayed task");
}

/// The abort's reason reaches stderr even under libtest's output capture
/// (which intercepts `eprintln!` and is lost when the process aborts), so a
/// test binary that dies there says why.
#[test]
fn the_delayed_task_abort_is_never_silent() {
  let output = std::process::Command::new(std::env::current_exe().unwrap())
    .args([
      "--exact",
      "runtime::tests::tokio_context::delayed_task_abort_child",
      "--ignored",
      "--test-threads=1",
    ])
    .env(ABORT_CHILD_ENV, "1")
    .stdin(std::process::Stdio::null())
    .output()
    .unwrap();
  let stderr = String::from_utf8_lossy(&output.stderr);
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    !output.status.success(),
    "the child should abort\nstdout:\n{stdout}\nstderr:\n{stderr}"
  );
  assert!(
    stderr.contains("V8 posted a delayed task, but this isolate was created outside of a tokio runtime context"),
    "the abort's reason is missing from stderr ({:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
    output.status
  );
}
