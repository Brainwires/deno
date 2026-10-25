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
    // The code blocks `--doc` / `--doc-only` extract live in memory: the graph
    // check below goes through `TypeChecker::check_graph`, which writes them
    // out for the native compiler (`run_native_check`), or keeps the
    // in-process checker for a graph the native one can't express.
    return legacy_check(flags, check_flags).await;
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
    log_checked_summary(roots.len());
    Ok(())
  } else {
    Err(deno_core::anyhow::anyhow!("Type checking failed."))
  }
}

/// One native type check: the graph Deno resolved (for its check cache and
/// its own diagnostics) and the roots the external compiler is pinned to.
#[derive(Clone)]
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
  // Boxed: the native check's future is large (clippy::large_futures).
  match Box::pin(run_native_check_once(
    check.clone(),
    NativeAttempt::ReuseGenerated,
  ))
  .await?
  {
    NativeOutcome::Checked(clean) => Ok(clean),
    // The reused generated project left an import unresolved: generate it
    // again and check once more (the roots are listed already).
    NativeOutcome::GeneratedStale => {
      match Box::pin(run_native_check_once(check, NativeAttempt::Regenerate))
        .await?
      {
        NativeOutcome::Checked(clean) => Ok(clean),
        NativeOutcome::GeneratedStale => unreachable!("regenerated"),
      }
    }
  }
}

/// Whether a native check may reuse the generated project (`.deno/`) when
/// what it was generated from is unchanged, or must generate it again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeAttempt {
  ReuseGenerated,
  Regenerate,
}

enum NativeOutcome {
  /// Checked; whether clean.
  Checked(bool),
  /// The reused generated project could not resolve an import.
  GeneratedStale,
}

async fn run_native_check_once(
  check: NativeCheck<'_>,
  attempt: NativeAttempt,
) -> Result<NativeOutcome, AnyError> {
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
      return Ok(NativeOutcome::Checked(false));
    }
    return Ok(NativeOutcome::Checked(true));
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
    return Ok(NativeOutcome::Checked(true));
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
  // denext: the code blocks a doc test or `check --doc` extracted live in
  // memory only; the external specifiers they import are mapped with the
  // project's, and the blocks themselves are written out below.
  let has_code_blocks = code_block_modules(graph).next().is_some();
  let extra_specifiers = if has_code_blocks {
    code_block_external_specifiers(graph)
  } else {
    Vec::new()
  };
  // denext: quiet on this thread only - lowering the global `log` level hid
  // other threads' output meanwhile (the inspector's "Debugger session
  // started.", so a debugger client waited for it forever).
  //
  // denext: the generated project depends on the configuration, the external
  // modules the graph reaches and the roots, not on the project's own source,
  // so a check after a local edit (each of the dev overlay's) reuses it
  // instead of generating it again (`generated_project_inputs`).
  let generated_dir = project_root.join(".deno");
  let inputs = (!reload && !has_code_blocks).then(|| {
    generated_project_inputs(
      flags,
      cli_options,
      graph,
      lib,
      type_check_mode,
      &sync_roots,
    )
  });
  let reused = attempt == NativeAttempt::ReuseGenerated
    && inputs.as_deref().is_some_and(|inputs| {
      generated_project_is_current(&generated_dir, inputs)
    });
  if reused {
    log::debug!("Reusing the generated project (native tsc)");
  } else {
    let quiet = deno_lib::util::logger::quiet_below_error();
    let sync_result = crate::tools::installer::sync_types_command(
      flags.clone(),
      SyncTypesFlags { roots: sync_roots },
      crate::tools::installer::RootTsConfigMode::CheckMode,
      extra_specifiers,
    )
    .await;
    drop(quiet);
    sync_result?;
    if let Some(inputs) = &inputs {
      record_generated_project(&generated_dir, inputs);
    }
  }
  // Removed (with its directory) when the check is done.
  let code_blocks = if has_code_blocks {
    Some(materialize_code_blocks(&project_root, graph)?)
  } else {
    None
  };

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
    .filter(|s| s.scheme() == "file" && s.fragment().is_none())
    .filter_map(|s| s.to_file_path().ok())
    .filter(|p| p.exists())
    .map(|p| p.to_string_lossy().replace('\\', "/"))
    .collect();
  // A remote (`http(s):`) root is not a `file://` path, but sync-types mirrored
  // it under `.deno/remote/`. Pin tsc to those mirror files too, so a remote
  // entrypoint is checked as itself rather than triggering the whole-project
  // `include` fallback below (which would check unrelated files, or nothing).
  files.extend(remote_root_mirror_files(&project_root, roots));
  if let Some(code_blocks) = &code_blocks {
    files.extend(
      code_blocks
        .files
        .iter()
        .map(|(path, _)| path.to_string_lossy().replace('\\', "/")),
    );
  }

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
    return Ok(NativeOutcome::Checked(false));
  }
  // Nothing the compiler can load (only built-in or in-memory modules): when
  // not `deno check`, that is nothing to check rather than the whole project.
  if files.is_empty() && !whole_project_fallback {
    return Ok(NativeOutcome::Checked(true));
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

  if attempt == NativeAttempt::ReuseGenerated {
    log_check_roots(roots, &current_dir);
  }

  let output = crate::tsc::native::run_native_tsc(
    &tsc_path,
    &tsconfig_path,
    &project_root,
  )
  .await?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  let mut parsed =
    crate::tsc::native::parse_tsc_diagnostics(&stdout, &project_root);
  if let Some(code_blocks) = &code_blocks {
    code_blocks.remap_diagnostics(&mut parsed);
    // The compiler reported the written blocks (under `.deno/`) before the
    // project's own files; list them by module as the in-process checker
    // does (`mod.ts` before `mod.ts#3-7.ts`), in order within each.
    parsed.sort_by(|a, b| a.file_name.cmp(&b.file_name));
  }
  drop(code_blocks);
  // A reused generated project that can't resolve an import may predate a
  // dependency (one cached, or a `node_modules` changed, since): generate it
  // again rather than report what a fresh one would resolve.
  if reused && parsed.iter().any(needs_generated_project) {
    let _ = std::fs::remove_file(generated_dir.join(GENERATED_INPUTS_FILE));
    log::debug!("The reused generated project left an import unresolved");
    return Ok(NativeOutcome::GeneratedStale);
  }
  // denext: an import Deno could not resolve (a bare specifier that is not a
  // dependency, say) gets Deno's message and `deno add` hint instead of the
  // compiler's bare "Cannot find module", as the in-process checker reports it.
  let bare_importable_pkg_names: Vec<String> = cli_options
    .workspace()
    .resolver_jsr_pkgs()
    .map(|pkg| pkg.name)
    .collect();
  explain_unresolved_imports(graph, &mut parsed, &bare_importable_pkg_names);
  let mut diagnostics = Diagnostics::from(parsed);
  // Fold in deno's own diagnostics for missing entrypoints (some roots existed
  // and were type-checked by tsc above; any that didn't are reported here).
  diagnostics.extend(root_diagnostics);

  // Captured for an upcoming "Checked N files" summary; not surfaced yet.
  let stats = crate::tsc::native::parse_tsc_stats(&stdout);
  log::debug!("native tsc {stats:?}");

  if diagnostics.has_diagnostic() {
    log::error!("{}\n", diagnostics);
    return Ok(NativeOutcome::Checked(false));
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

  Ok(NativeOutcome::Checked(true))
}

/// Gives a compiler diagnostic for an import it could not find (TS2307,
/// TS2792, TS2882) the message of the resolution error Deno's graph recorded
/// for that import, when Deno could not resolve it either: `Import "x" not a
/// dependency` with its `deno add` hint, an unmapped bare specifier, and so
/// on. A module Deno resolved but could not find keeps the compiler's message,
/// as does any import the graph has no error for. Matched by the import's
/// position (the compiler and the graph both point at the specifier string).
fn explain_unresolved_imports(
  graph: &ModuleGraph,
  diagnostics: &mut [crate::tsc::Diagnostic],
  bare_importable_pkg_names: &[String],
) {
  for diagnostic in diagnostics.iter_mut() {
    if !matches!(diagnostic.code, 2307 | 2792 | 2882) {
      continue;
    }
    let (Some(file_name), Some(start)) =
      (diagnostic.file_name.as_deref(), diagnostic.start.as_ref())
    else {
      continue;
    };
    let Ok(specifier) = ModuleSpecifier::parse(file_name) else {
      continue;
    };
    let Some(deno_graph::Module::Js(module)) = graph.get(&specifier) else {
      continue;
    };
    let at_start = |range: &deno_graph::Range| {
      range.range.start.line as u64 == start.line
        && range.range.start.character as u64 == start.character
    };
    let explained = module.dependencies.values().find_map(|dep| {
      [&dep.maybe_code, &dep.maybe_type]
        .into_iter()
        .find_map(|resolution| {
          let deno_graph::Resolution::Err(error) = resolution else {
            return None;
          };
          let error_ref =
            crate::graph_util::resolution_error_for_tsc_diagnostic(error)?;
          if error_ref.is_module_not_found
            || !(at_start(error_ref.range)
              || dep.imports.iter().any(|i| at_start(&i.specifier_range)))
          {
            return None;
          }
          crate::tsc::Diagnostic::maybe_from_resolution_error(
            error,
            bare_importable_pkg_names,
          )
        })
    });
    if let Some(explained) = explained {
      diagnostic.code = explained.code;
      diagnostic.message_text = explained.message_text;
      diagnostic.message_chain = None;
    }
  }
}

/// The file in the generated `.deno/` that records what its project was
/// generated from (`generated_project_inputs`) and the generated
/// `tsconfig.json` it then held (denext).
const GENERATED_INPUTS_FILE: &str = "check-inputs";

/// A digest of everything the generated project (`deno sync-types`'s
/// `.deno/`) is made from, short of the project's own source: this Deno and
/// its compiler, the command's flags (its roots among them), the type lib
/// and check mode (`--check=all` checks remote modules too),
/// the registries, the workspace's config files (deno.json, package.json, the
/// import map, the lockfile, a root tsconfig.json, an installed
/// `node_modules`' records), and the graph's modules from outside the project
/// (remote, `jsr:` and `npm:` ones, with their resolved versions), local
/// Wasm modules (whose exports it types) and the modules local files augment
/// (`declare module "<name>"`). A local edit that changes none of them
/// reuses the project.
fn generated_project_inputs(
  flags: &Flags,
  cli_options: &CliOptions,
  graph: &ModuleGraph,
  lib: TsTypeLib,
  type_check_mode: TypeCheckMode,
  sync_roots: &[String],
) -> String {
  use deno_lib::util::hash::FastInsecureHasher;
  let mut hasher = FastInsecureHasher::new_deno_versioned();
  hasher.write_str(crate::tsc::native::typescript_version());
  // The flags that change what is generated: not the log level, the
  // permissions or a script's arguments, so the dev overlay's checks and
  // `deno check` of the same roots share one.
  hasher.write_str(&format!(
    "{lib:?}\n{type_check_mode:?}\n{sync_roots:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
    flags.subcommand,
    flags.initial_cwd,
    flags.config_flag,
    flags.import_map_path,
    flags.node_modules_dir,
    flags.node_modules_linker,
    flags.vendor,
    flags.unstable_config,
    (flags.no_npm, flags.no_remote, flags.no_lock, flags.cached_only),
    flags.lock,
    flags.ext,
    flags.internal,
  ));
  for name in [
    "JSR_URL",
    "NPM_CONFIG_REGISTRY",
    "npm_config_registry",
    "DENO_DIR",
    "DENO_INTERNAL_SYNC_TYPES_SPECIFIERS",
  ] {
    hasher.write_str(name).write_u8(0);
    hasher.write_str(&std::env::var(name).unwrap_or_default());
    hasher.write_u8(0);
  }
  let workspace = cli_options.workspace();
  let mut files: Vec<PathBuf> = workspace
    .deno_jsons()
    .filter_map(|config| config.specifier.to_file_path().ok())
    .chain(workspace.package_jsons().map(|pkg| pkg.path.clone()))
    .collect();
  if let Ok(Some(path)) = workspace.to_import_map_path() {
    files.push(path);
  }
  if let Ok(Some(path)) = workspace.resolve_lockfile_path() {
    files.push(path);
  }
  if let Ok(root) = workspace.root_dir_url().to_file_path() {
    files.push(root.join("tsconfig.json"));
    let node_modules = root.join("node_modules");
    files.push(node_modules.join(".package-lock.json"));
    files.push(node_modules.join(".modules.yaml"));
    files.push(node_modules.join(".deno").join(".setup-cache.bin"));
  }
  files.sort();
  files.dedup();
  for path in &files {
    hasher.write_str(&path.to_string_lossy()).write_u8(0);
    match std::fs::read(path) {
      Ok(bytes) => hasher.write_u8(1).write(&bytes),
      Err(_) => hasher.write_u8(0),
    };
  }
  let mut modules: Vec<String> = Vec::new();
  for module in graph.modules() {
    let specifier = module.specifier();
    if specifier.scheme() != "file" {
      modules.push(specifier.to_string());
    } else if matches!(module, deno_graph::Module::Wasm(_)) {
      let stamp = specifier
        .to_file_path()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| (meta.len(), meta.modified().ok()));
      modules.push(format!("{specifier} {stamp:?}"));
    }
  }
  modules.sort();
  for module in &modules {
    hasher.write_str(module).write_u8(0);
  }
  // denext: the modules the project's files augment are generated as imports
  // (`deno sync-types`, `.deno/module_augmentations.d.ts`).
  hasher.write_u8(0xff);
  for name in crate::tools::installer::graph_augmentation_names(graph) {
    hasher.write_str(&name).write_u8(0);
  }
  format!("{:016x}", hasher.finish())
}

/// A digest of a generated `tsconfig.json`'s text.
fn generated_config_digest(text: &[u8]) -> String {
  let mut hasher =
    deno_lib::util::hash::FastInsecureHasher::new_without_deno_version();
  hasher.write(text);
  format!("{:016x}", hasher.finish())
}

/// Record that the generated project in `generated_dir` was made from
/// `inputs` (best effort: without the record the next check generates it).
fn record_generated_project(generated_dir: &Path, inputs: &str) {
  let Ok(config) = std::fs::read(generated_dir.join("tsconfig.json")) else {
    return;
  };
  let record = format!("{inputs}\n{}\n", generated_config_digest(&config));
  let _ = std::fs::write(generated_dir.join(GENERATED_INPUTS_FILE), record);
}

/// Whether the generated project in `generated_dir` was made from `inputs`,
/// is unchanged since (another check, `deno sync-types` or the tsgo language
/// server may have generated it from other inputs), and every file outside
/// the project its `paths`, `typeRoots` and `files` name still exists (a
/// cleared Deno cache).
fn generated_project_is_current(generated_dir: &Path, inputs: &str) -> bool {
  let Ok(record) =
    std::fs::read_to_string(generated_dir.join(GENERATED_INPUTS_FILE))
  else {
    return false;
  };
  let mut lines = record.lines();
  if lines.next() != Some(inputs) {
    return false;
  }
  let Ok(config) = std::fs::read(generated_dir.join("tsconfig.json")) else {
    return false;
  };
  if lines.next() != Some(generated_config_digest(&config).as_str()) {
    return false;
  }
  generated_targets_exist(generated_dir, &config)
}

/// Whether the targets a generated `tsconfig.json` names in `.deno/` or
/// outside the project exist (a `*` pattern's directory part). Targets in the
/// project's own tree are the user's to have.
fn generated_targets_exist(generated_dir: &Path, config: &[u8]) -> bool {
  let Ok(value) =
    deno_core::serde_json::from_slice::<deno_core::serde_json::Value>(config)
  else {
    return false;
  };
  let Some(project_root) = generated_dir.parent() else {
    return false;
  };
  let options = value.get("compilerOptions");
  let paths = options
    .and_then(|o| o.get("paths"))
    .and_then(|p| p.as_object())
    .into_iter()
    .flat_map(|paths| paths.values())
    .filter_map(|targets| targets.as_array())
    .flatten();
  let type_roots = options
    .and_then(|o| o.get("typeRoots"))
    .and_then(|t| t.as_array())
    .into_iter()
    .flatten();
  let files = value
    .get("files")
    .and_then(|f| f.as_array())
    .into_iter()
    .flatten();
  paths
    .chain(type_roots)
    .chain(files)
    .filter_map(|target| target.as_str())
    .all(|target| {
      let target = target.split('*').next().unwrap_or_default();
      let path = generated_dir.join(target);
      let path = deno_path_util::normalize_path(std::borrow::Cow::Owned(path));
      if path.starts_with(project_root) && !path.starts_with(generated_dir) {
        return true;
      }
      path.exists()
    })
}

/// Whether a compiler diagnostic says an import, a type package or a JSX
/// runtime did not resolve: what a stale generated project reports.
fn needs_generated_project(diagnostic: &crate::tsc::Diagnostic) -> bool {
  // TS2307 / TS2792 cannot find module, TS2688 type definition file, TS2875
  // JSX runtime, TS7016 no declaration file.
  matches!(diagnostic.code, 2307 | 2792 | 2688 | 2875 | 7016)
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

/// The code blocks a doc test or `check --doc` extracted
/// (`file:///a/mod.ts#3-7.ts`), held in memory by the file fetcher.
fn code_block_modules(
  graph: &ModuleGraph,
) -> impl Iterator<Item = &deno_graph::JsModule> {
  graph.modules().filter_map(|module| match module {
    deno_graph::Module::Js(js)
      if js.specifier.scheme() == "file"
        && js.specifier.fragment().is_some() =>
    {
      Some(js)
    }
    _ => None,
  })
}

/// The external specifiers (`jsr:`, `npm:`, `http(s):`, import-map keys) the
/// local modules of `graph`, code blocks included, import: what `deno
/// sync-types` maps, which reads the project from disk and so never sees a
/// code block's own imports.
fn code_block_external_specifiers(graph: &ModuleGraph) -> Vec<String> {
  let mut specifiers = std::collections::BTreeSet::new();
  for module in graph.modules() {
    if module.specifier().scheme() != "file" {
      continue;
    }
    for raw in module.dependencies().keys() {
      if raw.starts_with('.')
        || raw.starts_with('/')
        || raw.starts_with("node:")
        || raw.starts_with("file:")
      {
        continue;
      }
      specifiers.insert(raw.clone());
    }
  }
  specifiers.into_iter().collect()
}

/// Code blocks written out for the native compiler, which reads only files:
/// one file per block in a fresh directory below the project's `.deno/doc/`
/// (inside the project, so the generated `paths` and `node_modules` resolve
/// as for its other modules), removed with the directory on drop.
struct CodeBlocks {
  dir: tempfile::TempDir,
  /// Each written file and the block's specifier.
  files: Vec<(PathBuf, ModuleSpecifier)>,
}

impl CodeBlocks {
  /// Report a diagnostic in a written block against the block's specifier
  /// (`file:///a/mod.ts#3-7.ts`), as the in-process checker does.
  fn remap_diagnostics(&self, diagnostics: &mut [crate::tsc::Diagnostic]) {
    let dir_name = self
      .dir
      .path()
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
    let by_suffix = self
      .files
      .iter()
      .filter_map(|(path, specifier)| {
        let name = path.file_name()?.to_string_lossy();
        Some((format!("/{dir_name}/{name}"), specifier.to_string()))
      })
      .collect::<Vec<_>>();
    fn remap(
      diagnostic: &mut crate::tsc::Diagnostic,
      by_suffix: &[(String, String)],
    ) {
      if let Some(file_name) = &diagnostic.file_name
        && let Some((_, specifier)) = by_suffix
          .iter()
          .find(|(suffix, _)| file_name.replace('\\', "/").ends_with(suffix))
      {
        diagnostic.file_name = Some(specifier.clone());
      }
      if let Some(related) = &mut diagnostic.related_information {
        for diagnostic in related {
          remap(diagnostic, by_suffix);
        }
      }
    }
    for diagnostic in diagnostics {
      remap(diagnostic, &by_suffix);
    }
  }
}

/// Write each code block of `graph` below `<project>/.deno/doc/`, its local
/// imports (relative ones and the absolute `file:` import of the module it
/// documents) rewritten relative to the written file. Only the import's own
/// line is touched, so every other diagnostic position is the block's own.
fn materialize_code_blocks(
  project_root: &Path,
  graph: &ModuleGraph,
) -> Result<CodeBlocks, AnyError> {
  let parent = project_root.join(".deno").join("doc");
  std::fs::create_dir_all(&parent)?;
  let dir = tempfile::Builder::new()
    .prefix("check-")
    .tempdir_in(&parent)?;
  let mut files = Vec::new();
  for (index, js) in code_block_modules(graph).enumerate() {
    let fragment = js.specifier.fragment().unwrap_or_default();
    let name = format!(
      "{index}_{}",
      fragment.replace(
        |c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'),
        "_"
      )
    );
    let path = dir.path().join(name);
    std::fs::write(&path, rewrite_local_imports(js, dir.path()))?;
    files.push((path, js.specifier.clone()));
  }
  Ok(CodeBlocks { dir, files })
}

/// The block's source with each local import (`./x.ts`, `../x.ts`,
/// `file:///a/x.ts`) pointing at the same file from `dir`.
fn rewrite_local_imports(js: &deno_graph::JsModule, dir: &Path) -> String {
  let mut lines = js
    .source
    .text
    .split('\n')
    .map(str::to_string)
    .collect::<Vec<_>>();
  for (raw, dependency) in &js.dependencies {
    if !(raw.starts_with("./")
      || raw.starts_with("../")
      || raw.starts_with("file:"))
    {
      continue;
    }
    let Some(target) = dependency
      .maybe_code
      .maybe_specifier()
      .or_else(|| dependency.maybe_type.maybe_specifier())
      .filter(|specifier| specifier.scheme() == "file")
      .and_then(|specifier| specifier.to_file_path().ok())
    else {
      continue;
    };
    let Some(relative) = pathdiff::diff_paths(&target, dir) else {
      continue;
    };
    let mut relative = relative.to_string_lossy().replace('\\', "/");
    if !relative.starts_with("../") {
      relative = format!("./{relative}");
    }
    for import in &dependency.imports {
      let Some(line) = lines.get_mut(import.specifier_range.range.start.line)
      else {
        continue;
      };
      for quote in ['"', '\''] {
        *line = line.replace(
          &format!("{quote}{raw}{quote}"),
          &format!("{quote}{relative}{quote}"),
        );
      }
    }
  }
  lines.join("\n")
}

/// The in-isolate TypeScript 6.0.3 checker (`--check-engine=tsc`), and
/// `--doc` / `--doc-only` with either engine: their graph check goes through
/// [`TypeChecker::check_graph`], which picks it.
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
    .await?;
  log_checked_summary(specifiers_for_typecheck.len());
  Ok(())
}

/// denext: `deno check`'s closing summary, like `deno fmt`'s and `deno
/// lint`'s, counting the roots it checked: printed after a clean check (the
/// type-checked files may be cached), on either engine; nothing when no file
/// matched, and nothing under `--quiet` (denoland/deno#35739).
fn log_checked_summary(roots: usize) {
  if let Some(summary) = checked_summary(roots) {
    log::info!("{summary}");
  }
}

fn checked_summary(roots: usize) -> Option<String> {
  match roots {
    0 => None,
    1 => Some("Checked 1 file".to_string()),
    n => Some(format!("Checked {n} files")),
  }
}

#[cfg(test)]
mod tests {
  use deno_core::serde_json;
  use deno_core::serde_json::json;

  use super::*;

  #[test]
  fn checked_summary_counts_the_roots() {
    assert_eq!(checked_summary(0), None);
    assert_eq!(checked_summary(1).as_deref(), Some("Checked 1 file"));
    assert_eq!(checked_summary(3).as_deref(), Some("Checked 3 files"));
  }

  #[test]
  fn generated_project_is_reused_only_unchanged() {
    let project = tempfile::tempdir().unwrap();
    let generated = project.path().join(".deno");
    std::fs::create_dir_all(generated.join("types").join("deno")).unwrap();
    let config = json!({
      "compilerOptions": {
        "typeRoots": ["./types"],
        "paths": {
          "lib/*": ["../lib/*"],
          "dep": ["./types/deno/index.d.ts"],
        },
      },
    });
    std::fs::write(generated.join("types/deno/index.d.ts"), "").unwrap();
    std::fs::write(
      generated.join("tsconfig.json"),
      serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    // never recorded
    assert!(!generated_project_is_current(&generated, "inputs"));
    record_generated_project(&generated, "inputs");
    assert!(generated_project_is_current(&generated, "inputs"));
    // other inputs
    assert!(!generated_project_is_current(&generated, "other"));
    // generated again since (by another check, the language server, ...)
    std::fs::write(generated.join("tsconfig.json"), "{}").unwrap();
    assert!(!generated_project_is_current(&generated, "inputs"));
    // a file it maps went away
    std::fs::write(
      generated.join("tsconfig.json"),
      serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    record_generated_project(&generated, "inputs");
    assert!(generated_project_is_current(&generated, "inputs"));
    std::fs::remove_file(generated.join("types/deno/index.d.ts")).unwrap();
    assert!(!generated_project_is_current(&generated, "inputs"));
  }

  #[test]
  fn generated_targets_outside_the_project_must_exist() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("app");
    let generated = project.join(".deno");
    std::fs::create_dir_all(&generated).unwrap();
    let cache = root.path().join("cache");
    std::fs::create_dir_all(cache.join("pkg")).unwrap();
    std::fs::write(cache.join("pkg").join("index.d.ts"), "").unwrap();
    let config = |target: &std::path::Path| {
      let target = target.to_string_lossy().replace('\\', "/");
      serde_json::to_vec(&json!({
        "compilerOptions": { "paths": { "pkg": [target], "@/*": ["../src/*"] } },
        "files": [],
      }))
      .unwrap()
    };
    // the project's own targets are the user's (`../src/` need not exist)
    assert!(generated_targets_exist(
      &generated,
      &config(&cache.join("pkg").join("index.d.ts"))
    ));
    assert!(!generated_targets_exist(
      &generated,
      &config(&cache.join("gone").join("index.d.ts"))
    ));
    assert!(!generated_targets_exist(&generated, b"not json"));
  }

  #[test]
  fn code_block_diagnostics_name_the_block() {
    let project = tempfile::tempdir().unwrap();
    let dir = tempfile::Builder::new()
      .prefix("check-")
      .tempdir_in(project.path())
      .unwrap();
    let dir_name = dir
      .path()
      .file_name()
      .unwrap()
      .to_string_lossy()
      .to_string();
    let block = ModuleSpecifier::parse("file:///a/mod.ts#3-7.ts").unwrap();
    let code_blocks = CodeBlocks {
      files: vec![(dir.path().join("0_3-7.ts"), block.clone())],
      dir,
    };
    let in_block = format!("file:///p/.deno/doc/{dir_name}/0_3-7.ts");
    let mut diagnostics: Vec<crate::tsc::Diagnostic> =
      serde_json::from_value(json!([
        {
          "category": 1,
          "code": 2322,
          "fileName": in_block,
          "relatedInformation": [
            { "category": 3, "code": 6500, "fileName": in_block },
            { "category": 3, "code": 6500, "fileName": "file:///a/mod.ts" },
          ],
        },
        { "category": 1, "code": 2322, "fileName": "file:///a/other.ts" },
      ]))
      .unwrap();
    code_blocks.remap_diagnostics(&mut diagnostics);
    assert_eq!(
      diagnostics[0].file_name.as_deref(),
      Some("file:///a/mod.ts#3-7.ts")
    );
    let related = diagnostics[0].related_information.as_ref().unwrap();
    assert_eq!(related[0].file_name.as_deref(), Some(block.as_str()));
    assert_eq!(related[1].file_name.as_deref(), Some("file:///a/mod.ts"));
    assert_eq!(
      diagnostics[1].file_name.as_deref(),
      Some("file:///a/other.ts")
    );
  }
}
