// Copyright 2018-2026 the Deno authors. MIT license.

//! The low-download check on `deno add` (DENEXT.md item 85): a typosquat
//! guard. A package added by name that the lockfile doesn't know yet, and
//! that has fewer weekly downloads than `low-download-threshold` (1000 by
//! default) on the public npm registry, gets a warning with its count and
//! how to allow it (`low-download-check=warn`, the default). With
//! `low-download-check=prompt` (or `required`, or `paranoid`) a terminal asks
//! for confirmation and the add is refused without one, unless
//! `--allow-low-downloads=<name>` or `allowed-unpopular-packages` names it.
//!
//! The count comes from `https://api.npmjs.org/downloads/point/last-week/`:
//! unscoped names in bulk (up to 128 comma-separated names a request, which
//! the API answers with one count each), scoped names one at a time (the bulk
//! form takes none). Packages from another registry (a private one has no
//! public counts) are not checked. An unknown count (the API can't be
//! reached, or has no count for the name) lets the add through, unless
//! `required` (or `paranoid`). A rate-limited answer (429) is retried as its
//! `Retry-After` allows, then counts as unknown, except that it never refuses
//! the add, even under `required`: rate limiting says nothing about the
//! package, and a large dependency tree would otherwise fail at random. The
//! names it left unchecked are named in a warning.

use std::io::IsTerminal;
use std::io::Write;
use std::time::Duration;

use deno_core::anyhow::anyhow;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_npmrc::LowDownloadCheck;
use deno_npmrc::ResolvedNpmRc;
use deno_npmrc::SupplyChainConfig;
use serde::Deserialize;

use crate::colors;
use crate::http_util;
use crate::http_util::HttpClientProvider;

/// Env var naming another downloads API base URL. When it is set, packages
/// from the default registry are checked against it whatever that registry is
/// (the spec tests' fixture registry).
pub const DOWNLOADS_URL_ENV_VAR: &str = "DENO_NPM_DOWNLOADS_URL";
const DOWNLOADS_DEFAULT_URL: &str = "https://api.npmjs.org/";
const NPMJS_REGISTRY_HOST: &str = "registry.npmjs.org";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The most names one bulk request carries (npm's limit).
const BULK_MAX_NAMES: usize = 128;
/// How often a rate-limited (429) request is tried again.
const RATE_LIMIT_RETRIES: u32 = 2;
/// The longest `Retry-After` waited for; a longer one gives up at once.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);
/// The wait when a 429 has no `Retry-After` in seconds.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Deserialize)]
struct PointResponse {
  downloads: Option<u64>,
}

/// What the downloads API told about one package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
  Known(u64),
  /// No count: the API has none for the name, or could not be reached.
  Unknown,
  /// The API still answered 429 after the retries `Retry-After` allowed.
  RateLimited,
}

/// The requests that fetch the counts of `names`: each scoped name alone (the
/// bulk form takes none), the unscoped ones together, up to
/// [`BULK_MAX_NAMES`] a request.
fn plan_requests<'a>(names: &[&'a str]) -> Vec<Vec<&'a str>> {
  let (scoped, unscoped): (Vec<&str>, Vec<&str>) =
    names.iter().partition(|name| name.starts_with('@'));
  let mut requests = unscoped
    .chunks(BULK_MAX_NAMES)
    .map(|chunk| chunk.to_vec())
    .collect::<Vec<_>>();
  requests.extend(scoped.into_iter().map(|name| vec![name]));
  requests
}

/// How long to wait before retrying a 429 with this `Retry-After`; `None`
/// when it asks for longer than [`MAX_RETRY_AFTER`].
fn retry_after_delay(retry_after: Option<&str>) -> Option<Duration> {
  let delay = retry_after
    .and_then(|value| value.trim().parse::<u64>().ok())
    .map(Duration::from_secs)
    .unwrap_or(DEFAULT_RETRY_AFTER);
  (delay <= MAX_RETRY_AFTER).then_some(delay)
}

/// Reads the counts of `names` (one request's) from the API's answer: a
/// single name's point object, or, for several, an object of them by name
/// (`null` for a name with no count).
fn parse_counts(names: &[&str], text: &str) -> Result<Vec<Count>, AnyError> {
  let count = |point: Option<PointResponse>| match point {
    Some(PointResponse {
      downloads: Some(downloads),
    }) => Count::Known(downloads),
    _ => Count::Unknown,
  };
  if names.len() == 1 {
    let point: PointResponse = serde_json::from_str(text)?;
    return Ok(vec![count(Some(point))]);
  }
  let mut points: std::collections::HashMap<String, Option<PointResponse>> =
    serde_json::from_str(text)?;
  Ok(
    names
      .iter()
      .map(|name| count(points.remove(*name).flatten()))
      .collect(),
  )
}

/// What the gate needs from the command line.
pub struct LowDownloadsOptions<'a> {
  /// `--allow-low-downloads`: `Some(empty)` allows every package.
  pub allow_flag: Option<&'a [String]>,
}

/// One package `deno add` is about to add.
pub struct AddedNpmPackage {
  pub name: String,
}

/// Whether `name`'s downloads are counted: it comes from the public npm
/// registry (or the default registry, with the downloads API overridden).
fn is_checked_registry(npmrc: &ResolvedNpmRc, name: &str) -> bool {
  let url = npmrc.get_registry_url(name);
  if url.host_str() == Some(NPMJS_REGISTRY_HOST) {
    return true;
  }
  super::service_url_overridden(DOWNLOADS_URL_ENV_VAR)
    && url == &npmrc.default_config.registry_url
}

/// The API's answer to one request.
enum Answer {
  Body(String),
  NotFound,
  RateLimited,
}

async fn get_answer(
  http_client_provider: &HttpClientProvider,
  url: &deno_core::url::Url,
) -> Result<Answer, AnyError> {
  let client = http_client_provider.get_or_create()?;
  let mut retries = 0;
  loop {
    let response =
      tokio::time::timeout(REQUEST_TIMEOUT, client.get(url.clone())?.send())
        .await
        .map_err(|_| {
          anyhow!("timed out after {}s", REQUEST_TIMEOUT.as_secs())
        })??;
    let status = response.status();
    if status == http::StatusCode::TOO_MANY_REQUESTS {
      let retry_after = response
        .headers()
        .get(http::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
      match retry_after_delay(retry_after.as_deref()) {
        Some(delay) if retries < RATE_LIMIT_RETRIES => {
          retries += 1;
          tokio::time::sleep(delay).await;
          continue;
        }
        _ => return Ok(Answer::RateLimited),
      }
    }
    if status == http::StatusCode::NOT_FOUND {
      // the API has no count for it (a brand-new name, or none at all)
      return Ok(Answer::NotFound);
    }
    if !status.is_success() {
      bail!("{url} answered {status}");
    }
    return Ok(Answer::Body(http_util::body_to_string(response).await?));
  }
}

/// The weekly downloads of `names`, fetched in one request (see
/// [`plan_requests`]).
async fn weekly_downloads(
  http_client_provider: &HttpClientProvider,
  names: &[&str],
) -> Vec<Count> {
  let base =
    super::service_url_from_env(DOWNLOADS_URL_ENV_VAR, DOWNLOADS_DEFAULT_URL);
  // a scoped name's `/` is encoded
  let path = names
    .iter()
    .map(|name| name.replace('/', "%2F"))
    .collect::<Vec<_>>()
    .join(",");
  let result = match base.join(&format!("downloads/point/last-week/{path}")) {
    Ok(url) => {
      get_answer(http_client_provider, &url)
        .await
        .and_then(|answer| match answer {
          Answer::Body(text) => parse_counts(names, &text)
            .map_err(|err| anyhow!("unexpected response from {url}: {err}")),
          Answer::NotFound => Ok(vec![Count::Unknown; names.len()]),
          Answer::RateLimited => Ok(vec![Count::RateLimited; names.len()]),
        })
    }
    Err(err) => Err(err.into()),
  };
  result.unwrap_or_else(|err| {
    log::debug!(
      "Could not get the download counts of {}: {err:#}",
      names.join(", ")
    );
    vec![Count::Unknown; names.len()]
  })
}

fn is_interactive() -> bool {
  #[allow(clippy::disallowed_methods, reason = "CI detection")]
  let ci = std::env::var_os("CI").is_some_and(|v| !v.is_empty() && v != "0");
  !ci && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn confirm(question: &str) -> bool {
  crate::util::draw_thread::DrawThread::hide();
  let mut stderr = std::io::stderr();
  let _ = write!(stderr, "{question} [y/N] ");
  let _ = stderr.flush();
  let mut line = String::new();
  let read = std::io::stdin().read_line(&mut line);
  crate::util::draw_thread::DrawThread::show();
  read.is_ok() && matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

/// Why a package is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
  /// Named on the `deno add` command line.
  Added,
  /// A dependency the added packages bring in (DENEXT.md R11a).
  Dependency,
}

/// Runs the gate over the npm packages `deno add` is about to add. An error
/// refuses the add (nothing has been written yet).
pub async fn check_added_packages(
  http_client_provider: &HttpClientProvider,
  npmrc: &ResolvedNpmRc,
  config: &SupplyChainConfig,
  options: LowDownloadsOptions<'_>,
  packages: &[AddedNpmPackage],
  is_in_lockfile: impl Fn(&str) -> bool,
) -> Result<(), AnyError> {
  let names = packages
    .iter()
    .filter(|p| !is_in_lockfile(&p.name))
    .map(|p| p.name.clone())
    .collect::<Vec<_>>();
  check_names(
    http_client_provider,
    npmrc,
    config,
    options.allow_flag,
    &names,
    Origin::Added,
  )
  .await
}

/// What `deno add` arms for the install that follows it: the dependencies
/// the install resolves get the same check (DENEXT.md R11a).
#[derive(Debug, Clone, Default)]
pub struct DependencyCheck {
  /// Names not to check: those the lockfile already had, and the added
  /// packages themselves (checked before anything was written).
  pub known: std::collections::HashSet<String>,
  /// `--allow-low-downloads`.
  pub allow_flag: Option<Vec<String>>,
}

static DEPENDENCY_CHECK: std::sync::Mutex<Option<DependencyCheck>> =
  std::sync::Mutex::new(None);

/// Arms (or, with `None`, disarms) the check of the dependencies the next
/// install resolves.
pub fn set_dependency_check(check: Option<DependencyCheck>) {
  *DEPENDENCY_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = check;
}

/// Runs the armed dependency check over packages an install is about to
/// install; a no-op when `deno add` armed none. Checked names are added to
/// `known`, so a later resolution in the same install skips them.
pub async fn check_dependencies(
  http_client_provider: &HttpClientProvider,
  npmrc: &ResolvedNpmRc,
  names: impl IntoIterator<Item = String>,
) -> Result<(), AnyError> {
  let (names, allow_flag) = {
    let mut guard = DEPENDENCY_CHECK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(check) = guard.as_mut() else {
      return Ok(());
    };
    let mut out = Vec::new();
    for name in names {
      if check.known.insert(name.clone()) {
        out.push(name);
      }
    }
    (out, check.allow_flag.clone())
  };
  if names.is_empty() {
    return Ok(());
  }
  check_names(
    http_client_provider,
    npmrc,
    &npmrc.supply_chain,
    allow_flag.as_deref(),
    &names,
    Origin::Dependency,
  )
  .await
}

/// How many download counts are fetched at once.
const CONCURRENT_REQUESTS: usize = 8;

async fn check_names(
  http_client_provider: &HttpClientProvider,
  npmrc: &ResolvedNpmRc,
  config: &SupplyChainConfig,
  allow_flag: Option<&[String]>,
  names: &[String],
  origin: Origin,
) -> Result<(), AnyError> {
  use deno_core::futures::StreamExt;

  let mode = config.low_download_check;
  if config.low_download_threshold == 0 || mode == LowDownloadCheck::Off {
    return Ok(());
  }
  let allow_all = allow_flag.is_some_and(|names| names.is_empty());
  if allow_all && config.paranoid {
    bail!(
      "paranoid is on: --allow-low-downloads needs the packages it allows (--allow-low-downloads=<name>,...)"
    );
  }
  if allow_all {
    return Ok(());
  }
  let threshold = config.low_download_threshold;
  let to_check = names
    .iter()
    .filter(|name| {
      !allow_flag.is_some_and(|names| names.iter().any(|n| n == *name))
        && !config.is_allowed_unpopular(name)
        && is_checked_registry(npmrc, name)
    })
    .collect::<Vec<_>>();
  let to_check = to_check
    .iter()
    .map(|name| name.as_str())
    .collect::<Vec<_>>();
  let counts = deno_core::futures::stream::iter(plan_requests(&to_check))
    .map(|names| async move {
      let counts = weekly_downloads(http_client_provider, &names).await;
      names.into_iter().zip(counts).collect::<Vec<_>>()
    })
    .buffered(CONCURRENT_REQUESTS)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
  let rate_limited = counts
    .iter()
    .filter(|(_, count)| *count == Count::RateLimited)
    .map(|(name, _)| format!("npm:{name}"))
    .collect::<Vec<_>>();
  if !rate_limited.is_empty() {
    let message = format!(
      "The npm downloads API rate-limited the low-download check, so these were not checked: {}",
      rate_limited.join(", ")
    );
    if mode == LowDownloadCheck::Warn {
      log::debug!("{message}");
    } else {
      log::warn!("{} {message}.", colors::yellow("Warning"));
    }
  }
  for (name, count) in counts {
    let subject = match origin {
      Origin::Added => format!("npm:{name}"),
      Origin::Dependency => {
        format!("npm:{name} (a dependency of the packages being added)")
      }
    };
    let reason = match count {
      Count::Known(count) if count >= threshold => continue,
      Count::Known(count) => {
        format!(
          "{subject} has {count} weekly downloads (fewer than {threshold})"
        )
      }
      Count::Unknown if mode == LowDownloadCheck::Required => format!(
        "The weekly downloads of {subject} are unknown (low-download-check=required, or paranoid, refuses unknown counts)"
      ),
      // Unknown otherwise; rate-limited in every mode (warned about above):
      // rate limiting alone never refuses.
      Count::Unknown | Count::RateLimited => continue,
    };
    let hint = match super::popular_names::did_you_mean(name) {
      Some(popular) => format!(
        "It may be a typosquat of a popular package: did you mean npm:{popular}?"
      ),
      None => "It may be a typosquat of a popular package: check the name."
        .to_string(),
    };
    let allow = format!(
      "--allow-low-downloads=npm:{name}, or list it in allowed-unpopular-packages (.npmrc) or \"supplyChain\": {{ \"allowLowDownloads\": [\"{name}\"] }} (deno.json)"
    );
    if mode == LowDownloadCheck::Warn {
      log::warn!(
        "{} {reason}. {hint}\nTo silence this, pass {allow}. To be asked first (and refused without a terminal), set low-download-check=prompt.",
        colors::yellow("Warning"),
      );
      continue;
    }
    if is_interactive() {
      log::warn!("{} {reason}. {hint}", colors::yellow("Warning"));
      let question = match origin {
        Origin::Added => format!("Add npm:{name} anyway?"),
        Origin::Dependency => format!("Install npm:{name} anyway?"),
      };
      if confirm(&question) {
        continue;
      }
      super::mark_gate_refused();
      match origin {
        Origin::Added => bail!("Did not add npm:{name}."),
        Origin::Dependency => {
          bail!("Did not add the packages: they depend on npm:{name}.")
        }
      }
    }
    super::mark_gate_refused();
    bail!("{reason}. {hint}\nTo add it anyway, pass {allow}.");
  }
  Ok(())
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn unscoped_names_go_in_bulk_and_scoped_names_alone() {
    let names = ["a", "@s/b", "c", "@s/d"];
    assert_eq!(
      plan_requests(&names),
      vec![vec!["a", "c"], vec!["@s/b"], vec!["@s/d"]]
    );
    let many = (0..300).map(|i| format!("p{i}")).collect::<Vec<_>>();
    let many = many.iter().map(String::as_str).collect::<Vec<_>>();
    let requests = plan_requests(&many);
    assert_eq!(
      requests.iter().map(Vec::len).collect::<Vec<_>>(),
      vec![128, 128, 44]
    );
    assert_eq!(requests.concat(), many);
    assert!(plan_requests(&[]).is_empty());
  }

  #[test]
  fn bulk_and_single_answers_are_read() {
    assert_eq!(
      parse_counts(&["a"], r#"{"downloads":12,"package":"a"}"#).unwrap(),
      vec![Count::Known(12)]
    );
    assert_eq!(
      parse_counts(&["a"], r#"{"package":"a"}"#).unwrap(),
      vec![Count::Unknown]
    );
    assert_eq!(
      parse_counts(
        &["a", "b", "c"],
        r#"{"a":{"downloads":5000,"package":"a"},"b":null}"#
      )
      .unwrap(),
      vec![Count::Known(5000), Count::Unknown, Count::Unknown]
    );
    assert!(parse_counts(&["a", "b"], "[]").is_err());
  }

  #[test]
  fn retry_after_is_honoured_up_to_a_limit() {
    assert_eq!(retry_after_delay(None), Some(DEFAULT_RETRY_AFTER));
    assert_eq!(retry_after_delay(Some("0")), Some(Duration::ZERO));
    assert_eq!(retry_after_delay(Some(" 3 ")), Some(Duration::from_secs(3)));
    assert_eq!(
      retry_after_delay(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
      Some(DEFAULT_RETRY_AFTER)
    );
    assert_eq!(retry_after_delay(Some("3600")), None);
  }
}
