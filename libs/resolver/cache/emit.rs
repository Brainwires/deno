// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::PathBuf;

use anyhow::Error as AnyError;
use anyhow::anyhow;
use deno_unsync::sync::AtomicFlag;
use url::Url;

use super::DiskCache;
use super::DiskCacheSys;

#[allow(clippy::disallowed_types, reason = "definition")]
pub type EmitCacheRc<TSys> = deno_maybe_sync::MaybeArc<EmitCache<TSys>>;

#[sys_traits::auto_impl]
pub trait EmitCacheSys: DiskCacheSys + sys_traits::EnvVar {}

/// The cache that stores previously emitted files.
#[derive(Debug)]
pub struct EmitCache<TSys: EmitCacheSys> {
  disk_cache: DiskCache<TSys>,
  emit_failed_flag: AtomicFlag,
  file_serializer: EmitFileSerializer,
  mode: Mode,
}

#[derive(Debug)]
enum Mode {
  Normal,
  Disable,
}

impl<TSys: EmitCacheSys> EmitCache<TSys> {
  pub fn new(
    sys: &TSys,
    disk_cache: DiskCache<TSys>,
    cache_version: Cow<'static, str>,
  ) -> Self {
    let emit_cache_mode = sys.env_var_os("DENO_EMIT_CACHE_MODE");
    let mode = match emit_cache_mode.as_ref().and_then(|s| s.to_str()) {
      Some("normal") | Some("") | None => Mode::Normal,
      Some("disable") => Mode::Disable,
      _ => {
        log::warn!("Unknown DENO_EMIT_CACHE_MODE value, defaulting to normal");
        Mode::Normal
      }
    };

    Self {
      disk_cache,
      emit_failed_flag: Default::default(),
      file_serializer: EmitFileSerializer { cache_version },
      mode,
    }
  }

  /// Gets the emitted code with embedded sourcemap from the cache.
  ///
  /// The expected source hash is used in order to verify
  /// that you're getting a value from the cache that is
  /// for the provided source.
  ///
  /// Cached emits from previous CLI releases will not be returned
  /// or emits that do not match the source.
  ///
  /// `name` is the options' cache directory (see
  /// `TranspileAndEmitOptions::emit_cache_name`): `None` for a module's usual
  /// options, kept in the cache's root.
  pub fn get_emit_code(
    &self,
    specifier: &Url,
    name: Option<&str>,
    expected_source_hash: u64,
  ) -> Option<String> {
    self.get_emit_code_of(
      specifier,
      EmitForm::Plain,
      name,
      Some(expected_source_hash),
    )
  }

  /// Like [`Self::get_emit_code`], for the emit of a module that a program
  /// transform changed (`deno test`'s Vitest compatibility), which is cached
  /// apart from the module's plain emit: a run with the transform and one
  /// without it each keep theirs, so `deno coverage` can read the profiles of
  /// both. It stays in the cache's root whatever the options, where
  /// `deno coverage` finds it without them.
  pub fn get_transformed_emit_code(
    &self,
    specifier: &Url,
    expected_source_hash: u64,
  ) -> Option<String> {
    self.get_emit_code_of(
      specifier,
      EmitForm::Transformed,
      None,
      Some(expected_source_hash),
    )
  }

  /// The emit of `specifier` that a program transform changed, whatever
  /// source and options it was emitted from (only checked to be intact and of
  /// this CLI version). The caller must check that it belongs to the module's
  /// current source: `deno coverage` does, through its source map, to find the
  /// emit of a module that a program transform it cannot reproduce changed
  /// (the source hash covers the transform's key).
  pub fn get_transformed_emit_code_any_source_hash(
    &self,
    specifier: &Url,
  ) -> Option<String> {
    self.get_emit_code_of(specifier, EmitForm::Transformed, None, None)
  }

  fn get_emit_code_of(
    &self,
    specifier: &Url,
    form: EmitForm,
    name: Option<&str>,
    expected_source_hash: Option<u64>,
  ) -> Option<String> {
    if matches!(self.mode, Mode::Disable) {
      return None;
    }

    let emit_filename = self.get_emit_filename(specifier, form, name)?;
    let bytes = self.disk_cache.get(&emit_filename).ok()?;
    self
      .file_serializer
      .deserialize(bytes, expected_source_hash)
  }

  /// Sets the emit code in the cache (`name` as in [`Self::get_emit_code`]).
  pub fn set_emit_code(
    &self,
    specifier: &Url,
    name: Option<&str>,
    source_hash: u64,
    code: &[u8],
  ) {
    self.set_emit_code_of(specifier, EmitForm::Plain, name, source_hash, code)
  }

  /// Sets the emit code of a module that a program transform changed (see
  /// [`Self::get_transformed_emit_code`]).
  pub fn set_transformed_emit_code(
    &self,
    specifier: &Url,
    source_hash: u64,
    code: &[u8],
  ) {
    self.set_emit_code_of(
      specifier,
      EmitForm::Transformed,
      None,
      source_hash,
      code,
    )
  }

  fn set_emit_code_of(
    &self,
    specifier: &Url,
    form: EmitForm,
    name: Option<&str>,
    source_hash: u64,
    code: &[u8],
  ) {
    if let Err(err) =
      self.set_emit_code_result(specifier, form, name, source_hash, code)
    {
      // might error in cases such as a readonly file system
      log::debug!("Error saving emit data ({}): {}", specifier, err);
      // assume the cache can't be written to and disable caching to it
      self.emit_failed_flag.raise();
    }
  }

  fn set_emit_code_result(
    &self,
    specifier: &Url,
    form: EmitForm,
    name: Option<&str>,
    source_hash: u64,
    code: &[u8],
  ) -> Result<(), AnyError> {
    if matches!(self.mode, Mode::Disable) || self.emit_failed_flag.is_raised() {
      log::debug!("Skipped emit cache save of {}", specifier);
      return Ok(());
    }

    let emit_filename = self
      .get_emit_filename(specifier, form, name)
      .ok_or_else(|| anyhow!("Could not get emit filename."))?;
    let cache_data = self.file_serializer.serialize(code, source_hash);
    self.disk_cache.set(&emit_filename, &cache_data)?;

    Ok(())
  }

  fn get_emit_filename(
    &self,
    specifier: &Url,
    form: EmitForm,
    name: Option<&str>,
  ) -> Option<PathBuf> {
    let extension = match form {
      EmitForm::Plain => "js",
      EmitForm::Transformed => "transformed.js",
    };
    let filename = self
      .disk_cache
      .get_cache_filename_with_extension(specifier, extension)?;
    // denext: emits made with other options than a module's usual ones live
    // in a sub-directory named for them (denoland/deno#30791). The usual
    // ones stay where they always were.
    match name {
      Some(name) if is_valid_cache_dir_name(name) => {
        Some(PathBuf::from(name).join(filename))
      }
      Some(_) => None,
      None => Some(filename),
    }
  }
}

/// A cache directory name is one plain path segment (letters, digits, `-`,
/// `_`, `+` and `.`, not only dots), so it can't leave the cache.
fn is_valid_cache_dir_name(name: &str) -> bool {
  !name.is_empty()
    && name.chars().any(|c| c != '.')
    && name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'))
}

/// Which emit of a module a cache entry holds: there is one entry for each.
#[derive(Debug, Clone, Copy)]
enum EmitForm {
  /// The module's usual emit.
  Plain,
  /// The emit of a module that a program transform changed.
  Transformed,
}

const LAST_LINE_PREFIX: &str = "\n// denoCacheMetadata=";

#[derive(Debug)]
struct EmitFileSerializer {
  cache_version: Cow<'static, str>,
}

impl EmitFileSerializer {
  pub fn deserialize(
    &self,
    mut bytes: Vec<u8>,
    expected_source_hash: Option<u64>,
  ) -> Option<String> {
    let last_newline_index = bytes.iter().rposition(|&b| b == b'\n')?;
    let (content, last_line) = bytes.split_at(last_newline_index);
    let hashes = last_line.strip_prefix(LAST_LINE_PREFIX.as_bytes())?;
    let hashes = String::from_utf8_lossy(hashes);
    let (source_hash, emit_hash) = hashes.split_once(',')?;

    // verify the meta data file is for this source and CLI version
    let source_hash = source_hash.parse::<u64>().ok()?;
    if expected_source_hash.is_some_and(|expected| source_hash != expected) {
      return None;
    }
    let emit_hash = emit_hash.parse::<u64>().ok()?;
    // prevent using an emit from a different cli version or emits that were tampered with
    if emit_hash != self.compute_emit_hash(content) {
      return None;
    }

    // everything looks good, truncate and return it
    bytes.truncate(content.len());
    String::from_utf8(bytes).ok()
  }

  pub fn serialize(&self, code: &[u8], source_hash: u64) -> Vec<u8> {
    let source_hash = source_hash.to_string();
    let emit_hash = self.compute_emit_hash(code).to_string();
    let capacity = code.len()
      + LAST_LINE_PREFIX.len()
      + source_hash.len()
      + 1
      + emit_hash.len();
    let mut cache_data = Vec::with_capacity(capacity);
    cache_data.extend(code);
    cache_data.extend(LAST_LINE_PREFIX.as_bytes());
    cache_data.extend(source_hash.as_bytes());
    cache_data.push(b',');
    cache_data.extend(emit_hash.as_bytes());
    debug_assert_eq!(cache_data.len(), capacity);
    cache_data
  }

  fn compute_emit_hash(&self, bytes: &[u8]) -> u64 {
    // it's ok to use an insecure hash here because
    // if someone can change the emit source then they
    // can also change the version hash
    let mut hasher = twox_hash::XxHash64::default();
    bytes.hash(&mut hasher);
    // emit should not be re-used between cli versions
    self.cache_version.hash(&mut hasher);
    hasher.finish()
  }
}

#[cfg(test)]
mod test {
  use test_util::TempDir;

  use super::*;

  #[test]
  pub fn emit_cache_general_use() {
    let temp_dir = TempDir::new();
    let disk_cache =
      DiskCache::new(sys_traits::impls::RealSys, temp_dir.path().to_path_buf());
    let cache = EmitCache {
      disk_cache: disk_cache.clone(),
      file_serializer: EmitFileSerializer {
        cache_version: "1.0.0".into(),
      },
      emit_failed_flag: Default::default(),
      mode: Mode::Normal,
    };

    let specifier1 = deno_path_util::url_from_file_path(
      temp_dir.path().join("file1.ts").as_path(),
    )
    .unwrap();
    let specifier2 = deno_path_util::url_from_file_path(
      temp_dir.path().join("file2.ts").as_path(),
    )
    .unwrap();
    assert_eq!(cache.get_emit_code(&specifier1, None, 1), None);
    let emit_code1 = "text1".to_string();
    let emit_code2 = "text2".to_string();
    cache.set_emit_code(&specifier1, None, 10, emit_code1.as_bytes());
    cache.set_emit_code(&specifier2, None, 2, emit_code2.as_bytes());
    // providing the incorrect source hash
    assert_eq!(cache.get_emit_code(&specifier1, None, 5), None);
    // providing the correct source hash
    assert_eq!(
      cache.get_emit_code(&specifier1, None, 10),
      Some(emit_code1.clone()),
    );
    assert_eq!(cache.get_emit_code(&specifier2, None, 2), Some(emit_code2));

    // try changing the cli version (should not load previous ones)
    let cache = EmitCache {
      disk_cache: disk_cache.clone(),
      file_serializer: EmitFileSerializer {
        cache_version: "2.0.0".into(),
      },
      emit_failed_flag: Default::default(),
      mode: Mode::Normal,
    };
    assert_eq!(cache.get_emit_code(&specifier1, None, 10), None);
    cache.set_emit_code(&specifier1, None, 5, emit_code1.as_bytes());

    // recreating the cache should still load the data because the CLI version is the same
    let cache = EmitCache {
      disk_cache,
      file_serializer: EmitFileSerializer {
        cache_version: "2.0.0".into(),
      },
      emit_failed_flag: Default::default(),
      mode: Mode::Normal,
    };
    assert_eq!(cache.get_emit_code(&specifier1, None, 5), Some(emit_code1));

    // adding when already exists should not cause issue
    let emit_code3 = "asdf".to_string();
    cache.set_emit_code(&specifier1, None, 20, emit_code3.as_bytes());
    assert_eq!(cache.get_emit_code(&specifier1, None, 5), None);
    assert_eq!(
      cache.get_emit_code(&specifier1, None, 20),
      Some(emit_code3.clone())
    );

    // a transformed emit is kept apart from the plain one
    assert_eq!(cache.get_transformed_emit_code(&specifier1, 20), None);
    assert_eq!(
      cache.get_transformed_emit_code_any_source_hash(&specifier1),
      None
    );
    let transformed_code = "transformed".to_string();
    cache.set_transformed_emit_code(
      &specifier1,
      30,
      transformed_code.as_bytes(),
    );
    assert_eq!(
      cache.get_emit_code(&specifier1, None, 20),
      Some(emit_code3.clone())
    );
    assert_eq!(cache.get_emit_code(&specifier1, None, 30), None);
    assert_eq!(
      cache.get_transformed_emit_code(&specifier1, 30),
      Some(transformed_code.clone())
    );
    assert_eq!(cache.get_transformed_emit_code(&specifier1, 20), None);
    // and a later plain emit leaves it in place
    cache.set_emit_code(&specifier1, None, 40, emit_code3.as_bytes());
    // whatever the source hash
    assert_eq!(
      cache.get_transformed_emit_code_any_source_hash(&specifier1),
      Some(transformed_code)
    );
    let specifier3 = deno_path_util::url_from_file_path(
      temp_dir.path().join("file3.ts").as_path(),
    )
    .unwrap();
    assert_eq!(
      cache.get_transformed_emit_code_any_source_hash(&specifier3),
      None
    );
    // but still only for this CLI version
    let cache = EmitCache {
      disk_cache: cache.disk_cache.clone(),
      file_serializer: EmitFileSerializer {
        cache_version: "3.0.0".into(),
      },
      emit_failed_flag: Default::default(),
      mode: Mode::Normal,
    };
    assert_eq!(
      cache.get_transformed_emit_code_any_source_hash(&specifier1),
      None
    );
  }

  // denext: emits made with other options than a module's usual ones are
  // kept in a named sub-directory, apart from the usual ones and from each
  // other (denoland/deno#30791).
  #[test]
  pub fn emit_cache_named_dirs() {
    let temp_dir = TempDir::new();
    let disk_cache =
      DiskCache::new(sys_traits::impls::RealSys, temp_dir.path().to_path_buf());
    let cache = EmitCache {
      disk_cache,
      file_serializer: EmitFileSerializer {
        cache_version: "1.0.0".into(),
      },
      emit_failed_flag: Default::default(),
      mode: Mode::Normal,
    };
    let specifier = deno_path_util::url_from_file_path(
      temp_dir.path().join("mod.ts").as_path(),
    )
    .unwrap();

    cache.set_emit_code(&specifier, None, 1, b"usual");
    cache.set_emit_code(&specifier, Some("vitest"), 2, b"vitest");
    cache.set_emit_code(
      &specifier,
      Some("no-verbatim-module-syntax"),
      3,
      b"no verbatim",
    );
    cache.set_transformed_emit_code(&specifier, 4, b"mocked");
    // Each keeps its own entry: none overwrote another.
    assert_eq!(cache.get_emit_code(&specifier, None, 1).unwrap(), "usual");
    assert_eq!(
      cache.get_emit_code(&specifier, Some("vitest"), 2).unwrap(),
      "vitest"
    );
    assert_eq!(
      cache
        .get_emit_code(&specifier, Some("no-verbatim-module-syntax"), 3)
        .unwrap(),
      "no verbatim"
    );
    assert_eq!(
      cache.get_transformed_emit_code(&specifier, 4).unwrap(),
      "mocked"
    );
    assert_eq!(cache.get_emit_code(&specifier, Some("other"), 1), None);

    // Laid out under readable names, the usual emit where it always was.
    let usual = cache
      .get_emit_filename(&specifier, EmitForm::Plain, None)
      .unwrap();
    assert!(!usual.starts_with("vitest"));
    assert!(temp_dir.path().join(&usual).is_file());
    let named = cache
      .get_emit_filename(&specifier, EmitForm::Plain, Some("vitest"))
      .unwrap();
    assert_eq!(named, PathBuf::from("vitest").join(&usual));
    assert!(temp_dir.path().join(&named).is_file());
    assert!(
      temp_dir
        .path()
        .join("no-verbatim-module-syntax")
        .join(&usual)
        .is_file()
    );

    // A name is one path segment.
    for bad in ["", "..", ".", "a/b", "a\\b", "/abs"] {
      assert_eq!(
        cache.get_emit_filename(&specifier, EmitForm::Plain, Some(bad)),
        None,
        "{bad}"
      );
      cache.set_emit_code(&specifier, Some(bad), 9, b"x");
      assert_eq!(cache.get_emit_code(&specifier, Some(bad), 9), None);
    }
  }
}
