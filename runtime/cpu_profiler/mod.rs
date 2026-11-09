// Copyright 2018-2026 the Deno authors. MIT license.

mod cpuprof;
mod flamegraph;
mod heapprof;

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::time::SystemTime;

pub use heapprof::DEFAULT_HEAP_PROF_INTERVAL;
pub use heapprof::HeapProfiler;
pub use heapprof::HeapProfilerConfig;
pub use heapprof::heap_prof_filename;

/// Configuration for CPU profiling that can be passed to workers.
#[derive(Clone, Debug)]
pub struct CpuProfilerConfig {
  pub dir: PathBuf,
  pub name: Option<String>,
  pub interval: u32,
  pub md: bool,
  pub flamegraph: bool,
}

/// denext: Node's diagnostic file name,
/// `<prefix>.<YYYYMMDD>.<HHMMSS>.<pid>.<threadId>.<seq>.<ext>` (Node's
/// `DiagnosticFilename`): local time, the thread id is 0 on the main thread
/// and the worker's `threadId` in a worker, and the sequence number counts
/// every such file the process writes, from 001.
pub fn diagnostic_filename(prefix: &str, ext: &str, thread_id: u32) -> String {
  static SEQ: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
  let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
  let (year, month, day, hours, minutes, seconds) = local_time_now();
  let pid = std::process::id();
  format!(
    "{prefix}.{year:04}{month:02}{day:02}.{hours:02}{minutes:02}{seconds:02}.{pid}.{thread_id}.{seq:03}.{ext}"
  )
}

/// The current local date and time as `(year, month, day, hours, minutes,
/// seconds)`.
#[cfg(unix)]
fn local_time_now() -> (i64, u32, u32, u32, u32, u32) {
  let now = SystemTime::now()
    .duration_since(SystemTime::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or_default() as libc::time_t;
  // SAFETY: `tm` is plain old data; zeroed is a valid value for it.
  let mut tm: libc::tm = unsafe { std::mem::zeroed() };
  // SAFETY: both pointers are valid for the duration of the call, and
  // `localtime_r` is the reentrant variant.
  let ok = unsafe { !libc::localtime_r(&now, &mut tm).is_null() };
  if !ok {
    return utc_time_now();
  }
  (
    tm.tm_year as i64 + 1900,
    (tm.tm_mon + 1) as u32,
    tm.tm_mday as u32,
    tm.tm_hour as u32,
    tm.tm_min as u32,
    tm.tm_sec as u32,
  )
}

#[cfg(not(unix))]
fn local_time_now() -> (i64, u32, u32, u32, u32, u32) {
  utc_time_now()
}

fn utc_time_now() -> (i64, u32, u32, u32, u32, u32) {
  let secs = SystemTime::now()
    .duration_since(SystemTime::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or_default();
  // Howard Hinnant's civil-from-days.
  let z = (secs / 86400) as i64 + 719468;
  let era = if z >= 0 { z } else { z - 146096 } / 146097;
  let doe = (z - era * 146097) as u64;
  let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
  let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
  let year = yoe as i64 + era * 400 + if month <= 2 { 1 } else { 0 };
  let sod = secs % 86400;
  (
    year,
    month,
    day,
    (sod / 3600) as u32,
    ((sod % 3600) / 60) as u32,
    (sod % 60) as u32,
  )
}

/// denext: a `--cpu-prof-name` / `--heap-prof-name` may hold `${pid}`, which
/// is replaced with the process id, as Node does.
pub fn expand_profile_name(name: &str) -> String {
  name.replace("${pid}", &std::process::id().to_string())
}

/// Generate a default CPU profile filename, Node's
/// `CPU.<date>.<time>.<pid>.<threadId>.<seq>.cpuprofile`.
pub fn cpu_prof_default_filename(thread_id: u32) -> String {
  diagnostic_filename("CPU", "cpuprofile", thread_id)
}

/// Generate a CPU profile filename from config. `thread_id` is 0 on the main
/// thread and the worker's id in a worker, where a custom name gets the
/// worker's id appended so that every thread writes its own file.
pub fn cpu_prof_filename(config: &CpuProfilerConfig, thread_id: u32) -> String {
  match &config.name {
    Some(name) if thread_id != 0 => {
      let name = expand_profile_name(name);
      let stem = name.strip_suffix(".cpuprofile").unwrap_or(&name);
      format!("{}.worker-{}.cpuprofile", stem, thread_id)
    }
    Some(name) => expand_profile_name(name),
    None => cpu_prof_default_filename(thread_id),
  }
}

use deno_core::InspectorSessionKind;
use deno_core::JsRuntime;
use deno_core::JsRuntimeInspector;
use deno_core::LocalInspectorSession;
use deno_core::SourceMapApplication;
use deno_core::SourceMapper;
use deno_core::error::CoreError;
use deno_core::parking_lot::Mutex;
use deno_core::serde_json;
use deno_path_util::fs::atomic_write_file_with_retries;
use sys_traits::impls::RealSys;

static NEXT_MSG_ID: AtomicI32 = AtomicI32::new(0);

fn next_msg_id() -> i32 {
  NEXT_MSG_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug)]
pub struct CpuProfilerInner {
  dir: PathBuf,
  filename: String,
  interval: u32,
  generate_md: bool,
  generate_flamegraph: bool,
  /// Set by `stop_profiling` and checked by `callback`. This is safe because
  /// the inspector processes messages on a single thread, so `stop_profiling`
  /// always sets this before the callback fires for that message.
  profile_msg_id: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct CpuProfilerState(Arc<Mutex<CpuProfilerInner>>);

impl CpuProfilerState {
  pub fn new(
    dir: PathBuf,
    filename: String,
    interval: u32,
    generate_md: bool,
    generate_flamegraph: bool,
  ) -> Self {
    Self(Arc::new(Mutex::new(CpuProfilerInner {
      dir,
      filename,
      interval,
      generate_md,
      generate_flamegraph,
      profile_msg_id: None,
    })))
  }

  pub fn callback(
    &self,
    msg: deno_core::InspectorMsg,
    source_mapper: &Rc<RefCell<SourceMapper>>,
  ) {
    let deno_core::InspectorMsgKind::Message(msg_id) = msg.kind else {
      return;
    };
    let maybe_profile_msg_id = self.0.lock().profile_msg_id.as_ref().cloned();

    if let Some(profile_msg_id) = maybe_profile_msg_id
      && profile_msg_id == msg_id
    {
      let mut message: serde_json::Value =
        match serde_json::from_str(&msg.content) {
          Ok(v) => v,
          Err(err) => {
            log::error!("Failed to parse CPU profiler response: {:?}", err);
            return;
          }
        };

      // Extract the profile from result.profile and apply source maps
      if let Some(result) = message.get_mut("result") {
        if let Some(profile) = result.get_mut("profile") {
          apply_source_maps(profile, &mut source_mapper.borrow_mut());
          self.write_profile(profile);
        } else {
          log::error!("No 'profile' field in CPU profiler response");
        }
      } else {
        log::error!("No 'result' field in CPU profiler response");
      }
    }
  }

  fn write_profile(&self, profile: &serde_json::Value) {
    let inner = self.0.lock();
    let filepath = inner.dir.join(&inner.filename);

    let profile_str = match serde_json::to_string_pretty(&profile) {
      Ok(s) => s,
      Err(err) => {
        log::error!("Failed to serialize CPU profile: {:?}", err);
        return;
      }
    };

    // denext: written to a temporary file and renamed into place, so a
    // process ended while it writes (a signal's flush that ran out of time,
    // see `cli/util/signal_flush.rs`) leaves the whole profile or none.
    if let Err(err) = atomic_write_file_with_retries(
      &RealSys,
      &filepath,
      profile_str.as_bytes(),
      0o666,
    ) {
      log::error!(
        "Failed to write CPU profile file at {:?}, reason: {:?}",
        filepath,
        err
      );
      return;
    }

    // Generate markdown report if requested
    if inner.generate_md {
      let md_filename = inner.filename.replace(".cpuprofile", ".md");
      let md_filepath = inner.dir.join(&md_filename);
      if let Err(err) = cpuprof::generate_markdown_report(
        profile,
        &md_filepath,
        inner.interval as i64,
      ) {
        log::error!(
          "Failed to generate markdown report at {:?}, reason: {:?}",
          md_filepath,
          err
        );
      }
    }

    // Generate flamegraph SVG if requested
    if inner.generate_flamegraph {
      let svg_filename = inner.filename.replace(".cpuprofile", ".svg");
      let svg_filepath = inner.dir.join(&svg_filename);
      if let Err(err) =
        flamegraph::generate_flamegraph_svg(profile, &svg_filepath)
      {
        log::error!(
          "Failed to generate flamegraph at {:?}, reason: {:?}",
          svg_filepath,
          err
        );
      }
    }
  }
}

pub struct CpuProfiler {
  pub state: CpuProfilerState,
  session: LocalInspectorSession,
  interval: u32,
}

impl CpuProfiler {
  pub fn new(
    js_runtime: &mut JsRuntime,
    cpu_prof_dir: PathBuf,
    filename: String,
    interval: u32,
    generate_md: bool,
    generate_flamegraph: bool,
  ) -> Self {
    let state = CpuProfilerState::new(
      cpu_prof_dir,
      filename,
      interval,
      generate_md,
      generate_flamegraph,
    );

    js_runtime.maybe_init_inspector();
    let insp = js_runtime.inspector();
    let source_mapper = js_runtime.source_mapper();

    let s = state.clone();
    let callback =
      Box::new(move |message| s.clone().callback(message, &source_mapper));
    let session = JsRuntimeInspector::create_local_session(
      insp,
      callback,
      InspectorSessionKind::NonBlocking {
        wait_for_disconnect: false,
      },
    );

    Self {
      state,
      session,
      interval,
    }
  }

  pub fn start_profiling(&mut self) {
    self
      .session
      .post_message::<()>(next_msg_id(), "Profiler.enable", None);

    // Note: Profiler.setSamplingInterval must be called before Profiler.start
    // but after Profiler.enable
    if self.interval != 1000 {
      self.session.post_message(
        next_msg_id(),
        "Profiler.setSamplingInterval",
        Some(cdp::SetSamplingIntervalArgs {
          interval: self.interval,
        }),
      );
    }

    self
      .session
      .post_message::<()>(next_msg_id(), "Profiler.start", None);

    log::debug!("CPU profiler started with interval: {}us", self.interval);
  }

  // fs::create_dir_all is on the Deno project's clippy disallowed list
  // (preferring the sys_traits abstraction), but the CPU profiler runs in the
  // runtime crate where using std::fs directly is acceptable.
  pub fn stop_profiling(&mut self) -> Result<(), CoreError> {
    #[allow(
      clippy::disallowed_methods,
      reason = "always using real fs with profiler"
    )]
    fs::create_dir_all(&self.state.0.lock().dir)?;

    let msg_id = next_msg_id();
    self.state.0.lock().profile_msg_id.replace(msg_id);

    self
      .session
      .post_message::<()>(msg_id, "Profiler.stop", None);

    log::debug!("CPU profiler stopped");

    Ok(())
  }
}

/// Apply source maps to all call frames in the CPU profile.
/// V8's profiler reports positions in transpiled JavaScript; this maps them
/// back to the original TypeScript (or other) source locations.
fn apply_source_maps(
  profile: &mut serde_json::Value,
  source_mapper: &mut SourceMapper,
) {
  let Some(nodes) = profile.get_mut("nodes").and_then(|n| n.as_array_mut())
  else {
    return;
  };

  for node in nodes {
    let Some(call_frame) = node.get_mut("callFrame") else {
      continue;
    };

    let Some(url) = call_frame.get("url").and_then(|u| u.as_str()) else {
      continue;
    };
    if url.is_empty() {
      continue;
    }

    // V8 profile line/column numbers are 0-based;
    // SourceMapper::apply_source_map expects 1-based.
    let line_number = call_frame
      .get("lineNumber")
      .and_then(|l| l.as_i64())
      .unwrap_or(-1);
    let column_number = call_frame
      .get("columnNumber")
      .and_then(|c| c.as_i64())
      .unwrap_or(-1);

    if line_number < 0 || column_number < 0 {
      continue;
    }

    let url_str = url.to_string();
    match source_mapper.apply_source_map(
      &url_str,
      (line_number + 1) as u32,
      (column_number + 1) as u32,
    ) {
      SourceMapApplication::LineAndColumn {
        line_number: new_line,
        column_number: new_col,
      } => {
        // Convert back to 0-based for the profile
        call_frame["lineNumber"] = serde_json::Value::from(new_line as i64 - 1);
        call_frame["columnNumber"] =
          serde_json::Value::from(new_col as i64 - 1);
      }
      SourceMapApplication::LineAndColumnAndFileName {
        file_name,
        line_number: new_line,
        column_number: new_col,
      } => {
        call_frame["url"] = serde_json::Value::from(file_name);
        call_frame["lineNumber"] = serde_json::Value::from(new_line as i64 - 1);
        call_frame["columnNumber"] =
          serde_json::Value::from(new_col as i64 - 1);
      }
      SourceMapApplication::Unchanged => {}
    }
  }
}

mod cdp {
  use serde::Serialize;

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#method-setSamplingInterval>
  #[derive(Debug, Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct SetSamplingIntervalArgs {
    pub interval: u32,
  }
}
