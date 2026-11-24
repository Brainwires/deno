// Copyright 2018-2026 the Deno authors. MIT license.

//! denext: `Deno.openRoot()`, a directory handle whose operations can't
//! leave the directory (Go 1.24's `os.Root`).
//!
//! Every path a [`FsRoot`] is given is relative to the directory it opened,
//! and resolved beneath it: an absolute path is refused, `..` is resolved
//! lexically and refused where it would climb above the root, and the
//! directory itself is never looked up by its name again, so moving it, or
//! swapping its path for a symlink, after it was opened changes nothing.
//!
//! - Unix: through the race-safe opens of
//!   [`deno_permissions::checked_open`], with the root's own descriptor as
//!   the scope's only root. Linux uses `openat2(RESOLVE_BENEATH |
//!   RESOLVE_NO_MAGICLINKS)` where the kernel has it (5.6+), otherwise (and
//!   on macOS and the BSDs) each component is opened from the previous
//!   one's descriptor with `O_NOFOLLOW`. A symlink inside the root is
//!   followed only when its target, resolved as a new path, stays beneath
//!   the root (and outside every deny); the walk then continues from the
//!   root's descriptor.
//! - Windows ([`crate::root_windows`]): each component is opened with
//!   `NtCreateFile` relative to the previous component's handle, with
//!   `FILE_OPEN_REPARSE_POINT`, so no symlink or junction is ever followed:
//!   a path through one is refused.
//!
//! Permissions: opening a root checks read access to its path. An
//! operation that writes checks write access to the directory's current
//! location (the path the kernel reports for the root's descriptor or
//! handle), so a root that was moved writes only where writing is allowed.
//! The deny rules hold inside a root as they hold everywhere: on Unix the
//! object each open reaches is checked against them; on Windows, where no
//! link is followed, each operation's path is checked as `Deno.open()`
//! would check it.

#![allow(
  clippy::disallowed_types,
  reason = "a root is shared with the blocking pool, so it is a real `Arc` \
            (and `checked_open` takes the root's descriptor as one)"
)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::OpState;
use deno_core::Resource;
use deno_core::ResourceId;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_core::unsync::spawn_blocking;
use deno_error::JsErrorBox;
use deno_io::StdFileResourceInner;
use deno_io::fs::FileResource;
use deno_io::fs::FsError;
use deno_permissions::OpenAccessKind;
use deno_permissions::PermissionsContainer;

#[cfg(unix)]
use self::unix as imp;
use crate::OpenOptions;
use crate::interface::FsDirEntry;
use crate::ops::FsOpsError;
use crate::ops::FsOpsErrorKind;
use crate::ops::SerializableStat;
#[cfg(windows)]
use crate::root_windows as imp;

/// A path refused before anything is opened: absolute, or climbing out.
fn escape_error(path: &str, root: &Path) -> FsOpsError {
  FsOpsErrorKind::Other(JsErrorBox::new(
    "NotCapable",
    format!(
      "Path \"{path}\" escapes from the root \"{}\": a path in a root must be relative and stay beneath it",
      root.display()
    ),
  ))
  .into_box()
}

/// An open that reached outside the root (a symlink, a `..` after one) or
/// into a denied path.
pub(crate) fn outside_error(path: &str, root: &Path) -> FsError {
  FsError::Io(io::Error::new(
    io::ErrorKind::PermissionDenied,
    RootEscape(format!(
      "Path \"{path}\" leads outside the root \"{}\" or into a denied path",
      root.display()
    )),
  ))
}

/// A link whose target would lead outside the root, made without the
/// unscoped grants a link to anywhere takes.
#[cfg(unix)]
pub(crate) fn target_error(target: &Path, path: &str, root: &Path) -> FsError {
  FsError::Io(io::Error::new(
    io::ErrorKind::PermissionDenied,
    RootEscape(format!(
      "The target \"{}\" of the link \"{path}\" leads outside the root \"{}\" or into a denied path: a link to anywhere requires unscoped --allow-read and --allow-write permissions, because a link's target is only resolved when the link is traversed",
      target.display(),
      root.display()
    )),
  ))
}

/// The payload of [`outside_error`], so the op reports it as `NotCapable`
/// rather than as an OS error.
#[derive(Debug)]
pub(crate) struct RootEscape(pub(crate) String);

impl std::fmt::Display for RootEscape {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.0)
  }
}

impl std::error::Error for RootEscape {}

/// `err` with the operation and the path, as the path-based ops report it
/// (`NotFound: No such file or directory (os error 2): open 'a.txt'`); an
/// escape found while opening is `NotCapable`.
fn op_error(err: FsError, operation: &'static str, path: &str) -> FsOpsError {
  if let FsError::Io(io_err) = &err
    && let Some(escape) = io_err
      .get_ref()
      .and_then(|e| e.downcast_ref::<RootEscape>())
  {
    return FsOpsErrorKind::Other(JsErrorBox::new(
      "NotCapable",
      escape.0.clone(),
    ))
    .into_box();
  }
  // a refusal from the scope (a deny reached through a link)
  if let FsError::PermissionCheck(_) = &err {
    return FsOpsError::from(err);
  }
  crate::ops::operation_error_with_path(err, operation, path)
}

/// The components of `path`, relative to the root: `.` dropped, `..`
/// resolved lexically. `None` when the path is absolute (on Windows also a
/// drive-relative `C:x` or a rooted `\x`) or a `..` climbs above the root.
/// An empty result is the root itself (`""`, `"."`).
pub(crate) fn relative_components(path: &str) -> Option<Vec<OsString>> {
  let mut out: Vec<OsString> = Vec::new();
  // on Windows `/` separates too; elsewhere a `\` is part of a name
  for component in Path::new(path).components() {
    match component {
      Component::Normal(name) => out.push(name.to_os_string()),
      Component::CurDir => {}
      Component::ParentDir => {
        out.pop()?;
      }
      Component::RootDir | Component::Prefix(_) => return None,
    }
  }
  Some(out)
}

/// The directory a root holds open.
pub struct FsRoot {
  /// The path `Deno.openRoot()` was given (for messages).
  given: PathBuf,
  inner: imp::Root,
}

impl FsRoot {
  /// The path checked before an operation, or the refusal.
  fn components(&self, path: &str) -> Result<Vec<OsString>, FsOpsError> {
    relative_components(path).ok_or_else(|| escape_error(path, &self.given))
  }

  /// Checks write access to the directory where it is now.
  fn check_write(
    &self,
    permissions: &PermissionsContainer,
    api_name: &str,
  ) -> Result<(), FsOpsError> {
    let current = self.inner.current_path().ok_or_else(|| {
      FsOpsErrorKind::Other(JsErrorBox::new(
        "NotCapable",
        format!(
          "The root \"{}\" can't be written to: its directory has no path any more",
          self.given.display()
        ),
      ))
      .into_box()
    })?;
    permissions.check_open(
      Cow::Owned(current),
      OpenAccessKind::WriteNoFollow,
      Some(api_name),
    )?;
    Ok(())
  }

  /// Everything an operation on `path` needs before it runs: the path's
  /// components, the permission checks (on the JS thread, where a prompt
  /// may ask), and the platform's scope.
  fn prepare(
    &self,
    permissions: &PermissionsContainer,
    path: &str,
    access: OpenAccessKind,
    api_name: &str,
  ) -> Result<imp::Prepared, FsOpsError> {
    let components = self.components(path)?;
    if access.is_write() {
      self.check_write(permissions, api_name)?;
    }
    self
      .inner
      .prepare(permissions, components, access, api_name)
      .map_err(|err| op_error(err, "open", path))
  }
}

pub struct FsRootResource(Arc<FsRoot>);

impl Resource for FsRootResource {
  fn name(&self) -> Cow<'_, str> {
    "fsRoot".into()
  }
}

fn root_open(
  permissions: &PermissionsContainer,
  path: &str,
  api_name: &str,
) -> Result<(PathBuf, imp::Root), FsOpsError> {
  let checked = permissions.check_open(
    Cow::Borrowed(Path::new(path)),
    OpenAccessKind::Read,
    Some(api_name),
  )?;
  let checked = checked.as_owned();
  let given = PathBuf::from(path);
  let root = imp::Root::open_dir(&checked, &given)?;
  Ok((given, root))
}

fn add_root(
  state: &mut OpState,
  given: PathBuf,
  inner: imp::Root,
) -> ResourceId {
  state
    .resource_table
    .add(FsRootResource(Arc::new(FsRoot { given, inner })))
}

fn get_root(
  state: &OpState,
  rid: ResourceId,
) -> Result<Arc<FsRoot>, FsOpsError> {
  Ok(state.resource_table.get::<FsRootResource>(rid)?.0.clone())
}

#[op2(fast, stack_trace)]
#[smi]
pub fn op_fs_root_open_sync(
  state: &mut OpState,
  #[string] path: &str,
) -> Result<ResourceId, FsOpsError> {
  let (given, root) = root_open(
    state.borrow::<PermissionsContainer>(),
    path,
    "Deno.openRootSync()",
  )?;
  Ok(add_root(state, given, root))
}

#[op2(async(deferred), fast, stack_trace)]
#[smi]
pub async fn op_fs_root_open_async(
  state: Rc<RefCell<OpState>>,
  #[string] path: String,
) -> Result<ResourceId, FsOpsError> {
  let checked = state
    .borrow()
    .borrow::<PermissionsContainer>()
    .check_open(
      Cow::Owned(PathBuf::from(&path)),
      OpenAccessKind::Read,
      Some("Deno.openRoot()"),
    )?
    .as_owned();
  let given = PathBuf::from(path);
  let requested = given.clone();
  let root = spawn_blocking(move || imp::Root::open_dir(&checked, &requested))
    .await
    .map_err(|e| FsOpsErrorKind::Other(JsErrorBox::from_err(e)).into_box())??;
  Ok(add_root(&mut state.borrow_mut(), given, root))
}

/// Runs `f` on the blocking pool (an open may wait: a FIFO).
async fn blocking<T: Send + 'static>(
  f: impl FnOnce() -> Result<T, FsOpsError> + Send + 'static,
) -> Result<T, FsOpsError> {
  spawn_blocking(f)
    .await
    .map_err(|e| FsOpsErrorKind::Other(JsErrorBox::from_err(e)).into_box())?
}

#[derive(deno_core::FromV8, Default, Debug, Clone, Copy)]
pub struct RootOpenOptions {
  #[from_v8(default)]
  read: bool,
  #[from_v8(default)]
  write: bool,
  #[from_v8(default)]
  create: bool,
  #[from_v8(default)]
  truncate: bool,
  #[from_v8(default)]
  append: bool,
  #[from_v8(default)]
  create_new: bool,
  #[from_v8(default)]
  mode: Option<u32>,
}

impl From<RootOpenOptions> for OpenOptions {
  fn from(o: RootOpenOptions) -> Self {
    OpenOptions {
      read: o.read,
      write: o.write,
      create: o.create,
      truncate: o.truncate,
      append: o.append,
      create_new: o.create_new,
      custom_flags: None,
      mode: o.mode,
    }
  }
}

fn open_file(
  root: &FsRoot,
  permissions: &PermissionsContainer,
  path: &str,
  options: Option<RootOpenOptions>,
  api_name: &str,
) -> Result<(imp::Prepared, OpenOptions), FsOpsError> {
  let options = options
    .map(OpenOptions::from)
    .unwrap_or_else(OpenOptions::read);
  let prepared =
    root.prepare(permissions, path, options.access_kind(), api_name)?;
  Ok((prepared, options))
}

fn file_rid(state: &mut OpState, file: std::fs::File, path: PathBuf) -> u32 {
  state.resource_table.add(FileResource::new(
    Rc::new(StdFileResourceInner::file(file, Some(path))),
    "fsFile".to_string(),
  ))
}

#[op2(stack_trace)]
#[smi]
pub fn op_fs_root_file_open_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
  #[scoped] options: Option<RootOpenOptions>,
) -> Result<ResourceId, FsOpsError> {
  let root = get_root(state, rid)?;
  let (prepared, options) = open_file(
    &root,
    state.borrow::<PermissionsContainer>(),
    path,
    options,
    "Deno.Root.openSync()",
  )?;
  let display = prepared.display_path();
  let file = root
    .inner
    .open(&prepared, &options)
    .map_err(|e| op_error(e, "open", path))?;
  Ok(file_rid(state, file, display))
}

#[op2(async(deferred), stack_trace)]
#[smi]
pub async fn op_fs_root_file_open_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
  #[scoped] options: Option<RootOpenOptions>,
) -> Result<ResourceId, FsOpsError> {
  let (root, prepared, options) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let (prepared, options) = open_file(
      &root,
      state.borrow::<PermissionsContainer>(),
      &path,
      options,
      "Deno.Root.open()",
    )?;
    (root, prepared, options)
  };
  let display = prepared.display_path();
  let file = blocking(move || {
    root
      .inner
      .open(&prepared, &options)
      .map_err(|e| op_error(e, "open", &path))
  })
  .await?;
  Ok(file_rid(&mut state.borrow_mut(), file, display))
}

fn read_file(
  root: &FsRoot,
  prepared: &imp::Prepared,
  path: &str,
) -> Result<Vec<u8>, FsOpsError> {
  let mut file = root
    .inner
    .open(prepared, &OpenOptions::read())
    .map_err(|e| op_error(e, "readfile", path))?;
  let mut buf = Vec::new();
  file
    .read_to_end(&mut buf)
    .map_err(|e| op_error(FsError::Io(e), "readfile", path))?;
  Ok(buf)
}

#[op2(stack_trace)]
pub fn op_fs_root_read_file_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
) -> Result<Uint8Array, FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::Read,
    "Deno.Root.readFileSync()",
  )?;
  Ok(read_file(&root, &prepared, path)?.into())
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_read_file_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
) -> Result<Uint8Array, FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::Read,
      "Deno.Root.readFile()",
    )?;
    (root, prepared)
  };
  let buf = blocking(move || read_file(&root, &prepared, &path)).await?;
  Ok(buf.into())
}

fn write_options(
  mode: Option<u32>,
  append: bool,
  create: bool,
  create_new: bool,
) -> OpenOptions {
  OpenOptions {
    read: false,
    write: true,
    create,
    truncate: !append,
    append,
    create_new,
    custom_flags: None,
    mode,
  }
}

fn write_file(
  root: &FsRoot,
  prepared: &imp::Prepared,
  options: &OpenOptions,
  data: &[u8],
  path: &str,
) -> Result<(), FsOpsError> {
  let mut file = root
    .inner
    .open(prepared, options)
    .map_err(|e| op_error(e, "writefile", path))?;
  file
    .write_all(data)
    .map_err(|e| op_error(FsError::Io(e), "writefile", path))?;
  Ok(())
}

#[op2(stack_trace)]
#[allow(clippy::too_many_arguments, reason = "an op's arguments")]
pub fn op_fs_root_write_file_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
  mode: Option<u32>,
  append: bool,
  create: bool,
  create_new: bool,
  #[buffer] data: &[u8],
) -> Result<(), FsOpsError> {
  let root = get_root(state, rid)?;
  let options = write_options(mode, append, create, create_new);
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    options.access_kind(),
    "Deno.Root.writeFileSync()",
  )?;
  write_file(&root, &prepared, &options, data, path)
}

#[op2(async(deferred), stack_trace)]
#[allow(clippy::too_many_arguments, reason = "an op's arguments")]
pub async fn op_fs_root_write_file_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
  mode: Option<u32>,
  append: bool,
  create: bool,
  create_new: bool,
  data: Uint8Array,
) -> Result<(), FsOpsError> {
  let options = write_options(mode, append, create, create_new);
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      options.access_kind(),
      "Deno.Root.writeFile()",
    )?;
    (root, prepared)
  };
  blocking(move || write_file(&root, &prepared, &options, &data.0, &path)).await
}

#[op2(stack_trace)]
pub fn op_fs_root_stat_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
  follow: bool,
) -> Result<SerializableStat, FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::ReadNoFollow,
    if follow {
      "Deno.Root.statSync()"
    } else {
      "Deno.Root.lstatSync()"
    },
  )?;
  let stat = root
    .inner
    .stat(&prepared, follow)
    .map_err(|e| op_error(e, if follow { "stat" } else { "lstat" }, path))?;
  Ok(SerializableStat::from(stat))
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_stat_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
  follow: bool,
) -> Result<SerializableStat, FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::ReadNoFollow,
      if follow {
        "Deno.Root.stat()"
      } else {
        "Deno.Root.lstat()"
      },
    )?;
    (root, prepared)
  };
  let stat = blocking(move || {
    root
      .inner
      .stat(&prepared, follow)
      .map_err(|e| op_error(e, if follow { "stat" } else { "lstat" }, &path))
  })
  .await?;
  Ok(SerializableStat::from(stat))
}

#[op2(stack_trace)]
pub fn op_fs_root_mkdir_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
  recursive: bool,
  mode: Option<u32>,
) -> Result<(), FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::WriteNoFollow,
    "Deno.Root.mkdirSync()",
  )?;
  root
    .inner
    .mkdir(&prepared, recursive, mode.unwrap_or(0o777) & 0o777)
    .map_err(|e| op_error(e, "mkdir", path))
}

#[op2(async(deferred), stack_trace)]
pub async fn op_fs_root_mkdir_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
  recursive: bool,
  mode: Option<u32>,
) -> Result<(), FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::WriteNoFollow,
      "Deno.Root.mkdir()",
    )?;
    (root, prepared)
  };
  blocking(move || {
    root
      .inner
      .mkdir(&prepared, recursive, mode.unwrap_or(0o777) & 0o777)
      .map_err(|e| op_error(e, "mkdir", &path))
  })
  .await
}

#[op2(fast, stack_trace)]
pub fn op_fs_root_remove_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
  recursive: bool,
) -> Result<(), FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::WriteNoFollow,
    "Deno.Root.removeSync()",
  )?;
  root
    .inner
    .remove(&prepared, recursive)
    .map_err(|e| op_error(e, "remove", path))
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_remove_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
  recursive: bool,
) -> Result<(), FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::WriteNoFollow,
      "Deno.Root.remove()",
    )?;
    (root, prepared)
  };
  blocking(move || {
    root
      .inner
      .remove(&prepared, recursive)
      .map_err(|e| op_error(e, "remove", &path))
  })
  .await
}

fn prepare_rename(
  root: &FsRoot,
  permissions: &PermissionsContainer,
  from: &str,
  to: &str,
  api_name: &str,
) -> Result<(imp::Prepared, imp::Prepared), FsOpsError> {
  Ok((
    root.prepare(permissions, from, OpenAccessKind::WriteNoFollow, api_name)?,
    root.prepare(permissions, to, OpenAccessKind::WriteNoFollow, api_name)?,
  ))
}

#[op2(fast, stack_trace)]
pub fn op_fs_root_rename_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] from: &str,
  #[string] to: &str,
) -> Result<(), FsOpsError> {
  let root = get_root(state, rid)?;
  let (a, b) = prepare_rename(
    &root,
    state.borrow::<PermissionsContainer>(),
    from,
    to,
    "Deno.Root.renameSync()",
  )?;
  root
    .inner
    .rename(&a, &b)
    .map_err(|e| op_error(e, "rename", from))
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_rename_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] from: String,
  #[string] to: String,
) -> Result<(), FsOpsError> {
  let (root, a, b) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let (a, b) = prepare_rename(
      &root,
      state.borrow::<PermissionsContainer>(),
      &from,
      &to,
      "Deno.Root.rename()",
    )?;
    (root, a, b)
  };
  blocking(move || {
    root
      .inner
      .rename(&a, &b)
      .map_err(|e| op_error(e, "rename", &from))
  })
  .await
}

#[op2(stack_trace)]
pub fn op_fs_root_read_dir_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
) -> Result<Vec<FsDirEntry>, FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::Read,
    "Deno.Root.readDirSync()",
  )?;
  root
    .inner
    .read_dir(&prepared)
    .map_err(|e| op_error(e, "readdir", path))
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_read_dir_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
) -> Result<Vec<FsDirEntry>, FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::Read,
      "Deno.Root.readDir()",
    )?;
    (root, prepared)
  };
  blocking(move || {
    root
      .inner
      .read_dir(&prepared)
      .map_err(|e| op_error(e, "readdir", &path))
  })
  .await
}

/// A link's path, and what its target is held to (decided on the JS
/// thread, with the permissions as they are now).
fn prepare_symlink(
  root: &FsRoot,
  permissions: &PermissionsContainer,
  path: &str,
  api_name: &str,
) -> Result<(imp::Prepared, imp::LinkTargets), FsOpsError> {
  let prepared =
    root.prepare(permissions, path, OpenAccessKind::WriteNoFollow, api_name)?;
  let targets = root
    .inner
    .link_targets(permissions, api_name)
    .map_err(|err| op_error(err, "symlink", path))?;
  Ok((prepared, targets))
}

#[op2(fast, stack_trace)]
pub fn op_fs_root_symlink_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] target: &str,
  #[string] path: &str,
) -> Result<(), FsOpsError> {
  let root = get_root(state, rid)?;
  let (prepared, targets) = prepare_symlink(
    &root,
    state.borrow::<PermissionsContainer>(),
    path,
    "Deno.Root.symlinkSync()",
  )?;
  root
    .inner
    .symlink(Path::new(target), &prepared, &targets)
    .map_err(|e| op_error(e, "symlink", path))
}

#[op2(async(deferred), fast, stack_trace)]
pub async fn op_fs_root_symlink_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] target: String,
  #[string] path: String,
) -> Result<(), FsOpsError> {
  let (root, prepared, targets) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let (prepared, targets) = prepare_symlink(
      &root,
      state.borrow::<PermissionsContainer>(),
      &path,
      "Deno.Root.symlink()",
    )?;
    (root, prepared, targets)
  };
  blocking(move || {
    root
      .inner
      .symlink(Path::new(&target), &prepared, &targets)
      .map_err(|e| op_error(e, "symlink", &path))
  })
  .await
}

#[op2(stack_trace)]
#[string]
pub fn op_fs_root_read_link_sync(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] path: &str,
) -> Result<String, FsOpsError> {
  let root = get_root(state, rid)?;
  let prepared = root.prepare(
    state.borrow::<PermissionsContainer>(),
    path,
    OpenAccessKind::ReadNoFollow,
    "Deno.Root.readLinkSync()",
  )?;
  let target = root
    .inner
    .read_link(&prepared)
    .map_err(|e| op_error(e, "readlink", path))?;
  crate::ops::path_into_string(target.into_os_string())
}

#[op2(async(deferred), fast, stack_trace)]
#[string]
pub async fn op_fs_root_read_link_async(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] path: String,
) -> Result<String, FsOpsError> {
  let (root, prepared) = {
    let state = state.borrow();
    let root = get_root(&state, rid)?;
    let prepared = root.prepare(
      state.borrow::<PermissionsContainer>(),
      &path,
      OpenAccessKind::ReadNoFollow,
      "Deno.Root.readLink()",
    )?;
    (root, prepared)
  };
  let target = blocking(move || {
    root
      .inner
      .read_link(&prepared)
      .map_err(|e| op_error(e, "readlink", &path))
  })
  .await?;
  crate::ops::path_into_string(target.into_os_string())
}

#[cfg(unix)]
mod unix {
  //! The Unix root: the scoped opens of `crate::scoped` with the root's
  //! descriptor as the only allow root.

  use std::ffi::OsString;
  use std::io;
  use std::os::fd::AsFd;
  use std::os::fd::OwnedFd;
  use std::path::Path;
  use std::path::PathBuf;
  use std::sync::Arc;

  use deno_io::fs::FsError;
  use deno_io::fs::FsResult;
  use deno_io::fs::FsStat;
  use deno_permissions::CheckedPathBuf;
  use deno_permissions::OpenAccessKind;
  use deno_permissions::PermissionsContainer;
  use deno_permissions::checked_open::CheckedOpenOptions;
  use deno_permissions::checked_open::KERNEL_TREES;
  use deno_permissions::checked_open::OpenScope;
  use deno_permissions::checked_open::OpenedPath;
  use deno_permissions::checked_open::open_checked;
  use deno_permissions::checked_open::opened_path;
  use deno_permissions::checked_open::opens_by_name;

  use crate::OpenOptions;
  use crate::interface::FsDirEntry;

  pub struct Root {
    /// The directory's real path when it was opened: the spelling every
    /// path in the root's scope is given in.
    path: PathBuf,
    /// The other spellings of it, where they differ: the path the caller
    /// opened it by, made absolute, and the path the permission check
    /// returned (canonicalized when a scope is checked). A symlink on the way
    /// (macOS's `/var` for `/private/var`) makes them differ, and an
    /// absolute symlink inside the root that names the root the way the
    /// caller did still resolves beneath it. Each opens through `fd` only.
    aliases: Vec<PathBuf>,
    fd: Arc<OwnedFd>,
  }

  /// What a link's target is held to ([`Root::link_targets`]).
  pub enum LinkTargets {
    /// Unscoped read and write: any target, as `Deno.symlink()` stores.
    Any,
    /// The target must stay beneath the root; the scope it is followed in.
    Beneath(OpenScope),
  }

  /// One operation's path, with the scope it is opened in.
  pub struct Prepared {
    abs: PathBuf,
    rel: String,
    scope: OpenScope,
  }

  impl Prepared {
    pub fn display_path(&self) -> PathBuf {
      self.abs.clone()
    }
  }

  /// A refusal by the scope is the root's escape error.
  fn map_scope_err(err: FsError, p: &Prepared, root: &Path) -> FsError {
    match err {
      FsError::PermissionCheck(_) => super::outside_error(&p.rel, root),
      err => err,
    }
  }

  impl Root {
    #[allow(
      clippy::disallowed_methods,
      reason = "the root's own directory, after its permission check"
    )]
    pub fn open_dir(path: &CheckedPathBuf, requested: &Path) -> FsResult<Self> {
      let checked = path.as_checked_path();
      // the root itself: as the check granted it (race-safely when the
      // grant is scoped), links on the way followed, as Go's `OpenRoot`
      let opts = CheckedOpenOptions::new().read(true).directory(true);
      let file = match checked.open_scope() {
        Some(scope) => open_checked(scope, &checked, &opts)
          .map_err(crate::scoped::fs_error)?
          .into_std_file(),
        None => {
          use std::os::unix::fs::OpenOptionsExt;
          std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&*checked)?
        }
      };
      let fd = OwnedFd::from(file);
      // the permission check hands back the path canonicalized when it
      // checks a scope (`/private/var/…`), so the caller's own spelling
      // (`/var/…`, from `Deno.makeTempDir()`) is kept beside it
      let checked = std::path::absolute(&*checked)?;
      let requested = std::path::absolute(requested)?;
      let path = match opened_path(fd.as_fd()) {
        OpenedPath::At(path) => path,
        // no `/proc`: the path as checked, made absolute
        _ => checked.clone(),
      };
      let mut aliases = Vec::new();
      for alias in [requested, checked] {
        if alias != path && !aliases.contains(&alias) {
          aliases.push(alias);
        }
      }
      Ok(Self {
        path,
        aliases,
        fd: Arc::new(fd),
      })
    }

    pub fn current_path(&self) -> Option<PathBuf> {
      match opened_path(self.fd.as_fd()) {
        OpenedPath::At(path) => Some(path),
        OpenedPath::Unlinked => None,
        OpenedPath::Unknown => Some(self.path.clone()),
      }
    }

    /// The scope an operation of `access` is opened in: the root's
    /// descriptor, outside the denies. `named` is the path the operation
    /// names, when it names one.
    ///
    /// The kernel's own trees (`/dev`, Linux's `/proc` and `/sys`) are not
    /// places in the file system, and a by-name open is held to rules of
    /// their own there (`check_special_file`: most of `/proc` takes every
    /// permission). A root is held to the same ones: a path that names one
    /// of the trees is checked as `Deno.open()` checks it, and a root the
    /// trees lie beneath (one opened on `/`) takes them as denies for every
    /// other path, so a link in the root that leads into one is refused as
    /// a link out of the root is. Nothing changes with every permission
    /// granted, which is what the special-file check asks for at most.
    fn scope(
      &self,
      permissions: &PermissionsContainer,
      access: OpenAccessKind,
      named: Option<&Path>,
      api_name: &str,
    ) -> FsResult<OpenScope> {
      let mut paths = vec![self.path.clone()];
      paths.extend(self.aliases.iter().cloned());
      let mut denies = permissions.open_denies(access);
      if !permissions.has_all_permissions() {
        if let Some(named) = named.filter(|named| opens_by_name(named)) {
          let _ = permissions.check_open(
            std::borrow::Cow::Borrowed(named),
            access,
            Some(api_name),
          )?;
        } else {
          denies.extend(
            KERNEL_TREES
              .iter()
              .map(Path::new)
              .filter(|tree| paths.iter().any(|root| tree.starts_with(root)))
              .map(Path::to_path_buf),
          );
        }
      }
      Ok(OpenScope::with_open_root(paths, self.fd.clone(), denies)?)
    }

    pub fn prepare(
      &self,
      permissions: &PermissionsContainer,
      components: Vec<OsString>,
      access: OpenAccessKind,
      api_name: &str,
    ) -> FsResult<Prepared> {
      let mut abs = self.path.clone();
      let mut rel = PathBuf::new();
      for c in components {
        abs.push(&c);
        rel.push(&c);
      }
      let scope = self.scope(permissions, access, Some(&abs), api_name)?;
      Ok(Prepared {
        abs,
        rel: rel.to_string_lossy().into_owned(),
        scope,
      })
    }

    pub fn open(
      &self,
      p: &Prepared,
      options: &OpenOptions,
    ) -> FsResult<std::fs::File> {
      // `O_RDONLY | O_CREAT`, as `Deno.open` allows it: create first
      if options.create && !options.write && !options.append {
        let create = OpenOptions {
          read: false,
          write: true,
          create: true,
          truncate: false,
          append: false,
          create_new: options.create_new,
          custom_flags: None,
          mode: options.mode,
        };
        crate::scoped::open(&p.scope, &p.abs, create)
          .map_err(|e| map_scope_err(e, p, &self.path))?;
        let read = OpenOptions {
          create: false,
          create_new: false,
          ..*options
        };
        return crate::scoped::open(&p.scope, &p.abs, read)
          .map_err(|e| map_scope_err(e, p, &self.path));
      }
      crate::scoped::open(&p.scope, &p.abs, *options)
        .map_err(|e| map_scope_err(e, p, &self.path))
    }

    pub fn stat(&self, p: &Prepared, follow: bool) -> FsResult<FsStat> {
      let result = if follow {
        crate::scoped::stat(&p.scope, &p.abs)
      } else {
        crate::scoped::lstat(&p.scope, &p.abs)
      };
      result.map_err(|e| map_scope_err(e, p, &self.path))
    }

    pub fn mkdir(
      &self,
      p: &Prepared,
      recursive: bool,
      mode: u32,
    ) -> FsResult<()> {
      if !recursive {
        return crate::scoped::mkdir(&p.scope, &p.abs, false, Some(mode))
          .map_err(|e| map_scope_err(e, p, &self.path));
      }
      // each level beneath the root, every step in the scope (never by
      // name: a level that exists must be a directory there)
      let mut level = self.path.clone();
      let rel = PathBuf::from(&p.rel);
      for c in rel.components() {
        level.push(c);
        match crate::scoped::mkdir(&p.scope, &level, false, Some(mode)) {
          Ok(()) => {}
          Err(FsError::Io(err))
            if err.kind() == io::ErrorKind::AlreadyExists =>
          {
            let st = crate::scoped::stat(&p.scope, &level)
              .map_err(|e| map_scope_err(e, p, &self.path))?;
            if !st.is_directory {
              return Err(FsError::Io(err));
            }
          }
          Err(err) => return Err(map_scope_err(err, p, &self.path)),
        }
      }
      Ok(())
    }

    pub fn remove(&self, p: &Prepared, recursive: bool) -> FsResult<()> {
      // the root itself can't be removed through it
      if p.rel.is_empty() {
        return Err(super::outside_error(&p.rel, &self.path));
      }
      crate::scoped::remove(&p.scope, &p.abs, recursive)
        .map_err(|e| map_scope_err(e, p, &self.path))
    }

    pub fn rename(&self, from: &Prepared, to: &Prepared) -> FsResult<()> {
      // nor renamed, nor replaced
      for p in [from, to] {
        if p.rel.is_empty() {
          return Err(super::outside_error(&p.rel, &self.path));
        }
      }
      crate::scoped::rename(&from.scope, &from.abs, &to.scope, &to.abs)
        .map_err(|e| map_scope_err(e, from, &self.path))
    }

    pub fn read_dir(&self, p: &Prepared) -> FsResult<Vec<FsDirEntry>> {
      crate::scoped::read_dir(&p.scope, &p.abs)
        .map_err(|e| map_scope_err(e, p, &self.path))
    }

    /// What the target of a link made now is held to. A link's target is a
    /// string the kernel resolves when the link is followed, by anything
    /// that opens the link's name, not only by this root: `Deno.symlink()`
    /// takes unscoped read and write for that reason, and with both a root
    /// stores any target too. With a scoped (or partly denied) grant the
    /// target must stay beneath the root, the rule `node:wasi`'s
    /// `path_symlink` has for a preopen (DENEXT.md item 224).
    pub fn link_targets(
      &self,
      permissions: &PermissionsContainer,
      api_name: &str,
    ) -> FsResult<LinkTargets> {
      if permissions.query_read_all() && permissions.query_write_all() {
        return Ok(LinkTargets::Any);
      }
      Ok(LinkTargets::Beneath(self.scope(
        permissions,
        OpenAccessKind::ReadWrite,
        None,
        api_name,
      )?))
    }

    pub fn symlink(
      &self,
      target: &Path,
      p: &Prepared,
      targets: &LinkTargets,
    ) -> FsResult<()> {
      let result = match targets {
        LinkTargets::Any => crate::scoped::symlink(&p.scope, target, &p.abs),
        LinkTargets::Beneath(scope) => {
          crate::scoped::symlink_checked(&p.scope, target, &p.abs, |folder| {
            if self.target_stays_beneath(scope, folder, target) {
              Ok(())
            } else {
              Err(super::target_error(target, &p.rel, &self.path))
            }
          })
        }
      };
      result.map_err(|e| map_scope_err(e, p, &self.path))
    }

    /// Whether a link made in `folder` (where the kernel says the link's
    /// folder is now) with `target` leads, when it is followed, to a place
    /// beneath the root:
    ///
    /// - the target is relative, and its `..` all lead it (after a name a
    ///   `..` is resolved from wherever that name leads, which can't be
    ///   told from the text);
    /// - counted from the link's real folder, the leading `..` don't climb
    ///   above the root;
    /// - where the target exists, following it in `scope` (the root, outside
    ///   the read and write denies) is not refused: no link on its way
    ///   leads out or into a denied path. A target that doesn't resolve (a
    ///   missing tail) is beneath the root by the two rules above.
    ///
    /// `false` where the folder's or the root's place can't be told.
    fn target_stays_beneath(
      &self,
      scope: &OpenScope,
      folder: Option<&Path>,
      target: &Path,
    ) -> bool {
      use std::path::Component;
      let (Some(folder), OpenedPath::At(root)) =
        (folder, opened_path(self.fd.as_fd()))
      else {
        return false;
      };
      let Ok(folder) = folder.strip_prefix(&root) else {
        return false;
      };
      // the place the target names, relative to the root
      let mut place: Vec<&std::ffi::OsStr> = folder.iter().collect();
      // after a name, a `..` is not the text's to resolve
      let mut named = false;
      for component in target.components() {
        match component {
          Component::CurDir => {}
          Component::ParentDir if !named => {
            if place.pop().is_none() {
              return false;
            }
          }
          Component::Normal(name) => {
            named = true;
            place.push(name);
          }
          Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
            return false;
          }
        }
      }
      let mut abs = self.path.clone();
      abs.extend(place);
      !matches!(
        crate::scoped::stat(scope, &abs),
        Err(FsError::PermissionCheck(_))
      )
    }

    pub fn read_link(&self, p: &Prepared) -> FsResult<PathBuf> {
      crate::scoped::read_link(&p.scope, &p.abs)
        .map_err(|e| map_scope_err(e, p, &self.path))
    }
  }

  #[cfg(test)]
  #[allow(
    clippy::disallowed_methods,
    reason = "a real folder with real links for the root to open"
  )]
  mod tests {
    use std::ffi::OsString;
    use std::io::Read;
    use std::path::Path;
    use std::sync::Arc;

    use deno_permissions::CheckedPathBuf;
    use deno_permissions::OpenAccessKind;
    use deno_permissions::Permissions;
    use deno_permissions::PermissionsContainer;
    use deno_permissions::PermissionsOptions;
    use deno_permissions::RuntimePermissionDescriptorParser;

    use super::Root;
    use crate::OpenOptions;

    fn read(root: &Root, name: &str) -> Result<String, String> {
      let perms = PermissionsContainer::allow_all(Arc::new(
        RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
      ));
      let p = root
        .prepare(&perms, vec![name.into()], OpenAccessKind::Read, "test")
        .map_err(|e| e.to_string())?;
      let mut file = root
        .open(&p, &OpenOptions::read())
        .map_err(|e| e.to_string())?;
      let mut text = String::new();
      file.read_to_string(&mut text).unwrap();
      Ok(text)
    }

    /// The permission check hands back a canonicalized path when it checks
    /// a scope; an absolute link inside the root that names the root the
    /// way the caller opened it (through a link: macOS's `/var`) still
    /// resolves beneath it, and one that leaves it is still refused.
    #[test]
    fn an_absolute_link_by_the_callers_spelling_stays_inside() {
      let tmp = tempfile::tempdir().unwrap();
      let real = tmp.path().join("real");
      std::fs::create_dir(&real).unwrap();
      std::fs::write(real.join("f.txt"), "inside").unwrap();
      std::fs::write(tmp.path().join("secret.txt"), "outside").unwrap();
      let link = tmp.path().join("link");
      std::os::unix::fs::symlink(&real, &link).unwrap();
      std::os::unix::fs::symlink(link.join("f.txt"), real.join("abs")).unwrap();
      std::os::unix::fs::symlink(
        link.join("..").join("secret.txt"),
        real.join("out"),
      )
      .unwrap();
      let canonical = std::fs::canonicalize(&real).unwrap();
      let root =
        Root::open_dir(&CheckedPathBuf::unsafe_new(canonical), &link).unwrap();
      assert_eq!(read(&root, "f.txt").unwrap(), "inside");
      assert_eq!(read(&root, "abs").unwrap(), "inside");
      assert!(read(&root, "out").is_err());
    }

    /// Unscoped read and write, and nothing else.
    fn read_write_only() -> PermissionsContainer {
      let parser =
        RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
      let perms = Permissions::from_options(
        &parser,
        &PermissionsOptions {
          allow_read: Some(vec![]),
          allow_write: Some(vec![]),
          prompt: false,
          ..Default::default()
        },
      )
      .unwrap();
      PermissionsContainer::new(Arc::new(parser), perms)
    }

    /// With grants scoped to the root's folder a link's target stays
    /// beneath the root; with unscoped read and write it is any string.
    #[test]
    fn a_link_made_with_scoped_grants_stays_beneath_the_root() {
      let tmp = tempfile::tempdir().unwrap();
      let base = std::fs::canonicalize(tmp.path()).unwrap();
      let dir = base.join("root");
      std::fs::create_dir_all(dir.join("sub")).unwrap();
      std::fs::write(dir.join("f.txt"), "inside").unwrap();
      std::fs::write(base.join("secret.txt"), "outside").unwrap();
      // links that were there before: to the root itself, and out
      std::os::unix::fs::symlink("..", dir.join("sub/up")).unwrap();
      std::os::unix::fs::symlink("..", dir.join("out")).unwrap();

      let parser =
        RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
      let scoped = Permissions::from_options(
        &parser,
        &PermissionsOptions {
          allow_read: Some(vec![dir.display().to_string()]),
          allow_write: Some(vec![dir.display().to_string()]),
          prompt: false,
          ..Default::default()
        },
      )
      .unwrap();
      let scoped = PermissionsContainer::new(Arc::new(parser), scoped);
      let root =
        Root::open_dir(&CheckedPathBuf::unsafe_new(dir.clone()), &dir).unwrap();
      let link = |perms: &PermissionsContainer, target: &str, path: &str| {
        let p = root.prepare(
          perms,
          components(Path::new(path)),
          OpenAccessKind::WriteNoFollow,
          "test",
        )?;
        let targets = root.link_targets(perms, "test")?;
        root.symlink(Path::new(target), &p, &targets)
      };

      let secret = base.join("secret.txt");
      for (target, path) in [
        ("../secret.txt", "a"),
        (secret.to_str().unwrap(), "b"),
        ("sub/../../secret.txt", "c"),
        ("../../secret.txt", "sub/d"),
        // `sub/up` is the root: from there `..` is outside
        ("../secret.txt", "sub/up/e"),
        // through a link that leads out
        ("out/secret.txt", "f"),
      ] {
        let err = link(&scoped, target, path).unwrap_err();
        assert_eq!(
          err.kind(),
          std::io::ErrorKind::PermissionDenied,
          "{target}: {err}"
        );
        assert!(
          std::fs::symlink_metadata(dir.join(path)).is_err(),
          "{target}"
        );
      }
      for (target, path) in [
        ("f.txt", "h"),
        ("./f.txt", "i"),
        ("../f.txt", "sub/j"),
        ("missing/later.txt", "k"),
        (".", "l"),
        ("../f.txt", "sub/up/sub/m"),
      ] {
        link(&scoped, target, path).unwrap();
        assert_eq!(
          std::fs::read_link(dir.join(path)).unwrap(),
          Path::new(target)
        );
      }

      // unscoped read and write: any target, as `Deno.symlink()` stores
      let all = PermissionsContainer::allow_all(Arc::new(
        RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
      ));
      link(&all, "../secret.txt", "n").unwrap();
      assert!(read(&root, "n").is_err());
    }

    fn components(path: &Path) -> Vec<OsString> {
      path
        .components()
        .filter_map(|c| match c {
          std::path::Component::Normal(name) => Some(name.to_os_string()),
          _ => None,
        })
        .collect()
    }

    /// A root on `/` holds the kernel's trees to the special-file rule a
    /// by-name open has: a path that names one is checked as `Deno.open()`
    /// checks it, and a link that leads into one is refused.
    #[test]
    fn the_kernel_trees_keep_the_special_file_rule() {
      let perms = read_write_only();
      let slash = Path::new("/");
      let root =
        Root::open_dir(&CheckedPathBuf::unsafe_new(slash.to_path_buf()), slash)
          .unwrap();
      let prepare = |path: &Path, access| {
        root.prepare(&perms, components(path), access, "test")
      };

      // no grant by name, none here
      let null = prepare(Path::new("/dev/null"), OpenAccessKind::Read).unwrap();
      root.open(&null, &OpenOptions::read()).unwrap();

      // every permission by name, every permission here
      let mut refused = vec!["/dev/stdin"];
      if cfg!(target_os = "linux") {
        refused.extend(["/proc/self/environ", "/proc/self/mem"]);
      }
      let tmp = tempfile::tempdir().unwrap();
      let dir = std::fs::canonicalize(tmp.path()).unwrap();
      for (i, path) in refused.into_iter().enumerate() {
        for access in [
          OpenAccessKind::Read,
          OpenAccessKind::ReadNoFollow,
          OpenAccessKind::ReadWrite,
        ] {
          assert!(prepare(Path::new(path), access).is_err(), "{path}");
        }
        // ... and through a link in the root: the name is no tree's, so it
        // prepares, and the open is refused where the link leads
        let link = dir.join(format!("link{i}"));
        std::os::unix::fs::symlink(path, &link).unwrap();
        let p = prepare(&link, OpenAccessKind::Read).unwrap();
        let err = root.open(&p, &OpenOptions::read()).unwrap_err();
        assert_eq!(
          err.kind(),
          std::io::ErrorKind::PermissionDenied,
          "{path}: {err}"
        );
      }
      // a link to a folder of one, the file named after it
      let devices = dir.join("devices");
      std::os::unix::fs::symlink("/dev", &devices).unwrap();
      let p =
        prepare(&devices.join("stdin"), OpenAccessKind::ReadNoFollow).unwrap();
      assert!(root.stat(&p, false).is_err());

      // with every permission nothing is held back
      let all = PermissionsContainer::allow_all(Arc::new(
        RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
      ));
      root
        .prepare(
          &all,
          components(Path::new("/dev/stdin")),
          OpenAccessKind::ReadNoFollow,
          "test",
        )
        .unwrap();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::relative_components;

  fn rel(p: &str) -> Option<Vec<String>> {
    relative_components(p).map(|v| {
      v.into_iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect()
    })
  }

  #[test]
  fn relative_paths_stay_beneath_the_root() {
    assert_eq!(rel(""), Some(vec![]));
    assert_eq!(rel("."), Some(vec![]));
    assert_eq!(rel("a/b"), Some(vec!["a".into(), "b".into()]));
    assert_eq!(rel("a/./b/"), Some(vec!["a".into(), "b".into()]));
    assert_eq!(rel("a/../b"), Some(vec!["b".into()]));
    assert_eq!(rel("a/.."), Some(vec![]));
    assert_eq!(rel(".."), None);
    assert_eq!(rel("../x"), None);
    assert_eq!(rel("a/../../x"), None);
    assert_eq!(rel("/etc/passwd"), None);
  }

  #[cfg(windows)]
  #[test]
  fn windows_prefixes_are_absolute() {
    assert_eq!(rel("C:\\x"), None);
    assert_eq!(rel("C:x"), None);
    assert_eq!(rel("\\x"), None);
    assert_eq!(rel("\\\\server\\share\\x"), None);
    assert_eq!(rel("a\\..\\..\\x"), None);
    assert_eq!(rel("a\\b"), Some(vec!["a".into(), "b".into()]));
  }
}
