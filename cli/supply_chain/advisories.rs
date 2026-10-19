// Copyright 2018-2026 the Deno authors. MIT license.

//! The malicious-version gate (`advisory-check`, DENEXT.md item 81).
//!
//! Before npm package versions are installed, they are looked up in OSV
//! (<https://osv.dev>, `POST /v1/querybatch`, no account needed). A version
//! with an advisory whose id starts with `MAL-` (the OpenSSF malicious
//! packages dataset: typosquats, hijacked releases, credential stealers) is
//! refused. Versions found clean are remembered in `DENO_DIR` for a day, so
//! repeated installs don't query again; a hit is never cached.
//!
//! When OSV can't be reached the gate warns and lets the install through,
//! unless `advisory-check=required` (or `paranoid`), which refuses it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use deno_core::anyhow::anyhow;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::serde_json::json;
use deno_error::JsErrorBox;
use deno_npm_installer::NpmInstallGate;
use deno_npmrc::AdvisoryCheck;
use deno_semver::package::PackageNv;
use serde::Deserialize;
use serde::Serialize;

use crate::colors;
use crate::http_util;
use crate::http_util::HttpClientProvider;

/// Env var naming another OSV API base URL (a mirror, or the spec tests'
/// fixture server).
pub const OSV_URL_ENV_VAR: &str = "DENO_OSV_URL";
const OSV_DEFAULT_URL: &str = "https://api.osv.dev/";
/// Env var overriding how long a clean result is reused, in seconds.
const CACHE_TTL_ENV_VAR: &str = "DENO_OSV_CACHE_TTL";
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// OSV accepts up to 1000 queries per batch.
const BATCH_SIZE: usize = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct OsvInstallGate {
  http_client_provider: Arc<HttpClientProvider>,
  mode: AdvisoryCheck,
  cache_path: Option<PathBuf>,
  /// `--cached-only`: no network
  offline: bool,
}

/// One warning per process about an unreachable OSV.
static WARNED: std::sync::atomic::AtomicBool =
  std::sync::atomic::AtomicBool::new(false);

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
  /// `name@version` -> unix seconds when it was found clean
  clean: HashMap<String, u64>,
}

#[derive(Deserialize)]
struct QueryBatchResponse {
  results: Vec<QueryResult>,
}

#[derive(Deserialize, Default)]
struct QueryResult {
  #[serde(default)]
  vulns: Vec<QueryVuln>,
}

#[derive(Deserialize)]
struct QueryVuln {
  id: String,
}

/// A version OSV lists as malicious.
pub struct MaliciousVersion {
  pub nv: PackageNv,
  pub advisories: Vec<String>,
}

fn now_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

fn cache_ttl() -> Duration {
  #[allow(clippy::disallowed_methods, reason = "test hook")]
  std::env::var(CACHE_TTL_ENV_VAR)
    .ok()
    .and_then(|v| v.trim().parse::<u64>().ok())
    .map(Duration::from_secs)
    .unwrap_or(DEFAULT_CACHE_TTL)
}

impl OsvInstallGate {
  pub fn new(
    http_client_provider: Arc<HttpClientProvider>,
    mode: AdvisoryCheck,
    cache_path: Option<PathBuf>,
    offline: bool,
  ) -> Self {
    Self {
      http_client_provider,
      mode,
      cache_path,
      offline,
    }
  }

  fn read_cache(&self) -> CacheFile {
    self
      .cache_path
      .as_ref()
      .and_then(|path| std::fs::read(path).ok())
      .and_then(|bytes| serde_json::from_slice(&bytes).ok())
      .unwrap_or_default()
  }

  fn write_cache(&self, newly_clean: &[PackageNv]) {
    let Some(path) = &self.cache_path else {
      return;
    };
    // re-read so concurrent processes' entries aren't lost
    let mut cache = self.read_cache();
    let now = now_secs();
    let ttl = cache_ttl().as_secs();
    cache
      .clean
      .retain(|_, checked| now.saturating_sub(*checked) < ttl);
    for nv in newly_clean {
      cache.clean.insert(nv.to_string(), now);
    }
    let Ok(bytes) = serde_json::to_vec(&cache) else {
      return;
    };
    if let Some(parent) = path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    // write next to it, then rename over it, so a reader never sees half a
    // file
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok()
      && std::fs::rename(&tmp, path).is_err()
    {
      let _ = std::fs::remove_file(&tmp);
    }
  }

  /// Queries OSV for `packages`; returns the malicious ones.
  pub async fn query(
    &self,
    packages: &[PackageNv],
  ) -> Result<Vec<MaliciousVersion>, AnyError> {
    let base = super::service_url_from_env(OSV_URL_ENV_VAR, OSV_DEFAULT_URL);
    let url = base.join("v1/querybatch")?;
    let client = self.http_client_provider.get_or_create()?;
    let mut malicious = Vec::new();
    for chunk in packages.chunks(BATCH_SIZE) {
      let body = json!({
        "queries": chunk.iter().map(|nv| json!({
          "package": { "ecosystem": "npm", "name": nv.name.as_str() },
          "version": nv.version.to_string(),
        })).collect::<Vec<_>>(),
      });
      let request = client.post_json(url.clone(), &body)?;
      let response = tokio::time::timeout(REQUEST_TIMEOUT, request.send())
        .await
        .map_err(|_| {
          anyhow!("timed out after {}s", REQUEST_TIMEOUT.as_secs())
        })??;
      let status = response.status();
      if !status.is_success() {
        bail!("{url} answered {status}");
      }
      let text = http_util::body_to_string(response).await?;
      let response: QueryBatchResponse = serde_json::from_str(&text)
        .map_err(|err| anyhow!("unexpected response from {url}: {err}"))?;
      if response.results.len() != chunk.len() {
        bail!(
          "unexpected response from {url}: {} results for {} queries",
          response.results.len(),
          chunk.len()
        );
      }
      for (nv, result) in chunk.iter().zip(response.results) {
        let advisories = result
          .vulns
          .into_iter()
          .map(|v| v.id)
          .filter(|id| id.starts_with("MAL-"))
          .collect::<Vec<_>>();
        if !advisories.is_empty() {
          malicious.push(MaliciousVersion {
            nv: nv.clone(),
            advisories,
          });
        }
      }
    }
    Ok(malicious)
  }

  /// Checks `packages`: an error refuses the install.
  pub async fn check(&self, packages: Vec<PackageNv>) -> Result<(), AnyError> {
    if self.mode == AdvisoryCheck::Off || packages.is_empty() {
      return Ok(());
    }
    let cache = self.read_cache();
    let now = now_secs();
    let ttl = cache_ttl().as_secs();
    let to_query = packages
      .into_iter()
      .filter(|nv| {
        cache
          .clean
          .get(&nv.to_string())
          .is_none_or(|checked| now.saturating_sub(*checked) >= ttl)
      })
      .collect::<Vec<_>>();
    if to_query.is_empty() {
      return Ok(());
    }
    log::debug!("Checking {} npm package versions with OSV", to_query.len());
    if self.offline && self.mode != AdvisoryCheck::Required {
      // `--cached-only` asked for no network: the packages were checked
      // when they were downloaded
      log::debug!("Skipping the OSV check: --cached-only");
      return Ok(());
    }
    let result = if self.offline {
      Err(anyhow!("--cached-only does not allow network access"))
    } else {
      self.query(&to_query).await
    };
    match result {
      Ok(malicious) if malicious.is_empty() => {
        self.write_cache(&to_query);
        Ok(())
      }
      Ok(malicious) => {
        super::mark_gate_refused();
        let clean = to_query
          .into_iter()
          .filter(|nv| !malicious.iter().any(|m| &m.nv == nv))
          .collect::<Vec<_>>();
        self.write_cache(&clean);
        Err(anyhow!(format_malicious_error(&malicious)))
      }
      Err(err) => {
        if self.mode == AdvisoryCheck::Required {
          super::mark_gate_refused();
          bail!(
            "Could not check npm packages against OSV's malicious-package list ({err:#}), and advisory-check is required (or paranoid is on), so nothing was installed.",
          );
        }
        if !WARNED.swap(true, std::sync::atomic::Ordering::SeqCst) {
          log::warn!(
            "{} Could not check npm packages against OSV's malicious-package list ({:#}); installing anyway. Set advisory-check=required (or paranoid) to refuse instead.",
            colors::yellow("Warning"),
            err
          );
        }
        Ok(())
      }
    }
  }
}

pub fn format_malicious_error(malicious: &[MaliciousVersion]) -> String {
  let mut text = String::from(
    "Refusing to install npm package versions listed as malicious:\n",
  );
  for m in malicious {
    text.push_str(&format!("  npm:{}@{}:", m.nv.name, m.nv.version));
    for id in &m.advisories {
      text.push_str(&format!(" {id} (https://osv.dev/vulnerability/{id})"));
    }
    text.push('\n');
  }
  text.push_str(
    "Use another version. To turn this check off (not recommended), set advisory-check=off in .npmrc or \"supplyChain\": { \"advisoryCheck\": \"off\" } in deno.json.",
  );
  text
}

#[async_trait::async_trait(?Send)]
impl NpmInstallGate for OsvInstallGate {
  async fn check_packages(
    &self,
    packages: Vec<PackageNv>,
  ) -> Result<(), JsErrorBox> {
    self
      .check(packages)
      .await
      .map_err(|err| JsErrorBox::generic(format!("{err:#}")))
  }
}
