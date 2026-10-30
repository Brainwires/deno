// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: crash reports (DENEXT.md items 177 and 180).
//!
//! Two things a panic may say about the process that panicked:
//!
//! - The CLI's panic banner printed the whole command line (`Args: [...]`),
//!   and a command line can carry secrets (`--env` values, tokens in URLs,
//!   script arguments), which then land in container and CI logs. The banner
//!   names only the subcommand, when it is one of Deno's own, and how many
//!   arguments there were ([`command_line_summary`]).
//! - An opt-in crash report: with `DENEXT_CRASH_REPORT_URL` set to an
//!   `https://` URL or a local `file://` path, the `deno` CLI and `denort`
//!   (`deno compile` binaries) send one JSON report per panicking process
//!   ([`install`], [`report_panic`]); a desktop app sends the same report to
//!   the `error_reporting_url` baked into it ([`report_panic_to`]). The URL is
//!   read from the environment only, never baked into a CLI or denort
//!   artifact: it often carries an ingest token. A report never carries the
//!   environment, the command line, the working directory, the host or user
//!   name, or JavaScript source; the panic message has the home and working
//!   directories replaced (`~`, `<cwd>`), control characters removed, and is
//!   cut at 2048 characters.
//!
//! The sender ([`send_error_report`]) is the desktop runtime's, moved here: a
//! report goes only to `https://` (with a 2 s connect and a 5 s overall
//! bound, on its own thread) or appends one line to a local file. An HTTPS
//! client is created when the first report goes out unless an embedder has
//! stashed one ([`set_error_report_client`]), so a panic before any
//! JavaScript ran is delivered too.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_core::url::Url;
use deno_tls::rustls::RootCertStore;

/// The report destination: `https://…` or `file:///abs/path.jsonl`.
pub const URL_ENV: &str = "DENEXT_CRASH_REPORT_URL";
/// `1`: panic on purpose right after start-up, to test the report pipeline.
pub const SELFTEST_ENV: &str = "DENEXT_CRASH_REPORT_SELFTEST";
/// The application a report is about (`denext@4.10.3`), set by denext.
pub const APP_ENV: &str = "DENEXT_CRASH_REPORT_APP";
/// The panic message of [`maybe_selftest`].
pub const SELFTEST_MESSAGE: &str = "denext crash-report self-test";

/// The report schema; 1 is the desktop runtime's original report.
const SCHEMA: u32 = 2;
/// The longest panic message a report carries, in characters.
const MAX_MESSAGE_CHARS: usize = 2048;
/// The largest report body, in bytes.
const MAX_BODY_BYTES: usize = 16 * 1024;
/// How long a report's TCP connect may take.
const REPORT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an HTTPS report may take (connect, TLS, request and response)
/// before it is given up. The report is posted on its own thread and joined,
/// so the panic hook's report is out before the process exits; without a
/// bound, an endpoint that accepts and never answers (or a black-holed
/// network) hung the exiting process forever.
const ERROR_REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// The subcommands a report or banner may name. Anything else (a script
/// path, a script argument, a typo) is reported as unknown.
const SUBCOMMANDS: &[&str] = &[
  "add",
  "approve-builds",
  "approve-scripts",
  "audit",
  "bench",
  "bump-version",
  "bundle",
  "cache",
  "check",
  "ci",
  "clean",
  "compile",
  "completions",
  "coverage",
  "create",
  "deploy",
  "desktop",
  "doc",
  "eval",
  "fmt",
  "help",
  "i",
  "info",
  "init",
  "install",
  "jupyter",
  "link",
  "lint",
  "list",
  "lsp",
  "outdated",
  "pack",
  "publish",
  "remove",
  "repl",
  "rm",
  "run",
  "sandbox",
  "serve",
  "sync-types",
  "task",
  "test",
  "transpile",
  "types",
  "uninstall",
  "unlink",
  "update",
  "upgrade",
  "vendor",
  "why",
  "x",
];

/// The subcommand of a command line (`argv[0]` excluded): its first word that
/// is not a flag, when that word is one of Deno's subcommands; `None`
/// otherwise.
pub fn subcommand_from_args<S: AsRef<OsStr>>(
  args: &[S],
) -> Option<&'static str> {
  let word = args
    .iter()
    .map(|a| a.as_ref())
    .find(|a| !a.as_encoded_bytes().starts_with(b"-"))?;
  let word = word.to_str()?;
  SUBCOMMANDS.iter().copied().find(|s| *s == word)
}

/// The line a panic banner prints about the command line: the subcommand
/// (or `unknown`) and the argument count, never an argument itself.
pub fn command_line_summary(args: &[OsString]) -> String {
  let rest = args.get(1..).unwrap_or_default();
  let count = rest.len();
  format!(
    "{} ({count} argument{})",
    subcommand_from_args(rest).unwrap_or("unknown"),
    if count == 1 { "" } else { "s" }
  )
}

/// Which binary is reporting, and how it finds its TLS roots.
#[derive(Clone, Copy)]
pub struct BinaryInfo {
  /// `deno`, `denort` or `desktop`.
  pub binary: &'static str,
  /// The runtime version (`3.0.0-denext.4`).
  pub version: &'static str,
  /// The commit the binary was built from.
  pub git_hash: &'static str,
  /// The root certificates an HTTPS report trusts (the CLI's own:
  /// `DENO_TLS_CA_STORE`, `DENO_CERT`), else Mozilla's.
  pub root_cert_store: Option<fn() -> Option<RootCertStore>>,
}

struct EnvConfig {
  url: Url,
  selftest: bool,
}

static BINARY: OnceLock<BinaryInfo> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();
static SUBCOMMAND: OnceLock<Option<&'static str>> = OnceLock::new();
static ENV_CONFIG: OnceLock<Option<EnvConfig>> = OnceLock::new();
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Records which binary this is and when it started, without reading the
/// environment (the desktop runtime's reports go to its baked URL).
pub fn init_binary(info: BinaryInfo) {
  let _ = STARTED.get_or_init(Instant::now);
  let _ = BINARY.set(info);
}

/// Reads `DENEXT_CRASH_REPORT_URL` (once) for the CLI or `denort`. An invalid
/// value prints one warning, without the value, and turns reporting off;
/// start-up never fails because of it. Returns whether reports are on.
#[allow(clippy::print_stderr, reason = "a start-up warning")]
pub fn install(info: BinaryInfo) -> bool {
  init_binary(info);
  let config = ENV_CONFIG.get_or_init(|| {
    let value = std::env::var(URL_ENV).ok().filter(|v| !v.is_empty())?;
    match parse_report_url(&value) {
      Ok(url) => Some(EnvConfig {
        url,
        selftest: std::env::var(SELFTEST_ENV).is_ok_and(|v| v == "1"),
      }),
      Err(reason) => {
        eprintln!(
          "warning: {URL_ENV} is ignored ({reason}); crash reports are off"
        );
        None
      }
    }
  });
  if info.binary == "deno" {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let _ = SUBCOMMAND.set(subcommand_from_args(&args));
  }
  config.is_some()
}

/// Panics with [`SELFTEST_MESSAGE`] when `DENEXT_CRASH_REPORT_SELFTEST=1` and
/// a valid `DENEXT_CRASH_REPORT_URL` are set: operators check their report
/// pipeline end to end with it. Call it after the panic hook is installed.
pub fn maybe_selftest() {
  if ENV_CONFIG
    .get()
    .and_then(|c| c.as_ref())
    .is_some_and(|c| c.selftest)
  {
    panic!("{SELFTEST_MESSAGE}");
  }
}

/// A report destination: `https://` or a local `file://` path. `http:` is
/// refused (a report would cross the network in the clear), as are a remote
/// `file://` host and anything else.
pub fn parse_report_url(value: &str) -> Result<Url, String> {
  let url = Url::parse(value).map_err(|_| "not a URL".to_string())?;
  match url.scheme() {
    "https" => Ok(url),
    "file" => match deno_path_util::url_to_file_path(&url) {
      Ok(_) => Ok(url),
      Err(_) => Err("a file:// URL must name a local path".to_string()),
    },
    "http" => Err("http:// is not allowed, use https://".to_string()),
    other => Err(format!("{other}: is not allowed, use https:// or file://")),
  }
}

/// The report of a panic in the CLI or `denort`, sent to
/// `DENEXT_CRASH_REPORT_URL` when it is set. Blocks for at most about 5 s.
/// Only the first panic of the process is reported.
pub fn report_panic(info: &std::panic::PanicHookInfo<'_>) {
  let Some(config) = ENV_CONFIG.get().and_then(|c| c.as_ref()) else {
    return;
  };
  if REPORTED.swap(true, Ordering::SeqCst) {
    return;
  }
  let body = panic_report_body(info, None);
  send_error_report(config.url.as_str(), &body);
}

/// The report of a panic in a desktop app, sent to its baked
/// `error_reporting_url` with the app's version. Only the first panic of the
/// process is reported.
pub fn report_panic_to(
  url: &str,
  info: &std::panic::PanicHookInfo<'_>,
  app_version: Option<&str>,
) {
  if REPORTED.swap(true, Ordering::SeqCst) {
    return;
  }
  let body = panic_report_body(info, Some(app_version));
  send_error_report(url, &body);
}

/// The panic's message, whatever its payload type.
pub fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
  if let Some(s) = info.payload().downcast_ref::<&str>() {
    (*s).to_string()
  } else if let Some(s) = info.payload().downcast_ref::<String>() {
    s.clone()
  } else {
    "Deno runtime panicked".to_string()
  }
}

/// The stack trace of the panicking thread: `deno_panic`'s VLQ-encoded,
/// image-relative addresses, which `.github/denext-runtime/symbolicate.py`
/// turns into functions and lines with the release's symbol archives. `None`
/// in a build without the `panic-trace` feature.
pub fn trace() -> Option<String> {
  trace_impl()
}

#[cfg(all(
  feature = "panic-trace-frame-pointer",
  target_os = "linux",
  any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn trace_impl() -> Option<String> {
  Some(deno_panic::trace_frame_pointer())
}

#[cfg(all(
  feature = "panic-trace",
  not(all(
    feature = "panic-trace-frame-pointer",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
  ))
))]
fn trace_impl() -> Option<String> {
  Some(deno_panic::trace())
}

#[cfg(not(feature = "panic-trace"))]
fn trace_impl() -> Option<String> {
  None
}

fn panic_report_body(
  info: &std::panic::PanicHookInfo<'_>,
  desktop_app_version: Option<Option<&str>>,
) -> String {
  let message = panic_message(info);
  let selftest = message == SELFTEST_MESSAGE
    && ENV_CONFIG
      .get()
      .and_then(|c| c.as_ref())
      .is_some_and(|c| c.selftest);
  let home =
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok();
  #[allow(
    clippy::disallowed_methods,
    reason = "only to scrub the panic message"
  )]
  let cwd = std::env::current_dir()
    .ok()
    .and_then(|p| p.to_str().map(str::to_string));
  let binary = BINARY.get().copied();
  let thread = std::thread::current();
  let report = PanicReport {
    kind: if selftest { "selftest" } else { "panic" },
    binary: match (desktop_app_version, binary) {
      (Some(_), _) => "desktop",
      (None, Some(b)) => b.binary,
      (None, None) => "deno",
    },
    version: binary.map(|b| b.version).unwrap_or(""),
    git_hash: binary.map(|b| b.git_hash).unwrap_or(""),
    message: scrub_message(&message, home.as_deref(), cwd.as_deref()),
    stack: info
      .location()
      .map(|l| format!("at {}:{}:{}", l.file(), l.line(), l.column())),
    thread: thread.name().unwrap_or("<unnamed>").to_string(),
    trace: trace(),
    subcommand: SUBCOMMAND.get().copied().flatten(),
    uptime_ms: STARTED
      .get()
      .map(|s| s.elapsed().as_millis() as u64)
      .unwrap_or(0),
    app: std::env::var(APP_ENV).ok().filter(|a| valid_app(a)),
    app_version: desktop_app_version.map(|v| v.map(str::to_string)),
  };
  report.to_body()
}

/// One crash report (schema 2). A superset of the desktop runtime's schema 1
/// field names, so one endpoint can read both.
struct PanicReport {
  kind: &'static str,
  binary: &'static str,
  version: &'static str,
  git_hash: &'static str,
  message: String,
  stack: Option<String>,
  thread: String,
  trace: Option<String>,
  subcommand: Option<&'static str>,
  uptime_ms: u64,
  app: Option<String>,
  /// `Some` for a desktop app's report (its version, if it has one).
  app_version: Option<Option<String>>,
}

impl PanicReport {
  fn to_value(&self, trace: Option<&str>) -> serde_json::Value {
    let mut value = json!({
      "schema": SCHEMA,
      "kind": self.kind,
      "id": uuid::Uuid::new_v4().to_string(),
      "time": rfc3339_utc(std::time::SystemTime::now()),
      "binary": self.binary,
      "version": self.version,
      "gitHash": self.git_hash,
      "target": release_target(),
      "platform": std::env::consts::OS,
      "arch": std::env::consts::ARCH,
      "message": self.message,
      "stack": self.stack,
      "thread": self.thread,
      "trace": trace,
      "subcommand": self.subcommand,
      "uptimeMs": self.uptime_ms,
      "app": self.app,
    });
    if let Some(app_version) = &self.app_version {
      value["appVersion"] = json!(app_version);
    }
    value
  }

  /// The JSON body, at most [`MAX_BODY_BYTES`]: an over-long trace is cut,
  /// and dropped if that is not enough.
  fn to_body(&self) -> String {
    let body = self.to_value(self.trace.as_deref()).to_string();
    if body.len() <= MAX_BODY_BYTES {
      return body;
    }
    if let Some(trace) = &self.trace {
      let excess = body.len() - MAX_BODY_BYTES;
      if excess < trace.len() {
        // The trace is ASCII (base64url VLQ).
        let cut = &trace[..trace.len() - excess];
        let body = self.to_value(Some(cut)).to_string();
        if body.len() <= MAX_BODY_BYTES {
          return body;
        }
      }
    }
    self.to_value(None).to_string()
  }
}

/// `denext@4.10.3`: letters, digits and `@._+/-`, at most 64 characters.
fn valid_app(app: &str) -> bool {
  !app.is_empty()
    && app.len() <= 64
    && app
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b"@._+/-".contains(&b))
}

/// The panic message as a report carries it: the working directory and the
/// home directory replaced with `<cwd>` and `~` (the longer one first),
/// control characters removed, at most [`MAX_MESSAGE_CHARS`] characters.
fn scrub_message(
  message: &str,
  home: Option<&str>,
  cwd: Option<&str>,
) -> String {
  let mut prefixes: Vec<(&str, &str)> = [(cwd, "<cwd>"), (home, "~")]
    .into_iter()
    .filter_map(|(p, r)| p.filter(|p| p.len() > 1).map(|p| (p, r)))
    .collect();
  prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
  let mut out = message.to_string();
  for (prefix, replacement) in prefixes {
    out = out.replace(prefix, replacement);
  }
  out
    .chars()
    .filter(|c| !c.is_control())
    .take(MAX_MESSAGE_CHARS)
    .collect()
}

/// The release target triple of this build (one of the six the fork
/// releases), from the compile-time OS and architecture.
fn release_target() -> &'static str {
  match (std::env::consts::OS, std::env::consts::ARCH) {
    ("macos", "aarch64") => "aarch64-apple-darwin",
    ("macos", "x86_64") => "x86_64-apple-darwin",
    ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
    ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
    ("windows", "aarch64") => "aarch64-pc-windows-msvc",
    ("windows", "x86_64") => "x86_64-pc-windows-msvc",
    _ => "unknown",
  }
}

/// `2026-10-30T12:34:56.789Z`.
fn rfc3339_utc(time: std::time::SystemTime) -> String {
  let since = time
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  let secs = since.as_secs();
  let days = (secs / 86_400) as i64;
  let rem = secs % 86_400;
  // Howard Hinnant's civil_from_days.
  let z = days + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let day = doy - (153 * mp + 2) / 5 + 1;
  let month = if mp < 10 { mp + 3 } else { mp - 9 };
  let year = yoe + era * 400 + i64::from(month <= 2);
  format!(
    "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
    rem / 3_600,
    (rem % 3_600) / 60,
    rem % 60,
    since.subsec_millis()
  )
}

/// An HTTPS client an embedder stashed for reports (the desktop runtime's
/// `OpState` client, configured with the app's TLS roots). Without one, the
/// first HTTPS report creates its own.
static ERROR_REPORT_CLIENT: OnceLock<deno_fetch::Client> = OnceLock::new();

/// Stashes the HTTPS client reports use.
pub fn set_error_report_client(client: deno_fetch::Client) {
  let _ = ERROR_REPORT_CLIENT.set(client);
}

/// Whether a report client has been stashed.
pub fn has_error_report_client() -> bool {
  ERROR_REPORT_CLIENT.get().is_some()
}

/// The client for a report: the stashed one, else a new one with the
/// binary's root certificates (or Mozilla's), the proxy settings of the
/// environment (`HTTPS_PROXY`, `NO_PROXY`), and a 2 s connect bound.
fn report_client() -> Option<deno_fetch::Client> {
  if let Some(client) = ERROR_REPORT_CLIENT.get() {
    return Some(client.clone());
  }
  // A panic can come before the binary installed its TLS provider.
  let _ =
    deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
  let binary = BINARY.get().copied();
  let root_cert_store = binary
    .and_then(|b| b.root_cert_store)
    .and_then(|f| f())
    .unwrap_or_else(deno_tls::create_default_root_cert_store);
  let user_agent = format!(
    "denext-crash-report/{}",
    binary.map(|b| b.version).unwrap_or("unknown")
  );
  deno_fetch::create_http_client(
    &user_agent,
    deno_fetch::CreateHttpClientOptions {
      root_cert_store: Some(root_cert_store),
      connect_timeout: Some(REPORT_CONNECT_TIMEOUT),
      ..Default::default()
    },
  )
  .ok()
}

#[allow(
  clippy::disallowed_methods,
  reason = "best-effort panic-hook error-report append; the path is operator configuration and the FileSystem trait isn't reachable from a panic hook"
)]
fn append_to_file(path: &Path, body: &str) {
  let mut line = body.to_string();
  line.push('\n');
  let _ = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
    .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

fn post_error_report(url: String, body: String) {
  post_error_report_within(None, url, body, ERROR_REPORT_TIMEOUT);
}

/// POSTs `body` to `url` on a thread of its own and waits at most `timeout`
/// for it (plus 100 ms to shut the thread's runtime down). `client`: `None`
/// uses [`report_client`], created on that thread.
pub(crate) fn post_error_report_within(
  client: Option<deno_fetch::Client>,
  url: String,
  body: String,
  timeout: Duration,
) {
  let _ = std::thread::Builder::new()
    .name("crash-report".into())
    .spawn(move || {
      let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
      else {
        return;
      };
      runtime.block_on(async move {
        let Some(client) = client.or_else(report_client) else {
          log::warn!("crash report: no HTTPS client; dropping the report");
          return;
        };
        let Ok(uri) = url.parse::<http::Uri>() else {
          return;
        };
        let mut req =
          http::Request::new(deno_fetch::ReqBody::full(body.into()));
        *req.method_mut() = http::Method::POST;
        *req.uri_mut() = uri;
        req.headers_mut().insert(
          http::header::CONTENT_TYPE,
          http::HeaderValue::from_static("application/json"),
        );
        if tokio::time::timeout(timeout, client.send(req))
          .await
          .is_err()
        {
          log::warn!(
            "crash report: not delivered within {timeout:?}; dropping it"
          );
        }
      });
      // Don't wait on a resolver thread still stuck in the dropped request.
      runtime.shutdown_timeout(Duration::from_millis(100));
    })
    .map(|t| t.join());
}

/// Send a JSON error report to the given URL. Best-effort — never panics.
/// Accepts only `file://` and `https://`. Plain `http://` is rejected:
/// error reports usually carry stack traces and runtime context, so
/// anyone on-path could read them. A bare path (or any unparseable
/// string) is also rejected — previously such inputs were silently
/// treated as a local file path, which let a malformed metadata field
/// land error reports at an attacker-chosen location on disk.
pub fn send_error_report(url: &str, body: &str) {
  let Ok(parsed) = Url::parse(url) else {
    log::warn!("crash report: the report URL is not a valid URL; dropping it");
    return;
  };

  match parsed.scheme() {
    "file" => {
      // `url_to_file_path` rejects `file://host/...` URLs (non-local),
      // so a local path is the only way to reach `append_to_file`.
      let Ok(path) = deno_path_util::url_to_file_path(&parsed) else {
        log::warn!(
          "crash report: the file:// report URL is not a local path; dropping it"
        );
        return;
      };
      append_to_file(&path, body);
    }
    "https" => post_error_report(parsed.to_string(), body.to_string()),
    other => {
      log::warn!(
        "crash report: refusing to send a report over '{other}' (file:// or https:// only); dropping it",
      );
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
  }

  #[test]
  fn subcommand_is_the_first_non_flag_word() {
    assert_eq!(
      subcommand_from_args(&os(&["run", "-A", "main.ts"])),
      Some("run")
    );
    assert_eq!(
      subcommand_from_args(&os(&["-q", "--unstable", "test"])),
      Some("test")
    );
    assert_eq!(
      subcommand_from_args(&os(&["approve-scripts"])),
      Some("approve-scripts")
    );
  }

  #[test]
  fn subcommand_is_none_for_anything_else() {
    // A script path, a typo, a flag value or nothing at all.
    assert_eq!(subcommand_from_args(&os(&["main.ts", "run"])), None);
    assert_eq!(subcommand_from_args(&os(&["rnu"])), None);
    assert_eq!(
      subcommand_from_args(&os(&["--log-level", "debug", "run"])),
      None
    );
    assert_eq!(subcommand_from_args(&os(&["--version"])), None);
    assert_eq!(subcommand_from_args::<OsString>(&[]), None);
    assert_eq!(
      subcommand_from_args(&os(&["https://x.test/?token=s3cret"])),
      None
    );
  }

  #[test]
  fn summary_never_carries_an_argument() {
    let args = os(&[
      "/usr/bin/deno",
      "run",
      "--env-file=.env",
      "main.ts",
      "--token=s3cret",
    ]);
    let line = command_line_summary(&args);
    assert_eq!(line, "run (4 arguments)");
    assert!(!line.contains("s3cret"));
    assert_eq!(
      command_line_summary(&os(&["deno", "s3cret.ts"])),
      "unknown (1 argument)"
    );
    assert_eq!(
      command_line_summary(&os(&["deno"])),
      "unknown (0 arguments)"
    );
    assert_eq!(command_line_summary(&[]), "unknown (0 arguments)");
  }

  #[test]
  fn report_urls() {
    assert!(parse_report_url("https://ingest.example/r?token=x").is_ok());
    #[cfg(unix)]
    assert!(parse_report_url("file:///tmp/crash.jsonl").is_ok());
    #[cfg(windows)]
    assert!(parse_report_url("file:///C:/crash.jsonl").is_ok());
    let http = parse_report_url("http://ingest.example/r").unwrap_err();
    assert!(http.contains("http://"), "{http}");
    assert!(parse_report_url("file://server/share/crash.jsonl").is_err());
    assert!(parse_report_url("ftp://x.example/").is_err());
    assert!(parse_report_url("/tmp/crash.jsonl").is_err());
    assert!(parse_report_url("").is_err());
  }

  #[test]
  fn scrub_replaces_home_and_cwd_and_cuts() {
    let home = "/home/alice";
    let cwd = "/home/alice/work/app";
    let scrubbed = scrub_message(
      "cannot open /home/alice/work/app/data.db or /home/alice/.cache/x\u{1b}[31m\nnext",
      Some(home),
      Some(cwd),
    );
    assert_eq!(scrubbed, "cannot open <cwd>/data.db or ~/.cache/x[31mnext");
    assert!(!scrubbed.contains("alice"));

    let long = "é".repeat(MAX_MESSAGE_CHARS + 10);
    assert_eq!(
      scrub_message(&long, None, None).chars().count(),
      MAX_MESSAGE_CHARS
    );
    // A root cwd or home replaces nothing.
    assert_eq!(scrub_message("/a/b", Some("/"), Some("/")), "/a/b");
  }

  fn report(trace: Option<String>) -> PanicReport {
    PanicReport {
      kind: "panic",
      binary: "deno",
      version: "3.0.0-denext.4",
      git_hash: "abc",
      message: "boom".to_string(),
      stack: Some("at cli/main.rs:1:2".to_string()),
      thread: "main".to_string(),
      trace,
      subcommand: Some("run"),
      uptime_ms: 12,
      app: Some("denext@4.10.3".to_string()),
      app_version: None,
    }
  }

  #[test]
  fn payload_has_the_schema_and_nothing_private() {
    let value: serde_json::Value =
      serde_json::from_str(&report(Some("AAAA".into())).to_body()).unwrap();
    let object = value.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(
      keys,
      [
        "app",
        "arch",
        "binary",
        "gitHash",
        "id",
        "kind",
        "message",
        "platform",
        "schema",
        "stack",
        "subcommand",
        "target",
        "thread",
        "time",
        "trace",
        "uptimeMs",
        "version",
      ]
    );
    assert_eq!(value["schema"], 2);
    assert_eq!(value["subcommand"], "run");
    assert_eq!(value["trace"], "AAAA");
    let time = value["time"].as_str().unwrap();
    assert!(time.ends_with('Z') && time.len() == 24, "{time}");
    assert_eq!(value["id"].as_str().unwrap().len(), 36);
    for private in ["env", "argv", "args", "cwd", "hostname", "user"] {
      assert!(!object.contains_key(private), "{private}");
    }
  }

  #[test]
  fn desktop_payload_has_its_app_version() {
    let mut r = report(None);
    r.binary = "desktop";
    r.app_version = Some(Some("1.2.3".to_string()));
    let value: serde_json::Value = serde_json::from_str(&r.to_body()).unwrap();
    assert_eq!(value["appVersion"], "1.2.3");
    assert_eq!(value["binary"], "desktop");
    assert!(value["trace"].is_null());
  }

  #[test]
  fn body_is_capped() {
    let body = report(Some("A".repeat(64 * 1024))).to_body();
    assert!(body.len() <= MAX_BODY_BYTES, "{}", body.len());
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(value["trace"].as_str().unwrap().len() > 8 * 1024);
  }

  #[test]
  fn app_names() {
    assert!(valid_app("denext@4.10.3"));
    assert!(valid_app("@scope/app_1+build.2-x"));
    assert!(!valid_app(""));
    assert!(!valid_app("a b"));
    assert!(!valid_app("x\ny"));
    assert!(!valid_app(&"a".repeat(65)));
  }

  #[test]
  fn rfc3339() {
    let t = std::time::UNIX_EPOCH + Duration::from_millis(1_793_362_496_789);
    assert_eq!(rfc3339_utc(t), "2026-10-30T12:14:56.789Z");
    assert_eq!(
      rfc3339_utc(std::time::UNIX_EPOCH),
      "1970-01-01T00:00:00.000Z"
    );
    let leap = std::time::UNIX_EPOCH + Duration::from_secs(951_782_400);
    assert_eq!(rfc3339_utc(leap), "2000-02-29T00:00:00.000Z");
  }

  #[test]
  fn release_target_is_a_release_triple() {
    assert!(
      [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
      ]
      .contains(&release_target())
    );
  }

  #[test]
  fn only_the_first_report_goes_out() {
    // Four threads race for the single shot; exactly one wins.
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    let winners: usize = (0..4)
      .map(|_| {
        let flag = flag.clone();
        std::thread::spawn(move || !flag.swap(true, Ordering::SeqCst))
      })
      .collect::<Vec<_>>()
      .into_iter()
      .map(|t| usize::from(t.join().unwrap()))
      .sum();
    assert_eq!(winners, 1);
  }

  #[test]
  fn file_report_appends_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.jsonl");
    let url = Url::from_file_path(&path).unwrap();
    send_error_report(url.as_str(), r#"{"a":1}"#);
    send_error_report(url.as_str(), r#"{"b":2}"#);
    assert_eq!(
      std::fs::read_to_string(&path).unwrap(),
      "{\"a\":1}\n{\"b\":2}\n"
    );
    // http:// and a bare path write nothing.
    send_error_report("http://127.0.0.1:9/r", "{}");
    send_error_report(path.to_str().unwrap(), "{}");
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
  }

  #[test]
  fn error_report_post_gives_up_on_a_silent_endpoint() {
    // An HTTPS endpoint that accepts the connection and never answers (not
    // even the TLS handshake). The post used to wait on it forever.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let held = std::thread::spawn(move || {
      let conn = listener.accept();
      std::thread::sleep(Duration::from_secs(10));
      drop(conn);
    });
    let started = Instant::now();
    // No client stashed: the report creates its own.
    post_error_report_within(
      None,
      format!("https://{addr}/report"),
      "{}".into(),
      Duration::from_millis(500),
    );
    assert!(
      started.elapsed() < Duration::from_secs(5),
      "took {:?}",
      started.elapsed()
    );
    drop(held);
  }

  #[test]
  fn https_report_is_delivered_without_a_stashed_client() {
    // A panic before any JavaScript ran (the desktop runtime stashed its
    // client only when JavaScript reported an error, so such a report was
    // dropped): the report creates its own client, with the binary's root
    // certificates, and reaches the endpoint.
    let (addr, received) = tls_sink();
    init_binary(BinaryInfo {
      binary: "deno",
      version: "test",
      git_hash: "test",
      root_cert_store: Some(test_roots),
    });
    assert!(!has_error_report_client());
    send_error_report(
      &format!("https://localhost:{}/report", addr.port()),
      r#"{"schema":2}"#,
    );
    let got = received
      .recv_timeout(Duration::from_secs(5))
      .expect("the report reached the endpoint");
    assert!(got.starts_with("POST /report "), "{got}");
    assert!(got.contains(r#"{"schema":2}"#), "{got}");
  }

  /// Mozilla's roots plus test_server's CA (tests/testdata/tls/RootCA.pem).
  fn test_roots() -> Option<RootCertStore> {
    let ca =
      std::fs::read(test_util::testdata_path().join("tls/RootCA.pem")).ok()?;
    let mut store = deno_tls::create_default_root_cert_store();
    for cert in rustls_pemfile_certs(&ca) {
      store.add(cert).ok()?;
    }
    Some(store)
  }

  fn rustls_pemfile_certs(
    pem: &[u8],
  ) -> Vec<deno_tls::rustls::pki_types::CertificateDer<'static>> {
    deno_tls::rustls_pemfile::certs(&mut std::io::BufReader::new(pem))
      .collect::<Result<Vec<_>, _>>()
      .unwrap()
  }

  /// A one-connection HTTPS server on 127.0.0.1 with test_server's
  /// `localhost` certificate; sends what it read on the channel.
  fn tls_sink() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<String>) {
    use std::io::Read;
    use std::io::Write;
    let tls = test_util::testdata_path().join("tls");
    let certs =
      rustls_pemfile_certs(&std::fs::read(tls.join("localhost.crt")).unwrap());
    let key =
      deno_tls::rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::read(tls.join("localhost.key")).unwrap().as_slice(),
      ))
      .unwrap()
      .unwrap();
    let _ =
      deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let config = std::sync::Arc::new(
      deno_tls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let (mut tcp, _) = listener.accept().unwrap();
      tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
      let mut conn = deno_tls::rustls::ServerConnection::new(config).unwrap();
      let mut stream = deno_tls::rustls::Stream::new(&mut conn, &mut tcp);
      let mut seen = Vec::new();
      let mut buf = [0u8; 4096];
      while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
          break;
        }
        seen.extend_from_slice(&buf[..n]);
        if seen.windows(12).any(|w| w == br#"{"schema":2}"#) {
          break;
        }
      }
      let _ = stream
        .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n");
      let _ = tx.send(String::from_utf8_lossy(&seen).into_owned());
    });
    (addr, rx)
  }
}
