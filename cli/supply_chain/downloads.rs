// Copyright 2018-2026 the Deno authors. MIT license.

//! The low-download gate on `deno add` (DENEXT.md item 82): a typosquat
//! guard. A package added by name that the lockfile doesn't know yet, and
//! that has fewer weekly downloads than `low-download-threshold` (1000 by
//! default) on the public npm registry, asks for confirmation in a terminal
//! and is refused otherwise, unless `--allow-low-downloads=<name>` or
//! `allowed-unpopular-packages` names it.
//!
//! The count comes from `https://api.npmjs.org/downloads/point/last-week/`.
//! Packages from another registry (a private one has no public counts) are
//! not checked. An unknown count (the API can't be reached, or has no count
//! for the name) lets the add through, unless `paranoid`.

use std::io::IsTerminal;
use std::io::Write;
use std::time::Duration;

use deno_core::anyhow::anyhow;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
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

#[derive(Deserialize)]
struct PointResponse {
  downloads: Option<u64>,
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

async fn weekly_downloads(
  http_client_provider: &HttpClientProvider,
  name: &str,
) -> Result<Option<u64>, AnyError> {
  let base =
    super::service_url_from_env(DOWNLOADS_URL_ENV_VAR, DOWNLOADS_DEFAULT_URL);
  // a scoped name's `/` is encoded
  let encoded = name.replace('/', "%2F");
  let url = base.join(&format!("downloads/point/last-week/{encoded}"))?;
  let client = http_client_provider.get_or_create()?;
  let response =
    tokio::time::timeout(REQUEST_TIMEOUT, client.get(url.clone())?.send())
      .await
      .map_err(|_| {
        anyhow!("timed out after {}s", REQUEST_TIMEOUT.as_secs())
      })??;
  let status = response.status();
  if status == http::StatusCode::NOT_FOUND {
    // the API has no count for it (a brand-new name, or none at all)
    return Ok(None);
  }
  if !status.is_success() {
    bail!("{url} answered {status}");
  }
  let text = http_util::body_to_string(response).await?;
  let response: PointResponse = serde_json::from_str(&text)
    .map_err(|err| anyhow!("unexpected response from {url}: {err}"))?;
  Ok(response.downloads)
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
  if config.low_download_threshold == 0 {
    return Ok(());
  }
  let allow_all = options.allow_flag.is_some_and(|names| names.is_empty());
  if allow_all && config.paranoid {
    bail!(
      "paranoid is on: --allow-low-downloads needs the packages it allows (--allow-low-downloads=<name>,...)"
    );
  }
  let threshold = config.low_download_threshold;
  for package in packages {
    let name = package.name.as_str();
    if allow_all
      || options
        .allow_flag
        .is_some_and(|names| names.iter().any(|n| n == name))
      || config.is_allowed_unpopular(name)
      || is_in_lockfile(name)
      || !is_checked_registry(npmrc, name)
    {
      continue;
    }
    let count = match weekly_downloads(http_client_provider, name).await {
      Ok(count) => count,
      Err(err) => {
        log::debug!("Could not get the download count of {name}: {err:#}");
        None
      }
    };
    let reason = match count {
      Some(count) if count >= threshold => continue,
      Some(count) => {
        format!(
          "npm:{name} has {count} weekly downloads (fewer than {threshold})"
        )
      }
      None if config.low_downloads_fail_closed => format!(
        "The weekly downloads of npm:{name} are unknown (paranoid refuses unknown counts)"
      ),
      None => continue,
    };
    let hint = "It may be a typosquat of a popular package: check the name.";
    if is_interactive() {
      log::warn!("{} {reason}. {hint}", colors::yellow("Warning"));
      if confirm(&format!("Add npm:{name} anyway?")) {
        continue;
      }
      super::mark_gate_refused();
      bail!("Did not add npm:{name}.");
    }
    super::mark_gate_refused();
    bail!(
      "{reason}. {hint}\nTo add it anyway, pass --allow-low-downloads=npm:{name}, or list it in allowed-unpopular-packages (.npmrc) or \"supplyChain\": {{ \"allowLowDownloads\": [\"{name}\"] }} (deno.json)."
    );
  }
  Ok(())
}
