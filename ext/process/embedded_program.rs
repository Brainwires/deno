// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 215): programs a packaged app carries in its binary
//! and may spawn only with the bytes it was packaged with.
//!
//! A scoped run grant names a path. When that path is in a folder the app can
//! write (a per-user cache), the grant alone says nothing about what runs. So
//! the embedder registers each such program here with the SHA-256 recorded at
//! packaging and a way to read the embedded bytes, and every spawn path checks
//! a registered path after the permission check: the file is written from the
//! embedded bytes when it is missing, and it is refused (and removed, so the
//! next spawn writes it again) when it holds anything else.
//!
//! The hash and the bytes live in the binary, out of reach of the app's own
//! write grants, and the check is in the op rather than in JS, so code in the
//! app that calls `Deno.Command` itself gets the same answer.
//!
//! The check and the exec are two steps. What holds between them:
//!
//! - The file is hashed through one handle opened without following a final
//!   symlink, and that handle stays open past the spawn.
//! - Windows: the handle shares read only, so until the child's image is
//!   mapped the file can be neither written, nor renamed, nor deleted.
//! - Unix: nothing can lock a file against its owner. After an asynchronous
//!   spawn the path is compared with the handle's device, inode, size, mtime
//!   and ctime as they were before the hash; on a difference the child is
//!   killed and the spawn refused. So a same-user program that wins the race
//!   (microseconds) gets at most a moment of a child, and app code holding only
//!   the Deno APIs cannot try: the embedder denies writes to the folder.

#![allow(
  clippy::disallowed_methods,
  reason = "these are files on the real file system, whatever the app's virtual one holds"
)]

use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use sha2::Digest;
use sha2::Sha256;

/// Reads the embedded bytes of a program.
pub type EmbeddedProgramReader =
  Arc<dyn Fn() -> std::io::Result<Vec<u8>> + Send + Sync>;

/// One program the app may spawn only as packaged.
#[derive(Clone)]
pub struct EmbeddedProgram {
  /// The absolute path it runs from, as the run grant names it.
  pub path: PathBuf,
  /// The SHA-256 of its bytes, recorded at packaging.
  pub sha256: [u8; 32],
  /// Its bytes, from the binary.
  pub read: EmbeddedProgramReader,
}

static PROGRAMS: LazyLock<Mutex<Vec<EmbeddedProgram>>> =
  LazyLock::new(Default::default);

/// Registers the programs of this process (once, at startup).
pub fn set_embedded_programs(programs: Vec<EmbeddedProgram>) {
  *PROGRAMS
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = programs;
}

/// Whether two paths name the same entry for this OS's usual file systems: a
/// spelling that differs only by case passes the run grant's check on a
/// case-insensitive volume only by being the same file, and must not miss the
/// list by being spelled differently.
fn same_path(a: &Path, b: &Path) -> bool {
  if cfg!(any(target_os = "macos", windows)) {
    a.to_string_lossy()
      .eq_ignore_ascii_case(&b.to_string_lossy())
  } else {
    a == b
  }
}

/// The registered program `cmd` names, if any.
fn find(cmd: &Path) -> Option<EmbeddedProgram> {
  PROGRAMS
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .iter()
    .find(|p| same_path(&p.path, cmd))
    .cloned()
}

/// The checks that passed for one spawn: held until the spawn has returned.
pub(crate) struct VerifiedProgram {
  // The handle the file was hashed through (on Windows, also the lock).
  file: File,
  #[cfg(unix)]
  path: PathBuf,
  #[cfg(unix)]
  stamp: FileStamp,
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct FileStamp {
  dev: u64,
  ino: u64,
  size: u64,
  mtime: (i64, i64),
  ctime: (i64, i64),
}

#[cfg(unix)]
fn stamp_of(meta: &std::fs::Metadata) -> FileStamp {
  use std::os::unix::fs::MetadataExt;
  FileStamp {
    dev: meta.dev(),
    ino: meta.ino(),
    size: meta.size(),
    mtime: (meta.mtime(), meta.mtime_nsec()),
    ctime: (meta.ctime(), meta.ctime_nsec()),
  }
}

impl VerifiedProgram {
  /// Whether the path still is the file that was hashed, unchanged since. For
  /// the moment right after the spawn, while the handle is still held.
  pub(crate) fn unchanged(&self) -> bool {
    #[cfg(unix)]
    {
      let by_path = std::fs::symlink_metadata(&self.path);
      let by_handle = self.file.metadata();
      matches!((by_path, by_handle), (Ok(p), Ok(h))
        if stamp_of(&p) == self.stamp && stamp_of(&h) == self.stamp)
    }
    #[cfg(not(unix))]
    {
      // The handle's share mode has kept it so.
      let _ = &self.file;
      true
    }
  }
}

/// Opens `path` without following a final symlink, keeping it from being
/// changed where the OS can (Windows).
fn open_no_follow(path: &Path) -> std::io::Result<File> {
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK: a FIFO put in its place must not hold the spawn up.
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ (the loader's own open asks for no more), and
    // FILE_FLAG_OPEN_REPARSE_POINT.
    options.share_mode(0x1);
    options.custom_flags(0x0020_0000);
  }
  options.open(path)
}

/// The SHA-256 of everything `file` holds, from its current position.
fn hash_file(file: &mut File) -> std::io::Result<[u8; 32]> {
  let mut hasher = Sha256::new();
  let mut buf = vec![0u8; 64 * 1024];
  loop {
    let n = file.read(&mut buf)?;
    if n == 0 {
      return Ok(hasher.finalize().into());
    }
    hasher.update(&buf[..n]);
  }
}

/// Writes the embedded bytes to the program's path (a new file renamed into
/// place, so nothing ever sees a part of it).
fn materialize(program: &EmbeddedProgram) -> Result<(), String> {
  let bytes = (program.read)().map_err(|e| {
    format!(
      "the embedded copy of {} cannot be read: {e}",
      program.path.display()
    )
  })?;
  if <[u8; 32]>::from(Sha256::digest(&bytes)) != program.sha256 {
    return Err(format!(
      "the embedded copy of {} differs from the SHA-256 recorded when the app was packaged",
      program.path.display()
    ));
  }
  let write = || -> std::io::Result<()> {
    let parent = program
      .path
      .parent()
      .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt;
      dir.mode(0o700);
    }
    dir.create(parent)?;
    let mut temp_options = std::fs::OpenOptions::new();
    temp_options.write(true).create_new(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      temp_options.mode(0o500);
    }
    let temp = parent.join(format!(
      ".{}.{:016x}.tmp",
      std::process::id(),
      rand::random::<u64>()
    ));
    let mut file = temp_options.open(&temp)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let moved = written.and_then(|()| std::fs::rename(&temp, &program.path));
    if moved.is_err() {
      let _ = std::fs::remove_file(&temp);
    }
    moved
  };
  write().map_err(|e| {
    format!(
      "{} cannot be written from its embedded copy: {e}",
      program.path.display()
    )
  })
}

/// Removes whatever is at `path`, as far as it can: what is refused stays
/// refused either way.
fn remove_any(path: &Path) {
  let _ = if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
    std::fs::remove_dir_all(path)
  } else {
    std::fs::remove_file(path)
  };
}

/// Checks the file at the program's path, writing it first when it is missing.
fn verify(program: &EmbeddedProgram) -> Result<VerifiedProgram, String> {
  let refused = |why: String| {
    format!(
      "Refusing to run {}: {why}. It is written again from the app on the next start",
      program.path.display()
    )
  };
  let mut file = match open_no_follow(&program.path) {
    Ok(file) => file,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      materialize(program).map_err(|e| format!("Cannot run a sidecar: {e}"))?;
      open_no_follow(&program.path)
        .map_err(|e| format!("Cannot run {}: {e}", program.path.display()))?
    }
    Err(e) => {
      // A symlink in its place (ELOOP), or something else unreadable.
      remove_any(&program.path);
      return Err(refused(format!("it cannot be opened as a file: {e}")));
    }
  };
  let meta = file
    .metadata()
    .map_err(|e| format!("Cannot run {}: {e}", program.path.display()))?;
  #[cfg(unix)]
  let stamp = stamp_of(&meta);
  let bytes_match = meta.is_file()
    && hash_file(&mut file).is_ok_and(|hash| hash == program.sha256);
  if !bytes_match {
    drop(file);
    remove_any(&program.path);
    return Err(refused(
      "its contents differ from the SHA-256 recorded when the app was packaged"
        .to_string(),
    ));
  }
  Ok(VerifiedProgram {
    file,
    #[cfg(unix)]
    path: program.path.clone(),
    #[cfg(unix)]
    stamp,
  })
}

/// What a spawn of `cmd` must hold on to, `None` when `cmd` is not a registered
/// program; an error message when it may not run.
pub(crate) fn check(cmd: &Path) -> Result<Option<VerifiedProgram>, String> {
  match find(cmd) {
    Some(program) => verify(&program).map(Some),
    None => Ok(None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn program(path: &Path, bytes: &'static [u8]) -> EmbeddedProgram {
    EmbeddedProgram {
      path: path.to_path_buf(),
      sha256: Sha256::digest(bytes).into(),
      read: Arc::new(move || Ok(bytes.to_vec())),
    }
  }

  fn target() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sidecars").join("x").join("prog");
    (dir, path)
  }

  #[test]
  fn a_missing_program_is_written_from_the_embedded_bytes() {
    let (_dir, path) = target();
    let p = program(&path, b"packaged");
    verify(&p).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"packaged");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(&path).unwrap().permissions().mode();
      assert_eq!(mode & 0o777, 0o500);
    }
    // Once written, the next spawn takes it as it is.
    verify(&p).unwrap();
  }

  #[test]
  fn a_swapped_program_is_refused_then_written_again() {
    let (_dir, path) = target();
    let p = program(&path, b"packaged");
    drop(verify(&p).unwrap());
    let other = path.with_file_name("other");
    std::fs::write(&other, b"something else").unwrap();
    std::fs::rename(&other, &path).unwrap();
    let err = verify(&p).err().unwrap();
    assert!(err.starts_with("Refusing to run"), "{err}");
    assert!(err.contains("SHA-256"), "{err}");
    // The bytes that were refused are gone, and the next spawn heals it.
    assert!(!path.exists());
    verify(&p).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"packaged");
  }

  #[cfg(unix)]
  #[test]
  fn a_symlink_in_its_place_is_refused_even_to_the_right_bytes() {
    let (dir, path) = target();
    let p = program(&path, b"packaged");
    drop(verify(&p).unwrap());
    let real = dir.path().join("real");
    std::fs::write(&real, b"packaged").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let err = verify(&p).err().unwrap();
    assert!(err.starts_with("Refusing to run"), "{err}");
    assert!(std::fs::symlink_metadata(&path).is_err());
  }

  #[test]
  fn embedded_bytes_that_do_not_match_the_recorded_hash_are_not_written() {
    let (_dir, path) = target();
    let mut p = program(&path, b"packaged");
    p.sha256 = [0; 32];
    let err = verify(&p).err().unwrap();
    assert!(err.contains("embedded copy"), "{err}");
    assert!(!path.exists());
  }

  #[cfg(unix)]
  #[test]
  fn a_fifo_in_its_place_does_not_hold_the_spawn_up() {
    let (_dir, path) = target();
    let p = program(&path, b"packaged");
    drop(verify(&p).unwrap());
    std::fs::remove_file(&path).unwrap();
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(verify(&p).is_err());
    assert!(std::fs::symlink_metadata(&path).is_err());
  }

  #[cfg(unix)]
  #[test]
  fn a_change_after_the_hash_is_seen_while_the_handle_is_held() {
    let (_dir, path) = target();
    let p = program(&path, b"packaged");
    let held = verify(&p).unwrap();
    assert!(held.unchanged());
    let other = path.with_file_name("other");
    std::fs::write(&other, b"something else").unwrap();
    std::fs::rename(&other, &path).unwrap();
    assert!(!held.unchanged());
    drop(held);

    // ... and written in place, under the same inode.
    std::fs::remove_file(&path).unwrap();
    let held = verify(&p).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
      .unwrap();
    assert!(!held.unchanged());
  }

  #[cfg(windows)]
  #[test]
  fn the_held_handle_keeps_the_file_from_changing() {
    let (_dir, path) = target();
    let p = program(&path, b"packaged");
    let held = verify(&p).unwrap();
    assert!(std::fs::write(&path, b"something else").is_err());
    assert!(std::fs::rename(&path, path.with_file_name("moved")).is_err());
    assert!(std::fs::remove_file(&path).is_err());
    drop(held);
    assert!(std::fs::write(&path, b"packaged").is_ok());
  }
}
