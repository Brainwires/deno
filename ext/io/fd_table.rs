// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::fs::File;

/// How an fd's lifetime is managed.
pub enum FdOwnership {
  /// FdTable owns the File; dropping the entry closes the fd.
  /// Used by fs.openSync, stdio fds 0/1/2, etc.
  TableOwned(Rc<dyn File>),
  /// An inherited extra stdio fd (fd >= 3 installed by the Node
  /// `child_process` spawn path) that can be used by node:fs, but may still be
  /// claimed later by libuv APIs such as net.Socket({ fd }).
  ///
  /// The `File` here is a `dup()` of the inherited descriptor, not the original
  /// numeric fd. Dropping this entry closes only the dup, so the original fd
  /// stays open and remains claimable by libuv (this is what lets node:fs and a
  /// later `net.Socket({ fd })` both work). The trade-off is that the original
  /// fd is retained for the process lifetime unless libuv reclaims it, which
  /// differs from Node, where node:fs `autoClose` closes the real fd.
  InheritedExtraStdio(Rc<dyn File>),
  /// A uv handle (e.g. uv_pipe_t) owns the fd; FdTable just tracks
  /// that it exists for duplicate detection. The entry is removed
  /// when uv_close fires, but no file is dropped.
  UvOwned,
  /// A raw fd returned by a Deno op specifically for later adoption by a
  /// libuv wrapper. FdTable does not own this fd; it only records that this
  /// isolate is allowed to claim it via `PipeWrap::open`/`TCPWrap::open`.
  Adoptable,
}

/// Central table tracking all known file descriptors.
///
/// Both Deno's resource table and Node's fd-based ops use this table
/// to look up files and detect duplicate registrations.
pub struct FdTable {
  entries: HashMap<i32, FdOwnership>,
  /// denext: this table's key in [`TABLE_OWNED_FDS`].
  id: u64,
}

static NEXT_TABLE_ID: AtomicU64 = AtomicU64::new(1);

/// denext: the TableOwned descriptors above stdio of every isolate's table
/// (the main thread's and each worker's), by the owning table's id, so a
/// libuv wrap in another isolate of the process can tell a descriptor one
/// of them opened (and had permission to open) from an unknown one. An entry
/// lives exactly as long as the owning table's entry: [`FdTable::remove`]
/// and the table's drop take it out.
static TABLE_OWNED_FDS: OnceLock<Mutex<HashMap<i32, u64>>> = OnceLock::new();

fn table_owned_fds() -> std::sync::MutexGuard<'static, HashMap<i32, u64>> {
  TABLE_OWNED_FDS
    .get_or_init(Default::default)
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

static INTERNAL_FDS: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();

/// Register a process-owned fd that must not be adopted by user fd APIs.
pub fn register_internal_fd(fd: i32) {
  INTERNAL_FDS
    .get_or_init(Default::default)
    .lock()
    .unwrap()
    .insert(fd);
}

impl FdTable {
  pub fn new() -> Self {
    Self {
      entries: HashMap::new(),
      id: NEXT_TABLE_ID.fetch_add(1, Ordering::Relaxed),
    }
  }

  /// Register a TableOwned fd. Returns false if already registered.
  pub fn register(&mut self, fd: i32, file: Rc<dyn File>) -> bool {
    if self.entries.contains_key(&fd) {
      return false;
    }
    self.entries.insert(fd, FdOwnership::TableOwned(file));
    if fd > 2 {
      table_owned_fds().insert(fd, self.id);
    }
    true
  }

  /// denext: whether `fd` is a TableOwned descriptor of another isolate's
  /// table (a worker's, or the main thread's from a worker).
  pub fn is_table_owned_elsewhere(&self, fd: i32) -> bool {
    !self.entries.contains_key(&fd)
      && table_owned_fds()
        .get(&fd)
        .is_some_and(|owner| *owner != self.id)
  }

  fn forget_table_owned(&self, fd: i32) {
    let mut fds = table_owned_fds();
    if fds.get(&fd) == Some(&self.id) {
      fds.remove(&fd);
    }
  }

  /// Register an inherited extra stdio fd. Returns false if already registered.
  pub fn register_inherited_extra_stdio(
    &mut self,
    fd: i32,
    file: Rc<dyn File>,
  ) -> bool {
    if self.entries.contains_key(&fd) {
      return false;
    }
    self
      .entries
      .insert(fd, FdOwnership::InheritedExtraStdio(file));
    true
  }

  /// Register a UvOwned fd (tracked but not owned). Returns false if
  /// already registered.
  pub fn register_uv_owned(&mut self, fd: i32) -> bool {
    if self.entries.contains_key(&fd) {
      return false;
    }
    self.entries.insert(fd, FdOwnership::UvOwned);
    true
  }

  /// Register an fd that may be adopted by a uv wrapper later. Returns false
  /// if already registered.
  pub fn register_uv_adoptable(&mut self, fd: i32) -> bool {
    if self.entries.contains_key(&fd) {
      return false;
    }
    self.entries.insert(fd, FdOwnership::Adoptable);
    true
  }

  /// Get the File for a TableOwned fd. Returns None for UvOwned or missing.
  pub fn get(&self, fd: i32) -> Option<&Rc<dyn File>> {
    match self.entries.get(&fd) {
      Some(FdOwnership::TableOwned(file)) => Some(file),
      Some(FdOwnership::InheritedExtraStdio(file)) => Some(file),
      Some(FdOwnership::UvOwned | FdOwnership::Adoptable) | None => None,
    }
  }

  /// Remove an fd entry. For TableOwned, returns the File (caller drops
  /// to close). For UvOwned, returns None (uv handle closes the fd).
  pub fn remove(&mut self, fd: i32) -> Option<Rc<dyn File>> {
    match self.entries.remove(&fd) {
      Some(FdOwnership::TableOwned(file)) => {
        self.forget_table_owned(fd);
        Some(file)
      }
      Some(FdOwnership::InheritedExtraStdio(file)) => Some(file),
      Some(FdOwnership::UvOwned) => None,
      Some(FdOwnership::Adoptable) => None,
      None => None,
    }
  }

  /// Check if an fd is registered (either ownership type).
  pub fn contains(&self, fd: i32) -> bool {
    if self.entries.contains_key(&fd) {
      return true;
    }
    INTERNAL_FDS
      .get()
      .is_some_and(|fds| fds.lock().unwrap().contains(&fd))
  }

  /// Check if an fd is an inherited extra stdio descriptor.
  pub fn is_inherited_extra_stdio(&self, fd: i32) -> bool {
    matches!(
      self.entries.get(&fd),
      Some(FdOwnership::InheritedExtraStdio(_))
    )
  }

  /// Check if an fd is a TableOwned entry: a descriptor this isolate opened
  /// itself (e.g. `fs.openSync` on a FIFO, a socket or a terminal).
  pub fn is_table_owned(&self, fd: i32) -> bool {
    matches!(self.entries.get(&fd), Some(FdOwnership::TableOwned(_)))
  }

  /// Check if an fd was explicitly registered as adoptable by a uv wrapper.
  pub fn is_uv_adoptable(&self, fd: i32) -> bool {
    matches!(self.entries.get(&fd), Some(FdOwnership::Adoptable))
  }

  /// Check whether a libuv stream wrap (`PipeWrap::open`, `TCPWrap::open`)
  /// may adopt `fd`. Stdio fds (0-2) may be re-opened; inherited extra stdio
  /// fds may be adopted (e.g. via `net.Socket({ fd })`) and are consumed by
  /// `finish_uv_adopt` only once the uv open actually claims the fd, so a
  /// failed open leaves the fd usable by node:fs. Fds explicitly registered
  /// as uv-adoptable by Deno internals may also be adopted. Any other fd is
  /// rejected.
  ///
  /// Unknown non-stdio fds are process-wide rather than isolate-local, so
  /// callers must reject them unless the current isolate has all permissions.
  ///
  /// Returns `Some(replace_registration)` if adoption may proceed (pass the
  /// flag to `finish_uv_adopt` on success), or `None` if the fd may not be
  /// adopted and the caller should reject it.
  pub fn begin_uv_adopt(&self, fd: i32, allow_untracked: bool) -> Option<bool> {
    if self.is_inherited_extra_stdio(fd) || self.is_uv_adoptable(fd) {
      Some(true)
    } else if (0..=2).contains(&fd) || (allow_untracked && !self.contains(fd)) {
      Some(false)
    } else {
      None
    }
  }

  /// Record a successful uv adoption started with `begin_uv_adopt`. Drops the
  /// inherited entry if there was one (libuv now owns the original fd; only
  /// the node:fs dup is released, and its close is deferred until any
  /// `Rc<dyn File>` clone held by an in-flight node:fs stream drops), then
  /// tracks the fd as UvOwned so it can't be re-adopted by another wrap.
  pub fn finish_uv_adopt(&mut self, fd: i32, replace_registration: bool) {
    if replace_registration {
      self.remove(fd);
    }
    self.register_uv_owned(fd);
  }
}

impl Default for FdTable {
  fn default() -> Self {
    Self::new()
  }
}

impl Drop for FdTable {
  fn drop(&mut self) {
    let mut fds = table_owned_fds();
    fds.retain(|_, owner| *owner != self.id);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn uv_adopt_rejects_unknown_non_stdio_fds() {
    let fd_table = FdTable::new();

    assert_eq!(fd_table.begin_uv_adopt(0, false), Some(false));
    assert_eq!(fd_table.begin_uv_adopt(1, false), Some(false));
    assert_eq!(fd_table.begin_uv_adopt(2, false), Some(false));
    assert_eq!(fd_table.begin_uv_adopt(3, false), None);
    assert_eq!(fd_table.begin_uv_adopt(40, false), None);
  }

  #[test]
  fn uv_adopt_rejects_already_uv_owned_fds() {
    let mut fd_table = FdTable::new();
    assert!(fd_table.register_uv_owned(40));

    assert_eq!(fd_table.begin_uv_adopt(40, false), None);
    assert_eq!(fd_table.begin_uv_adopt(40, true), None);
  }

  #[test]
  fn uv_adopt_allows_registered_adoptable_fds_once() {
    let mut fd_table = FdTable::new();
    assert!(fd_table.register_uv_adoptable(40));

    assert_eq!(fd_table.begin_uv_adopt(40, false), Some(true));
    fd_table.finish_uv_adopt(40, true);
    assert_eq!(fd_table.begin_uv_adopt(40, false), None);
  }

  #[test]
  fn table_owned_fds_are_visible_to_other_tables_while_owned() {
    // A number no other test registers.
    let fd = 9_871;
    let mut owner = FdTable::new();
    let other = FdTable::new();
    let file = || -> Rc<dyn File> {
      Rc::new(crate::StdFileResourceInner::file(
        std::fs::File::open(std::env::current_exe().unwrap()).unwrap(),
        None,
      ))
    };
    assert!(owner.register(fd, file()));
    assert!(!owner.is_table_owned_elsewhere(fd));
    assert!(other.is_table_owned_elsewhere(fd));
    drop(owner.remove(fd));
    assert!(!other.is_table_owned_elsewhere(fd));

    assert!(owner.register(fd, file()));
    assert!(other.is_table_owned_elsewhere(fd));
    drop(owner);
    assert!(!other.is_table_owned_elsewhere(fd));
  }

  #[test]
  fn uv_adopt_allows_untracked_fds_once_when_requested() {
    let mut fd_table = FdTable::new();

    assert_eq!(fd_table.begin_uv_adopt(40, true), Some(false));
    fd_table.finish_uv_adopt(40, false);
    assert_eq!(fd_table.begin_uv_adopt(40, true), None);
  }
}
