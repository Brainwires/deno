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
//! Every built-in host is pinned to port 443 ([`BUILTIN_PORT`]): the
//! downloads are HTTPS, and a host name the jail lets a script reach is also
//! a way out for what the script can read, so a built-in grant opens that
//! host's HTTPS port and no other (no `CONNECT github.com:22`). A host the
//! project lists itself keeps the meaning it gives it (`host` is any port).
//!
//! Data only, mirrored from denext's `src/build/jail-exceptions.ts`.

/// The only port a built-in host grant opens.
pub const BUILTIN_PORT: u16 = 443;

/// GitHub release downloads: the release URL and the hosts it redirects to.
const GITHUB: &[&str] = &[
  "github.com:443",
  "objects.githubusercontent.com:443",
  "release-assets.githubusercontent.com:443",
];

/// One package's built-in exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinException {
  /// The npm package name.
  pub name: &'static str,
  /// Hosts groups its scripts may reach (`host:443`, see [`BUILTIN_PORT`]).
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
const NODE_HEADERS: &[&str] = &["nodejs.org:443"];

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
    &[&["binaries.prisma.sh:443"]],
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
    &[&["downloads.sentry-cdn.com:443"]],
    &[
      "SENTRYCLI_CDNURL",
      "SENTRYCLI_SKIP_DOWNLOAD",
      "SENTRYCLI_USE_LOCAL",
    ],
    false,
  ),
  entry(
    "@tensorflow/tfjs-node",
    &[&["storage.googleapis.com:443"], NODE_HEADERS],
    &[],
    true,
  ),
  entry("better-sqlite3", &[GITHUB, NODE_HEADERS], &[], true),
  entry("canvas", &[GITHUB, NODE_HEADERS], &[], true),
  entry(
    "chromedriver",
    &[&[
      "chromedriver.storage.googleapis.com:443",
      "googlechromelabs.github.io:443",
      "storage.googleapis.com:443",
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
///
/// The GitHub-hosted ones (`binary.host` on github.com, which only their
/// older versions declare: bcrypt 5, argon2 before 0.28) also get the hosts
/// GitHub redirects a release download to ([`DECLARED_HOST_REDIRECTS`]).
pub const BUILTIN_KNOWN_PACKAGES: &[(&str, &[&str])] = &[
  ("@discordjs/opus", &["github.com"]),
  ("argon2", &["github.com"]),
  ("bcrypt", &["github.com"]),
  ("grpc", &["node-precompiled-binaries.grpc.io"]),
  ("libxmljs", &["github.com"]),
  ("nodegit", &["axonodegit.s3.amazonaws.com"]),
  ("wrtc", &["node-webrtc.s3.amazonaws.com"]),
];

/// Hosts a declared download host hands its downloads off to: a script
/// allowed the declared host also gets these, as a download from it
/// fails without them. Built-in grants, so pinned to [`BUILTIN_PORT`].
pub const DECLARED_HOST_REDIRECTS: &[(&str, &[&str])] = &[(
  "github.com",
  &[
    "objects.githubusercontent.com:443",
    "release-assets.githubusercontent.com:443",
  ],
)];

/// The hosts a declared download host (`host`, any port) redirects to.
pub fn declared_host_redirects(host: &str) -> &'static [&'static str] {
  let name = match host.rsplit_once(':') {
    Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
    _ => host,
  };
  let name = name.trim_end_matches('.');
  DECLARED_HOST_REDIRECTS
    .iter()
    .find(|(h, _)| h.eq_ignore_ascii_case(name))
    .map(|(_, hosts)| *hosts)
    .unwrap_or_default()
}

/// The built-in exception of a package.
pub fn builtin_exception(name: &str) -> Option<&'static BuiltinException> {
  BUILTIN_EXCEPTIONS
    .binary_search_by(|e| e.name.cmp(name))
    .ok()
    .map(|i| &BUILTIN_EXCEPTIONS[i])
}

/// The grant for a declared `host[:port]` that the built-in list of reviewed
/// hosts (`hosts`, from [`builtin_known_hosts`]) covers: the host pinned to
/// [`BUILTIN_PORT`], or `None` when the list doesn't name it or the
/// declaration names another port.
pub fn builtin_known_grant(declared: &str, hosts: &[&str]) -> Option<String> {
  let (name, port) = match declared.rsplit_once(':') {
    Some((name, port)) if !name.contains(':') => {
      (name, Some(port.parse::<u16>().ok()?))
    }
    _ => (declared, None),
  };
  if port.is_some_and(|p| p != BUILTIN_PORT) {
    return None;
  }
  let name = name.trim_end_matches('.');
  hosts
    .iter()
    .any(|h| h.eq_ignore_ascii_case(name))
    .then(|| format!("{}:{BUILTIN_PORT}", name.to_ascii_lowercase()))
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
  }

  // a built-in grant reaches its download host's HTTPS port only
  #[test]
  fn builtin_hosts_are_pinned_to_443() {
    for e in BUILTIN_EXCEPTIONS {
      for host in e.host_list() {
        assert!(host.ends_with(":443"), "{}: {host}", e.name);
      }
    }
    for (_, hosts) in DECLARED_HOST_REDIRECTS {
      for host in *hosts {
        assert!(host.ends_with(":443"), "{host}");
      }
    }
    let github = builtin_known_hosts("bcrypt");
    assert_eq!(
      builtin_known_grant("github.com", github).as_deref(),
      Some("github.com:443")
    );
    assert_eq!(
      builtin_known_grant("GitHub.com.:443", github).as_deref(),
      Some("github.com:443")
    );
    assert_eq!(builtin_known_grant("github.com:22", github), None);
    assert_eq!(builtin_known_grant("github.com:x", github), None);
    assert_eq!(builtin_known_grant("example.com", github), None);
    assert_eq!(builtin_exception("left-pad"), None);
  }

  #[test]
  fn native_packages_get_github_and_headers() {
    let e = builtin_exception("better-sqlite3").unwrap();
    assert_eq!(
      e.host_list(),
      [
        "github.com:443",
        "objects.githubusercontent.com:443",
        "release-assets.githubusercontent.com:443",
        "nodejs.org:443"
      ]
    );
    assert!(e.node_gyp);
    assert_eq!(
      builtin_known_hosts("grpc"),
      ["node-precompiled-binaries.grpc.io"]
    );
    assert!(builtin_known_hosts("sharp").is_empty());
  }

  #[test]
  fn known_packages_sorted_and_github_redirects() {
    for pair in BUILTIN_KNOWN_PACKAGES.windows(2) {
      assert!(pair[0].0 < pair[1].0, "{} / {}", pair[0].0, pair[1].0);
    }
    for name in ["@discordjs/opus", "argon2", "bcrypt", "libxmljs"] {
      assert_eq!(builtin_known_hosts(name), ["github.com"], "{name}");
      assert!(builtin_exception(name).is_none(), "{name}");
    }
    let redirects = [
      "objects.githubusercontent.com:443",
      "release-assets.githubusercontent.com:443",
    ];
    assert_eq!(declared_host_redirects("github.com"), redirects);
    assert_eq!(declared_host_redirects("GitHub.com.:443"), redirects);
    assert!(declared_host_redirects("example.com").is_empty());
  }
}
