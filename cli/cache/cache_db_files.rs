// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: one file per runtime version for each DENO_DIR cache database.
//!
//! Every cache database used to be a single file that was wiped
//! (`CacheDBConfiguration::on_version_change`) whenever a CLI of another
//! version opened it. With several denext runtimes beside each other (project
//! pins, dev builds), each switch threw away and rebuilt the whole V8 code
//! cache and the analysis caches. Each version now opens its own file,
//! `<name>_<version>` (with SQLite's `-wal` / `-shm` / `-journal` side files),
//! and the files of other versions, the unversioned ones of earlier builds
//! included, are removed once they have gone unused for [`STALE_AFTER`]
//! (DENEXT.md, "Cache databases per runtime version").

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use sys_traits::FsSetFileTimes;

/// Another version's cache database is removed once none of its files has
/// been used for this long.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A version's own cache database is marked used (its modification time set)
/// at most this often, so a read-only run doesn't write the file every time.
pub(crate) const LAST_USED_REFRESH: Duration =
  Duration::from_secs(24 * 60 * 60);

/// A version's database that has gone unused this long is opened under the
/// upkeep lock ([`lock_for_long_unused_open`]): a day short of
/// [`STALE_AFTER`], so a database another version may prune is never opened
/// without it.
const OPEN_UNDER_LOCK_AFTER: Duration = Duration::from_secs(6 * 24 * 60 * 60);

/// SQLite's side files of a database file.
const SIDE_FILE_SUFFIXES: &[&str] = &["-wal", "-shm", "-journal"];

/// The part of a file name that names the version: the version with any
/// character outside `[A-Za-z0-9.+-]` replaced by `_`. It always starts with
/// a digit (a version that doesn't is prefixed with `0`), which is how a
/// version's file is told from a different cache whose name merely starts
/// with this one's.
pub(crate) fn version_file_tag(version: &str) -> String {
  let mut tag: String = version
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-') {
        c
      } else {
        '_'
      }
    })
    .collect();
  if !tag.starts_with(|c: char| c.is_ascii_digit()) {
    tag.insert(0, '0');
  }
  tag
}

/// The file of the cache database `base` for `version`: `<base>_<tag>`.
pub(crate) fn versioned_path(base: &Path, version: &str) -> PathBuf {
  let mut name = base
    .file_name()
    .map(|n| n.to_os_string())
    .unwrap_or_default();
  name.push("_");
  name.push(version_file_tag(version));
  base.with_file_name(name)
}

/// The database file (without a side-file suffix) that `file_name` belongs
/// to, when it is a copy of the cache `base_name`: the unversioned file of an
/// earlier build (`<base>`) or a version's (`<base>_<tag>`).
fn cache_db_group<'a>(base_name: &str, file_name: &'a str) -> Option<&'a str> {
  let rest = file_name.strip_prefix(base_name)?;
  let stem = SIDE_FILE_SUFFIXES
    .iter()
    .find_map(|suffix| rest.strip_suffix(suffix))
    .unwrap_or(rest);
  let is_copy = stem.is_empty()
    || stem.strip_prefix('_').is_some_and(|tag| {
      tag.starts_with(|c: char| c.is_ascii_digit())
        && tag.chars().all(|c| {
          c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')
        })
    });
  is_copy.then(|| &file_name[..base_name.len() + stem.len()])
}

/// Upkeep after this version's database `current` (of the cache `base`) was
/// opened: when the open created it, or when it was last marked used more
/// than [`LAST_USED_REFRESH`] ago (it is marked now), the other versions'
/// stale files are removed. So this runs at most once a day per cache.
pub(crate) fn maintain_versioned_files(
  base: &Path,
  current: &Path,
  created: bool,
  now: SystemTime,
) {
  if !created {
    let Ok(modified) = std::fs::metadata(current).and_then(|m| m.modified())
    else {
      return;
    };
    let recently = now
      .duration_since(modified)
      .map_or(true, |age| age < LAST_USED_REFRESH);
    if recently {
      return;
    }
    // by path, never through a file handle: closing any handle to a file
    // drops the POSIX locks this process's SQLite connection holds on it
    if let Err(err) =
      sys_traits::impls::RealSys.fs_set_file_times(current, now, now)
    {
      log::debug!(
        "Failed marking cache database '{}' used: {err:#}",
        current.display()
      );
      return;
    }
  }
  prune_stale_versions(base, current, now);
}

/// The upkeep lock of the cache `base`: the file `<base>.lock` beside its
/// databases, locked by the OS (unlocked when this is dropped, and released
/// by the OS when the process ends). Pruning holds it exclusively; a version opening a database
/// of its own that has gone unused long enough to be pruned holds it shared
/// until its connection is open. So a database is never removed while the
/// version that owns it is between finding the file and holding SQLite's
/// own locks on it, where the check for those locks can't see it: a
/// connection opened on a file removed in that moment would go on with a
/// database no later connection shares.
pub(crate) struct UpkeepLock {
  file: std::fs::File,
}

impl Drop for UpkeepLock {
  /// Unlocks, instead of leaving it to the handle's close: the lock belongs
  /// to the open file, which a child process this one starts on another
  /// thread shares until it has replaced its image (the descriptor is
  /// close-on-exec, but the copy lives from the fork to the exec). Closing
  /// this handle alone would leave the lock held for that long, and a
  /// pruning pass right after would find it busy and remove nothing.
  fn drop(&mut self) {
    _ = self.file.unlock();
  }
}

fn upkeep_lock_file(base: &Path) -> Option<std::fs::File> {
  let mut name = base.file_name()?.to_os_string();
  name.push(".lock");
  std::fs::OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(base.with_file_name(name))
    .ok()
}

/// Before this version's database `current` (of the cache `base`) is
/// opened: when the file exists and was last marked used
/// [`OPEN_UNDER_LOCK_AFTER`] or more before `now`, takes the upkeep lock
/// (waiting for a pruning pass to finish; they are short) and marks the file
/// used. The caller opens its connection and then drops the lock. `None`
/// when the file is new, missing or recently used, or the lock can't be had
/// (a DENO_DIR that can't be written): the open goes on without it.
pub(crate) fn lock_for_long_unused_open(
  base: &Path,
  current: &Path,
  now: SystemTime,
) -> Option<UpkeepLock> {
  let modified = std::fs::metadata(current).ok()?.modified().ok()?;
  let long_unused = now
    .duration_since(modified)
    .is_ok_and(|age| age >= OPEN_UNDER_LOCK_AFTER);
  if !long_unused {
    return None;
  }
  let file = upkeep_lock_file(base)?;
  file.lock_shared().ok()?;
  // (by path, as in `maintain_versioned_files`)
  let _ = sys_traits::impls::RealSys.fs_set_file_times(current, now, now);
  Some(UpkeepLock { file })
}

/// Removes the other versions' files of the cache `base` (and the
/// unversioned ones of earlier builds) whose last use, the newest
/// modification time among a database's files, is [`STALE_AFTER`] or more
/// before `now`. `current` (this version's) is never touched, nor is a
/// database another process holds a lock on. The whole pass holds the
/// cache's [`UpkeepLock`]; when it can't be taken at once (a version is
/// opening a long-unused database, or another pass runs) nothing is removed
/// this time.
pub(crate) fn prune_stale_versions(
  base: &Path,
  current: &Path,
  now: SystemTime,
) {
  let pass = prune_pass(base, current, now);
  log::debug!(
    "Pruning the versions of cache database '{}': {pass}",
    base.display()
  );
}

/// What a pruning pass did, for the debug log and the tests' messages.
enum PrunePass {
  /// Nothing was looked at, and why.
  NotRun(String),
  /// The pass ran: every other version's database found, in name order.
  Ran(Vec<PrunedGroup>),
}

/// One database (with its side files) a pruning pass found.
struct PrunedGroup {
  /// The database's file name.
  name: String,
  /// The newest modification time among its files; `None`: unknown.
  last_used: Option<SystemTime>,
  /// Each of its files with its modification time (`None`: unknown, or not
  /// a regular file).
  files: Vec<(String, Option<SystemTime>)>,
  outcome: PruneOutcome,
}

enum PruneOutcome {
  Removed,
  /// The last use is unknown.
  KeptUnknownAge,
  /// Used less than [`STALE_AFTER`] ago.
  KeptRecent,
  /// Another process holds it (or the check failed): what the check found.
  KeptHeld(String),
  /// Removing the database file failed.
  KeptRemoveFailed(std::io::Error),
}

impl std::fmt::Display for PrunePass {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let groups = match self {
      PrunePass::NotRun(why) => return write!(f, "not run: {why}"),
      PrunePass::Ran(groups) => groups,
    };
    write!(f, "ran over {} database(s)", groups.len())?;
    for group in groups {
      write!(f, "; '{}' ", group.name)?;
      match &group.outcome {
        PruneOutcome::Removed => write!(f, "removed")?,
        PruneOutcome::KeptUnknownAge => {
          write!(f, "kept (a file's modification time is unknown)")?
        }
        PruneOutcome::KeptRecent => write!(f, "kept (recently used)")?,
        PruneOutcome::KeptHeld(why) => write!(f, "kept (held: {why})")?,
        PruneOutcome::KeptRemoveFailed(err) => {
          write!(f, "kept (removing it failed: {err:#})")?
        }
      }
      write!(f, ", last used {:?}, files", group.last_used)?;
      for (name, modified) in &group.files {
        write!(f, " '{name}' modified {modified:?}")?;
      }
    }
    Ok(())
  }
}

fn prune_pass(base: &Path, current: &Path, now: SystemTime) -> PrunePass {
  let (Some(dir), Some(base_name), Some(current_name)) = (
    base.parent(),
    base.file_name().and_then(|n| n.to_str()),
    current.file_name().and_then(|n| n.to_str()),
  ) else {
    return PrunePass::NotRun("the cache's path has no UTF-8 name".into());
  };
  let Some(lock) = upkeep_lock_file(base) else {
    return PrunePass::NotRun("the upkeep lock file can't be opened".into());
  };
  if let Err(err) = lock.try_lock() {
    return PrunePass::NotRun(format!("the upkeep lock is held ({err})"));
  }
  let _lock = UpkeepLock { file: lock };
  let entries = match std::fs::read_dir(dir) {
    Ok(entries) => entries,
    Err(err) => {
      return PrunePass::NotRun(format!("reading the directory: {err:#}"));
    }
  };
  // database file name -> its last use (`None`: unknown, so kept) and
  // its files
  type Group = (Option<SystemTime>, Vec<(String, Option<SystemTime>)>);
  let mut groups: HashMap<String, Group> = HashMap::new();
  for entry in entries.flatten() {
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
      continue;
    };
    let Some(group) = cache_db_group(base_name, name) else {
      continue;
    };
    if group == current_name {
      continue;
    }
    let modified = entry
      .metadata()
      .ok()
      .filter(|m| m.is_file())
      .and_then(|m| m.modified().ok());
    let (last_used, files) = groups
      .entry(group.to_string())
      .or_insert((Some(SystemTime::UNIX_EPOCH), Vec::new()));
    *last_used = match (*last_used, modified) {
      (Some(a), Some(b)) => Some(a.max(b)),
      _ => None,
    };
    files.push((name.to_string(), modified));
  }
  let mut groups = groups.into_iter().collect::<Vec<_>>();
  groups.sort_by(|a, b| a.0.cmp(&b.0));
  let pruned = groups
    .into_iter()
    .map(|(name, (last_used, mut files))| {
      files.sort();
      let outcome = prune_group(dir, &name, last_used, now);
      PrunedGroup {
        name,
        last_used,
        files,
        outcome,
      }
    })
    .collect();
  PrunePass::Ran(pruned)
}

/// Removes the database `group` in `dir` with its side files when it is
/// stale and no other process holds it.
fn prune_group(
  dir: &Path,
  group: &str,
  last_used: Option<SystemTime>,
  now: SystemTime,
) -> PruneOutcome {
  let Some(last_used) = last_used else {
    return PruneOutcome::KeptUnknownAge;
  };
  let stale = now
    .duration_since(last_used)
    .is_ok_and(|age| age >= STALE_AFTER);
  if !stale {
    return PruneOutcome::KeptRecent;
  }
  let db = dir.join(group);
  for path in [db.clone(), dir.join(format!("{group}-shm"))] {
    if let Some(why) = held_by_another_process(&path) {
      return PruneOutcome::KeptHeld(format!("'{}': {why}", path.display()));
    }
  }
  // the database first: where an open file can't be removed (Windows),
  // that failing keeps its side files too
  match std::fs::remove_file(&db) {
    Ok(()) => {}
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
    Err(err) => return PruneOutcome::KeptRemoveFailed(err),
  }
  for suffix in SIDE_FILE_SUFFIXES {
    _ = std::fs::remove_file(dir.join(format!("{group}{suffix}")));
  }
  PruneOutcome::Removed
}

/// Whether another process holds a POSIX lock on `path` (SQLite's unix VFS
/// locks the database while a transaction runs, and every open connection to
/// a WAL database keeps a read lock in its `-shm` file). Locks of this
/// process aren't reported, and it holds none on another version's files.
/// When the check itself fails the file counts as held. `Some` says what was
/// found; `None`: not held (or no such file).
#[cfg(unix)]
fn held_by_another_process(path: &Path) -> Option<String> {
  use std::os::fd::AsRawFd;

  let file = match std::fs::File::open(path) {
    Ok(file) => file,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
    Err(err) => return Some(format!("opening it failed: {err:#}")),
  };
  // SAFETY: an all-zero `flock` is a valid value of the plain C struct
  let mut lock: libc::flock = unsafe { std::mem::zeroed() };
  lock.l_type = libc::F_WRLCK as libc::c_short;
  lock.l_whence = libc::SEEK_SET as libc::c_short;
  lock.l_start = 0;
  lock.l_len = 0; // the whole file
  // SAFETY: `file` is open for the call and `lock` a valid `flock`
  let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) };
  if rc == -1 {
    let err = std::io::Error::last_os_error();
    return Some(format!("asking for its locks failed: {err:#}"));
  }
  (lock.l_type != libc::F_UNLCK as libc::c_short)
    .then(|| format!("process {} holds a lock on it", lock.l_pid))
}

/// Windows refuses to remove a file another process has open (SQLite opens
/// its files without `FILE_SHARE_DELETE`), which the removal handles.
#[cfg(not(unix))]
fn held_by_another_process(_path: &Path) -> Option<String> {
  None
}

#[cfg(test)]
mod tests {
  use test_util::TempDir;

  use super::*;

  const DAY: Duration = Duration::from_secs(24 * 60 * 60);

  fn touch(path: &Path, at: SystemTime) {
    if !path.exists() {
      std::fs::write(path, b"x").unwrap();
    }
    sys_traits::impls::RealSys
      .fs_set_file_times(path, at, at)
      .unwrap();
  }

  #[test]
  fn version_tags_and_paths() {
    assert_eq!(version_file_tag("3.0.0-denext.28"), "3.0.0-denext.28");
    assert_eq!(version_file_tag("2.9.8+abc1234"), "2.9.8+abc1234");
    assert_eq!(version_file_tag("1.0 beta/x"), "1.0_beta_x");
    assert_eq!(version_file_tag("v1"), "0v1");
    assert_eq!(version_file_tag(""), "0");
    let base = Path::new("/deno_dir/v8_code_cache_v2_denext");
    assert_eq!(
      versioned_path(base, "3.0.0-denext.28"),
      Path::new("/deno_dir/v8_code_cache_v2_denext_3.0.0-denext.28")
    );
  }

  #[test]
  fn groups_of_a_cache() {
    let base = "check_cache_v2_denext";
    for (name, group) in [
      ("check_cache_v2_denext", Some("check_cache_v2_denext")),
      ("check_cache_v2_denext-wal", Some("check_cache_v2_denext")),
      ("check_cache_v2_denext-shm", Some("check_cache_v2_denext")),
      (
        "check_cache_v2_denext-journal",
        Some("check_cache_v2_denext"),
      ),
      (
        "check_cache_v2_denext_3.0.0-denext.27",
        Some("check_cache_v2_denext_3.0.0-denext.27"),
      ),
      (
        "check_cache_v2_denext_3.0.0-denext.27-wal",
        Some("check_cache_v2_denext_3.0.0-denext.27"),
      ),
      (
        "check_cache_v2_denext_2.9.8+abc-shm",
        Some("check_cache_v2_denext_2.9.8+abc"),
      ),
      // other caches and stray files
      ("check_cache_v2", None),
      ("fast_check_cache_v2_denext", None),
      ("check_cache_v2_denext_extra", None),
      ("check_cache_v2_denext_", None),
      ("check_cache_v2_denext2", None),
      ("check_cache_v2_denext_1.0/x", None),
    ] {
      assert_eq!(cache_db_group(base, name), group, "{name}");
    }
  }

  #[test]
  fn prunes_only_stale_files_of_other_versions() {
    let temp = TempDir::new();
    let dir = temp.path().as_path().to_path_buf();
    let now = SystemTime::now();
    let old = now - 8 * DAY;
    let recent = now - 2 * DAY;
    let base = dir.join("test_cache");
    let current = versioned_path(&base, "2.0");
    // stale: the unversioned file of an earlier build and version 0.9
    for name in ["test_cache", "test_cache-wal", "test_cache-shm"] {
      touch(&dir.join(name), old);
    }
    for name in ["test_cache_0.9", "test_cache_0.9-shm"] {
      touch(&dir.join(name), old);
    }
    // a database whose side file was used recently is in use
    touch(&dir.join("test_cache_1.0"), old);
    touch(&dir.join("test_cache_1.0-wal"), recent);
    // recently used
    touch(&dir.join("test_cache_1.5"), recent);
    // this version's own files, however old, and files of no version
    for name in [
      "test_cache_2.0",
      "test_cache_2.0-wal",
      "test_cache_extra",
      "test_cache2",
      "other_cache_0.9",
    ] {
      touch(&dir.join(name), old);
    }

    let pass = prune_pass(&base, &current, now);

    let mut left = std::fs::read_dir(&dir)
      .unwrap()
      .map(|e| e.unwrap().file_name().into_string().unwrap())
      .collect::<Vec<_>>();
    left.sort();
    assert_eq!(
      left,
      [
        "other_cache_0.9",
        // the cache's upkeep lock
        "test_cache.lock",
        "test_cache2",
        "test_cache_1.0",
        "test_cache_1.0-wal",
        "test_cache_1.5",
        "test_cache_2.0",
        "test_cache_2.0-wal",
        "test_cache_extra",
      ],
      "{pass}"
    );
  }

  #[test]
  fn marks_used_once_a_day_and_prunes_then() {
    let temp = TempDir::new();
    let dir = temp.path().as_path().to_path_buf();
    let now = SystemTime::now();
    let base = dir.join("test_cache");
    let current = versioned_path(&base, "2.0");
    let other = dir.join("test_cache_1.0");
    let modified = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();

    // used an hour ago: nothing is written, nothing pruned
    touch(&current, now - Duration::from_secs(3600));
    touch(&other, now - 8 * DAY);
    let before = modified(&current);
    maintain_versioned_files(&base, &current, false, now);
    assert_eq!(modified(&current), before);
    assert!(other.exists());

    // last marked two days ago: marked now, and the stale version goes
    touch(&current, now - 2 * DAY);
    maintain_versioned_files(&base, &current, false, now);
    assert!(
      now.duration_since(modified(&current)).unwrap_or_default()
        < Duration::from_secs(5)
    );
    assert!(!other.exists());

    // a database the open just created prunes at once
    touch(&other, now - 8 * DAY);
    maintain_versioned_files(&base, &current, true, now);
    assert!(!other.exists());
  }

  /// A version's database that went unused long enough to be pruned is
  /// opened under the cache's upkeep lock and marked used; while the lock is
  /// held no pruning pass removes anything, and the next one does. A new,
  /// missing or recently used database is opened without it.
  #[test]
  fn a_long_unused_database_is_opened_under_the_upkeep_lock() {
    let temp = TempDir::new();
    let dir = temp.path().as_path().to_path_buf();
    let now = SystemTime::now();
    let base = dir.join("test_cache");
    let modified = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
    // version 1.0 opens its database, unused for eight days
    let opening = versioned_path(&base, "1.0");
    touch(&opening, now - 8 * DAY);
    let held = lock_for_long_unused_open(&base, &opening, now)
      .expect("a long-unused database is opened under the lock");
    assert!(
      now.duration_since(modified(&opening)).unwrap_or_default()
        < Duration::from_secs(5)
    );
    // meanwhile version 2.0 prunes: nothing goes, not even another stale
    // version's files
    let current = versioned_path(&base, "2.0");
    let stale = versioned_path(&base, "0.9");
    touch(&current, now);
    touch(&stale, now - 8 * DAY);
    let pass = prune_pass(&base, &current, now);
    assert!(
      matches!(pass, PrunePass::NotRun(_)),
      "a pass ran while the upkeep lock was held: {pass}"
    );
    assert!(stale.exists() && opening.exists(), "{pass}");
    // the connection is open: the lock goes, and the next pass prunes what
    // is still stale
    drop(held);
    let pass = prune_pass(&base, &current, now);
    let PrunePass::Ran(groups) = &pass else {
      panic!("no pass after the upkeep lock was dropped: {pass}");
    };
    // each database's outcome, by name: the stale one goes, the one just
    // opened (marked used) stays, and this version's own isn't looked at
    let outcomes = groups
      .iter()
      .map(|g| (g.name.as_str(), &g.outcome))
      .collect::<Vec<_>>();
    assert!(
      matches!(
        outcomes[..],
        [
          ("test_cache_0.9", PruneOutcome::Removed),
          ("test_cache_1.0", PruneOutcome::KeptRecent)
        ]
      ),
      "{pass}"
    );
    assert!(!stale.exists(), "{pass}");
    assert!(opening.exists() && current.exists(), "{pass}");

    // no lock for a database used within the last days, or a new one
    touch(&opening, now - 5 * DAY);
    let before = modified(&opening);
    assert!(lock_for_long_unused_open(&base, &opening, now).is_none());
    assert_eq!(modified(&opening), before);
    let missing = versioned_path(&base, "3.0");
    assert!(lock_for_long_unused_open(&base, &missing, now).is_none());
    assert!(!missing.exists());
  }

  /// Dropping the upkeep lock unlocks it even while a copy of its
  /// descriptor is still open: what a child process started on another
  /// thread holds between its fork and its exec (the lock belongs to the
  /// open file both share, so closing one descriptor doesn't release it).
  /// The next pruning pass runs at once.
  #[cfg(unix)]
  #[test]
  fn dropping_the_upkeep_lock_unlocks_a_shared_descriptor() {
    let temp = TempDir::new();
    let dir = temp.path().as_path().to_path_buf();
    let now = SystemTime::now();
    let base = dir.join("test_cache");
    let current = versioned_path(&base, "2.0");
    let stale = versioned_path(&base, "0.9");
    touch(&current, now);
    touch(&stale, now - 8 * DAY);
    for shared in [true, false] {
      let file = upkeep_lock_file(&base).unwrap();
      if shared {
        file.lock_shared().unwrap();
      } else {
        file.lock().unwrap();
      }
      let held = UpkeepLock { file };
      // the same open file, as a forked child has it
      let copy = held.file.try_clone().unwrap();
      let pass = prune_pass(&base, &current, now);
      assert!(
        matches!(pass, PrunePass::NotRun(_)),
        "a pass ran while the upkeep lock was held (shared: {shared}): {pass}"
      );
      drop(held);
      let pass = prune_pass(&base, &current, now);
      assert!(
        matches!(pass, PrunePass::Ran(_)),
        "the dropped upkeep lock is still held through the copy of its \
         descriptor (shared: {shared}): {pass}"
      );
      drop(copy);
    }
    assert!(!stale.exists());
  }

  /// A stale database another process still has open is kept, and removed
  /// once that process is gone. The holder is this test binary, re-run on
  /// this test alone (a lock of this process wouldn't be reported).
  #[cfg(unix)]
  #[test]
  fn keeps_a_database_another_process_holds() {
    use std::io::BufRead;
    use std::io::Write;

    const CHILD_ENV: &str = "DENO_TEST_CACHE_DB_HOLDER";
    if let Some(path) = std::env::var_os(CHILD_ENV) {
      let conn = deno_runtime::deno_webstorage::rusqlite::Connection::open(
        PathBuf::from(path),
      )
      .unwrap();
      conn
        .execute_batch(
          "PRAGMA journal_mode=WAL; CREATE TABLE t(v); INSERT INTO t VALUES (1);",
        )
        .unwrap();
      writeln!(std::io::stdout(), "holder ready").unwrap();
      std::io::stdout().flush().unwrap();
      // hold it until the parent closes stdin
      let mut line = String::new();
      while std::io::stdin().read_line(&mut line).unwrap_or(0) > 0 {}
      drop(conn);
      return;
    }

    let temp = TempDir::new();
    let dir = temp.path().as_path().to_path_buf();
    let base = dir.join("test_cache");
    let current = versioned_path(&base, "2.0");
    let held = dir.join("test_cache_1.0");
    let mut child = std::process::Command::new(
      std::env::current_exe().unwrap(),
    )
    .args([
      "--exact",
      "cache::cache_db_files::tests::keeps_a_database_another_process_holds",
      "--test-threads=1",
      "--nocapture",
    ])
    .env(CHILD_ENV, &held)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
      line.clear();
      assert!(stdout.read_line(&mut line).unwrap() > 0, "holder exited");
      if line.contains("holder ready") {
        break;
      }
    }

    let now = SystemTime::now();
    let age_all = || {
      for suffix in ["", "-wal", "-shm"] {
        let path = dir.join(format!("test_cache_1.0{suffix}"));
        if path.exists() {
          touch(&path, now - 8 * DAY);
        }
      }
    };
    age_all();
    let pass = prune_pass(&base, &current, now);
    assert!(
      held.exists(),
      "removed while another process held it: {pass}"
    );

    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert!(status.success());
    age_all();
    let pass = prune_pass(&base, &current, now);
    assert!(!held.exists(), "{pass}");
    assert!(!dir.join("test_cache_1.0-wal").exists());
    assert!(!dir.join("test_cache_1.0-shm").exists());
  }
}
