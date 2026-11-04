// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: the per-user parts of Node's compile cache
//! (`module.enableCompileCache()`) that JavaScript must not see: the default
//! cache directory under the Deno cache directory, and the key that
//! authenticates every cache entry.
//!
//! V8 runs whatever code cache it is given once the source length and flags
//! match, so an entry must come from this user's own runtime compiling that
//! very source. Each entry carries an HMAC-SHA256, under a per-user key kept
//! in `<DENO_DIR>/node_compile_cache.key`, over the module's cache name, the
//! SHA-256 of its source and V8's code cache. The key never reaches
//! JavaScript, so a program that can write a cache directory (say with only
//! `--allow-write=/tmp`) but not read the key can't make an entry another
//! program accepts, and an entry made for one source is refused for any
//! other, even one with the same length and CRC-32.

use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use deno_core::op2;
use hmac::Hmac;
use hmac::Mac;
use sha2::Digest;
use sha2::Sha256;

const KEY_LEN: usize = 32;
const MAC_LEN: usize = 32;
const KEY_FILE: &str = "node_compile_cache.key";
const DEFAULT_DIR: &str = "node_compile_cache";
const MAC_DOMAIN: &[u8] = b"denext node compile cache v1\0";

/// The Deno cache directory, resolved as `deno_cache_dir::resolve_deno_dir`
/// does: `DENO_DIR`, then `$XDG_CACHE_HOME/deno`, the OS cache directory's
/// `deno`, and `~/.deno`.
fn deno_dir() -> Option<PathBuf> {
  use sys_traits::EnvCacheDir;
  use sys_traits::EnvCurrentDir;
  use sys_traits::EnvHomeDir;
  use sys_traits::EnvVar;
  use sys_traits::impls::RealSys;

  let sys = RealSys;
  let root = if let Some(dir) = sys.env_var_path("DENO_DIR") {
    dir
  } else if let Some(dir) = sys.env_var_path("XDG_CACHE_HOME") {
    dir.join("deno")
  } else if let Some(dir) = sys.env_cache_dir() {
    dir.join("deno")
  } else {
    sys.env_home_dir()?.join(".deno")
  };
  if root.is_absolute() {
    Some(root)
  } else {
    #[allow(
      clippy::disallowed_methods,
      reason = "resolves a relative DENO_DIR as the CLI does"
    )]
    let cwd = sys.env_current_dir().ok()?;
    Some(cwd.join(root))
  }
}

/// `<DENO_DIR>/node_compile_cache`: the directory `enableCompileCache()`
/// uses when it is given none and `NODE_COMPILE_CACHE` is unset. Unlike
/// Node's `os.tmpdir()`, a program can only write it when it is granted the
/// Deno cache directory.
#[op2]
#[string]
pub fn op_node_compile_cache_default_dir() -> Option<String> {
  Some(deno_dir()?.join(DEFAULT_DIR).to_string_lossy().into_owned())
}

fn key() -> Option<&'static [u8; KEY_LEN]> {
  static KEY: OnceLock<Option<[u8; KEY_LEN]>> = OnceLock::new();
  KEY
    .get_or_init(|| {
      let dir = deno_dir()?;
      load_or_create_key(&dir).ok()
    })
    .as_ref()
}

#[allow(
  clippy::disallowed_methods,
  reason = "requires real fs: the per-user key lives in the Deno cache \
   directory, outside the program's file system view and permissions"
)]
fn load_or_create_key(deno_dir: &Path) -> std::io::Result<[u8; KEY_LEN]> {
  use std::io::Write;

  create_private_dir(deno_dir)?;
  if let Some(reason) = super::fs::compile_cache_untrusted_dir(deno_dir) {
    return Err(std::io::Error::other(reason));
  }
  let path = deno_dir.join(KEY_FILE);
  match read_key(&path) {
    Ok(key) => return Ok(key),
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
    Err(err) => return Err(err),
  }
  let mut key = [0u8; KEY_LEN];
  {
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut key);
  }
  // Written in full to a private temporary file, then linked into place:
  // the link fails if another process made the key first, and that key is
  // read instead.
  let tmp =
    deno_dir.join(format!("{KEY_FILE}.{}.{:016x}", std::process::id(), {
      use rand::RngCore;
      rand::rngs::OsRng.next_u64()
    }));
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
  }
  let result = (|| {
    let mut file = options.open(&tmp)?;
    file.write_all(&key)?;
    file.sync_all()?;
    drop(file);
    std::fs::hard_link(&tmp, &path)
  })();
  let _ = std::fs::remove_file(&tmp);
  match result {
    Ok(()) => Ok(key),
    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
      read_key(&path)
    }
    Err(err) => Err(err),
  }
}

#[allow(
  clippy::disallowed_methods,
  reason = "requires real fs: see load_or_create_key"
)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
  let mut builder = std::fs::DirBuilder::new();
  builder.recursive(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
  }
  builder.create(dir)
}

/// The key, from a regular file of exactly `KEY_LEN` bytes that the current
/// user owns and nobody else can read or write.
#[allow(
  clippy::disallowed_methods,
  reason = "requires real fs: see load_or_create_key"
)]
fn read_key(path: &Path) -> std::io::Result<[u8; KEY_LEN]> {
  use std::io::Read;

  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
  }
  let mut file = options.open(path)?;
  let metadata = file.metadata()?;
  if !metadata.is_file() || metadata.len() != KEY_LEN as u64 {
    return Err(std::io::Error::other(format!(
      "'{}' is not a {KEY_LEN}-byte file",
      path.display()
    )));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
      return Err(std::io::Error::other(format!(
        "'{}' is not private to the current user",
        path.display()
      )));
    }
  }
  let mut key = [0u8; KEY_LEN];
  file.read_exact(&mut key)?;
  Ok(key)
}

fn entry_mac(
  key: &[u8; KEY_LEN],
  name: &str,
  source: &[u8],
  payload: &[u8],
) -> Hmac<Sha256> {
  let mut mac =
    <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
  mac.update(MAC_DOMAIN);
  mac.update(&(name.len() as u64).to_le_bytes());
  mac.update(name.as_bytes());
  mac.update(&Sha256::digest(source));
  mac.update(&(payload.len() as u64).to_le_bytes());
  mac.update(payload);
  mac
}

/// The MAC a cache entry for `name` (the module's cache name: its type and
/// path, relative in portable mode) with this `source` and V8 `payload`
/// carries. Empty when the per-user key is unavailable, in which case
/// nothing is written.
#[op2]
#[buffer]
pub fn op_node_compile_cache_mac(
  #[string] name: &str,
  #[buffer] source: &[u8],
  #[buffer] payload: &[u8],
) -> Vec<u8> {
  let Some(key) = key() else {
    return Vec::new();
  };
  entry_mac(key, name, source, payload)
    .finalize()
    .into_bytes()
    .to_vec()
}

/// Whether `mac` authenticates a cache entry for `name` with this `source`
/// and V8 `payload` (constant-time). `false` when the key is unavailable.
#[op2(fast)]
pub fn op_node_compile_cache_verify(
  #[string] name: &str,
  #[buffer] source: &[u8],
  #[buffer] payload: &[u8],
  #[buffer] mac: &[u8],
) -> bool {
  let Some(key) = key() else {
    return false;
  };
  mac.len() == MAC_LEN
    && entry_mac(key, name, source, payload)
      .verify_slice(mac)
      .is_ok()
}

/// Whether the per-user key is available, so entries can be authenticated.
#[op2(fast)]
pub fn op_node_compile_cache_key_available() -> bool {
  key().is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mac_binds_name_source_and_payload() {
    let key = [7u8; KEY_LEN];
    let mac = entry_mac(&key, "a.js", b"source", b"payload")
      .finalize()
      .into_bytes();
    assert!(
      entry_mac(&key, "a.js", b"source", b"payload")
        .verify_slice(&mac)
        .is_ok()
    );
    assert!(
      entry_mac(&key, "b.js", b"source", b"payload")
        .verify_slice(&mac)
        .is_err()
    );
    assert!(
      entry_mac(&key, "a.js", b"sourcf", b"payload")
        .verify_slice(&mac)
        .is_err()
    );
    assert!(
      entry_mac(&key, "a.js", b"source", b"payloae")
        .verify_slice(&mac)
        .is_err()
    );
    assert!(
      entry_mac(&[8u8; KEY_LEN], "a.js", b"source", b"payload")
        .verify_slice(&mac)
        .is_err()
    );
  }
}
