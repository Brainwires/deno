// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::From;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::OpState;
use deno_core::RcRef;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::ToV8;
use deno_core::op2;
use deno_core::parking_lot::Mutex;
use deno_error::JsErrorClass;
use deno_error::builtin_classes::GENERIC_ERROR;
use deno_permissions::PermissionsContainer;
use notify::Error as NotifyError;
use notify::EventKind;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::AccessKind;
use notify::event::AccessMode;
#[cfg(any(target_os = "linux", target_os = "android"))]
use notify::event::CreateKind;
use notify::event::Event as NotifyEvent;
use notify::event::ModifyKind;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

deno_core::extension!(
  deno_fs_events,
  ops = [op_fs_events_open, op_fs_events_poll],
);

/// Capacity of the queue between the notify backend thread and the JS-side
/// consumer. The watcher callback must not block, so when this queue is full
/// further events are dropped; the drop is recorded and surfaced to the
/// consumer as a `flag: "rescan"` event (see [`op_fs_events_poll`]) instead
/// of being silently lost (see denoland/deno#11373). The capacity is sized so
/// that ordinary bursts (e.g. unpacking an archive into a watched directory)
/// fit without dropping; tokio's bounded channel allocates lazily, so an
/// idle watcher does not pay for it.
const FS_EVENT_QUEUE_CAPACITY: usize = 1024;

struct FsEventsResource {
  receiver: AsyncRefCell<mpsc::Receiver<Result<FsEvent, NotifyError>>>,
  cancel: CancelHandle,
  /// Shared backend used to clean up our watch on drop.
  inner: Arc<WatcherInner>,
  /// Identifies our `WatchSender` entry in [`WatcherInner::senders`].
  id: u64,
  /// The (path, recursive_mode) pairs this resource registered. Tracked so the
  /// shared watcher unwatches them when the last interested resource closes.
  /// Emptied by [`FsEventsResource::release_watches`].
  watched: Mutex<Vec<(PathBuf, RecursiveMode)>>,
  /// Set by the watcher callback when the event queue overflowed and events
  /// were dropped; drained by `op_fs_events_poll`, which surfaces the loss as
  /// a `flag: "rescan"` event.
  overflowed: Arc<AtomicBool>,
}

impl FsEventsResource {
  /// Reference-count the underlying watches: only unwatch when no other
  /// resource still depends on this path. Without this, calling
  /// `Deno.watchFs(path)` repeatedly leaks watches in the shared
  /// `RecommendedWatcher` — on Windows each leaked watch registers a
  /// separate `ReadDirectoryChangesW` request, so the next watcher created
  /// for the same path receives every event N times.
  ///
  /// This runs on close and not only on drop: an in-flight
  /// `op_fs_events_poll` keeps a closed resource alive, and a watcher opened
  /// for the same path in the meantime would see a stale count and skip
  /// `watch()`, so it would never get events if the path was recreated (see
  /// denoland/deno#36937). It only releases once.
  fn release_watches(&self) {
    let watched = std::mem::take(&mut *self.watched.lock());
    if watched.is_empty() {
      return;
    }
    let mut failed = Vec::new();
    {
      let mut watched_paths = self.inner.watched_paths.lock();
      let mut watcher = self.inner.watcher.lock();
      for (path, mode) in &watched {
        let key = (path.clone(), *mode);
        let Some(count) = watched_paths.get_mut(&key) else {
          continue;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
          watched_paths.remove(&key);
          failed.extend(unwatch_shared(
            &mut watcher,
            &watched_paths,
            path,
            *mode,
          ));
        }
      }
    }
    report_backend_errors(&self.inner.senders, failed);
  }
}

impl Resource for FsEventsResource {
  fn name(&self) -> Cow<'_, str> {
    "fsEvents".into()
  }

  fn close(self: Rc<Self>) {
    self.cancel.cancel();
    self.release_watches();
  }
}

impl Drop for FsEventsResource {
  fn drop(&mut self) {
    // Remove this resource's sender from the shared dispatch list so the
    // watcher callback stops trying to deliver events to a dead channel.
    self.inner.senders.lock().retain(|ws| ws.id != self.id);

    self.release_watches();
  }
}

/// Represents a file system event.
///
/// We do not use the event directly from the notify crate. We flatten
/// the structure into this simpler structure. We want to only make it more
/// complex as needed.
///
/// Feel free to expand this struct as long as you can add tests to demonstrate
/// the complexity.
#[derive(ToV8, Debug, Clone)]
struct FsEvent {
  kind: &'static str,
  #[to_v8(serde)]
  paths: Vec<PathBuf>,
  flag: Option<&'static str>,
}

impl From<NotifyEvent> for FsEvent {
  fn from(e: NotifyEvent) -> Self {
    let kind = match e.kind {
      EventKind::Any => "any",
      EventKind::Access(_) => "access",
      EventKind::Create(_) => "create",
      EventKind::Modify(modify_kind) => match modify_kind {
        ModifyKind::Name(_) => "rename",
        ModifyKind::Any
        | ModifyKind::Data(_)
        | ModifyKind::Metadata(_)
        | ModifyKind::Other => "modify",
      },
      EventKind::Remove(_) => "remove",
      EventKind::Other => "other",
    };
    let flag = e.flag().map(|f| match f {
      notify::event::Flag::Rescan => "rescan",
    });
    FsEvent {
      kind,
      paths: e.paths,
      flag,
    }
  }
}

fn is_ignored_notify_event(event: &NotifyEvent) -> bool {
  // notify 8 added inotify OPEN events to the default watch mask. Deno did not
  // expose these with notify 6, and forwarding them can self-amplify because
  // our path filtering performs filesystem reads.
  matches!(
    event.kind,
    EventKind::Access(AccessKind::Open(AccessMode::Any))
  )
}

struct WatchSender {
  /// Unique identifier so the matching entry can be removed when the
  /// owning [`FsEventsResource`] is dropped.
  id: u64,
  /// Original paths as provided by the caller.
  paths: Vec<PathBuf>,
  /// Pre-canonicalized versions of `paths`, computed once at watch time
  /// to avoid repeated syscalls in the event callback hot path.
  /// Entries are `None` if canonicalization failed for that path.
  canonical_paths: Vec<Option<PathBuf>>,
  /// Paths whose events should be filtered out (the `ignore` option).
  ignore: Vec<PathBuf>,
  /// Pre-canonicalized versions of `ignore`, mirroring `canonical_paths`.
  canonical_ignore: Vec<Option<PathBuf>>,
  sender: mpsc::Sender<Result<FsEvent, NotifyError>>,
  /// See [`FsEventsResource::overflowed`].
  overflowed: Arc<AtomicBool>,
  /// The permissions of the runtime that opened the watch, which every
  /// reported path is checked against (see [`WatchSender::readable_event`]).
  permissions: Option<PermissionsContainer>,
  /// Whether the watch is recursive. A non-recursive watcher is told only
  /// about its watched paths and their direct children (see
  /// [`event_matches_watched_paths`]); only recursive watchers are told
  /// about what the inotify new-directory scan finds in a new directory.
  recursive: bool,
}

impl WatchSender {
  /// denext: the event with only the paths the watching runtime may read,
  /// or `None` when none is left.
  ///
  /// The watch was permission-checked on the paths it names, but a recursive
  /// watch reports on everything below them: a `--deny-read` or
  /// `--ignore-read` scope inside the watched tree, and, where the backend
  /// follows symlinks while walking the tree (inotify and kqueue unless
  /// told not to, see [`ensure_watcher`]; FSEvents and
  /// `ReadDirectoryChangesW` report the paths they see changing), whatever
  /// a link leads to. An event path is passed on only when reading it is
  /// granted, by the path as reported and by the path it resolves to, so a
  /// denied file's name, and the fact that it changed, stay hidden. The
  /// check never prompts.
  fn readable_event(&self, event: &FsEvent) -> Option<FsEvent> {
    let Some(permissions) = &self.permissions else {
      return Some(event.clone());
    };
    if event.paths.is_empty() {
      return Some(event.clone());
    }
    let paths: Vec<PathBuf> = event
      .paths
      .iter()
      .filter(|path| permissions.query_read_reachable(path))
      .cloned()
      .collect();
    if paths.is_empty() {
      return None;
    }
    Some(FsEvent {
      kind: event.kind,
      paths,
      flag: event.flag,
    })
  }

  /// denext: the watcher error with only the paths the watching runtime may
  /// read, as [`Self::readable_event`] does for events, or `None` when it
  /// named paths and none is left (an error about a denied path says that
  /// the path exists). An error that names no path is delivered as is.
  fn readable_error(&self, err: &NotifyError) -> Option<NotifyError> {
    let mut err = clone_notify_error(err);
    let Some(permissions) = &self.permissions else {
      return Some(err);
    };
    if err.paths.is_empty() {
      return Some(err);
    }
    err
      .paths
      .retain(|path| permissions.query_read_reachable(path));
    if err.paths.is_empty() {
      return None;
    }
    Some(err)
  }

  /// Deliver an event or error without blocking the notify backend thread.
  /// If the queue is full the message is dropped and the overflow is
  /// recorded so the consumer learns about the loss via a `"rescan"` event.
  /// A closed channel just means the watcher resource is gone; that is not
  /// a loss.
  fn send_or_record_overflow(&self, msg: Result<FsEvent, NotifyError>) {
    if let Err(TrySendError::Full(_)) = self.sender.try_send(msg) {
      self.overflowed.store(true, Ordering::Relaxed);
    }
  }
}

/// `notify::Error` is not `Clone` (it can wrap an `std::io::Error`), but the
/// shared watcher callback has to deliver an error to every interested
/// resource, so rebuild an equivalent error for each receiver. For I/O errors
/// the original `ErrorKind` and message are preserved.
fn clone_notify_error(err: &NotifyError) -> NotifyError {
  let kind = match &err.kind {
    notify::ErrorKind::Generic(msg) => notify::ErrorKind::Generic(msg.clone()),
    notify::ErrorKind::Io(io_err) => notify::ErrorKind::Io(
      std::io::Error::new(io_err.kind(), io_err.to_string()),
    ),
    notify::ErrorKind::PathNotFound => notify::ErrorKind::PathNotFound,
    notify::ErrorKind::WatchNotFound => notify::ErrorKind::WatchNotFound,
    notify::ErrorKind::InvalidConfig(config) => {
      notify::ErrorKind::InvalidConfig(*config)
    }
    notify::ErrorKind::MaxFilesWatch => notify::ErrorKind::MaxFilesWatch,
  };
  NotifyError {
    kind,
    paths: err.paths.clone(),
  }
}

struct WatcherInner {
  /// Per-watcher dispatch list. The shared notify callback iterates this
  /// list on every event. Wrapped in `Arc<Mutex<...>>` so the watcher
  /// callback (which lives on the notify backend thread) can hold a
  /// reference without keeping the rest of [`WatcherInner`] alive.
  senders: Arc<Mutex<Vec<WatchSender>>>,
  /// The shared `RecommendedWatcher` instance backing every
  /// `Deno.watchFs(...)` call in this OpState.
  watcher: Mutex<RecommendedWatcher>,
  /// Reference count per (path, recursive_mode) registered with `watcher`.
  /// When a count drops to zero, the path is unwatched.
  watched_paths: Mutex<HashMap<(PathBuf, RecursiveMode), usize>>,
  /// Monotonic counter for assigning `WatchSender::id`.
  next_id: AtomicU64,
}

struct WatcherState {
  inner: Arc<WatcherInner>,
}

#[allow(
  clippy::disallowed_methods,
  reason = "always using real fs with watcher"
)]
fn canonicalize_path(path: &Path) -> Option<PathBuf> {
  path.canonicalize().ok()
}

/// Check if `event_path` (or its canonicalized form) matches one of the
/// watched paths. The watched paths are pre-canonicalized to avoid
/// repeated syscalls in the hot path.
///
/// denext: with `recursive` an event anywhere below a watched directory
/// matches; without it only the watched path itself and its direct
/// children do. The notify backend is shared by every watcher of the
/// runtime, so a non-recursive watch whose directory is also under a
/// recursive watch (`Deno.watchFs(root)` beside
/// `Deno.watchFs(root + "/sub", { recursive: false })`) is handed the
/// recursive watch's nested events, and a prefix match passed them on.
fn event_matches_watched_paths(
  event_path: &Path,
  paths: &[PathBuf],
  canonical_paths: &[Option<PathBuf>],
  recursive: bool,
) -> bool {
  // Canonicalize the event path (and, for a non-recursive watch, its
  // parent) at most once per call.
  let canonical_event_path = canonicalize_path(event_path);
  let mut canonical_parent: Option<Option<PathBuf>> = None;
  for (path, canonical_path) in paths.iter().zip(canonical_paths.iter()) {
    if same_file::is_same_file(event_path, path).unwrap_or(false) {
      return true;
    }
    let Some(cp) = canonical_path else {
      continue;
    };
    if recursive {
      if canonical_event_path
        .as_ref()
        .is_some_and(|ce| ce.starts_with(cp))
      {
        return true;
      }
      continue;
    }
    if canonical_event_path.as_ref() == Some(cp) {
      return true;
    }
    // A direct child: its parent is the watched directory. The parent is
    // compared rather than the canonical event path, so a symlink in the
    // directory counts as its child wherever it leads.
    let parent = canonical_parent
      .get_or_insert_with(|| event_path.parent().and_then(canonicalize_path));
    if parent.as_ref() == Some(cp) {
      return true;
    }
  }
  false
}

/// Check if `event_path` refers to a file that has been removed and
/// that file is within one of the watched paths. This is needed because
/// `event_matches_watched_paths` will fail for removed files (canonicalize
/// and is_same_file don't work on non-existent paths). On macOS with
/// FSEvents, remove events may arrive as generic events for a path that
/// no longer exists. Without `recursive`, only a removed direct child (or
/// the watched path itself) matches.
fn removed_event_matches_watched_paths(
  event_path: &Path,
  paths: &[PathBuf],
  canonical_paths: &[Option<PathBuf>],
  recursive: bool,
) -> bool {
  if !is_file_removed(event_path) {
    return false;
  }
  let canonical_parent = event_path.parent().and_then(canonicalize_path);
  for (path, canonical_path) in paths.iter().zip(canonical_paths.iter()) {
    // Direct path comparison: the file is gone so is_same_file won't work,
    // but the event path may match the watched path or its canonical form
    // exactly (e.g. when watching a single file that gets deleted).
    if event_path == path {
      return true;
    }
    if canonical_path
      .as_ref()
      .is_some_and(|cp| event_path == cp.as_path())
    {
      return true;
    }
    // Check if the removed file's parent is within (non-recursive: is) a
    // watched directory.
    if let (Some(cp_event), Some(cp_watched)) =
      (&canonical_parent, canonical_path)
      && (cp_event == cp_watched
        || (recursive && cp_event.starts_with(cp_watched)))
    {
      return true;
    }
  }
  false
}

fn is_file_removed(event_path: &Path) -> bool {
  let exists_path = std::fs::exists(event_path);
  match exists_path {
    Ok(res) => !res,
    Err(_) => false,
  }
}

// The notify crate's Windows backend reports a missing watch path as a
// Generic error with this message (see `add_watch` in notify's
// src/windows.rs), unlike the inotify backend which reports PathNotFound.
// Normalize it so a watch on a vanished path surfaces as NotFound on all
// platforms; the node:fs `fs.watch` polyfill relies on this to emit a
// recoverable ENOENT instead of an unrecognized error that kills watchers
// like chokidar/vite (see denoland/deno#35855).
const WINDOWS_PATH_NOT_FOUND_MSG: &str =
  "Input watch path is neither a file nor a directory.";

deno_error::js_error_wrapper!(NotifyError, JsNotifyError, |err| {
  match &err.kind {
    notify::ErrorKind::Generic(msg) if msg == WINDOWS_PATH_NOT_FOUND_MSG => {
      "NotFound".into()
    }
    notify::ErrorKind::Generic(_) => GENERIC_ERROR.into(),
    notify::ErrorKind::Io(e) => e.get_class(),
    notify::ErrorKind::PathNotFound => "NotFound".into(),
    notify::ErrorKind::WatchNotFound => "NotFound".into(),
    notify::ErrorKind::InvalidConfig(_) => "InvalidData".into(),
    notify::ErrorKind::MaxFilesWatch => GENERIC_ERROR.into(),
  }
});

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum FsEventsError {
  #[class(inherit)]
  #[error(transparent)]
  Resource(#[from] deno_core::error::ResourceError),
  #[class(inherit)]
  #[error(transparent)]
  Permission(#[from] deno_permissions::PermissionCheckError),
  #[class(inherit)]
  #[error(transparent)]
  Notify(JsNotifyError),
  #[class(inherit)]
  #[error(transparent)]
  Canceled(#[from] deno_core::Canceled),
}

fn make_watch_sender(
  id: u64,
  paths: Vec<PathBuf>,
  ignore: Vec<PathBuf>,
  sender: mpsc::Sender<Result<FsEvent, NotifyError>>,
  overflowed: Arc<AtomicBool>,
  permissions: Option<PermissionsContainer>,
  recursive: bool,
) -> WatchSender {
  let canonical_paths = paths.iter().map(|p| canonicalize_path(p)).collect();
  let canonical_ignore = ignore.iter().map(|p| canonicalize_path(p)).collect();
  WatchSender {
    id,
    paths,
    canonical_paths,
    ignore,
    canonical_ignore,
    sender,
    overflowed,
    permissions,
    recursive,
  }
}

/// Hands one backend event or error to one watcher: an event only when it
/// concerns the watcher's paths (and not only its `ignore` paths), each with
/// only the paths the watcher's runtime may read.
fn dispatch(ws: &WatchSender, res: &Result<FsEvent, NotifyError>) {
  match res {
    // Only send the event if the path matches one of the paths
    // that the user is watching.
    Ok(event) => {
      // Skip events whose paths all fall under one of the ignored
      // paths (the `ignore` option). Removed files are checked too so
      // deletions inside an ignored directory don't leak through
      // (canonicalize/is_same_file fail for paths that no longer
      // exist). The `is_empty` guard keeps this off the event-callback
      // hot path for the common case where `ignore` is unused.
      //
      // `all` (not `any`) means an event is dropped only when every
      // path is ignored, so a rename out of an ignored dir into a
      // watched one (paths = [from, to] with `from` ignored and `to`
      // watched) is still delivered. The reverse case (rename into an
      // ignored dir) still delivers an event containing the ignored
      // `to` path; that is intentional.
      if !ws.ignore.is_empty()
        && !event.paths.is_empty()
        && event.paths.iter().all(|event_path| {
          // An ignored directory is ignored with everything below it.
          event_matches_watched_paths(
            event_path,
            &ws.ignore,
            &ws.canonical_ignore,
            true,
          ) || removed_event_matches_watched_paths(
            event_path,
            &ws.ignore,
            &ws.canonical_ignore,
            true,
          )
        })
      {
        return;
      }
      if event.paths.iter().any(|event_path| {
        event_matches_watched_paths(
          event_path,
          &ws.paths,
          &ws.canonical_paths,
          ws.recursive,
        )
      }) {
        if let Some(event) = ws.readable_event(event) {
          ws.send_or_record_overflow(Ok(event));
        }
      } else if event.paths.iter().any(|event_path| {
        removed_event_matches_watched_paths(
          event_path,
          &ws.paths,
          &ws.canonical_paths,
          ws.recursive,
        )
      }) {
        let remove_event = FsEvent {
          kind: "remove",
          paths: event.paths.clone(),
          flag: None,
        };
        if let Some(event) = ws.readable_event(&remove_event) {
          ws.send_or_record_overflow(Ok(event));
        }
      }
    }
    // Watcher errors are not reliably path-scoped (their `paths` are
    // often empty), so deliver them to every watcher rather than
    // dropping them on the floor, with the paths this watcher may not
    // read left out.
    Err(err) => {
      if let Some(err) = ws.readable_error(err) {
        ws.send_or_record_overflow(Err(err));
      }
    }
  }
}

fn ensure_watcher(
  state: &mut OpState,
) -> Result<Arc<WatcherInner>, FsEventsError> {
  if let Some(ws) = state.try_borrow::<WatcherState>() {
    return Ok(ws.inner.clone());
  }

  let senders: Arc<Mutex<Vec<WatchSender>>> = Arc::new(Mutex::new(Vec::new()));
  let sender_clone = senders.clone();
  #[cfg(any(target_os = "linux", target_os = "android"))]
  let scanner = Arc::new(new_dir_scan::NewDirScanner::new(senders.clone()));
  #[cfg(any(target_os = "linux", target_os = "android"))]
  let mut scan_queue = new_dir_scan::ScanQueue::new(scanner.clone());
  let watcher: RecommendedWatcher = Watcher::new(
    move |res: Result<NotifyEvent, NotifyError>| {
      #[cfg(any(target_os = "linux", target_os = "android"))]
      let new_dir = match &res {
        Ok(NotifyEvent {
          kind: EventKind::Create(CreateKind::Folder),
          paths,
          ..
        }) if paths.len() == 1 => Some(paths[0].clone()),
        _ => None,
      };
      let res2 = match res {
        Ok(event) if is_ignored_notify_event(&event) => return,
        Ok(event) => Ok(FsEvent::from(event)),
        Err(e) => Err(e),
      };
      #[cfg(any(target_os = "linux", target_os = "android"))]
      {
        // Held back while a new directory above its path waits for its scan,
        // so the scan's events come first.
        if scan_queue.hold_back(&res2) {
          if let Some(dir) = new_dir {
            scan_queue.add(dir);
          }
          return;
        }
      }
      for ws in sender_clone.lock().iter() {
        dispatch(ws, &res2);
      }
      #[cfg(any(target_os = "linux", target_os = "android"))]
      if let Some(dir) = new_dir {
        scan_queue.add(dir);
      }
    },
    // denext: don't walk into symlinked directories while setting up a
    // recursive watch (honored by the inotify and kqueue backends, which
    // otherwise add a watch on every directory a link inside the tree leads
    // to). Events from a link's target were never wanted: they are reported
    // under a path outside the watched tree, or inside a deny scope, and
    // `WatchSender::readable_event` drops whatever still gets through. A
    // watched path that is itself a link is still followed.
    notify::Config::default().with_follow_symlinks(false),
  )
  .map_err(|e| FsEventsError::Notify(JsNotifyError(e)))?;

  let inner = Arc::new(WatcherInner {
    senders,
    watcher: Mutex::new(watcher),
    watched_paths: Mutex::new(HashMap::new()),
    next_id: AtomicU64::new(0),
  });
  #[cfg(any(target_os = "linux", target_os = "android"))]
  scanner.set_watcher(&inner);

  state.put::<WatcherState>(WatcherState {
    inner: inner.clone(),
  });

  Ok(inner)
}

/// denext: what a recursive inotify watch misses in a new directory.
///
/// inotify has no recursive watch: notify's backend reports a new directory's
/// `Create` event to the handler and only then, once the batch it read is
/// handled, adds a watch on the directory (`add_watch` after `handle_event` in
/// notify's `inotify.rs`). Whatever is created in the directory before that
/// watch exists is never reported, and a file whose creation fell before the
/// watch but whose write fell after is reported only as modified. `mkdir` and
/// a write into the new directory right after it (node:fs's
/// test-fs-watch-recursive-add-file-to-new-folder.js) lost the file's event
/// (a timeout) or saw `change` instead of `rename`, many runs in a hundred on
/// a loaded Linux machine. Node.js's own recursive watcher on Linux reads a
/// new directory when it appears and reports each entry it hasn't seen as
/// `rename` (lib/internal/fs/recursive_watch.js).
///
/// Here a new directory under a recursive watch is read on a helper thread
/// once the backend has added its watch: a configure call is answered by the
/// backend's event loop after it finished the batch that reported the
/// directory, so by then the watch exists and everything created later is
/// reported by inotify. Each entry found is delivered as a `create` event to
/// the recursive watchers. Until then the events under the directory are
/// held back, so a modify inotify saw never overtakes the `create` the read
/// found; a `create` inotify reported in that window is not repeated.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod new_dir_scan {
  use std::collections::HashSet;
  use std::path::Path;
  use std::path::PathBuf;
  use std::sync::Arc;
  use std::sync::OnceLock;
  use std::sync::Weak;

  use deno_core::parking_lot::Mutex;
  use notify::Error as NotifyError;
  use notify::Watcher;

  use super::FsEvent;
  use super::WatchSender;
  use super::WatcherInner;
  use super::dispatch;

  /// At most this many entries are read from one new directory tree; a
  /// larger tree is mostly created after its watch anyway.
  const MAX_SCANNED_ENTRIES: usize = 16 * 1024;

  struct PendingDir {
    dir: PathBuf,
    /// Events under `dir` that arrived before its scan was delivered.
    held: Vec<FsEvent>,
  }

  pub(super) struct NewDirScanner {
    senders: Arc<Mutex<Vec<WatchSender>>>,
    pending: Mutex<Vec<PendingDir>>,
    watcher: OnceLock<Weak<WatcherInner>>,
  }

  impl NewDirScanner {
    pub(super) fn new(senders: Arc<Mutex<Vec<WatchSender>>>) -> Self {
      Self {
        senders,
        pending: Mutex::new(Vec::new()),
        watcher: OnceLock::new(),
      }
    }

    pub(super) fn set_watcher(&self, inner: &Arc<WatcherInner>) {
      let _ = self.watcher.set(Arc::downgrade(inner));
    }

    fn has_recursive_watcher(&self) -> bool {
      self.senders.lock().iter().any(|ws| ws.recursive)
    }

    /// Holds `res` back when one of its paths is inside a directory waiting
    /// for its scan (the deepest such directory, whose scan comes last).
    fn hold_back(&self, res: &Result<FsEvent, NotifyError>) -> bool {
      let Ok(event) = res else {
        return false;
      };
      let mut pending = self.pending.lock();
      if pending.is_empty() {
        return false;
      }
      let inside = |dir: &Path| {
        event
          .paths
          .iter()
          .any(|path| path != dir && path.starts_with(dir))
      };
      let Some(pd) = pending
        .iter_mut()
        .filter(|pd| inside(&pd.dir))
        .max_by_key(|pd| pd.dir.components().count())
      else {
        return false;
      };
      pd.held.push(event.clone());
      true
    }

    /// Waits until the backend's event loop has handled every event it read
    /// before this call, and so added the watches of the directories they
    /// reported. notify's inotify backend answers `configure` from its event
    /// loop and applies nothing (the configuration it was created with is
    /// passed again regardless).
    fn barrier(&self) {
      let Some(inner) = self.watcher.get().and_then(Weak::upgrade) else {
        return;
      };
      let _ = inner
        .watcher
        .lock()
        .configure(notify::Config::default().with_follow_symlinks(false));
    }

    fn scan_and_deliver(&self, dir: &Path) {
      let skip: Vec<PathBuf> = self
        .pending
        .lock()
        .iter()
        .filter(|pd| pd.dir != dir && pd.dir.starts_with(dir))
        .map(|pd| pd.dir.clone())
        .collect();
      let mut found = Vec::new();
      scan(dir, &skip, &mut found);

      let mut pending = self.pending.lock();
      let held = match pending.iter().position(|pd| pd.dir == dir) {
        Some(i) => pending.remove(i).held,
        None => Vec::new(),
      };
      let reported: HashSet<&Path> = held
        .iter()
        .filter(|event| event.kind == "create")
        .flat_map(|event| event.paths.iter().map(PathBuf::as_path))
        .collect();
      let senders = self.senders.lock();
      for path in found {
        if reported.contains(path.as_path()) {
          continue;
        }
        let event = Ok(FsEvent {
          kind: "create",
          paths: vec![path],
          flag: None,
        });
        for ws in senders.iter().filter(|ws| ws.recursive) {
          dispatch(ws, &event);
        }
      }
      for event in held {
        let event = Ok(event);
        for ws in senders.iter() {
          dispatch(ws, &event);
        }
      }
    }
  }

  /// Every entry below `dir`, depth first, not following symlinks and not
  /// entering the directories in `skip` (which get their own scan).
  #[allow(
    clippy::disallowed_methods,
    reason = "always using real fs with watcher"
  )]
  fn scan(dir: &Path, skip: &[PathBuf], found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      if found.len() >= MAX_SCANNED_ENTRIES {
        return;
      }
      let path = entry.path();
      let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
      found.push(path.clone());
      if is_dir && !skip.contains(&path) {
        scan(&path, skip, found);
      }
    }
  }

  /// The backend thread's side: owned by the watcher callback, so the helper
  /// thread ends when the watcher (and with it the callback) is dropped.
  pub(super) struct ScanQueue {
    scanner: Arc<NewDirScanner>,
    sender: Option<std::sync::mpsc::Sender<PathBuf>>,
    spawn_failed: bool,
  }

  impl ScanQueue {
    pub(super) fn new(scanner: Arc<NewDirScanner>) -> Self {
      Self {
        scanner,
        sender: None,
        spawn_failed: false,
      }
    }

    pub(super) fn hold_back(&self, res: &Result<FsEvent, NotifyError>) -> bool {
      self.scanner.hold_back(res)
    }

    /// Queues the scan of `dir`, a directory the backend just reported as
    /// created, when a recursive watcher may want its contents.
    pub(super) fn add(&mut self, dir: PathBuf) {
      if self.spawn_failed || !self.scanner.has_recursive_watcher() {
        return;
      }
      if self.sender.is_none() {
        let (sender, receiver) = std::sync::mpsc::channel::<PathBuf>();
        let scanner = self.scanner.clone();
        let spawned = std::thread::Builder::new()
          .name("deno-fs-events-scan".into())
          .spawn(move || {
            while let Ok(first) = receiver.recv() {
              let mut dirs = vec![first];
              while let Ok(dir) = receiver.try_recv() {
                dirs.push(dir);
              }
              scanner.barrier();
              for dir in dirs {
                scanner.scan_and_deliver(&dir);
              }
            }
          });
        if spawned.is_err() {
          self.spawn_failed = true;
          return;
        }
        self.sender = Some(sender);
      }
      self.scanner.pending.lock().push(PendingDir {
        dir: dir.clone(),
        held: Vec::new(),
      });
      if let Some(sender) = &self.sender
        && sender.send(dir.clone()).is_err()
      {
        // The helper is gone: deliver what was held instead of keeping it.
        self.scanner.scan_and_deliver(&dir);
      }
    }
  }
}

/// Make `path` absolute and collapse `.` / `..` segments so that paths
/// reported back in `FsEvent` don't carry the leftover relative bits notify
/// pastes onto its event paths (see denoland/deno#32000). Symlinks are
/// intentionally not resolved here so user-visible event paths still reflect
/// the path the caller passed in.
fn normalize_watch_path(path: PathBuf) -> PathBuf {
  if path.is_absolute() {
    return deno_path_util::normalize_path(Cow::Owned(path)).into_owned();
  }
  #[allow(
    clippy::disallowed_methods,
    reason = "fs watcher needs the real cwd to absolutize the watch path"
  )]
  let cwd = std::env::current_dir();
  match cwd {
    Ok(cwd) => {
      deno_path_util::normalize_path(Cow::Owned(cwd.join(&path))).into_owned()
    }
    Err(_) => path,
  }
}

#[op2(stack_trace)]
#[smi]
fn op_fs_events_open(
  state: &mut OpState,
  recursive: bool,
  #[scoped] ignore: Vec<String>,
  #[scoped] paths: Vec<String>,
) -> Result<ResourceId, FsEventsError> {
  let mut resolved_paths = Vec::with_capacity(paths.len());
  let mut ignore_paths = Vec::with_capacity(ignore.len());
  {
    let permissions_container = state.borrow_mut::<PermissionsContainer>();
    for path in ignore {
      let checked = permissions_container
        .check_open(
          Cow::Owned(PathBuf::from(path)),
          deno_permissions::OpenAccessKind::ReadNoFollow,
          Some("Deno.watchFs()"),
        )?
        .into_owned_path();
      ignore_paths.push(normalize_watch_path(checked));
    }
    for path in paths {
      // denext (DENEXT.md 286): the watcher follows a watched path that is
      // a symlink and reports on its target, so the path is checked as an
      // access that follows the final link (its target must be allowed);
      // the path as given is still the one watched, so events keep the
      // caller's spelling.
      permissions_container.check_open(
        Cow::Borrowed(Path::new(&path)),
        deno_permissions::OpenAccessKind::Read,
        Some("Deno.watchFs()"),
      )?;
      let checked = permissions_container
        .check_open(
          Cow::Owned(PathBuf::from(path)),
          deno_permissions::OpenAccessKind::ReadNoFollow,
          Some("Deno.watchFs()"),
        )?
        .into_owned_path();
      resolved_paths.push(normalize_watch_path(checked));
    }
  }

  open_fs_events_resource(state, resolved_paths, ignore_paths, recursive)
}

fn open_fs_events_resource(
  state: &mut OpState,
  resolved_paths: Vec<PathBuf>,
  ignore_paths: Vec<PathBuf>,
  recursive: bool,
) -> Result<ResourceId, FsEventsError> {
  let (sender, receiver) =
    mpsc::channel::<Result<FsEvent, NotifyError>>(FS_EVENT_QUEUE_CAPACITY);
  let overflowed = Arc::new(AtomicBool::new(false));

  let inner = ensure_watcher(state)?;
  let permissions = state.try_borrow::<PermissionsContainer>().cloned();

  let id = inner.next_id.fetch_add(1, Ordering::Relaxed);

  inner.senders.lock().push(make_watch_sender(
    id,
    resolved_paths.clone(),
    ignore_paths,
    sender,
    overflowed.clone(),
    permissions,
    recursive,
  ));

  let recursive_mode = if recursive {
    RecursiveMode::Recursive
  } else {
    RecursiveMode::NonRecursive
  };

  // Register each path with the shared watcher exactly once per
  // (path, mode) pair. Subsequent resources requesting the same
  // (path, mode) bump the refcount but skip the `watch` syscall.
  // This is the core of the duplicate-event fix on Windows (see
  // denoland/deno#27742): otherwise repeated `watch` calls register
  // duplicate ReadDirectoryChangesW operations whose callbacks all
  // fire on every change.
  let mut watched = Vec::with_capacity(resolved_paths.len());
  let mut failed = Vec::new();
  {
    let mut watched_paths = inner.watched_paths.lock();
    let mut watcher = inner.watcher.lock();
    for path in &resolved_paths {
      let key = (path.clone(), recursive_mode);
      let count = watched_paths.entry(key.clone()).or_insert(0);
      if *count == 0
        && let Err(e) = watcher.watch(path, recursive_mode)
      {
        // Roll back any partial state we accumulated for this call so
        // a failed open doesn't leave dangling refcounts/senders.
        watched_paths.remove(&key);
        drop(watcher);
        drop(watched_paths);
        rollback_partial_open(&inner, id, &watched);
        return Err(FsEventsError::Notify(JsNotifyError(e)));
      }
      let added = *count == 0;
      *count += 1;
      watched.push((path.clone(), recursive_mode));
      if added && SHARES_BACKEND_WATCHES {
        failed.extend(apply_backend_steps(
          &mut watcher,
          backend_steps_after_watch(&watched_paths, path, recursive_mode),
        ));
      }
    }
  }
  report_backend_errors(&inner.senders, failed);

  let resource = FsEventsResource {
    receiver: AsyncRefCell::new(receiver),
    cancel: Default::default(),
    inner,
    id,
    watched: Mutex::new(watched),
    overflowed,
  };
  let rid = state.resource_table.add(resource);
  Ok(rid)
}

/// denext: whether the backend shares one watch between overlapping paths.
/// inotify watches inodes, not (path, mode) pairs: notify's backend keeps one
/// watch (and one "recursive" mark) per directory, so watches on overlapping
/// paths share it. Other backends watch each path on its own and are left
/// alone (re-adding a path on Windows would add a second
/// `ReadDirectoryChangesW` request, denoland/deno#27742).
const SHARES_BACKEND_WATCHES: bool =
  cfg!(any(target_os = "linux", target_os = "android"));

/// denext: one request to the shared backend.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BackendStep {
  Watch(PathBuf, RecursiveMode),
  Unwatch(PathBuf),
}

/// Whether a registered recursive watch holds `path` (at it or above it).
fn held_by_a_recursive_watch(
  registered: &HashMap<(PathBuf, RecursiveMode), usize>,
  path: &Path,
) -> bool {
  registered
    .keys()
    .any(|(p, mode)| *mode == RecursiveMode::Recursive && path.starts_with(p))
}

/// denext: what a backend that shares watches is asked for after `path` was
/// registered and watched with `mode` (`registered` holds it). Watching
/// `dir/sub` non-recursively marks its directory non-recursive, so
/// directories made in it later would no longer be watched for a recursive
/// watch at or above it: the mark is put back by watching `path` itself
/// recursively. That walks what lies beneath `path` and nothing else: never
/// the recursive watch's whole tree, which a program that opens a watcher
/// per directory under one recursive watch paid for at every open.
fn backend_steps_after_watch(
  registered: &HashMap<(PathBuf, RecursiveMode), usize>,
  path: &Path,
  mode: RecursiveMode,
) -> Vec<BackendStep> {
  if mode == RecursiveMode::NonRecursive
    && held_by_a_recursive_watch(registered, path)
  {
    vec![BackendStep::Watch(
      path.to_path_buf(),
      RecursiveMode::Recursive,
    )]
  } else {
    Vec::new()
  }
}

/// denext: what a backend that shares watches is asked for when the last
/// registration of `path` with `mode` goes (`registered` no longer holds
/// it; `is_dir`: `path` is a directory now).
///
/// - A non-recursive watch of a directory a recursive watch still holds:
///   nothing. The directory's watch is the recursive one's too, and
///   unwatching it would take the directory, and (marked recursive) all
///   beneath it, out of that watch.
/// - Any other non-recursive watch: unwatched. Its mark is non-recursive,
///   so nothing else goes with it.
/// - A recursive watch: unwatched, which removes the watch of everything
///   beneath `path`; then the watches still registered at or beneath it are
///   applied again (the non-recursive ones first, so the recursive ones
///   leave their directories marked recursive), and when a recursive watch
///   above still holds `path`, `path` is watched recursively again for it.
///
/// Nothing above `path` is walked: what a removal takes from the backend
/// lies at `path` or beneath it.
fn backend_steps_to_unwatch(
  registered: &HashMap<(PathBuf, RecursiveMode), usize>,
  path: &Path,
  mode: RecursiveMode,
  is_dir: bool,
) -> Vec<BackendStep> {
  let held = held_by_a_recursive_watch(registered, path);
  if mode == RecursiveMode::NonRecursive {
    if held && is_dir {
      return Vec::new();
    }
    return vec![BackendStep::Unwatch(path.to_path_buf())];
  }
  let mut steps = vec![BackendStep::Unwatch(path.to_path_buf())];
  let mut beneath: Vec<&(PathBuf, RecursiveMode)> = registered
    .keys()
    .filter(|(p, _)| p.starts_with(path))
    .collect();
  beneath.sort_by(|(a, a_mode), (b, b_mode)| {
    (*a_mode == RecursiveMode::Recursive, a)
      .cmp(&(*b_mode == RecursiveMode::Recursive, b))
  });
  steps.extend(
    beneath
      .into_iter()
      .map(|(p, mode)| BackendStep::Watch(p.clone(), *mode)),
  );
  if held {
    steps.push(BackendStep::Watch(
      path.to_path_buf(),
      RecursiveMode::Recursive,
    ));
  }
  steps
}

/// Runs `steps`. Errors are ignored (a path that was deleted, a watch the
/// backend already dropped) but the one that leaves a watcher without
/// events it expects: the watch limit, which is returned for
/// [`report_backend_errors`].
fn apply_backend_steps(
  watcher: &mut RecommendedWatcher,
  steps: Vec<BackendStep>,
) -> Vec<NotifyError> {
  let mut failed = Vec::new();
  for step in steps {
    match step {
      BackendStep::Unwatch(path) => {
        let _ = watcher.unwatch(&path);
      }
      BackendStep::Watch(path, mode) => {
        if let Err(err) = watcher.watch(&path, mode)
          && matches!(err.kind, notify::ErrorKind::MaxFilesWatch)
        {
          failed.push(err);
        }
      }
    }
  }
  failed
}

/// Hands every watcher the errors of [`apply_backend_steps`], as the
/// backend does when it reaches the watch limit while adding a new
/// directory: part of some watcher's tree is no longer watched.
fn report_backend_errors(
  senders: &Mutex<Vec<WatchSender>>,
  failed: Vec<NotifyError>,
) {
  if failed.is_empty() {
    return;
  }
  let senders = senders.lock();
  for err in failed {
    let res = Err(err);
    for ws in senders.iter() {
      dispatch(ws, &res);
    }
  }
}

/// denext: removes the watch of `path` with `mode` from the shared backend
/// once its last registration is gone, keeping what the watches still
/// registered need ([`backend_steps_to_unwatch`]).
fn unwatch_shared(
  watcher: &mut RecommendedWatcher,
  watched_paths: &HashMap<(PathBuf, RecursiveMode), usize>,
  path: &Path,
  mode: RecursiveMode,
) -> Vec<NotifyError> {
  if !SHARES_BACKEND_WATCHES {
    // Best-effort: ignore errors (e.g. the watcher already lost track of
    // the path because the path was deleted).
    let _ = watcher.unwatch(path);
    return Vec::new();
  }
  #[allow(
    clippy::disallowed_methods,
    reason = "always using real fs with watcher"
  )]
  let is_dir = path.is_dir();
  apply_backend_steps(
    watcher,
    backend_steps_to_unwatch(watched_paths, path, mode, is_dir),
  )
}

/// Undo the `senders` push and any `watch` calls we performed before
/// hitting an error in `op_fs_events_open`. Mirrors the cleanup that
/// would run via [`FsEventsResource`]'s `Drop`, but is needed because
/// the resource itself was never constructed.
fn rollback_partial_open(
  inner: &Arc<WatcherInner>,
  id: u64,
  watched: &[(PathBuf, RecursiveMode)],
) {
  inner.senders.lock().retain(|ws| ws.id != id);

  let mut watched_paths = inner.watched_paths.lock();
  let mut watcher = inner.watcher.lock();
  for (path, mode) in watched {
    let key = (path.clone(), *mode);
    if let Some(count) = watched_paths.get_mut(&key) {
      *count = count.saturating_sub(1);
      if *count == 0 {
        watched_paths.remove(&key);
        // (the open failed: its error is the one reported)
        unwatch_shared(&mut watcher, &watched_paths, path, *mode);
      }
    }
  }
}

#[op2]
async fn op_fs_events_poll(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<Option<FsEvent>, FsEventsError> {
  let resource = state.borrow().resource_table.get::<FsEventsResource>(rid)?;
  let mut receiver = RcRef::map(&resource, |r| &r.receiver).borrow_mut().await;

  // If the event queue overflowed since the last poll, events were dropped:
  // tell the consumer via a `"rescan"`-flagged event rather than losing them
  // silently (denoland/deno#11373). Everything still queued predates the
  // rescan and is superseded by it, so drain and discard those stale events
  // rather than delivering them after the rescan. Queued watcher errors must
  // not be lost though: deliver such an error now and re-arm the flag so the
  // next poll still reports the rescan. There is no missed-wakeup hazard
  // here: the flag can only be set while the queue is full, so a consumer
  // blocked in `recv()` below always has queued events to wake it, and the
  // flag is checked again on the next poll.
  if resource.overflowed.swap(false, Ordering::Relaxed) {
    loop {
      match receiver.try_recv() {
        Ok(Ok(_)) => continue,
        Ok(Err(err)) => {
          resource.overflowed.store(true, Ordering::Relaxed);
          return Err(FsEventsError::Notify(JsNotifyError(err)));
        }
        Err(_) => break,
      }
    }
    let paths = resource
      .watched
      .lock()
      .iter()
      .map(|(p, _)| p.clone())
      .collect();
    return Ok(Some(FsEvent {
      kind: "any",
      paths,
      flag: Some("rescan"),
    }));
  }

  let cancel = RcRef::map(resource, |r| &r.cancel);
  let maybe_result = receiver.recv().or_cancel(cancel).await?;
  match maybe_result {
    Some(Ok(value)) => Ok(Some(value)),
    Some(Err(err)) => Err(FsEventsError::Notify(JsNotifyError(err))),
    None => Ok(None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn registered(
    watches: &[(&str, RecursiveMode)],
  ) -> HashMap<(PathBuf, RecursiveMode), usize> {
    watches
      .iter()
      .map(|(path, mode)| ((PathBuf::from(path), *mode), 1))
      .collect()
  }

  fn watch(path: &str, mode: RecursiveMode) -> BackendStep {
    BackendStep::Watch(PathBuf::from(path), mode)
  }

  // denext: a watcher opened inside a recursive watch puts the recursive
  // mark back on its own directory; it never walks the recursive watch's
  // tree again.
  #[test]
  fn a_watch_inside_a_recursive_one_asks_for_its_own_path_only() {
    use RecursiveMode::NonRecursive;
    use RecursiveMode::Recursive;
    let all = registered(&[
      ("/proj", Recursive),
      ("/proj/src", NonRecursive),
      ("/proj/src/a", NonRecursive),
      ("/other", NonRecursive),
    ]);
    let after =
      |path: &str, mode| backend_steps_after_watch(&all, Path::new(path), mode);
    assert_eq!(
      after("/proj/src", NonRecursive),
      vec![watch("/proj/src", Recursive)]
    );
    assert_eq!(
      after("/proj/src/a", NonRecursive),
      vec![watch("/proj/src/a", Recursive)]
    );
    // no recursive watch holds it: nothing to put back
    assert_eq!(after("/other", NonRecursive), vec![]);
    // a recursive watch marks its own tree
    assert_eq!(after("/proj", Recursive), vec![]);
    // the same path watched both ways
    let both = registered(&[("/proj", Recursive), ("/proj", NonRecursive)]);
    assert_eq!(
      backend_steps_after_watch(&both, Path::new("/proj"), NonRecursive),
      vec![watch("/proj", Recursive)]
    );
  }

  // denext: closing a watcher asks the backend only for what lies at its
  // path and beneath it.
  #[test]
  fn an_unwatch_asks_for_nothing_above_its_path() {
    use RecursiveMode::NonRecursive;
    use RecursiveMode::Recursive;
    let unwatch = |path: &str| BackendStep::Unwatch(PathBuf::from(path));
    // (the registrations left after the one that goes)
    let left = registered(&[
      ("/proj", Recursive),
      ("/proj/src/a", NonRecursive),
      ("/proj/src/a/deep", Recursive),
      ("/proj/src/b", NonRecursive),
      ("/proj/docs", NonRecursive),
    ]);
    let steps = |path: &str, mode, is_dir| {
      backend_steps_to_unwatch(&left, Path::new(path), mode, is_dir)
    };
    // a directory the recursive watch still holds keeps its watch
    assert_eq!(steps("/proj/src", NonRecursive, true), vec![]);
    // a file's own watch goes, and nothing with it
    assert_eq!(
      steps("/proj/src/main.ts", NonRecursive, false),
      vec![unwatch("/proj/src/main.ts")]
    );
    // a recursive watch inside the recursive one: what it took with it is
    // applied again (non-recursive first), then its path for the one above
    assert_eq!(
      steps("/proj/src", Recursive, true),
      vec![
        unwatch("/proj/src"),
        watch("/proj/src/a", NonRecursive),
        watch("/proj/src/b", NonRecursive),
        watch("/proj/src/a/deep", Recursive),
        watch("/proj/src", Recursive),
      ]
    );
    // the recursive root is never part of a restore
    for (path, mode, is_dir) in [
      ("/proj/src", NonRecursive, true),
      ("/proj/src", NonRecursive, false),
      ("/proj/src", Recursive, true),
      ("/proj/src/a/deep/x", NonRecursive, true),
      ("/proj/docs/guide", Recursive, true),
    ] {
      for step in steps(path, mode, is_dir) {
        let touched = match &step {
          BackendStep::Watch(p, _) | BackendStep::Unwatch(p) => p,
        };
        assert!(touched.starts_with(path), "{path}: {step:?}");
      }
    }
    // nothing recursive holds it: the watch goes; a recursive one takes
    // those beneath it, which are applied again
    let alone = registered(&[
      ("/a/sub", NonRecursive),
      ("/a/sub/x", Recursive),
      ("/b", Recursive),
    ]);
    assert_eq!(
      backend_steps_to_unwatch(&alone, Path::new("/a"), NonRecursive, true),
      vec![unwatch("/a")]
    );
    assert_eq!(
      backend_steps_to_unwatch(&alone, Path::new("/a"), Recursive, true),
      vec![
        unwatch("/a"),
        watch("/a/sub", NonRecursive),
        watch("/a/sub/x", Recursive),
      ]
    );
  }

  fn notify_event(kind: EventKind) -> NotifyEvent {
    NotifyEvent {
      kind,
      paths: vec![PathBuf::from("file.txt")],
      attrs: Default::default(),
    }
  }

  #[test]
  fn ignores_notify_open_any_events() {
    let event =
      notify_event(EventKind::Access(AccessKind::Open(AccessMode::Any)));

    assert!(is_ignored_notify_event(&event));
  }

  #[test]
  fn preserves_notify_close_write_as_access() {
    let event =
      notify_event(EventKind::Access(AccessKind::Close(AccessMode::Write)));

    assert!(!is_ignored_notify_event(&event));
    assert_eq!(FsEvent::from(event).kind, "access");
  }

  #[test]
  fn maps_windows_missing_path_generic_error_to_not_found() {
    let err = JsNotifyError(NotifyError {
      kind: notify::ErrorKind::Generic(WINDOWS_PATH_NOT_FOUND_MSG.to_string()),
      paths: vec![PathBuf::from("watched/file.txt")],
    });
    assert_eq!(err.get_class(), "NotFound");

    let other = JsNotifyError(NotifyError {
      kind: notify::ErrorKind::Generic("some other failure".to_string()),
      paths: vec![],
    });
    assert_eq!(other.get_class(), GENERIC_ERROR);
  }

  // The test above only proves the match arm fires for our own constant. This
  // drives the real notify backend against a path that does not exist so a
  // notify upgrade that rewords the Windows missing-path message fails loudly
  // here instead of silently regressing fs.watch to a non-ENOENT error (see
  // denoland/deno#35855). notify's `watch_inner` returns this Generic error
  // synchronously from its is_dir/is_file check, so no real watch is opened.
  #[cfg(windows)]
  #[test]
  fn notify_windows_missing_path_message_matches_constant() {
    let mut watcher: RecommendedWatcher = Watcher::new(
      |_res: Result<NotifyEvent, NotifyError>| {},
      Default::default(),
    )
    .unwrap();

    let missing = PathBuf::from(r"C:\deno-nonexistent\watch\target");
    let err = watcher
      .watch(&missing, RecursiveMode::NonRecursive)
      .expect_err("watching a nonexistent path should fail");

    assert!(
      matches!(
        &err.kind,
        notify::ErrorKind::Generic(msg) if msg == WINDOWS_PATH_NOT_FOUND_MSG
      ),
      "notify no longer reports the expected Generic message; got {:?}",
      err.kind,
    );
    assert_eq!(JsNotifyError(err).get_class(), "NotFound");
  }

  #[test]
  fn clone_notify_error_preserves_kind_and_paths() {
    let err = NotifyError {
      kind: notify::ErrorKind::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "denied",
      )),
      paths: vec![PathBuf::from("watched/dir")],
    };

    let cloned = clone_notify_error(&err);

    assert_eq!(cloned.paths, err.paths);
    match cloned.kind {
      notify::ErrorKind::Io(io_err) => {
        assert_eq!(io_err.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(io_err.to_string(), "denied");
      }
      kind => panic!("expected Io error, got {kind:?}"),
    }
  }

  fn permissions_with(
    allow_read: Option<Vec<String>>,
    deny_read: Option<Vec<String>>,
  ) -> PermissionsContainer {
    use deno_permissions::Permissions;
    use deno_permissions::PermissionsOptions;
    use deno_permissions::RuntimePermissionDescriptorParser;

    let parser =
      RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
    let perms = Permissions::from_options(
      &parser,
      &PermissionsOptions {
        allow_read,
        deny_read,
        ..Default::default()
      },
    )
    .unwrap();
    PermissionsContainer::new(Arc::new(parser), perms)
  }

  // a watcher error is delivered with only the paths the runtime may read:
  // one that names only denied paths is dropped, one with no path is
  // delivered as is
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[test]
  fn watcher_errors_leave_out_paths_the_runtime_may_not_read() {
    let dir = test_util::TempDir::new();
    let root = canonicalize_path(dir.path().as_path()).unwrap();
    let secret = root.join("secret");
    let ok = root.join("ok");
    std::fs::create_dir_all(&secret).unwrap();
    std::fs::create_dir_all(&ok).unwrap();
    let (sender, mut receiver) = mpsc::channel(8);
    let ws = make_watch_sender(
      0,
      vec![root.clone()],
      vec![],
      sender,
      Arc::new(AtomicBool::new(false)),
      Some(permissions_with(
        Some(vec![]),
        Some(vec![secret.to_string_lossy().into_owned()]),
      )),
      true,
    );
    let err = |paths: Vec<PathBuf>| {
      Err(NotifyError {
        kind: notify::ErrorKind::Io(std::io::Error::new(
          std::io::ErrorKind::PermissionDenied,
          "denied",
        )),
        paths,
      })
    };

    dispatch(&ws, &err(vec![secret.join("inner")]));
    assert!(receiver.try_recv().is_err(), "an error about a denied path");

    dispatch(&ws, &err(vec![secret.clone(), ok.clone()]));
    match receiver.try_recv() {
      Ok(Err(e)) => assert_eq!(e.paths, vec![ok.clone()]),
      other => panic!("expected an error, got {other:?}"),
    }

    dispatch(&ws, &err(vec![]));
    match receiver.try_recv() {
      Ok(Err(e)) => assert!(e.paths.is_empty()),
      other => panic!("expected an error, got {other:?}"),
    }
  }

  fn watch_sender_for(
    path: &Path,
    recursive: bool,
  ) -> (WatchSender, mpsc::Receiver<Result<FsEvent, NotifyError>>) {
    let (sender, receiver) = mpsc::channel(16);
    let ws = make_watch_sender(
      0,
      vec![path.to_path_buf()],
      vec![],
      sender,
      Arc::new(AtomicBool::new(false)),
      None,
      recursive,
    );
    (ws, receiver)
  }

  fn received_paths(
    receiver: &mut mpsc::Receiver<Result<FsEvent, NotifyError>>,
  ) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    while let Ok(res) = receiver.try_recv() {
      paths.extend(res.unwrap().paths);
    }
    paths
  }

  // denext: the notify backend is shared, so a recursive watch on an
  // ancestor makes it report nested paths to every watcher; a non-recursive
  // watcher passes on only its directory and the directory's direct children
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[test]
  fn non_recursive_watcher_gets_only_direct_children() {
    let dir = test_util::TempDir::new();
    let root = canonicalize_path(dir.path().as_path()).unwrap();
    let sub = root.join("sub");
    let nested = sub.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let direct = sub.join("direct.txt");
    let deep = nested.join("deep.txt");
    let sibling = root.join("sibling.txt");
    std::fs::write(&direct, b"x").unwrap();
    std::fs::write(&deep, b"x").unwrap();
    std::fs::write(&sibling, b"x").unwrap();
    // removed paths take the removed-file path through the matcher
    let gone_direct = sub.join("gone.txt");
    let gone_deep = nested.join("gone.txt");

    let (recursive, mut recursive_rx) = watch_sender_for(&root, true);
    let (flat, mut flat_rx) = watch_sender_for(&sub, false);
    for path in [
      &sub,
      &direct,
      &nested,
      &deep,
      &sibling,
      &gone_direct,
      &gone_deep,
    ] {
      let event = Ok(FsEvent {
        kind: "modify",
        paths: vec![path.clone()],
        flag: None,
      });
      dispatch(&recursive, &event);
      dispatch(&flat, &event);
    }

    assert_eq!(
      received_paths(&mut recursive_rx),
      vec![
        sub.clone(),
        direct.clone(),
        nested.clone(),
        deep.clone(),
        sibling.clone(),
        gone_direct.clone(),
        gone_deep.clone(),
      ]
    );
    assert_eq!(
      received_paths(&mut flat_rx),
      vec![
        sub.clone(),
        direct.clone(),
        nested.clone(),
        gone_direct.clone()
      ]
    );
  }

  // a non-recursive watch on a file gets the file's events, not a sibling's
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[test]
  fn non_recursive_file_watcher_gets_only_the_file() {
    let dir = test_util::TempDir::new();
    let root = canonicalize_path(dir.path().as_path()).unwrap();
    let file = root.join("file.txt");
    let other = root.join("other.txt");
    std::fs::write(&file, b"x").unwrap();
    std::fs::write(&other, b"x").unwrap();
    let (flat, mut flat_rx) = watch_sender_for(&file, false);
    for path in [&file, &other, &root] {
      dispatch(
        &flat,
        &Ok(FsEvent {
          kind: "modify",
          paths: vec![path.clone()],
          flag: None,
        }),
      );
    }
    assert_eq!(received_paths(&mut flat_rx), vec![file]);
  }

  // https://github.com/denoland/deno/issues/36937
  //
  // A watcher that is closed while `op_fs_events_poll` still holds the
  // resource is only dropped once that poll returns. The watch must be
  // released on close, otherwise a watcher opened for the same path right
  // after the close sees a stale count and skips `watch()`, which leaves it
  // without events when the path was recreated in between.
  #[test]
  fn closing_a_resource_releases_its_watch_while_it_is_still_referenced() {
    let dir = test_util::TempDir::new();
    let path = normalize_watch_path(dir.path().to_path_buf());
    let key = (path.clone(), RecursiveMode::NonRecursive);

    let mut state = OpState::new(None);
    let open = |state: &mut OpState| {
      open_fs_events_resource(state, vec![path.clone()], vec![], false).unwrap()
    };

    let rid = open(&mut state);
    let inner = state.borrow::<WatcherState>().inner.clone();
    assert_eq!(inner.watched_paths.lock().get(&key), Some(&1));

    // `op_fs_events_poll` holds the resource across its await.
    let in_flight_poll =
      state.resource_table.get::<FsEventsResource>(rid).unwrap();
    state
      .resource_table
      .take::<FsEventsResource>(rid)
      .unwrap()
      .close();
    assert_eq!(inner.watched_paths.lock().get(&key), None);

    let second_rid = open(&mut state);
    assert_eq!(inner.watched_paths.lock().get(&key), Some(&1));

    // The first resource is dropped late, and must not release the watch of
    // the second one.
    drop(in_flight_poll);
    assert_eq!(inner.watched_paths.lock().get(&key), Some(&1));

    state
      .resource_table
      .take::<FsEventsResource>(second_rid)
      .unwrap()
      .close();
    assert_eq!(inner.watched_paths.lock().get(&key), None);
  }

  // denext: inotify adds a new directory's watch only after reporting the
  // directory, so what is made in it right away was lost or reported only as
  // modified. The scan of the new directory reports it, and as created before
  // anything inotify saw.
  #[cfg(any(target_os = "linux", target_os = "android"))]
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[test]
  fn recursive_watch_reports_what_a_new_directory_got_before_its_watch() {
    use std::time::Duration;
    use std::time::Instant;

    use tokio::sync::mpsc::error::TryRecvError;

    let dir = test_util::TempDir::new();
    let root =
      normalize_watch_path(canonicalize_path(dir.path().as_path()).unwrap());
    let mut state = OpState::new(None);
    let rid =
      open_fs_events_resource(&mut state, vec![root.clone()], vec![], true)
        .unwrap();
    let resource = state.resource_table.get::<FsEventsResource>(rid).unwrap();
    let mut receiver = RcRef::map(&resource, |r| &r.receiver)
      .try_borrow_mut()
      .unwrap();

    for i in 0..50 {
      let new_dir = root.join(format!("dir-{i}"));
      let nested = new_dir.join("nested");
      std::fs::create_dir_all(&nested).unwrap();
      let file = nested.join("file.txt");
      std::fs::write(&file, "x").unwrap();

      let mut first_kinds: HashMap<PathBuf, &'static str> = HashMap::new();
      let deadline = Instant::now() + Duration::from_secs(10);
      while !first_kinds.contains_key(&file)
        || !first_kinds.contains_key(&nested)
      {
        match receiver.try_recv() {
          Ok(Ok(event)) => {
            for path in event.paths {
              first_kinds.entry(path).or_insert(event.kind);
            }
          }
          Ok(Err(err)) => panic!("run {i}: watcher error {err:?}"),
          Err(TryRecvError::Empty) => {
            assert!(
              Instant::now() < deadline,
              "run {i}: no event for {} (got {first_kinds:?})",
              file.display()
            );
            std::thread::sleep(Duration::from_millis(1));
          }
          Err(TryRecvError::Disconnected) => panic!("run {i}: disconnected"),
        }
      }
      assert_eq!(first_kinds[&nested], "create", "run {i}");
      assert_eq!(first_kinds[&file], "create", "run {i}");
    }
  }
}
