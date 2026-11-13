// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS MIDI APIs behind one interface: CoreMIDI on macOS and WinMM on
//! Windows (both through `midir`), the ALSA sequencer on Linux (`alsa.rs`,
//! `libasound.so.2` loaded at run time, so the binary doesn't depend on it).
//! Other platforms have no backend and `requestMIDIAccess()` rejects with
//! `NotSupportedError`.

use std::sync::Mutex;
use std::sync::OnceLock;

/// A port as the OS reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
  /// Stable for the device as far as the OS allows: CoreMIDI's unique ID,
  /// WinMM's port name, ALSA's `client:port` address.
  pub id: String,
  pub name: Option<String>,
  pub manufacturer: Option<String>,
  pub version: Option<String>,
  pub input: bool,
  /// A software synthesizer (`MIDIOptions.software`): ALSA's synthesizer
  /// ports and Windows' "Microsoft GS Wavetable Synth".
  pub software: bool,
}

/// Receives each chunk of bytes an input delivers, on a backend thread.
pub type InputCallback = Box<dyn FnMut(&[u8]) + Send + 'static>;

/// An open input. Dropping it closes the port; the callback is not called
/// after the drop returns.
pub trait InputConnection: Send {}

/// An open output. Dropping it closes the port.
pub trait OutputConnection: Send {
  /// Sends one complete MIDI message.
  fn send(&mut self, message: &[u8]) -> Result<(), String>;
}

pub trait Backend: Send {
  fn ports(&mut self) -> Vec<PortInfo>;
  fn open_input(
    &mut self,
    id: &str,
    callback: InputCallback,
  ) -> Result<Box<dyn InputConnection>, String>;
  fn open_output(
    &mut self,
    id: &str,
  ) -> Result<Box<dyn OutputConnection>, String>;
  /// A virtual port pair named `name`: an output whose messages come back on
  /// the input of the same name (`DENO_MIDI_VIRTUAL_LOOPBACK`). Kept until
  /// the process exits.
  fn create_loopback(&mut self, name: &str) -> Result<(), String>;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn new_backend() -> Result<Box<dyn Backend>, String> {
  crate::midir_backend::MidirBackend::new()
    .map(|b| Box::new(b) as Box<dyn Backend>)
}

#[cfg(target_os = "linux")]
fn new_backend() -> Result<Box<dyn Backend>, String> {
  crate::alsa::AlsaBackend::new().map(|b| Box::new(b) as Box<dyn Backend>)
}

#[cfg(not(any(
  target_os = "macos",
  target_os = "windows",
  target_os = "linux"
)))]
fn new_backend() -> Result<Box<dyn Backend>, String> {
  Err("Web MIDI is not supported on this platform".to_string())
}

/// The process's backend, made on first use (with the loopback pair when
/// `DENO_MIDI_VIRTUAL_LOOPBACK` names one). A failure is kept: a system
/// without MIDI support doesn't gain it while the process runs.
pub fn backend() -> Result<&'static Mutex<Box<dyn Backend>>, String> {
  static BACKEND: OnceLock<Result<Mutex<Box<dyn Backend>>, String>> =
    OnceLock::new();
  BACKEND
    .get_or_init(|| {
      let mut backend = new_backend()?;
      if let Ok(name) = std::env::var("DENO_MIDI_VIRTUAL_LOOPBACK")
        && !name.is_empty()
      {
        // not fatal: Windows has no virtual ports, and a test looks for
        // the pair before using it
        if let Err(err) = backend.create_loopback(&name) {
          log::warn!("DENO_MIDI_VIRTUAL_LOOPBACK: {err}");
        }
      }
      Ok(Mutex::new(backend))
    })
    .as_ref()
    .map_err(|e| e.clone())
}
