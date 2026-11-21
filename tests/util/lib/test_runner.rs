// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::Hasher;
use std::io::IsTerminal;
use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::channel;
use std::time::Duration;
use std::time::Instant;

use console_static_text::ConsoleStaticText;
use file_test_runner::TestResult;
use file_test_runner::collection::CollectedCategoryOrTest;
use file_test_runner::collection::CollectedTestCategory;
use file_test_runner::reporter::LogReporter;
use parking_lot::Mutex;
use serde::Serialize;

use crate::IS_CI;
use crate::colors;
use crate::semaphore::Semaphore;

pub struct ShardConfig {
  pub index: usize,
  pub total: usize,
}

impl ShardConfig {
  /// Reads CI_SHARD_INDEX and CI_SHARD_TOTAL from env.
  /// Returns None if not set or total <= 1.
  pub fn from_env() -> Option<Self> {
    let total: usize = std::env::var("CI_SHARD_TOTAL").ok()?.parse().ok()?;
    let index: usize = std::env::var("CI_SHARD_INDEX").ok()?.parse().ok()?;
    if total <= 1 {
      return None;
    }
    Some(Self { index, total })
  }
}

/// Filter a collected test category to only include tests assigned to this shard.
/// Distributes by a hash of the test name.
pub fn filter_to_shard<T>(
  category: CollectedTestCategory<T>,
  shard: &ShardConfig,
) -> CollectedTestCategory<T> {
  let all_names = collect_test_names(&category);

  let total_count = all_names.len();
  let my_tests = assign_shard_tests(all_names, shard);

  let shard_count = my_tests.len();
  crate::eprintln!(
    "shard {}/{}: running {shard_count}/{total_count} tests",
    shard.index + 1,
    shard.total,
  );

  let (mine, _) = category.partition(|test| my_tests.contains(&test.name));
  mine
}

fn collect_test_names<T>(category: &CollectedTestCategory<T>) -> Vec<String> {
  let mut names = Vec::new();
  fn walk<T>(children: &[CollectedCategoryOrTest<T>], names: &mut Vec<String>) {
    for child in children {
      match child {
        CollectedCategoryOrTest::Test(t) => names.push(t.name.clone()),
        CollectedCategoryOrTest::Category(c) => walk(&c.children, names),
      }
    }
  }
  walk(&category.children, &mut names);
  names
}

fn assign_shard_tests(
  all_names: Vec<String>,
  shard: &ShardConfig,
) -> HashSet<String> {
  all_names
    .into_iter()
    .filter(|name| shard_index_of(name, shard.total) == shard.index)
    .collect()
}

/// Which shard a test belongs to, derived only from its own name.
///
/// Deliberately *not* round-robin over the sorted test list: with positional
/// assignment, adding or removing a single test shifts every test after it
/// into a different shard. That made suite updates (e.g. bumping the vendored
/// node_compat suite) silently re-deal expensive tests onto one runner, which
/// then ran out of memory. Hashing the name keeps every other test where it
/// was, so a suite bump only moves the tests it actually adds or removes.
///
/// Shards end up approximately, rather than exactly, even in size. That is the
/// intended trade: stability matters more here than a perfect split.
fn shard_index_of(name: &str, shard_total: usize) -> usize {
  let mut hasher = twox_hash::XxHash64::default();
  hasher.write(name.as_bytes());
  (hasher.finish() % shard_total as u64) as usize
}

/// Tracks the number of times each test has been flaky
pub struct FlakyTestTracker {
  flaky_counts: Mutex<HashMap<String, usize>>,
}

impl FlakyTestTracker {
  pub fn record_flaky(&self, test_name: &str) {
    let mut counts = self.flaky_counts.lock();
    *counts.entry(test_name.to_string()).or_insert(0) += 1;
  }

  pub fn get_count(&self, test_name: &str) -> usize {
    let counts = self.flaky_counts.lock();
    counts.get(test_name).copied().unwrap_or(0)
  }
}

impl Default for FlakyTestTracker {
  fn default() -> Self {
    Self {
      flaky_counts: Mutex::new(HashMap::new()),
    }
  }
}

pub fn flaky_test_ci(
  test_name: &str,
  flaky_test_tracker: &FlakyTestTracker,
  parallelism: Option<&Parallelism>,
  run_test: impl Fn() -> TestResult,
) -> TestResult {
  run_maybe_flaky_test(
    test_name,
    *IS_CI,
    flaky_test_tracker,
    parallelism,
    run_test,
  )
}

/// Coordinates semaphore max adjustments between the memory monitor
/// and the flaky test single-concurrency mode, preventing race conditions.
struct ParallelismController {
  semaphore: Semaphore,
  max_parallelism: usize,
  state: Mutex<ControllerState>,
}

struct ControllerState {
  single_concurrency_count: usize,
}

impl ParallelismController {
  fn new(max: usize) -> Self {
    Self {
      semaphore: Semaphore::new(max),
      max_parallelism: max,
      state: Mutex::new(ControllerState {
        single_concurrency_count: 0,
      }),
    }
  }

  fn acquire(&self) -> crate::semaphore::Permit<'_> {
    self.semaphore.acquire()
  }

  fn enter_single_concurrency(&self) {
    let mut state = self.state.lock();
    if state.single_concurrency_count == 0 {
      self.semaphore.set_max(1);
    }
    state.single_concurrency_count += 1;
  }

  fn exit_single_concurrency(&self) {
    let mut state = self.state.lock();
    state.single_concurrency_count -= 1;
    if state.single_concurrency_count == 0 {
      // restore to max_parallelism; if memory is still low the
      // monitor will re-reduce on its next check
      self.semaphore.set_max(self.max_parallelism);
    }
  }

  /// Try to reduce parallelism by 1 for memory pressure.
  /// Returns false if single-concurrency mode is active or already at 1.
  fn try_reduce_for_memory(&self) -> bool {
    let state = self.state.lock();
    if state.single_concurrency_count > 0 {
      return false;
    }
    let current = self.semaphore.get_max();
    if current > 1 {
      self.semaphore.set_max(current - 1);
      true
    } else {
      false
    }
  }

  /// Try to increase parallelism by 1 after memory recovery.
  /// Returns false if single-concurrency mode is active or already at max.
  fn try_increase_for_memory(&self) -> bool {
    let state = self.state.lock();
    if state.single_concurrency_count > 0 {
      return false;
    }
    let current = self.semaphore.get_max();
    if current < self.max_parallelism {
      self.semaphore.set_max(current + 1);
      true
    } else {
      false
    }
  }
}

struct SingleConcurrencyFlagGuard<'a>(&'a Parallelism);

impl<'a> Drop for SingleConcurrencyFlagGuard<'a> {
  fn drop(&mut self) {
    self.0.controller.exit_single_concurrency();
  }
}

pub struct Parallelism {
  controller: Arc<ParallelismController>,
  max_parallelism: file_test_runner::Parallelism,
  // dropping this shuts down the memory monitor thread
  _monitor_tx: Option<std::sync::mpsc::Sender<()>>,
}

impl Default for Parallelism {
  fn default() -> Self {
    let parallelism = file_test_runner::Parallelism::default();
    let controller = Arc::new(ParallelismController::new(parallelism.get()));
    let monitor_tx = spawn_memory_monitor(controller.clone());
    Self {
      max_parallelism: parallelism,
      controller,
      _monitor_tx: monitor_tx,
    }
  }
}

fn spawn_memory_monitor(
  controller: Arc<ParallelismController>,
) -> Option<std::sync::mpsc::Sender<()>> {
  let info = crate::memory::mem_info()?;
  let threshold = info.total / 10; // 10% of total memory

  let (tx, rx) = std::sync::mpsc::channel::<()>();
  crate::print::spawn_thread(move || {
    loop {
      match rx.recv_timeout(Duration::from_secs(2)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        // channel closed, exit
        _ => return,
      }
      let Some(current) = crate::memory::mem_info() else {
        continue;
      };
      if current.available < threshold {
        controller.try_reduce_for_memory();
      } else {
        controller.try_increase_for_memory();
      }
    }
  });

  Some(tx)
}

impl Parallelism {
  pub fn max_parallelism(&self) -> file_test_runner::Parallelism {
    self.max_parallelism
  }

  fn acquire(&self) -> crate::semaphore::Permit<'_> {
    self.controller.acquire()
  }

  fn raise_single_concurrency_flag(&self) -> SingleConcurrencyFlagGuard<'_> {
    self.controller.enter_single_concurrency();
    SingleConcurrencyFlagGuard(self)
  }
}

pub fn run_maybe_flaky_test(
  test_name: &str,
  is_flaky: bool,
  flaky_test_tracker: &FlakyTestTracker,
  parallelism: Option<&Parallelism>,
  main_action: impl Fn() -> TestResult,
) -> TestResult {
  let ci_parallelism = parallelism.filter(|_| *IS_CI);
  let action = || run_with_parallelism(ci_parallelism, &main_action);
  if !is_flaky {
    return action();
  }
  for i in 0..2 {
    let result = action();
    if !result.is_failed() {
      return result;
    }
    flaky_test_tracker.record_flaky(test_name);
    #[allow(clippy::print_stderr, reason = "test reporter")]
    if *IS_CI {
      ::std::eprintln!(
        "{} {} was flaky on run {}",
        colors::bold_red("Warning"),
        colors::gray(test_name),
        i,
      );
      // denext: the failed attempt's output, so a flake's cause is in the
      // log even when a retry passes (it was dropped before).
      ::std::eprintln!("{}", flaky_failure_output(&result));
    }
    std::thread::sleep(Duration::from_millis(100));
  }

  // on the CI, try running the test in isolation with no other tests running
  #[allow(clippy::print_stderr, reason = "test reporter")]
  let _maybe_guard = if let Some(parallelism) = ci_parallelism {
    let guard = parallelism.raise_single_concurrency_flag();
    ::std::eprintln!(
      "{} {} was flaky. Temporarily reducing test concurrency to 1 and trying a few more times.",
      colors::bold_red("***WARNING***"),
      colors::gray(test_name)
    );

    for _ in 0..2 {
      let result = action();
      if !result.is_failed() {
        return result;
      }
      flaky_test_tracker.record_flaky(test_name);
      std::thread::sleep(Duration::from_millis(100));
    }

    Some(guard)
  } else {
    None
  };

  // surface result now
  action()
}

/// The most a flaky attempt's failure output adds to the log: its end, where
/// the panic and the assertion are.
const FLAKY_OUTPUT_LIMIT: usize = 8 * 1024;

/// The captured output of a failed attempt (each failed sub test's, named),
/// cut to its last `FLAKY_OUTPUT_LIMIT` bytes (denext).
fn flaky_failure_output(result: &TestResult) -> String {
  fn collect(result: &TestResult, prefix: &str, out: &mut Vec<u8>) {
    match result {
      TestResult::Failed { output, .. } => {
        if !prefix.is_empty() {
          out.extend_from_slice(format!("--- {prefix} ---\n").as_bytes());
        }
        out.extend_from_slice(output);
      }
      TestResult::SubTests { sub_tests, .. } => {
        for sub in sub_tests {
          let name = if prefix.is_empty() {
            sub.name.clone()
          } else {
            format!("{prefix}::{}", sub.name)
          };
          collect(&sub.result, &name, out);
        }
      }
      TestResult::Passed { .. } | TestResult::Ignored => {}
    }
  }
  let mut out = Vec::new();
  collect(result, "", &mut out);
  let start = out.len().saturating_sub(FLAKY_OUTPUT_LIMIT);
  let text = String::from_utf8_lossy(&out[start..]);
  if start > 0 {
    format!("[... {start} bytes cut]\n{text}")
  } else {
    text.into_owned()
  }
}

fn run_with_parallelism(
  parallelism: Option<&Parallelism>,
  action: impl Fn() -> TestResult,
) -> TestResult {
  let _maybe_permit = parallelism.map(|p| p.acquire());
  let duration = std::time::Instant::now();
  let result = action();
  result.with_duration(duration.elapsed())
}

// denext: per-test timeouts scaled by the CPU each running test gets, as
// Node's own tools/test.py scales its timeouts and `common.platformTimeout`
// lengthens a test's waits on a slow machine. A runner with at least one CPU
// per test keeps the timeouts as written (factor 1); a test that shares a CPU
// (a container capped below the harness's parallelism, an over-subscribed
// host's quota) gets a longer one, at most `MAX_TIMEOUT_SCALE` times as long,
// so a real hang still fails.

/// The longest a scaled timeout gets, relative to the written one.
pub const MAX_TIMEOUT_SCALE: f64 = 8.0;

/// The CPUs this process may use, and where the number came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvailableCpus {
  pub count: f64,
  /// `"cgroup cpu quota"` or `"host CPUs"`.
  pub source: &'static str,
}

impl AvailableCpus {
  /// The cgroup's CPU quota (v2 `cpu.max`, else v1's CFS quota, the tightest
  /// along the cgroup's ancestors) when there is one, at most the host's CPU
  /// count (which counts the CPU affinity mask), else the host's CPU count.
  pub fn detect() -> Self {
    let host = std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(1) as f64;
    match cgroup_cpu_quota() {
      Some(quota) if quota <= host => Self {
        count: quota,
        source: "cgroup cpu quota",
      },
      _ => Self {
        count: host,
        source: "host CPUs",
      },
    }
  }
}

/// How much longer a test's timeout is when `parallelism` tests share
/// `available_cpus`: clamp(1 / min(1, CPU per test), 1, MAX_TIMEOUT_SCALE).
pub fn timeout_scale_factor(available_cpus: f64, parallelism: usize) -> f64 {
  let per_test = available_cpus / parallelism.max(1) as f64;
  if !per_test.is_finite() || per_test <= 0.0 {
    return MAX_TIMEOUT_SCALE;
  }
  (1.0 / per_test.min(1.0)).clamp(1.0, MAX_TIMEOUT_SCALE)
}

/// `timeout` scaled by `factor` (whole milliseconds; a factor of 1 leaves it
/// exactly as it was).
pub fn scale_timeout(timeout: Duration, factor: f64) -> Duration {
  if factor <= 1.0 {
    return timeout;
  }
  Duration::from_millis((timeout.as_millis() as f64 * factor).round() as u64)
}

/// A cgroup v2 `cpu.max` (`"<quota> <period>"`, or `"max <period>"` for no
/// quota) as CPUs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cgroup_v2_cpu_max(text: &str) -> Option<f64> {
  let mut parts = text.split_whitespace();
  let quota = parts.next()?;
  let period = parts.next().map_or(Some(100_000.0), |p| p.parse().ok())?;
  cpus_from_quota(quota.parse().ok()?, period)
}

/// A cgroup v1 `cpu.cfs_quota_us` / `cpu.cfs_period_us` pair as CPUs (a
/// quota of -1 is none).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cgroup_v1_cfs(quota: &str, period: &str) -> Option<f64> {
  cpus_from_quota(quota.trim().parse().ok()?, period.trim().parse().ok()?)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cpus_from_quota(quota: f64, period: f64) -> Option<f64> {
  (quota > 0.0 && period > 0.0).then(|| quota / period)
}

/// `/proc/self/cgroup`'s cgroup v2 path (the `0::<path>` line) and its
/// cgroup v1 path for the `cpu` controller.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_self_cgroup(text: &str) -> (Option<&str>, Option<&str>) {
  let mut v2 = None;
  let mut v1 = None;
  for line in text.lines() {
    let mut fields = line.splitn(3, ':');
    let (Some(id), Some(controllers), Some(path)) =
      (fields.next(), fields.next(), fields.next())
    else {
      continue;
    };
    if id == "0" && controllers.is_empty() {
      v2 = Some(path);
    } else if controllers.split(',').any(|c| c == "cpu") {
      v1 = Some(path);
    }
  }
  (v2, v1)
}

/// The directories from `base`/`path` up to `base` (a path that escapes the
/// mount, as a cgroup namespace's `/..` does, starts at `base`).
#[cfg(target_os = "linux")]
fn cgroup_dirs(base: &std::path::Path, path: &str) -> Vec<std::path::PathBuf> {
  let rel = path.trim_start_matches('/');
  let start = if rel.split('/').any(|c| c == "..") {
    base.to_path_buf()
  } else {
    base.join(rel)
  };
  start
    .ancestors()
    .take_while(|dir| dir.starts_with(base))
    .map(|dir| dir.to_path_buf())
    .collect()
}

#[cfg(target_os = "linux")]
fn cgroup_cpu_quota() -> Option<f64> {
  use std::path::Path;
  let read = |p: &Path| std::fs::read_to_string(p).ok();
  let min = |a: Option<f64>, b: Option<f64>| match (a, b) {
    (Some(a), Some(b)) => Some(a.min(b)),
    (a, b) => a.or(b),
  };
  let cgroup = read(Path::new("/proc/self/cgroup")).unwrap_or_default();
  let (v2, v1) = parse_proc_self_cgroup(&cgroup);
  let mut quota = None;
  let base = Path::new("/sys/fs/cgroup");
  if base.join("cgroup.controllers").exists() {
    for dir in cgroup_dirs(base, v2.unwrap_or("/")) {
      let q =
        read(&dir.join("cpu.max")).and_then(|t| parse_cgroup_v2_cpu_max(&t));
      quota = min(quota, q);
    }
    return quota;
  }
  for base in ["/sys/fs/cgroup/cpu,cpuacct", "/sys/fs/cgroup/cpu"] {
    let base = Path::new(base);
    if !base.exists() {
      continue;
    }
    for dir in cgroup_dirs(base, v1.unwrap_or("/")) {
      let q = match (
        read(&dir.join("cpu.cfs_quota_us")),
        read(&dir.join("cpu.cfs_period_us")),
      ) {
        (Some(q), Some(p)) => parse_cgroup_v1_cfs(&q, &p),
        _ => None,
      };
      quota = min(quota, q);
    }
    break;
  }
  quota
}

#[cfg(not(target_os = "linux"))]
fn cgroup_cpu_quota() -> Option<f64> {
  None
}

pub struct TestTimeoutHolder {
  _tx: std::sync::mpsc::Sender<()>,
}

pub fn with_timeout(
  test_name: String,
  duration: Duration,
) -> TestTimeoutHolder {
  let (tx, rx) = ::std::sync::mpsc::channel::<()>();
  crate::print::spawn_thread(move || {
    if rx.recv_timeout(duration)
      == Err(::std::sync::mpsc::RecvTimeoutError::Timeout)
    {
      use std::io::Write;
      #[allow(clippy::print_stderr, reason = "test reporter")]
      {
        ::std::eprintln!(
          "Test {test_name} timed out after {} seconds, aborting",
          duration.as_secs()
        );
      }
      _ = std::io::stderr().flush();
      #[allow(clippy::disallowed_methods, reason = "test code")]
      ::std::process::exit(1);
    }
  });
  TestTimeoutHolder { _tx: tx }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedTestResult {
  name: String,
  path: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  duration: Option<u128>,
  #[serde(skip_serializing_if = "is_false")]
  failed: bool,
  #[serde(skip_serializing_if = "is_false")]
  ignored: bool,
  #[serde(skip_serializing_if = "is_zero")]
  flaky_count: usize,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  sub_tests: Vec<RecordedTestResult>,
}

fn is_false(value: &bool) -> bool {
  !value
}

fn is_zero(value: &usize) -> bool {
  *value == 0
}

#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedReport {
  tests: Vec<RecordedTestResult>,
}

struct JsonReporter {
  data: Arc<Mutex<RecordedReport>>,
  flaky_tracker: Arc<FlakyTestTracker>,
  test_module_name: String,
}

impl JsonReporter {
  pub fn new(
    flaky_tracker: Arc<FlakyTestTracker>,
    test_module_name: String,
  ) -> Self {
    Self {
      data: Default::default(),
      flaky_tracker,
      test_module_name,
    }
  }

  fn write_report_to_file(&self) {
    let json = {
      let data = self.data.lock();
      serde_json::to_string(&*data).unwrap()
    };
    let file_path = crate::root_path()
      .join("target")
      .join(format!("test_results_{}.json", self.test_module_name));

    file_path.write(json);
  }

  fn flatten_and_record_test(
    &self,
    tests: &mut Vec<RecordedTestResult>,
    test_name: String,
    path: String,
    result: &TestResult,
    main_duration: Option<Duration>,
  ) {
    match result {
      TestResult::SubTests {
        sub_tests,
        duration,
      } => {
        let mut sub_test_results = Vec::with_capacity(sub_tests.len());
        for sub_test in sub_tests {
          let full_name = format!("{}::{}", test_name, sub_test.name);
          self.flatten_and_record_test(
            &mut sub_test_results,
            full_name,
            path.clone(),
            &sub_test.result,
            None,
          );
        }
        let flaky_count = self.flaky_tracker.get_count(&test_name);
        tests.push(RecordedTestResult {
          name: test_name,
          path,
          duration: duration.or(main_duration).map(|d| d.as_millis()),
          failed: sub_tests.iter().any(|s| s.result.is_failed()),
          ignored: false,
          flaky_count,
          sub_tests: sub_test_results,
        })
      }
      TestResult::Passed { duration } => {
        let flaky_count = self.flaky_tracker.get_count(&test_name);
        let test_result = RecordedTestResult {
          name: test_name,
          path,
          duration: duration.or(main_duration).map(|d| d.as_millis()),
          failed: false,
          ignored: false,
          flaky_count,
          sub_tests: Vec::new(),
        };
        tests.push(test_result);
      }
      TestResult::Failed { duration, .. } => {
        let flaky_count = self.flaky_tracker.get_count(&test_name);
        let test_result = RecordedTestResult {
          name: test_name,
          path,
          duration: duration.or(main_duration).map(|d| d.as_millis()),
          failed: true,
          ignored: false,
          flaky_count,
          sub_tests: Vec::new(),
        };
        tests.push(test_result.clone());
      }
      TestResult::Ignored => {
        let flaky_count = self.flaky_tracker.get_count(&test_name);
        let test_result = RecordedTestResult {
          name: test_name,
          path,
          duration: None,
          failed: false,
          ignored: true,
          flaky_count,
          sub_tests: Vec::new(),
        };
        tests.push(test_result);
      }
    }
  }
}

impl<TData> file_test_runner::reporter::Reporter<TData> for JsonReporter {
  fn report_category_start(
    &self,
    _category: &file_test_runner::collection::CollectedTestCategory<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
  }

  fn report_category_end(
    &self,
    _category: &file_test_runner::collection::CollectedTestCategory<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
  }

  fn report_test_start(
    &self,
    _test: &file_test_runner::collection::CollectedTest<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
  }

  fn report_test_end(
    &self,
    test: &file_test_runner::collection::CollectedTest<TData>,
    duration: Duration,
    result: &TestResult,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
    let mut data = self.data.lock();

    let relative_path = test
      .path
      .strip_prefix(crate::root_path())
      .unwrap_or(&test.path);
    let path = match test.line_and_column {
      Some((line, col)) => {
        format!("{}:{}:{}", relative_path.display(), line + 1, col + 1)
      }
      None => relative_path.display().to_string(),
    }
    .replace("\\", "/");

    // Use the helper function to recursively flatten subtests
    self.flatten_and_record_test(
      &mut data.tests,
      test.name.to_string(),
      path,
      result,
      Some(duration),
    );
  }

  fn report_failures(
    &self,
    _failures: &[file_test_runner::reporter::ReporterFailure<TData>],
    _total_tests: usize,
  ) {
    // Write the report to file when failures are reported (at the end of test run)
    self.write_report_to_file();
  }
}

pub trait ReporterData {
  fn times_flaky() -> usize;
}

/// Checks if `--list` was passed in CLI args. If so, prints all test names
/// in libtest format and returns `true` (caller should exit early).
pub fn print_tests_if_list_flag<TData>(
  category: &CollectedTestCategory<TData>,
) -> bool {
  if !std::env::args().any(|arg| arg == "--list") {
    return false;
  }
  fn print_tests_recursive<T>(category: &CollectedTestCategory<T>) {
    for child in &category.children {
      match child {
        CollectedCategoryOrTest::Category(c) => print_tests_recursive(c),
        CollectedCategoryOrTest::Test(t) => {
          #[allow(clippy::print_stdout, reason = "test code")]
          {
            println!("{}: test", t.name);
          }
        }
      }
    }
  }
  print_tests_recursive(category);
  true
}

pub fn get_test_reporter<TData: 'static>(
  test_module_name: &str,
  flaky_test_tracker: Arc<FlakyTestTracker>,
) -> Arc<dyn file_test_runner::reporter::Reporter<TData>> {
  let mut reporters: Vec<Box<dyn file_test_runner::reporter::Reporter<TData>>> =
    Vec::with_capacity(2);
  reporters.push(get_display_reporter());
  if *IS_CI {
    reporters.push(Box::new(JsonReporter::new(
      flaky_test_tracker,
      test_module_name.to_string(),
    )));
  }
  Arc::new(file_test_runner::reporter::AggregateReporter::new(
    reporters,
  ))
}

fn get_display_reporter<TData>()
-> Box<dyn file_test_runner::reporter::Reporter<TData>> {
  if *file_test_runner::NO_CAPTURE
    || *IS_CI
    || !std::io::stderr().is_terminal()
    || std::env::var("DENO_TEST_UTIL_REPORTER").ok().as_deref() == Some("log")
  {
    Box::new(file_test_runner::reporter::LogReporter::default())
  } else {
    Box::new(PtyReporter::new())
  }
}

struct PtyReporterPendingTest {
  name: String,
  start_time: Instant,
}

struct PtyReporterFailedTest {
  name: String,
  path: String,
}

struct PtyReporterData {
  static_text: ConsoleStaticText,
  pending_tests: Vec<PtyReporterPendingTest>,
  failed_tests: Vec<PtyReporterFailedTest>,
  passed_tests: usize,
  ignored_tests: usize,
  hide: bool,
}

impl PtyReporterData {
  pub fn render_clear(&mut self) -> String {
    self.static_text.render_clear().unwrap_or_default()
  }

  pub fn render(&mut self) -> Option<String> {
    if self.hide {
      return Some(self.render_clear());
    }
    let mut items = Vec::new();
    const MAX_ITEM_DISPLAY: usize = 10;
    if !self.pending_tests.is_empty() {
      let text = if self.pending_tests.len() > MAX_ITEM_DISPLAY {
        "oldest pending:"
      } else {
        "pending:"
      };
      items.push(console_static_text::TextItem::Text(
        colors::yellow(text).into(),
      ));
      items.extend(self.pending_tests.iter().take(MAX_ITEM_DISPLAY).map(
        |item| {
          console_static_text::TextItem::Text(
            format!(
              "- {} ({}s)",
              item.name,
              item.start_time.elapsed().as_secs()
            )
            .into(),
          )
        },
      ));
    }
    if !self.failed_tests.is_empty() {
      items.push(console_static_text::TextItem::Text(
        colors::red("failed:").to_string().into(),
      ));
      for item in self.failed_tests.iter().rev().take(MAX_ITEM_DISPLAY) {
        items.push(console_static_text::TextItem::Text(
          format!("- {} ({})", item.name, colors::gray(&item.path)).into(),
        ));
      }
    }

    items.push(console_static_text::TextItem::Text(
      format!(
        "    {} Pending - {} Passed - {} Failed - {} Ignored",
        self.pending_tests.len(),
        self.passed_tests,
        self.failed_tests.len(),
        self.ignored_tests
      )
      .into(),
    ));

    self.static_text.render_items(items.iter())
  }
}

struct PtyReporter {
  data: Arc<Mutex<PtyReporterData>>,
  _tx: std::sync::mpsc::Sender<()>,
}

impl PtyReporter {
  pub fn new() -> Self {
    let (tx, rx) = channel();
    let data = Arc::new(Mutex::new(PtyReporterData {
      static_text: ConsoleStaticText::new(move || {
        let size = crossterm::terminal::size().ok();
        console_static_text::ConsoleSize {
          cols: size.map(|(cols, _)| cols),
          rows: size.map(|(_, rows)| rows),
        }
      }),
      pending_tests: Default::default(),
      failed_tests: Default::default(),
      passed_tests: Default::default(),
      ignored_tests: Default::default(),
      hide: false,
    }));

    crate::print::spawn_thread({
      let data = data.clone();
      move || {
        loop {
          match rx.recv_timeout(Duration::from_millis(1_000)) {
            Err(RecvTimeoutError::Timeout) => {
              let mut data = data.lock();
              if let Some(text) = data.render() {
                let mut stderr = std::io::stderr().lock();
                _ = stderr.write_all(text.as_bytes());
                _ = stderr.flush();
              }
            }
            _ => {
              return;
            }
          }
        }
      }
    });
    Self { data, _tx: tx }
  }
}

impl<TData> file_test_runner::reporter::Reporter<TData> for PtyReporter {
  fn report_category_start(
    &self,
    category: &file_test_runner::collection::CollectedTestCategory<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
    let mut data = self.data.lock();
    data.hide = false;
    let mut final_text = data.render_clear().into_bytes();
    _ = LogReporter::write_report_category_start(&mut final_text, category);
    if let Some(text) = data.render() {
      final_text.extend_from_slice(text.as_bytes());
    }
    let mut stderr = std::io::stderr().lock();
    _ = stderr.write_all(&final_text);
    _ = stderr.flush();
  }

  fn report_category_end(
    &self,
    _category: &file_test_runner::collection::CollectedTestCategory<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
  }

  fn report_test_start(
    &self,
    test: &file_test_runner::collection::CollectedTest<TData>,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
    let mut data = self.data.lock();
    data.pending_tests.push(PtyReporterPendingTest {
      name: test.name.clone(),
      start_time: std::time::Instant::now(),
    });
    if let Some(final_text) = data.render() {
      let mut stderr = std::io::stderr().lock();
      _ = stderr.write_all(final_text.as_bytes());
      _ = stderr.flush();
    }
  }

  fn report_test_end(
    &self,
    test: &file_test_runner::collection::CollectedTest<TData>,
    duration: Duration,
    result: &TestResult,
    _context: &file_test_runner::reporter::ReporterContext,
  ) {
    let mut data = self.data.lock();
    let clear_text = data.static_text.render_clear().unwrap_or_default();
    if let Some(index) =
      data.pending_tests.iter().position(|t| t.name == test.name)
    {
      data.pending_tests.remove(index);
    }
    match result {
      TestResult::Passed { .. } => {
        data.passed_tests += 1;
      }
      TestResult::Ignored => {
        data.ignored_tests += 1;
      }
      TestResult::Failed { .. } => {
        data.failed_tests.push(PtyReporterFailedTest {
          name: test.name.to_string(),
          path: match test.line_and_column {
            Some((line, col)) => {
              format!("{}:{}:{}", test.path.display(), line + 1, col + 1)
            }
            None => test.path.display().to_string(),
          },
        });
      }
      TestResult::SubTests { .. } => {
        // ignore
      }
    }
    let mut final_text = clear_text.into_bytes();
    _ = LogReporter::write_report_test_end(
      &mut final_text,
      test,
      duration,
      result,
      &file_test_runner::reporter::ReporterContext { is_parallel: true },
    );
    if let Some(text) = data.render() {
      final_text.extend_from_slice(text.as_bytes());
    }
    let mut stderr = std::io::stderr().lock();
    _ = stderr.write_all(&final_text);
    _ = stderr.flush();
  }

  fn report_failures(
    &self,
    failures: &[file_test_runner::reporter::ReporterFailure<TData>],
    total_tests: usize,
  ) {
    let clear_text = {
      let mut data = self.data.lock();
      data.hide = true;
      data.render_clear()
    };
    let mut final_text = clear_text.into_bytes();
    _ = LogReporter::write_report_failures(
      &mut final_text,
      failures,
      total_tests,
    );
    let mut stderr = std::io::stderr().lock();
    _ = stderr.write_all(&final_text);
    _ = stderr.flush();
  }
}

#[cfg(test)]
mod test {
  use super::*;

  // denext: a flaky attempt's failure output reaches the log.
  #[test]
  fn flaky_failure_output_is_the_failed_attempts() {
    let failed = TestResult::Failed {
      duration: None,
      output: b"assertion failed: worker2.js".to_vec(),
    };
    assert_eq!(
      flaky_failure_output(&failed),
      "assertion failed: worker2.js"
    );
    let sub = TestResult::SubTests {
      duration: None,
      sub_tests: vec![
        file_test_runner::SubTestResult {
          name: "ok".to_string(),
          result: TestResult::Passed { duration: None },
        },
        file_test_runner::SubTestResult {
          name: "step".to_string(),
          result: TestResult::Failed {
            duration: None,
            output: b"boom".to_vec(),
          },
        },
      ],
    };
    assert_eq!(flaky_failure_output(&sub), "--- step ---\nboom");
    let long = TestResult::Failed {
      duration: None,
      output: vec![b'x'; FLAKY_OUTPUT_LIMIT + 10],
    };
    let text = flaky_failure_output(&long);
    assert!(text.starts_with("[... 10 bytes cut]\n"));
    assert_eq!(
      text.len() - "[... 10 bytes cut]\n".len(),
      FLAKY_OUTPUT_LIMIT
    );
  }

  // denext: the timeout scale factor and the cgroup readings behind it.
  #[test]
  fn timeout_scale_factor_follows_the_cpu_per_test() {
    // an uncontended runner: at least one CPU per test keeps the timeouts
    assert_eq!(timeout_scale_factor(4.0, 3), 1.0);
    assert_eq!(timeout_scale_factor(3.0, 2), 1.0);
    assert_eq!(timeout_scale_factor(2.0, 1), 1.0);
    assert_eq!(timeout_scale_factor(1.0, 1), 1.0);
    assert_eq!(timeout_scale_factor(64.0, 63), 1.0);
    // the hunt's constrained containers (--cpus=1 / 0.5, two tests at once)
    assert_eq!(timeout_scale_factor(1.0, 2), 2.0);
    assert_eq!(timeout_scale_factor(0.5, 2), 4.0);
    assert_eq!(timeout_scale_factor(0.5, 1), 2.0);
    assert_eq!(timeout_scale_factor(1.5, 2), 4.0 / 3.0);
    // never more than MAX_TIMEOUT_SCALE: a hang still fails
    assert_eq!(timeout_scale_factor(0.25, 8), MAX_TIMEOUT_SCALE);
    assert_eq!(timeout_scale_factor(0.0, 2), MAX_TIMEOUT_SCALE);
    assert_eq!(timeout_scale_factor(f64::NAN, 2), MAX_TIMEOUT_SCALE);
    // a parallelism of 0 counts as 1
    assert_eq!(timeout_scale_factor(0.5, 0), 2.0);
  }

  #[test]
  fn scale_timeout_keeps_factor_one_exact() {
    let t = Duration::from_millis(10_000);
    assert_eq!(scale_timeout(t, 1.0), t);
    assert_eq!(scale_timeout(t, 2.0), Duration::from_millis(20_000));
    assert_eq!(scale_timeout(t, 4.0 / 3.0), Duration::from_millis(13_333));
    assert_eq!(
      scale_timeout(Duration::from_millis(30_000), MAX_TIMEOUT_SCALE),
      Duration::from_millis(240_000)
    );
  }

  #[test]
  fn cgroup_quotas_parse() {
    assert_eq!(parse_cgroup_v2_cpu_max("max 100000\n"), None);
    assert_eq!(parse_cgroup_v2_cpu_max("50000 100000\n"), Some(0.5));
    assert_eq!(parse_cgroup_v2_cpu_max("300000 100000"), Some(3.0));
    assert_eq!(parse_cgroup_v2_cpu_max("200000"), Some(2.0));
    assert_eq!(parse_cgroup_v2_cpu_max(""), None);
    assert_eq!(parse_cgroup_v2_cpu_max("garbage 100000"), None);
    assert_eq!(parse_cgroup_v1_cfs("-1\n", "100000\n"), None);
    assert_eq!(parse_cgroup_v1_cfs("100000\n", "100000\n"), Some(1.0));
    assert_eq!(parse_cgroup_v1_cfs("25000", "50000"), Some(0.5));
    assert_eq!(parse_cgroup_v1_cfs("1000", "0"), None);
  }

  #[test]
  fn proc_self_cgroup_parses() {
    assert_eq!(parse_proc_self_cgroup("0::/\n"), (Some("/"), None));
    assert_eq!(
      parse_proc_self_cgroup("0::/system.slice/docker-abc.scope\n"),
      (Some("/system.slice/docker-abc.scope"), None)
    );
    assert_eq!(
      parse_proc_self_cgroup(
        "12:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n1:name=systemd:/x\n"
      ),
      (None, Some("/docker/abc"))
    );
    assert_eq!(parse_proc_self_cgroup(""), (None, None));
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn cgroup_dirs_walk_up_to_the_mount() {
    use std::path::Path;
    use std::path::PathBuf;
    let base = Path::new("/sys/fs/cgroup");
    assert_eq!(
      cgroup_dirs(base, "/"),
      vec![PathBuf::from("/sys/fs/cgroup")]
    );
    assert_eq!(
      cgroup_dirs(base, "/a/b"),
      vec![
        PathBuf::from("/sys/fs/cgroup/a/b"),
        PathBuf::from("/sys/fs/cgroup/a"),
        PathBuf::from("/sys/fs/cgroup"),
      ]
    );
    assert_eq!(
      cgroup_dirs(base, "/../../x"),
      vec![PathBuf::from("/sys/fs/cgroup")]
    );
  }

  #[test]
  fn available_cpus_is_positive() {
    let cpus = AvailableCpus::detect();
    assert!(cpus.count > 0.0);
  }

  #[test]
  fn adding_a_test_does_not_move_the_others() {
    let names: Vec<String> = (0..200)
      .map(|i| format!("suite::category::test-{i:03}.js"))
      .collect();
    let assign = |names: &[String]| -> Vec<(String, usize)> {
      (0..3)
        .flat_map(|index| {
          let shard = ShardConfig { index, total: 3 };
          assign_shard_tests(names.to_vec(), &shard)
            .into_iter()
            .map(move |name| (name, index))
        })
        .collect()
    };

    let before = assign(&names);

    // Insert a test that sorts near the front, which under positional
    // round-robin would have re-dealt every test after it.
    let mut after_names = names.clone();
    after_names.push("suite::category::test-000-new.js".to_string());
    let after: HashMap<String, usize> =
      assign(&after_names).into_iter().collect();

    for (name, shard) in before {
      assert_eq!(after.get(&name), Some(&shard), "{name} changed shard");
    }
  }

  #[test]
  fn shards_are_roughly_even() {
    let names: Vec<String> = (0..3000)
      .map(|i| format!("suite::category::test-{i}.js"))
      .collect();
    for total in [2, 3, 5] {
      let mut counts = vec![0usize; total];
      for name in &names {
        counts[shard_index_of(name, total)] += 1;
      }
      let expected = names.len() / total;
      for count in counts {
        // within 10% of an even split
        assert!(
          count.abs_diff(expected) < expected / 10,
          "shard sizes {count} vs expected {expected}"
        );
      }
    }
  }
}
