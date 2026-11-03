// Copyright 2018-2026 the Deno authors. MIT license.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use deno_core::InspectorSessionKind;
use deno_core::JsRuntime;
use deno_core::JsRuntimeInspector;
use deno_core::LocalInspectorSession;
use deno_core::SourceMapper;
use deno_core::error::CoreError;
use deno_core::parking_lot::Mutex;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_path_util::fs::atomic_write_file_with_retries;
use sys_traits::impls::RealSys;
use uuid::Uuid;

static NEXT_MSG_ID: AtomicI32 = AtomicI32::new(0);

fn next_msg_id() -> i32 {
  NEXT_MSG_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug)]
pub struct CoverageCollectorInner {
  dir: PathBuf,
  coverage_msg_id: Option<i32>,
  /// denext: with source maps to write, the coverages wait here until the
  /// emitted sources are read (see `CoverageCollector::stop_collecting`).
  defer_writes: bool,
  pending: Option<Vec<cdp::ScriptCoverage>>,
  /// `Debugger.getScriptSource` requests in flight, by message id, and the
  /// sources they answered with.
  source_requests: HashMap<i32, String>,
  sources: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct CoverageCollectorState(Arc<Mutex<CoverageCollectorInner>>);

impl CoverageCollectorState {
  pub fn new(dir: PathBuf) -> Self {
    Self(Arc::new(Mutex::new(CoverageCollectorInner {
      dir,
      coverage_msg_id: None,
      defer_writes: false,
      pending: None,
      source_requests: HashMap::new(),
      sources: HashMap::new(),
    })))
  }

  pub fn callback(&self, msg: deno_core::InspectorMsg) {
    let deno_core::InspectorMsgKind::Message(msg_id) = msg.kind else {
      return;
    };
    let maybe_coverage_msg_id = self.0.lock().coverage_msg_id.as_ref().cloned();

    if let Some(coverage_msg_id) = maybe_coverage_msg_id
      && coverage_msg_id == msg_id
    {
      let message: serde_json::Value =
        serde_json::from_str(&msg.content).unwrap();
      let coverages: cdp::TakePreciseCoverageResponse =
        serde_json::from_value(message["result"].clone()).unwrap();
      let mut inner = self.0.lock();
      if inner.defer_writes {
        inner.pending = Some(coverages.result);
        return;
      }
      drop(inner);
      self.write_coverages(coverages.result);
      return;
    }

    let mut inner = self.0.lock();
    if let Some(url) = inner.source_requests.remove(&msg_id) {
      let message: serde_json::Value =
        serde_json::from_str(&msg.content).unwrap_or_default();
      if let Some(source) = message["result"]["scriptSource"].as_str() {
        inner.sources.insert(url, source.to_string());
      }
    }
  }

  /// denext: beside a transpiled script's coverage, the map from its emitted
  /// code (whose offsets the coverage has) to its source, for node:test's
  /// coverage: `<uuid>.sourcemap`, `{ url, lineLengths, data }` (the
  /// `source-map-cache` entry of Node's `NODE_V8_COVERAGE` files, lengths in
  /// UTF-16 code units as V8's offsets are). Not a `.json` file, which
  /// `deno coverage` would read as a script's coverage.
  fn write_source_map(&self, url: &str, source: &str, source_map: &[u8]) {
    let Ok(data) = serde_json::from_slice::<serde_json::Value>(source_map)
    else {
      return;
    };
    let line_lengths = source
      .split('\n')
      .map(|line| line.encode_utf16().count())
      .collect::<Vec<_>>();
    let entry = serde_json::json!({
      "url": url,
      "lineLengths": line_lengths,
      "data": data,
    });
    let filepath = self
      .0
      .lock()
      .dir
      .join(format!("{}.sourcemap", Uuid::new_v4()));
    if let Err(err) = atomic_write_file_with_retries(
      &RealSys,
      &filepath,
      entry.to_string().as_bytes(),
      0o666,
    ) {
      log::error!(
        "Failed to write coverage source map at {:?}, reason: {:?}",
        filepath,
        err
      );
    }
  }

  fn write_coverages(&self, script_coverages: Vec<cdp::ScriptCoverage>) {
    for script_coverage in script_coverages {
      if !is_reported(&script_coverage.url) {
        continue;
      }

      let filename = format!("{}.json", Uuid::new_v4());
      let filepath = self.0.lock().dir.join(filename);

      let coverage = serde_json::to_string_pretty(&script_coverage).unwrap();
      // denext: written to a temporary file and renamed into place, so a
      // process ended while it writes (a signal's flush that ran out of time,
      // see `cli/util/signal_flush.rs`) leaves either the whole file or none,
      // never a truncated `.json` that `deno coverage` can't parse. The
      // temporary file ends in `.tmp`, which `deno coverage` doesn't read.
      if let Err(err) = atomic_write_file_with_retries(
        &RealSys,
        &filepath,
        coverage.as_bytes(),
        0o666,
      ) {
        log::error!(
          "Failed to write coverage file at {:?}, reason: {:?}",
          filepath,
          err
        );
        continue;
      }
    }
  }
}

/// Filter out internal and http/https JS files, eval'd scripts, and scripts
/// with invalid urls from being included in coverage reports.
fn is_reported(url: &str) -> bool {
  !(url.is_empty()
    || url.starts_with("ext:")
    || url.starts_with("[ext:")
    || url.starts_with("http:")
    || url.starts_with("https:")
    || url.starts_with("node:")
    || Url::parse(url).is_err())
}

/// denext: a module the runtime transpiles before V8 runs it, whose
/// coverage offsets are the emitted code's.
fn is_transpiled(url: &str) -> bool {
  let path = url.split(['?', '#']).next().unwrap_or(url);
  url.starts_with("file:")
    && [".ts", ".mts", ".cts", ".tsx", ".jsx"]
      .iter()
      .any(|ext| path.ends_with(ext))
}

pub struct CoverageCollector {
  pub state: CoverageCollectorState,
  session: LocalInspectorSession,
  /// denext: set when the process is a test file of node:test's runner
  /// (`NODE_TEST_CONTEXT`), whose coverage maps transpiled scripts back.
  source_mapper: Option<Rc<RefCell<SourceMapper>>>,
}

impl CoverageCollector {
  pub fn new(js_runtime: &mut JsRuntime, coverage_dir: PathBuf) -> Self {
    let state = CoverageCollectorState::new(coverage_dir);

    js_runtime.maybe_init_inspector();
    let insp = js_runtime.inspector();

    let s = state.clone();
    let callback = Box::new(move |message| s.clone().callback(message));
    let session = JsRuntimeInspector::create_local_session(
      insp,
      callback,
      InspectorSessionKind::Blocking,
    );

    #[allow(
      clippy::disallowed_methods,
      reason = "node:test's runner marks its test files' processes in the environment"
    )]
    let source_mapper = std::env::var_os("NODE_TEST_CONTEXT")
      .is_some()
      .then(|| js_runtime.source_mapper());
    state.0.lock().defer_writes = source_mapper.is_some();

    Self {
      state,
      session,
      source_mapper,
    }
  }

  pub fn start_collecting(&mut self) {
    self
      .session
      .post_message::<()>(next_msg_id(), "Profiler.enable", None);
    self.session.post_message(
      next_msg_id(),
      "Profiler.startPreciseCoverage",
      Some(cdp::StartPreciseCoverageArgs {
        call_count: true,
        detailed: true,
        allow_triggered_updates: false,
      }),
    );
  }

  pub fn stop_collecting(&mut self) -> Result<(), CoreError> {
    #[allow(
      clippy::disallowed_methods,
      reason = "coverage always uses real fs"
    )]
    fs::create_dir_all(&self.state.0.lock().dir)?;
    let msg_id = next_msg_id();
    self.state.0.lock().coverage_msg_id.replace(msg_id);

    self.session.post_message::<()>(
      msg_id,
      "Profiler.takePreciseCoverage",
      None,
    );
    let Some(source_mapper) = self.source_mapper.as_ref() else {
      return Ok(());
    };
    let Some(coverages) = self.state.0.lock().pending.take() else {
      return Ok(());
    };
    // denext: each transpiled script's map, and its emitted source (V8's,
    // which the coverage offsets are of) for the map's line lengths.
    let mut maps = Vec::new();
    for coverage in &coverages {
      if is_reported(&coverage.url)
        && is_transpiled(&coverage.url)
        && let Some(map) =
          source_mapper.borrow_mut().source_map_json(&coverage.url)
      {
        maps.push((coverage.url.clone(), coverage.script_id.clone(), map));
      }
    }
    if !maps.is_empty() {
      self
        .session
        .post_message::<()>(next_msg_id(), "Debugger.enable", None);
      for (url, script_id, _) in &maps {
        let id = next_msg_id();
        self.state.0.lock().source_requests.insert(id, url.clone());
        self.session.post_message(
          id,
          "Debugger.getScriptSource",
          Some(serde_json::json!({ "scriptId": script_id })),
        );
      }
      self
        .session
        .post_message::<()>(next_msg_id(), "Debugger.disable", None);
    }
    self.state.write_coverages(coverages);
    for (url, _, map) in maps {
      let source = self.state.0.lock().sources.remove(&url);
      if let Some(source) = source {
        self.state.write_source_map(&url, &source, &map);
      }
    }
    Ok(())
  }
}

mod cdp {
  use serde::Deserialize;
  use serde::Serialize;

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#method-takePreciseCoverage>
  #[derive(Debug, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct TakePreciseCoverageResponse {
    pub result: Vec<ScriptCoverage>,
    pub timestamp: f64,
  }

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#type-CoverageRange>
  #[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Clone)]
  #[serde(rename_all = "camelCase")]
  pub struct CoverageRange {
    /// Start character index.
    #[serde(rename = "startOffset")]
    pub start_char_offset: usize,
    /// End character index.
    #[serde(rename = "endOffset")]
    pub end_char_offset: usize,
    pub count: i64,
  }

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#type-FunctionCoverage>
  #[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Clone)]
  #[serde(rename_all = "camelCase")]
  pub struct FunctionCoverage {
    pub function_name: String,
    pub ranges: Vec<CoverageRange>,
    pub is_block_coverage: bool,
  }

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#type-ScriptCoverage>
  #[derive(Debug, Eq, PartialEq, Serialize, Deserialize, Clone)]
  #[serde(rename_all = "camelCase")]
  pub struct ScriptCoverage {
    pub script_id: String,
    pub url: String,
    pub functions: Vec<FunctionCoverage>,
  }

  /// <https://chromedevtools.github.io/devtools-protocol/tot/Profiler/#method-startPreciseCoverage>
  #[derive(Debug, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct StartPreciseCoverageArgs {
    pub call_count: bool,
    pub detailed: bool,
    pub allow_triggered_updates: bool,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn transpiled_modules_are_told_by_extension() {
    assert!(is_transpiled("file:///a/math.ts"));
    assert!(is_transpiled("file:///a/math.mts?x=1"));
    assert!(is_transpiled("file:///a/view.tsx"));
    assert!(!is_transpiled("file:///a/math.js"));
    assert!(!is_transpiled("file:///a/math.d.mjs"));
    assert!(!is_transpiled("https://x.test/a.ts"));
  }

  // A coverage file is renamed into place once written, so the directory
  // only ever holds whole `.json` files (and no temporary file is left).
  #[test]
  #[allow(
    clippy::disallowed_methods,
    reason = "the test reads the real directory the collector wrote to"
  )]
  fn coverage_files_are_written_whole() {
    let dir = tempfile::tempdir().unwrap();
    let state = CoverageCollectorState::new(dir.path().to_path_buf());
    state.write_coverages(vec![
      cdp::ScriptCoverage {
        script_id: "1".to_string(),
        url: "file:///a.ts".to_string(),
        functions: vec![],
      },
      // Not written: internal.
      cdp::ScriptCoverage {
        script_id: "2".to_string(),
        url: "ext:core/a.js".to_string(),
        functions: vec![],
      },
    ]);
    let names = std::fs::read_dir(dir.path())
      .unwrap()
      .map(|entry| entry.unwrap().file_name().into_string().unwrap())
      .collect::<Vec<_>>();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with(".json"), "{names:?}");
    let written: cdp::ScriptCoverage = serde_json::from_str(
      &std::fs::read_to_string(dir.path().join(&names[0])).unwrap(),
    )
    .unwrap();
    assert_eq!(written.url, "file:///a.ts");
  }
}
