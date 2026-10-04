// Copyright 2018-2026 the Deno authors. MIT license.

//! The development switches a desktop runtime reads from its environment,
//! honoured only in a development build.
//!
//! `deno desktop --hmr` / `--inspect*` compile a throwaway binary and launch
//! it with these variables:
//!
//! * `DENO_DESKTOP_HMR=<dir>`: watch `<dir>` and hot-replace the app's
//!   modules with the files there (`Debugger.setScriptSource`), and resolve
//!   `node_modules` from it;
//! * `DENO_DESKTOP_DEV_URL=<url>`: load the window from a dev server (whose
//!   origin then counts as the app's own for bindings);
//! * `DENO_DESKTOP_FRAMEWORK_DEV`: the entrypoint boots a framework dev server
//!   and the working directory is the source tree;
//! * `DENO_DESKTOP_INSPECT_INTERNAL_PORT=<addr>` (+ `_BRK`, `_WAIT`): open a V8
//!   inspector on `<addr>`;
//! * `DENO_DESKTOP_MUX_WS=<addr>`: the DevTools multiplexer the windows'
//!   DevTools attach to.
//!
//! In a packaged app every one of them hands control of the app to whoever
//! can set its environment (`open --env`, a launcher, another program): its
//! code reloaded from their directory, an inspector to evaluate code in it,
//! its window on their site. So they are honoured only when the binary says it
//! is a development build (`Metadata::desktop_dev`, which only `deno
//! desktop`'s own development path writes and a code signature covers); any
//! other runtime ignores them, with a note on stderr.
//!
//! The switches are read once, at startup ([`init`]), and every reader goes
//! through [`get`].

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The variable names (see the module docs).
pub const HMR_ENV: &str = "DENO_DESKTOP_HMR";
pub const DEV_URL_ENV: &str = "DENO_DESKTOP_DEV_URL";
pub const FRAMEWORK_DEV_ENV: &str = "DENO_DESKTOP_FRAMEWORK_DEV";
pub const INSPECT_PORT_ENV: &str = "DENO_DESKTOP_INSPECT_INTERNAL_PORT";
pub const INSPECT_BRK_ENV: &str = "DENO_DESKTOP_INSPECT_BRK";
pub const INSPECT_WAIT_ENV: &str = "DENO_DESKTOP_INSPECT_WAIT";
pub const MUX_WS_ENV: &str = "DENO_DESKTOP_MUX_WS";

/// Every development switch, for the stderr note.
pub const ALL: &[&str] = &[
  HMR_ENV,
  DEV_URL_ENV,
  FRAMEWORK_DEV_ENV,
  INSPECT_PORT_ENV,
  INSPECT_BRK_ENV,
  INSPECT_WAIT_ENV,
  MUX_WS_ENV,
];

/// The development switches in effect: all off unless this is a
/// development build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevSwitches {
  pub hmr_dir: Option<PathBuf>,
  pub dev_url: Option<String>,
  pub framework_dev: bool,
  /// The raw value (parsed, with its error, where it is used).
  pub inspect_port: Option<String>,
  pub inspect_brk: bool,
  pub inspect_wait: bool,
  pub mux_ws: Option<String>,
}

impl DevSwitches {
  /// The switches `lookup` reports, if `dev_build`; else none, and the names
  /// of the ones that were set (to say they are ignored).
  pub fn resolve(
    dev_build: bool,
    lookup: impl Fn(&str) -> Option<OsString>,
  ) -> (Self, Vec<&'static str>) {
    if !dev_build {
      let ignored = ALL
        .iter()
        .copied()
        .filter(|k| lookup(k).is_some())
        .collect();
      return (Self::default(), ignored);
    }
    let string = |k: &str| lookup(k).and_then(|v| v.into_string().ok());
    let switches = Self {
      hmr_dir: lookup(HMR_ENV).map(PathBuf::from),
      dev_url: string(DEV_URL_ENV),
      framework_dev: lookup(FRAMEWORK_DEV_ENV).is_some(),
      inspect_port: string(INSPECT_PORT_ENV),
      inspect_brk: lookup(INSPECT_BRK_ENV).is_some(),
      inspect_wait: lookup(INSPECT_WAIT_ENV).is_some(),
      mux_ws: string(MUX_WS_ENV),
    };
    (switches, Vec::new())
  }

  /// A development run of any kind (a dev server, HMR, framework dev).
  pub fn is_dev_run(&self) -> bool {
    self.hmr_dir.is_some() || self.dev_url.is_some() || self.framework_dev
  }

  /// A framework dev server serves the app (external or in the runtime).
  pub fn is_framework_dev(&self) -> bool {
    self.dev_url.is_some() || self.framework_dev
  }
}

static SWITCHES: OnceLock<DevSwitches> = OnceLock::new();

/// Read the switches from this process's environment, once, for a binary
/// that is (`dev_build`) or isn't a development build. Later calls keep the
/// first answer.
#[allow(
  clippy::print_stderr,
  reason = "a note on an ignored switch, before logging matters"
)]
pub fn init(dev_build: bool) -> &'static DevSwitches {
  SWITCHES.get_or_init(|| {
    let (switches, ignored) =
      DevSwitches::resolve(dev_build, |k| std::env::var_os(k));
    if !ignored.is_empty() {
      eprintln!(
        "[desktop] ignoring {} (development switches are honoured only by a \
         `deno desktop --hmr` / `--inspect` development build)",
        ignored.join(", ")
      );
    }
    switches
  })
}

/// The switches in effect; all off before [`init`] ran.
pub fn get() -> &'static DevSwitches {
  static OFF: OnceLock<DevSwitches> = OnceLock::new();
  SWITCHES
    .get()
    .unwrap_or_else(|| OFF.get_or_init(DevSwitches::default))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn env<'a>(
    vars: &'a [(&'a str, &'a str)],
  ) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |k| {
      vars
        .iter()
        .find(|(n, _)| *n == k)
        .map(|(_, v)| OsString::from(v))
    }
  }

  const EVERYTHING: &[(&str, &str)] = &[
    (HMR_ENV, "/tmp/evil"),
    (DEV_URL_ENV, "https://evil.example"),
    (FRAMEWORK_DEV_ENV, "1"),
    (INSPECT_PORT_ENV, "127.0.0.1:9229"),
    (INSPECT_BRK_ENV, "1"),
    (INSPECT_WAIT_ENV, "1"),
    (MUX_WS_ENV, "127.0.0.1:9230"),
  ];

  #[test]
  fn a_packaged_app_honours_no_development_switch() {
    let (switches, ignored) = DevSwitches::resolve(false, env(EVERYTHING));
    assert_eq!(switches, DevSwitches::default());
    assert!(!switches.is_dev_run());
    assert!(!switches.is_framework_dev());
    // Every one is named in the note.
    assert_eq!(ignored, ALL.to_vec());
    let (_, ignored) = DevSwitches::resolve(false, env(&[]));
    assert!(ignored.is_empty());
  }

  #[test]
  fn a_development_build_reads_them() {
    let (switches, ignored) = DevSwitches::resolve(true, env(EVERYTHING));
    assert!(ignored.is_empty());
    assert_eq!(switches.hmr_dir, Some(PathBuf::from("/tmp/evil")));
    assert_eq!(switches.dev_url.as_deref(), Some("https://evil.example"));
    assert!(switches.framework_dev);
    assert_eq!(switches.inspect_port.as_deref(), Some("127.0.0.1:9229"));
    assert!(switches.inspect_brk && switches.inspect_wait);
    assert_eq!(switches.mux_ws.as_deref(), Some("127.0.0.1:9230"));
    assert!(switches.is_dev_run() && switches.is_framework_dev());
    let (none, _) = DevSwitches::resolve(true, env(&[]));
    assert_eq!(none, DevSwitches::default());
    let (hmr_only, _) = DevSwitches::resolve(true, env(&[(HMR_ENV, "/src")]));
    assert!(hmr_only.is_dev_run() && !hmr_only.is_framework_dev());
  }

  #[test]
  fn before_init_everything_is_off() {
    // A reader that runs before startup read the switches sees none.
    if SWITCHES.get().is_none() {
      assert_eq!(get(), &DevSwitches::default());
    }
  }
}
