// Copyright 2018-2026 the Deno authors. MIT license.

//! The jail's built-in per-package exceptions: npm packages whose approved
//! lifecycle scripts download a prebuilt binary (or, building from source,
//! Node's headers) at install time. Without an entry here such a script
//! gets no network in the jail and the install fails in a plain
//! `package.json` project, where nobody wrote a `supplyChain.jailScripts`
//! entry for it.
//!
//! Each entry is the narrowest grant that lets the script do its job: the
//! hosts it downloads from (redirect targets included), never the whole
//! network, plus the variables that point it at a mirror or skip the
//! download. A package the project lists itself under
//! `supplyChain.jailScripts.packages` gets exactly that entry instead
//! (`"net": false` included), `jailScripts.net: true` still grants every
//! host, and `paranoid` applies none of these. Packages that download into a
//! cache under the user's home (puppeteer, playwright's browsers, cypress) or
//! write outside their own folder (`@prisma/client`) are not here: they need
//! a `write` grant only the project can give.
//!
//! Data only, mirrored from denext's `src/build/jail-exceptions.ts`.

/// GitHub release downloads: the release URL and the hosts it redirects to.
const GITHUB: &[&str] = &[
  "github.com",
  "objects.githubusercontent.com",
  "release-assets.githubusercontent.com",
];

/// One package's built-in exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinException {
  /// The npm package name.
  pub name: &'static str,
  /// Hosts groups its scripts may reach (`host` or `host:port`).
  pub hosts: &'static [&'static [&'static str]],
  /// Environment variables passed through to its scripts.
  pub env: &'static [&'static str],
  /// Builds from source with node-gyp when no prebuilt binary fits, which
  /// needs Node's headers (DENEXT.md item 288: provided offline).
  pub node_gyp: bool,
}

impl BuiltinException {
  /// The hosts, flattened, in the order listed.
  pub fn host_list(&self) -> Vec<String> {
    let mut out = Vec::new();
    for group in self.hosts {
      for host in *group {
        if !out.iter().any(|h| h == host) {
          out.push(host.to_string());
        }
      }
    }
    out
  }
}

/// node-gyp's Node.js headers, for a build from source when the offline
/// copy can't be had.
const NODE_HEADERS: &[&str] = &["nodejs.org"];

const fn entry(
  name: &'static str,
  hosts: &'static [&'static [&'static str]],
  env: &'static [&'static str],
  node_gyp: bool,
) -> BuiltinException {
  BuiltinException {
    name,
    hosts,
    env,
    node_gyp,
  }
}

/// The built-in exceptions, sorted by name.
pub const BUILTIN_EXCEPTIONS: &[BuiltinException] = &[
  entry(
    "@prisma/engines",
    &[&["binaries.prisma.sh"]],
    &[
      "PRISMA_ENGINES_CHECKSUM_IGNORE_MISSING",
      "PRISMA_ENGINES_MIRROR",
      "PRISMA_QUERY_ENGINE_BINARY",
      "PRISMA_QUERY_ENGINE_LIBRARY",
      "PRISMA_SCHEMA_ENGINE_BINARY",
    ],
    false,
  ),
  entry(
    "@sentry/cli",
    &[&["downloads.sentry-cdn.com"]],
    &[
      "SENTRYCLI_CDNURL",
      "SENTRYCLI_SKIP_DOWNLOAD",
      "SENTRYCLI_USE_LOCAL",
    ],
    false,
  ),
  entry(
    "@tensorflow/tfjs-node",
    &[&["storage.googleapis.com"], NODE_HEADERS],
    &[],
    true,
  ),
  entry("better-sqlite3", &[GITHUB, NODE_HEADERS], &[], true),
  entry("canvas", &[GITHUB, NODE_HEADERS], &[], true),
  entry(
    "chromedriver",
    &[&[
      "chromedriver.storage.googleapis.com",
      "googlechromelabs.github.io",
      "storage.googleapis.com",
    ]],
    &[
      "CHROMEDRIVER_CDNBINARIESURL",
      "CHROMEDRIVER_CDNURL",
      "CHROMEDRIVER_FILEPATH",
      "CHROMEDRIVER_SKIP_DOWNLOAD",
      "DETECT_CHROMEDRIVER_VERSION",
    ],
    false,
  ),
  entry(
    "electron",
    &[GITHUB],
    &[
      "ELECTRON_CUSTOM_DIR",
      "ELECTRON_CUSTOM_FILENAME",
      "ELECTRON_GET_USE_PROXY",
      "ELECTRON_MIRROR",
      "ELECTRON_SKIP_BINARY_DOWNLOAD",
      "electron_config_cache",
    ],
    false,
  ),
  entry("ffmpeg-static", &[GITHUB], &["FFMPEG_BINARIES_URL"], false),
  entry("keytar", &[GITHUB, NODE_HEADERS], &[], true),
  entry("node-pty", &[NODE_HEADERS], &[], true),
  entry(
    "node-sass",
    &[GITHUB, NODE_HEADERS],
    &["SASS_BINARY_NAME", "SASS_BINARY_PATH", "SASS_BINARY_SITE"],
    true,
  ),
  entry("re2", &[GITHUB, NODE_HEADERS], &[], true),
  entry(
    "sharp",
    &[GITHUB, NODE_HEADERS],
    &["SHARP_FORCE_GLOBAL_LIBVIPS", "SHARP_IGNORE_GLOBAL_LIBVIPS"],
    true,
  ),
  entry("sqlite3", &[GITHUB, NODE_HEADERS], &[], true),
];

/// Download hosts packages declare for themselves (node-pre-gyp's
/// `binary.host`) that are reviewed: honoured as if the project listed them
/// under `jailScripts.knownPackages` (DENEXT.md item 124).
pub const BUILTIN_KNOWN_PACKAGES: &[(&str, &[&str])] = &[
  ("grpc", &["node-precompiled-binaries.grpc.io"]),
  ("nodegit", &["axonodegit.s3.amazonaws.com"]),
  ("wrtc", &["node-webrtc.s3.amazonaws.com"]),
];

/// The built-in exception of a package.
pub fn builtin_exception(name: &str) -> Option<&'static BuiltinException> {
  BUILTIN_EXCEPTIONS
    .binary_search_by(|e| e.name.cmp(name))
    .ok()
    .map(|i| &BUILTIN_EXCEPTIONS[i])
}

/// The reviewed hosts a package may declare for itself.
pub fn builtin_known_hosts(name: &str) -> &'static [&'static str] {
  BUILTIN_KNOWN_PACKAGES
    .iter()
    .find(|(n, _)| *n == name)
    .map(|(_, hosts)| *hosts)
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sorted_unique_and_hosts_only() {
    for pair in BUILTIN_EXCEPTIONS.windows(2) {
      assert!(
        pair[0].name < pair[1].name,
        "{} / {}",
        pair[0].name,
        pair[1].name
      );
    }
    for e in BUILTIN_EXCEPTIONS {
      let hosts = e.host_list();
      assert!(!hosts.is_empty(), "{}", e.name);
      for host in hosts {
        assert!(
          !host.contains('/') && !host.contains('*') && host != "localhost",
          "{}: {host}",
          e.name
        );
      }
      assert_eq!(builtin_exception(e.name), Some(e));
    }
    assert_eq!(builtin_exception("left-pad"), None);
  }

  #[test]
  fn native_packages_get_github_and_headers() {
    let e = builtin_exception("better-sqlite3").unwrap();
    assert_eq!(
      e.host_list(),
      [
        "github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
        "nodejs.org"
      ]
    );
    assert!(e.node_gyp);
    assert_eq!(
      builtin_known_hosts("grpc"),
      ["node-precompiled-binaries.grpc.io"]
    );
    assert!(builtin_known_hosts("sharp").is_empty());
  }
}
