// Copyright 2018-2026 the Deno authors. MIT license.

//! Finding and evaluating a Vitest config.
//!
//! The config is evaluated once, in its own worker, by
//! `deno-vitest:///config_loader.js`, which reduces it to JSON (see
//! [`LoadedConfig`]).

use std::path::Path;
use std::path::PathBuf;

use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::located_script_name;
use deno_core::url::Url;
use deno_core::v8;
use deno_runtime::WorkerExecutionMode;
use deno_runtime::deno_permissions::PermissionsContainer;
use serde::Deserialize;

use super::module_url;
use crate::worker::CliMainWorkerFactory;

const CONFIG_NAMES: [&str; 2] = ["vitest.config", "vite.config"];
const CONFIG_EXTENSIONS: [&str; 6] = ["ts", "mts", "cts", "js", "mjs", "cjs"];

/// Finds the Vitest config: `explicit` (from `--vitest-config`), or a
/// `vitest.config.*` in `cwd`, or a `vite.config.*` in `cwd` when the
/// project depends on Vitest (so `deno test` leaves the Vite configs of
/// other projects alone).
pub fn find_config(
  cwd: &Path,
  explicit: Option<&str>,
) -> Result<Option<PathBuf>, AnyError> {
  if let Some(explicit) = explicit {
    let path = cwd.join(explicit);
    if !path.is_file() {
      return Err(anyhow!("Vitest config not found: {}", path.display()));
    }
    return Ok(Some(path));
  }
  for name in CONFIG_NAMES {
    for ext in CONFIG_EXTENSIONS {
      let path = cwd.join(format!("{name}.{ext}"));
      if path.is_file() {
        if name == "vite.config" && !depends_on_vitest(cwd) {
          return Ok(None);
        }
        return Ok(Some(path));
      }
    }
  }
  Ok(None)
}

/// Whether the nearest package.json lists `vitest` (or Vite+, which bundles
/// it) as a dependency.
fn depends_on_vitest(dir: &Path) -> bool {
  for dir in dir.ancestors() {
    let path = dir.join("package.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
      continue;
    };
    let Ok(json) = deno_core::serde_json::from_str::<deno_core::serde_json::Value>(&text) else {
      return false;
    };
    return [
      "dependencies",
      "devDependencies",
      "peerDependencies",
      "optionalDependencies",
    ]
    .iter()
    .filter_map(|key| json.get(key).and_then(|v| v.as_object()))
    .any(|deps| deps.contains_key("vitest") || deps.contains_key("vite-plus"));
  }
  false
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedConfig {
  pub projects: Vec<LoadedProject>,
}

/// One test project, as reduced by `config_loader.js`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedProject {
  pub name: String,
  pub root: String,
  #[serde(default)]
  pub dir: Option<String>,
  pub include: Vec<String>,
  pub exclude: Vec<String>,
  #[serde(default)]
  pub alias: Vec<LoadedAlias>,
  /// The runtime options handed to the per-file preload.
  pub config: deno_core::serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoadedAlias {
  pub find: LoadedAliasFind,
  pub replacement: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LoadedAliasFind {
  String(String),
  Regex { regex: String, flags: String },
}

/// Evaluates the config at `path` in a dedicated worker.
pub async fn load_config(
  worker_factory: &CliMainWorkerFactory,
  permissions: PermissionsContainer,
  path: &Path,
) -> Result<LoadedConfig, AnyError> {
  let config_url = Url::from_file_path(path)
    .map_err(|_| anyhow!("Invalid config path {}", path.display()))?;
  let mut main_module = module_url("config_loader.js");
  main_module
    .query_pairs_mut()
    .append_pair("config", config_url.as_str());
  let worker = worker_factory
    .create_custom_worker(
      WorkerExecutionMode::Run,
      main_module.clone(),
      vec![],
      vec![],
      permissions,
      vec![],
      Default::default(),
      None,
    )
    .await?;
  let mut worker = worker.into_main_worker();
  worker.execute_main_module(&main_module).await?;
  worker.run_event_loop(false).await?;
  let value = worker
    .execute_script(
      located_script_name!(),
      "globalThis[Symbol.for('deno.vitest.loadedConfig')] ?? null"
        .to_string()
        .into(),
    )
    .map_err(|err| anyhow!("{err}"))?;
  let json = {
    deno_core::scope!(scope, &mut worker.js_runtime);
    let value = v8::Local::new(scope, value);
    if value.is_null_or_undefined() {
      return Err(anyhow!("the config loader produced no result"));
    }
    value.to_rust_string_lossy(scope)
  };
  Ok(deno_core::serde_json::from_str(&json)?)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn finds_vitest_config_before_vite_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vite.config.ts"), "").unwrap();
    std::fs::write(dir.path().join("vitest.config.mts"), "").unwrap();
    assert_eq!(
      find_config(dir.path(), None).unwrap(),
      Some(dir.path().join("vitest.config.mts"))
    );
  }

  #[test]
  fn vite_config_needs_a_vitest_dependency() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("vite.config.ts"), "").unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
    assert_eq!(find_config(dir.path(), None).unwrap(), None);
    std::fs::write(
      dir.path().join("package.json"),
      r#"{"devDependencies":{"vitest":"^3.0.0"}}"#,
    )
    .unwrap();
    assert_eq!(
      find_config(dir.path(), None).unwrap(),
      Some(dir.path().join("vite.config.ts"))
    );
  }

  #[test]
  fn explicit_config_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_config(dir.path(), Some("nope.config.ts")).is_err());
  }
}
