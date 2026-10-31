// Copyright 2018-2026 the Deno authors. MIT license.

//! Post-install setup for stock TypeScript compatibility.
//!
//! After dependency resolution, this module:
//! 1. Installs jsr: compatibility packages via npm.jsr.io
//! 2. Mirrors http(s): modules into .deno/remote/<host><path>/...
//! 3. Generates .deno/tsconfig.json with paths mappings for npm:/jsr:/https:
//! 4. In global-cache mode, generates per-package referenced tsconfigs so
//!    stock TypeScript can preserve Deno's contextual npm resolution.
//!
//! This enables stock TypeScript tooling (tsc, tsserver, VS Code) to work
//! with Deno projects that use jsr:, npm:, and http(s): specifiers.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;

use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::serde_json::json;
use deno_core::url::Url;
use deno_runtime::deno_permissions::PermissionsContainer;
use deno_semver::Version;
use deno_semver::VersionReq;
use flate2::read::GzDecoder;

use crate::file_fetcher::CliFileFetcher;
use crate::http_util::HttpClient;
use crate::npm::CliNpmResolver;

/// The `@types/node` version installed for projects that don't bring their own.
///
/// Pinned, and kept equal to the version `cli/tsc/dts/node` is generated from
/// (`tools/update_types_node.ts` reads this constant), so a project type-checks
/// the same way whether it uses the downloaded typings or the built-in ones,
/// and so a `@types/node` release doesn't silently change anyone's diagnostics.
/// Bump both by running `./tools/update_types_node.ts` after editing this.
pub const TYPES_NODE_VERSION: &str = "24.2.0";

/// Installed JSR package info for reporting.
pub struct InstalledJsrPackage {
  /// e.g. "@jsr/std__assert"
  pub name: String,
  /// e.g. "1.0.19"
  pub version: String,
}

#[derive(Default)]
struct NpmCacheProjects {
  /// Exact npm: specifier/import-map target to its resolved package folder.
  package_paths: BTreeMap<String, PathBuf>,
  /// Paths relative to `.deno/tsconfig.json` for the generated package
  /// projects.
  references: Vec<String>,
}

struct NodeTypesSetup {
  type_root: Option<String>,
  undici_types_dir: Option<PathBuf>,
}

fn path_for_typescript(path: &Path) -> String {
  path.to_string_lossy().replace('\\', "/")
}

/// denext (DENEXT.md item 190): where the npm-shaped copies stock TypeScript
/// reads for a project live (the `@jsr/*` packages in global-cache mode and
/// the built-in `@types/node`): `<DENO_DIR>/npm_compat/<name>-<hash>`, one
/// folder per project root, outside the project. Inside it they were picked
/// up by the project's own tools: a default `deno test` ran the `_test.ts`
/// files the JSR packages ship. Without a `DENO_DIR` (it could not be
/// resolved) it is the old `.deno/npm-compat`.
pub fn npm_compat_dir(
  deno_dir_root: Option<&Path>,
  project_root: &Path,
) -> PathBuf {
  let Some(deno_dir_root) = deno_dir_root else {
    return legacy_npm_compat_dir(project_root);
  };
  deno_dir_root
    .join("npm_compat")
    .join(npm_compat_project_key(project_root))
}

/// The `.deno/npm-compat` a type check wrote into the project before item
/// 190.
fn legacy_npm_compat_dir(project_root: &Path) -> PathBuf {
  project_root.join(".deno").join("npm-compat")
}

/// `<name>-<hash>` for a project root: its folder name (kept to
/// `[A-Za-z0-9._-]`, at most 32 characters, for a person looking in the
/// cache) and the first 16 hex digits of the SHA-256 of its canonical path
/// (the path as given when it cannot be canonicalized).
fn npm_compat_project_key(project_root: &Path) -> String {
  use sha2::Digest;
  let canonical = std::fs::canonicalize(project_root)
    .unwrap_or_else(|_| project_root.to_path_buf());
  let hash = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
  let hash = faster_hex::hex_string(&hash[..8]);
  let name: String = canonical
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_default()
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
        c
      } else {
        '_'
      }
    })
    .take(32)
    .collect();
  let name = name.trim_start_matches('.');
  if name.is_empty() {
    hash
  } else {
    format!("{name}-{hash}")
  }
}

fn collect_typescript_project_files(package_folder: &Path) -> Vec<String> {
  fn collect(dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let Ok(file_type) = entry.file_type() else {
        continue;
      };
      if file_type.is_symlink() {
        continue;
      }
      if file_type.is_dir() {
        collect(&path, files);
        continue;
      }
      let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
      if file_name.ends_with(".d.ts")
        || file_name.ends_with(".d.mts")
        || file_name.ends_with(".d.cts")
      {
        files.push(path_for_typescript(&path));
      }
    }
  }

  let mut files = Vec::new();
  collect(package_folder, &mut files);
  files.sort();
  files
}

/// Build the `paths` entries for a package project's edge to one dependency.
///
/// The bare `alias -> [folder]` mapping is always correct: stock tsc reads the
/// folder's own `package.json` (`types` / `exports["."]`) to resolve the root
/// import, so it is emitted unconditionally.
///
/// For subpath imports (`dep/subpath`), a literal `alias/* -> folder/*`
/// wildcard performs a raw path substitution that ignores the dependency's
/// `exports` map, so a transitive `import "dep/subpath"` where the dependency
/// remaps `./subpath` via `exports` mis-resolves to a nonexistent path. When
/// the dependency declares an `exports` map we instead emit an exact
/// `alias/<subpath> -> [<resolved declaration>]` entry per export key,
/// preferring the generated declaration (mirroring how the top-level
/// `generate_npm_paths` handles subpaths). The `alias/*` wildcard is kept only
/// as a fallback for packages without an `exports` map.
fn dependency_project_paths(
  alias: &str,
  dependency_folder: &Path,
) -> serde_json::Map<String, Value> {
  let mut paths = serde_json::Map::new();
  let folder = path_for_typescript(dependency_folder);
  paths.insert(alias.to_string(), json!([folder]));

  let export_keys =
    crate::tsc::tsconfig_gen::package_export_keys(dependency_folder);
  if export_keys.is_empty() {
    // No subpath exports to consult (root-only or no `exports` map): fall back
    // to a literal subpath wildcard so `dep/subpath` still resolves to
    // `folder/subpath`.
    paths.insert(format!("{alias}/*"), json!([format!("{folder}/*")]));
    return paths;
  }

  // Emit one exact mapping per declared subpath export. Export keys may
  // themselves be wildcards (`"./features/*"`); the resolved value keeps the
  // `*` and the emitted key is a matching wildcard, so those resolve too.
  for exp_key in export_keys {
    let sub = exp_key.trim_start_matches("./");
    let Some(resolved) =
      crate::tsc::tsconfig_gen::resolve_package_types_entry_path(
        dependency_folder,
        &exp_key,
      )
      .filter(|p| p.exists())
      .or_else(|| {
        crate::tsc::tsconfig_gen::resolve_package_source_entry_path(
          dependency_folder,
          &exp_key,
        )
      })
    else {
      continue;
    };
    paths.insert(
      format!("{alias}/{sub}"),
      json!([path_for_typescript(&resolved)]),
    );
  }
  paths
}

/// Generate a referenced TypeScript project for every resolved package copy in
/// Deno's global npm cache.
///
/// A package project owns the cached package's type/source files and maps every
/// bare dependency name to the exact cache folder selected by the npm snapshot.
/// TypeScript uses these compiler options as a "project reference redirect"
/// while resolving imports from the package, which gives stock tsc the same
/// per-referrer dependency context as Deno's resolver without materializing a
/// node_modules tree.
fn generate_npm_cache_projects(
  project_root: &Path,
  deno_imports: Option<&Value>,
  npm_resolver: &CliNpmResolver,
) -> Result<NpmCacheProjects, AnyError> {
  let projects_root = project_root.join(".deno/npm");
  if projects_root.exists() {
    std::fs::remove_dir_all(&projects_root)?;
  }
  let Some(managed) = npm_resolver.as_managed() else {
    return Ok(Default::default());
  };
  if managed.root_node_modules_path().is_some() {
    return Ok(Default::default());
  }

  let snapshot = managed.resolution().snapshot();
  let mut packages = snapshot
    .all_packages_for_every_system()
    .filter_map(|package| {
      let folder = managed.resolve_pkg_folder_from_pkg_id(&package.id).ok()?;
      folder.exists().then_some((package, folder))
    })
    .collect::<Vec<_>>();
  packages.sort_by_key(|(package, _)| package.id.as_serialized());

  let mut result = NpmCacheProjects::default();
  for (package, package_folder) in packages {
    let files = collect_typescript_project_files(&package_folder);
    if files.is_empty() {
      continue;
    }
    let folder_name = deno_resolver::npm::get_package_folder_id_folder_name(
      &package.get_package_cache_folder_id(),
    );
    let project_dir = projects_root.join(&folder_name);
    std::fs::create_dir_all(&project_dir)?;

    let mut paths = serde_json::Map::new();
    let mut dependencies = package.dependencies.iter().collect::<Vec<_>>();
    dependencies.sort_by_key(|(alias, _)| *alias);
    for (alias, dependency_id) in dependencies {
      let Ok(dependency_folder) =
        managed.resolve_pkg_folder_from_pkg_id(dependency_id)
      else {
        continue;
      };
      if !dependency_folder.exists() {
        continue;
      }
      for (key, value) in dependency_project_paths(alias, &dependency_folder) {
        paths.insert(key, value);
      }
    }

    let config = json!({
      "_deno_generated": true,
      "compilerOptions": {
        "composite": true,
        "module": "esnext",
        "moduleResolution": "bundler",
        "paths": paths,
        "skipLibCheck": true,
        "types": [],
      },
      // `files` is intentional rather than an `include` glob. TypeScript only
      // applies a referenced project's resolver options to cache files it can
      // identify as explicit project files.
      "files": files,
    });
    std::fs::write(
      project_dir.join("tsconfig.json"),
      serde_json::to_string_pretty(&config)?,
    )?;
    result
      .references
      .push(format!("./npm/{folder_name}/tsconfig.json"));
  }

  if let Some(imports) = deno_imports.and_then(|v| v.as_object()) {
    for target in imports.values().filter_map(|v| v.as_str()) {
      if !target.starts_with("npm:") {
        continue;
      }
      let Ok(req_ref) =
        deno_semver::npm::NpmPackageReqReference::from_str(target)
      else {
        continue;
      };
      if let Ok(folder) =
        managed.resolve_pkg_folder_from_deno_module_req(req_ref.req())
        && folder.exists()
      {
        result.package_paths.insert(target.to_string(), folder);
      }
    }
  }

  Ok(result)
}

/// Run post-install setup: install jsr packages and generate tsconfig.
///
/// Called after the sync command builds the graph and initializes npm
/// resolution. Returns the list of newly installed JSR packages for reporting.
fn is_special_specifier(s: &str) -> bool {
  s.starts_with("jsr:")
    || s.starts_with("npm:")
    || s.starts_with("http://")
    || s.starts_with("https://")
}

#[allow(
  clippy::too_many_arguments,
  reason = "threads the independent inputs needed to set up stock-tsc compat"
)]
pub async fn setup_npm_compat(
  project_root: &Path,
  npm_compat_dir: &Path,
  root_config: Option<&Path>,
  import_map_flag: Option<&Path>,
  file_fetcher: &CliFileFetcher,
  http_client: &HttpClient,
  permissions: &PermissionsContainer,
  graph_specifiers: &[String],
  local_wasm_modules: &[(Url, String)],
  npm_resolver: &CliNpmResolver,
  resolved_compiler_options: Option<&Value>,
  manage_root_tsconfig: bool,
  type_check_remote: bool,
) -> Result<Vec<InstalledJsrPackage>, AnyError> {
  // The root config the workspace loaded (denext): `--config other.json`, or
  // `deno publish`'s `jsr.json(c)`, is not the root's `deno.json`.
  let deno_json = root_config.map(read_config_file).transpose()?;
  let deno_compiler_options = deno_json
    .as_ref()
    .and_then(|d| d.get("compilerOptions"))
    .cloned();

  // Workspace member aliases (`@std/assert` -> the local `./assert` member's
  // exports) shadow any published jsr mapping, so compute them up front and let
  // them win in the generated `paths`.
  let mut member_paths = deno_json
    .as_ref()
    .map(|d| workspace_member_paths(project_root, d))
    .unwrap_or_default();

  // Combine the root import map (inline `imports`, or an `importMap` file) with
  // the specifiers discovered in the module graph. Graph specifiers (e.g.
  // `jsr:@std/path` written directly in source) are keyed by themselves so the
  // import-map-driven generation maps them like an alias.
  let mut combined = deno_json
    .as_ref()
    .and_then(|d| d.get("imports"))
    .and_then(|v| v.as_object())
    .cloned()
    .unwrap_or_default();
  // denext: an import map given with `--import-map` is the one Deno uses (the
  // config's `importMap` is then ignored, with a warning).
  if let Some(map_path) = import_map_flag {
    for (k, v) in
      read_import_map_file(project_root, map_path).unwrap_or_default()
    {
      combined.insert(k, v);
    }
  } else if let Some(map_imports) = deno_json
    .as_ref()
    .and_then(|d| read_referenced_import_map(project_root, d))
  {
    for (k, v) in map_imports {
      combined.entry(k).or_insert(v);
    }
  }
  // denext: a workspace member's own `imports` (`"bar": "jsr:@foo/bar@1"` in
  // `foo/deno.json`) resolve that member's modules in Deno. The generated
  // `paths` serve the whole project, so a member's entry joins them unless
  // the root or an earlier member maps the key already (a key the members map
  // differently keeps the in-process checker: `TypeChecker::native_unsupported`).
  if let Some(d) = deno_json.as_ref() {
    for (key, value) in workspace_member_imports(project_root, d) {
      combined.entry(key).or_insert(value);
    }
  }
  // denext: without a `node_modules` (`nodeModulesDir: "none"`) a bare import
  // of a root `package.json` dependency resolves to npm in Deno, but nothing
  // on disk holds it for the compiler: map it as an import map entry would.
  let global_cache_layout = npm_resolver
    .as_managed()
    .is_some_and(|managed| managed.root_node_modules_path().is_none());
  if global_cache_layout {
    for (name, target) in package_json_dependency_imports(project_root) {
      combined.entry(name).or_insert(Value::String(target));
    }
  }
  // Snapshot the import-map alias -> target pairs (before adding graph specs)
  // so we can resolve bare specifiers against them.
  let alias_targets: Vec<(String, String)> = combined
    .iter()
    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
    .collect();

  // An alias that points at `jsr:`/`npm:<name>` where `<name>` is itself a
  // workspace member (e.g. `"fresh": "jsr:@fresh/core"` in a repo whose
  // `packages/fresh` member is named `@fresh/core`) must resolve to the LOCAL
  // member, not the published registry copy. Otherwise imports through the alias
  // get the registry package's types while local/relative imports get the
  // member's, and the two identical-looking types don't unify.
  add_member_alias_paths(&mut member_paths, &alias_targets);
  for spec in graph_specifiers {
    // denext: an absolute `file:` import is keyed to itself too, and
    // `add_file_url_paths` maps it to the file on disk below.
    if is_special_specifier(spec) || spec.starts_with("file:") {
      // Direct scheme specifier: key it to itself.
      combined
        .entry(spec.clone())
        .or_insert_with(|| Value::String(spec.clone()));
    } else if let Some(resolved) =
      resolve_bare_against_import_map(spec, &alias_targets)
    {
      // Bare alias (+ maybe subpath): map it to the resolved scheme specifier
      // so the per-flavor path generators handle it (e.g. `@std/fmt/colors` ->
      // `jsr:@std/fmt@^1/colors`, `lume/foo` -> `https://.../lume@3/foo`).
      combined
        .entry(spec.clone())
        .or_insert(Value::String(resolved));
    }
  }

  // denext: the compiler's module resolution has no notion of a `file:` URL
  // (Deno resolves one from anywhere, the in-process checker too), so map
  // every `file:` import, written in source or reached through an import map,
  // to its path.
  add_file_url_paths(&mut member_paths, &combined);

  // Always generate the base config: even a project with no external deps needs
  // the generated tsconfig + private `@types/deno` so stock tooling sees the
  // Deno globals. There's simply nothing to map into `paths` when there are no
  // external specifiers or workspace members.
  let combined_imports = Value::Object(combined);
  let deno_imports = Some(&combined_imports);
  let deno_compiler_options = deno_compiler_options.as_ref();

  // Stock TypeScript has no hook into Deno's contextual global-cache npm
  // resolver. Model that context with project references: every resolved npm
  // package copy gets a tiny tsconfig whose `paths` point at the exact package
  // copies selected for its dependency edges. TypeScript applies the referenced
  // project's compiler options while resolving imports from files owned by that
  // project, so duplicate versions and peer-dependency copies remain distinct.
  let npm_cache_projects =
    generate_npm_cache_projects(project_root, deno_imports, npm_resolver)?;
  let use_global_cache_layout = global_cache_layout;

  // denext (DENEXT.md item 190): an earlier type check's copy inside the
  // project is removed (it is generated, and a default `deno test` ran the
  // packages' tests from it).
  let legacy_dir = legacy_npm_compat_dir(project_root);
  if npm_compat_dir != legacy_dir && legacy_dir.is_dir() {
    if let Err(e) = std::fs::remove_dir_all(&legacy_dir) {
      log::debug!("Failed to remove {}: {e}", legacy_dir.display());
    }
  }
  let jsr_packages_dir = if use_global_cache_layout {
    npm_compat_dir.join("@jsr")
  } else {
    project_root.join("node_modules/@jsr")
  };
  let installed = install_jsr_packages(
    &jsr_packages_dir,
    deno_imports,
    &member_paths,
    http_client,
  )
  .await?;

  // Mirror http(s): modules (and their transitive remote/relative imports)
  // into .deno/remote/<host><path>/...
  let http_modules = match install_http_modules(
    project_root,
    deno_imports,
    file_fetcher,
    permissions,
    type_check_remote,
  )
  .await
  {
    Ok(map) => map,
    Err(e) => {
      log::warn!("Failed to materialize remote modules: {e}");
      BTreeMap::new()
    }
  };

  // Local `.wasm` imports (`import { x } from "./foo.wasm"`) can't be checked as
  // binaries: stock tsc reports a spurious `TS2307 Cannot find module`. Mirror
  // each wasm module's generated typed declarations into `.deno/wasm/<rel>`
  // (renamed to `.d.wasm.ts` so `allowArbitraryExtensions` picks them up) and
  // add `.deno/wasm` as a `rootDirs` overlay of the project root so tsc resolves
  // the relative `./foo.wasm` specifier to the mirror. Unlike remote wasm (a
  // non-relative specifier handled via `paths`), a relative specifier is never
  // matched by tsconfig `paths`, hence the `rootDirs` overlay.
  let has_local_wasm =
    match install_local_wasm_modules(project_root, local_wasm_modules) {
      Ok(has_wasm) => has_wasm,
      Err(e) => {
        log::warn!("Failed to materialize local wasm modules: {e}");
        false
      }
    };

  // Warn about npm packages that could be resolved neither through a local
  // node_modules directory nor through the generated global-cache projects.
  let mut unmaterialized: Vec<String> = combined_imports
    .as_object()
    .into_iter()
    .flatten()
    .filter_map(|(_k, v)| v.as_str())
    .filter(|s| s.starts_with("npm:"))
    .filter(|s| !npm_cache_projects.package_paths.contains_key(*s))
    .filter_map(|s| deno_semver::npm::NpmPackageReqReference::from_str(s).ok())
    .map(|r| r.req().name.to_string())
    .filter(|name| !project_root.join(format!("node_modules/{name}")).exists())
    .collect();
  unmaterialized.sort();
  unmaterialized.dedup();
  if !unmaterialized.is_empty() {
    log::warn!(
      "sync-types: {} npm package(s) are not present under node_modules and \
       were left unmapped ({}). Enable a node_modules directory (e.g. set \
       \"nodeModulesDir\": \"auto\" in deno.json) so stock TypeScript can \
       resolve them.",
      unmaterialized.len(),
      unmaterialized.join(", "),
    );
  }

  // Ensure @types/node is available so Node globals (timers, node: builtins,
  // Buffer, URLPattern, ...) resolve under stock tooling.
  let node_types =
    ensure_types_node(project_root, npm_compat_dir, use_global_cache_layout);
  if let Some(undici_types_dir) = &node_types.undici_types_dir {
    member_paths.insert(
      "undici-types".to_string(),
      json!([path_for_typescript(undici_types_dir)]),
    );
    member_paths.insert(
      "undici-types/*".to_string(),
      json!([format!("{}/*", path_for_typescript(undici_types_dir))]),
    );
  }

  // The project's own `exclude` (from deno.json) tells us which paths Deno
  // doesn't check (test fixtures, generated output); mirror it into the tsconfig
  // so we don't surface diagnostics for files Deno itself skips.
  let mut excludes: Vec<String> = deno_json
    .as_ref()
    .and_then(|d| d.get("exclude"))
    .and_then(|v| v.as_array())
    .map(|arr| {
      arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
    })
    .unwrap_or_default();

  // Exclude Deno's vendor directory. Vendored deps are third-party code Deno
  // doesn't type-check, and their `jsr:`/`npm:` imports resolve through Deno's
  // vendor map (not ours), so type-checking them here just surfaces the deps'
  // own errors. The vendor dir is `vendor/` at the project root when `vendor`
  // is enabled in deno.json or the directory is present.
  let vendor_enabled = deno_json
    .as_ref()
    .and_then(|d| d.get("vendor"))
    .and_then(|v| v.as_bool())
    .unwrap_or(false);
  if vendor_enabled || project_root.join("vendor").is_dir() {
    excludes.push("vendor".to_string());
  }
  // denext: the generated `.deno/` itself is never a source of the project:
  // what the compiler needs from it is reached through `types`, `typeRoots`
  // and `paths`. Leaving it to the open `include` made a whole-project check
  // pick up another check's staging directory (`.node.tmp-<pid>-<n>`) while it
  // was being renamed into place.
  excludes.push(".deno".to_string());

  // Generate .deno/tsconfig.json and ensure root tsconfig.json extends it
  generate_deno_tsconfig(
    project_root,
    deno_compiler_options,
    resolved_compiler_options,
    deno_imports,
    &http_modules,
    &member_paths,
    &jsr_packages_dir,
    &npm_cache_projects.package_paths,
    &npm_cache_projects.references,
    node_types.type_root.as_deref(),
    &excludes,
    has_local_wasm,
    manage_root_tsconfig,
  )?;

  Ok(installed)
}

/// Resolve a bare specifier (import-map alias, possibly with a subpath) against
/// the import map by longest-prefix match, returning the composed scheme
/// specifier if the matched alias targets `npm:`/`jsr:`/`http(s):`.
///
/// - exact alias:            `@fresh/core`      + `@fresh/core` -> `jsr:@fresh/core@^2`
/// - alias + subpath:        `@std/fmt/colors`  + `@std/fmt`    -> `jsr:@std/fmt@^1/colors`
/// - trailing-slash prefix:  `lume/foo`         + `lume/`       -> `https://.../lume@3/foo`
fn resolve_bare_against_import_map(
  spec: &str,
  alias_targets: &[(String, String)],
) -> Option<String> {
  let mut best: Option<(&str, &str)> = None;
  for (alias, target) in alias_targets {
    let matches = spec == alias
      || (alias.ends_with('/') && spec.starts_with(alias.as_str()))
      || (!alias.ends_with('/') && spec.starts_with(&format!("{alias}/")));
    if matches && best.is_none_or(|(b, _)| alias.len() > b.len()) {
      best = Some((alias, target));
    }
  }
  let (alias, target) = best?;
  if !is_special_specifier(target) {
    return None;
  }
  // Append whatever of `spec` extends past the matched alias. For a trailing-
  // slash prefix that's `foo`; for a non-slash alias it's `/subpath` (or empty
  // for an exact match).
  Some(format!("{}{}", target, &spec[alias.len()..]))
}

/// Ensure an `@types/node` (with its `undici-types`) is present so the
/// generated tsconfig can load Node's and the web's globals (timers, `node:`
/// builtins, `Buffer`, `URLPattern`, ...).
///
/// If the project already installed `@types/node` under its `node_modules`, we
/// reuse that in place. Otherwise the copy built into the binary - the one the
/// in-process checker uses (`cli/tsc/dts/node`, generated from
/// `TYPES_NODE_VERSION`, `undici-types` included) - is written under
/// `npm_compat_dir` ([`npm_compat_dir`], outside the project): type checking
/// never needs the network for these globals (denext), and never writes into
/// the project's real `node_modules`.
fn ensure_types_node(
  project_root: &Path,
  npm_compat_dir: &Path,
  use_global_cache_layout: bool,
) -> NodeTypesSetup {
  // Reuse an @types/node the project already installed under node_modules.
  if !use_global_cache_layout {
    let node_modules = project_root.join("node_modules");
    // This tree is owned by the project's package manager rather than the
    // atomic publisher below, so `package.json` is not a valid completion
    // marker here.
    if node_modules.join("@types/node").exists() {
      let undici_types_dir = node_modules.join("undici-types");
      return NodeTypesSetup {
        type_root: Some("../node_modules/@types".to_string()),
        undici_types_dir: undici_types_dir.exists().then_some(undici_types_dir),
      };
    }
  }
  let types_dir = npm_compat_dir.join("@types");
  let node_dir = types_dir.join("node");
  if let Err(e) = write_embedded_types_node(&node_dir) {
    log::debug!("Failed to write the built-in @types/node: {e}");
    return NodeTypesSetup {
      type_root: None,
      undici_types_dir: None,
    };
  }
  NodeTypesSetup {
    type_root: Some(path_for_typescript(&types_dir)),
    undici_types_dir: Some(node_dir.join("undici")),
  }
}

/// Whether `dir` holds the built-in `@types/node` this binary writes (an
/// earlier Deno's npm download, with its separate `undici-types`, does not
/// count: it is replaced).
fn is_embedded_types_node(dir: &Path) -> bool {
  crate::tsc::tsconfig_gen::is_built_in_types_node(dir)
    && is_materialized_package_at_version(dir, TYPES_NODE_VERSION)
}

/// Write the built-in `@types/node` into `dest` as a package, atomically (see
/// `publish_dir_atomic`). A complete copy already there is kept.
fn write_embedded_types_node(dest: &Path) -> Result<(), AnyError> {
  if is_embedded_types_node(dest) {
    return Ok(());
  }
  publish_dir_atomic(dest, is_embedded_types_node, |dir| {
    for (name, asset) in crate::tsc::LAZILY_LOADED_STATIC_ASSETS.iter() {
      let Some(relative) = name.strip_prefix("node/") else {
        continue;
      };
      let path = dir.join(relative);
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
      }
      std::fs::write(&path, asset.source.as_str())?;
    }
    let mut manifest = json!({
      "name": "@types/node",
      "version": TYPES_NODE_VERSION,
      "types": "index.d.cts",
    });
    manifest[crate::tsc::tsconfig_gen::BUILT_IN_TYPES_NODE_MARKER] =
      json!(true);
    std::fs::write(
      dir.join("package.json"),
      serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(())
  })
}

/// The root `package.json`'s npm dependencies (`dependencies`, then
/// `devDependencies`) as import-map entries: `"picocolors": "1"` ->
/// `npm:picocolors@1`, an `npm:` alias as written. Other kinds (`workspace:`,
/// `file:`, git, URLs) are left to the compiler.
fn package_json_dependency_imports(
  project_root: &Path,
) -> Vec<(String, String)> {
  let Ok(content) = std::fs::read_to_string(project_root.join("package.json"))
  else {
    return Vec::new();
  };
  let Ok(package_json) = serde_json::from_str::<Value>(&content) else {
    return Vec::new();
  };
  let mut imports = Vec::new();
  for field in ["dependencies", "devDependencies"] {
    let Some(deps) = package_json.get(field).and_then(|d| d.as_object()) else {
      continue;
    };
    for (name, req) in deps {
      let Some(req) = req.as_str() else {
        continue;
      };
      let target = if req.starts_with("npm:") {
        req.to_string()
      } else if VersionReq::parse_from_npm(req).is_ok() {
        format!("npm:{name}@{req}")
      } else {
        continue;
      };
      if !imports.iter().any(|(n, _)| n == name) {
        imports.push((name.clone(), target));
      }
    }
  }
  imports
}

/// If `deno_json` references an external import map via `importMap`, read that
/// file and return its `imports` object.
fn read_referenced_import_map(
  project_root: &Path,
  deno_json: &Value,
) -> Option<serde_json::Map<String, Value>> {
  let rel = deno_json.get("importMap").and_then(|v| v.as_str())?;
  read_import_map_file(project_root, &project_root.join(rel))
}

/// The `imports` of the import map file at `map_path`. A relative target is
/// relative to the map file, which may live elsewhere (`"importMap":
/// "../import_map.json"`, `--import-map other/map.json`); it is rebased onto
/// the project root, as the generated `paths` take it (denext).
fn read_import_map_file(
  project_root: &Path,
  map_path: &Path,
) -> Option<serde_json::Map<String, Value>> {
  let content = std::fs::read_to_string(map_path).ok()?;
  let parsed: Value = serde_json::from_str(&content).ok()?;
  let mut imports =
    parsed.get("imports").and_then(|v| v.as_object()).cloned()?;
  let map_dir = map_path
    .parent()
    .and_then(|dir| pathdiff::diff_paths(dir, project_root));
  if let Some(map_dir) = map_dir.as_deref().and_then(|d| d.to_str()) {
    for value in imports.values_mut() {
      if let Some(target) = value.as_str()
        && (target.starts_with("./") || target.starts_with("../"))
      {
        *value = Value::String(rebase_relative_target(map_dir, target));
      }
    }
  }
  Some(imports)
}

/// Build tsconfig `paths` entries that map each workspace member's package name
/// (and subpath exports) to its local source files, so stock tooling resolves
/// e.g. `@std/assert` and `@std/assert/equals` to `../assert/mod.ts` and
/// `../assert/equals.ts` rather than a published copy. Paths are relative to
/// the generated `.deno/tsconfig.json`.
fn workspace_member_paths(
  project_root: &Path,
  deno_json: &Value,
) -> serde_json::Map<String, Value> {
  let mut paths = serde_json::Map::new();
  let Some(members) = deno_json.get("workspace").and_then(|w| w.as_array())
  else {
    return paths;
  };
  let member_patterns: Vec<&str> =
    members.iter().filter_map(|m| m.as_str()).collect();
  for member_rel in expand_member_patterns(project_root, &member_patterns) {
    let member_rel = member_rel.as_str();
    let Some(member_json) = read_deno_json(&project_root.join(member_rel))
      .ok()
      .flatten()
    else {
      continue;
    };
    let Some(name) = member_json.get("name").and_then(|n| n.as_str()) else {
      continue;
    };
    let Some(exports) = member_json.get("exports") else {
      continue;
    };
    let mut add = |sub: &str, file: &str| {
      let file = file.trim_start_matches("./");
      let target = format!("../{member_rel}/{file}");
      let key = if sub == "." {
        name.to_string()
      } else {
        format!("{name}/{}", sub.trim_start_matches("./"))
      };
      paths.insert(key, json!([target]));
    };
    match exports {
      // `"exports": "./mod.ts"`
      Value::String(file) => add(".", file),
      // `"exports": { ".": "./mod.ts", "./x": "./x.ts" }`
      Value::Object(map) => {
        for (sub, target) in map {
          if let Some(file) = target.as_str() {
            add(sub, file);
          }
        }
      }
      _ => {}
    }
  }
  paths
}

/// The `imports` of each workspace member's `deno.json`, a relative target
/// (`./lib/x.ts`) rebased onto the project root (`./member/lib/x.ts`) as the
/// root import map's targets are.
fn workspace_member_imports(
  project_root: &Path,
  deno_json: &Value,
) -> Vec<(String, Value)> {
  let Some(members) = deno_json.get("workspace").and_then(|w| w.as_array())
  else {
    return Vec::new();
  };
  let member_patterns: Vec<&str> =
    members.iter().filter_map(|m| m.as_str()).collect();
  let mut imports = Vec::new();
  for member_rel in expand_member_patterns(project_root, &member_patterns) {
    let Some(member_json) = read_deno_json(&project_root.join(&member_rel))
      .ok()
      .flatten()
    else {
      continue;
    };
    let Some(member_imports) =
      member_json.get("imports").and_then(|i| i.as_object())
    else {
      continue;
    };
    for (key, value) in member_imports {
      let value = match value.as_str() {
        Some(target)
          if target.starts_with("./") || target.starts_with("../") =>
        {
          Value::String(rebase_relative_target(&member_rel, target))
        }
        _ => value.clone(),
      };
      imports.push((key.clone(), value));
    }
  }
  imports
}

/// `target`, relative to the directory `dir_rel` (itself relative to the
/// project root), relative to the project root instead (`"pkg"`,
/// `"../lib/x.ts"` -> `"./lib/x.ts"`).
fn rebase_relative_target(dir_rel: &str, target: &str) -> String {
  let mut parts: Vec<&str> = dir_rel
    .split(['/', '\\'])
    .filter(|p| !p.is_empty() && *p != ".")
    .collect();
  let trailing_slash = target.ends_with('/');
  for part in target.split('/') {
    match part {
      "" | "." => {}
      ".." => {
        if parts.last().is_some_and(|last| *last != "..") {
          parts.pop();
        } else {
          parts.push("..");
        }
      }
      part => parts.push(part),
    }
  }
  let mut rebased = format!("./{}", parts.join("/"));
  if trailing_slash && !rebased.ends_with('/') {
    rebased.push('/');
  }
  rebased
}

/// Expand workspace member patterns into concrete member directories (relative
/// to the project root). A trailing `/*` (e.g. `./packages/*`) enumerates the
/// immediate subdirectories that contain a `deno.json(c)`; everything else is
/// treated as a literal member path. Deno's own workspace config supports this
/// glob form, and it's common in monorepos.
fn expand_member_patterns(
  project_root: &Path,
  patterns: &[&str],
) -> Vec<String> {
  let mut out = Vec::new();
  for pattern in patterns {
    let cleaned = pattern.trim_start_matches("./").trim_end_matches('/');
    if let Some(parent) = cleaned.strip_suffix("/*") {
      let Ok(entries) = std::fs::read_dir(project_root.join(parent)) else {
        continue;
      };
      for entry in entries.flatten() {
        if !entry.path().is_dir() {
          continue;
        }
        let dir = entry.path();
        if (dir.join("deno.json").exists() || dir.join("deno.jsonc").exists())
          && let Some(name) = entry.file_name().to_str()
        {
          out.push(format!("{parent}/{name}"));
        }
      }
    } else {
      out.push(cleaned.to_string());
    }
  }
  out.sort();
  out
}

/// Extract the bare package name from a `jsr:`/`npm:` specifier, dropping the
/// scheme and any version/subpath (`jsr:@fresh/core@^2` -> `@fresh/core`,
/// `npm:chalk@5` -> `chalk`, `npm:@scope/pkg@1` -> `@scope/pkg`).
fn scheme_package_name(spec: &str) -> Option<String> {
  let rest = spec
    .strip_prefix("jsr:")
    .or_else(|| spec.strip_prefix("npm:"))?;
  if let Some(scoped) = rest.strip_prefix('@') {
    let (scope, name_and_rest) = scoped.split_once('/')?;
    let name = name_and_rest.split('@').next()?;
    Some(format!("@{scope}/{name}"))
  } else {
    Some(rest.split('@').next()?.to_string())
  }
}

/// For each import-map alias whose target's package name matches a workspace
/// member, mirror that member's `paths` entries under the alias so imports
/// through the alias resolve to the local member's source rather than a
/// materialized registry copy.
fn add_member_alias_paths(
  member_paths: &mut serde_json::Map<String, Value>,
  alias_targets: &[(String, String)],
) {
  // Snapshot member entries so we can extend `member_paths` while iterating.
  let member_entries: Vec<(String, Value)> = member_paths
    .iter()
    .map(|(k, v)| (k.clone(), v.clone()))
    .collect();
  for (alias, target) in alias_targets {
    let Some(base) = scheme_package_name(target) else {
      continue;
    };
    let sub_prefix = format!("{base}/");
    for (member_key, member_val) in &member_entries {
      if member_key == &base {
        member_paths
          .entry(alias.clone())
          .or_insert_with(|| member_val.clone());
      } else if let Some(sub) = member_key.strip_prefix(&sub_prefix) {
        member_paths
          .entry(format!("{alias}/{sub}"))
          .or_insert_with(|| member_val.clone());
      }
    }
  }
}

/// Map each import (a specifier written in source, or an import-map alias)
/// whose target is a `file:` URL to that file for the generated `paths`: the
/// stock compiler resolves `file:///a/b.ts` as a bare package name and reports
/// TS2307. A prefix alias (`"lib/": "file:///a/lib/"`) maps its subpaths.
fn add_file_url_paths(
  paths: &mut serde_json::Map<String, Value>,
  imports: &serde_json::Map<String, Value>,
) {
  for (key, target) in imports {
    let Some(target) = target.as_str().filter(|t| t.starts_with("file:"))
    else {
      continue;
    };
    let Some(path) = Url::parse(target)
      .ok()
      .and_then(|url| url.to_file_path().ok())
    else {
      continue;
    };
    let path = path_for_typescript(&path);
    if let (Some(prefix), true) = (key.strip_suffix('/'), target.ends_with('/'))
    {
      paths.entry(format!("{prefix}/*")).or_insert_with(|| {
        json!([format!("{}/*", path.trim_end_matches('/'))])
      });
    } else {
      paths.entry(key.clone()).or_insert_with(|| json!([path]));
    }
  }
}

fn read_deno_json(project_root: &Path) -> Result<Option<Value>, AnyError> {
  let deno_json_path = project_root.join("deno.json");
  let deno_jsonc_path = project_root.join("deno.jsonc");

  if deno_json_path.exists() {
    read_config_file(&deno_json_path).map(Some)
  } else if deno_jsonc_path.exists() {
    read_config_file(&deno_jsonc_path).map(Some)
  } else {
    Ok(None)
  }
}

/// A config file's JSON, read as JSONC (as Deno reads a `deno.json` too).
fn read_config_file(path: &Path) -> Result<Value, AnyError> {
  let content = std::fs::read_to_string(path)?;
  let parsed: Option<Value> = jsonc_parser::parse_to_serde_value(
    &content,
    &jsonc_parser::ParseOptions::default(),
  )?;
  Ok(parsed.unwrap_or(json!({})))
}

/// Generate tsconfig.deno.json at the project root with paths mappings.
#[allow(
  clippy::too_many_arguments,
  reason = "threads the independent inputs needed to generate a tsconfig"
)]
fn generate_deno_tsconfig(
  project_root: &Path,
  deno_compiler_options: Option<&Value>,
  resolved_compiler_options: Option<&Value>,
  deno_imports: Option<&Value>,
  http_modules: &BTreeMap<Url, String>,
  member_paths: &serde_json::Map<String, Value>,
  jsr_packages_dir: &Path,
  npm_package_paths: &BTreeMap<String, PathBuf>,
  npm_project_references: &[String],
  node_types_root: Option<&str>,
  excludes: &[String],
  has_local_wasm: bool,
  manage_root_tsconfig: bool,
) -> Result<(), AnyError> {
  let generated = crate::tsc::tsconfig_gen::generate_tsconfig(
    project_root,
    deno_compiler_options,
    resolved_compiler_options,
    deno_imports,
    // Command-line roots scope graph/dependency discovery only. Keep the
    // generated project open so bundlers can consume its resolver mappings for
    // any entrypoint they are asked to bundle.
    &[],
    http_modules,
    member_paths,
    jsr_packages_dir,
    npm_package_paths,
    npm_project_references,
    node_types_root,
    excludes,
    has_local_wasm,
    manage_root_tsconfig,
  )
  .map_err(|e| anyhow!("Failed to generate tsconfig: {e}"))?;

  log::debug!("Generated {}", generated.tsconfig_path.display());

  Ok(())
}

/// Install jsr: compatibility packages by downloading from npm.jsr.io.
///
/// Uses `HttpClient::download` directly (not `CliFileFetcher`) because we
/// don't want each registry/tarball URL surfacing as a user-visible
/// "Download …" line in `deno install` output — those are an
/// implementation detail of the stock-tsc compatibility setup. The
/// fetcher's caching and redirect handling are unnecessary for npm.jsr.io.
async fn install_jsr_packages(
  jsr_packages_dir: &Path,
  deno_imports: Option<&Value>,
  member_paths: &serde_json::Map<String, Value>,
  http_client: &HttpClient,
) -> Result<Vec<InstalledJsrPackage>, AnyError> {
  let mut installed = Vec::new();
  let imports = match deno_imports.and_then(|v| v.as_object()) {
    Some(imports) => imports,
    None => return Ok(installed),
  };

  for (_alias, target) in imports {
    let target_str = match target.as_str() {
      Some(s) if s.starts_with("jsr:") => s,
      _ => continue,
    };

    let Some((scope, name, req_version)) =
      crate::tsc::tsconfig_gen::parse_jsr_specifier(target_str)
    else {
      continue;
    };

    // denext: a package a workspace member provides (`"@scope/pkg":
    // "jsr:@scope/pkg@1.2.3"` with that member at 1.2.3, as Deno resolves it)
    // maps to the member's sources (`member_paths`), which win in the generated
    // `paths`; the registry copy is never used, and may not exist yet.
    let member_name = format!("@{}/{}", scope.trim_start_matches('@'), name);
    if member_paths.contains_key(&member_name) {
      continue;
    }

    let npm_name = format!("{}__{}", scope.trim_start_matches('@'), name);
    let pkg_dir = jsr_packages_dir.join(&npm_name);
    if is_materialized_package(&pkg_dir) {
      continue;
    }

    let registry_name = format!("@jsr/{npm_name}");
    // Use the same env var as the rest of Deno (`JSR_NPM_URL`, see
    // libs/npmrc) for the npm-compat jsr registry, so a custom/test registry is
    // honored. The previous `DENO_NPM_JSR_REGISTRY` was read by nothing else, so
    // tests (which set `JSR_NPM_URL` to the mock registry) fell through to the
    // real npm.jsr.io and the fetch failed -> the package was never installed.
    let npm_jsr_registry = std::env::var("JSR_NPM_URL")
      .unwrap_or_else(|_| "https://npm.jsr.io".to_string());
    let metadata_url = format!(
      "{}/{}",
      npm_jsr_registry.trim_end_matches('/'),
      registry_name.replace('/', "%2f")
    );
    let metadata_url = match Url::parse(&metadata_url) {
      Ok(u) => u,
      Err(e) => {
        log::debug!("Invalid jsr registry URL {metadata_url}: {e}");
        continue;
      }
    };

    log::debug!("Installing {} from {}", registry_name, npm_jsr_registry);

    let metadata_bytes = match http_client.download(metadata_url).await {
      Ok(b) => b,
      Err(e) => {
        log::debug!("Failed to fetch metadata for {registry_name}: {e}");
        continue;
      }
    };

    let metadata: Value =
      serde_json::from_slice(&metadata_bytes).map_err(|e| {
        anyhow!("Failed to parse metadata for {registry_name}: {e}")
      })?;

    // An unresolvable version leaves this package unmapped (the compiler then
    // reports its imports) rather than failing the whole config generation,
    // like the other per-package failures here.
    let resolved_version = match resolve_jsr_version(
      &metadata,
      req_version.as_deref(),
      &registry_name,
    ) {
      Ok(version) => version,
      Err(e) => {
        log::debug!("{e}");
        continue;
      }
    };

    let tarball_url = metadata
      .get("versions")
      .and_then(|vs| vs.get(&resolved_version))
      .and_then(|v| v.get("dist"))
      .and_then(|d| d.get("tarball"))
      .and_then(|t| t.as_str())
      .ok_or_else(|| {
        anyhow!("No tarball URL for {registry_name}@{resolved_version}")
      })?;
    let tarball_url = match Url::parse(tarball_url) {
      Ok(u) => u,
      Err(e) => {
        log::debug!(
          "Invalid tarball URL {tarball_url} for {registry_name}: {e}"
        );
        continue;
      }
    };

    let tarball_bytes = match http_client.download(tarball_url).await {
      Ok(b) => b,
      Err(e) => {
        log::debug!("Failed to download {registry_name}: {e}");
        continue;
      }
    };

    if let Err(e) = extract_tarball_gz_atomic(&tarball_bytes, &pkg_dir, None) {
      log::debug!("Failed to extract {registry_name}: {e}");
      continue;
    }

    installed.push(InstalledJsrPackage {
      name: registry_name,
      version: resolved_version,
    });
  }

  Ok(installed)
}

/// Whether `dir` holds a fully materialized package rather than a directory
/// that some other process is still extracting into (or left half-extracted
/// after being killed).
///
/// `package.json` is the last thing [`extract_tarball_gz_atomic`] publishes -
/// it only ever appears via the atomic rename of a complete staging dir - so
/// its presence is the marker that the tree is usable. A bare `exists()` check
/// on the destination is not: with several `deno check` processes sharing a
/// cache, one can observe a directory the other has only started filling in,
/// skip the download, and then fail type checking with
/// `TS2688 Cannot find type definition file for 'node'`.
/// npm and npm.jsr.io package tarballs are required to contain a top-level
/// `package.json`; this marker relies on that invariant.
fn is_materialized_package(dir: &Path) -> bool {
  dir.join("package.json").exists()
}

/// Whether an existing `dest` tree can stand in for the one being extracted:
/// complete, and at the version we asked for when we pinned one.
fn published_copy_is_current(
  dest: &Path,
  expected_version: Option<&str>,
) -> bool {
  match expected_version {
    Some(version) => is_materialized_package_at_version(dest, version),
    None => is_materialized_package(dest),
  }
}

fn is_materialized_package_at_version(dir: &Path, version: &str) -> bool {
  let Ok(contents) = std::fs::read_to_string(dir.join("package.json")) else {
    return false;
  };
  let Ok(manifest) = serde_json::from_str::<Value>(&contents) else {
    return false;
  };
  manifest.get("version").and_then(|v| v.as_str()) == Some(version)
}

fn cleanup_stale_staging_dirs(
  parent: &Path,
  staging_prefix: &str,
  stale_before: std::time::SystemTime,
) {
  let Ok(entries) = std::fs::read_dir(parent) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if !entry
      .file_name()
      .to_string_lossy()
      .starts_with(staging_prefix)
      || !entry.file_type().is_ok_and(|file_type| file_type.is_dir())
      || !entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified < stale_before)
    {
      continue;
    }
    let _ = std::fs::remove_dir_all(path);
  }
}

/// Extract a gzipped npm-style tarball so that `dest` is never observable in a
/// half-extracted state (see `publish_dir_atomic`).
fn extract_tarball_gz_atomic(
  gz_bytes: &[u8],
  dest: &Path,
  expected_version: Option<&str>,
) -> Result<(), AnyError> {
  publish_dir_atomic(
    dest,
    |dest| published_copy_is_current(dest, expected_version),
    |dir| extract_tarball_gz(gz_bytes, dir),
  )
}

/// Fill a sibling staging directory of `dest` on the same filesystem with
/// `fill`, then `rename` it into place, so `dest` is never observable
/// half-written.
///
/// Concurrent `deno check` invocations sharing a project race to materialize
/// the same package. The rename makes each one either publish a complete tree
/// or lose harmlessly to a winner that already did (`is_current`).
fn publish_dir_atomic(
  dest: &Path,
  is_current: impl Fn(&Path) -> bool,
  fill: impl FnOnce(&Path) -> Result<(), AnyError>,
) -> Result<(), AnyError> {
  let parent = dest.parent().ok_or_else(|| {
    anyhow!("Invalid extract destination: {}", dest.display())
  })?;
  std::fs::create_dir_all(parent)?;

  // Stage as a sibling so the rename stays within one filesystem. The pid plus
  // a process-local counter keeps concurrent extractions - across processes and
  // within one - from sharing a staging dir.
  static STAGING_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
  let name = dest.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
    anyhow!("Invalid extract destination: {}", dest.display())
  })?;
  // The leading dot is required because this parent is passed to TypeScript as
  // a typeRoots directory, and TypeScript ignores dot-prefixed entries when it
  // enumerates type packages.
  let staging_prefix = format!(".{name}.tmp-");
  // Reclaim staging dirs left by killed processes. Keep recent dirs because
  // they may belong to another extraction currently racing with this one.
  if let Some(stale_before) = std::time::SystemTime::now()
    .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
  {
    cleanup_stale_staging_dirs(parent, &staging_prefix, stale_before);
  }
  let tmp_dir = parent.join(format!(
    "{}{}-{}",
    staging_prefix,
    std::process::id(),
    STAGING_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
  ));
  // A staging dir left over from a previous crashed run would make extraction
  // append to a foreign tree.
  let _ = std::fs::remove_dir_all(&tmp_dir);

  if let Err(e) = fill(&tmp_dir) {
    let _ = std::fs::remove_dir_all(&tmp_dir);
    return Err(e);
  }

  match std::fs::rename(&tmp_dir, dest) {
    Ok(()) => Ok(()),
    Err(rename_err) => {
      // `dest` is in the way. On Windows a rename onto *any* existing directory
      // fails; on Unix only onto a non-empty one - so this is the normal path
      // for a lost race, not just an error path.
      if is_current(dest) {
        // Another process published a complete copy first. Its tree is as good
        // as ours, so keep it and drop the staging dir.
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Ok(());
      }
      // `dest` is stale: half-extracted by a killed process, written
      // non-atomically by an older Deno, or a complete copy of a version we no
      // longer want (an older pin, or whatever `latest` was when Deno still
      // followed it). Clearing it is what heals a cache that is already
      // poisoned - otherwise every later run skips the download and keeps type
      // checking against the wrong tree.
      let _ = std::fs::remove_dir_all(dest);
      match std::fs::rename(&tmp_dir, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
          let _ = std::fs::remove_dir_all(&tmp_dir);
          // A third process may have published `dest` in between.
          if is_current(dest) {
            return Ok(());
          }
          Err(anyhow!(
            "Failed to move extracted package into {}: {e} (initial: {rename_err})",
            dest.display()
          ))
        }
      }
    }
  }
}

/// Extract a gzipped npm-style tarball into `dest`, stripping the leading
/// `package/` directory the way `tar --strip-components=1` does. Replaces
/// the previous `tar` shell-out so the install works the same on Linux,
/// macOS (BSD tar), Windows, and minimal containers without `tar`/`curl`.
fn extract_tarball_gz(gz_bytes: &[u8], dest: &Path) -> Result<(), AnyError> {
  std::fs::create_dir_all(dest)?;
  let mut archive = tar::Archive::new(GzDecoder::new(gz_bytes));
  for entry in archive.entries()? {
    let mut entry = entry?;
    // Never materialize link entries. A symlink entry pointing outside `dest`,
    // followed by a regular-file entry nested "under" it, would let `unpack`
    // (via `create_dir_all` following the symlink) write outside `dest`. We only
    // need regular files/dirs for type extraction, so skip links entirely.
    let entry_type = entry.header().entry_type();
    if entry_type.is_symlink() || entry_type.is_hard_link() {
      continue;
    }
    let path = entry.path()?.into_owned();
    // Skip the leading "package/" (or whatever the single root dir is named).
    let stripped: PathBuf = path.components().skip(1).collect();
    if stripped.as_os_str().is_empty() {
      continue;
    }
    if stripped.is_absolute()
      || stripped.components().any(|c| {
        matches!(
          c,
          std::path::Component::ParentDir | std::path::Component::RootDir
        )
      })
    {
      return Err(anyhow!(
        "Refusing to extract tar entry outside dest: {}",
        path.display()
      ));
    }
    let out_path = dest.join(stripped);
    // tar entries aren't guaranteed to list a directory before the files under
    // it, and `Entry::unpack` won't create missing parents — so ensure the
    // parent exists first (otherwise nested files like `_dist/mod.d.ts` fail).
    if let Some(parent) = out_path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    entry.unpack(&out_path)?;
  }
  Ok(())
}

/// A resolved entry for the tsconfig `paths` table: maps a user-facing URL
/// (whatever appears in source as `import "..."`) to the local mirror file
/// path (relative to `.deno/`) that tsc should resolve it to.
pub type HttpModulePaths = BTreeMap<Url, String>;

/// Materialize local (`file:`) `.wasm` modules' generated typed declarations
/// under `.deno/wasm/`, mirroring their path relative to the project root and
/// renaming `foo.wasm` -> `foo.d.wasm.ts` so stock tsc resolves the relative
/// `import "./foo.wasm"` via `allowArbitraryExtensions` + a `rootDirs` overlay
/// (added by `build_tsconfig`). `dts_modules` pairs each wasm's `file:` URL with
/// its already-generated declaration text (from deno_graph). Returns whether any
/// wasm module was written (so the caller can enable the tsconfig options only
/// when needed).
///
/// Unlike remote wasm (a non-relative specifier keyed into tsconfig `paths` by
/// `install_http_modules`), a relative `./foo.wasm` specifier is never matched
/// by `paths`; the `rootDirs` overlay is what makes tsc find the mirror.
fn install_local_wasm_modules(
  project_root: &Path,
  dts_modules: &[(Url, String)],
) -> Result<bool, AnyError> {
  if dts_modules.is_empty() {
    return Ok(false);
  }
  let wasm_root = project_root.join(".deno").join("wasm");
  let mut wrote_any = false;
  for (url, dts) in dts_modules {
    let Ok(path) = url.to_file_path() else {
      continue;
    };
    // Mirror the wasm file's location relative to the project root so the
    // `rootDirs` overlay resolves it to the same relative specifier the source
    // used. A wasm module outside the project root has no stable relative
    // location under `.deno/wasm/`, so skip it (rare; tsc keeps its TS2307).
    let Ok(rel) = path.strip_prefix(project_root) else {
      continue;
    };
    // `foo.wasm` -> `foo.d.wasm.ts` (the extension `allowArbitraryExtensions`
    // looks for when resolving `import "./foo.wasm"`).
    let file_name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let stem = file_name.strip_suffix(".wasm").unwrap_or(file_name);
    let dts_name = format!("{stem}.d.wasm.ts");
    let dest = wasm_root.join(rel).with_file_name(dts_name);
    if let Some(parent) = dest.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, dts.as_bytes())?;
    wrote_any = true;
  }
  Ok(wrote_any)
}

/// Materialize http(s): modules referenced from `deno.json` `imports` (and
/// their transitive remote/relative imports) into `.deno/remote/<host><path>`.
///
/// Uses `CliFileFetcher`, which transparently reuses `DENO_DIR` cache, follows
/// redirects, and exposes response headers. When a module advertises types via
/// the `X-TypeScript-Types` header (esm.sh and friends), the types URL is
/// fetched too and the paths entry for the original URL is pointed at the
/// types file so tsc gets real declarations.
///
/// Module specifier discovery within source still uses a regex scanner — this
/// is acknowledged tech debt; a deno_graph walk is the production answer.
///
/// Returns a map from user-facing URL to the local mirror path (relative to
/// `.deno/`), suitable for direct use as tsconfig `paths` values.
pub async fn install_http_modules(
  project_root: &Path,
  deno_imports: Option<&Value>,
  file_fetcher: &CliFileFetcher,
  permissions: &PermissionsContainer,
  type_check_remote: bool,
) -> Result<HttpModulePaths, AnyError> {
  let mut paths: HttpModulePaths = BTreeMap::new();
  let mut queue: VecDeque<Url> = VecDeque::new();

  let Some(imports) = deno_imports.and_then(|v| v.as_object()) else {
    return Ok(paths);
  };

  for (_alias, target) in imports {
    let Some(s) = target.as_str() else {
      continue;
    };
    if !(s.starts_with("http://") || s.starts_with("https://")) {
      continue;
    }
    if let Ok(url) = Url::parse(s) {
      queue.push_back(url);
    }
  }

  if queue.is_empty() {
    return Ok(paths);
  }

  let remote_root = project_root.join(".deno").join("remote");

  while let Some(requested_url) = queue.pop_front() {
    if paths.contains_key(&requested_url) {
      continue;
    }
    // A trailing-slash URL is a directory/prefix specifier, not a module; the
    // files under it are discovered and mirrored per-subpath. Skip silently
    // (fetching the bare prefix just 404s).
    if requested_url.path().ends_with('/') {
      continue;
    }

    // Fetch (cached) source + headers via the standard file fetcher, gated
    // by --allow-import the same way runtime http imports are. Follows
    // redirects; `file.url` is the canonical post-redirect URL.
    let file = match file_fetcher.fetch(&requested_url, permissions).await {
      Ok(f) => f,
      Err(e) => {
        log::warn!(
          "Skipping {requested_url}: {e} (try running `deno install --allow-import` to grant access)"
        );
        continue;
      }
    };
    let final_url = file.url.clone();

    // If the module advertises a separate types file (X-TypeScript-Types,
    // common on esm.sh), fetch *that* and use it as the source-of-truth for
    // both mirroring and import scanning. Otherwise use the module itself.
    let types_url = file
      .maybe_headers
      .as_ref()
      .and_then(|h| {
        h.get("x-typescript-types")
          .or_else(|| h.get("types"))
          .or_else(|| h.get("typescript-types"))
      })
      .and_then(|v| Url::parse(v).ok().or_else(|| final_url.join(v).ok()));

    let (effective_url, effective_bytes) = if let Some(t) = types_url.as_ref() {
      match file_fetcher.fetch(t, permissions).await {
        Ok(f) => (f.url.clone(), f.source.clone()),
        Err(e) => {
          log::debug!(
            "X-TypeScript-Types fetch failed for {t} ({e}), falling back to source"
          );
          (final_url.clone(), file.source.clone())
        }
      }
    } else {
      (final_url.clone(), file.source.clone())
    };

    // wasm modules can't be type-checked as binaries: mirror the generated
    // TypeScript (typed exports + the wasm's own imports) instead, written as a
    // checkable `.mts` so tsc surfaces the exports' types to importers and the
    // wasm's own import errors. Always type-checked (never `@ts-nocheck`).
    let is_wasm = final_url.path().ends_with(".wasm");
    let (mirror_url, mirror_bytes): (Url, std::borrow::Cow<[u8]>) = if is_wasm {
      match deno_graph::source::wasm::wasm_module_to_dts(
        effective_bytes.as_ref(),
      ) {
        Ok(dts) => {
          let mut u = effective_url.clone();
          u.set_path(&format!("{}.mts", u.path()));
          (u, std::borrow::Cow::Owned(dts.into_bytes()))
        }
        Err(e) => {
          log::debug!("wasm dts generation failed for {effective_url}: {e}");
          (
            effective_url.clone(),
            std::borrow::Cow::Borrowed(effective_bytes.as_ref()),
          )
        }
      }
    } else {
      (
        effective_url.clone(),
        std::borrow::Cow::Borrowed(effective_bytes.as_ref()),
      )
    };

    // Mirror the chosen content. We deliberately don't mirror both the JS
    // and the .d.ts: their URL paths often collide on disk (e.g.
    // `cowsay@1.6.0` as file vs `cowsay@1.6.0/index.d.ts` under a dir).
    let local = match write_mirror(
      &remote_root,
      &mirror_url,
      &mirror_bytes,
      type_check_remote || is_wasm,
    ) {
      Ok(p) => p,
      Err(e) => {
        log::debug!("Failed to mirror {effective_url} ({e})");
        continue;
      }
    };
    // Emit paths entries for every URL form the user (or transitive imports)
    // might reference for this module — all pointing at the same mirror.
    paths.insert(requested_url.clone(), local.clone());
    if final_url != requested_url {
      paths.insert(final_url.clone(), local.clone());
    }
    if effective_url != final_url {
      paths.insert(effective_url.clone(), local.clone());
    }

    // Scan the mirrored (type-bearing) source for transitive imports. For wasm
    // this is the generated `.mts` (which carries the wasm's imports), not the
    // binary - so scan with `mirror_url` (`.wasm.mts`), whose media type passes
    // the analyzer's gate; `effective_url` (`.wasm`) would be rejected as
    // `MediaType::Wasm` and the imports would never be walked.
    let scan_source = String::from_utf8_lossy(&mirror_bytes).into_owned();
    for spec in scan_import_specifiers(&mirror_url, &scan_source) {
      let resolved =
        if spec.starts_with("http://") || spec.starts_with("https://") {
          Url::parse(&spec).ok()
        } else if spec.starts_with("./")
          || spec.starts_with("../")
          || spec.starts_with('/')
        {
          // `./`, `../`, and same-host absolute paths (e.g. `/src/foo.ts`,
          // common on deno.land/x and esm.sh CDNs) all resolve against the
          // effective URL via Url::join.
          effective_url.join(&spec).ok()
        } else {
          None
        };
      if let Some(child) = resolved
        && (child.scheme() == "http" || child.scheme() == "https")
        && !paths.contains_key(&child)
      {
        queue.push_back(child);
      }
    }
  }

  Ok(paths)
}

/// Write `bytes` to the local mirror file for `url`, returning the path
/// relative to `.deno/` (suitable for tsconfig `paths`).
fn write_mirror(
  remote_root: &Path,
  url: &Url,
  bytes: &[u8],
  type_check_remote: bool,
) -> Result<String, AnyError> {
  let local_path = url_to_local_path(remote_root, url)
    .ok_or_else(|| anyhow!("URL has no resolvable mirror path: {url}"))?;
  if let Some(parent) = local_path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  // Mark mirrored script modules `// @ts-nocheck`. Remote dependencies are not
  // the user's code and Deno itself doesn't re-type-check them; without this,
  // stock tsc/tsgo would report every type error inside the dependency (which
  // dwarfs the project's own diagnostics). `@ts-nocheck` suppresses errors in
  // the file while keeping its exported and ambient types available to
  // importers, and triple-slash directives (which may follow a leading comment)
  // still apply. Skipped under `deno check --all`, which does type-check remote
  // modules.
  let is_script = matches!(
    local_path.extension().and_then(|e| e.to_str()),
    Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
  );
  if is_script && !type_check_remote {
    let mut content = Vec::with_capacity(bytes.len() + 16);
    content.extend_from_slice(b"// @ts-nocheck\n");
    content.extend_from_slice(bytes);
    std::fs::write(&local_path, &content)?;
  } else {
    std::fs::write(&local_path, bytes)?;
  }
  url_to_tsconfig_path(url)
    .ok_or_else(|| anyhow!("URL has no resolvable mirror path: {url}"))
}

/// Compute the mirror location for `url` relative to the remote root, e.g.
/// `https://example.com/x/foo.ts` → `example.com/x/foo.ts`. Includes a
/// non-default port and a hash of the query string so URLs that share a path
/// but differ by port or query (common on CDNs like esm.sh) don't collide on
/// disk or in the generated `paths`. The query hash goes into the last
/// segment's stem (before its first `.`), keeping the file in the same
/// directory and preserving its extension so relative imports still resolve.
/// Returns None for directory-like URLs (path ends in `/`), since we can't
/// infer an index filename without server cooperation.
fn url_to_mirror_rel(url: &Url) -> Option<String> {
  let host = url.host_str()?;
  let path = url.path();
  if path.ends_with('/') || path.is_empty() {
    return None;
  }
  let mut host_seg = host.to_string();
  if let Some(port) = url.port() {
    host_seg.push_str(&format!("__{port}"));
  }
  let rel = path.trim_start_matches('/');
  let rel = match url.query() {
    Some(query) => {
      let hash = short_hash(query);
      match rel.rsplit_once('/') {
        Some((dir, file)) => {
          format!("{dir}/{}", inject_stem_suffix(file, &hash))
        }
        None => inject_stem_suffix(rel, &hash),
      }
    }
    None => rel.to_string(),
  };
  Some(format!("{host_seg}/{rel}"))
}

/// Insert `suffix` into a filename before its first `.` (`mod.d.ts` ->
/// `mod.<suffix>.d.ts`), or append it when there's no extension.
fn inject_stem_suffix(file: &str, suffix: &str) -> String {
  match file.split_once('.') {
    Some((stem, ext)) => format!("{stem}.{suffix}.{ext}"),
    None => format!("{file}.{suffix}"),
  }
}

/// Deterministic short hex hash, used to disambiguate mirror paths by query.
fn short_hash(s: &str) -> String {
  use std::hash::Hash;
  use std::hash::Hasher;
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  s.hash(&mut hasher);
  format!("{:08x}", hasher.finish() as u32)
}

/// Compute the `paths`-relative mirror location for `url`, e.g.
/// `https://example.com/x/foo.ts` → `./remote/example.com/x/foo.ts`.
fn url_to_tsconfig_path(url: &Url) -> Option<String> {
  Some(format!("./remote/{}", url_to_mirror_rel(url)?))
}

/// Map a URL to its mirror file path under `<remote_root>/<mirror-rel>`.
fn url_to_local_path(remote_root: &Path, url: &Url) -> Option<PathBuf> {
  Some(remote_root.join(url_to_mirror_rel(url)?))
}

/// Extract module specifier string literals from a JS/TS source via
/// `deno_ast` + `deno_graph` analysis. Returns the raw specifier strings for
/// static imports/exports and string-literal dynamic imports. Template/expr
/// dynamic imports are skipped — they aren't statically analyzable.
///
/// Returns an empty vec if the source can't be parsed; the caller will simply
/// stop walking that branch.
fn scan_import_specifiers(specifier: &Url, source: &str) -> Vec<String> {
  use deno_ast::MediaType;
  use deno_graph::analysis::DependencyDescriptor;
  use deno_graph::analysis::DynamicArgument;

  let media_type = MediaType::from_specifier(specifier);
  if !matches!(
    media_type,
    MediaType::JavaScript
      | MediaType::Jsx
      | MediaType::Mjs
      | MediaType::Cjs
      | MediaType::TypeScript
      | MediaType::Mts
      | MediaType::Cts
      | MediaType::Dts
      | MediaType::Dmts
      | MediaType::Dcts
      | MediaType::Tsx
  ) {
    return Vec::new();
  }

  let parsed = match deno_ast::parse_module(deno_ast::ParseParams {
    specifier: specifier.clone(),
    text: source.into(),
    media_type,
    capture_tokens: false,
    maybe_syntax: None,
    scope_analysis: false,
  }) {
    Ok(p) => p,
    Err(e) => {
      log::debug!("Failed to parse {specifier} for import scan: {e}");
      return Vec::new();
    }
  };

  let module_info = deno_graph::ast::ParserModuleAnalyzer::module_info(&parsed);
  let mut out = Vec::new();
  for dep in &module_info.dependencies {
    match dep {
      DependencyDescriptor::Static(d) => out.push(d.specifier.to_string()),
      DependencyDescriptor::Dynamic(d) => {
        if let DynamicArgument::String(s) = &d.argument {
          out.push(s.clone());
        }
      }
    }
  }
  out
}

fn resolve_jsr_version(
  metadata: &Value,
  req_version: Option<&str>,
  registry_name: &str,
) -> Result<String, AnyError> {
  match req_version {
    None => metadata
      .get("dist-tags")
      .and_then(|dt| dt.get("latest"))
      .and_then(|v| v.as_str())
      .map(|s| s.to_string())
      .ok_or_else(|| anyhow!("No latest version for {registry_name}")),
    Some(req_str) => {
      if let Ok(exact) = Version::parse_standard(req_str)
        && metadata
          .get("versions")
          .and_then(|vs| vs.get(exact.to_string()))
          .is_some()
      {
        return Ok(exact.to_string());
      }

      let version_req = VersionReq::parse_from_npm(req_str)
        .map_err(|e| anyhow!("Invalid version req '{req_str}': {e}"))?;

      let versions = metadata
        .get("versions")
        .and_then(|vs| vs.as_object())
        .ok_or_else(|| anyhow!("No versions for {registry_name}"))?;

      let mut best: Option<Version> = None;
      for key in versions.keys() {
        if let Ok(v) = Version::parse_standard(key)
          && version_req.matches(&v)
          && best.as_ref().is_none_or(|b| v > *b)
        {
          best = Some(v);
        }
      }

      best.map(|v| v.to_string()).ok_or_else(|| {
        anyhow!("No version matching '{req_str}' for {registry_name}")
      })
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_scheme_package_name() {
    assert_eq!(
      scheme_package_name("jsr:@fresh/core@^2.0.0").as_deref(),
      Some("@fresh/core")
    );
    assert_eq!(scheme_package_name("npm:chalk@5").as_deref(), Some("chalk"));
    assert_eq!(
      scheme_package_name("npm:@scope/pkg@1.2.3").as_deref(),
      Some("@scope/pkg")
    );
    assert_eq!(scheme_package_name("npm:chalk").as_deref(), Some("chalk"));
    assert_eq!(scheme_package_name("./local.ts"), None);
    assert_eq!(scheme_package_name("https://example.com/x.ts"), None);
  }

  #[test]
  fn test_package_json_dependency_imports() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{
        "dependencies": { "picocolors": "1", "alias": "npm:chalk@5" },
        "devDependencies": {
          "picocolors": "2",
          "local": "file:../local",
          "ws": "workspace:*"
        }
      }"#,
    )
    .unwrap();
    let mut imports = package_json_dependency_imports(dir.path());
    imports.sort();
    assert_eq!(
      imports,
      vec![
        ("alias".to_string(), "npm:chalk@5".to_string()),
        ("picocolors".to_string(), "npm:picocolors@1".to_string()),
      ]
    );
  }

  #[test]
  fn test_write_embedded_types_node() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("@types/node");
    // A copy an earlier Deno downloaded from npm at the pinned version (its
    // `undici-types` lived beside it): replaced by the built-in one.
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
      dest.join("package.json"),
      format!(r#"{{"name":"@types/node","version":"{TYPES_NODE_VERSION}"}}"#),
    )
    .unwrap();
    std::fs::write(dest.join("index.d.ts"), "downloaded").unwrap();

    write_embedded_types_node(&dest).unwrap();
    assert!(is_embedded_types_node(&dest));
    assert!(!dest.join("index.d.ts").exists());
    let index = std::fs::read_to_string(dest.join("index.d.cts")).unwrap();
    assert!(index.contains("/// <reference path=\"./globals.d.cts\" />"));
    assert!(dest.join("fs/promises.d.cts").exists());
    assert!(dest.join("undici/index.d.ts").exists());
    let manifest: Value = serde_json::from_str(
      &std::fs::read_to_string(dest.join("package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["types"], json!("index.d.cts"));

    // A complete built-in copy is kept as it is.
    std::fs::write(dest.join("marker"), "").unwrap();
    write_embedded_types_node(&dest).unwrap();
    assert!(dest.join("marker").exists());
    let leftovers = std::fs::read_dir(dir.path().join("@types"))
      .unwrap()
      .flatten()
      .map(|e| e.file_name().to_string_lossy().into_owned())
      .filter(|n| n != "node")
      .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "leftover staging dirs: {leftovers:?}");
  }

  #[test]
  fn npm_compat_dir_is_outside_the_project() {
    let deno_dir = tempfile::tempdir().unwrap();
    let parent = tempfile::tempdir().unwrap();
    let a = parent.path().join("my app");
    let b = parent.path().join("other");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let dir_a = npm_compat_dir(Some(deno_dir.path()), &a);
    let dir_b = npm_compat_dir(Some(deno_dir.path()), &b);
    assert!(dir_a.starts_with(deno_dir.path().join("npm_compat")));
    assert!(!dir_a.starts_with(&a));
    let name = dir_a.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("my_app-"), "{name}");
    assert_eq!(name.len(), "my_app-".len() + 16, "{name}");
    // One folder per project, the same one every time.
    assert_ne!(dir_a, dir_b);
    assert_eq!(dir_a, npm_compat_dir(Some(deno_dir.path()), &a));
    // No DENO_DIR: the old place.
    assert_eq!(npm_compat_dir(None, &a), a.join(".deno").join("npm-compat"));
  }

  #[test]
  fn test_ensure_types_node_uses_the_built_in_copy_offline() {
    let dir = tempfile::tempdir().unwrap();
    let compat = tempfile::tempdir().unwrap();
    let setup = ensure_types_node(dir.path(), compat.path(), true);
    assert_eq!(
      setup.type_root,
      Some(path_for_typescript(&compat.path().join("@types")))
    );
    let node_dir = compat.path().join("@types/node");
    assert!(is_embedded_types_node(&node_dir));
    assert_eq!(setup.undici_types_dir, Some(node_dir.join("undici")));
    // Nothing is written into the project (DENEXT.md item 190).
    assert!(!dir.path().join(".deno").exists());

    // A project's own `@types/node` under `node_modules` wins.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("node_modules/@types/node"))
      .unwrap();
    let setup = ensure_types_node(project.path(), compat.path(), false);
    assert_eq!(setup.type_root.as_deref(), Some("../node_modules/@types"));
    assert!(!project.path().join(".deno").exists());
  }

  #[test]
  fn test_rebase_relative_target() {
    assert_eq!(rebase_relative_target("foo", "./mod.ts"), "./foo/mod.ts");
    assert_eq!(
      rebase_relative_target("pkgs/a", "../b/x.ts"),
      "./pkgs/b/x.ts"
    );
    assert_eq!(rebase_relative_target("a", "../../x.ts"), "./../x.ts");
    assert_eq!(rebase_relative_target("a", "./lib/"), "./a/lib/");
    assert_eq!(rebase_relative_target("", "./x.ts"), "./x.ts");
    assert_eq!(rebase_relative_target(".", "./x/"), "./x/");
    assert_eq!(rebase_relative_target("..", "./shared/"), "./../shared/");
  }

  #[test]
  fn test_read_import_map_file_from_a_flag_rebases_onto_the_root() {
    // `--import-map compile/map.json` with the project rooted at `app/`.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(dir.path().join("compile")).unwrap();
    let map_path = dir.path().join("compile/map.json");
    std::fs::write(
      &map_path,
      r#"{ "imports": { "hello": "../run/hello.js", "lib/": "./lib/" } }"#,
    )
    .unwrap();
    let imports = read_import_map_file(&root, &map_path).unwrap();
    assert_eq!(imports.get("hello"), Some(&json!("./../run/hello.js")));
    assert_eq!(imports.get("lib/"), Some(&json!("./../compile/lib/")));
    // In the project root itself, nothing to rebase.
    let imports = read_import_map_file(dir.path(), &map_path).unwrap();
    assert_eq!(imports.get("hello"), Some(&json!("./run/hello.js")));
  }

  #[test]
  fn test_read_referenced_import_map_rebases_onto_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sub");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
      dir.path().join("import_map.json"),
      r#"{ "imports": { "~/shared/": "./shared/", "x": "npm:x@1" } }"#,
    )
    .unwrap();
    let imports = read_referenced_import_map(
      &root,
      &json!({ "importMap": "../import_map.json" }),
    )
    .unwrap();
    assert_eq!(imports.get("~/shared/"), Some(&json!("./../shared/")));
    assert_eq!(imports.get("x"), Some(&json!("npm:x@1")));
  }

  #[test]
  fn test_workspace_member_imports() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("foo")).unwrap();
    std::fs::create_dir_all(root.join("packages/bar")).unwrap();
    std::fs::write(
      root.join("foo/deno.json"),
      r#"{ "imports": { "bar": "jsr:@foo/bar@1", "util": "./util.ts" } }"#,
    )
    .unwrap();
    std::fs::write(
      root.join("packages/bar/deno.jsonc"),
      "// a comment\n{ \"imports\": { \"lib/\": \"../lib/\" } }",
    )
    .unwrap();
    let root_json = json!({ "workspace": ["./foo", "./packages/*"] });
    let mut imports = workspace_member_imports(root, &root_json);
    imports.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
      imports,
      vec![
        ("bar".to_string(), json!("jsr:@foo/bar@1")),
        ("lib/".to_string(), json!("./packages/lib/")),
        ("util".to_string(), json!("./foo/util.ts")),
      ]
    );
  }

  #[test]
  fn test_add_file_url_paths() {
    let root = if cfg!(windows) {
      "file:///C:/proj"
    } else {
      "file:///proj"
    };
    let path = |rel: &str| {
      let url = Url::parse(&format!("{root}/{rel}")).unwrap();
      path_for_typescript(&url.to_file_path().unwrap())
    };
    let mut imports = serde_json::Map::new();
    // written in source: keyed to itself
    let direct = format!("{root}/lib/a%20b.ts");
    imports.insert(direct.clone(), json!(direct));
    // import-map aliases to a file and to a directory
    imports.insert("a".into(), json!(format!("{root}/lib/a.ts")));
    imports.insert("lib/".into(), json!(format!("{root}/lib/")));
    // other targets are left to their own generators
    imports.insert("chalk".into(), json!("npm:chalk@5"));
    imports.insert("rel".into(), json!("./rel.ts"));
    let mut paths = serde_json::Map::new();
    // an existing mapping wins
    paths.insert("a".into(), json!(["../member/a.ts"]));

    add_file_url_paths(&mut paths, &imports);

    assert_eq!(paths.get(&direct).unwrap(), &json!([path("lib/a b.ts")]));
    assert_eq!(paths.get("a").unwrap(), &json!(["../member/a.ts"]));
    assert_eq!(
      paths.get("lib/*").unwrap(),
      &json!([format!("{}/*", path("lib"))])
    );
    assert!(!paths.contains_key("chalk"));
    assert!(!paths.contains_key("rel"));
    assert_eq!(paths.len(), 3);
  }

  #[test]
  fn test_add_member_alias_paths() {
    // `packages/fresh` (name `@fresh/core`) is a workspace member; the root
    // import map aliases `fresh` -> `jsr:@fresh/core`.
    let mut member_paths = serde_json::Map::new();
    member_paths.insert(
      "@fresh/core".into(),
      json!(["../packages/fresh/src/mod.ts"]),
    );
    member_paths.insert(
      "@fresh/core/runtime".into(),
      json!(["../packages/fresh/src/runtime.ts"]),
    );

    let alias_targets = vec![
      ("fresh".to_string(), "jsr:@fresh/core@^2.0.0".to_string()),
      // an alias to a non-member package is left alone
      ("chalk".to_string(), "npm:chalk@5".to_string()),
    ];

    add_member_alias_paths(&mut member_paths, &alias_targets);

    // alias and its subpath now point at the local member source
    assert_eq!(
      member_paths.get("fresh").unwrap(),
      &json!(["../packages/fresh/src/mod.ts"])
    );
    assert_eq!(
      member_paths.get("fresh/runtime").unwrap(),
      &json!(["../packages/fresh/src/runtime.ts"])
    );
    // non-member alias produced no new entry
    assert!(!member_paths.contains_key("chalk"));
  }

  #[test]
  fn test_dependency_project_paths_resolves_subpath_exports() {
    let dir = tempfile::tempdir().unwrap();
    let pkg_dir = dir.path().join("dep");
    std::fs::create_dir_all(pkg_dir.join("dist/nested")).unwrap();
    // The resolved declaration must exist on disk (dependency_project_paths
    // verifies existence before mapping, falling back to source otherwise).
    std::fs::write(pkg_dir.join("dist/index.d.ts"), "").unwrap();
    std::fs::write(pkg_dir.join("dist/nested/subpath.d.ts"), "").unwrap();
    std::fs::write(
      pkg_dir.join("package.json"),
      serde_json::to_string(&json!({
        "name": "dep",
        "exports": {
          ".": { "types": "./dist/index.d.ts", "default": "./dist/index.js" },
          // Remaps `dep/subpath` to a non-literal declaration path: the naive
          // `dep/* -> folder/*` wildcard would mis-resolve this.
          "./subpath": {
            "types": "./dist/nested/subpath.d.ts",
            "default": "./dist/nested/subpath.js",
          },
        }
      }))
      .unwrap(),
    )
    .unwrap();

    let folder = path_for_typescript(&pkg_dir);
    let paths = dependency_project_paths("dep", &pkg_dir);

    // Bare alias still maps to the folder (tsc reads its package.json).
    assert_eq!(paths.get("dep"), Some(&json!([folder])));
    // The subpath maps to the exact declaration, not `folder/subpath`.
    assert_eq!(
      paths.get("dep/subpath"),
      Some(&json!([format!("{folder}/dist/nested/subpath.d.ts")]))
    );
    // A package with an exports map does NOT get the literal wildcard fallback.
    assert!(!paths.contains_key("dep/*"));
  }

  #[test]
  fn test_dependency_project_paths_wildcard_fallback_without_exports() {
    let dir = tempfile::tempdir().unwrap();
    let pkg_dir = dir.path().join("dep");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::write(
      pkg_dir.join("package.json"),
      serde_json::to_string(&json!({
        "name": "dep",
        "types": "./index.d.ts",
      }))
      .unwrap(),
    )
    .unwrap();

    let folder = path_for_typescript(&pkg_dir);
    let paths = dependency_project_paths("dep", &pkg_dir);

    // Without an exports map, keep the literal subpath wildcard fallback.
    assert_eq!(paths.get("dep"), Some(&json!([folder])));
    assert_eq!(paths.get("dep/*"), Some(&json!([format!("{folder}/*")])));
  }

  /// A minimal npm-style `.tgz`: every entry under a leading `package/`.
  fn make_tarball_gz(files: &[(&str, &str)]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    {
      let mut builder = tar::Builder::new(&mut tar_bytes);
      for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
          .append_data(
            &mut header,
            format!("package/{path}"),
            contents.as_bytes(),
          )
          .unwrap();
      }
      builder.finish().unwrap();
    }
    let mut encoder =
      flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
    encoder.finish().unwrap()
  }

  #[test]
  fn test_cleanup_stale_staging_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let staging_dir = dir.path().join(".node.tmp-123-0");
    let other_dir = dir.path().join(".other.tmp-123-0");
    let similarly_named_file = dir.path().join(".node.tmp-123-1");
    std::fs::create_dir(&staging_dir).unwrap();
    std::fs::create_dir(&other_dir).unwrap();
    std::fs::write(&similarly_named_file, "keep").unwrap();

    // A cutoff older than every filesystem entry keeps active staging dirs.
    cleanup_stale_staging_dirs(
      dir.path(),
      ".node.tmp-",
      std::time::SystemTime::UNIX_EPOCH,
    );
    assert!(staging_dir.exists());

    // A later cutoff removes only matching directories.
    cleanup_stale_staging_dirs(
      dir.path(),
      ".node.tmp-",
      std::time::SystemTime::now() + std::time::Duration::from_secs(1),
    );
    assert!(!staging_dir.exists());
    assert!(other_dir.exists());
    assert!(similarly_named_file.exists());
  }

  #[test]
  fn test_extract_tarball_gz_atomic_leaves_no_staging_dir() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("@types/node");
    let gz = make_tarball_gz(&[
      ("package.json", r#"{"name":"@types/node"}"#),
      ("index.d.ts", "declare const x: number;"),
    ]);

    extract_tarball_gz_atomic(&gz, &dest, None).unwrap();

    assert!(is_materialized_package(&dest));
    assert_eq!(
      std::fs::read_to_string(dest.join("index.d.ts")).unwrap(),
      "declare const x: number;"
    );
    // The staging sibling must be gone, not left behind next to `dest`.
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
      .unwrap()
      .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
      .filter(|n| n.starts_with('.') && n.contains(".tmp-"))
      .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "leftover staging dirs: {leftovers:?}");
  }

  #[test]
  fn test_extract_tarball_gz_atomic_heals_partial_dest() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("node");
    // A tree a killed process left half-extracted: no `package.json`, plus a
    // stale file that must not survive into the published package.
    std::fs::create_dir_all(dest.join("ts5.0")).unwrap();
    std::fs::write(dest.join("ts5.0/stale.d.ts"), "stale").unwrap();
    assert!(dest.exists());
    assert!(!is_materialized_package(&dest));

    let gz = make_tarball_gz(&[
      ("package.json", r#"{"name":"@types/node"}"#),
      ("index.d.ts", "fresh"),
    ]);
    extract_tarball_gz_atomic(&gz, &dest, None).unwrap();

    assert!(is_materialized_package(&dest));
    assert_eq!(
      std::fs::read_to_string(dest.join("index.d.ts")).unwrap(),
      "fresh"
    );
    assert!(!dest.join("ts5.0/stale.d.ts").exists());
  }

  #[test]
  fn test_extract_tarball_gz_atomic_keeps_winners_package() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("node");
    // Stand in for a concurrent process that published `dest` first.
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("package.json"), r#"{"name":"winner"}"#).unwrap();
    std::fs::write(dest.join("index.d.ts"), "winner").unwrap();

    let gz = make_tarball_gz(&[
      ("package.json", r#"{"name":"loser"}"#),
      ("index.d.ts", "loser"),
    ]);
    // Losing the race is not an error, and must not clobber the winner.
    extract_tarball_gz_atomic(&gz, &dest, None).unwrap();

    assert_eq!(
      std::fs::read_to_string(dest.join("index.d.ts")).unwrap(),
      "winner"
    );
    let leftovers = std::fs::read_dir(dest.parent().unwrap())
      .unwrap()
      .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
      .filter(|n| n.starts_with('.') && n.contains(".tmp-"))
      .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "leftover staging dirs: {leftovers:?}");
  }

  #[test]
  fn test_extract_tarball_gz_atomic_replaces_other_version() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("node");
    // A complete copy of a version we no longer want: what an older Deno left
    // behind when this followed the registry's `latest` instead of a pin.
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
      dest.join("package.json"),
      r#"{"name":"@types/node","version":"26.6.2"}"#,
    )
    .unwrap();
    std::fs::write(dest.join("index.d.ts"), "stale").unwrap();

    let gz = make_tarball_gz(&[
      (
        "package.json",
        r#"{"name":"@types/node","version":"24.2.0"}"#,
      ),
      ("index.d.ts", "pinned"),
    ]);
    extract_tarball_gz_atomic(&gz, &dest, Some("24.2.0")).unwrap();

    assert_eq!(
      std::fs::read_to_string(dest.join("index.d.ts")).unwrap(),
      "pinned"
    );
  }

  #[test]
  fn test_extract_tarball_gz_atomic_keeps_same_version() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("node");
    // A concurrent process published the version we wanted; keep its tree.
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
      dest.join("package.json"),
      r#"{"name":"@types/node","version":"24.2.0"}"#,
    )
    .unwrap();
    std::fs::write(dest.join("index.d.ts"), "winner").unwrap();

    let gz = make_tarball_gz(&[
      (
        "package.json",
        r#"{"name":"@types/node","version":"24.2.0"}"#,
      ),
      ("index.d.ts", "loser"),
    ]);
    extract_tarball_gz_atomic(&gz, &dest, Some("24.2.0")).unwrap();

    assert_eq!(
      std::fs::read_to_string(dest.join("index.d.ts")).unwrap(),
      "winner"
    );
  }

  #[test]
  fn test_url_to_mirror_rel_disambiguates_query_and_port() {
    let rel = |u: &str| url_to_mirror_rel(&Url::parse(u).unwrap()).unwrap();

    // Plain URL: host/path, unchanged.
    assert_eq!(rel("https://example.com/x/foo.ts"), "example.com/x/foo.ts");

    // Distinct queries must not collide, and the extension is preserved.
    let a = rel("https://esm.sh/react.ts?a=1");
    let b = rel("https://esm.sh/react.ts?a=2");
    assert_ne!(a, b);
    assert!(a.ends_with(".ts") && b.ends_with(".ts"));
    assert!(a.starts_with("esm.sh/") && b.starts_with("esm.sh/"));

    // Non-default port is part of the host segment.
    let p = rel("https://example.com:8443/x/foo.ts");
    assert!(p.starts_with("example.com__8443/"));
    // Same host, different port -> different mirror path.
    assert_ne!(p, rel("https://example.com:9443/x/foo.ts"));
  }
}
