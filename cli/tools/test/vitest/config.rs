// Copyright 2018-2026 the Deno authors. MIT license.

//! Finding and evaluating a Vitest (or Jest) config.
//!
//! The config is evaluated once, in its own worker, by
//! `deno-vitest:///config_loader.js`, which reduces it to JSON (see
//! [`LoadedConfig`]). A Jest config is mapped onto the same shape by
//! `deno-vitest:///jest_config.js`.

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
const JEST_CONFIG_EXTENSIONS: [&str; 7] =
  ["ts", "mts", "cts", "js", "mjs", "cjs", "json"];

/// Which test framework a config belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFlavor {
  Vitest,
  Jest,
}

impl ConfigFlavor {
  pub fn label(self) -> &'static str {
    match self {
      Self::Vitest => "Vitest",
      Self::Jest => "Jest",
    }
  }
}

/// A test config found for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundConfig {
  pub path: PathBuf,
  pub flavor: ConfigFlavor,
}

/// Finds the test config: `explicit` (from `--vitest-config`; a `jest.*`
/// file or a package.json is a Jest config), or a `vitest.config.*` in
/// `cwd`, or a `vite.config.*` in `cwd` when the project depends on Vitest
/// (so `deno test` leaves the Vite configs of other projects alone). Without
/// one, a Jest project: a `jest.config.*`, a package.json with a `"jest"`
/// key, or a package.json depending on Jest (Jest's defaults).
pub fn find_config(
  cwd: &Path,
  explicit: Option<&str>,
) -> Result<Option<FoundConfig>, AnyError> {
  if let Some(explicit) = explicit {
    let path = cwd.join(explicit);
    if !path.is_file() {
      return Err(anyhow!("Test config not found: {}", path.display()));
    }
    let name = path
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_default();
    let flavor = if name.starts_with("jest") || name == "package.json" {
      ConfigFlavor::Jest
    } else {
      ConfigFlavor::Vitest
    };
    return Ok(Some(FoundConfig { path, flavor }));
  }
  let package_json = read_package_json(cwd);
  let depends_on = |names: &[&str]| {
    package_json
      .as_ref()
      .is_some_and(|json| depends_on_any(json, names))
  };
  'vitest: for name in CONFIG_NAMES {
    for ext in CONFIG_EXTENSIONS {
      let path = cwd.join(format!("{name}.{ext}"));
      if path.is_file() {
        if name == "vite.config" && !depends_on(&["vitest", "vite-plus"]) {
          break 'vitest;
        }
        return Ok(Some(FoundConfig {
          path,
          flavor: ConfigFlavor::Vitest,
        }));
      }
    }
  }
  for ext in JEST_CONFIG_EXTENSIONS {
    let path = cwd.join(format!("jest.config.{ext}"));
    if path.is_file() {
      return Ok(Some(FoundConfig {
        path,
        flavor: ConfigFlavor::Jest,
      }));
    }
  }
  let path = cwd.join("package.json");
  if let Some(json) = &package_json
    && (json.get("jest").is_some_and(|j| j.is_object())
      || (depends_on(&["jest"]) && !depends_on(&["vitest", "vite-plus"])))
  {
    return Ok(Some(FoundConfig {
      path,
      flavor: ConfigFlavor::Jest,
    }));
  }
  Ok(None)
}

/// The nearest package.json at or above `dir`.
fn read_package_json(dir: &Path) -> Option<deno_core::serde_json::Value> {
  for dir in dir.ancestors() {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
      continue;
    };
    return deno_core::serde_json::from_str(&text).ok();
  }
  None
}

/// Whether a package.json lists one of `names` as a dependency.
fn depends_on_any(json: &deno_core::serde_json::Value, names: &[&str]) -> bool {
  [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
  ]
  .iter()
  .filter_map(|key| json.get(key).and_then(|v| v.as_object()))
  .any(|deps| names.iter().any(|name| deps.contains_key(*name)))
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
  /// How a Jest project selects its test files (its `include` holds the
  /// `testMatch` globs, matched against absolute paths).
  #[serde(default)]
  pub jest: Option<LoadedJestMatch>,
  /// Jest's `moduleDirectories` (other than `node_modules`, looked up in the
  /// importer's ancestors) and `modulePaths` (absolute directories).
  #[serde(default)]
  pub module_directories: Vec<String>,
  /// Config options the run cannot honour, reported once.
  #[serde(default)]
  pub warnings: Vec<String>,
  /// Packages the built-in modules import (jsdom, happy-dom) that resolve
  /// from another file than the project root: package name -> file path.
  #[serde(default)]
  pub package_bases: std::collections::HashMap<String, String>,
  /// The automatic JSX runtime of a Jest project's transform.
  #[serde(default)]
  pub jsx: Option<LoadedJsx>,
  /// Which `.js` user modules are parsed as JSX.
  #[serde(default)]
  pub jsx_in_js: Option<LoadedJsxInJs>,
}

/// Which `.js` user modules under `root` a project parses as JSX: all of
/// them (`mode: "always"`, babel-jest & co., or Vite's `esbuild.loader:
/// "jsx"`) or those that fail to parse as plain JavaScript (`"fallback"`),
/// in both cases only those `include` (when not empty) and `exclude` select.
#[derive(Debug, Clone, Deserialize)]
pub struct LoadedJsxInJs {
  pub root: String,
  pub mode: String,
  /// Vite filters: regexes, or globs against absolute paths.
  #[serde(default)]
  pub include: Vec<LoadedAliasFind>,
  #[serde(default)]
  pub exclude: Vec<LoadedAliasFind>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedJsx {
  pub import_source: String,
  #[serde(default)]
  pub development: bool,
}

/// The test file selection of a Jest project.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedJestMatch {
  /// `roots`: the directories searched for tests.
  pub roots: Vec<String>,
  /// `testRegex` (JavaScript regex sources), used instead of `testMatch`.
  #[serde(default)]
  pub test_regex: Vec<String>,
  /// `testPathIgnorePatterns` (JavaScript regex sources).
  #[serde(default)]
  pub ignore: Vec<String>,
  /// `moduleFileExtensions`: a test file must have one of them.
  #[serde(default)]
  pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoadedAlias {
  pub find: LoadedAliasFind,
  pub replacement: String,
  /// Jest's `moduleNameMapper`: a match maps the whole specifier to the
  /// replacement (with `$1` & co. substituted), where a Vite alias only
  /// replaces the matched part.
  #[serde(default)]
  pub whole: bool,
  /// The dependencies (modules in `node_modules`) whose imports the alias
  /// applies to: Jest's `moduleNameMapper` applies to every module, a Vite
  /// alias only to the dependencies Vitest inlines (`server.deps.inline`).
  #[serde(default)]
  pub deps: LoadedAliasDeps,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub enum LoadedAliasDeps {
  /// None of them.
  #[default]
  None,
  /// `true`: all of them.
  All(bool),
  /// Those a pattern selects: a package name (a module under
  /// `node_modules/<name>/`) or a regex tested against the module's path.
  Some(Vec<LoadedAliasFind>),
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
  found: &FoundConfig,
) -> Result<LoadedConfig, AnyError> {
  let path = &found.path;
  let config_url = Url::from_file_path(path)
    .map_err(|_| anyhow!("Invalid config path {}", path.display()))?;
  let mut main_module = module_url("config_loader.js");
  {
    let mut query = main_module.query_pairs_mut();
    query.append_pair("config", config_url.as_str());
    if found.flavor == ConfigFlavor::Jest {
      query.append_pair("flavor", "jest");
    }
  }
  let worker = worker_factory
    .create_custom_worker(
      WorkerExecutionMode::Run,
      main_module.clone(),
      vec![],
      vec![],
      permissions,
      vec![super::runner_reads::deno_vitest_runner::init(
        super::runner_reads::RunnerWorker::Config,
      )],
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
      Some(FoundConfig {
        path: dir.path().join("vitest.config.mts"),
        flavor: ConfigFlavor::Vitest,
      })
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
      Some(FoundConfig {
        path: dir.path().join("vite.config.ts"),
        flavor: ConfigFlavor::Vitest,
      })
    );
  }

  #[test]
  fn explicit_config_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_config(dir.path(), Some("nope.config.ts")).is_err());
  }

  #[test]
  fn explicit_jest_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("scripts")).unwrap();
    std::fs::write(dir.path().join("scripts/jest.config.js"), "").unwrap();
    assert_eq!(
      find_config(dir.path(), Some("scripts/jest.config.js"))
        .unwrap()
        .map(|c| c.flavor),
      Some(ConfigFlavor::Jest)
    );
  }

  #[test]
  fn finds_jest_projects() {
    let found = |files: &[(&str, &str)]| {
      let dir = tempfile::tempdir().unwrap();
      for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
      }
      find_config(dir.path(), None).unwrap().map(|c| {
        (
          c.path.file_name().unwrap().to_string_lossy().to_string(),
          c.flavor,
        )
      })
    };
    let jest = |name: &str| Some((name.to_string(), ConfigFlavor::Jest));
    assert_eq!(found(&[("jest.config.cjs", "")]), jest("jest.config.cjs"));
    assert_eq!(
      found(&[("jest.config.json", "{}")]),
      jest("jest.config.json")
    );
    // a Vitest config wins
    assert_eq!(
      found(&[("jest.config.js", ""), ("vitest.config.ts", "")]),
      Some(("vitest.config.ts".to_string(), ConfigFlavor::Vitest))
    );
    // a vite.config of a project without Vitest does not hide Jest
    assert_eq!(
      found(&[("vite.config.ts", ""), ("jest.config.ts", "")]),
      jest("jest.config.ts")
    );
    assert_eq!(
      found(&[("package.json", r#"{"jest":{"testEnvironment":"node"}}"#)]),
      jest("package.json")
    );
    assert_eq!(
      found(&[("package.json", r#"{"devDependencies":{"jest":"^29"}}"#)]),
      jest("package.json")
    );
    assert_eq!(
      found(&[(
        "package.json",
        r#"{"devDependencies":{"jest":"^29","vitest":"^3"}}"#
      )]),
      None
    );
    assert_eq!(found(&[("package.json", r#"{"name":"x"}"#)]), None);
  }
}
