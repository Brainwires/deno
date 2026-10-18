// Copyright 2018-2026 the Deno authors. MIT license.

//! Snapshots of shared and resizable buffers for ops.
//!
//! An op must not hold a Rust slice over a `SharedArrayBuffer` (another thread
//! can write it at the same time) or over a resizable `ArrayBuffer` (JavaScript
//! can shrink it while an async op still holds the slice, and V8 decommits the
//! pages). Instead the op gets a private copy, and the bytes the op changed are
//! written back afterwards: with relaxed atomic stores for a shared buffer, and
//! for a resizable one on the thread that owns it, clamped to its current
//! length. Bytes the op did not change are left alone, so JavaScript (or
//! another thread) writing the buffer while the op runs is not overwritten.

use std::sync::Mutex;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread::ThreadId;

/// Whether a backing store cannot be lent to an op as a Rust slice.
pub fn needs_copy(store: &v8::BackingStore) -> bool {
  store.is_shared() || store.is_resizable_by_user_javascript()
}

/// Copies `len` bytes at `byte_offset` out of `store`, clamped to its current
/// length.
///
/// # Safety
///
/// For a store that is not shared, the caller must be on the thread that owns
/// it, with no JavaScript running.
pub unsafe fn snapshot(
  store: &v8::BackingStore,
  byte_offset: usize,
  len: usize,
) -> Vec<u8> {
  let end = std::cmp::min(byte_offset.saturating_add(len), store.byte_length());
  let len = end.saturating_sub(byte_offset);
  let Some(data) = store.data() else {
    return Vec::new();
  };
  let base = data.as_ptr().cast::<u8>();
  if store.is_shared() {
    (0..len)
      .map(|i| {
        // SAFETY: in bounds of the store's current length; shared memory is
        // only ever accessed atomically from Rust.
        unsafe { AtomicU8::from_ptr(base.add(byte_offset + i)) }
          .load(Ordering::Relaxed)
      })
      .collect()
  } else {
    let mut copy = vec![0u8; len];
    // SAFETY: in bounds of the store's current length; the caller guarantees
    // nothing else accesses the buffer.
    unsafe {
      std::ptr::copy_nonoverlapping(
        base.add(byte_offset),
        copy.as_mut_ptr(),
        len,
      );
    }
    copy
  }
}

/// Writes the bytes of `changed` that differ from `original` back into
/// `store` at `byte_offset`, clamped to the store's current length.
///
/// # Safety
///
/// For a store that is not shared, the caller must be on the thread that owns
/// it, with no JavaScript running.
pub unsafe fn write_back(
  store: &v8::BackingStore,
  byte_offset: usize,
  original: &[u8],
  changed: &[u8],
) {
  let Some(data) = store.data() else {
    return;
  };
  let base = data.as_ptr().cast::<u8>();
  let len = std::cmp::min(original.len(), changed.len());
  let end = std::cmp::min(byte_offset.saturating_add(len), store.byte_length());
  let len = end.saturating_sub(byte_offset);
  let shared = store.is_shared();
  for i in 0..len {
    if original[i] == changed[i] {
      continue;
    }
    // SAFETY: in bounds of the store's current length (see above).
    unsafe {
      let ptr = base.add(byte_offset + i);
      if shared {
        AtomicU8::from_ptr(ptr).store(changed[i], Ordering::Relaxed);
      } else {
        ptr.write(changed[i]);
      }
    }
  }
}

/// The write-back of a [`crate::V8Slice`] made from a copy. Runs when the last
/// clone of the slice is dropped.
pub(crate) struct CopyBack {
  /// The private copy the op wrote into.
  pub(crate) copy: v8::SharedRef<v8::BackingStore>,
  /// The buffer the op was given.
  pub(crate) target: v8::SharedRef<v8::BackingStore>,
  pub(crate) byte_offset: usize,
  pub(crate) original: Box<[u8]>,
  pub(crate) owner: ThreadId,
}

struct PendingCopyBack {
  owner: ThreadId,
  target: v8::SharedRef<v8::BackingStore>,
  byte_offset: usize,
  original: Box<[u8]>,
  changed: Box<[u8]>,
}

// SAFETY: `v8::SharedRef<v8::BackingStore>` is a thread-safe reference count;
// a pending write-back touches the target only on its owner thread.
unsafe impl Send for PendingCopyBack {}

static PENDING: Mutex<Vec<PendingCopyBack>> = Mutex::new(Vec::new());
static PENDING_COUNT: AtomicUsize = AtomicUsize::new(0);

impl Drop for CopyBack {
  fn drop(&mut self) {
    let changed = match self.copy.data() {
      Some(data) => {
        // SAFETY: the copy is a fixed, private store; every slice of it has
        // been dropped, so nothing else accesses it.
        unsafe {
          std::slice::from_raw_parts(
            data.as_ptr().cast::<u8>(),
            std::cmp::min(self.copy.byte_length(), self.original.len()),
          )
        }
      }
      None => &[],
    };
    if self.target.is_shared() || std::thread::current().id() == self.owner {
      // SAFETY: a shared target is written atomically; otherwise this is the
      // owner thread, which is running this Rust code, not JavaScript.
      unsafe {
        write_back(&self.target, self.byte_offset, &self.original, changed)
      };
      return;
    }
    // A resizable buffer can only be written on its own thread: hand the
    // bytes to it. It applies them before it resolves the next op.
    let pending = PendingCopyBack {
      owner: self.owner,
      target: self.target.clone(),
      byte_offset: self.byte_offset,
      original: std::mem::take(&mut self.original),
      changed: changed.into(),
    };
    let mut queue = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    queue.push(pending);
    PENDING_COUNT.fetch_add(1, Ordering::SeqCst);
  }
}

/// Applies the write-backs other threads queued for buffers this thread owns.
/// Called by the op driver before it resolves an async op.
pub fn apply_pending_copy_backs() {
  if PENDING_COUNT.load(Ordering::SeqCst) == 0 {
    return;
  }
  let me = std::thread::current().id();
  let mine = {
    let mut queue = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let (mine, others): (Vec<_>, Vec<_>) =
      queue.drain(..).partition(|p| p.owner == me);
    *queue = others;
    PENDING_COUNT.fetch_sub(mine.len(), Ordering::SeqCst);
    mine
  };
  for p in mine {
    // SAFETY: this is the owner thread, running Rust code, not JavaScript.
    unsafe { write_back(&p.target, p.byte_offset, &p.original, &p.changed) };
  }
}
