// Copyright 2018-2026 the Deno authors. MIT license.

//! CoreMIDI (macOS) and WinMM (Windows) through `midir`. On macOS the port
//! list comes from CoreMIDI directly (the `coremidi` crate midir is built
//! on), for each endpoint's manufacturer and driver version, which midir
//! doesn't expose; a port's id is its CoreMIDI unique ID, which is also
//! midir's port id.

use midir::MidiInput;
use midir::MidiInputConnection;
use midir::MidiOutput;
use midir::MidiOutputConnection;

use crate::backend::Backend;
use crate::backend::InputCallback;
use crate::backend::InputConnection;
use crate::backend::OutputConnection;
use crate::backend::PortInfo;

const CLIENT_NAME: &str = "Deno";

pub struct MidirBackend {
  #[cfg(target_os = "windows")]
  input: MidiInput,
  #[cfg(target_os = "windows")]
  output: MidiOutput,
  /// The loopback pair, kept alive for the process.
  #[allow(dead_code, reason = "held only to keep the ports open")]
  loopback: Vec<Box<dyn Send>>,
}

impl MidirBackend {
  pub fn new() -> Result<Self, String> {
    #[cfg(target_os = "windows")]
    {
      Ok(Self {
        input: MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?,
        output: MidiOutput::new(CLIENT_NAME).map_err(|e| e.to_string())?,
        loopback: Vec::new(),
      })
    }
    #[cfg(target_os = "macos")]
    {
      // fails here, not on the first open, when CoreMIDI can't be reached
      MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
      Ok(Self {
        loopback: Vec::new(),
      })
    }
  }
}

struct MidirInput(
  #[allow(
    dead_code,
    reason = "held only to keep the input port open; dropping it closes it"
  )]
  MidiInputConnection<()>,
);
impl InputConnection for MidirInput {}

struct MidirOutput(MidiOutputConnection);
impl OutputConnection for MidirOutput {
  fn send(&mut self, message: &[u8]) -> Result<(), String> {
    self.0.send(message).map_err(|e| e.to_string())
  }
}

#[cfg(target_os = "macos")]
fn coremidi_ports() -> Vec<PortInfo> {
  use coremidi::Destinations;
  use coremidi::Properties;
  use coremidi::Sources;

  fn info(endpoint: &coremidi::Endpoint, input: bool) -> Option<PortInfo> {
    let id = endpoint.unique_id()?;
    let manufacturer = endpoint
      .get_property(&Properties::manufacturer())
      .ok()
      .filter(|s: &String| !s.is_empty());
    let version = endpoint
      .get_property(&Properties::driver_version())
      .ok()
      .map(|v: i32| v.to_string());
    Some(PortInfo {
      id: id.to_string(),
      name: endpoint.display_name().or_else(|| endpoint.name()),
      manufacturer,
      version,
      input,
      software: false,
    })
  }

  let mut ports = Vec::new();
  ports.extend(Sources.into_iter().filter_map(|s| info(&s, true)));
  ports.extend(Destinations.into_iter().filter_map(|d| info(&d, false)));
  ports
}

impl Backend for MidirBackend {
  fn ports(&mut self) -> Vec<PortInfo> {
    #[cfg(target_os = "macos")]
    {
      coremidi_ports()
    }
    #[cfg(target_os = "windows")]
    {
      let mut ports = Vec::new();
      for port in self.input.ports() {
        let name = self.input.port_name(&port).ok();
        ports.push(PortInfo {
          id: port.id(),
          name,
          manufacturer: None,
          version: None,
          input: true,
          software: false,
        });
      }
      for port in self.output.ports() {
        let name = self.output.port_name(&port).ok();
        let software = name
          .as_deref()
          .is_some_and(|n| n == "Microsoft GS Wavetable Synth");
        ports.push(PortInfo {
          id: port.id(),
          name,
          manufacturer: None,
          version: None,
          input: false,
          software,
        });
      }
      ports
    }
  }

  fn open_input(
    &mut self,
    id: &str,
    mut callback: InputCallback,
  ) -> Result<Box<dyn InputConnection>, String> {
    let input = MidiInput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
    let port = input
      .find_port_by_id(id)
      .ok_or_else(|| "the port is no longer available".to_string())?;
    let connection = input
      .connect(
        &port,
        "Deno MIDI input",
        move |_, bytes, _| callback(bytes),
        (),
      )
      .map_err(|e| e.to_string())?;
    Ok(Box::new(MidirInput(connection)))
  }

  fn open_output(
    &mut self,
    id: &str,
  ) -> Result<Box<dyn OutputConnection>, String> {
    let output = MidiOutput::new(CLIENT_NAME).map_err(|e| e.to_string())?;
    let port = output
      .find_port_by_id(id)
      .ok_or_else(|| "the port is no longer available".to_string())?;
    let connection = output
      .connect(&port, "Deno MIDI output")
      .map_err(|e| e.to_string())?;
    Ok(Box::new(MidirOutput(connection)))
  }

  #[cfg(target_os = "macos")]
  fn create_loopback(&mut self, name: &str) -> Result<(), String> {
    use std::sync::Arc;
    use std::sync::Mutex;

    use midir::os::unix::VirtualInput;
    use midir::os::unix::VirtualOutput;

    // a virtual source (our input) and a virtual destination (our output)
    // of the same name: what the destination receives, the source sends
    let source = MidiOutput::new(CLIENT_NAME)
      .map_err(|e| e.to_string())?
      .create_virtual(name)
      .map_err(|e| e.to_string())?;
    let source = Arc::new(Mutex::new(source));
    let forward = source.clone();
    let destination = MidiInput::new(CLIENT_NAME)
      .map_err(|e| e.to_string())?
      .create_virtual(
        name,
        move |_, bytes, _| {
          if let Ok(mut source) = forward.lock() {
            let _ = source.send(bytes);
          }
        },
        (),
      )
      .map_err(|e| e.to_string())?;
    self.loopback.push(Box::new(source));
    self.loopback.push(Box::new(destination));
    Ok(())
  }

  #[cfg(target_os = "windows")]
  fn create_loopback(&mut self, _name: &str) -> Result<(), String> {
    // WinMM has no virtual ports (Windows MIDI Services' loopback endpoints
    // aren't reachable through it)
    Err("virtual MIDI ports are not supported on Windows".to_string())
  }
}
