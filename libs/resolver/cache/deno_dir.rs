// Copyright 2018-2026 the Deno authors. MIT license.

use std::env;
use std::path::PathBuf;

use deno_cache_dir::DenoDirResolutionError;
use deno_cache_dir::ResolveDenoDirSys;

use super::DiskCache;
use super::DiskCacheSys;

#[derive(Debug, Clone)]
pub struct DenoDirOptions {
  pub maybe_initial_cwd: Option<PathBuf>,
  pub maybe_custom_root: Option<PathBuf>,
}

#[sys_traits::auto_impl]
pub trait DenoDirSys: DiskCacheSys + ResolveDenoDirSys + Clone {}

#[allow(clippy::disallowed_types, reason = "definition")]
pub type DenoDirProviderRc<TSys> =
  deno_maybe_sync::MaybeArc<DenoDirProvider<TSys>>;

/// Lazily creates the deno dir which might be useful in scenarios
/// where functionality wants to continue if the DENO_DIR can't be created.
pub struct DenoDirProvider<TSys: DenoDirSys> {
  options: DenoDirOptions,
  sys: TSys,
  deno_dir_cell: once_cell::sync::OnceCell<DenoDir<TSys>>,
}

impl<TSys: DenoDirSys> DenoDirProvider<TSys> {
  pub fn new(sys: TSys, options: DenoDirOptions) -> Self {
    Self {
      options,
      sys,
      deno_dir_cell: Default::default(),
    }
  }

  pub fn get_or_create(
    &self,
  ) -> Result<&DenoDir<TSys>, DenoDirResolutionError> {
    self.deno_dir_cell.get_or_try_init(|| {
      let path = deno_cache_dir::resolve_deno_dir(
        &self.sys,
        deno_cache_dir::ResolveDenoDirOptions {
          maybe_custom_root: self.options.maybe_custom_root.as_deref(),
          maybe_initial_cwd: self.options.maybe_initial_cwd.as_deref(),
        },
      )?;
      Ok(DenoDir::new(self.sys.clone(), path.into_owned()))
    })
  }
}

/// denext: the suffix of the DENO_DIR caches this fork keeps apart from stock
/// Deno's. A stock `deno` and this fork share one DENO_DIR (the download
/// caches, `remote/`, `npm/`, `registries/` and the emits in `gen/`, are
/// keyed by content and the `deno_ast` version, so both reuse them), but each
/// of the SQLite caches below is wiped whenever a different CLI version opens
/// it (`CacheDBConfiguration::on_version_change`), so alternating the two
/// binaries threw away each one's type-check, code, fast-check, analysis and
/// fmt/lint incremental caches on every switch. The fork keeps its own copy
/// of each instead (see DENEXT.md, "Caches of their own").
pub const DENEXT_CACHE_SUFFIX: &str = "_denext";

/// `DenoDir` serves as coordinator for multiple `DiskCache`s containing them
/// in single directory that can be controlled with `$DENO_DIR` env variable.
#[derive(Debug, Clone)]
pub struct DenoDir<TSys: DiskCacheSys> {
  /// Example: /Users/rld/.deno/
  pub root: PathBuf,
  /// Used by TsCompiler to cache compiler output.
  pub gen_cache: DiskCache<TSys>,
}

impl<TSys: DiskCacheSys> DenoDir<TSys> {
  pub fn new(sys: TSys, root: PathBuf) -> Self {
    #[cfg(not(target_arch = "wasm32"))]
    assert!(root.is_absolute());

    let gen_path = root.join("gen");
    Self {
      root,
      gen_cache: DiskCache::new(sys, gen_path),
    }
  }

  /// The root directory of the DENO_DIR for display purposes only.
  pub fn root_path_for_display(&self) -> std::path::Display<'_> {
    self.root.display()
  }

  /// The fork's own copy of the version-keyed cache `name`, see
  /// [`DENEXT_CACHE_SUFFIX`].
  fn denext_cache_path(&self, name: &str) -> PathBuf {
    self.root.join(format!("{name}{DENEXT_CACHE_SUFFIX}"))
  }

  /// Path for the V8 code cache.
  pub fn code_cache_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("v8_code_cache_v2")
  }

  /// Path for the incremental cache used for formatting.
  pub fn fmt_incremental_cache_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("fmt_incremental_cache_v2")
  }

  /// Path for the incremental cache used for linting.
  pub fn lint_incremental_cache_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("lint_incremental_cache_v2")
  }

  /// Path for caching swc dependency analysis.
  pub fn dep_analysis_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("dep_analysis_cache_v2")
  }

  /// Path for the cache used for fast check.
  pub fn fast_check_cache_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("fast_check_cache_v2")
  }

  /// Path for caching node analysis.
  pub fn node_analysis_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("node_analysis_cache_v2")
  }

  /// Path for the cache used for type checking.
  pub fn type_checking_cache_db_file_path(&self) -> PathBuf {
    // bump this version name to invalidate the entire cache
    self.denext_cache_path("check_cache_v2")
  }

  /// Path to the registries cache, used for the lps.
  pub fn registries_folder_path(&self) -> PathBuf {
    self.root.join("registries")
  }

  /// Path to the remote cache folder.
  pub fn remote_folder_path(&self) -> PathBuf {
    self.root.join("remote")
  }

  /// Path to the origin data cache folder.
  pub fn origin_data_folder_path(&self) -> PathBuf {
    // TODO(@crowlKats): change to origin_data for 2.0
    self.root.join("location_data")
  }

  /// File used for the upgrade checker.
  pub fn upgrade_check_file_path(&self) -> PathBuf {
    self.root.join("latest.txt")
  }

  /// Folder used for the npm cache.
  pub fn npm_folder_path(&self) -> PathBuf {
    self.root.join("npm")
  }

  /// Path used for the REPL history file.
  /// Can be overridden or disabled by setting `DENO_REPL_HISTORY` environment variable.
  pub fn repl_history_file_path(&self) -> Option<PathBuf> {
    #[allow(
      clippy::disallowed_methods,
      reason = "needs real environment variable"
    )]
    if let Some(deno_repl_history) = env::var_os("DENO_REPL_HISTORY") {
      if deno_repl_history.is_empty() {
        None
      } else {
        Some(PathBuf::from(deno_repl_history))
      }
    } else {
      Some(self.root.join("deno_history.txt"))
    }
  }

  /// Folder path used for downloading new versions of deno.
  pub fn dl_folder_path(&self) -> PathBuf {
    self.root.join("dl")
  }
}

#[cfg(test)]
mod test {
  use sys_traits::impls::RealSys;

  use super::*;

  #[test]
  fn version_keyed_caches_are_the_forks_own() {
    let root = if cfg!(windows) {
      PathBuf::from("C:\\deno_dir")
    } else {
      PathBuf::from("/deno_dir")
    };
    let dir = DenoDir::new(RealSys, root.clone());
    let name = |path: PathBuf| {
      assert_eq!(path.parent(), Some(root.as_path()));
      path.file_name().unwrap().to_string_lossy().into_owned()
    };
    // the SQLite caches a different CLI version wipes
    assert_eq!(
      [
        name(dir.code_cache_db_file_path()),
        name(dir.fmt_incremental_cache_db_file_path()),
        name(dir.lint_incremental_cache_db_file_path()),
        name(dir.dep_analysis_db_file_path()),
        name(dir.fast_check_cache_db_file_path()),
        name(dir.node_analysis_db_file_path()),
        name(dir.type_checking_cache_db_file_path()),
      ],
      [
        "v8_code_cache_v2_denext",
        "fmt_incremental_cache_v2_denext",
        "lint_incremental_cache_v2_denext",
        "dep_analysis_cache_v2_denext",
        "fast_check_cache_v2_denext",
        "node_analysis_cache_v2_denext",
        "check_cache_v2_denext",
      ]
    );
    // the caches keyed by content stay shared with stock Deno
    assert_eq!(dir.gen_cache.location, root.join("gen"));
    assert_eq!(dir.remote_folder_path(), root.join("remote"));
    assert_eq!(dir.npm_folder_path(), root.join("npm"));
    assert_eq!(dir.registries_folder_path(), root.join("registries"));
  }
}
