// Copyright 2018-2026 the Deno authors. MIT license.

//! The ALSA sequencer, through `libasound.so.2` loaded at run time.
//!
//! Linking libasound would make every `deno` binary need it to start (servers
//! and containers rarely have it, and the release's dynamic-dependency check
//! refuses a new library), so the library is opened with `dlopen` on the
//! first `requestMIDIAccess()`. Without it, or without the kernel's sequencer
//! (`/dev/snd/seq`, the `snd-seq` module), the backend fails to start and
//! `requestMIDIAccess()` rejects with `NotSupportedError`.
//!
//! Each open port gets its own sequencer client with one port of ours,
//! subscribed to (input) or from (output) the device's port. Our ports are
//! `NO_EXPORT`, so no other client can subscribe to them, and the port list
//! skips them. Input events are turned back into MIDI bytes with ALSA's
//! `snd_midi_event` decoder (running status off) on a thread that polls the
//! client; output bytes are encoded with the encoder and sent directly
//! (unqueued) to the subscribers. Timing is the JavaScript side's scheduler's.

use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_long;
use std::ffi::c_short;
use std::ffi::c_uint;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

use crate::backend::Backend;
use crate::backend::InputCallback;
use crate::backend::InputConnection;
use crate::backend::OutputConnection;
use crate::backend::PortInfo;

// asoundlib's constants (seq.h, seq_event.h)
const SND_SEQ_OPEN_DUPLEX: c_int = 3;
const SND_SEQ_NONBLOCK: c_int = 1;
const CAP_READ: c_uint = 1 << 0;
const CAP_WRITE: c_uint = 1 << 1;
const CAP_SUBS_READ: c_uint = 1 << 5;
const CAP_SUBS_WRITE: c_uint = 1 << 6;
const CAP_NO_EXPORT: c_uint = 1 << 7;
const TYPE_MIDI_GENERIC: c_uint = 1 << 1;
const TYPE_SYNTH: c_uint = 1 << 10;
const TYPE_SYNTHESIZER: c_uint = 1 << 18;
const TYPE_APPLICATION: c_uint = 1 << 20;
const ADDRESS_SUBSCRIBERS: u8 = 254;
const ADDRESS_UNKNOWN: u8 = 253;
const QUEUE_DIRECT: u8 = 253;
const EVENT_NONE: u8 = 255;
const EVENT_SYSEX: u8 = 130;
const POLLIN: c_short = 0x001;
/// How often a reader thread checks whether its port was closed.
const POLL_TIMEOUT_MS: c_int = 100;

/// `snd_seq_event_t`: 28 bytes, 4-aligned; `data` is a union whose `ext`
/// member (`{ unsigned int len; void *ptr; }`, packed) carries a system
/// exclusive message.
#[repr(C)]
#[derive(Clone, Copy)]
struct SeqEvent {
  kind: u8,
  flags: u8,
  tag: u8,
  queue: u8,
  time: [u32; 2],
  source: [u8; 2],
  dest: [u8; 2],
  data: [u32; 3],
}

const _: () = assert!(std::mem::size_of::<SeqEvent>() == 28);

impl SeqEvent {
  fn zeroed() -> Self {
    Self {
      kind: EVENT_NONE,
      flags: 0,
      tag: 0,
      queue: 0,
      time: [0; 2],
      source: [0; 2],
      dest: [0; 2],
      data: [0; 3],
    }
  }

  /// The length of an `ext` (variable-length) payload.
  fn ext_len(&self) -> usize {
    self.data[0] as usize
  }

  /// Addressed to our port's subscribers, sent at once.
  fn route_to_subscribers(&mut self, port: c_int) {
    self.source[1] = port as u8;
    self.dest = [ADDRESS_SUBSCRIBERS, ADDRESS_UNKNOWN];
    self.queue = QUEUE_DIRECT;
  }
}

type Handle = *mut c_void;

struct Lib {
  _lib: libloading::Library,
  seq_open:
    unsafe extern "C" fn(*mut Handle, *const c_char, c_int, c_int) -> c_int,
  seq_close: unsafe extern "C" fn(Handle) -> c_int,
  seq_set_client_name: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
  seq_client_info_malloc: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
  seq_client_info_free: unsafe extern "C" fn(*mut c_void),
  seq_client_info_set_client: unsafe extern "C" fn(*mut c_void, c_int),
  seq_client_info_get_client: unsafe extern "C" fn(*const c_void) -> c_int,
  seq_query_next_client: unsafe extern "C" fn(Handle, *mut c_void) -> c_int,
  seq_port_info_malloc: unsafe extern "C" fn(*mut *mut c_void) -> c_int,
  seq_port_info_free: unsafe extern "C" fn(*mut c_void),
  seq_port_info_set_client: unsafe extern "C" fn(*mut c_void, c_int),
  seq_port_info_set_port: unsafe extern "C" fn(*mut c_void, c_int),
  seq_port_info_get_port: unsafe extern "C" fn(*const c_void) -> c_int,
  seq_port_info_get_name: unsafe extern "C" fn(*const c_void) -> *const c_char,
  seq_port_info_get_capability: unsafe extern "C" fn(*const c_void) -> c_uint,
  seq_port_info_get_type: unsafe extern "C" fn(*const c_void) -> c_uint,
  seq_query_next_port: unsafe extern "C" fn(Handle, *mut c_void) -> c_int,
  seq_create_simple_port:
    unsafe extern "C" fn(Handle, *const c_char, c_uint, c_uint) -> c_int,
  seq_connect_from: unsafe extern "C" fn(Handle, c_int, c_int, c_int) -> c_int,
  seq_connect_to: unsafe extern "C" fn(Handle, c_int, c_int, c_int) -> c_int,
  seq_poll_descriptors_count: unsafe extern "C" fn(Handle, c_short) -> c_int,
  seq_poll_descriptors:
    unsafe extern "C" fn(Handle, *mut libc::pollfd, c_uint, c_short) -> c_int,
  seq_event_input: unsafe extern "C" fn(Handle, *mut *mut SeqEvent) -> c_int,
  seq_event_output_direct: unsafe extern "C" fn(Handle, *mut SeqEvent) -> c_int,
  midi_event_new: unsafe extern "C" fn(usize, *mut *mut c_void) -> c_int,
  midi_event_free: unsafe extern "C" fn(*mut c_void),
  midi_event_no_status: unsafe extern "C" fn(*mut c_void, c_int),
  midi_event_encode: unsafe extern "C" fn(
    *mut c_void,
    *const u8,
    c_long,
    *mut SeqEvent,
  ) -> c_long,
  midi_event_decode: unsafe extern "C" fn(
    *mut c_void,
    *mut u8,
    c_long,
    *const SeqEvent,
  ) -> c_long,
}

fn lib() -> Result<&'static Lib, String> {
  static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
  LIB.get_or_init(load).as_ref().map_err(|e| e.clone())
}

fn load() -> Result<Lib, String> {
  // SAFETY: libasound's initializers have no preconditions.
  let lib =
    unsafe { libloading::Library::new("libasound.so.2") }.map_err(|e| {
      format!("the ALSA library (libasound.so.2) is not available: {e}")
    })?;
  /// A function from the library. The caller's type is the one
  /// asoundlib.h declares; the library stays loaded for the process (it is
  /// kept in `Lib`, which lives in a static), so the pointer stays valid.
  fn get<T: Copy>(lib: &libloading::Library, name: &str) -> Result<T, String> {
    let symbol = format!("{name}\0");
    // SAFETY: see above.
    let f: libloading::Symbol<T> = unsafe { lib.get(symbol.as_bytes()) }
      .map_err(|e| format!("libasound.so.2 has no {name}: {e}"))?;
    Ok(*f)
  }
  macro_rules! sym {
    ($name:literal) => {
      get(&lib, $name)?
    };
  }
  Ok(Lib {
    seq_open: sym!("snd_seq_open"),
    seq_close: sym!("snd_seq_close"),
    seq_set_client_name: sym!("snd_seq_set_client_name"),
    seq_client_info_malloc: sym!("snd_seq_client_info_malloc"),
    seq_client_info_free: sym!("snd_seq_client_info_free"),
    seq_client_info_set_client: sym!("snd_seq_client_info_set_client"),
    seq_client_info_get_client: sym!("snd_seq_client_info_get_client"),
    seq_query_next_client: sym!("snd_seq_query_next_client"),
    seq_port_info_malloc: sym!("snd_seq_port_info_malloc"),
    seq_port_info_free: sym!("snd_seq_port_info_free"),
    seq_port_info_set_client: sym!("snd_seq_port_info_set_client"),
    seq_port_info_set_port: sym!("snd_seq_port_info_set_port"),
    seq_port_info_get_port: sym!("snd_seq_port_info_get_port"),
    seq_port_info_get_name: sym!("snd_seq_port_info_get_name"),
    seq_port_info_get_capability: sym!("snd_seq_port_info_get_capability"),
    seq_port_info_get_type: sym!("snd_seq_port_info_get_type"),
    seq_query_next_port: sym!("snd_seq_query_next_port"),
    seq_create_simple_port: sym!("snd_seq_create_simple_port"),
    seq_connect_from: sym!("snd_seq_connect_from"),
    seq_connect_to: sym!("snd_seq_connect_to"),
    seq_poll_descriptors_count: sym!("snd_seq_poll_descriptors_count"),
    seq_poll_descriptors: sym!("snd_seq_poll_descriptors"),
    seq_event_input: sym!("snd_seq_event_input"),
    seq_event_output_direct: sym!("snd_seq_event_output_direct"),
    midi_event_new: sym!("snd_midi_event_new"),
    midi_event_free: sym!("snd_midi_event_free"),
    midi_event_no_status: sym!("snd_midi_event_no_status"),
    midi_event_encode: sym!("snd_midi_event_encode"),
    midi_event_decode: sym!("snd_midi_event_decode"),
    _lib: lib,
  })
}

fn errno_message(code: c_int) -> String {
  std::io::Error::from_raw_os_error(-code).to_string()
}

/// A sequencer client (`snd_seq_t`), closed on drop.
struct Seq {
  lib: &'static Lib,
  handle: Handle,
}

// SAFETY: a `snd_seq_t` may be used from any thread, one at a time; each
// `Seq` is owned by one thread at a time (the backend's mutex, or a reader
// thread).
unsafe impl Send for Seq {}

impl Seq {
  fn open(name: &str, nonblock: bool) -> Result<Self, String> {
    #[allow(
      clippy::disallowed_methods,
      reason = "a device node, not a file the program asked for"
    )]
    let has_sequencer = std::path::Path::new("/dev/snd/seq").exists();
    if !has_sequencer {
      // checked first so ALSA doesn't print its own error to stderr
      return Err(
        "the ALSA sequencer is not available (no /dev/snd/seq; load the snd-seq kernel module)"
          .to_string(),
      );
    }
    let lib = lib()?;
    let mut handle = std::ptr::null_mut();
    let flags = if nonblock { SND_SEQ_NONBLOCK } else { 0 };
    // SAFETY: valid out pointer and NUL-terminated name.
    let rc = unsafe {
      (lib.seq_open)(
        &mut handle,
        c"default".as_ptr(),
        SND_SEQ_OPEN_DUPLEX,
        flags,
      )
    };
    if rc < 0 {
      return Err(format!(
        "can't open the ALSA sequencer: {}",
        errno_message(rc)
      ));
    }
    let seq = Seq { lib, handle };
    let name = CString::new(name).unwrap_or_default();
    // SAFETY: open handle, NUL-terminated name.
    unsafe { (lib.seq_set_client_name)(seq.handle, name.as_ptr()) };
    Ok(seq)
  }

  fn create_port(&self, name: &str, caps: c_uint) -> Result<c_int, String> {
    let name = CString::new(name).unwrap_or_default();
    // SAFETY: open handle, NUL-terminated name.
    let port = unsafe {
      (self.lib.seq_create_simple_port)(
        self.handle,
        name.as_ptr(),
        caps,
        TYPE_MIDI_GENERIC | TYPE_APPLICATION,
      )
    };
    if port < 0 {
      return Err(format!("can't create a port: {}", errno_message(port)));
    }
    Ok(port)
  }

  fn poll_fds(&self) -> Vec<libc::pollfd> {
    // SAFETY: open handle; the buffer holds `count` entries.
    unsafe {
      let count = (self.lib.seq_poll_descriptors_count)(self.handle, POLLIN);
      let mut fds = vec![
        libc::pollfd {
          fd: -1,
          events: 0,
          revents: 0
        };
        count.max(0) as usize
      ];
      let n = (self.lib.seq_poll_descriptors)(
        self.handle,
        fds.as_mut_ptr(),
        fds.len() as c_uint,
        POLLIN,
      );
      fds.truncate(n.max(0) as usize);
      fds
    }
  }

  /// Polls for input events until `stop` is set, calling `on_event` with
  /// each.
  fn read_events(
    &self,
    stop: &AtomicBool,
    mut on_event: impl FnMut(&SeqEvent),
  ) {
    let mut fds = self.poll_fds();
    while !stop.load(Ordering::Relaxed) {
      // SAFETY: the fds come from the open handle.
      let ready = unsafe {
        libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_TIMEOUT_MS)
      };
      if ready < 0 {
        if std::io::Error::last_os_error().kind()
          == std::io::ErrorKind::Interrupted
        {
          continue;
        }
        return;
      }
      if ready == 0 {
        continue;
      }
      loop {
        let mut event: *mut SeqEvent = std::ptr::null_mut();
        // SAFETY: open nonblocking handle and a valid out pointer; the
        // event stays valid until the next call.
        let rc = unsafe { (self.lib.seq_event_input)(self.handle, &mut event) };
        if rc == -libc::EAGAIN || (rc < 0 && rc != -libc::ENOSPC) {
          // drained, or the client is gone
          break;
        }
        if rc < 0 || event.is_null() {
          // -ENOSPC: the input buffer overran and events were lost
          continue;
        }
        // SAFETY: ALSA returned a valid event.
        on_event(unsafe { &*event });
      }
    }
  }
}

impl Drop for Seq {
  fn drop(&mut self) {
    // SAFETY: the handle is open and not used after this.
    unsafe { (self.lib.seq_close)(self.handle) };
  }
}

/// A `snd_midi_event_t` encoder / decoder, freed on drop.
struct MidiEvent {
  lib: &'static Lib,
  dev: *mut c_void,
}

// SAFETY: a parser is used by one thread at a time (moved into a reader
// thread, or made and dropped within one `send`).
unsafe impl Send for MidiEvent {}

impl MidiEvent {
  fn new(lib: &'static Lib, size: usize) -> Result<Self, String> {
    let mut dev = std::ptr::null_mut();
    // SAFETY: valid out pointer.
    let rc = unsafe { (lib.midi_event_new)(size, &mut dev) };
    if rc < 0 {
      return Err(errno_message(rc));
    }
    // SAFETY: a valid parser; never use running status.
    unsafe { (lib.midi_event_no_status)(dev, 1) };
    Ok(Self { lib, dev })
  }
}

impl Drop for MidiEvent {
  fn drop(&mut self) {
    // SAFETY: allocated by snd_midi_event_new, not used after this.
    unsafe { (self.lib.midi_event_free)(self.dev) };
  }
}

/// Whether an event carries MIDI data (rather than a port or client
/// notification): the note, controller, system common and real-time types
/// (5-42) and system exclusive (130).
fn is_midi_event(kind: u8) -> bool {
  (5..=42).contains(&kind) || kind == EVENT_SYSEX
}

pub struct AlsaBackend {
  seq: Seq,
  /// The loopback pair's reader, kept for the process.
  #[allow(dead_code, reason = "held only to keep the port open")]
  loopback: Option<Reader>,
}

impl AlsaBackend {
  pub fn new() -> Result<Self, String> {
    Ok(Self {
      seq: Seq::open("Deno", false)?,
      loopback: None,
    })
  }
}

/// A thread reading one sequencer client, stopped and joined on drop.
struct Reader {
  stop: Arc<AtomicBool>,
  thread: Option<JoinHandle<()>>,
}

impl Drop for Reader {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

impl InputConnection for Reader {}

fn parse_address(id: &str) -> Result<(c_int, c_int), String> {
  let (client, port) = id
    .split_once(':')
    .ok_or_else(|| "the port is no longer available".to_string())?;
  let parse = |s: &str| {
    s.parse::<c_int>()
      .map_err(|_| "the port is no longer available".to_string())
  };
  Ok((parse(client)?, parse(port)?))
}

struct AlsaOutput {
  seq: Seq,
  port: c_int,
}

impl OutputConnection for AlsaOutput {
  fn send(&mut self, message: &[u8]) -> Result<(), String> {
    let encoder = MidiEvent::new(self.seq.lib, message.len().max(16))?;
    let mut event = SeqEvent::zeroed();
    let mut offset = 0;
    while offset < message.len() {
      // SAFETY: valid encoder, bytes and event.
      let used = unsafe {
        (self.seq.lib.midi_event_encode)(
          encoder.dev,
          message[offset..].as_ptr(),
          (message.len() - offset) as c_long,
          &mut event,
        )
      };
      if used <= 0 {
        return Err("can't encode the MIDI message".to_string());
      }
      offset += used as usize;
      if event.kind != EVENT_NONE {
        event.route_to_subscribers(self.port);
        // SAFETY: open handle and a complete event (a system exclusive
        // one points into the encoder's buffer, alive until `encoder` drops).
        let rc = unsafe {
          (self.seq.lib.seq_event_output_direct)(self.seq.handle, &mut event)
        };
        if rc < 0 {
          return Err(errno_message(rc));
        }
        event = SeqEvent::zeroed();
      }
    }
    Ok(())
  }
}

impl Backend for AlsaBackend {
  fn ports(&mut self) -> Vec<PortInfo> {
    let lib = self.seq.lib;
    let mut ports = Vec::new();
    // SAFETY: the info structs are allocated here and freed below; the
    // strings ALSA returns live in them and are copied before the next
    // query.
    unsafe {
      let mut client = std::ptr::null_mut();
      let mut port = std::ptr::null_mut();
      if (lib.seq_client_info_malloc)(&mut client) < 0 {
        return ports;
      }
      if (lib.seq_port_info_malloc)(&mut port) < 0 {
        (lib.seq_client_info_free)(client);
        return ports;
      }
      (lib.seq_client_info_set_client)(client, -1);
      while (lib.seq_query_next_client)(self.seq.handle, client) >= 0 {
        let client_id = (lib.seq_client_info_get_client)(client);
        (lib.seq_port_info_set_client)(port, client_id);
        (lib.seq_port_info_set_port)(port, -1);
        while (lib.seq_query_next_port)(self.seq.handle, port) >= 0 {
          let caps = (lib.seq_port_info_get_capability)(port);
          let kind = (lib.seq_port_info_get_type)(port);
          if kind & (TYPE_MIDI_GENERIC | TYPE_SYNTH | TYPE_APPLICATION) == 0
            || caps & CAP_NO_EXPORT != 0
          {
            continue;
          }
          let port_id = (lib.seq_port_info_get_port)(port);
          let name_ptr = (lib.seq_port_info_get_name)(port);
          let name = (!name_ptr.is_null())
            .then(|| CStr::from_ptr(name_ptr).to_string_lossy().into_owned());
          let software = kind & (TYPE_SYNTH | TYPE_SYNTHESIZER) != 0;
          let info = |input| PortInfo {
            id: format!("{client_id}:{port_id}"),
            name: name.clone(),
            manufacturer: None,
            version: None,
            input,
            software,
          };
          if caps & (CAP_READ | CAP_SUBS_READ) == CAP_READ | CAP_SUBS_READ {
            ports.push(info(true));
          }
          if caps & (CAP_WRITE | CAP_SUBS_WRITE) == CAP_WRITE | CAP_SUBS_WRITE {
            ports.push(info(false));
          }
        }
      }
      (lib.seq_port_info_free)(port);
      (lib.seq_client_info_free)(client);
    }
    ports
  }

  fn open_input(
    &mut self,
    id: &str,
    mut callback: InputCallback,
  ) -> Result<Box<dyn InputConnection>, String> {
    let (client, source_port) = parse_address(id)?;
    let seq = Seq::open("Deno", true)?;
    let port = seq.create_port(
      "Deno MIDI input",
      CAP_WRITE | CAP_SUBS_WRITE | CAP_NO_EXPORT,
    )?;
    // SAFETY: open handle and our port.
    let rc = unsafe {
      (seq.lib.seq_connect_from)(seq.handle, port, client, source_port)
    };
    if rc < 0 {
      return Err(format!("can't connect to the port: {}", errno_message(rc)));
    }
    let decoder = MidiEvent::new(seq.lib, 256)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
      .name("deno-midi-input".to_string())
      .spawn(move || {
        let decoder = decoder;
        let mut buf = vec![0u8; 256];
        seq.read_events(&thread_stop, |event| {
          if !is_midi_event(event.kind) {
            return;
          }
          let needed = if event.kind == EVENT_SYSEX {
            event.ext_len() + 16
          } else {
            16
          };
          if buf.len() < needed {
            buf.resize(needed, 0);
          }
          // SAFETY: valid decoder, a buffer of `buf.len()` bytes and an
          // event ALSA returned.
          let n = unsafe {
            (seq.lib.midi_event_decode)(
              decoder.dev,
              buf.as_mut_ptr(),
              buf.len() as c_long,
              event,
            )
          };
          if n > 0 {
            callback(&buf[..n as usize]);
          }
        });
      })
      .map_err(|e| e.to_string())?;
    Ok(Box::new(Reader {
      stop,
      thread: Some(thread),
    }))
  }

  fn open_output(
    &mut self,
    id: &str,
  ) -> Result<Box<dyn OutputConnection>, String> {
    let (client, dest_port) = parse_address(id)?;
    let seq = Seq::open("Deno", false)?;
    let port = seq.create_port(
      "Deno MIDI output",
      CAP_READ | CAP_SUBS_READ | CAP_NO_EXPORT,
    )?;
    // SAFETY: open handle and our port.
    let rc =
      unsafe { (seq.lib.seq_connect_to)(seq.handle, port, client, dest_port) };
    if rc < 0 {
      return Err(format!("can't connect to the port: {}", errno_message(rc)));
    }
    Ok(Box::new(AlsaOutput { seq, port }))
  }

  fn create_loopback(&mut self, name: &str) -> Result<(), String> {
    // one port anyone may write to and read from: each MIDI event it
    // receives goes out again to its readers
    let seq = Seq::open(name, true)?;
    let port = seq.create_port(
      name,
      CAP_READ | CAP_WRITE | CAP_SUBS_READ | CAP_SUBS_WRITE,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
      .name("deno-midi-loopback".to_string())
      .spawn(move || {
        seq.read_events(&thread_stop, |event| {
          if !is_midi_event(event.kind) {
            return;
          }
          let mut event = *event;
          event.route_to_subscribers(port);
          // SAFETY: open handle; a system exclusive payload points into
          // ALSA's input buffer, valid until the next read.
          unsafe { (seq.lib.seq_event_output_direct)(seq.handle, &mut event) };
        });
      })
      .map_err(|e| e.to_string())?;
    self.loopback = Some(Reader {
      stop,
      thread: Some(thread),
    });
    Ok(())
  }
}
