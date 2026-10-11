// Copyright 2018-2026 the Deno authors. MIT license.

//! Vitest compatibility for `deno test`.
//!
//! Test files written for Vitest run unchanged:
//!
//! - `vitest` (and `vite-plus/test`, `@jest/globals`, `npm:vitest`, ...)
//!   resolve to built-in `deno-vitest:` modules (see [`VitestOverrides`])
//!   instead of the npm package. Those modules are Vitest's own runtime,
//!   vendored under `js/vendor` by `tools/vendor_vitest.ts`, plus a bridge
//!   (`js/runtime.js`) that runs a test file with Vitest's runner while
//!   registering every test with `Deno.test()`.
//! - A `vitest.config.*` / `vite.config.*` file is evaluated once (see
//!   [`config`]); its `test` block picks the test files and configures the
//!   per-file preload (`js/preload.js`): setup files, globals, environment
//!   (the project's own jsdom / happy-dom), mock reset flags, timeouts,
//!   `import.meta.env` and `define`.
//!
//! Module mocking (`vi.mock`) is not implemented yet and throws.

mod config;
mod glob;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleType;
use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::url::Url;
use deno_error::JsErrorBox;
use deno_graph::Module;
use deno_graph::ModuleGraph;
use deno_resolver::graph::SpecifierOverride;
use deno_resolver::graph::SpecifierOverrides;
use deno_runtime::deno_permissions::PermissionsContainer;
use node_resolver::NodeResolutionKind;
use regex::Regex;

pub use self::config::LoadedProject;
use crate::args::TestFlags;
use crate::util::fs::CollectSpecifiersOptions;
use crate::util::fs::collect_specifiers;
use crate::worker::CliMainWorkerFactory;

/// The URL scheme of the built-in Vitest modules.
pub const SCHEME: &str = "deno-vitest";

macro_rules! sources {
  ($($path:literal),* $(,)?) => {
    &[$(($path, include_str!(concat!("js/", $path)))),*]
  };
}

/// The built-in modules, by path below `deno-vitest:///`.
static SOURCES: &[(&str, &str)] = sources!(
  "config.js",
  "config_loader.js",
  "jest_globals.js",
  "preload.js",
  "runtime.js",
  "snapshot_environment.js",
  "vitest.js",
  "shims/empty.js",
  "shims/environment_support.js",
  "shims/rpc.js",
  "shims/std_env.js",
  "shims/tinybench.js",
  "vendor/chai.js",
  "vendor/constants.js",
  "vendor/defaults.js",
  "vendor/display.js",
  "vendor/environments.js",
  "vendor/expect_type.js",
  "vendor/magic_string.js",
  "vendor/offset.js",
  "vendor/pathe.js",
  "vendor/plugins.js",
  "vendor/pretty_format.js",
  "vendor/runner.js",
  "vendor/source_map.js",
  "vendor/sourcemap_codec.js",
  "vendor/spy.js",
  "vendor/task_utils.js",
  "vendor/tinyrainbow.js",
  "vendor/utils.js",
  "vendor/vitest_core.js",
);

const RESOLVED_CONFIG_PATH: &str = "resolved_config.json";

/// The resolved config served as `deno-vitest:///resolved_config.json`.
static RESOLVED_CONFIG: RwLock<Option<Arc<str>>> = RwLock::new(None);

/// The URL of a built-in module.
pub fn module_url(path: &str) -> Url {
  Url::parse(&format!("{SCHEME}:///{path}")).unwrap()
}

/// Loads a `deno-vitest:` module.
pub fn load_module(specifier: &Url) -> Result<ModuleSource, JsErrorBox> {
  let path = specifier.path().trim_start_matches('/');
  if path == RESOLVED_CONFIG_PATH {
    let json = RESOLVED_CONFIG
      .read()
      .unwrap()
      .clone()
      .unwrap_or_else(|| Arc::from("{}"));
    return Ok(ModuleSource::new(
      ModuleType::Json,
      ModuleSourceCode::String(json.to_string().into()),
      specifier,
      None,
    ));
  }
  let Some((_, code)) = SOURCES.iter().find(|(p, _)| *p == path) else {
    return Err(JsErrorBox::generic(format!(
      "Module not found \"{specifier}\"."
    )));
  };
  Ok(ModuleSource::new(
    ModuleType::JavaScript,
    ModuleSourceCode::String(code.to_string().into()),
    specifier,
    None,
  ))
}

/// The built-in module a specifier names, if any: `vitest`, `npm:vitest@x`,
/// `vite-plus/test`, `@jest/globals`, ...
fn builtin_module(raw_specifier: &str, referrer: &Url) -> Option<&'static str> {
  let (name, sub_path) = split_package_specifier(raw_specifier)?;
  let module = match (name, sub_path) {
    ("vitest", "") | ("vite-plus", "/test") => "vitest.js",
    ("vitest", "/config") | ("vite-plus", "/test/config") => "config.js",
    ("vitest", "/globals" | "/importMeta" | "/jsdom")
    | ("vite-plus", "/test/globals" | "/test/importMeta" | "/test/jsdom") => {
      "shims/empty.js"
    }
    ("@jest/globals", "") => "jest_globals.js",
    // A Vite config imports `defineConfig` & co. from Vite itself; only the
    // config file gets the stand-in (plugins keep their real dependencies).
    ("vite" | "vite-plus", "") if is_config_file(referrer) => "config.js",
    _ => return None,
  };
  Some(module)
}

/// Splits `npm:@scope/name@1.2.3/sub` or `name/sub` into the package name
/// and the sub path (with its leading `/`, or empty).
fn split_package_specifier(raw: &str) -> Option<(&str, &str)> {
  let (rest, is_npm) = match raw.strip_prefix("npm:") {
    Some(rest) => (rest.trim_start_matches('/'), true),
    None => (raw, false),
  };
  if rest.is_empty() || rest.starts_with('.') || rest.starts_with('/') {
    return None;
  }
  if !is_npm && rest.contains(':') {
    return None;
  }
  let name_end = if let Some(scoped) = rest.strip_prefix('@') {
    let slash = scoped.find('/')? + 1;
    slash + 1 + scoped[slash..].find('/').unwrap_or(scoped.len() - slash)
  } else {
    rest.find('/').unwrap_or(rest.len())
  };
  let name_and_version = &rest[..name_end];
  let sub_path = &rest[name_end..];
  let name = if is_npm {
    // strip the version requirement: `vitest@^3` / `@jest/globals@29`
    match name_and_version[1..].find('@') {
      Some(at) => &name_and_version[..at + 1],
      None => name_and_version,
    }
  } else {
    name_and_version
  };
  Some((name, sub_path))
}

fn is_config_file(referrer: &Url) -> bool {
  let Some(name) = referrer.path().rsplit('/').next() else {
    return false;
  };
  [
    "vite.config.",
    "vitest.config.",
    "vitest.workspace.",
    "vite.workspace.",
  ]
  .iter()
  .any(|prefix| name.starts_with(prefix))
}

/// A `resolve.alias` / `test.alias` entry of a Vitest config.
#[derive(Debug, Clone)]
pub struct Alias {
  find: AliasFind,
  replacement: String,
}

#[derive(Debug, Clone)]
enum AliasFind {
  Exact(String),
  Regex(Regex),
}

impl Alias {
  fn new(alias: &config::LoadedAlias) -> Result<Self, AnyError> {
    let find = match &alias.find {
      config::LoadedAliasFind::String(s) => AliasFind::Exact(s.clone()),
      config::LoadedAliasFind::Regex { regex, flags } => {
        let mut prefix = String::new();
        if flags.contains('i') {
          prefix.push_str("(?i)");
        }
        if flags.contains('m') {
          prefix.push_str("(?m)");
        }
        if flags.contains('s') {
          prefix.push_str("(?s)");
        }
        AliasFind::Regex(Regex::new(&format!("{prefix}{regex}")).map_err(
          |err| anyhow!("Unsupported alias regex /{regex}/{flags}: {err}"),
        )?)
      }
    };
    Ok(Self {
      find,
      replacement: alias.replacement.clone(),
    })
  }

  /// Applies the alias like `@rollup/plugin-alias` does.
  fn apply(&self, specifier: &str) -> Option<String> {
    match &self.find {
      AliasFind::Exact(find) => {
        if specifier == find {
          Some(self.replacement.clone())
        } else if specifier.starts_with(find.as_str())
          && specifier[find.len()..].starts_with('/')
        {
          Some(format!("{}{}", self.replacement, &specifier[find.len()..]))
        } else {
          None
        }
      }
      AliasFind::Regex(regex) => {
        if !regex.is_match(specifier) {
          return None;
        }
        Some(
          regex
            .replace(specifier, js_replacement_to_rust(&self.replacement))
            .into_owned(),
        )
      }
    }
  }
}

/// Converts a JS `String.prototype.replace` pattern (`$1`, `$&`) to the
/// regex crate's syntax (`${1}`, `${0}`), where `$1a` would name a group.
fn js_replacement_to_rust(replacement: &str) -> String {
  let mut out = String::with_capacity(replacement.len());
  let mut chars = replacement.chars().peekable();
  while let Some(c) = chars.next() {
    if c != '$' {
      out.push(c);
      continue;
    }
    match chars.peek() {
      Some('&') => {
        chars.next();
        out.push_str("${0}");
      }
      Some('$') => {
        chars.next();
        out.push_str("$$");
      }
      Some(d) if d.is_ascii_digit() => {
        let mut digits = String::new();
        while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
          digits.push(*d);
          chars.next();
        }
        out.push_str(&format!("${{{digits}}}"));
      }
      _ => out.push_str("$$"),
    }
  }
  out
}

/// Resolves the built-in Vitest modules and applies config aliases.
#[derive(Debug)]
pub struct VitestOverrides {
  /// Bare specifiers imported by the built-in modules (jsdom, happy-dom)
  /// resolve as if imported from here.
  root_referrer: RwLock<Url>,
  /// The config's aliases (set once the config is loaded).
  aliases: RwLock<Vec<Alias>>,
}

impl VitestOverrides {
  fn new(root: &Path) -> Result<Self, AnyError> {
    Ok(Self {
      root_referrer: RwLock::new(root_referrer(root)?),
      aliases: Default::default(),
    })
  }
}

fn root_referrer(root: &Path) -> Result<Url, AnyError> {
  Ok(
    Url::from_directory_path(root)
      .map_err(|_| anyhow!("Invalid Vitest root {}", root.display()))?
      .join("__deno_vitest_root__.js")?,
  )
}

impl SpecifierOverrides for VitestOverrides {
  fn resolve(
    &self,
    raw_specifier: &str,
    referrer: &Url,
    resolution_kind: NodeResolutionKind,
  ) -> Option<SpecifierOverride> {
    if referrer.scheme() == SCHEME {
      if raw_specifier.starts_with("./")
        || raw_specifier.starts_with("../")
        || raw_specifier.starts_with(&format!("{SCHEME}:"))
      {
        return referrer
          .join(raw_specifier)
          .ok()
          .map(SpecifierOverride::Url);
      }
      if raw_specifier.starts_with("node:") {
        return None;
      }
      return Some(SpecifierOverride::Rewrite {
        specifier: raw_specifier.to_string(),
        referrer: Some(self.root_referrer.read().unwrap().clone()),
      });
    }
    // Type checking keeps resolving to the real package's types.
    if resolution_kind == NodeResolutionKind::Types {
      return None;
    }
    if let Some(module) = builtin_module(raw_specifier, referrer) {
      return Some(SpecifierOverride::Url(module_url(module)));
    }
    let aliases = self.aliases.read().unwrap();
    if aliases.is_empty() || referrer.path().contains("/node_modules/") {
      return None;
    }
    for alias in aliases.iter() {
      if let Some(specifier) = alias.apply(raw_specifier) {
        let specifier = if specifier.starts_with('/') {
          Url::from_file_path(&specifier)
            .map(|u| u.to_string())
            .unwrap_or(specifier)
        } else {
          specifier
        };
        return Some(SpecifierOverride::Rewrite {
          specifier,
          referrer: None,
        });
      }
    }
    None
  }
}

/// A test project of the Vitest config, with its compiled globs.
#[derive(Debug)]
struct Project {
  loaded: LoadedProject,
  base: PathBuf,
  include: Vec<glob::Glob>,
  exclude: Vec<glob::Glob>,
}

impl Project {
  fn new(loaded: LoadedProject) -> Result<Self, AnyError> {
    let base = PathBuf::from(loaded.dir.as_ref().unwrap_or(&loaded.root));
    let compile = |patterns: &[String]| {
      patterns
        .iter()
        .map(|p| {
          glob::Glob::new(p)
            .map_err(|err| anyhow!("Invalid Vitest glob \"{p}\": {err}"))
        })
        .collect::<Result<Vec<_>, _>>()
    };
    Ok(Self {
      include: compile(&loaded.include)?,
      exclude: compile(&loaded.exclude)?,
      base,
      loaded,
    })
  }

  fn matches(&self, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(&self.base) else {
      return false;
    };
    let relative = relative.to_string_lossy().replace('\\', "/");
    self.include.iter().any(|g| g.is_match(&relative))
      && !self.exclude.iter().any(|g| g.is_match(&relative))
  }
}

/// The state of the Vitest compatibility layer for one `deno test` run.
#[derive(Debug, Default)]
pub struct VitestCompat {
  /// The selected projects of the Vitest config (empty without a config).
  projects: Vec<Project>,
  /// Test modules that import a built-in Vitest module (only computed
  /// without a config: with one, every test module is a Vitest file).
  vitest_files: HashSet<Url>,
  has_config: bool,
}

impl VitestCompat {
  /// Installs the built-in modules and loads the Vitest config, if any.
  pub async fn setup(
    resolver: &crate::resolver::CliResolver,
    worker_factory: &CliMainWorkerFactory,
    permissions: PermissionsContainer,
    initial_cwd: &Path,
    test_flags: &TestFlags,
    update_snapshots: bool,
  ) -> Result<Self, AnyError> {
    let config_path =
      config::find_config(initial_cwd, test_flags.vitest.config.as_deref())?;
    // Installed first: the config itself imports `vitest/config`.
    let overrides = Arc::new(VitestOverrides::new(initial_cwd)?);
    resolver.set_specifier_overrides(overrides.clone());
    let mut compat = Self::default();
    let mut aliases = Vec::new();
    if let Some(config_path) = config_path {
      let loaded =
        config::load_config(worker_factory, permissions, &config_path)
          .await
          .map_err(|err| {
            anyhow!(
              "Failed to load the Vitest config {}: {err:#}\n\nPass --no-vitest-compat to run without it.",
              config_path.display()
            )
          })?;
      let selected = &test_flags.vitest.projects;
      for name in selected {
        if !loaded.projects.iter().any(|p| &p.name == name) {
          return Err(anyhow!(
            "No Vitest project named \"{name}\" in {} (projects: {})",
            config_path.display(),
            loaded
              .projects
              .iter()
              .map(|p| format!("\"{}\"", p.name))
              .collect::<Vec<_>>()
              .join(", ")
          ));
        }
      }
      for project in loaded.projects {
        if !selected.is_empty() && !selected.contains(&project.name) {
          continue;
        }
        for alias in &project.alias {
          match Alias::new(alias) {
            Ok(alias) => aliases.push(alias),
            Err(err) => {
              log::warn!("{} {err:#}", deno_runtime::colors::yellow("Warning"))
            }
          }
        }
        compat.projects.push(Project::new(project)?);
      }
      compat.has_config = true;
      if compat.projects.len() > 1 && selected.is_empty() {
        log::warn!(
          "{} the Vitest config has {} projects; each test file runs once, in the first project that includes it. Select projects with --vitest-project.",
          deno_runtime::colors::yellow("Warning"),
          compat.projects.len()
        );
      }
      set_resolved_config(
        &compat.projects,
        update_snapshots,
        test_flags.filter.as_deref(),
      );
    } else {
      set_resolved_config(&[], update_snapshots, test_flags.filter.as_deref());
    }
    if let Some(project) = compat.projects.first() {
      *overrides.root_referrer.write().unwrap() =
        root_referrer(Path::new(&project.loaded.root))?;
    }
    *overrides.aliases.write().unwrap() = aliases;
    Ok(compat)
  }

  /// Whether a Vitest config was found.
  pub fn has_config(&self) -> bool {
    self.has_config
  }

  /// The test modules the config's `include` / `exclude` select.
  pub fn discover_specifiers(&self) -> Result<Vec<Url>, AnyError> {
    let mut seen = HashSet::new();
    let mut specifiers = Vec::new();
    for project in &self.projects {
      let found = collect_specifiers(
        CollectSpecifiersOptions {
          file_patterns: deno_config::glob::FilePatterns::new_with_base(
            project.base.clone(),
          ),
          vendor_folder: None,
          include_ignored_specified: false,
        },
        |entry| project.matches(entry.path),
      )?;
      for specifier in found {
        if seen.insert(specifier.clone()) {
          specifiers.push(specifier);
        }
      }
    }
    specifiers.sort();
    Ok(specifiers)
  }

  /// Without a config, finds the test modules that (transitively, through
  /// local modules) import a built-in Vitest module.
  pub fn detect_vitest_files(&mut self, graph: &ModuleGraph, roots: &[Url]) {
    if self.has_config {
      return;
    }
    let mut importers: HashMap<&Url, Vec<&Url>> = HashMap::new();
    let mut queue = VecDeque::new();
    for module in graph.modules() {
      let Module::Js(module) = module else {
        continue;
      };
      for dep in module.dependencies.values() {
        for resolved in [dep.maybe_code.ok(), dep.maybe_type.ok()]
          .into_iter()
          .flatten()
        {
          if resolved.specifier.scheme() == SCHEME {
            queue.push_back(&module.specifier);
          } else {
            importers
              .entry(&resolved.specifier)
              .or_default()
              .push(&module.specifier);
          }
        }
      }
    }
    let mut marked = HashSet::new();
    while let Some(specifier) = queue.pop_front() {
      if !marked.insert(specifier) {
        continue;
      }
      if let Some(parents) = importers.get(specifier) {
        queue.extend(parents.iter().copied());
      }
    }
    self.vitest_files = roots
      .iter()
      .filter(|r| marked.contains(r))
      .cloned()
      .collect();
  }

  /// The preload module that runs `specifier` with Vitest's runner, or
  /// `None` when it is not a Vitest test file.
  pub fn preload_for(&self, specifier: &Url) -> Option<Url> {
    if !self.has_config && !self.vitest_files.contains(specifier) {
      return None;
    }
    let project = deno_path_util::url_to_file_path(specifier)
      .ok()
      .and_then(|path| self.projects.iter().find(|p| p.matches(&path)))
      .or_else(|| self.projects.first());
    let mut url = module_url("preload.js");
    {
      let mut query = url.query_pairs_mut();
      query.append_pair("file", specifier.as_str());
      if let Some(project) = project {
        query.append_pair("project", &project.loaded.name);
      }
      if let Some(environment) = file_environment(specifier) {
        query.append_pair("environment", &environment);
      }
    }
    Some(url)
  }
}

fn set_resolved_config(
  projects: &[Project],
  update_snapshots: bool,
  filter: Option<&str>,
) {
  let ci = std::env::var("CI")
    .map(|v| !v.is_empty() && v != "false")
    .unwrap_or(false);
  let json = deno_core::serde_json::json!({
    "update": update_snapshots,
    "ci": ci,
    // `--filter`, so Vitest skips the hooks of suites with no selected test.
    "filter": filter,
    "projects": projects
      .iter()
      .map(|p| {
        let mut config = p.loaded.config.clone();
        if let Some(object) = config.as_object_mut() {
          object.insert("name".to_string(), p.loaded.name.clone().into());
        }
        config
      })
      .collect::<Vec<_>>(),
  });
  *RESOLVED_CONFIG.write().unwrap() = Some(Arc::from(json.to_string()));
}

/// The test environment a file asks for with a `@vitest-environment` (or
/// `@jest-environment`) docblock comment.
fn file_environment(specifier: &Url) -> Option<String> {
  let path = deno_path_util::url_to_file_path(specifier).ok()?;
  let text = std::fs::read_to_string(path).ok()?;
  parse_environment_comment(&text)
}

fn parse_environment_comment(text: &str) -> Option<String> {
  let head = &text[..text.len().min(4096)];
  for marker in ["@vitest-environment", "@jest-environment"] {
    let Some(index) = head.find(marker) else {
      continue;
    };
    let name: String = head[index + marker.len()..]
      .trim_start_matches([' ', '\t'])
      .chars()
      .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
      .collect();
    if !name.is_empty() {
      return Some(name);
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  fn referrer() -> Url {
    Url::parse("file:///project/src/a.test.ts").unwrap()
  }

  #[test]
  fn builtin_modules() {
    let r = referrer();
    assert_eq!(builtin_module("vitest", &r), Some("vitest.js"));
    assert_eq!(builtin_module("npm:vitest", &r), Some("vitest.js"));
    assert_eq!(builtin_module("npm:vitest@^3.2.0", &r), Some("vitest.js"));
    assert_eq!(
      builtin_module("npm:/vitest@3/config", &r),
      Some("config.js")
    );
    assert_eq!(builtin_module("vitest/config", &r), Some("config.js"));
    assert_eq!(builtin_module("vite-plus/test", &r), Some("vitest.js"));
    assert_eq!(
      builtin_module("vite-plus/test/config", &r),
      Some("config.js")
    );
    assert_eq!(builtin_module("@jest/globals", &r), Some("jest_globals.js"));
    assert_eq!(
      builtin_module("npm:@jest/globals@29", &r),
      Some("jest_globals.js")
    );
    assert_eq!(builtin_module("vitest/globals", &r), Some("shims/empty.js"));
    assert_eq!(builtin_module("vitest-foo", &r), None);
    assert_eq!(builtin_module("@vitest/spy", &r), None);
    assert_eq!(builtin_module("./vitest", &r), None);
    assert_eq!(builtin_module("jsr:@std/expect", &r), None);
    // Vite itself only for config files.
    assert_eq!(builtin_module("vite", &r), None);
    let config = Url::parse("file:///project/vite.config.ts").unwrap();
    assert_eq!(builtin_module("vite", &config), Some("config.js"));
    assert_eq!(builtin_module("vite-plus", &config), Some("config.js"));
  }

  #[test]
  fn overrides_from_builtin_modules() {
    let overrides = VitestOverrides {
      root_referrer: RwLock::new(
        Url::parse("file:///project/__deno_vitest_root__.js").unwrap(),
      ),
      aliases: Default::default(),
    };
    let builtin = module_url("vitest.js");
    assert_eq!(
      overrides.resolve(
        "./vendor/chai.js",
        &builtin,
        NodeResolutionKind::Execution
      ),
      Some(SpecifierOverride::Url(module_url("vendor/chai.js")))
    );
    assert_eq!(
      overrides.resolve("jsdom", &builtin, NodeResolutionKind::Execution),
      Some(SpecifierOverride::Rewrite {
        specifier: "jsdom".to_string(),
        referrer: Some(overrides.root_referrer.read().unwrap().clone()),
      })
    );
    assert_eq!(
      overrides.resolve("node:fs", &builtin, NodeResolutionKind::Execution),
      None
    );
    assert_eq!(
      overrides.resolve("vitest", &referrer(), NodeResolutionKind::Types),
      None
    );
  }

  #[test]
  fn aliases() {
    let exact = Alias {
      find: AliasFind::Exact("@".to_string()),
      replacement: "/project/src".to_string(),
    };
    assert_eq!(exact.apply("@/a"), Some("/project/src/a".to_string()));
    assert_eq!(exact.apply("@"), Some("/project/src".to_string()));
    assert_eq!(exact.apply("@scope/pkg"), None);
    let regex = Alias {
      find: AliasFind::Regex(Regex::new("^react$").unwrap()),
      replacement: "preact/compat".to_string(),
    };
    assert_eq!(regex.apply("react"), Some("preact/compat".to_string()));
    assert_eq!(regex.apply("react-dom"), None);
    let groups = Alias {
      find: AliasFind::Regex(Regex::new("^~/(.*)$").unwrap()),
      replacement: "/src/$1".to_string(),
    };
    assert_eq!(groups.apply("~/a/b"), Some("/src/a/b".to_string()));
    let suffix = Alias {
      find: AliasFind::Regex(Regex::new("^@x/(.*)$").unwrap()),
      replacement: "/lib/$1abc/$&".to_string(),
    };
    assert_eq!(suffix.apply("@x/y"), Some("/lib/yabc/@x/y".to_string()));
  }

  #[test]
  fn environment_comment() {
    assert_eq!(
      parse_environment_comment("// @vitest-environment jsdom\nimport x;"),
      Some("jsdom".to_string())
    );
    assert_eq!(
      parse_environment_comment("/**\n * @vitest-environment happy-dom\n */\n"),
      Some("happy-dom".to_string())
    );
    assert_eq!(
      parse_environment_comment("/** @jest-environment node */"),
      Some("node".to_string())
    );
    assert_eq!(parse_environment_comment("import x;"), None);
  }
}
