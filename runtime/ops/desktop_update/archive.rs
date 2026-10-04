// Copyright 2018-2026 the Deno authors. MIT license.

//! The download sink and the safe extractor for an update archive.
//!
//! [`DownloadSink`] writes the archive as it streams in, refusing a byte past
//! the size the signed manifest declares, and hashes it; nothing reads the
//! file until [`DownloadSink::finish`] has matched both size and SHA-256.
//!
//! [`extract_tar_gz`] unpacks the verified `.tar.gz` into a FRESH directory
//! with the same rules as denext's runtime downloader
//! (`src/build/safe-extract.ts`), since the archive is still hostile input:
//!
//! - every entry path is relative and normalized: no absolute path, drive
//!   letter, backslash, `:`, NUL or `..` segment (tar-slip);
//! - only regular files, directories and symlinks (pax global headers are
//!   skipped); a hard link, device, FIFO or anything else is refused;
//! - a symlink must be relative and resolve INSIDE the app (the archive's
//!   top-level entry, not merely the extraction directory, which also holds
//!   the download and is not swapped in with the app); symlinks
//!   are created LAST, after every file, and never under another symlink, so
//!   no entry is written through a link; a dangling one is refused; on
//!   Windows an archive with a symlink is refused;
//! - a duplicate entry path is refused, and files are created with
//!   `create_new` (never opened through an existing path);
//! - file modes are masked to `0o755` (no setuid / setgid / sticky, nothing
//!   group- or world-writable: another local user must not be able to change
//!   the installed app) and always readable and writable by the owner;
//!   directories get the process default (the archive's are ignored);
//! - the total extracted size and the entry count are capped.
//!
//! The archive must hold exactly one top-level entry: the app (`<App>.app/`,
//! `<App>/`, or a single `<App>.AppImage` file).

#![allow(
  clippy::disallowed_methods,
  reason = "the updater stages files next to the app install, outside any \
            user permission sandbox, by design"
)]

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;

/// Entries an archive may hold.
pub const MAX_ENTRIES: usize = 200_000;
/// The uncompressed bytes an archive may expand to: 32x the compressed size
/// plus 64 MiB, never more than this.
pub const MAX_EXTRACTED_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// Streams the archive to disk with a hard size cap and a running SHA-256.
pub struct DownloadSink {
  file: Option<File>,
  path: PathBuf,
  hasher: sha2::Sha256,
  written: u64,
  expected_size: u64,
  expected_sha256: String,
}

impl DownloadSink {
  /// Create `path` (which must not exist) for an archive of exactly
  /// `expected_size` bytes hashing to `expected_sha256`.
  pub fn create(
    path: PathBuf,
    expected_size: u64,
    expected_sha256: &str,
  ) -> Result<Self, UpdateError> {
    let file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&path)
      .map_err(|e| UpdateError::io(path.display(), e))?;
    Ok(Self {
      file: Some(file),
      path,
      hasher: sha2::Sha256::new(),
      written: 0,
      expected_size,
      expected_sha256: expected_sha256.to_string(),
    })
  }

  /// Bytes written so far.
  pub fn written(&self) -> u64 {
    self.written
  }

  /// Append `chunk`. A chunk that would take the file past the declared size
  /// is refused (and nothing of it is written): the caller must abort.
  pub fn write(&mut self, chunk: &[u8]) -> Result<(), UpdateError> {
    let Some(file) = self.file.as_mut() else {
      return err(Code::Io, "the download is closed");
    };
    let next = self.written.saturating_add(chunk.len() as u64);
    if next > self.expected_size {
      return err(
        Code::SizeExceeded,
        format!(
          "the download is larger than the {} bytes the manifest declares",
          self.expected_size
        ),
      );
    }
    file
      .write_all(chunk)
      .map_err(|e| UpdateError::io(self.path.display(), e))?;
    self.hasher.update(chunk);
    self.written = next;
    Ok(())
  }

  /// Close the file and check size and SHA-256. On success the archive at
  /// [`Self::path`] is the one the signed manifest names.
  pub fn finish(mut self) -> Result<PathBuf, UpdateError> {
    let Some(mut file) = self.file.take() else {
      return err(Code::Io, "the download is closed");
    };
    file
      .flush()
      .and_then(|_| file.sync_all())
      .map_err(|e| UpdateError::io(self.path.display(), e))?;
    drop(file);
    if self.written != self.expected_size {
      return err(
        Code::Integrity,
        format!(
          "the download is {} bytes, the manifest declares {}",
          self.written, self.expected_size
        ),
      );
    }
    let actual = faster_hex::hex_string(&self.hasher.clone().finalize());
    if actual != self.expected_sha256 {
      return err(
        Code::Integrity,
        format!(
          "SHA-256 mismatch: the manifest declares {}, the download is {actual}",
          self.expected_sha256
        ),
      );
    }
    Ok(self.path.clone())
  }
}

/// Validate and normalize an archive entry path (see the module docs).
/// Returns the normalized `/`-joined segments, or an empty vec for the root.
pub fn safe_entry_path(raw: &[u8]) -> Result<Vec<String>, UpdateError> {
  let Ok(raw) = std::str::from_utf8(raw) else {
    return err(Code::UnsafeArchive, "entry name is not UTF-8");
  };
  if raw.contains('\0') {
    return err(
      Code::UnsafeArchive,
      format!("entry name contains NUL: {raw:?}"),
    );
  }
  if raw.contains('\\') {
    return err(
      Code::UnsafeArchive,
      format!("entry name contains a backslash: {raw}"),
    );
  }
  if raw.starts_with('/') {
    return err(Code::UnsafeArchive, format!("absolute entry path: {raw}"));
  }
  if raw.contains(':') {
    return err(
      Code::UnsafeArchive,
      format!("entry name contains ':' (drive letter or stream): {raw}"),
    );
  }
  let mut parts = Vec::new();
  for seg in raw.split('/') {
    match seg {
      "" | "." => continue,
      ".." => {
        return err(
          Code::UnsafeArchive,
          format!("entry path escapes the destination: {raw}"),
        );
      }
      s => parts.push(s.to_string()),
    }
  }
  Ok(parts)
}

/// Whether a symlink at `link` (normalized segments) pointing at `target`
/// stays inside the archive's top-level entry (`link[0]`), resolved
/// lexically from the link's directory. A target that resolves to the
/// top-level entry itself or outside it (a sibling next to the app in the
/// extraction directory, the directory itself) is refused.
fn symlink_stays_inside(link: &[String], target: &str) -> bool {
  if target.is_empty()
    || target.starts_with('/')
    || target.contains('\\')
    || target.contains('\0')
    || target.contains(':')
  {
    return false;
  }
  let mut stack: Vec<&str> = link[..link.len().saturating_sub(1)]
    .iter()
    .map(|s| s.as_str())
    .collect();
  for seg in target.split('/') {
    match seg {
      "" | "." => {}
      ".." => {
        if stack.pop().is_none() {
          return false;
        }
      }
      s => stack.push(s),
    }
  }
  stack.len() > 1 && link.first().is_some_and(|top| stack[0] == top.as_str())
}

fn join(root: &Path, parts: &[String]) -> PathBuf {
  let mut p = root.to_path_buf();
  for s in parts {
    p.push(s);
  }
  p
}

/// Refuse if any ancestor of `parts` under `root` is not a real directory.
fn ensure_real_parents(
  root: &Path,
  parts: &[String],
) -> Result<(), UpdateError> {
  let mut p = root.to_path_buf();
  for s in &parts[..parts.len().saturating_sub(1)] {
    p.push(s);
    match std::fs::symlink_metadata(&p) {
      Ok(m) if m.file_type().is_dir() => {}
      Ok(_) => {
        return err(
          Code::UnsafeArchive,
          format!("{} sits under a non-directory", parts.join("/")),
        );
      }
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
        std::fs::create_dir(&p).map_err(|e| UpdateError::io(p.display(), e))?;
      }
      Err(e) => return Err(UpdateError::io(p.display(), e)),
    }
  }
  Ok(())
}

/// What [`extract_tar_gz`] found at the top level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
  /// The single top-level entry's name.
  pub top: String,
  /// Whether it is a directory (else a regular file).
  pub top_is_dir: bool,
  /// Regular files written.
  pub files: usize,
}

/// Extract the `.tar.gz` at `archive` into `dest` (which must NOT exist; it
/// is created). See the module docs for the rules. `max_bytes` caps the
/// total uncompressed size.
pub fn extract_tar_gz(
  archive: &Path,
  dest: &Path,
  max_bytes: u64,
) -> Result<Extracted, UpdateError> {
  std::fs::create_dir(dest).map_err(|e| UpdateError::io(dest.display(), e))?;
  let file =
    File::open(archive).map_err(|e| UpdateError::io(archive.display(), e))?;
  let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
  let mut tar = tar::Archive::new(gz);
  let entries = tar
    .entries()
    .map_err(|e| UpdateError::new(Code::UnsafeArchive, e.to_string()))?;

  let mut seen: HashSet<Vec<String>> = HashSet::new();
  let mut tops: HashSet<String> = HashSet::new();
  let mut top_is_dir: Option<bool> = None;
  let mut symlinks: Vec<(Vec<String>, String)> = Vec::new();
  let mut total: u64 = 0;
  let mut count = 0usize;
  let mut files = 0usize;

  for entry in entries {
    let mut entry = entry
      .map_err(|e| UpdateError::new(Code::UnsafeArchive, e.to_string()))?;
    let kind = entry.header().entry_type();
    if kind == tar::EntryType::XGlobalHeader {
      continue;
    }
    count += 1;
    if count > MAX_ENTRIES {
      return err(
        Code::UnsafeArchive,
        format!("the archive has more than {MAX_ENTRIES} entries"),
      );
    }
    let parts = safe_entry_path(&entry.path_bytes())?;
    if parts.is_empty() {
      if kind == tar::EntryType::Directory {
        continue; // `./`
      }
      return err(Code::UnsafeArchive, "an entry names the archive root");
    }
    if !seen.insert(parts.clone()) {
      return err(
        Code::UnsafeArchive,
        format!("duplicate archive entry: {}", parts.join("/")),
      );
    }
    tops.insert(parts[0].clone());
    if tops.len() > 1 {
      return err(
        Code::UnsafeArchive,
        "the archive must hold exactly one top-level entry (the app)",
      );
    }
    let is_top = parts.len() == 1;
    let path = join(dest, &parts);
    match kind {
      tar::EntryType::Directory => {
        if is_top {
          top_is_dir = Some(true);
        }
        ensure_real_parents(dest, &parts)?;
        match std::fs::symlink_metadata(&path) {
          Ok(m) if m.file_type().is_dir() => {}
          Ok(_) => {
            return err(
              Code::UnsafeArchive,
              format!("{} is not a directory", parts.join("/")),
            );
          }
          Err(_) => std::fs::create_dir(&path)
            .map_err(|e| UpdateError::io(path.display(), e))?,
        }
      }
      tar::EntryType::Regular | tar::EntryType::Continuous => {
        if is_top {
          top_is_dir = Some(false);
        }
        let size = entry.header().size().map_err(|e| {
          UpdateError::new(Code::UnsafeArchive, format!("bad size: {e}"))
        })?;
        total = total.saturating_add(size);
        if total > max_bytes {
          return err(
            Code::UnsafeArchive,
            format!("the archive expands past {max_bytes} bytes"),
          );
        }
        ensure_real_parents(dest, &parts)?;
        let mut out = std::fs::OpenOptions::new()
          .write(true)
          .create_new(true)
          .open(&path)
          .map_err(|e| UpdateError::io(path.display(), e))?;
        let copied = std::io::copy(&mut (&mut entry).take(size), &mut out)
          .map_err(|e| {
            UpdateError::new(
              Code::UnsafeArchive,
              format!("{}: {e}", parts.join("/")),
            )
          })?;
        if copied != size {
          return err(
            Code::UnsafeArchive,
            format!("{} is truncated", parts.join("/")),
          );
        }
        #[cfg(unix)]
        {
          use std::os::unix::fs::PermissionsExt;
          let mode = entry.header().mode().unwrap_or(0o644) & 0o755;
          std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(mode | 0o600),
          )
          .map_err(|e| UpdateError::io(path.display(), e))?;
        }
        files += 1;
      }
      tar::EntryType::Symlink => {
        if is_top {
          return err(Code::UnsafeArchive, "the top-level entry is a symlink");
        }
        if cfg!(windows) {
          return err(
            Code::UnsafeArchive,
            format!("symlink {} in a Windows update archive", parts.join("/")),
          );
        }
        let Some(target) = entry.link_name_bytes() else {
          return err(Code::UnsafeArchive, "a symlink without a target");
        };
        let Ok(target) = std::str::from_utf8(&target) else {
          return err(Code::UnsafeArchive, "a symlink target is not UTF-8");
        };
        if !symlink_stays_inside(&parts, target) {
          return err(
            Code::UnsafeArchive,
            format!(
              "symlink {} -> {target} points outside the archive",
              parts.join("/")
            ),
          );
        }
        symlinks.push((parts, target.to_string()));
      }
      other => {
        return err(
          Code::UnsafeArchive,
          format!("refusing {other:?} entry {}", parts.join("/")),
        );
      }
    }
  }

  // Symlinks last: no file was ever written through one. Each must sit under
  // real directories only (not under an earlier symlink).
  #[cfg(unix)]
  {
    let links: HashSet<Vec<String>> =
      symlinks.iter().map(|(p, _)| p.clone()).collect();
    for (parts, target) in &symlinks {
      for i in 1..parts.len() {
        if links.contains(&parts[..i]) {
          return err(
            Code::UnsafeArchive,
            format!("symlink {} sits under another symlink", parts.join("/")),
          );
        }
      }
      ensure_real_parents(dest, parts)?;
      let path = join(dest, parts);
      if std::fs::symlink_metadata(&path).is_ok() {
        return err(
          Code::UnsafeArchive,
          format!("symlink {} collides with another entry", parts.join("/")),
        );
      }
      std::os::unix::fs::symlink(target, &path)
        .map_err(|e| UpdateError::io(path.display(), e))?;
    }
    // What each link resolves to must be inside the app (the top-level
    // entry), not just inside the extraction directory.
    for (parts, _) in &symlinks {
      let top = dest.join(&parts[0]);
      let root = std::fs::canonicalize(&top)
        .map_err(|e| UpdateError::io(top.display(), e))?;
      let path = join(dest, parts);
      match std::fs::canonicalize(&path) {
        Ok(real) if real.starts_with(&root) && real != root => {}
        Ok(_) => {
          return err(
            Code::UnsafeArchive,
            format!("symlink {} resolves outside the archive", parts.join("/")),
          );
        }
        Err(_) => {
          return err(
            Code::UnsafeArchive,
            format!("dangling symlink {}", parts.join("/")),
          );
        }
      }
    }
  }
  #[cfg(not(unix))]
  debug_assert!(symlinks.is_empty());

  let Some(top) = tops.into_iter().next() else {
    return err(Code::UnsafeArchive, "the archive is empty");
  };
  // A top-level directory may be implied by its children.
  let top_is_dir = top_is_dir.unwrap_or(true);
  Ok(Extracted {
    top,
    top_is_dir,
    files,
  })
}

/// Whether `path` has no `..`/root components (a cheap guard for paths that
/// were built from validated segments).
pub fn is_plain_relative(path: &Path) -> bool {
  path
    .components()
    .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
  }

  /// A raw ustar header block for `name` (no validation, so hostile names
  /// can be written).
  fn header(name: &str, kind: u8, size: u64, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000755\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    let s = format!("{size:011o}\0");
    h[124..136].copy_from_slice(s.as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    let c = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(c.as_bytes());
    h
  }

  /// Build a `.tar.gz` from (name, kind, body, link) entries.
  fn tgz(dir: &Path, entries: &[(&str, u8, &[u8], &str)]) -> PathBuf {
    let mut raw = Vec::new();
    for (name, kind, body, link) in entries {
      raw.extend_from_slice(&header(name, *kind, body.len() as u64, link));
      raw.extend_from_slice(body);
      let pad = (512 - body.len() % 512) % 512;
      raw.extend(std::iter::repeat_n(0u8, pad));
    }
    raw.extend(std::iter::repeat_n(0u8, 1024));
    let path = dir.join("a.tar.gz");
    let mut enc = flate2::write::GzEncoder::new(
      File::create(&path).unwrap(),
      flate2::Compression::fast(),
    );
    enc.write_all(&raw).unwrap();
    enc.finish().unwrap();
    path
  }

  fn extract(
    entries: &[(&str, u8, &[u8], &str)],
  ) -> Result<Extracted, UpdateError> {
    let t = tmp();
    let a = tgz(t.path(), entries);
    extract_tar_gz(&a, &t.path().join("out"), 1 << 30)
  }

  #[test]
  fn extracts_a_bundle() {
    let t = tmp();
    let entries: &[(&str, u8, &[u8], &str)] = &[
      ("App.app/", b'5', b"", ""),
      ("App.app/Contents/", b'5', b"", ""),
      ("App.app/Contents/MacOS/app", b'0', b"#!/bin/sh\n", ""),
      ("App.app/Contents/Info.plist", b'0', b"<plist/>", ""),
    ];
    let a = tgz(t.path(), entries);
    let out = t.path().join("out");
    let x = extract_tar_gz(&a, &out, 1 << 30).unwrap();
    assert_eq!(x.top, "App.app");
    assert!(x.top_is_dir);
    assert_eq!(x.files, 2);
    assert_eq!(
      std::fs::read(out.join("App.app/Contents/MacOS/app")).unwrap(),
      b"#!/bin/sh\n"
    );
  }

  #[test]
  fn refuses_tar_slip() {
    for name in ["../evil", "App/../../evil", "/etc/evil", "C:/evil", "a\\b"] {
      let r = extract(&[(name, b'0', b"x", "")]);
      assert_eq!(r.unwrap_err().code, Code::UnsafeArchive, "{name}");
    }
  }

  #[test]
  fn refuses_special_entries() {
    for kind in [b'1', b'3', b'4', b'6'] {
      let r = extract(&[("App/", b'5', b"", ""), ("App/x", kind, b"", "y")]);
      assert_eq!(r.unwrap_err().code, Code::UnsafeArchive, "{}", kind as char);
    }
  }

  #[cfg(unix)]
  #[test]
  fn refuses_escaping_symlinks() {
    // Out of the extraction directory, and out of the app into the
    // extraction directory (`../download.part`, a sibling) or onto the app's
    // own top-level directory.
    for target in [
      "../../outside",
      "/etc/passwd",
      "../..",
      "../download.part",
      "../Other",
      "..",
      "../App",
    ] {
      let r =
        extract(&[("App/", b'5', b"", ""), ("App/link", b'2', b"", target)]);
      assert_eq!(r.unwrap_err().code, Code::UnsafeArchive, "{target}");
    }
  }

  #[cfg(unix)]
  #[test]
  fn refuses_writing_through_a_symlink() {
    // `App/dir -> .` then `App/dir/x`: the file is written before any link
    // exists, and the link then sits where `dir/` already is a real dir.
    let r = extract(&[
      ("App/", b'5', b"", ""),
      ("App/dir", b'2', b"", "."),
      ("App/dir/x", b'0', b"x", ""),
    ]);
    assert_eq!(r.unwrap_err().code, Code::UnsafeArchive);
    // A link under another link.
    let r = extract(&[
      ("App/", b'5', b"", ""),
      ("App/sub/", b'5', b"", ""),
      ("App/a", b'2', b"", "sub"),
      ("App/a/b", b'2', b"", "."),
    ]);
    assert_eq!(r.unwrap_err().code, Code::UnsafeArchive);
  }

  #[cfg(unix)]
  #[test]
  fn keeps_inside_symlinks_and_strips_setuid() {
    let t = tmp();
    let mut h = header("App/bin", b'0', 2, "");
    h[100..108].copy_from_slice(b"0004755\0");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut raw = Vec::new();
    raw.extend_from_slice(&header("App/", b'5', 0, ""));
    raw.extend_from_slice(&h);
    raw.extend_from_slice(b"hi");
    raw.extend(std::iter::repeat_n(0u8, 510));
    raw.extend_from_slice(&header("App/Versions/", b'5', 0, ""));
    raw.extend_from_slice(&header("App/Current", b'2', 0, "Versions"));
    raw.extend(std::iter::repeat_n(0u8, 1024));
    let a = t.path().join("a.tar.gz");
    let mut enc = flate2::write::GzEncoder::new(
      File::create(&a).unwrap(),
      flate2::Compression::fast(),
    );
    enc.write_all(&raw).unwrap();
    enc.finish().unwrap();
    let out = t.path().join("out");
    extract_tar_gz(&a, &out, 1 << 30).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(out.join("App/bin"))
      .unwrap()
      .permissions()
      .mode();
    assert_eq!(mode & 0o7777, 0o755);
    // Group- and world-writable bits are dropped; the owner can always read
    // and write.
    for (packed, expect) in [(0o777, 0o755), (0o666, 0o644), (0o2775, 0o755)] {
      let t = tmp();
      let mut h = header("App/f", b'0', 1, "");
      h[100..108].copy_from_slice(format!("{packed:07o}\0").as_bytes());
      h[148..156].copy_from_slice(b"        ");
      let sum: u32 = h.iter().map(|&b| b as u32).sum();
      h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
      let mut raw = h.to_vec();
      raw.push(b'x');
      raw.extend(std::iter::repeat_n(0u8, 511 + 1024));
      let a = t.path().join("m.tar.gz");
      let mut enc = flate2::write::GzEncoder::new(
        File::create(&a).unwrap(),
        flate2::Compression::fast(),
      );
      enc.write_all(&raw).unwrap();
      enc.finish().unwrap();
      let out = t.path().join("out");
      extract_tar_gz(&a, &out, 1 << 30).unwrap();
      let mode = std::fs::metadata(out.join("App/f"))
        .unwrap()
        .permissions()
        .mode();
      assert_eq!(mode & 0o7777, expect, "{packed:o}");
    }
    assert!(
      std::fs::symlink_metadata(out.join("App/Current"))
        .unwrap()
        .file_type()
        .is_symlink()
    );
  }

  #[test]
  fn refuses_duplicates_two_tops_and_bombs() {
    let r = extract(&[("App/x", b'0', b"1", ""), ("App/x", b'0', b"2", "")]);
    assert_eq!(r.unwrap_err().code, Code::UnsafeArchive);
    let r = extract(&[("App/x", b'0', b"1", ""), ("Other/x", b'0', b"2", "")]);
    assert_eq!(r.unwrap_err().code, Code::UnsafeArchive);
    let t = tmp();
    let a = tgz(t.path(), &[("App/big", b'0', &[0u8; 4096], "")]);
    let r = extract_tar_gz(&a, &t.path().join("out"), 1000);
    assert_eq!(r.unwrap_err().code, Code::UnsafeArchive);
  }

  #[test]
  fn sink_stops_at_the_declared_size() {
    let t = tmp();
    let body = b"hello world";
    let sha = faster_hex::hex_string(&sha2::Sha256::digest(body));
    let mut s =
      DownloadSink::create(t.path().join("a"), body.len() as u64, &sha)
        .unwrap();
    s.write(&body[..5]).unwrap();
    let e = s.write(b"0123456789").unwrap_err();
    assert_eq!(e.code, Code::SizeExceeded);
    // Nothing of the refused chunk was written.
    assert_eq!(s.written(), 5);
    s.write(&body[5..]).unwrap();
    assert_eq!(s.finish().unwrap(), t.path().join("a"));
  }

  #[test]
  fn sink_checks_sha_and_short_downloads() {
    let t = tmp();
    let sha = "0".repeat(64);
    let mut s = DownloadSink::create(t.path().join("a"), 3, &sha).unwrap();
    s.write(b"abc").unwrap();
    assert_eq!(s.finish().unwrap_err().code, Code::Integrity);
    let sha = faster_hex::hex_string(&sha2::Sha256::digest(b"abc"));
    let mut s = DownloadSink::create(t.path().join("b"), 3, &sha).unwrap();
    s.write(b"ab").unwrap();
    assert_eq!(s.finish().unwrap_err().code, Code::Integrity);
    // The target must not exist (never written through an existing path).
    assert!(DownloadSink::create(t.path().join("a"), 3, &sha).is_err());
  }
}
