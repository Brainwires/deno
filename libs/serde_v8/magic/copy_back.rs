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

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
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
  /// Where a write-back dropped on another thread goes: the queue of the
  /// runtime that made the copy. Dead once that runtime is gone.
  pub(crate) queue: Weak<CopyBackQueue>,
}

struct PendingCopyBack {
  target: v8::SharedRef<v8::BackingStore>,
  byte_offset: usize,
  original: Box<[u8]>,
  changed: Box<[u8]>,
}

// SAFETY: `v8::SharedRef<v8::BackingStore>` is a thread-safe reference count;
// a pending write-back touches the target only on its owner thread.
unsafe impl Send for PendingCopyBack {}

/// Queued write-backs alive in the process (see [`pending_copy_backs`]).
static LIVE_PENDING: AtomicUsize = AtomicUsize::new(0);

impl Drop for PendingCopyBack {
  fn drop(&mut self) {
    LIVE_PENDING.fetch_sub(1, Ordering::Relaxed);
  }
}

/// The number of write-backs queued and not yet applied or discarded, across
/// all runtimes. For tests.
#[doc(hidden)]
pub fn pending_copy_backs() -> usize {
  LIVE_PENDING.load(Ordering::Relaxed)
}

/// The write-backs other threads handed to one runtime for its resizable
/// buffers. The runtime owns it (its op driver holds the only strong
/// reference), so when the runtime is torn down its queued entries are freed
/// with it, and later ones are dropped at once instead of queued.
#[derive(Default)]
pub struct CopyBackQueue {
  entries: Mutex<Vec<PendingCopyBack>>,
  len: AtomicUsize,
}

thread_local! {
  static CURRENT_QUEUE: RefCell<Weak<CopyBackQueue>> =
    const { RefCell::new(Weak::new()) };
}

impl CopyBackQueue {
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// Makes this the queue that copies made on this thread report to. A
  /// runtime calls it when it starts and whenever it polls its ops.
  pub fn make_current(self: &Arc<Self>) {
    CURRENT_QUEUE.with(|current| {
      let mut current = current.borrow_mut();
      if !std::ptr::eq(current.as_ptr(), Arc::as_ptr(self)) {
        *current = Arc::downgrade(self);
      }
    });
  }

  fn current() -> Weak<Self> {
    CURRENT_QUEUE.with(|current| current.borrow().clone())
  }

  fn push(&self, pending: PendingCopyBack) {
    let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
    entries.push(pending);
    self.len.store(entries.len(), Ordering::Release);
  }

  /// Applies the queued write-backs. Called on the owner thread by the op
  /// driver before it resolves an async op.
  pub fn apply(&self) {
    if self.len.load(Ordering::Acquire) == 0 {
      return;
    }
    let pending = {
      let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
      self.len.store(0, Ordering::Release);
      std::mem::take(&mut *entries)
    };
    for p in pending {
      // SAFETY: the op driver calls this on the owner thread, running Rust
      // code, not JavaScript.
      unsafe { write_back(&p.target, p.byte_offset, &p.original, &p.changed) };
    }
  }

  /// Drops the queued write-backs without applying them (runtime shutdown).
  pub fn clear(&self) {
    let pending = {
      let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
      self.len.store(0, Ordering::Release);
      std::mem::take(&mut *entries)
    };
    drop(pending);
  }
}

impl CopyBack {
  /// The queue copies made now on this thread report to.
  pub(crate) fn current_queue() -> Weak<CopyBackQueue> {
    CopyBackQueue::current()
  }
}

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
    // bytes to its runtime, which applies them before it resolves the next
    // op. If that runtime is gone, so is the buffer's isolate: drop them.
    let Some(queue) = self.queue.upgrade() else {
      return;
    };
    LIVE_PENDING.fetch_add(1, Ordering::Relaxed);
    queue.push(PendingCopyBack {
      target: self.target.clone(),
      byte_offset: self.byte_offset,
      original: std::mem::take(&mut self.original),
      changed: changed.into(),
    });
  }
}
