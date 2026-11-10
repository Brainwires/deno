// Copyright 2018-2026 the Deno authors. MIT license.

//! Which raw descriptors `net.Socket({ fd })`, `server.listen({ fd })` and
//! `tty.ReadStream(fd)` may wrap, shared by `PipeWrap::open`,
//! `TCPWrap::open` and the `TTY` constructor.
//!
//! A descriptor that already belongs to the process or to this isolate is
//! gated like stdio, with no permission check:
//! - stdio (0-2), and fds the runtime registered for adoption (an inherited
//!   extra stdio slot, a `createPipe()` end, an IPC channel);
//! - an fd this isolate opened through node:fs (`fs.openSync` on a FIFO, a
//!   unix socket or a terminal), which it already had permission to open;
//! - an fd the process inherited at startup (socket activation's fd 3, a
//!   parent's pipe), claimed once process-wide;
//! - a FIFO, socket or terminal another isolate of the process (a worker,
//!   or the main thread from a worker) opened through node:fs, when this
//!   isolate holds unscoped read and write access (so it could have opened
//!   it itself): the wrap gets a dup and the other isolate keeps its fd.
//!
//! Any other descriptor (one a native addon or another isolate created) is
//! process-wide state, so it needs either every permission or an unscoped
//! `--allow-ffi` (which already gives the isolate the whole process).

use std::path::Path;

use deno_core::OpState;
use deno_core::uv_compat;
use deno_permissions::PermissionState;
use deno_permissions::PermissionsContainer;

enum Kind {
  /// Stdio or an fd the runtime registered (`FdTable::begin_uv_adopt`).
  Registered { replace_registration: bool },
  /// An fd the process inherited at startup, claimed process-wide.
  Inherited,
  /// A node:fs fd of this isolate: the wrap gets a dup and the node:fs
  /// entry is released once the open succeeds.
  #[cfg(unix)]
  TableOwned { original: i32 },
  /// A node:fs fd of another isolate: the wrap gets a dup, which it owns;
  /// the other isolate's entry stays as it is.
  #[cfg(unix)]
  OtherIsolate,
}

/// An adoption in progress: open `fd` with libuv, then call [`finish`].
pub(crate) struct FdAdoption {
  pub fd: i32,
  kind: Kind,
}

/// Whether `fd` is a FIFO, a socket or a terminal.
#[cfg(unix)]
fn is_stream_fd(fd: i32) -> bool {
  // SAFETY: zeroed `stat` is a valid out-parameter for fstat.
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: fstat only writes to `st`; an invalid fd returns -1.
  if unsafe { libc::fstat(fd, &mut st) } != 0 {
    return false;
  }
  let kind = st.st_mode & libc::S_IFMT;
  // SAFETY: isatty only inspects the descriptor.
  kind == libc::S_IFIFO
    || kind == libc::S_IFSOCK
    || (kind == libc::S_IFCHR && unsafe { libc::isatty(fd) } == 1)
}

/// Whether the isolate holds a grant that covers the whole process.
fn has_process_wide_grant(state: &OpState, api_name: &str) -> bool {
  let permissions = state.borrow::<PermissionsContainer>();
  permissions
    .check_has_all_permissions(Path::new(api_name))
    .is_ok()
    || matches!(permissions.query_ffi(None), Ok(PermissionState::Granted))
}

/// Whether the isolate holds unscoped read and write access, checked as
/// creating a symlink is (`check_read_all` + `check_write_all`).
#[cfg(unix)]
fn has_unscoped_read_write(state: &OpState, api_name: &str) -> bool {
  let permissions = state.borrow::<PermissionsContainer>();
  permissions.check_read_all(api_name).is_ok()
    && permissions.check_write_all(api_name).is_ok()
}

/// Decide whether a libuv stream wrap may adopt `fd`. On refusal returns the
/// uv error code to hand back to JS (`EEXIST` for a descriptor another wrap
/// or the runtime owns, `EPERM` for an unknown one without a process-wide
/// grant).
pub(crate) fn begin(
  state: &mut OpState,
  fd: i32,
  api_name: &str,
) -> Result<FdAdoption, i32> {
  if fd < 0 {
    return Err(uv_compat::UV_EBADF);
  }
  let table = state.borrow::<deno_io::FdTable>();
  if let Some(replace_registration) = table.begin_uv_adopt(fd, false) {
    return Ok(FdAdoption {
      fd,
      kind: Kind::Registered {
        replace_registration,
      },
    });
  }
  if table.is_table_owned(fd) {
    #[cfg(unix)]
    {
      // Only a stream a wrap can drive: a FIFO, a socket or a terminal. A
      // regular file stays with node:fs (its fd number is not reusable).
      if !is_stream_fd(fd) {
        return Err(uv_compat::UV_EEXIST);
      }
      // SAFETY: fcntl validates the descriptor; the dup is owned by the wrap
      // once the open succeeds and closed by `finish` otherwise.
      let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
      if dup < 0 {
        return Err(uv_compat::UV_EBADF);
      }
      return Ok(FdAdoption {
        fd: dup,
        kind: Kind::TableOwned { original: fd },
      });
    }
    // Windows: a node:fs descriptor is a CRT fd over a file handle that the
    // wraps cannot take over. Refuse it as unsupported, as libuv does for a
    // handle it cannot open.
    #[cfg(not(unix))]
    return Err(uv_compat::UV_ENOTSUP);
  }
  if table.contains(fd) {
    // Owned by another wrap (UvOwned) or by the runtime itself.
    return Err(uv_compat::UV_EEXIST);
  }
  #[cfg(unix)]
  if table.is_table_owned_elsewhere(fd) {
    if !is_stream_fd(fd) {
      return Err(uv_compat::UV_EEXIST);
    }
    if !has_process_wide_grant(state, api_name)
      && !has_unscoped_read_write(state, api_name)
    {
      return Err(uv_compat::UV_EPERM);
    }
    // SAFETY: fcntl validates the descriptor; the dup is owned by the wrap
    // once the open succeeds and closed by `finish` otherwise.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if dup < 0 {
      return Err(uv_compat::UV_EBADF);
    }
    return Ok(FdAdoption {
      fd: dup,
      kind: Kind::OtherIsolate,
    });
  }
  if deno_io::claim_inherited_fd(fd) {
    return Ok(FdAdoption {
      fd,
      kind: Kind::Inherited,
    });
  }
  if has_process_wide_grant(state, api_name)
    && let Some(replace_registration) =
      state.borrow::<deno_io::FdTable>().begin_uv_adopt(fd, true)
  {
    return Ok(FdAdoption {
      fd,
      kind: Kind::Registered {
        replace_registration,
      },
    });
  }
  Err(uv_compat::UV_EPERM)
}

/// Record the outcome of the libuv open started with [`begin`]. `ret` is the
/// open's return code; on success the adopted fd is tracked as UvOwned.
pub(crate) fn finish(state: &mut OpState, adoption: FdAdoption, ret: i32) {
  let FdAdoption { fd, kind } = adoption;
  let table = state.borrow_mut::<deno_io::FdTable>();
  match kind {
    Kind::Registered {
      replace_registration,
    } => {
      if ret == 0 {
        table.finish_uv_adopt(fd, replace_registration);
      }
    }
    Kind::Inherited => {
      if ret == 0 {
        table.register_uv_owned(fd);
      } else {
        deno_io::unclaim_inherited_fd(fd);
      }
    }
    #[cfg(unix)]
    Kind::TableOwned { original } => {
      if ret == 0 {
        // The wrap owns the dup now. Dropping the node:fs entry closes the
        // original number (or does so once an in-flight node:fs operation
        // lets go of it); the open file description lives on in the dup.
        drop(table.remove(original));
        table.register_uv_owned(fd);
      } else {
        // SAFETY: the dup was created by `begin` and never handed out.
        unsafe { libc::close(fd) };
      }
    }
    #[cfg(unix)]
    Kind::OtherIsolate => {
      if ret == 0 {
        table.register_uv_owned(fd);
      } else {
        // SAFETY: the dup was created by `begin` and never handed out.
        unsafe { libc::close(fd) };
      }
    }
  }
}

/// Whether `TTY(fd)` may wrap `fd` with no permission check (see the module
/// docs). Otherwise the caller falls back to its own permission check.
pub(crate) fn tty_fd_needs_no_permission(state: &OpState, fd: i32) -> bool {
  if (0..=2).contains(&fd) {
    return true;
  }
  let table = state.borrow::<deno_io::FdTable>();
  table.is_table_owned(fd)
    || table.is_inherited_extra_stdio(fd)
    || table.is_uv_adoptable(fd)
    || (!table.contains(fd)
      && (deno_io::is_inherited_fd(fd)
        || has_process_wide_grant(state, "node:tty TTY()")))
}
