// Copyright 2018-2026 the Deno authors. MIT license.

use std::hash::Hasher;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

/// A fast hasher for computing a combined hash of file contents.
/// Uses xxhash64 for speed.
pub struct InputHasher(twox_hash::XxHash64);

impl InputHasher {
  pub fn new_with_cli_args() -> Self {
    let mut hasher = Self(Default::default());
    for arg in std::env::args() {
      hasher.0.write(arg.as_bytes());
    }
    hasher
  }

  /// Hash a single file's contents. Skips if file doesn't exist.
  pub fn hash_file(&mut self, path: impl AsRef<Path>) -> &mut Self {
    if let Ok(mut file) = std::fs::File::open(path.as_ref()) {
      self.hash_reader(&mut file);
    }
    self
  }

  /// Recursively hash all file contents in a directory (sorted for
  /// determinism). Skips if directory doesn't exist.
  pub fn hash_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
    let path = path.as_ref();
    let mut entries = Vec::new();
    collect_entries_recursive(path, &mut entries);
    entries.sort();
    for entry_path in &entries {
      if let Ok(rel) = entry_path.strip_prefix(path) {
        self.0.write(rel.as_os_str().as_encoded_bytes());
      }
      if let Ok(mut file) = std::fs::File::open(entry_path) {
        self.hash_reader(&mut file);
      }
    }
    self
  }

  fn hash_reader(&mut self, reader: &mut impl Read) {
    let mut buf = [0u8; 8192];
    loop {
      match reader.read(&mut buf) {
        Ok(0) | Err(_) => break,
        Ok(n) => self.0.write(&buf[..n]),
      }
    }
  }

  pub fn finish(&self) -> u64 {
    self.0.finish()
  }
}

fn collect_entries_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
  let entries = match std::fs::read_dir(dir) {
    Ok(entries) => entries,
    Err(_) => return,
  };
  for entry in entries.flatten() {
    let file_type = match entry.file_type() {
      Ok(ft) => ft,
      Err(_) => continue,
    };
    if file_type.is_dir() {
      collect_entries_recursive(&entry.path(), out);
    } else {
      out.push(entry.path());
    }
  }
}

pub enum CiHashStatus {
  /// Not on CI — always run tests.
  Run,
  /// Hash found in hashy — skip tests.
  Skip,
  /// Hash not found — run tests, then call `commit()` on success.
  RunThenCommit(CiHashPending),
}

pub struct CiHashPending {
  key: String,
}

impl CiHashPending {
  /// Mark the hash as known-good after tests pass. A no-op in this fork:
  /// results are never sent to upstream's hashy service (see
  /// `check_ci_hash`).
  pub fn commit(self) {
    let _ = self.key;
  }
}

/// Upstream asks Deno's hashy service here whether tests can be skipped. This
/// fork never does: it always returns [`CiHashStatus::Run`].
///
/// `name` is used for the hash key and log messages (e.g. "specs",
/// "unit"). `configure` receives an `InputHasher` to add whatever files/dirs
/// are relevant.
///
/// Returns a `CiHashStatus` indicating whether to skip, run, or
/// run-then-commit.
///
/// ```ignore
/// let ci_hash = test_util::hash::check_ci_hash("specs", |hasher| {
///     hasher
///         .hash_dir(tests.join("specs"))
///         .hash_file(deno_exe_path());
/// });
/// if matches!(ci_hash, CiHashStatus::Skip) {
///     return;
/// }
/// // ... run tests ...
/// if let CiHashStatus::RunThenCommit(pending) = ci_hash {
///     pending.commit();
/// }
/// ```
pub fn check_ci_hash(
  _name: &str,
  _configure: impl FnOnce(&mut InputHasher),
) -> CiHashStatus {
  // denext fork: never consult or seed upstream Deno's hashy service
  // (hashy.deno.deno.net). A hit there would skip our tests on the strength
  // of someone else's cache, and a commit would send our input hashes to an
  // outside server. Every suite always runs.
  CiHashStatus::Run
}
