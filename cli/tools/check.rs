// Copyright 2018-2026 the Deno authors. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_core::ModuleSpecifier;
use deno_core::error::AnyError;
use deno_graph::ModuleGraph;
use deno_terminal::colors;

use crate::args::CheckEngine;
use crate::args::CheckFlags;
use crate::args::CliOptions;
use crate::args::Flags;
use crate::args::SyncTypesFlags;
use crate::args::TsTypeLib;
use crate::args::TypeCheckMode;
use crate::factory::CliFactory;
use crate::graph_container::CheckSpecifiersOptions;
use crate::graph_container::CollectSpecifiersOptions;
use crate::tsc::Diagnostics;
use crate::type_checker::TypeChecker;
use crate::util::extract;
use crate::util::file_watcher;

pub async fn check(
  flags: Arc<Flags>,
  check_flags: CheckFlags,
) -> Result<(), AnyError> {
  if crate::args::resolve_check_engine(&flags) == CheckEngine::Tsc {
    return legacy_check(flags, check_flags).await;
  }
  if check_flags.doc || check_flags.doc_only {
    // The code blocks `--doc` / `--doc-only` extract live in memory only, where
    // the native compiler (which reads files) can't see them: those checks keep
    // running on the in-process TypeScript 6.0.3.
    log::debug!("--doc/--doc-only: type-checking with the in-process checker");
    // The graph check below goes through `TypeChecker::check_graph`, which
    // picks the engine from the flags: pin it to the in-process one.
    let mut flags = (*flags).clone();
    flags.check_engine = Some(CheckEngine::Tsc);
    return legacy_check(Arc::new(flags), check_flags).await;
  }
  if let Some(watch_flags) = &flags.watch {
    // A watcher-aware factory reports the graph's files to the watcher, so the
    // native check re-runs on changes like the in-process one (upstream's
    // native check ran once, denoland/deno#36089).
    let no_clear_screen = watch_flags.no_clear_screen;
    file_watcher::watch_func(
      flags,
      file_watcher::PrintConfig::new("Check", !no_clear_screen),
      move |flags, watcher_communicator, changed_paths| {
        let check_flags = check_flags.clone();
        watcher_communicator.show_path_changed(changed_paths);
        Ok(async move {
          let factory = CliFactory::from_flags_for_watcher(
            flags.clone(),
            watcher_communicator.clone(),
          );
          native_check(&factory, flags, check_flags).await
        })
      },
    )
    .await
  } else {
    let factory = CliFactory::from_flags(flags.clone());
    native_check(&factory, flags, check_flags).await
  }
}

/// Type-check the project with the native TypeScript compiler.
///
/// `deno check` generates a stock `tsconfig.json` and materializes the types of
/// the project's dependencies (the same work `deno sync-types` performs), then
/// runs the pinned native `tsc` against it and remaps the compiler's
/// diagnostics back onto the original module specifiers.
async fn native_check(
  factory: &CliFactory,
  flags: Arc<Flags>,
  check_flags: CheckFlags,
) -> Result<(), AnyError> {
  let cli_options = factory.cli_options()?;

  // Build the module graph over the requested roots (or the whole project).
  // Deno owns resolution: this drives deno's own graph diagnostics (missing
  // modules + hints) and the incremental type-check cache, so we can skip the
  // external compiler entirely when nothing it sees has changed.
  // Resolve the requested roots the same way `deno check` always has:
  // globs are expanded, workspace `exclude` is applied, and
  // `include_ignored_specified: false` means an explicitly-passed excluded file
  // is skipped rather than force-checked. An empty result is not an error - it
  // just means there's nothing to check (e.g. every match was excluded), which
  // deno reports as a warning and a clean exit.
  let graph_container = factory.main_module_graph_container().await?;
  let mut roots = graph_container.collect_specifiers(
    &check_flags.files,
    CollectSpecifiersOptions {
      include_ignored_specified: false,
    },
  )?;
  // `collect_specifiers` can yield the same workspace-member file more than
  // once (resolved both by its explicit path and via the member's package
  // `exports`), which would otherwise duplicate the `Check <specifier>` line
  // and the tsc `files` entry. Deduplicate while preserving order.
  {
    let mut seen = std::collections::HashSet::new();
    roots.retain(|s| seen.insert(s.clone()));
  }
  if roots.is_empty() {
    log::warn!("{} No matching files found.", colors::yellow("Warning"));
    return Ok(());
  }
  let graph_kind = crate::args::graph_kind(cli_options.type_check_mode());
  let imports = factory
    .module_graph_builder()
    .await?
    .maybe_resolve_ts_config_imports(graph_kind);
  let graph = factory
    .module_graph_creator()
    .await?
    .create_graph_with_options(crate::graph_util::CreateGraphOptions {
      is_dynamic: false,
      graph_kind,
      roots: roots.clone(),
      imports,
      loader: None,
      npm_caching: cli_options.default_npm_caching_strategy(),
    })
    .await?;

  // Surface deno's own graph-resolution errors before handing off to tsc:
  // unsupported import attributes (e.g. `.css` without --unstable-raw-imports),
  // `compilerOptions.paths` that resolve to nothing, integrity/npm-resolution
  // failures, and invalid specifiers. Missing-module errors are deliberately
  // *not* surfaced here (`will_type_check` defers them to tsc, which reports
  // them as TS2307), so a missing import is never double-reported. Note: the
  // richer `deno add` hint Deno 2.x's forked tsc attached to import-level
  // missing modules is not re-added on top of tsc's TS2307 yet; restoring it
  // additively is a follow-up. `allow_unknown_media_types: true` matches
  // `deno check`, letting tsc handle unknown types instead of erroring.
  factory
    .module_graph_builder()
    .await?
    .graph_roots_valid(&graph, &roots, true, false)?;

  // Enforce the lockfile now that the graph has resolved the project's deps:
  // under `--frozen` this errors if the lockfile is out of date, otherwise it
  // writes the updated lockfile. Deno 2.x's forked tsc did this via the
  // module loader; native check builds the graph itself, so do it here - before
  // materializing types and spawning tsc, so `--frozen` fails fast.
  if let Some(lockfile) = factory.maybe_lockfile().await? {
    lockfile.write_if_changed()?;
  }

  let type_checker = factory.type_checker().await?;
  // A graph the generated stock project can't express keeps the in-process
  // checker (see `TypeChecker::native_unsupported`).
  if let Some(reason) = type_checker.native_unsupported(&graph) {
    log::debug!("Type checking in-process: {reason}");
    return legacy_check_with_factory(factory, check_flags).await;
  }
  let clean = run_native_check(NativeCheck {
    flags: &flags,
    cli_options,
    type_checker,
    deno_dir_root: factory.deno_dir().ok().map(|d| d.root.clone()),
    graph: &graph,
    roots: &roots,
    sync_roots: check_flags.files.clone(),
    lib: cli_options.ts_type_lib_window(),
    type_check_mode: cli_options.type_check_mode(),
    reload: cli_options.reload_flag(),
    whole_project_fallback: true,
  })
  .await?;
  if clean {
    Ok(())
  } else {
    Err(deno_core::anyhow::anyhow!("Type checking failed."))
  }
}

/// One native type check: the graph Deno resolved (for its check cache and
/// its own diagnostics) and the roots the external compiler is pinned to.
pub(crate) struct NativeCheck<'a> {
  pub flags: &'a Arc<Flags>,
  pub cli_options: &'a CliOptions,
  pub type_checker: &'a TypeChecker,
  /// `DENO_DIR`, where denext's toolchain cache may hold the compiler.
  pub deno_dir_root: Option<PathBuf>,
  pub graph: &'a ModuleGraph,
  pub roots: &'a [ModuleSpecifier],
  /// The roots to generate the config and materialize types for, as
  /// `deno sync-types` takes them (paths, globs, remote URLs); none means the
  /// whole project.
  pub sync_roots: Vec<String>,
  pub lib: TsTypeLib,
  pub type_check_mode: TypeCheckMode,
  pub reload: bool,
  /// `deno check` falls back to the generated config's whole-project
  /// `include` when no root maps to a file the compiler can load (an
  /// extensionless remote module); the other commands check nothing then.
  pub whole_project_fallback: bool,
}

/// Type-checks `check.roots` with the native TypeScript compiler, behind
/// Deno's check cache: an unchanged graph that checked clean before skips the
/// compiler (and the type materialization). Prints the `Check` lines and any
/// diagnostics; returns whether the check was clean. Shared by `deno check`
/// and every command that type-checks through [`TypeChecker`] (`run --check`,
/// `test`, `bench`, `bundle --check`, `publish`, ...).
pub(crate) async fn run_native_check(
  check: NativeCheck<'_>,
) -> Result<bool, AnyError> {
  let NativeCheck {
    flags,
    cli_options,
    type_checker,
    deno_dir_root,
    graph,
    roots,
    sync_roots,
    lib,
    type_check_mode,
    reload,
    whole_project_fallback,
  } = check;
  let project_root = cli_options
    .workspace()
    .root_dir_url()
    .to_file_path()
    .map_err(|_| {
      deno_core::anyhow::anyhow!("workspace root is not a local directory")
    })?;

  // Walk the graph exactly as Deno 2.x's forked tsc did to obtain the combined
  // check hash (tsc version folded in). The walk also produces deno's own graph
  // diagnostics (missing modules + hints), but merging them additively isn't
  // safe yet: Deno 2.x's forked tsc filtered those against tsc's reported
  // ambient-module list to suppress false "missing module" errors for `declare
  // module` shims (including our own `*.css`). Native tsc doesn't hand us that
  // list, so deno would flag ambient specifiers tsc resolves fine. Use the walk
  // only for caching for now; additive diagnostics come once ambient modules
  // are handled.
  let (missing_diagnostics, maybe_check_hash, any_roots) =
    type_checker.walk_graph_for_native_check(graph, lib, type_check_mode)?;

  // A root (entrypoint) that deno's graph couldn't resolve - a local file that
  // doesn't exist, or a remote URL that failed to fetch - can't be type-checked.
  // Handing a local one to tsc yields a leaky "File not found. Part of 'files'
  // list in tsconfig.json"; a remote one isn't a tsc `files` entry at all, so it
  // silently falls back to checking the whole project. Deno's graph walk already
  // produced the proper "Cannot find module" diagnostic for the root (the walk
  // only emits one when the root failed to resolve), so surface that and keep
  // the root out of tsc. Missing *imports* stay deferred to tsc (which reports
  // TS2307 for them), so this only owns the entrypoints.
  let root_urls: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
  let root_diagnostics = missing_diagnostics.filter(|d| {
    d.missing_specifier
      .as_ref()
      .is_some_and(|s| root_urls.contains(s))
  });
  // Nothing to check (only plain JavaScript without `checkJs`, say): as the
  // in-process checker does under `run`, `test` & co., report a missing root,
  // else check nothing and print no `Check` line (`deno check` keeps
  // upstream's native behaviour and lists its roots).
  if !any_roots && !whole_project_fallback {
    if root_diagnostics.has_diagnostic() {
      let current_dir =
        deno_path_util::url_from_directory_path(cli_options.initial_cwd())
          .map_err(|e| deno_core::anyhow::anyhow!("{e}"))?;
      log_check_roots(roots, &current_dir);
      log::error!("{}\n", root_diagnostics);
      return Ok(false);
    }
    return Ok(true);
  }
  let type_check_cache = type_checker.type_check_cache();

  // Cache hit: the hash is only recorded after a clean check, so a match means
  // the project type-checked cleanly and nothing the compiler sees has changed.
  // Skip both the (expensive) type materialization and the tsc spawn. A missing
  // root means there's a diagnostic to report, so never take the cache path.
  if !reload
    && !root_diagnostics.has_diagnostic()
    && let Some(check_hash) = maybe_check_hash
    && type_check_cache.has_check_hash(check_hash)
  {
    log::debug!("Already type checked (native tsc)");
    return Ok(true);
  }

  // Cache miss: generate the tsconfig.json and materialize dependency types so
  // the native compiler can resolve the project's jsr:/npm:/http(s): imports.
  // Suppress sync-types' own progress/summary output (an internal step here) so
  // it doesn't precede the type-check diagnostics. This clobbers the global log
  // level, which is not ideal; replacing it with source-level suppression (or
  // reusing the already-built graph so sync-types doesn't re-fetch) is tracked
  // as a follow-up.
  //
  // Resolve the compiler first: when it is missing, say so before writing
  // anything into the project.
  let tsc_path = crate::tsc::native::find_native_tsc(deno_dir_root.as_deref())?;
  // denext: the built-in `vitest` / `@jest/globals` types (item 29), written
  // before the config is generated so a `compilerOptions.types` entry naming
  // one of them resolves (unless `test.vitestCompat` is `false`).
  let vitest_compat = cli_options
    .resolve_workspace_test_options(&Default::default())
    .vitest_compat;
  if vitest_compat {
    crate::tools::test::vitest::write_native_check_types(&project_root)?;
  }
  let prev_level = log::max_level();
  log::set_max_level(log::LevelFilter::Error);
  let sync_result = crate::tools::installer::sync_types_command(
    flags.clone(),
    SyncTypesFlags { roots: sync_roots },
    crate::tools::installer::RootTsConfigMode::CheckMode,
  )
  .await;
  log::set_max_level(prev_level);
  sync_result?;
  if vitest_compat {
    crate::tools::test::vitest::add_native_check_type_paths(
      &project_root,
      &project_root.join(".deno").join("tsconfig.json"),
    )?;
  }

  // When Deno honors a user `tsconfig.json`, base tsc on a throwaway overlay of
  // it (its options + our generated `extends`/`references`) written to a temp
  // file in the project root - so the user's path-based options (rootDirs,
  // baseUrl, include/files) resolve relative to the project, WITHOUT us
  // rewriting their committed file. Otherwise point tsc at the generated config
  // directly. See `sync_types_command` / `build_check_root_overlay`.
  let config_disabled =
    matches!(flags.config_flag, crate::args::ConfigFlag::Disabled);
  let honor_user_tsconfig =
    crate::tsc::tsconfig_gen::should_honor_user_tsconfig(
      &project_root,
      config_disabled,
    );
  let root_tsconfig = project_root.join("tsconfig.json");
  let root_overlay = if honor_user_tsconfig && root_tsconfig.exists() {
    Some(crate::tsc::tsconfig_gen::build_check_root_overlay(
      &project_root,
      &root_tsconfig,
    )?)
  } else {
    None
  };
  // Pin tsc to exactly the roots deno resolved (glob-expanded and exclude-applied
  // above), so `deno check *.ts` checks only the matched files - not the whole
  // project - and an excluded file stays excluded. Local roots map to their
  // `file://` path; remote roots map to their `.deno/remote/` mirror (added just
  // below). Only an extensionless remote root - which stock tsc can't load - is
  // left out, falling back to the base config's `include`.
  let mut files: Vec<String> = roots
    .iter()
    .filter(|s| s.scheme() == "file")
    .filter_map(|s| s.to_file_path().ok())
    .filter(|p| p.exists())
    .map(|p| p.to_string_lossy().replace('\\', "/"))
    .collect();
  // A remote (`http(s):`) root is not a `file://` path, but sync-types mirrored
  // it under `.deno/remote/`. Pin tsc to those mirror files too, so a remote
  // entrypoint is checked as itself rather than triggering the whole-project
  // `include` fallback below (which would check unrelated files, or nothing).
  files.extend(remote_root_mirror_files(&project_root, roots));

  // Render each root relative to the invocation directory, matching deno's own
  // `Check <specifier>` output.
  let current_dir =
    deno_path_util::url_from_directory_path(cli_options.initial_cwd())
      .map_err(|e| deno_core::anyhow::anyhow!("{e}"))?;

  // Every requested root was a missing (non-existent) local entrypoint: there's
  // nothing for tsc to check, so report deno's graph diagnostics for them
  // directly instead of falling back to the base config's project-wide
  // `include` (which would check unrelated files).
  if files.is_empty() && root_diagnostics.has_diagnostic() {
    log_check_roots(roots, &current_dir);
    log::error!("{}\n", root_diagnostics);
    return Ok(false);
  }
  // Nothing the compiler can load (only built-in or in-memory modules): when
  // not `deno check`, that is nothing to check rather than the whole project.
  if files.is_empty() && !whole_project_fallback {
    return Ok(true);
  }

  // Holds the config's temp file open until tsc has run (dropping it deletes
  // the file), keeping `deno check` side-effect-free on the user's tree.
  let _check_tsconfig_guard;
  let tsconfig_path = if files.is_empty() {
    match root_overlay {
      Some(overlay) => {
        let tmp = write_root_overlay(&project_root, &overlay)?;
        let path = tmp.path().to_path_buf();
        _check_tsconfig_guard = tmp;
        path
      }
      None => project_root.join(".deno").join("tsconfig.json"),
    }
  } else {
    // `deno check <files>` checks only the named files (and their imports), not
    // the whole project. The generated `tsconfig.json` keeps an open `include`
    // (so bundlers can consume its resolver mappings), so write a per-file
    // config that extends it (by absolute path) and pins `files` - `files`/
    // `include` are not inherited through `extends`, so only these files are
    // type-checked while compilerOptions/paths still apply. `include: []`
    // nullifies the base's open `include` (tsc unions `files` with an inherited
    // `include`, which would otherwise re-add the whole project).
    //
    // `files`/`include` are not inherited through `extends`, so the base config's
    // own `files` - the declaration files sync-types materialized from
    // `compilerOptions.types` (npm packages, relative paths) that provide global
    // augmentations - would be dropped. Carry them over so those types still
    // apply when checking specific files.
    let generated_base = project_root.join(".deno").join("tsconfig.json");
    if let Ok(text) = std::fs::read_to_string(&generated_base)
      && let Ok(value) =
        deno_core::serde_json::from_str::<deno_core::serde_json::Value>(&text)
      && let Some(base_files) = value.get("files").and_then(|f| f.as_array())
    {
      for f in base_files {
        if let Some(s) = f.as_str() {
          let s = s.to_string();
          if !files.contains(&s) {
            files.push(s);
          }
        }
      }
    }
    // denext: with the user's tsconfig honored, its overlay (a temp file in
    // the project root) takes the `files` itself rather than being extended
    // from the temp dir: the compiler resolves a path in a message about an
    // option (TS5102's `baseUrl` hint) relative to the config it was given, as
    // it does for the user's own file. Its `references` go, as they did when
    // a per-file config extended it (`references` are not inherited).
    if let Some(mut overlay) = root_overlay {
      if let Some(object) = overlay.as_object_mut() {
        object.insert("include".to_string(), deno_core::serde_json::json!([]));
        object.insert("files".to_string(), deno_core::serde_json::json!(files));
        object.remove("references");
      }
      let tmp = write_root_overlay(&project_root, &overlay)?;
      let path = tmp.path().to_path_buf();
      _check_tsconfig_guard = tmp;
      path
    } else {
      // Write to the system temp dir, not the project root: this config only
      // references absolute paths (`extends` the base config by absolute path,
      // `files` are absolute, `include` is empty), and tsc runs with its cwd
      // pinned to `project_root` regardless of where the config lives, so its
      // location does not affect resolution. Keeping it out of the project tree
      // avoids leaving an artifact behind and, more importantly, avoids racing
      // with anything enumerating the project directory - e.g. a sibling
      // spec-test variant that copies the directory while this ephemeral file
      // briefly exists (and then vanishes when the guard drops).
      let base_tsconfig = project_root.join(".deno").join("tsconfig.json");
      let content = deno_core::serde_json::json!({
        "extends": base_tsconfig.to_string_lossy().replace('\\', "/"),
        "include": [],
        "files": files,
      });
      let mut tmp = tempfile::Builder::new()
        .prefix("deno-check-")
        .suffix(".tsconfig.json")
        .tempfile()?;
      std::io::Write::write_all(
        &mut tmp,
        deno_core::serde_json::to_string_pretty(&content)?.as_bytes(),
      )?;
      let path = tmp.path().to_path_buf();
      _check_tsconfig_guard = tmp;
      path
    }
  };

  log_check_roots(roots, &current_dir);

  let output = crate::tsc::native::run_native_tsc(
    &tsc_path,
    &tsconfig_path,
    &project_root,
  )
  .await?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let mut diagnostics = Diagnostics::from(
    crate::tsc::native::parse_tsc_diagnostics(&stdout, &project_root),
  );
  // Fold in deno's own diagnostics for missing entrypoints (some roots existed
  // and were type-checked by tsc above; any that didn't are reported here).
  diagnostics.extend(root_diagnostics);

  // Captured for an upcoming "Checked N files" summary; not surfaced yet.
  let stats = crate::tsc::native::parse_tsc_stats(&stdout);
  log::debug!("native tsc {stats:?}");

  if diagnostics.has_diagnostic() {
    log::error!("{}\n", diagnostics);
    return Ok(false);
  }

  // tsc exited non-zero but we have nothing to show. If it printed diagnostic
  // lines we deliberately dropped (e.g. `noImplicitOverride` in remote modules),
  // its exit code is expected - treat the check as clean. Only when tsc produced
  // no recognizable diagnostics at all is this a genuine failure (an internal
  // error or a malformed generated config); surface whatever it printed then.
  let tsc_reported_diagnostics =
    crate::tsc::native::output_has_diagnostics(&stdout);
  if !output.status.success() && !tsc_reported_diagnostics {
    let detail = stdout.trim();
    let detail = if detail.is_empty() {
      stderr.trim()
    } else {
      detail
    };
    return Err(deno_core::anyhow::anyhow!(
      "native tsc exited with {} without parseable diagnostics{}",
      output.status,
      if detail.is_empty() {
        String::new()
      } else {
        format!(":\n{detail}")
      }
    ));
  }

  // Type-checked clean: record the hash so an unchanged re-check skips tsc.
  if let Some(check_hash) = maybe_check_hash {
    type_check_cache.add_check_hash(check_hash);
  }

  Ok(true)
}

/// Write the root overlay (the user's `tsconfig.json` on top of the generated
/// one, see `build_check_root_overlay`) to a temp file in the project root, so
/// the user's path-based options resolve as in their own file.
fn write_root_overlay(
  project_root: &Path,
  overlay: &deno_core::serde_json::Value,
) -> Result<tempfile::NamedTempFile, AnyError> {
  let mut tmp = tempfile::Builder::new()
    .prefix("deno-check-root-")
    .suffix(".tsconfig.json")
    .tempfile_in(project_root)?;
  std::io::Write::write_all(
    &mut tmp,
    deno_core::serde_json::to_string_pretty(overlay)?.as_bytes(),
  )?;
  Ok(tmp)
}

/// Print a `Check <specifier>` line for each provided root, rendered relative
/// to `current_dir`, matching Deno 2.x's forked tsc output.
fn log_check_roots(
  roots: &[deno_core::ModuleSpecifier],
  current_dir: &deno_core::ModuleSpecifier,
) {
  for root in roots {
    log::info!(
      "{} {}",
      colors::green("Check"),
      crate::util::path::relative_specifier_path_for_display(current_dir, root),
    );
  }
}

/// Whether a mirror path carries an extension stock tsc can language-detect.
/// A module Deno serves by content-type but mirrors without a code extension
/// (`no_js_ext`) can't be loaded by stock tsc, which keys language off the
/// extension alone.
fn has_checkable_extension(path: &str) -> bool {
  let lower = path.to_ascii_lowercase();
  [
    ".d.ts", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx",
    ".mjs", ".cjs",
  ]
  .iter()
  .any(|ext| lower.ends_with(ext))
}

/// Resolve remote (`http(s):`) roots to their mirrored local files under
/// `.deno/remote/` (via the generated tsconfig's `paths`) so tsc can be pinned
/// to exactly the requested remote entrypoint via `files`, instead of dropping
/// them and falling back to the base config's whole-project `include`.
/// Extensionless remote modules have no mirror stock tsc can load and are
/// skipped (see `has_checkable_extension`).
fn remote_root_mirror_files(
  project_root: &Path,
  roots: &[deno_core::ModuleSpecifier],
) -> Vec<String> {
  let deno_dir = project_root.join(".deno");
  let Ok(text) = std::fs::read_to_string(deno_dir.join("tsconfig.json")) else {
    return Vec::new();
  };
  let Ok(value) =
    deno_core::serde_json::from_str::<deno_core::serde_json::Value>(&text)
  else {
    return Vec::new();
  };
  let Some(paths) = value
    .get("compilerOptions")
    .and_then(|c| c.get("paths"))
    .and_then(|p| p.as_object())
  else {
    return Vec::new();
  };
  let mut files = Vec::new();
  for root in roots.iter().filter(|s| s.scheme() != "file") {
    let Some(rel) = paths
      .get(root.as_str())
      .and_then(|t| t.as_array())
      .and_then(|a| a.first())
      .and_then(|v| v.as_str())
      .and_then(|t| t.strip_prefix("./"))
    else {
      continue;
    };
    if !has_checkable_extension(rel) {
      continue;
    }
    // `paths` targets are relative to `.deno/` (where the generated tsconfig
    // lives); rebase onto an absolute path for the `files` entry.
    let local = deno_dir.join(rel);
    if local.exists() {
      files.push(local.to_string_lossy().replace('\\', "/"));
    }
  }
  files
}

/// The in-isolate TypeScript 6.0.3 checker (`--check-engine=tsc`).
async fn legacy_check(
  flags: Arc<Flags>,
  check_flags: CheckFlags,
) -> Result<(), AnyError> {
  if let Some(watch_flags) = &flags.watch {
    let no_clear_screen = watch_flags.no_clear_screen;
    file_watcher::watch_func(
      flags,
      file_watcher::PrintConfig::new("Check", !no_clear_screen),
      move |flags, watcher_communicator, changed_paths| {
        let check_flags = check_flags.clone();
        watcher_communicator.show_path_changed(changed_paths);
        Ok(async move {
          let factory = CliFactory::from_flags_for_watcher(
            flags,
            watcher_communicator.clone(),
          );
          legacy_check_with_factory(&factory, check_flags).await
        })
      },
    )
    .await
  } else {
    let factory = CliFactory::from_flags(flags);
    legacy_check_with_factory(&factory, check_flags).await
  }
}

async fn legacy_check_with_factory(
  factory: &CliFactory,
  check_flags: CheckFlags,
) -> Result<(), AnyError> {
  // denext: a Vitest or Jest test file checks against the same types of
  // `vitest` / `@jest/globals` as under `deno test` (unless the workspace
  // turns the compatibility layer off with `test.vitestCompat: false`).
  let cli_options = factory.cli_options()?;
  if cli_options
    .resolve_workspace_test_options(&Default::default())
    .vitest_compat
  {
    crate::tools::test::vitest::install_type_overrides(
      factory.resolver().await?,
      &factory.deno_dir()?.root.join("vitest_compat"),
      cli_options.initial_cwd(),
    )?;
  }
  let main_graph_container = factory.main_module_graph_container().await?;

  let specifiers = main_graph_container.collect_specifiers(
    &check_flags.files,
    CollectSpecifiersOptions {
      include_ignored_specified: false,
    },
  )?;
  if specifiers.is_empty() {
    log::warn!("{} No matching files found.", colors::yellow("Warning"));
  }

  let specifiers_for_typecheck = if check_flags.doc || check_flags.doc_only {
    let file_fetcher = factory.file_fetcher()?;
    let root_permissions = factory.root_permissions_container()?;

    let mut specifiers_for_typecheck = if check_flags.doc {
      specifiers.clone()
    } else {
      vec![]
    };

    for s in specifiers {
      let file = file_fetcher.fetch(&s, root_permissions).await?;
      let snippet_files = extract::extract_snippet_files(file)?;
      for snippet_file in snippet_files {
        specifiers_for_typecheck.push(snippet_file.url.clone());
        file_fetcher.insert_memory_files(snippet_file);
      }
    }

    specifiers_for_typecheck
  } else {
    specifiers
  };

  main_graph_container
    .check_specifiers(
      &specifiers_for_typecheck,
      CheckSpecifiersOptions {
        allow_unknown_media_types: true,
        ..Default::default()
      },
    )
    .await
}
