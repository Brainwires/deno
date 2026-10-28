// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 147): deadlines and retries for the CLI's downloads.
//!
//! A stalled connection almost never recovers, and a new one usually answers
//! at once, so a download waits a short time at each stage and then retries on
//! a new connection, a few times, within an overall budget:
//!
//! - **connect** (`DENO_DOWNLOAD_CONNECT_TIMEOUT_MS`, 10 s): the TCP connect,
//!   in the client's connector;
//! - **response** (`DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS`, 15 s): from sending a
//!   request to its response headers (connect, TLS, a proxy, the server);
//! - **idle** (`DENO_DOWNLOAD_IDLE_TIMEOUT_MS`, 20 s): the longest gap
//!   between two reads of the body, so a slow body that keeps arriving is
//!   never cut off;
//! - **attempts** (`DENO_DOWNLOAD_ATTEMPTS`, 4) with a short jittered backoff,
//!   and **total** (`DENO_DOWNLOAD_TOTAL_TIMEOUT_MS`, 120 s): no new attempt
//!   starts once this much time has passed since the first.
//!
//! `0` turns a deadline off; a value that isn't a whole number keeps the
//! default.

use std::future::Future;
use std::sync::LazyLock;
use std::time::Duration;
use std::time::Instant;

use deno_core::url::Url;
use deno_error::JsError;
use rand::Rng;
use thiserror::Error;

pub const CONNECT_TIMEOUT_ENV: &str = "DENO_DOWNLOAD_CONNECT_TIMEOUT_MS";
pub const RESPONSE_TIMEOUT_ENV: &str = "DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS";
pub const IDLE_TIMEOUT_ENV: &str = "DENO_DOWNLOAD_IDLE_TIMEOUT_MS";
pub const ATTEMPTS_ENV: &str = "DENO_DOWNLOAD_ATTEMPTS";
pub const TOTAL_TIMEOUT_ENV: &str = "DENO_DOWNLOAD_TOTAL_TIMEOUT_MS";

const DEFAULT_CONNECT: Duration = Duration::from_secs(10);
const DEFAULT_RESPONSE: Duration = Duration::from_secs(15);
const DEFAULT_IDLE: Duration = Duration::from_secs(20);
const DEFAULT_ATTEMPTS: u32 = 4;
const DEFAULT_TOTAL: Duration = Duration::from_secs(120);

/// The first retry waits about this long; each later one twice as long.
const RETRY_BASE: Duration = Duration::from_millis(250);
const RETRY_CAP: Duration = Duration::from_secs(4);

/// The deadlines and retry budget every CLI download runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadDeadlines {
  pub connect: Option<Duration>,
  pub response: Option<Duration>,
  pub idle: Option<Duration>,
  /// At least 1.
  pub attempts: u32,
  pub total: Option<Duration>,
}

impl Default for DownloadDeadlines {
  fn default() -> Self {
    Self {
      connect: Some(DEFAULT_CONNECT),
      response: Some(DEFAULT_RESPONSE),
      idle: Some(DEFAULT_IDLE),
      attempts: DEFAULT_ATTEMPTS,
      total: Some(DEFAULT_TOTAL),
    }
  }
}

impl DownloadDeadlines {
  /// The deadlines the `DENO_DOWNLOAD_*` variables set, read once.
  pub fn from_env() -> Self {
    static DEADLINES: LazyLock<DownloadDeadlines> = LazyLock::new(|| {
      DownloadDeadlines::parse(|name| std::env::var(name).ok())
    });
    *DEADLINES
  }

  pub fn parse(env: impl Fn(&str) -> Option<String>) -> Self {
    let number = |name: &str| {
      env(name)
        .as_deref()
        .map(str::trim)
        .and_then(|raw| raw.parse::<u64>().ok())
    };
    let deadline = |name: &str, default: Duration| match number(name) {
      Some(0) => None,
      Some(ms) => Some(Duration::from_millis(ms)),
      None => Some(default),
    };
    Self {
      connect: deadline(CONNECT_TIMEOUT_ENV, DEFAULT_CONNECT),
      response: deadline(RESPONSE_TIMEOUT_ENV, DEFAULT_RESPONSE),
      idle: deadline(IDLE_TIMEOUT_ENV, DEFAULT_IDLE),
      attempts: match number(ATTEMPTS_ENV) {
        Some(n) if n >= 1 => n.min(u32::MAX as u64) as u32,
        _ => DEFAULT_ATTEMPTS,
      },
      total: deadline(TOTAL_TIMEOUT_ENV, DEFAULT_TOTAL),
    }
  }
}

/// Which deadline expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineKind {
  /// The TCP connect.
  Connect,
  /// The response headers.
  Response,
  /// The body stopped arriving.
  Idle,
}

impl DeadlineKind {
  pub fn env_var(self) -> &'static str {
    match self {
      Self::Connect => CONNECT_TIMEOUT_ENV,
      Self::Response => RESPONSE_TIMEOUT_ENV,
      Self::Idle => IDLE_TIMEOUT_ENV,
    }
  }

  fn describe(self, after: Duration) -> String {
    let after = format_duration(after);
    match self {
      Self::Connect => format!("could not connect within {after}"),
      Self::Response => format!("no response within {after}"),
      Self::Idle => format!("no data received for {after}"),
    }
  }
}

/// One request that hit a deadline.
#[derive(Debug, Error, JsError)]
#[class("TimedOut")]
#[error("{}", self.message())]
pub struct DeadlineError {
  pub url: Url,
  pub kind: DeadlineKind,
  pub after: Duration,
}

impl DeadlineError {
  fn message(&self) -> String {
    format!(
      "Timed out downloading '{}': {} ({} sets the deadline, 0 for none)",
      self.url,
      self.kind.describe(self.after),
      self.kind.env_var()
    )
  }
}

/// Why one attempt of a download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptFailure {
  Deadline(DeadlineKind, Duration),
  Error(String),
}

impl std::fmt::Display for AttemptFailure {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Deadline(kind, after) => {
        write!(f, "{} ({})", kind.describe(*after), kind.env_var())
      }
      Self::Error(message) => {
        // The first line is enough here; the full text was already logged
        // at debug level when the attempt failed.
        write!(f, "{}", message.lines().next().unwrap_or_default())
      }
    }
  }
}

/// A download whose attempts all failed, at least one on a deadline.
#[derive(Debug, Error, JsError)]
#[class("TimedOut")]
#[error("{}", self.message())]
pub struct AttemptsError {
  pub url: Url,
  pub attempts: Vec<AttemptFailure>,
  /// Set when the overall budget, not the attempt count, stopped the retries.
  pub stopped_by_total: Option<Duration>,
}

impl AttemptsError {
  fn message(&self) -> String {
    let count = self.attempts.len();
    let mut message = format!(
      "Timed out downloading '{}' ({count} {}, each on a new connection):",
      self.url,
      if count == 1 { "attempt" } else { "attempts" }
    );
    for (i, failure) in self.attempts.iter().enumerate() {
      message.push_str(&format!("\n    attempt {}: {failure}", i + 1));
    }
    if let Some(total) = self.stopped_by_total {
      message.push_str(&format!(
        "\n    no more attempts after {} ({TOTAL_TIMEOUT_ENV})",
        format_duration(total)
      ));
    }
    message.push_str(&format!(
      "\n    (set {ATTEMPTS_ENV} or the deadline variables to change this, 0 for no deadline)"
    ));
    message
  }
}

/// Counts a download's failed attempts against [`DownloadDeadlines`].
#[derive(Debug)]
pub struct RetryBudget {
  deadlines: DownloadDeadlines,
  started: Instant,
  failures: Vec<AttemptFailure>,
  stopped_by_total: bool,
}

impl RetryBudget {
  pub fn new(deadlines: DownloadDeadlines) -> Self {
    Self {
      deadlines,
      started: Instant::now(),
      failures: Vec::new(),
      stopped_by_total: false,
    }
  }

  /// Records a failed attempt. Returns how long to wait before the next one,
  /// or `None` when the attempts or the overall budget are spent.
  pub fn record(&mut self, failure: AttemptFailure) -> Option<Duration> {
    self.failures.push(failure);
    let retries = self.failures.len() as u32;
    if retries >= self.deadlines.attempts {
      return None;
    }
    let wait = retry_wait(retries);
    if let Some(total) = self.deadlines.total
      && self.started.elapsed() + wait >= total
    {
      self.stopped_by_total = true;
      return None;
    }
    Some(wait)
  }

  /// Whether any attempt so far failed on a deadline.
  pub fn saw_deadline(&self) -> bool {
    self
      .failures
      .iter()
      .any(|f| matches!(f, AttemptFailure::Deadline(..)))
  }

  pub fn into_error(self, url: Url) -> AttemptsError {
    AttemptsError {
      url,
      attempts: self.failures,
      stopped_by_total: if self.stopped_by_total {
        self.deadlines.total
      } else {
        None
      },
    }
  }
}

/// The wait before retry number `retry` (from 1): exponential from 250 ms,
/// capped at 4 s, with "equal jitter" (half fixed, half random) so many
/// downloads failing together don't retry in lockstep.
pub fn retry_wait(retry: u32) -> Duration {
  let exp = RETRY_BASE
    .saturating_mul(1u32 << retry.saturating_sub(1).min(16))
    .min(RETRY_CAP);
  let half = exp / 2;
  let jitter_ms = rand::thread_rng().gen_range(0..=half.as_millis() as u64);
  half + Duration::from_millis(jitter_ms)
}

/// Runs `fut`, or gives up after `limit` (`None`: no limit) and returns it.
pub async fn within<T>(
  limit: Option<Duration>,
  fut: impl Future<Output = T>,
) -> Result<T, Duration> {
  match limit {
    Some(limit) => tokio::time::timeout(limit, fut).await.map_err(|_| limit),
    None => Ok(fut.await),
  }
}

pub fn format_duration(after: Duration) -> String {
  let ms = after.as_millis();
  if ms.is_multiple_of(1000) {
    format!("{}s", ms / 1000)
  } else {
    format!("{ms}ms")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_deadlines() {
    let parse = |vars: &[(&str, &str)]| {
      let vars = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<Vec<_>>();
      DownloadDeadlines::parse(move |name| {
        vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
      })
    };
    assert_eq!(parse(&[]), DownloadDeadlines::default());
    assert_eq!(
      parse(&[]),
      DownloadDeadlines {
        connect: Some(Duration::from_secs(10)),
        response: Some(Duration::from_secs(15)),
        idle: Some(Duration::from_secs(20)),
        attempts: 4,
        total: Some(Duration::from_secs(120)),
      }
    );
    assert_eq!(
      parse(&[
        (CONNECT_TIMEOUT_ENV, "0"),
        (RESPONSE_TIMEOUT_ENV, " 1500 "),
        (IDLE_TIMEOUT_ENV, "soon"),
        (ATTEMPTS_ENV, "0"),
        (TOTAL_TIMEOUT_ENV, "0"),
      ]),
      DownloadDeadlines {
        connect: None,
        response: Some(Duration::from_millis(1500)),
        idle: Some(Duration::from_secs(20)),
        attempts: 4,
        total: None,
      }
    );
    assert_eq!(parse(&[(ATTEMPTS_ENV, "7")]).attempts, 7);
  }

  #[test]
  fn retry_waits_grow_with_jitter() {
    for _ in 0..100 {
      let first = retry_wait(1);
      assert!(first >= Duration::from_millis(125), "{first:?}");
      assert!(first <= Duration::from_millis(250), "{first:?}");
      let late = retry_wait(10);
      assert!(late >= Duration::from_secs(2), "{late:?}");
      assert!(late <= Duration::from_secs(4), "{late:?}");
    }
  }

  #[test]
  fn retry_budget_counts_attempts_and_total() {
    let mut budget = RetryBudget::new(DownloadDeadlines {
      attempts: 3,
      ..Default::default()
    });
    let stall =
      AttemptFailure::Deadline(DeadlineKind::Response, Duration::from_secs(15));
    assert!(budget.record(stall.clone()).is_some());
    assert!(
      budget
        .record(AttemptFailure::Error("reset".into()))
        .is_some()
    );
    assert!(budget.record(stall.clone()).is_none());
    assert!(budget.saw_deadline());
    let url = Url::parse("https://registry.example/pkg").unwrap();
    assert_eq!(
      budget.into_error(url.clone()).to_string(),
      "Timed out downloading 'https://registry.example/pkg' (3 attempts, each on a new connection):
    attempt 1: no response within 15s (DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS)
    attempt 2: reset
    attempt 3: no response within 15s (DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS)
    (set DENO_DOWNLOAD_ATTEMPTS or the deadline variables to change this, 0 for no deadline)"
    );

    let mut budget = RetryBudget::new(DownloadDeadlines {
      total: Some(Duration::from_millis(1)),
      ..Default::default()
    });
    assert!(budget.record(stall).is_none());
    assert!(
      budget.into_error(url).to_string().contains(
        "no more attempts after 1ms (DENO_DOWNLOAD_TOTAL_TIMEOUT_MS)"
      )
    );
  }

  #[test]
  fn deadline_error_message() {
    let err = DeadlineError {
      url: Url::parse("https://jsr.io/@std/fs/meta.json").unwrap(),
      kind: DeadlineKind::Idle,
      after: Duration::from_secs(20),
    };
    assert_eq!(
      err.to_string(),
      "Timed out downloading 'https://jsr.io/@std/fs/meta.json': no data received for 20s (DENO_DOWNLOAD_IDLE_TIMEOUT_MS sets the deadline, 0 for none)"
    );
  }
}
