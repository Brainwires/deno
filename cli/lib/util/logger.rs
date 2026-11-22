// Copyright 2018-2026 the Deno authors. MIT license.

use std::cell::Cell;
use std::hash::Hasher;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use arc_swap::ArcSwap;
use deno_runtime::deno_telemetry;
use deno_runtime::deno_telemetry::OtelConfig;
use deno_runtime::deno_telemetry::OtelConsoleConfig;
use deno_runtime::deno_webrtc;

struct CliLoggerInner {
  otel_console_config: OtelConsoleConfig,
  logger: env_logger::Logger,
}

struct CliLogger {
  inner: ArcSwap<CliLoggerInner>,
  on_log_start: fn(),
  on_log_end: fn(),
}

impl CliLogger {
  pub fn filter(&self) -> log::LevelFilter {
    self.inner.load().logger.filter()
  }
}

thread_local! {
  /// How many [`quiet_below_error`] guards this thread holds (denext).
  static QUIET_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// While the returned guard lives, log records below `Error` from this
/// thread are dropped (denext). Unlike lowering `log::max_level`, which is
/// global, this leaves other threads' records alone, such as the inspector
/// server's "Debugger session started.". Drop the guard on the thread that
/// created it.
pub fn quiet_below_error() -> QuietGuard {
  QUIET_DEPTH.with(|depth| depth.set(depth.get() + 1));
  QuietGuard(())
}

/// See [`quiet_below_error`].
#[must_use]
pub struct QuietGuard(());

impl Drop for QuietGuard {
  fn drop(&mut self) {
    QUIET_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
  }
}

fn is_quieted(level: log::Level) -> bool {
  level > log::Level::Error && QUIET_DEPTH.with(|depth| depth.get() > 0)
}

// ---------------------------------------------------------------------------
// denext: the kernel sandbox's re-executed image (DENEXT.md item 425)
// ---------------------------------------------------------------------------

/// The most records whose fingerprints are kept for the image after a
/// re-exec ([`printed_record_fingerprints`]); the startup diagnostics are
/// far fewer, and later records are never handed over.
const MAX_PRINTED: usize = 512;

/// Whether written records are still fingerprinted (until the hand-over, or
/// [`MAX_PRINTED`] of them).
static RECORDING: AtomicBool = AtomicBool::new(true);
static PRINTED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Whether records the image before a re-exec already printed are dropped
/// ([`suppress_already_printed`]).
static SUPPRESSING: AtomicBool = AtomicBool::new(false);
static ALREADY_PRINTED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A record's fingerprint: its level and its text (both images are the same
/// binary, so the hasher's fixed keys give the same value in each).
fn fingerprint(level: log::Level, args: &std::fmt::Arguments) -> u64 {
  struct HashWriter(std::hash::DefaultHasher);
  impl std::fmt::Write for HashWriter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
      self.0.write(s.as_bytes());
      Ok(())
    }
  }
  let mut writer = HashWriter(std::hash::DefaultHasher::new());
  writer.0.write_usize(level as usize);
  let _ = std::fmt::write(&mut writer, *args);
  writer.0.finish()
}

fn record_printed(fingerprint: u64) {
  if !RECORDING.load(Ordering::Relaxed) {
    return;
  }
  let mut printed = PRINTED.lock().unwrap_or_else(|e| e.into_inner());
  if printed.len() >= MAX_PRINTED {
    RECORDING.store(false, Ordering::Relaxed);
    return;
  }
  printed.push(fingerprint);
}

/// Whether a record with `fingerprint` was already printed by the image
/// before this one; each fingerprint handed over hides one record.
fn take_already_printed(fingerprint: u64) -> bool {
  if !SUPPRESSING.load(Ordering::Relaxed) {
    return false;
  }
  let mut handed = ALREADY_PRINTED.lock().unwrap_or_else(|e| e.into_inner());
  match handed.iter().position(|f| *f == fingerprint) {
    Some(index) => {
      handed.swap_remove(index);
      if handed.is_empty() {
        // nothing left to hide
        SUPPRESSING.store(false, Ordering::Relaxed);
      }
      true
    }
    None => false,
  }
}

/// The fingerprints of the records this process wrote so far, for the image
/// a kernel sandbox re-exec starts: it runs the same startup again and
/// hides the diagnostics this image already printed
/// ([`suppress_already_printed`]). Stops recording.
pub fn printed_record_fingerprints() -> Vec<u64> {
  RECORDING.store(false, Ordering::Relaxed);
  PRINTED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Stops fingerprinting written records: this process won't re-execute.
pub fn stop_recording_printed() {
  RECORDING.store(false, Ordering::Relaxed);
  PRINTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The image after a kernel sandbox re-exec: until
/// [`end_already_printed`], a record whose fingerprint the image before it
/// handed over is dropped, once per fingerprint (that image printed it, and
/// this one repeats the same startup). A record only this image produces is
/// written as usual.
pub fn suppress_already_printed(fingerprints: Vec<u64>) {
  RECORDING.store(false, Ordering::Relaxed);
  if fingerprints.is_empty() {
    return;
  }
  *ALREADY_PRINTED.lock().unwrap_or_else(|e| e.into_inner()) = fingerprints;
  SUPPRESSING.store(true, Ordering::Relaxed);
}

/// The image after the re-exec reached the point where the image before it
/// re-executed: what it prints from here on is its own, except a record the
/// image before printed that this one hasn't repeated yet, which is still
/// hidden once. Some startup diagnostics come later in this image than in
/// the one before it (a config file's ignored compiler options: printed
/// while type-checking there, while transpiling here); the image before
/// printed nothing but its startup, so nothing of this image's own is lost.
pub fn end_already_printed() {
  let handed = ALREADY_PRINTED.lock().unwrap_or_else(|e| e.into_inner());
  if handed.is_empty() {
    SUPPRESSING.store(false, Ordering::Relaxed);
  }
}

/// The WebRTC engine's records (the `rtc` and `rtc_*` crates) are shown at
/// the level `Deno.webrtc.setLogLevel` sets (off by default; ext/webrtc
/// reports what matters as `RTCPeerConnection` events, DENEXT.md item 370),
/// whatever `DENO_LOG` says, each prefixed with its level and source.
fn engine_enabled(metadata: &log::Metadata) -> bool {
  metadata.level() <= deno_webrtc::engine_log_level()
}

fn write_engine_record(record: &log::Record) {
  let mut target = record.target().to_string();
  if let Some(line_no) = record.line() {
    target.push(':');
    target.push_str(&line_no.to_string());
  }
  let _ = writeln!(
    std::io::stderr().lock(),
    "{} RS - {} - {}",
    record.level(),
    target,
    record.args()
  );
}

impl log::Log for CliLogger {
  fn enabled(&self, metadata: &log::Metadata) -> bool {
    if is_quieted(metadata.level()) {
      return false;
    }
    if deno_webrtc::is_engine_log_target(metadata.target()) {
      return engine_enabled(metadata);
    }
    self.inner.load().logger.enabled(metadata)
  }

  fn log(&self, record: &log::Record) {
    if self.enabled(record.metadata()) {
      // denext: the kernel sandbox's re-executed image doesn't print what
      // the image before it printed
      if RECORDING.load(Ordering::Relaxed)
        || SUPPRESSING.load(Ordering::Relaxed)
      {
        let fp = fingerprint(record.level(), record.args());
        if take_already_printed(fp) {
          return;
        }
        record_printed(fp);
      }
      (self.on_log_start)();

      let engine = deno_webrtc::is_engine_log_target(record.target());
      let write = |record: &log::Record| {
        if engine {
          write_engine_record(record);
        } else {
          self.inner.load().logger.log(record);
        }
      };
      match self.inner.load().otel_console_config {
        OtelConsoleConfig::Ignore => {
          write(record);
        }
        OtelConsoleConfig::Capture => {
          write(record);
          deno_telemetry::handle_log(record);
        }
        OtelConsoleConfig::Replace => {
          deno_telemetry::handle_log(record);
        }
      }

      (self.on_log_end)();
    }
  }

  fn flush(&self) {
    self.inner.load().logger.flush();
  }
}

pub struct InitLoggingOptions {
  pub on_log_start: fn(),
  pub on_log_end: fn(),
  pub maybe_level: Option<log::Level>,
  pub otel_config: Option<OtelConfig>,
}

static LOGGER: OnceLock<CliLogger> = OnceLock::new();

pub fn init(options: InitLoggingOptions) {
  let log_level = options.maybe_level.unwrap_or(log::Level::Info);
  let logger = env_logger::Builder::from_env(
    env_logger::Env::new()
      // Use `DENO_LOG` and `DENO_LOG_STYLE` instead of `RUST_` prefix
      .filter_or("DENO_LOG", log_level.to_level_filter().to_string())
      .write_style("DENO_LOG_STYLE"),
  )
  // https://github.com/denoland/deno/issues/6641
  .filter_module("rustyline", log::LevelFilter::Off)
  // wgpu crates (gfx_backend), have a lot of useless INFO and WARN logs
  .filter_module("wgpu", log::LevelFilter::Error)
  .filter_module("gfx", log::LevelFilter::Error)
  .filter_module("globset", log::LevelFilter::Error)
  // used to make available the lsp_debug which is then filtered out at runtime
  // in the cli logger
  .filter_module("deno::lsp::performance", log::LevelFilter::Debug)
  .filter_module("rustls", log::LevelFilter::Off)
  // WebRTC's engine (the `rtc` and `rtc_*` crates) logs every state change
  // at INFO and drops errors it can't return as WARN: its records bypass
  // this filter and follow `Deno.webrtc.setLogLevel` (off by default; see
  // `engine_enabled`).
  // swc_ecma_codegen's `srcmap!` macro emits error-level spans only on debug
  // build:
  // https://github.com/swc-project/swc/blob/74d6478be1eb8cdf1df096c360c159db64b64d8a/crates/swc_ecma_codegen/src/macros.rs#L112
  // We suppress them here to avoid flooding our CI logs in integration tests.
  .filter_module("swc_ecma_codegen", log::LevelFilter::Off)
  .filter_module("swc_common::source_map", log::LevelFilter::Off)
  .filter_module("swc_ecma_transforms_optimization", log::LevelFilter::Off)
  .filter_module("swc_ecma_parser", log::LevelFilter::Error)
  .filter_module("swc_ecma_lexer", log::LevelFilter::Error)
  // Suppress span lifecycle logs since they are too verbose
  .filter_module("tracing::span", log::LevelFilter::Off)
  .filter_module("tower_lsp", log::LevelFilter::Trace)
  .filter_module("opentelemetry_sdk", log::LevelFilter::Off)
  // for deno_compile, this is too verbose
  .filter_module("editpe", log::LevelFilter::Error)
  // too verbose
  .filter_module("cranelift_codegen", log::LevelFilter::Off)
  .write_style(if deno_terminal::colors::use_color() {
    env_logger::WriteStyle::Always
  } else {
    env_logger::WriteStyle::Never
  })
  .format(|buf, record| {
    let mut target = record.target().to_string();
    if let Some(line_no) = record.line() {
      target.push(':');
      target.push_str(&line_no.to_string());
    }
    if record.level() <= log::Level::Info
      || (record.target() == "deno::lsp::performance"
        && record.level() == log::Level::Debug)
    {
      // Print ERROR, WARN, INFO and lsp_debug logs as they are
      writeln!(buf, "{}", record.args())
    } else {
      // Add prefix to DEBUG or TRACE logs
      writeln!(
        buf,
        "{} RS - {} - {}",
        record.level(),
        target,
        record.args()
      )
    }
  })
  .build();

  let otel_console_config = options
    .otel_config
    .map(|c| c.console)
    .unwrap_or(OtelConsoleConfig::Ignore);

  let cli_logger = LOGGER.get_or_init(move || CliLogger {
    on_log_start: options.on_log_start,
    on_log_end: options.on_log_end,
    inner: ArcSwap::new(Arc::new(CliLoggerInner {
      logger: env_logger::Builder::new().build(),
      otel_console_config,
    })),
  });

  cli_logger.inner.swap(Arc::new(CliLoggerInner {
    logger,
    otel_console_config,
  }));

  let _ = log::set_logger(cli_logger);
  log::set_max_level(cli_logger.filter());
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn engine_records_follow_the_webrtc_log_level() {
    let record = |target: &'static str, level| {
      log::Metadata::builder().target(target).level(level).build()
    };
    assert!(deno_webrtc::is_engine_log_target("rtc_ice::agent"));
    assert!(deno_webrtc::is_engine_log_target("rtc::peer_connection"));
    assert!(!deno_webrtc::is_engine_log_target("rtcp_like"));
    assert!(!deno_webrtc::is_engine_log_target("deno_webrtc::peer"));
    // Off by default.
    assert!(!engine_enabled(&record(
      "rtc_ice::agent",
      log::Level::Error
    )));
    deno_webrtc::set_engine_log_level("debug").unwrap();
    assert!(engine_enabled(&record("rtc_ice::agent", log::Level::Debug)));
    assert!(!engine_enabled(&record(
      "rtc_ice::agent",
      log::Level::Trace
    )));
    deno_webrtc::set_engine_log_level("off").unwrap();
    assert!(!engine_enabled(&record(
      "rtc_ice::agent",
      log::Level::Error
    )));
    assert!(deno_webrtc::set_engine_log_level("loud").is_err());
  }

  #[test]
  fn fingerprints_tell_level_and_text_apart() {
    let a = fingerprint(log::Level::Warn, &format_args!("Warning {}", 1));
    assert_eq!(a, fingerprint(log::Level::Warn, &format_args!("Warning 1")));
    assert_ne!(
      a,
      fingerprint(log::Level::Error, &format_args!("Warning 1"))
    );
    assert_ne!(a, fingerprint(log::Level::Warn, &format_args!("Warning 2")));
  }

  #[test]
  fn already_printed_hides_each_handed_over_record_once() {
    let warn = fingerprint(log::Level::Warn, &format_args!("twice"));
    let other = fingerprint(log::Level::Warn, &format_args!("only here"));
    suppress_already_printed(vec![warn]);
    assert!(!take_already_printed(other));
    assert!(take_already_printed(warn));
    // a second record with the same text is this image's own
    assert!(!take_already_printed(warn));
    // past the point where the image before re-executed, a record it
    // printed that this image hasn't repeated yet is still hidden, once
    suppress_already_printed(vec![warn, warn]);
    assert!(take_already_printed(warn));
    end_already_printed();
    assert!(take_already_printed(warn));
    assert!(!take_already_printed(warn));
    // with nothing left, suppression ends
    suppress_already_printed(vec![other]);
    assert!(take_already_printed(other));
    end_already_printed();
    assert!(!SUPPRESSING.load(Ordering::Relaxed));
  }

  #[test]
  fn quiet_below_error_is_per_thread_and_nests() {
    assert!(!is_quieted(log::Level::Info));
    let outer = quiet_below_error();
    let inner = quiet_below_error();
    assert!(is_quieted(log::Level::Info));
    assert!(is_quieted(log::Level::Warn));
    assert!(!is_quieted(log::Level::Error));
    // Another thread is not quieted.
    std::thread::spawn(|| assert!(!is_quieted(log::Level::Info)))
      .join()
      .unwrap();
    drop(inner);
    assert!(is_quieted(log::Level::Info));
    drop(outer);
    assert!(!is_quieted(log::Level::Info));
  }
}
