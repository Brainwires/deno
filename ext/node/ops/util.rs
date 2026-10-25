// Copyright 2018-2026 the Deno authors. MIT license.

use std::path::Path;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::time::Duration;

use deno_core::OpState;
use deno_core::error::JsError;
use deno_core::op2;
use deno_core::v8;
use deno_dotenv::parse_env_content_hook;
use deno_error::JsErrorBox;
use node_resolver::InNpmPackageChecker;
use node_resolver::NpmPackageFolderResolver;

use crate::ExtNodeSys;
use crate::NodeResolverRc;

// === SIGINT watchdog ===
// `internalBinding('contextify')`'s watchdog, which Node uses to tell whether
// a Ctrl+C arrived while a script was being evaluated (the REPL, `vm`'s
// `breakOnSigint`). Reference-counted so nested start/stop pairs work. Ported
// from denoland/deno#32423.

static SIGINT_WATCHDOG_REFCOUNT: AtomicI32 = AtomicI32::new(0);
static SIGINT_WATCHDOG_PENDING: AtomicBool = AtomicBool::new(false);

#[op2(fast)]
pub fn op_node_start_sigint_watchdog() {
  let prev = SIGINT_WATCHDOG_REFCOUNT.fetch_add(1, Ordering::SeqCst);
  if prev == 0 {
    SIGINT_WATCHDOG_PENDING.store(false, Ordering::SeqCst);
    // The watchdog consumes SIGINT: other listeners (the test runner,
    // `Deno.addSignalListener`) don't see it while it watches.
    deno_signals::set_interceptor(
      deno_signals::SIGINT,
      Some(Box::new(|| {
        SIGINT_WATCHDOG_PENDING.store(true, Ordering::SeqCst);
        true
      })),
    );
  }
}

#[op2(fast)]
pub fn op_node_stop_sigint_watchdog() -> bool {
  let prev = SIGINT_WATCHDOG_REFCOUNT.fetch_sub(1, Ordering::SeqCst);
  let had_pending = SIGINT_WATCHDOG_PENDING.swap(false, Ordering::SeqCst);
  if prev == 1 {
    deno_signals::set_interceptor(deno_signals::SIGINT, None);
  }
  had_pending
}

#[op2(fast)]
pub fn op_node_watchdog_has_pending_sigint() -> bool {
  SIGINT_WATCHDOG_PENDING.load(Ordering::SeqCst)
}

// === SIGINT trace ===
// Node's `--trace-sigint` / `util.setTraceSigInt(true)`: when SIGINT is about
// to end the process (no listener took it over), print where the main thread
// was, then let the signal end it. Adapted from denoland/deno#32423.

/// How long the signal waits for the isolate to service the interrupt before
/// it reports an idle main thread (no JavaScript running).
const TRACE_SIGINT_IDLE_GRACE: Duration = Duration::from_millis(250);
/// How long the signal waits for a trace that has started printing.
const TRACE_SIGINT_PRINT_TIMEOUT: Duration = Duration::from_secs(5);
const TRACE_SIGINT_BANNER: &str =
  "KEYBOARD_INTERRUPT: Script execution was interrupted by `SIGINT`";

static TRACE_SIGINT_HOOK_ID: Mutex<Option<u32>> = Mutex::new(None);
/// Who prints the report for this SIGINT: the interrupt (with the stack) or
/// the signal (an idle main thread), whichever claims it first.
static TRACE_SIGINT_CLAIMED: AtomicBool = AtomicBool::new(false);
static TRACE_SIGINT_PRINTED: (Mutex<bool>, Condvar) =
  (Mutex::new(false), Condvar::new());

/// Turns the SIGINT trace on or off for the isolate behind `isolate`, whose
/// stack is captured in `context`. Used by `util.setTraceSigInt()` and by
/// `--trace-sigint` in `NODE_OPTIONS` (the embedder calls it at startup).
pub fn set_trace_sigint(
  isolate: v8::IsolateHandle,
  context: v8::Global<v8::Context>,
  enable: bool,
) {
  let mut hook_id = TRACE_SIGINT_HOOK_ID.lock().unwrap();
  if !enable {
    if let Some(id) = hook_id.take() {
      deno_signals::unregister_before_default_action(deno_signals::SIGINT, id);
    }
    return;
  }
  if hook_id.is_some() {
    return;
  }
  // Leaked on purpose: the interrupt may fire after the trace is turned off,
  // and a `v8::Global` must not be dropped off its isolate's thread.
  let context_ptr = Box::into_raw(Box::new(context)) as usize;
  let result = deno_signals::register_before_default_action(
    deno_signals::SIGINT,
    Box::new(move || {
      isolate.request_interrupt(
        trace_sigint_interrupt,
        context_ptr as *mut std::ffi::c_void,
      );
      wait_for_trace_sigint();
    }),
  );
  match result {
    Ok(id) => *hook_id = Some(id),
    Err(err) => log::debug!("Could not trace SIGINT: {err}"),
  }
}

/// Runs on the signal thread, before SIGINT's default action ends the
/// process: waits for the interrupt to print the stack, or reports an idle
/// main thread (which runs no JavaScript to interrupt), as Node does.
#[allow(clippy::print_stderr, reason = "Node's --trace-sigint report")]
fn wait_for_trace_sigint() {
  let (printed, cvar) = &TRACE_SIGINT_PRINTED;
  let guard = printed.lock().unwrap();
  let (guard, _) = cvar
    .wait_timeout_while(guard, TRACE_SIGINT_IDLE_GRACE, |printed| !*printed)
    .unwrap();
  if *guard {
    return;
  }
  if !TRACE_SIGINT_CLAIMED.swap(true, Ordering::SeqCst) {
    eprintln!("{TRACE_SIGINT_BANNER}");
    return;
  }
  // The interrupt claimed it and is printing.
  drop(
    cvar
      .wait_timeout_while(guard, TRACE_SIGINT_PRINT_TIMEOUT, |printed| {
        !*printed
      })
      .unwrap(),
  );
}

#[allow(clippy::print_stderr, reason = "Node's --trace-sigint report")]
unsafe extern "C" fn trace_sigint_interrupt(
  isolate_ptr: v8::UnsafeRawIsolatePtr,
  data: *mut std::ffi::c_void,
) {
  if TRACE_SIGINT_CLAIMED.swap(true, Ordering::SeqCst) {
    return;
  }
  let mut raw_ptr = isolate_ptr;
  // SAFETY: V8 passes the isolate the interrupt was requested on, and runs
  // the callback on its thread.
  let isolate =
    unsafe { v8::Isolate::ref_from_raw_isolate_ptr_mut(&mut raw_ptr) };
  // SAFETY: `data` is the `v8::Global<v8::Context>` leaked by
  // `set_trace_sigint`, never freed.
  let context_global = unsafe { &*(data as *const v8::Global<v8::Context>) };
  let stack = {
    v8::scope!(scope, isolate);
    let context = v8::Local::new(scope, context_global);
    let scope = &mut v8::ContextScope::new(scope, context);
    let message = v8::String::new(scope, "SIGINT").unwrap();
    let exception = v8::Exception::error(scope, message);
    JsError::from_v8_exception(scope, exception).stack
  };
  eprintln!("{TRACE_SIGINT_BANNER}");
  // The stack starts with the synthetic "Error: SIGINT" line; print only the
  // frames, as Node does.
  if let Some((_, frames)) = stack.as_deref().and_then(|s| s.split_once('\n')) {
    eprintln!("{frames}");
  }
  let (printed, cvar) = &TRACE_SIGINT_PRINTED;
  *printed.lock().unwrap() = true;
  cvar.notify_all();
}

#[op2(fast)]
pub fn op_node_set_trace_sigint(
  scope: &mut v8::PinScope<'_, '_>,
  enable: bool,
) {
  let isolate = scope.thread_safe_handle();
  let context = scope.get_current_context();
  let context = v8::Global::new(scope, context);
  set_trace_sigint(isolate, context, enable);
}

/// Whether `NODE_OPTIONS` turns Node's `--trace-sigint` on (the last of
/// `--trace-sigint` / `--no-trace-sigint` wins).
pub fn node_options_trace_sigint(node_options: &str) -> bool {
  let mut enabled = false;
  for option in node_options.split_ascii_whitespace() {
    match option.trim_matches(|c| c == '"' || c == '\'') {
      "--trace-sigint" => enabled = true,
      "--no-trace-sigint" => enabled = false,
      _ => {}
    }
  }
  enabled
}

#[cfg(test)]
mod trace_sigint_tests {
  use super::node_options_trace_sigint;

  #[test]
  fn node_options_turn_the_trace_on() {
    assert!(!node_options_trace_sigint(""));
    assert!(node_options_trace_sigint("--trace-sigint"));
    assert!(node_options_trace_sigint(
      "--max-old-space-size=100 \"--trace-sigint\" --no-warnings"
    ));
    assert!(!node_options_trace_sigint(
      "--trace-sigint --no-trace-sigint"
    ));
    assert!(node_options_trace_sigint(
      "--no-trace-sigint --trace-sigint"
    ));
    assert!(!node_options_trace_sigint("--trace-sigint-other"));
    assert!(!node_options_trace_sigint("--trace-warnings"));
  }
}

#[repr(u32)]
enum HandleType {
  #[allow(dead_code, reason = "variant kept for repr(u32) mapping")]
  Tcp = 0,
  Tty,
  #[allow(dead_code, reason = "variant kept for repr(u32) mapping")]
  Udp,
  File,
  Pipe,
  Unknown,
}

#[op2(fast)]
pub fn op_node_guess_handle_type(_state: &mut OpState, fd: u32) -> u32 {
  guess_handle_type(fd as i32) as u32
}

#[cfg(unix)]
fn guess_handle_type(fd: i32) -> HandleType {
  use deno_core::uv_compat;
  match uv_compat::uv_guess_handle(fd) {
    uv_compat::uv_handle_type::UV_TCP => HandleType::Tcp,
    uv_compat::uv_handle_type::UV_TTY => HandleType::Tty,
    uv_compat::uv_handle_type::UV_UDP => HandleType::Unknown,
    uv_compat::uv_handle_type::UV_FILE => HandleType::File,
    uv_compat::uv_handle_type::UV_NAMED_PIPE => HandleType::Pipe,
    _ => HandleType::Unknown,
  }
}

#[cfg(windows)]
fn guess_handle_type(fd: i32) -> HandleType {
  use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_CHAR;
  use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_DISK;
  use windows_sys::Win32::Storage::FileSystem::FILE_TYPE_PIPE;
  use windows_sys::Win32::Storage::FileSystem::GetFileType;
  use windows_sys::Win32::System::Console::GetConsoleMode;

  if fd < 0 {
    return HandleType::Unknown;
  }
  // SAFETY: get_osfhandle converts a CRT fd to an OS handle.
  // Returns -1 (INVALID_HANDLE_VALUE) for invalid fds.
  let handle = unsafe { libc::get_osfhandle(fd) };
  if handle == -1 {
    return HandleType::Unknown;
  }
  let handle = handle as windows_sys::Win32::Foundation::HANDLE;
  // SAFETY: handle is a valid OS handle from get_osfhandle.
  match unsafe { GetFileType(handle) } {
    FILE_TYPE_DISK => HandleType::File,
    FILE_TYPE_CHAR => {
      let mut mode = 0;
      // SAFETY: handle is valid, mode is a valid pointer.
      if unsafe { GetConsoleMode(handle, &mut mode) } == 1 {
        HandleType::Tty
      } else {
        HandleType::File
      }
    }
    FILE_TYPE_PIPE => HandleType::Pipe,
    _ => HandleType::Unknown,
  }
}

#[op2(fast)]
pub fn op_node_view_has_buffer(buffer: v8::Local<v8::ArrayBufferView>) -> bool {
  buffer.has_buffer()
}

/// Checks if the current call site is from a dependency package.
#[op2(fast)]
pub fn op_node_call_is_from_dependency<
  TInNpmPackageChecker: InNpmPackageChecker + 'static,
  TNpmPackageFolderResolver: NpmPackageFolderResolver + 'static,
  TSys: ExtNodeSys + 'static,
>(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
) -> bool {
  // non internal call site should appear in < 20 frames
  let Some(stack_trace) = v8::StackTrace::current_stack_trace(scope, 20) else {
    return false;
  };
  let mut only_internal = true;
  for i in 0..stack_trace.get_frame_count() {
    let Some(frame) = stack_trace.get_frame(scope, i) else {
      continue;
    };
    if !frame.is_user_javascript() {
      continue;
    }
    let Some(script) = frame.get_script_name(scope) else {
      continue;
    };
    let mut name_buf: [std::mem::MaybeUninit<u8>; 1024] =
      [std::mem::MaybeUninit::uninit(); 1024];
    let name = script.to_rust_cow_lossy(scope, &mut name_buf);

    if name.starts_with("node:") || name.starts_with("ext:") {
      continue;
    } else {
      only_internal = false;
    }

    if name.starts_with("https:")
      || name.contains("/node_modules/")
      || name.contains(r"\node_modules\")
    {
      return true;
    }

    let Ok(specifier) = url::Url::parse(&name) else {
      continue;
    };
    if only_internal {
      return true;
    }
    return state.borrow::<NodeResolverRc<
        TInNpmPackageChecker,
        TNpmPackageFolderResolver,
        TSys,
      >>().in_npm_package(&specifier);
  }
  only_internal
}

#[op2(fast)]
pub fn op_node_in_npm_package<
  TInNpmPackageChecker: InNpmPackageChecker + 'static,
  TNpmPackageFolderResolver: NpmPackageFolderResolver + 'static,
  TSys: ExtNodeSys + 'static,
>(
  state: &mut OpState,
  #[string] path: &str,
) -> bool {
  let specifier = if deno_path_util::specifier_has_uri_scheme(path) {
    match url::Url::parse(path) {
      Ok(url) => url,
      Err(_) => return false,
    }
  } else {
    match deno_path_util::url_from_file_path(Path::new(path)) {
      Ok(url) => url,
      Err(_) => return false,
    }
  };

  state.borrow::<NodeResolverRc<
    TInNpmPackageChecker,
    TNpmPackageFolderResolver,
    TSys,
  >>().in_npm_package(&specifier)
}

#[op2]
pub fn op_node_get_own_non_index_properties<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  obj: v8::Local<'s, v8::Object>,
  #[smi] filter: u32,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  let mut property_filter = v8::PropertyFilter::ALL_PROPERTIES;
  if filter & 1 << 0 != 0 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_WRITABLE;
  }
  if filter & 1 << 1 != 0 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_ENUMERABLE;
  }
  if filter & 1 << 2 != 0 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_CONFIGURABLE;
  }
  if filter & 1 << 3 != 0 {
    property_filter = property_filter | v8::PropertyFilter::SKIP_STRINGS;
  }
  if filter & 1 << 4 != 0 {
    property_filter = property_filter | v8::PropertyFilter::SKIP_SYMBOLS;
  }

  v8::tc_scope!(let tc_scope, scope);

  let result = obj.get_property_names(
    tc_scope,
    v8::GetPropertyNamesArgs {
      index_filter: v8::IndexFilter::SkipIndices,
      property_filter,
      key_conversion: v8::KeyConversionMode::NoNumbers,
      mode: v8::KeyCollectionMode::OwnOnly,
    },
  );

  match result {
    Some(names) => Ok(names.into()),
    None => {
      if tc_scope.has_caught() || tc_scope.has_terminated() {
        tc_scope.rethrow();
        // Dummy value, this result will be discarded because an error was thrown.
        let v = v8::undefined(tc_scope);
        Ok(v.into())
      } else {
        Err(JsErrorBox::type_error(
          "Failed to get own non-index properties",
        ))
      }
    }
  }
}

#[op2]
pub fn op_node_parse_env<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  #[string] content: &str,
) -> v8::Local<'a, v8::Object> {
  let env_obj = v8::Object::new(scope);
  parse_env_content_hook(content, &mut |key, value| {
    let key = v8::String::new(scope, key).unwrap();
    let value = v8::String::new(scope, value).unwrap();
    env_obj.set(scope, key.into(), value.into());
  });
  env_obj
}
