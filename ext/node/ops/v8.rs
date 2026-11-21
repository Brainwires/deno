// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use deno_core::FastString;
use deno_core::GarbageCollected;
use deno_core::OpState;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_core::v8;
use deno_error::JsErrorBox;
use deno_permissions::PermissionsContainer;
use v8::ValueDeserializerHelper;
use v8::ValueSerializerHelper;

static EXPOSE_GC_FROM_SET_FLAGS: AtomicBool = AtomicBool::new(false);

#[op2(fast)]
pub fn op_v8_cached_data_version_tag() -> u32 {
  v8::script_compiler::cached_data_version_tag()
}

/// `v8.isStringOneByteRepresentation(content)`: whether V8 stores the string
/// as Latin-1 (one byte per character) rather than UTF-16. The caller has
/// checked that `content` is a string.
#[op2(fast)]
pub fn op_v8_is_string_one_byte_representation(
  content: v8::Local<v8::Value>,
) -> bool {
  v8::Local::<v8::String>::try_from(content).is_ok_and(|s| s.is_onebyte())
}

#[op2(fast)]
pub fn op_v8_set_flags_from_string(#[string] flags: &str) {
  for flag in flags.split_ascii_whitespace() {
    match flag {
      "--expose_gc" | "--expose-gc" => {
        EXPOSE_GC_FROM_SET_FLAGS.store(true, Ordering::SeqCst);
      }
      "--noexpose_gc" | "--no-expose-gc" => {
        EXPOSE_GC_FROM_SET_FLAGS.store(false, Ordering::SeqCst);
      }
      _ => {}
    }
  }
}

fn gc_callback(
  scope: &mut v8::PinScope,
  _args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
  scope.low_memory_notification();
}

pub fn install_gc_if_exposed<'s, 'i, T>(
  scope: &mut v8::PinScope<'s, 'i, T>,
  context: v8::Local<'s, v8::Context>,
) {
  if !EXPOSE_GC_FROM_SET_FLAGS.load(Ordering::SeqCst) {
    return;
  }

  let scope = &mut v8::ContextScope::new(scope, context);
  let global = context.global(scope);
  let key = v8::String::new_external_onebyte_static(scope, b"gc").unwrap();
  let template = v8::FunctionTemplate::new(scope, gc_callback);
  let function = template.get_function(scope).unwrap();
  function.set_name(key);
  global.set(scope, key.into(), function.into());
}

#[op2(fast)]
pub fn op_v8_get_heap_statistics(
  scope: &mut v8::PinScope<'_, '_>,
  #[buffer] buffer: &mut [f64],
) {
  let stats = scope.get_heap_statistics();

  buffer[0] = stats.total_heap_size() as f64;
  buffer[1] = stats.total_heap_size_executable() as f64;
  buffer[2] = stats.total_physical_size() as f64;
  buffer[3] = stats.total_available_size() as f64;
  buffer[4] = stats.used_heap_size() as f64;
  buffer[5] = stats.heap_size_limit() as f64;
  buffer[6] = stats.malloced_memory() as f64;
  buffer[7] = stats.peak_malloced_memory() as f64;
  buffer[8] = if stats.does_zap_garbage() { 1.0 } else { 0.0 };
  buffer[9] = stats.number_of_native_contexts() as f64;
  buffer[10] = stats.number_of_detached_contexts() as f64;
  buffer[11] = stats.total_global_handles_size() as f64;
  buffer[12] = stats.used_global_handles_size() as f64;
  buffer[13] = stats.external_memory() as f64;
  buffer[14] = stats.total_allocated_bytes() as f64;
}

/// denext: a full garbage collection, now: `v8.startHeapProfile({ forceGC })`.
/// The inspector's `HeapProfiler.collectGarbage` runs the collection in a
/// later task and answers then, so it can't come before the synchronous
/// start of the sampling.
#[op2(fast)]
pub fn op_v8_force_gc(scope: &mut v8::PinScope<'_, '_>) {
  scope.low_memory_notification();
}

#[op2(fast)]
#[smi]
pub fn op_v8_number_of_heap_spaces(scope: &mut v8::PinScope<'_, '_>) -> u32 {
  scope.number_of_heap_spaces() as u32
}

#[op2]
#[string]
pub fn op_v8_update_heap_space_statistics(
  scope: &mut v8::PinScope<'_, '_>,
  #[buffer] buffer: &mut [f64],
  #[smi] space_index: u32,
) -> Option<String> {
  let stats = scope.get_heap_space_statistics(space_index as usize)?;
  buffer[0] = stats.space_size() as f64;
  buffer[1] = stats.space_used_size() as f64;
  buffer[2] = stats.space_available_size() as f64;
  buffer[3] = stats.physical_space_size() as f64;
  Some(stats.space_name().to_string_lossy().into_owned())
}

#[op2]
#[buffer]
pub fn op_v8_take_heap_snapshot(scope: &mut v8::PinScope<'_, '_>) -> Vec<u8> {
  let mut buf = Vec::new();
  scope.take_heap_snapshot(|chunk| {
    buf.extend_from_slice(chunk);
    true
  });
  buf
}

// --- setHeapSnapshotNearHeapLimit -----------------------------------------
//
// Implements `v8.setHeapSnapshotNearHeapLimit(limit)`. Installs a V8
// near-heap-limit callback that streams a `.heapsnapshot` file to disk (up to
// `limit` times) right before the process would OOM, mirroring Node's
// `Environment::NearHeapLimitCallback` (src/heap_utils.cc).

// State for the near-heap-limit snapshot callback. Leaked (never freed) so it
// outlives the isolate, which keeps the callback installed for the
// isolate/process lifetime.
struct HeapSnapshotNearHeapLimitState {
  // Raw isolate pointer captured at op-call time, used to take the snapshot
  // from within the extern "C" callback (which is not handed an isolate).
  isolate: v8::UnsafeRawIsolatePtr,
  limit: u32,
  taken: u32,
  // Reentrancy guard: taking a snapshot can trigger GC and re-enter the
  // callback; we must not recurse into another snapshot.
  processing: bool,
  // The single heap limit granted to the snapshot currently being written.
  // `Some` only while `processing`. Reentrant invocations return this exact
  // value rather than recomputing one from the (already raised) current limit,
  // which is what previously let the heap grow without bound. See the callback.
  granted_limit: Option<usize>,
  dir: PathBuf,
  pid: u32,
  seq: u32,
}

// Howard Hinnant's civil-from-days algorithm: converts a count of days since
// the Unix epoch into a (year, month, day) tuple (UTC). Avoids pulling in a
// date/time dependency just to format the snapshot filename.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
  let z = z + 719468;
  let era = if z >= 0 { z } else { z - 146096 } / 146097;
  let doe = (z - era * 146097) as u64; // [0, 146096]
  let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
  let y = yoe as i64 + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
  let mp = (5 * doy + 2) / 153; // [0, 11]
  let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
  let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
  let y = if m <= 2 { y + 1 } else { y };
  (y, m, d)
}

// Builds a filename matching Deno/Node's `writeHeapSnapshot` naming scheme:
// `Heap.<YYYYMMDD>.<HHMMSS>.<pid>.<threadId>.<seq(3 digits)>.heapsnapshot`.
//
// The thread id is hardcoded to 0, matching `writeHeapSnapshot` in v8.ts. In
// Node this slot is the worker's `threadId`, which isn't readily available to
// this native callback. As a result, two worker threads that OOM within the
// same second (each with its own `seq` starting at 0) can produce identical
// filenames and overwrite each other's snapshot. Plumbing the real `threadId`
// through would disambiguate them.
fn heap_snapshot_filename(pid: u32, seq: u32) -> String {
  let secs = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or_default();
  let days = (secs / 86400) as i64;
  let secs_of_day = secs % 86400;
  let (year, month, day) = civil_from_days(days);
  let hours = secs_of_day / 3600;
  let minutes = (secs_of_day % 3600) / 60;
  let seconds = secs_of_day % 60;
  format!(
    "Heap.{year:04}{month:02}{day:02}.{hours:02}{minutes:02}{seconds:02}.{pid}.0.{seq:03}.heapsnapshot"
  )
}

// Streams a heap snapshot into `path`, returning the number of bytes written.
//
// Chunks go straight to disk rather than being buffered in memory: buffering a
// whole snapshot would OOM the very process we are trying to snapshot.
fn write_heap_snapshot(
  isolate: &mut v8::Isolate,
  path: &Path,
) -> std::io::Result<u64> {
  let file = std::fs::File::create(path)?;
  let mut writer = std::io::BufWriter::new(file);
  let mut written = 0u64;
  let mut write_err = None;
  isolate.take_heap_snapshot(|chunk| match writer.write_all(chunk) {
    Ok(()) => {
      written += chunk.len() as u64;
      true
    }
    Err(e) => {
      write_err = Some(e);
      false
    }
  });
  if let Some(e) = write_err {
    return Err(e);
  }
  writer.flush()?;
  // Get the bytes onto the filesystem (and close the file, which Windows needs
  // before the rename): the process is usually killed by a V8 fatal OOM
  // immediately after this returns.
  let file = writer.into_inner().map_err(|e| e.into_error())?;
  file.sync_all()?;
  Ok(written)
}

#[allow(
  clippy::print_stderr,
  reason = "Node prints the snapshot path to stderr unconditionally; \
   mirror that so the OOM diagnostic is always visible."
)]
#[allow(
  clippy::disallowed_methods,
  reason = "requires real fs: this runs as an extern \"C\" V8 callback from \
   inside a GC, with no access to the FileSystem trait (same reason the \
   snapshot file itself is created with std::fs)."
)]
extern "C" fn near_heap_limit_snapshot_callback(
  data: *mut c_void,
  current_heap_limit: usize,
  _initial_heap_limit: usize,
) -> usize {
  // SAFETY: `data` is the leaked `HeapSnapshotNearHeapLimitState` pointer
  // installed by `op_v8_set_heap_snapshot_near_heap_limit`. It outlives the
  // isolate, so this mutable reference is valid for the callback's duration
  // (V8 invokes near-heap-limit callbacks synchronously and non-concurrently).
  let state = unsafe { &mut *(data as *mut HeapSnapshotNearHeapLimitState) };

  // SAFETY: `state.isolate` was captured from the live isolate scope in the op
  // and the isolate is valid while this near-heap-limit callback runs. The
  // reconstructed `Isolate` is a non-owning handle (no Drop), used only for the
  // duration of this call.
  let mut isolate =
    unsafe { v8::Isolate::from_raw_isolate_ptr_unchecked(state.isolate) };

  // Give V8 headroom so it doesn't immediately OOM while we write the snapshot.
  // Mirrors Node returning `current_heap_limit + max_young_gen_size`: sum the
  // used size of the young-generation spaces.
  let mut young_headroom = 0usize;
  let nspaces = isolate.number_of_heap_spaces();
  for i in 0..nspaces {
    if let Some(s) = isolate.get_heap_space_statistics(i) {
      let name = s.space_name().to_string_lossy();
      if matches!(name.as_ref(), "new_space" | "new_large_object_space") {
        young_headroom = young_headroom.saturating_add(s.space_used_size());
      }
    }
  }
  // Guarantee a sane minimum floor so V8 has room to finish writing.
  const MIN_HEADROOM: usize = 4 * 1024 * 1024;
  let new_limit =
    current_heap_limit.saturating_add(young_headroom.max(MIN_HEADROOM));

  // Nested/reentrant call: generating a snapshot itself runs a full GC
  // (`HeapSnapshotGenerator::GenerateSnapshot` calls `CollectAllAvailableGarbage`),
  // which re-enters this callback while we are still writing. Hand back the
  // *same* ceiling we granted when this snapshot started, never a freshly
  // computed one: V8 raises the limit whenever the returned value exceeds the
  // current one (`Heap::InvokeNearHeapLimitCallback`), so recomputing
  // `current_heap_limit + headroom` on every reentry ratchets the limit up
  // without bound and the process never OOMs -- it just eats the machine.
  // Returning a fixed ceiling raises the limit exactly once; once the snapshot
  // spends that budget V8 OOMs normally.
  if state.processing {
    return state.granted_limit.unwrap_or(current_heap_limit);
  }
  // No more snapshots allowed: remove the callback and return the *unchanged*
  // heap limit so V8 proceeds to OOM normally. Returning a raised limit here
  // would let the heap keep growing instead.
  if state.taken >= state.limit {
    // SAFETY: removing the near-heap-limit callback from within the callback
    // is supported by V8 (Node does the same). The 0 limit means "don't touch
    // the heap limit" (`Heap::RemoveNearHeapLimitCallback` only calls
    // `RestoreHeapLimit` for a non-zero value) -- we want the current limit
    // kept so the pending OOM actually happens.
    isolate
      .remove_near_heap_limit_callback(near_heap_limit_snapshot_callback, 0);
    return current_heap_limit;
  }

  state.processing = true;
  state.granted_limit = Some(new_limit);
  state.taken += 1;
  state.seq += 1;

  let filename = heap_snapshot_filename(state.pid, state.seq);
  let path = state.dir.join(&filename);
  // Generate into a temporary file and only move it into place once V8 has
  // actually produced bytes. Creating the destination up front is what leaves
  // the 0-byte `.heapsnapshot` files behind when generation produces nothing
  // or the process dies partway through (#36034).
  let tmp_path = state.dir.join(format!("{filename}.tmp"));

  // Printed unconditionally (not via `log`, which `--quiet` suppresses) and
  // before generating, which can take a while and may be the last thing this
  // process does. Mirrors Node.
  eprintln!("Writing heap snapshot to {}", path.display());

  let result = write_heap_snapshot(&mut isolate, &tmp_path);
  match result {
    // Treat "V8 emitted nothing" as a failure: an empty file is not a usable
    // snapshot, and silently leaving one behind is the reported bug.
    Ok(0) => {
      let _ = std::fs::remove_file(&tmp_path);
      eprintln!(
        "Failed to write heap snapshot to {}: V8 produced an empty snapshot",
        path.display()
      );
    }
    // Node prints nothing on success beyond the "Writing ..." line above.
    Ok(_) => match std::fs::rename(&tmp_path, &path) {
      Ok(()) => {}
      Err(e) => {
        let _ = std::fs::remove_file(&tmp_path);
        eprintln!("Failed to write heap snapshot to {}: {e}", path.display());
      }
    },
    Err(e) => {
      let _ = std::fs::remove_file(&tmp_path);
      eprintln!("Failed to write heap snapshot to {}: {e}", path.display());
    }
  }

  state.processing = false;
  state.granted_limit = None;
  new_limit
}

#[op2(nofast)]
pub fn op_v8_set_heap_snapshot_near_heap_limit(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
  #[smi] limit: u32,
) -> Result<(), deno_permissions::PermissionCheckError> {
  let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
  let dir = state
    .borrow_mut::<PermissionsContainer>()
    .check_write(Cow::Owned(dir), "v8.setHeapSnapshotNearHeapLimit")?
    .into_owned_path();
  // SAFETY: capture the raw pointer of the current isolate so the extern "C"
  // callback (which is not passed an isolate) can take a snapshot. The isolate
  // outlives the callback, so the pointer stays valid whenever it runs.
  let isolate = unsafe { scope.as_raw_isolate_ptr() };
  let state = Box::new(HeapSnapshotNearHeapLimitState {
    isolate,
    limit,
    taken: 0,
    processing: false,
    granted_limit: None,
    dir,
    pid: std::process::id(),
    seq: 0,
  });
  // Leak the state so it outlives the isolate (see the struct doc comment).
  let data = Box::into_raw(state) as *mut c_void;
  scope.add_near_heap_limit_callback(near_heap_limit_snapshot_callback, data);
  Ok(())
}

// Counts the live JS objects whose prototype chain includes the object that
// `marker[key]` holds (a constructor's `prototype`), as Node.js's
// `v8.queryObjects(ctor, { format: 'count' })` does with V8's
// `HeapProfiler::QueryObjects` (which the rusty_v8 crate does not bind):
// instances of subclasses count, and so do the subclasses' prototypes.
//
// It takes a heap snapshot (which first collects all garbage, as
// `QueryObjects` does) and reads only what it needs from the serialized
// JSON: the meta, the node and edge arrays as integers, and the indices of
// two strings, `__proto__` (the edge V8 writes from every JS object to its
// prototype) and `key` (the marker's edge to the target prototype). Parsing
// the whole snapshot into a `serde_json::Value` tree took seconds in a debug
// build on every call.
#[op2(nofast)]
#[smi]
pub fn op_v8_query_objects_count(
  scope: &mut v8::PinScope<'_, '_>,
  marker: v8::Local<v8::Object>,
  #[string] key: &str,
) -> u32 {
  let mut buf = Vec::new();
  scope.take_heap_snapshot(|chunk| {
    buf.extend_from_slice(chunk);
    true
  });
  // the marker (and with it the target prototype) is alive until here
  let _ = marker;
  heap_snapshot::count_prototype_chain_matches(&buf, key.as_bytes())
    .unwrap_or(0)
}

mod heap_snapshot {
  use deno_core::serde_json;
  use deno_core::serde_json::Value;

  /// Node types that are JS objects (what `QueryObjects` iterates).
  const JS_OBJECT_TYPES: [&str; 4] = ["object", "closure", "array", "regexp"];
  /// The longest prototype chain followed (V8 has no cycles, but a broken
  /// snapshot must not loop).
  const MAX_CHAIN: usize = 1 << 16;

  struct Meta {
    node_fields: usize,
    node_type: usize,
    node_edge_count: usize,
    js_object_types: Vec<u64>,
    edge_fields: usize,
    edge_type: usize,
    edge_name: usize,
    edge_to: usize,
    property_edge: u64,
  }

  fn field_index(fields: &[Value], name: &str) -> Option<usize> {
    fields.iter().position(|f| f == name)
  }

  fn parse_meta(meta: &Value) -> Option<Meta> {
    let node_fields = meta.get("node_fields")?.as_array()?;
    let edge_fields = meta.get("edge_fields")?.as_array()?;
    let node_type = field_index(node_fields, "type")?;
    let edge_type = field_index(edge_fields, "type")?;
    let node_types = meta.get("node_types")?.get(node_type)?.as_array()?;
    let edge_types = meta.get("edge_types")?.get(edge_type)?.as_array()?;
    Some(Meta {
      node_fields: node_fields.len(),
      node_type,
      node_edge_count: field_index(node_fields, "edge_count")?,
      js_object_types: node_types
        .iter()
        .enumerate()
        .filter(|(_, t)| JS_OBJECT_TYPES.iter().any(|j| *t == j))
        .map(|(i, _)| i as u64)
        .collect(),
      edge_fields: edge_fields.len(),
      edge_type,
      edge_name: field_index(edge_fields, "name_or_index")?,
      edge_to: field_index(edge_fields, "to_node")?,
      property_edge: edge_types.iter().position(|t| t == "property")? as u64,
    })
    .filter(|m| m.node_fields > 0 && m.edge_fields > 0)
  }

  /// The position just past `"<name>":` in `buf`, searching from `from`.
  fn after_key(buf: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut needle = Vec::with_capacity(name.len() + 3);
    needle.push(b'"');
    needle.extend_from_slice(name);
    needle.extend_from_slice(b"\":");
    buf[from..]
      .windows(needle.len())
      .position(|w| w == needle.as_slice())
      .map(|i| from + i + needle.len())
  }

  fn skip_ws(buf: &[u8], mut i: usize) -> usize {
    while i < buf.len() && buf[i].is_ascii_whitespace() {
      i += 1;
    }
    i
  }

  /// A JSON array of non-negative integers at `i`; returns it and the
  /// position after the closing bracket. (The work is left to std's
  /// non-generic, always optimized code: a debug build parses the tens of
  /// millions of digits in a snapshot a few times faster that way.)
  fn parse_uint_array(buf: &[u8], i: usize) -> Option<(Vec<u64>, usize)> {
    let i = skip_ws(buf, i);
    if buf.get(i) != Some(&b'[') {
      return None;
    }
    let body = &buf[i + 1..];
    let len = body.iter().position(|b| *b == b']')?;
    let text = std::str::from_utf8(&body[..len]).ok()?;
    let mut out = Vec::with_capacity(len / 2);
    if !text.trim_ascii().is_empty() {
      for field in text.split(',') {
        out.push(field.trim_ascii().parse::<u64>().ok()?);
      }
    }
    Some((out, i + 1 + len + 1))
  }

  /// The indices in the JSON string array at `i` of the strings whose raw
  /// (still escaped) text equals each of `wanted` (strings that need no
  /// escaping, so raw equality is equality).
  fn find_strings(
    buf: &[u8],
    i: usize,
    wanted: &[&[u8]],
  ) -> Option<Vec<Option<u64>>> {
    let mut found = vec![None; wanted.len()];
    let mut i = skip_ws(buf, i);
    if buf.get(i) != Some(&b'[') {
      return None;
    }
    i += 1;
    let mut index: u64 = 0;
    loop {
      i = skip_ws(buf, i);
      match *buf.get(i)? {
        b']' => return Some(found),
        b',' => i += 1,
        b'"' => {
          let start = i + 1;
          i = start;
          loop {
            match *buf.get(i)? {
              b'\\' => i += 2,
              b'"' => break,
              _ => i += 1,
            }
          }
          let raw = &buf[start..i];
          for (slot, w) in found.iter_mut().zip(wanted) {
            if slot.is_none() && raw == *w {
              *slot = Some(index);
            }
          }
          index += 1;
          i += 1;
        }
        _ => return None,
      }
    }
  }

  pub fn count_prototype_chain_matches(buf: &[u8], key: &[u8]) -> Option<u32> {
    let snapshot_at = after_key(buf, 0, b"snapshot")?;
    let meta: Value = serde_json::Deserializer::from_slice(&buf[snapshot_at..])
      .into_iter::<Value>()
      .next()?
      .ok()?;
    let meta = parse_meta(meta.get("meta")?)?;
    let (nodes, after_nodes) =
      parse_uint_array(buf, after_key(buf, snapshot_at, b"nodes")?)?;
    let (edges, after_edges) =
      parse_uint_array(buf, after_key(buf, after_nodes, b"edges")?)?;
    let strings_at = after_key(buf, after_edges, b"strings")?;
    let found = find_strings(buf, strings_at, &[b"__proto__", key])?;
    let (proto_name, key_name) = (found[0]?, found[1]?);

    let node_count = nodes.len() / meta.node_fields;
    // each node's prototype (a node index), and the target prototype
    let mut proto: Vec<Option<u32>> = vec![None; node_count];
    let mut target = None;
    let mut edge = 0usize;
    for (n, node) in nodes.chunks_exact(meta.node_fields).enumerate() {
      let count = node[meta.node_edge_count] as usize;
      for _ in 0..count {
        let e = edges.get(edge..edge + meta.edge_fields)?;
        edge += meta.edge_fields;
        if e[meta.edge_type] != meta.property_edge {
          continue;
        }
        let to = (e[meta.edge_to] as usize / meta.node_fields) as u32;
        let name = e[meta.edge_name];
        if name == proto_name {
          proto[n] = Some(to);
        } else if name == key_name && target.is_none() {
          target = Some(to);
        }
      }
    }
    let target = target?;

    // 0 unknown, 1 reaches the target, 2 does not
    let mut state = vec![0u8; node_count];
    let mut count: u32 = 0;
    let mut chain = Vec::new();
    for n in 0..node_count {
      let ty = nodes[n * meta.node_fields + meta.node_type];
      if !meta.js_object_types.contains(&ty) {
        continue;
      }
      chain.clear();
      let mut cur = proto[n];
      let verdict = loop {
        let Some(p) = cur else { break 2 };
        if p == target {
          break 1;
        }
        let p = p as usize;
        if state[p] != 0 {
          break state[p];
        }
        if chain.len() >= MAX_CHAIN {
          break 2;
        }
        chain.push(p);
        cur = proto[p];
      };
      for &c in &chain {
        state[c] = verdict;
      }
      if verdict == 1 {
        count = count.saturating_add(1);
      }
    }
    Some(count)
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    // nodes: [type, name, id, self_size, edge_count, trace_node_id,
    // detachedness]; edges: [type, name_or_index, to_node]
    fn snapshot(
      nodes: &[[u64; 7]],
      edges: &[[u64; 3]],
      strings: &[&str],
    ) -> Vec<u8> {
      let ints = |v: Vec<u64>| {
        v.iter()
          .map(|n| n.to_string())
          .collect::<Vec<_>>()
          .join(",")
      };
      let strings = strings
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(",\n");
      format!(
        r#"{{"snapshot":{{"meta":{{"node_fields":["type","name","id","self_size","edge_count","trace_node_id","detachedness"],"node_types":[["hidden","array","string","object","code","closure","regexp","number","native"],"string","number","number","number","number","number"],"edge_fields":["type","name_or_index","to_node"],"edge_types":[["context","element","property","internal","hidden","shortcut","weak"],"string_or_number","node"]}},"node_count":{},"edge_count":{}}},
"nodes":[{}],
"edges":[{}],
"trace_function_infos":[],
"samples":[],
"locations":[],
"strings":["a \"quoted\" \\ one",
{}]}}"#,
        nodes.len(),
        edges.len(),
        ints(nodes.iter().flatten().copied().collect()),
        ints(edges.iter().flatten().copied().collect()),
        strings
      )
      .into_bytes()
    }

    #[test]
    fn counts_the_prototype_chain() {
      // strings: 0 escaped, 1 "__proto__", 2 "marker-key", 3 "x"
      const OBJ: u64 = 3;
      const NATIVE: u64 = 8;
      const PROP: u64 = 2;
      const F: u64 = 7; // node_fields
      // node 0: marker -> 1 (Foo.prototype)
      // node 1: Foo.prototype, proto -> 6 (Object.prototype)
      // node 2: foo, proto -> 1
      // node 3: Bar.prototype, proto -> 1
      // node 4: bar, proto -> 3
      // node 5: native with proto edge -> 1 (not a JS object)
      // node 6: Object.prototype
      // node 7: unrelated object, proto -> 6
      let nodes = [
        [OBJ, 3, 1, 0, 1, 0, 0],
        [OBJ, 3, 2, 0, 1, 0, 0],
        [OBJ, 3, 3, 0, 2, 0, 0],
        [OBJ, 3, 4, 0, 1, 0, 0],
        [OBJ, 3, 5, 0, 1, 0, 0],
        [NATIVE, 3, 6, 0, 1, 0, 0],
        [OBJ, 3, 7, 0, 0, 0, 0],
        [OBJ, 3, 8, 0, 1, 0, 0],
      ];
      let edges = [
        [PROP, 2, F],
        [PROP, 1, 6 * F],
        [PROP, 3, 7 * F],
        [PROP, 1, F],
        [PROP, 1, F],
        [PROP, 1, 3 * F],
        [PROP, 1, F],
        [PROP, 1, 6 * F],
      ];
      let buf = snapshot(&nodes, &edges, &["__proto__", "marker-key", "x"]);
      // foo, Bar.prototype, bar
      assert_eq!(count_prototype_chain_matches(&buf, b"marker-key"), Some(3));
      // no such marker edge
      assert_eq!(count_prototype_chain_matches(&buf, b"other"), None);
      // garbage
      assert_eq!(count_prototype_chain_matches(b"{}", b"marker-key"), None);
      assert_eq!(
        count_prototype_chain_matches(&buf[..buf.len() / 2], b"marker-key"),
        None
      );
    }
  }
}

#[op2(fast)]
pub fn op_v8_get_heap_code_statistics(
  scope: &mut v8::PinScope<'_, '_>,
  #[buffer] buffer: &mut [f64],
) {
  if let Some(stats) = scope.get_heap_code_and_metadata_statistics() {
    buffer[0] = stats.code_and_metadata_size() as f64;
    buffer[1] = stats.bytecode_and_metadata_size() as f64;
    buffer[2] = stats.external_script_source_size() as f64;
    buffer[3] = stats.cpu_profiler_metadata_size() as f64;
  }
}

pub struct Serializer<'a> {
  delegate_state: Rc<SerializerDelegateState>,
  inner: v8::ValueSerializer<'a>,
}

struct SerializerDelegateState {
  obj: v8::TracedReference<v8::Object>,
}

pub struct SerializerDelegate {
  state: Rc<SerializerDelegateState>,
}

// SAFETY: we're sure this can be GCed
unsafe impl v8::cppgc::GarbageCollected for Serializer<'_> {
  fn trace(&self, visitor: &mut deno_core::v8::cppgc::Visitor) {
    visitor.trace(&self.delegate_state.obj);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Serializer"
  }
}

impl SerializerDelegate {
  fn obj<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Object> {
    self.state.obj.get(scope).unwrap()
  }
}

impl v8::ValueSerializerImpl for SerializerDelegate {
  fn get_shared_array_buffer_id<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    shared_array_buffer: v8::Local<'s, v8::SharedArrayBuffer>,
  ) -> Option<u32> {
    let obj = self.obj(scope);
    let key = FastString::from_static("_getSharedArrayBufferId")
      .v8_string(scope)
      .unwrap()
      .into();
    if let Some(v) = obj.get(scope, key)
      && let Ok(fun) = v.try_cast::<v8::Function>()
    {
      return fun
        .call(scope, obj.into(), &[shared_array_buffer.into()])
        .and_then(|ret| ret.uint32_value(scope));
    }
    None
  }
  fn has_custom_host_object(&self, _isolate: &v8::Isolate) -> bool {
    false
  }
  fn throw_data_clone_error<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    message: v8::Local<'s, v8::String>,
  ) {
    let obj = self.obj(scope);
    let key = FastString::from_static("_getDataCloneError")
      .v8_string(scope)
      .unwrap()
      .into();
    if let Some(v) = obj.get(scope, key) {
      let fun = v
        .try_cast::<v8::Function>()
        .expect("_getDataCloneError should be a function");
      if let Some(error) = fun.call(scope, obj.into(), &[message.into()]) {
        scope.throw_exception(error);
        return;
      }
    }
    let error = v8::Exception::type_error(scope, message);
    scope.throw_exception(error);
  }

  fn write_host_object<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    _value_serializer: &dyn ValueSerializerHelper,
  ) -> Option<bool> {
    let obj = self.obj(scope);
    let key = FastString::from_static("_writeHostObject")
      .v8_string(scope)
      .unwrap()
      .into();
    if let Some(v) = obj.get(scope, key)
      && let Ok(v) = v.try_cast::<v8::Function>()
    {
      v.call(scope, obj.into(), &[object.into()])?;
      return Some(true);
    }

    None
  }

  fn is_host_object<'s>(
    &self,
    _scope: &mut v8::PinScope<'s, '_>,
    _object: v8::Local<'s, v8::Object>,
  ) -> Option<bool> {
    // should never be called because has_custom_host_object returns false
    None
  }
}

#[op2]
#[cppgc]
pub fn op_v8_new_serializer(
  scope: &mut v8::PinScope<'_, '_>,
  obj: v8::Local<v8::Object>,
) -> Serializer<'static> {
  let delegate_state = Rc::new(SerializerDelegateState {
    obj: v8::TracedReference::new(scope, obj),
  });
  let inner = v8::ValueSerializer::new(
    scope,
    Box::new(SerializerDelegate {
      state: delegate_state.clone(),
    }),
  );
  Serializer {
    inner,
    delegate_state,
  }
}

#[op2(fast)]
pub fn op_v8_set_treat_array_buffer_views_as_host_objects(
  #[cppgc] ser: &Serializer,
  value: bool,
) {
  ser
    .inner
    .set_treat_array_buffer_views_as_host_objects(value);
}

#[op2]
pub fn op_v8_release_buffer(#[cppgc] ser: &Serializer) -> Uint8Array {
  ser.inner.release().into()
}

#[op2(fast)]
pub fn op_v8_transfer_array_buffer(
  #[cppgc] ser: &Serializer,
  #[smi] id: u32,
  array_buffer: v8::Local<v8::ArrayBuffer>,
) {
  ser.inner.transfer_array_buffer(id, array_buffer);
}

#[op2(fast)]
pub fn op_v8_write_double(#[cppgc] ser: &Serializer, double: f64) {
  ser.inner.write_double(double);
}

#[op2(fast)]
pub fn op_v8_write_header(#[cppgc] ser: &Serializer) {
  ser.inner.write_header();
}

#[op2]
pub fn op_v8_write_raw_bytes(
  #[cppgc] ser: &Serializer,
  #[anybuffer] source: &[u8],
) {
  ser.inner.write_raw_bytes(source);
}

#[op2(fast)]
pub fn op_v8_write_uint32(#[cppgc] ser: &Serializer, num: u32) {
  ser.inner.write_uint32(num);
}

#[op2(fast)]
pub fn op_v8_write_uint64(#[cppgc] ser: &Serializer, hi: u32, lo: u32) {
  let num = ((hi as u64) << 32) | (lo as u64);
  ser.inner.write_uint64(num);
}

#[op2(nofast, reentrant)]
pub fn op_v8_write_value(
  scope: &mut v8::PinScope<'_, '_>,
  #[cppgc] ser: &Serializer,
  value: v8::Local<v8::Value>,
) {
  let context = scope.get_current_context();
  ser.inner.write_value(context, value);
}

struct DeserBuffer {
  ptr: Option<NonNull<u8>>,
  len: usize,
  // Hold onto backing store to keep the underlying buffer
  // alive while we hold a reference to it.
  _backing_store: v8::SharedRef<v8::BackingStore>,
}

pub struct Deserializer<'a> {
  buf: DeserBuffer,
  delegate_state: Rc<DeserializerDelegateState>,
  inner: v8::ValueDeserializer<'a>,
}

// SAFETY: we're sure this can be GCed
unsafe impl deno_core::GarbageCollected for Deserializer<'_> {
  fn trace(&self, visitor: &mut deno_core::v8::cppgc::Visitor) {
    visitor.trace(&self.delegate_state.obj);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Deserializer"
  }
}

struct DeserializerDelegateState {
  obj: v8::TracedReference<v8::Object>,
}

pub struct DeserializerDelegate {
  state: Rc<DeserializerDelegateState>,
}

impl v8::ValueDeserializerImpl for DeserializerDelegate {
  fn read_host_object<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    _value_deserializer: &dyn v8::ValueDeserializerHelper,
  ) -> Option<v8::Local<'s, v8::Object>> {
    let obj = self.state.obj.get(scope).unwrap();
    let key = FastString::from_static("_readHostObject")
      .v8_string(scope)
      .unwrap()
      .into();
    let scope = std::pin::pin!(v8::AllowJavascriptExecutionScope::new(scope));
    let scope = &mut scope.init();
    if let Some(v) = obj.get(scope, key)
      && let Ok(v) = v.try_cast::<v8::Function>()
    {
      let result = v.call(scope, obj.into(), &[])?;
      match result.try_cast() {
        Ok(res) => return Some(res),
        Err(_) => {
          let msg =
            FastString::from_static("readHostObject must return an object")
              .v8_string(scope)
              .unwrap();
          let error = v8::Exception::type_error(scope, msg);
          scope.throw_exception(error);
          return None;
        }
      }
    }
    None
  }
}

#[op2]
#[cppgc]
pub fn op_v8_new_deserializer(
  scope: &mut v8::PinScope<'_, '_>,
  obj: v8::Local<v8::Object>,
  buffer: v8::Local<v8::ArrayBufferView>,
) -> Result<Deserializer<'static>, JsErrorBox> {
  let offset = buffer.byte_offset();
  let len = buffer.byte_length();
  let backing_store = buffer.get_backing_store().ok_or_else(|| {
    JsErrorBox::generic("deserialization buffer has no backing store")
  })?;
  let (buf_slice, buf_ptr) = if let Some(data) = backing_store.data() {
    // SAFETY: the offset is valid for the underlying buffer because we're getting it directly from v8
    let data_ptr = unsafe { data.as_ptr().cast::<u8>().add(offset) };
    (
      // SAFETY: the len is valid, from v8, and the data_ptr is valid (as above)
      unsafe { std::slice::from_raw_parts(data_ptr.cast_const().cast(), len) },
      NonNull::new(data_ptr),
    )
  } else {
    (&[] as &[u8], None::<NonNull<u8>>)
  };
  let delegate_state = Rc::new(DeserializerDelegateState {
    obj: v8::TracedReference::new(scope, obj),
  });
  let inner = v8::ValueDeserializer::new(
    scope,
    Box::new(DeserializerDelegate {
      state: delegate_state.clone(),
    }),
    buf_slice,
  );
  Ok(Deserializer {
    inner,
    delegate_state,
    buf: DeserBuffer {
      _backing_store: backing_store,
      ptr: buf_ptr,
      len,
    },
  })
}

#[op2(fast)]
pub fn op_v8_transfer_array_buffer_de(
  #[cppgc] deser: &Deserializer,
  #[smi] id: u32,
  array_buffer: v8::Local<v8::Value>,
) -> Result<(), deno_core::error::DataError> {
  if let Ok(shared_array_buffer) =
    array_buffer.try_cast::<v8::SharedArrayBuffer>()
  {
    deser
      .inner
      .transfer_shared_array_buffer(id, shared_array_buffer)
  }
  let array_buffer = array_buffer.try_cast::<v8::ArrayBuffer>()?;
  deser.inner.transfer_array_buffer(id, array_buffer);
  Ok(())
}

#[op2(fast)]
pub fn op_v8_read_double(
  #[cppgc] deser: &Deserializer,
) -> Result<f64, JsErrorBox> {
  let mut double = 0f64;
  if !deser.inner.read_double(&mut double) {
    return Err(JsErrorBox::type_error("ReadDouble() failed"));
  }
  Ok(double)
}

#[op2(nofast)]
pub fn op_v8_read_header(
  scope: &mut v8::PinScope<'_, '_>,
  #[cppgc] deser: &Deserializer,
) -> bool {
  let context = scope.get_current_context();
  let res = deser.inner.read_header(context);
  res.unwrap_or_default()
}

#[op2(fast)]
#[number]
pub fn op_v8_read_raw_bytes(
  #[cppgc] deser: &Deserializer,
  #[number] length: usize,
) -> isize {
  let Some(buf) = deser.inner.read_raw_bytes(length) else {
    return -1;
  };
  let Some(buf_ptr) = deser.buf.ptr else {
    return if length == 0 && buf.is_empty() && deser.buf.len == 0 {
      0
    } else {
      -1
    };
  };

  let Some(offset) =
    (buf.as_ptr() as usize).checked_sub(buf_ptr.as_ptr() as usize)
  else {
    return -1;
  };
  let Some(end) = offset.checked_add(buf.len()) else {
    return -1;
  };
  if end > deser.buf.len {
    return -1;
  }

  isize::try_from(offset).unwrap_or(-1)
}

#[op2(fast)]
pub fn op_v8_read_uint32(
  #[cppgc] deser: &Deserializer,
) -> Result<u32, JsErrorBox> {
  let mut value = 0;
  if !deser.inner.read_uint32(&mut value) {
    return Err(JsErrorBox::type_error("ReadUint32() failed"));
  }

  Ok(value)
}

#[op2]
pub fn op_v8_read_uint64(
  #[cppgc] deser: &Deserializer,
) -> Result<(u32, u32), JsErrorBox> {
  let mut val = 0;
  if !deser.inner.read_uint64(&mut val) {
    return Err(JsErrorBox::type_error("ReadUint64() failed"));
  }

  Ok(((val >> 32) as u32, val as u32))
}

#[op2(fast)]
pub fn op_v8_get_wire_format_version(#[cppgc] deser: &Deserializer) -> u32 {
  deser.inner.get_wire_format_version()
}

#[op2(reentrant)]
pub fn op_v8_read_value<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  #[cppgc] deser: &Deserializer,
) -> v8::Local<'s, v8::Value> {
  let context = scope.get_current_context();
  let val = deser.inner.read_value(context);
  val.unwrap_or_else(|| v8::null(scope).into())
}

// --- GCProfiler -----------------------------------------------------------
//
// Implements `v8.GCProfiler`, a thin per-instance recorder that hooks the
// V8 GC prologue/epilogue callbacks. Each active profiler captures heap and
// heap-space statistics on every GC and records the wall-clock cost.

#[derive(Default)]
struct GcProfilerRegistryInner {
  next_id: u64,
  // Profilers that have been started but not yet stopped.
  profilers: HashMap<u64, GcProfilerState>,
  callbacks_registered: bool,
}

struct GcProfilerRegistry {
  inner: Rc<RefCell<GcProfilerRegistryInner>>,
}

struct GcProfilerState {
  pending_before: Option<GcSnapshot>,
  pending_start: Option<Instant>,
  statistics: Vec<GcStat>,
}

#[derive(Clone)]
struct GcSnapshot {
  // total_heap_size, total_heap_size_executable, total_physical_size,
  // total_available_size, used_heap_size, heap_size_limit,
  // malloced_memory, peak_malloced_memory,
  // total_global_handles_size, used_global_handles_size, external_memory.
  heap: [f64; 11],
  spaces: Vec<HeapSpaceSnapshot>,
}

#[derive(Clone)]
struct HeapSpaceSnapshot {
  name: String,
  size: f64,
  used_size: f64,
  available_size: f64,
  physical_size: f64,
}

struct GcStat {
  gc_type: &'static str,
  // Cost in nanoseconds (matches Node.js).
  cost_ns: f64,
  before: GcSnapshot,
  after: GcSnapshot,
}

fn gc_type_name(gc_type: v8::GCType) -> &'static str {
  // V8 callbacks may surface combined flags (e.g. kGCTypeIncrementalMarking |
  // kGCTypeMarkSweepCompact). Pick the lowest-priority single bit so the
  // returned label is stable and informative.
  match gc_type {
    v8::GCType::kGCTypeScavenge => "Scavenge",
    v8::GCType::kGCTypeMinorMarkSweep => "MinorMarkSweep",
    v8::GCType::kGCTypeMarkSweepCompact => "MarkSweepCompact",
    v8::GCType::kGCTypeIncrementalMarking => "IncrementalMarking",
    v8::GCType::kGCTypeProcessWeakCallbacks => "ProcessWeakCallbacks",
    v8::GCType::kGCTypeAll => "All",
    _ => "Unknown",
  }
}

fn capture_snapshot(isolate: &mut v8::Isolate) -> GcSnapshot {
  let h = isolate.get_heap_statistics();
  let heap = [
    h.total_heap_size() as f64,
    h.total_heap_size_executable() as f64,
    h.total_physical_size() as f64,
    h.total_available_size() as f64,
    h.used_heap_size() as f64,
    h.heap_size_limit() as f64,
    h.malloced_memory() as f64,
    h.peak_malloced_memory() as f64,
    h.total_global_handles_size() as f64,
    h.used_global_handles_size() as f64,
    h.external_memory() as f64,
  ];
  let nspaces = isolate.number_of_heap_spaces();
  let mut spaces = Vec::with_capacity(nspaces);
  for i in 0..nspaces {
    if let Some(s) = isolate.get_heap_space_statistics(i) {
      spaces.push(HeapSpaceSnapshot {
        name: s.space_name().to_string_lossy().into_owned(),
        size: s.space_size() as f64,
        used_size: s.space_used_size() as f64,
        available_size: s.space_available_size() as f64,
        physical_size: s.physical_space_size() as f64,
      });
    }
  }
  GcSnapshot { heap, spaces }
}

fn registry_rc(
  isolate: &v8::Isolate,
) -> Option<Rc<RefCell<GcProfilerRegistryInner>>> {
  isolate
    .get_slot::<GcProfilerRegistry>()
    .map(|r| r.inner.clone())
}

extern "C" fn gc_prologue_callback(
  isolate: v8::UnsafeRawIsolatePtr,
  _gc_type: v8::GCType,
  _flags: v8::GCCallbackFlags,
  _data: *mut c_void,
) {
  // SAFETY: V8 guarantees the isolate is valid during this callback.
  let mut isolate =
    unsafe { v8::Isolate::from_raw_isolate_ptr_unchecked(isolate) };
  let Some(rc) = registry_rc(&isolate) else {
    return;
  };
  // Bail out fast if no profilers are active so we don't capture heap
  // statistics on every GC unnecessarily.
  if rc.borrow().profilers.is_empty() {
    return;
  }
  let snapshot = capture_snapshot(&mut isolate);
  let now = Instant::now();
  let mut inner = rc.borrow_mut();
  for state in inner.profilers.values_mut() {
    state.pending_before = Some(snapshot.clone());
    state.pending_start = Some(now);
  }
}

extern "C" fn gc_epilogue_callback(
  isolate: v8::UnsafeRawIsolatePtr,
  gc_type: v8::GCType,
  _flags: v8::GCCallbackFlags,
  _data: *mut c_void,
) {
  // SAFETY: V8 guarantees the isolate is valid during this callback.
  let mut isolate =
    unsafe { v8::Isolate::from_raw_isolate_ptr_unchecked(isolate) };
  let Some(rc) = registry_rc(&isolate) else {
    return;
  };
  if rc.borrow().profilers.is_empty() {
    return;
  }
  let snapshot = capture_snapshot(&mut isolate);
  let now = Instant::now();
  let gc_type_str = gc_type_name(gc_type);
  let mut inner = rc.borrow_mut();
  for state in inner.profilers.values_mut() {
    let (Some(before), Some(start)) =
      (state.pending_before.take(), state.pending_start.take())
    else {
      continue;
    };
    let cost_ns = now.saturating_duration_since(start).as_nanos() as f64;
    state.statistics.push(GcStat {
      gc_type: gc_type_str,
      cost_ns,
      before,
      after: snapshot.clone(),
    });
  }
}

fn ensure_registry(
  scope: &mut v8::PinScope<'_, '_>,
) -> Rc<RefCell<GcProfilerRegistryInner>> {
  if let Some(existing) = scope.get_slot::<GcProfilerRegistry>() {
    return existing.inner.clone();
  }
  let inner = Rc::new(RefCell::new(GcProfilerRegistryInner::default()));
  scope.set_slot(GcProfilerRegistry {
    inner: inner.clone(),
  });
  inner
}

fn ensure_callbacks_registered(
  scope: &mut v8::PinScope<'_, '_>,
  inner: &Rc<RefCell<GcProfilerRegistryInner>>,
) {
  if inner.borrow().callbacks_registered {
    return;
  }
  scope.add_gc_prologue_callback(
    gc_prologue_callback,
    std::ptr::null_mut(),
    v8::GCType::kGCTypeAll,
  );
  scope.add_gc_epilogue_callback(
    gc_epilogue_callback,
    std::ptr::null_mut(),
    v8::GCType::kGCTypeAll,
  );
  inner.borrow_mut().callbacks_registered = true;
}

pub struct GcProfilerHandle {
  id: std::cell::Cell<Option<u64>>,
}

// SAFETY: GcProfilerHandle has no traceable references.
unsafe impl GarbageCollected for GcProfilerHandle {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"GcProfilerHandle"
  }
}

#[op2]
#[cppgc]
pub fn op_v8_gc_profiler_new() -> GcProfilerHandle {
  GcProfilerHandle {
    id: std::cell::Cell::new(None),
  }
}

#[op2(fast)]
pub fn op_v8_gc_profiler_start(
  scope: &mut v8::PinScope<'_, '_>,
  #[cppgc] handle: &GcProfilerHandle,
) {
  if handle.id.get().is_some() {
    return;
  }
  let inner = ensure_registry(scope);
  ensure_callbacks_registered(scope, &inner);
  let id = {
    let mut borrow = inner.borrow_mut();
    let id = borrow.next_id;
    borrow.next_id = borrow.next_id.wrapping_add(1);
    borrow.profilers.insert(
      id,
      GcProfilerState {
        pending_before: None,
        pending_start: None,
        statistics: Vec::new(),
      },
    );
    id
  };
  handle.id.set(Some(id));
}

#[op2]
pub fn op_v8_gc_profiler_stop<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  #[cppgc] handle: &GcProfilerHandle,
) -> v8::Local<'s, v8::Value> {
  let Some(id) = handle.id.take() else {
    return v8::null(scope).into();
  };
  let Some(inner) = scope
    .get_slot::<GcProfilerRegistry>()
    .map(|r| r.inner.clone())
  else {
    return v8::null(scope).into();
  };
  let state = inner.borrow_mut().profilers.remove(&id);
  let Some(state) = state else {
    return v8::null(scope).into();
  };
  build_report(scope, &state.statistics).into()
}

const HEAP_KEYS: &[&str] = &[
  "totalHeapSize",
  "totalHeapSizeExecutable",
  "totalPhysicalSize",
  "totalAvailableSize",
  "usedHeapSize",
  "heapSizeLimit",
  "mallocedMemory",
  "peakMallocedMemory",
  "totalGlobalHandlesSize",
  "usedGlobalHandlesSize",
  "externalMemory",
];

fn build_snapshot<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  snap: &GcSnapshot,
) -> v8::Local<'s, v8::Object> {
  let obj = v8::Object::new(scope);

  let heap_stats = v8::Object::new(scope);
  for (i, key) in HEAP_KEYS.iter().enumerate() {
    let k = v8::String::new(scope, key).unwrap();
    let v = v8::Number::new(scope, snap.heap[i]);
    heap_stats.set(scope, k.into(), v.into());
  }
  let k = v8::String::new(scope, "heapStatistics").unwrap();
  obj.set(scope, k.into(), heap_stats.into());

  let spaces_array = v8::Array::new(scope, snap.spaces.len() as i32);
  for (i, space) in snap.spaces.iter().enumerate() {
    let space_obj = v8::Object::new(scope);
    let k = v8::String::new(scope, "spaceName").unwrap();
    let name = v8::String::new(scope, &space.name).unwrap();
    space_obj.set(scope, k.into(), name.into());
    for (name, value) in [
      ("spaceSize", space.size),
      ("spaceUsedSize", space.used_size),
      ("spaceAvailableSize", space.available_size),
      ("physicalSpaceSize", space.physical_size),
    ] {
      let k = v8::String::new(scope, name).unwrap();
      let v = v8::Number::new(scope, value);
      space_obj.set(scope, k.into(), v.into());
    }
    spaces_array.set_index(scope, i as u32, space_obj.into());
  }
  let k = v8::String::new(scope, "heapSpaceStatistics").unwrap();
  obj.set(scope, k.into(), spaces_array.into());

  obj
}

fn build_report<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  stats: &[GcStat],
) -> v8::Local<'s, v8::Object> {
  let arr = v8::Array::new(scope, stats.len() as i32);
  for (i, stat) in stats.iter().enumerate() {
    let entry = v8::Object::new(scope);

    let k = v8::String::new(scope, "gcType").unwrap();
    let v = v8::String::new(scope, stat.gc_type).unwrap();
    entry.set(scope, k.into(), v.into());

    let k = v8::String::new(scope, "cost").unwrap();
    let v = v8::Number::new(scope, stat.cost_ns);
    entry.set(scope, k.into(), v.into());

    let before = build_snapshot(scope, &stat.before);
    let k = v8::String::new(scope, "beforeGC").unwrap();
    entry.set(scope, k.into(), before.into());

    let after = build_snapshot(scope, &stat.after);
    let k = v8::String::new(scope, "afterGC").unwrap();
    entry.set(scope, k.into(), after.into());

    arr.set_index(scope, i as u32, entry.into());
  }
  let wrapper = v8::Object::new(scope);
  let k = v8::String::new(scope, "statistics").unwrap();
  wrapper.set(scope, k.into(), arr.into());
  wrapper
}
