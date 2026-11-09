// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: Node's `--heap-prof`. A sampling heap profile (V8's
//! `HeapProfiler.startSampling`) taken from startup to exit and written as a
//! `.heapprofile` file, the format Chrome DevTools' Memory panel loads.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use deno_core::InspectorSessionKind;
use deno_core::JsRuntime;
use deno_core::JsRuntimeInspector;
use deno_core::LocalInspectorSession;
use deno_core::error::CoreError;
use deno_core::parking_lot::Mutex;
use deno_core::serde_json;
use deno_path_util::fs::atomic_write_file_with_retries;
use sys_traits::impls::RealSys;

use super::diagnostic_filename;
use super::expand_profile_name;
use super::next_msg_id;

/// Node's default `--heap-prof-interval`, in bytes.
pub const DEFAULT_HEAP_PROF_INTERVAL: u64 = 512 * 1024;

/// Configuration for heap profiling that can be passed to workers.
#[derive(Clone, Debug)]
pub struct HeapProfilerConfig {
  pub dir: PathBuf,
  pub name: Option<String>,
  /// The average number of bytes between samples.
  pub interval: u64,
}

/// The heap profile's file name: the `--heap-prof-name` (with `${pid}`
/// expanded; a worker's id appended in a worker), else Node's
/// `Heap.<date>.<time>.<pid>.<threadId>.<seq>.heapprofile`.
pub fn heap_prof_filename(
  config: &HeapProfilerConfig,
  thread_id: u32,
) -> String {
  match &config.name {
    Some(name) if thread_id != 0 => {
      let name = expand_profile_name(name);
      let stem = name.strip_suffix(".heapprofile").unwrap_or(&name);
      format!("{}.worker-{}.heapprofile", stem, thread_id)
    }
    Some(name) => expand_profile_name(name),
    None => diagnostic_filename("Heap", "heapprofile", thread_id),
  }
}

#[derive(Debug)]
struct HeapProfilerInner {
  path: PathBuf,
  profile_msg_id: Option<i32>,
}

#[derive(Clone, Debug)]
struct HeapProfilerState(Arc<Mutex<HeapProfilerInner>>);

impl HeapProfilerState {
  fn callback(&self, msg: deno_core::InspectorMsg) {
    let deno_core::InspectorMsgKind::Message(msg_id) = msg.kind else {
      return;
    };
    let inner = self.0.lock();
    if inner.profile_msg_id != Some(msg_id) {
      return;
    }
    let message: serde_json::Value = match serde_json::from_str(&msg.content) {
      Ok(v) => v,
      Err(err) => {
        log::error!("Failed to parse heap profiler response: {:?}", err);
        return;
      }
    };
    let Some(profile) = message.get("result").and_then(|r| r.get("profile"))
    else {
      log::error!("No profile in the heap profiler response");
      return;
    };
    let profile_str = match serde_json::to_string(profile) {
      Ok(s) => s,
      Err(err) => {
        log::error!("Failed to serialize heap profile: {:?}", err);
        return;
      }
    };
    if let Err(err) = atomic_write_file_with_retries(
      &RealSys,
      &inner.path,
      profile_str.as_bytes(),
      0o666,
    ) {
      log::error!(
        "Failed to write heap profile file at {:?}, reason: {:?}",
        inner.path,
        err
      );
    }
  }
}

pub struct HeapProfiler {
  state: HeapProfilerState,
  session: LocalInspectorSession,
  dir: PathBuf,
  interval: u64,
}

impl HeapProfiler {
  pub fn new(
    js_runtime: &mut JsRuntime,
    config: &HeapProfilerConfig,
    thread_id: u32,
  ) -> Self {
    let filename = heap_prof_filename(config, thread_id);
    let state = HeapProfilerState(Arc::new(Mutex::new(HeapProfilerInner {
      path: config.dir.join(filename),
      profile_msg_id: None,
    })));
    js_runtime.maybe_init_inspector();
    let insp = js_runtime.inspector();
    let s = state.clone();
    let session = JsRuntimeInspector::create_local_session(
      insp,
      Box::new(move |message| s.callback(message)),
      InspectorSessionKind::NonBlocking {
        wait_for_disconnect: false,
      },
    );
    Self {
      state,
      session,
      dir: config.dir.clone(),
      interval: config.interval,
    }
  }

  pub fn start_profiling(&mut self) {
    self.session.post_message(
      next_msg_id(),
      "HeapProfiler.startSampling",
      Some(serde_json::json!({ "samplingInterval": self.interval as f64 })),
    );
  }

  pub fn stop_profiling(&mut self) -> Result<(), CoreError> {
    #[allow(
      clippy::disallowed_methods,
      reason = "always using real fs with profiler"
    )]
    fs::create_dir_all(&self.dir)?;
    let msg_id = next_msg_id();
    self.state.0.lock().profile_msg_id = Some(msg_id);
    self
      .session
      .post_message::<()>(msg_id, "HeapProfiler.stopSampling", None);
    Ok(())
  }
}
