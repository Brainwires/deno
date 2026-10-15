// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;

use deno_runtime::deno_telemetry::OtelRuntimeConfig;

use crate::shared::ReleaseChannel;

pub fn otel_runtime_config() -> OtelRuntimeConfig {
  OtelRuntimeConfig {
    runtime_name: Cow::Borrowed("deno"),
    runtime_version: Cow::Borrowed(crate::version::DENO_VERSION_INFO.deno),
  }
}

const GIT_COMMIT_HASH: &str = env!("GIT_COMMIT_HASH");
const TYPESCRIPT: &str = "6.0.3";
pub const DENO_VERSION: &str = env!("DENO_VERSION");

/// The Node.js version that Deno reports through `process.version` /
/// `process.versions.node`, used to enforce package.json `engines.node`
/// constraints.
///
/// Re-exported from `ext/node`, which is the single source of truth shared with
/// the `process.version` polyfill, so the engines check and the reported value
/// can never drift.
pub use deno_node::NODE_VERSION;
// TODO(bartlomieju): ideally we could remove this const.
const IS_CANARY: bool = option_env!("DENO_CANARY").is_some();
// TODO(bartlomieju): this is temporary, to allow Homebrew to cut RC releases as well
const IS_RC: bool = option_env!("DENO_RC").is_some();

pub static DENO_VERSION_INFO: std::sync::LazyLock<DenoVersionInfo> =
  std::sync::LazyLock::new(|| {
    #[cfg(not(all(
      debug_assertions,
      target_os = "macos",
      target_arch = "x86_64"
    )))]
    let release_channel = libsui::find_section("denover")
      .ok()
      .flatten()
      .and_then(|buf| std::str::from_utf8(buf).ok())
      .and_then(|str_| ReleaseChannel::deserialize(str_).ok())
      .unwrap_or({
        if IS_CANARY {
          ReleaseChannel::Canary
        } else if IS_RC {
          ReleaseChannel::Rc
        } else {
          release_channel_from_version_string(DENO_VERSION)
        }
      });

    #[cfg(all(debug_assertions, target_os = "macos", target_arch = "x86_64"))]
    let release_channel = if IS_CANARY {
      ReleaseChannel::Canary
    } else if IS_RC {
      ReleaseChannel::Rc
    } else {
      release_channel_from_version_string(DENO_VERSION)
    };

    let denext = denext_version(option_env!("DENEXT_VERSION"), DENO_VERSION)
      .filter(|_| release_channel != ReleaseChannel::Canary);

    DenoVersionInfo {
      deno: if release_channel == ReleaseChannel::Canary {
        concat!(env!("DENO_VERSION"), "+", env!("GIT_COMMIT_HASH_SHORT"))
      } else if let Some(denext) = denext {
        denext
      } else {
        env!("DENO_VERSION")
      },

      release_channel,

      git_hash: GIT_COMMIT_HASH,

      // Keep in sync with `deno` field.
      user_agent: if release_channel == ReleaseChannel::Canary {
        concat!(
          "Deno/",
          env!("DENO_VERSION"),
          "+",
          env!("GIT_COMMIT_HASH_SHORT")
        )
      } else if let Some(denext) = denext {
        Box::leak(format!("Deno/{denext}").into_boxed_str())
      } else {
        concat!("Deno/", env!("DENO_VERSION"))
      },

      typescript: TYPESCRIPT,
    }
  });

pub struct DenoVersionInfo {
  /// Human-readable version of the current Deno binary.
  ///
  /// For stable release, a semver, eg. `v1.46.2`.
  /// For canary release, a semver + 7-char git hash, eg. `v1.46.3+asdfqwq`.
  pub deno: &'static str,

  pub release_channel: ReleaseChannel,

  /// A full git hash.
  pub git_hash: &'static str,

  /// A user-agent header that will be used in HTTP client.
  pub user_agent: &'static str,

  pub typescript: &'static str,
}

impl DenoVersionInfo {
  /// A denext release build's version (`DENEXT_VERSION` at compile time,
  /// e.g. `2.9.7-denext.20`), or `None` for any other build.
  pub fn denext(&self) -> Option<&'static str> {
    (self.release_channel != ReleaseChannel::Canary
      && self.deno != DENO_VERSION)
      .then_some(self.deno)
  }

  /// For stable release, a semver like, eg. `v1.46.2`.
  /// For canary release a full git hash, eg. `9bdab6fb6b93eb43b1930f40987fa4997287f9c8`.
  pub fn version_or_git_hash(&self) -> &'static str {
    if self.release_channel == ReleaseChannel::Canary {
      self.git_hash
    } else {
      DENO_VERSION
    }
  }
}

/// A denext release build's version: `DENEXT_VERSION` at compile time (the
/// release, e.g. `2.9.7-denext.15`; see DENEXT.md "Release archives"), used
/// only when it extends this build's Deno version with `-` or `+`. It becomes
/// `Deno.version.deno`, `deno --version` and the user agent, so the binary
/// says which build it is. `deno compile` takes the release's own `denort`
/// (cli/standalone/binary.rs, `denext_denort`); downloads of `libdenort`
/// and `deno upgrade` keep using the plain Deno version (`DENO_VERSION`).
fn denext_version(
  denext: Option<&'static str>,
  deno: &str,
) -> Option<&'static str> {
  denext.filter(|v| {
    v.strip_prefix(deno)
      .is_some_and(|rest| rest.len() > 1 && rest.starts_with(['-', '+']))
  })
}

fn release_channel_from_version_string(version: &str) -> ReleaseChannel {
  let v = deno_semver::Version::parse_standard(version).ok();
  match v.and_then(|v| v.pre.first().map(|s| s.as_str().to_string())) {
    Some(ref s) if s == "alpha" => ReleaseChannel::Alpha,
    Some(ref s) if s == "beta" => ReleaseChannel::Beta,
    Some(ref s) if s == "rc" => ReleaseChannel::Rc,
    _ => ReleaseChannel::Stable,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn denext_version_extends_the_deno_version() {
    assert_eq!(denext_version(None, "2.9.7"), None);
    assert_eq!(
      denext_version(Some("2.9.7-denext.15"), "2.9.7"),
      Some("2.9.7-denext.15")
    );
    assert_eq!(
      denext_version(Some("2.9.7+denext.15"), "2.9.7"),
      Some("2.9.7+denext.15")
    );
    // only a suffix on this build's own version
    assert_eq!(denext_version(Some(""), "2.9.7"), None);
    assert_eq!(denext_version(Some("2.9.7"), "2.9.7"), None);
    assert_eq!(denext_version(Some("2.9.7-"), "2.9.7"), None);
    assert_eq!(denext_version(Some("2.9.71"), "2.9.7"), None);
    assert_eq!(denext_version(Some("1.0.0-denext.1"), "2.9.7"), None);
    // a denext version is still semver, on the stable channel
    assert_eq!(
      release_channel_from_version_string("2.9.7-denext.15"),
      ReleaseChannel::Stable
    );
    assert!(deno_semver::Version::parse_standard("2.9.7-denext.15").is_ok());
  }
}
