// Copyright 2018-2026 the Deno authors. MIT license.

//! The Seatbelt (SBPL) profile of a jailed lifecycle script on macOS, run
//! under `sandbox-exec` by [`crate::macos`]: the script's process tree gets a
//! deny-by-default profile (DENEXT.md item 124) that allows only what a script, a shell, a
//! compiler or a download needs: reading files (minus the credential files),
//! writing beneath the jail's roots, forking and running programs, signals
//! within the sandbox, `sysctl` reads (minus the process table, which holds
//! every process's environment), POSIX IPC, and a handful of mach services
//! (the user database, the per-user temp directories, logging). Everything
//! else is refused: LaunchServices (`open`), launchd job submission,
//! AppleEvents (`osascript`), the pasteboard, the keychain (`securityd`,
//! `trustd`), the DNS responder, hard links from outside the roots, the
//! terminals (`/dev/tty`, `/dev/ttys*`: no `TIOCSTI`), and every other mach
//! service. A setuid program (`/bin/ps`, `sudo`) can't be started inside it,
//! as under Linux's `no_new_privs`.
//!
//! The network: none (not even Unix sockets; a Docker socket runs anything),
//! the whole of it, or only the script's allow-listing proxy
//! (`localhost:<port>`, `crate::proxy`) with no DNS; without the whole network
//! a script can't listen for connections either. Seatbelt checks the resolved
//! path, so a symlink in a writable root can't lead outside it.
//!
//! Generating the text is pure string building, so it builds and is tested
//! on every platform; only running it needs macOS.

use std::path::Path;
use std::path::PathBuf;

use crate::HomeReadPolicy;
use crate::OsNet;

fn quote(path: &Path) -> String {
  let text = path.to_string_lossy();
  format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The terminal devices, which a jailed script may neither read, write nor
/// `ioctl` (a `TIOCSTI` types into the user's shell).
const TTY_FILTERS: &str = "(literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\")";

/// The mach services a script's programs need: the user database
/// (`getpwuid`), the per-user temp and cache directories (`confstr`, which
/// compilers ask for), and logging.
const MACH_ALLOW: &[&str] = &[
  "com.apple.system.opendirectoryd.libinfo",
  "com.apple.system.opendirectoryd.membership",
  "com.apple.bsd.dirhelper",
  "com.apple.system.logger",
  "com.apple.logd",
  "com.apple.system.notification_center",
];

/// Services refused by name as well as by the default, so a later allow
/// can't widen past them: the keychain and trust evaluation, LaunchServices,
/// the pasteboard, consent prompts, DNS.
const MACH_DENY: &[&str] = &[
  "com.apple.SecurityServer",
  "com.apple.securityd",
  "com.apple.securityd.xpc",
  "com.apple.trustd",
  "com.apple.trustd.agent",
  "com.apple.coreservices.launchservicesd",
  "com.apple.lsd.mapdb",
  "com.apple.lsd.modifydb",
  "com.apple.pasteboard.1",
  "com.apple.tccd",
  "com.apple.tccd.system",
  "com.apple.dnssd.service",
];

/// The read operations a home policy refuses: a file's contents (and a
/// directory's listing) and its extended attributes.
const HOME_READ_OPS: &str = "file-read-data file-read-xattr";

/// The Seatbelt profile.
pub fn profile(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  net: OsNet,
) -> String {
  profile_with_home_read(write_roots, deny_read, None, net)
}

/// The Seatbelt profile, with no file contents (nor directory listings)
/// under the home directory but the paths `home_read` allows, when given
/// (DENEXT.md item 437). Without it, the same text as [`profile`].
pub fn profile_with_home_read(
  write_roots: &[PathBuf],
  deny_read: &[PathBuf],
  home_read: Option<&HomeReadPolicy>,
  net: OsNet,
) -> String {
  let mut p = String::from("(version 1)\n(deny default)\n");
  // files: read everything but the credential files and the terminals;
  // write (and hard-link, which needs the source inside too) beneath the
  // roots and to the null devices; ioctl everything but the terminals
  p.push_str("(allow file-read*)\n");
  // with a home policy, no file contents (nor extended attributes, which
  // carry such things as a download's origin) under the home directories but
  // the allowed paths, refused as if missing: a tool that looks for an
  // optional file in a parent directory goes on as it would without one.
  // Metadata stays readable: a program resolving a path (`realpath`) stats
  // every directory on the way, as Landlock allows on Linux.
  if let Some(home_read) = home_read
    && !home_read.homes.is_empty()
  {
    p.push_str(&format!("(deny {HOME_READ_OPS} (with errno ENOENT)"));
    for home in &home_read.homes {
      p.push_str(&format!(" (subpath {})", quote(home)));
    }
    p.push_str(")\n");
    if !home_read.allowed.is_empty() {
      p.push_str(&format!("(allow {HOME_READ_OPS}"));
      for path in &home_read.allowed {
        p.push_str(&format!(" (subpath {})", quote(path)));
      }
      p.push_str(")\n");
    }
  }
  let mut denied = String::from(TTY_FILTERS);
  for path in deny_read {
    denied.push_str(&format!(" (subpath {})", quote(path)));
  }
  p.push_str(&format!("(deny file-read* {denied})\n"));
  // a rule for one operation outranks a `file-read*` one whatever the
  // order: the denied paths inside an allowed path need a deny of the same
  // operations of their own, after the allow
  if home_read.is_some() {
    p.push_str(&format!("(deny {HOME_READ_OPS} {denied})\n"));
  }
  p.push_str("(allow file-write*");
  for root in write_roots {
    p.push_str(&format!(" (subpath {})", quote(root)));
  }
  for dev in [
    "/dev/null",
    "/dev/zero",
    "/dev/dtracehelper",
    "/dev/stdout",
    "/dev/stderr",
  ] {
    p.push_str(&format!(" (literal \"{dev}\")"));
  }
  p.push_str(" (regex #\"^/dev/fd/\"))\n");
  if !write_roots.is_empty() {
    p.push_str("(allow file-link");
    for root in write_roots {
      p.push_str(&format!(" (subpath {})", quote(root)));
    }
    p.push_str(")\n");
  }
  p.push_str("(allow file-ioctl)\n");
  p.push_str("(deny file-ioctl ");
  p.push_str(TTY_FILTERS);
  p.push_str(")\n");
  // processes: fork and run programs (a setuid one can't start under a
  // profile), signal and inspect only the sandbox's own, read sysctls but
  // not the process table (`kern.procargs2` is every process's environment)
  p.push_str("(allow process-fork)\n(allow process-exec)\n");
  p.push_str("(allow signal (target same-sandbox))\n");
  p.push_str("(allow process-info* (target same-sandbox))\n");
  p.push_str("(allow sysctl-read)\n");
  p.push_str("(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))\n");
  p.push_str("(allow ipc-posix-sem)\n(allow ipc-posix-shm*)\n");
  // mach services: the short list, and the refused ones by name
  p.push_str("(allow mach-lookup");
  for name in MACH_ALLOW {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  p.push_str("(deny mach-lookup");
  for name in MACH_DENY {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  p.push_str("(deny lsopen)\n(deny appleevent-send)\n(deny job-creation)\n");
  // the network: Unix sockets reach other daemons (a Docker socket runs
  // anything): none, whatever the network setting, but the DNS resolver's
  // when it's allowed
  match net {
    OsNet::Open => {
      p.push_str("(allow network*)\n");
      p.push_str("(deny network-outbound (remote unix-socket))\n");
      p.push_str(
        "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
      );
      p.push_str(
        "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))\n",
      );
    }
    OsNet::Denied => {
      p.push_str("(deny network*)\n");
    }
    OsNet::ProxyOnly(port) => {
      // the last matching rule wins: the proxy's port on the loopback
      // interface only (TCP and UDP to anywhere else are denied), and no
      // serving: a listening socket would hand data to whoever connects in
      p.push_str("(deny network*)\n");
      p.push_str(&format!(
        "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
      ));
    }
  }
  p
}

// ---------------------------------------------------------------------------
// The cli profile (`--kernel-sandbox=enforce`, macOS Mode A)
// ---------------------------------------------------------------------------

use crate::plan::MacosPlan;
use crate::plan::NetPlan;
use crate::plan::Ports;
use crate::plan::SbFilter;
use crate::plan::SbScope;
use crate::plan::SbUnix;

fn quote_str(text: &str) -> String {
  format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn filter_text(f: &SbFilter) -> String {
  match f {
    SbFilter::Subpath(p) => format!("(subpath {})", quote_str(p)),
    SbFilter::Literal(p) => format!("(literal {})", quote_str(p)),
    SbFilter::Regex(r) => format!("(regex #\"{}\")", r.replace('"', "\\\"")),
  }
}

fn filters_text(filters: &[SbFilter]) -> String {
  filters
    .iter()
    .map(filter_text)
    .collect::<Vec<_>>()
    .join(" ")
}

/// `(allow <op>)` for every path, `(allow <op> <filters>)` for a scope, or
/// nothing for an empty one.
fn allow_scope(p: &mut String, op: &str, scope: &SbScope) {
  match scope {
    SbScope::All => p.push_str(&format!("(allow {op})\n")),
    SbScope::Filters { filters } if !filters.is_empty() => {
      p.push_str(&format!("(allow {op} {})\n", filters_text(filters)));
    }
    SbScope::Filters { .. } => {}
  }
}

/// The library paths dyld maps code from (and the shared cache), whatever
/// the grant: without them nothing starts.
const SYSTEM_CODE: &[&str] = &[
  "/usr/lib",
  "/System",
  "/Library/Apple",
  "/System/Volumes/Preboot/Cryptexes",
  "/private/var/db/dyld",
];

/// The marker pair of the cli profile: a process can tell that a kernel
/// sandbox of this runtime holds it when no carrier was handed down
/// (`crate::inherit::kernel_reports_marked`): reading
/// [`MARKER_SYSCTL_ALLOWED`] works and reading [`MARKER_SYSCTL_DENIED`] is
/// refused. A profile that allows every sysctl allows both, one that lists
/// the sysctls it allows doesn't name the first; neither reads as marked.
/// Both exist on every macOS, nothing needs the denied one (whether the
/// machine booted single-user), and the pair grants nothing.
pub const MARKER_SYSCTL_ALLOWED: &str = "kern.nisdomainname";
/// See [`MARKER_SYSCTL_ALLOWED`].
pub const MARKER_SYSCTL_DENIED: &str = "kern.singleuser";

/// The mach services the runtime needs whatever the grant: the user
/// database, temp directories, logging, notifications, preferences
/// (CoreFoundation, read-only).
const CLI_MACH_ALLOW: &[&str] = &[
  "com.apple.system.opendirectoryd.libinfo",
  "com.apple.system.opendirectoryd.membership",
  "com.apple.bsd.dirhelper",
  "com.apple.system.logger",
  "com.apple.logd",
  "com.apple.system.notification_center",
  "com.apple.cfprefsd.daemon",
  "com.apple.cfprefsd.agent",
];

/// The cli Seatbelt profile for `plan`: deny by default; file metadata
/// readable everywhere (path resolution needs it on every ancestor; a file's
/// existence and size are visible, which the gaps document); the read and write scopes; then the exact denies;
/// then the re-allows the denies must not swallow (the last matching rule
/// wins). Code maps only from the load scope, the runtime and the system
/// libraries. No LaunchServices, AppleEvents or launchd job submission.
pub fn cli_profile(plan: &MacosPlan) -> String {
  if plan.deny_only {
    return deny_only_profile(plan);
  }
  let mut p = String::from("(version 1)\n(deny default)\n");
  // files: allows
  p.push_str("(allow file-read-metadata)\n");
  allow_scope(&mut p, "file-read*", &plan.read);
  allow_scope(&mut p, "file-write*", &plan.write);
  // then the denies
  if !plan.deny_read.is_empty() {
    p.push_str(&format!(
      "(deny file-read* {})\n",
      filters_text(&plan.deny_read)
    ));
  }
  if !plan.deny_write.is_empty() {
    p.push_str(&format!(
      "(deny file-write* {})\n",
      filters_text(&plan.deny_write)
    ));
  }
  // then the re-allows
  if !plan.reallow_read.is_empty() {
    p.push_str(&format!(
      "(allow file-read* {})\n",
      filters_text(&plan.reallow_read)
    ));
  }
  if !plan.reallow_write.is_empty() {
    p.push_str(&format!(
      "(allow file-write* {})\n",
      filters_text(&plan.reallow_write)
    ));
  }
  // the terminals: every one refused but those the plan names (the
  // controlling terminal, the standard streams' own), never ioctl-able
  for op in ["file-read*", "file-write*"] {
    p.push_str(&format!("(deny {op} {TTY_FILTERS})\n"));
  }
  if !plan.ttys.is_empty() {
    let literals = plan
      .ttys
      .iter()
      .map(|t| format!("(literal {})", quote_str(t)))
      .collect::<Vec<_>>()
      .join(" ");
    for op in ["file-read*", "file-write*"] {
      p.push_str(&format!("(allow {op} {literals})\n"));
    }
  }
  p.push_str("(allow file-ioctl)\n");
  p.push_str(&format!("(deny file-ioctl {TTY_FILTERS})\n"));
  // code
  let mut map: Vec<SbFilter> = SYSTEM_CODE
    .iter()
    .map(|s| SbFilter::Subpath(s.to_string()))
    .collect();
  match &plan.map_exec {
    SbScope::All => p.push_str("(allow file-map-executable)\n"),
    SbScope::Filters { filters } => {
      map.extend(filters.iter().cloned());
      p.push_str(&format!(
        "(allow file-map-executable {})\n",
        filters_text(&map)
      ));
    }
  }
  // processes
  p.push_str("(allow process-fork)\n");
  if plan.exec && plan.exec_only.is_empty() {
    p.push_str("(allow process-exec)\n");
  } else {
    p.push_str("(deny process-exec)\n");
  }
  if plan.exec && !plan.exec_only.is_empty() {
    // a strict exec scope: these files only (the last matching rule wins)
    let only: Vec<SbFilter> = plan
      .exec_only
      .iter()
      .map(|p| SbFilter::Literal(p.clone()))
      .collect();
    p.push_str(&format!("(allow process-exec {})\n", filters_text(&only)));
  }
  p.push_str("(allow signal (target same-sandbox))\n");
  p.push_str("(allow process-info* (target same-sandbox))\n");
  p.push_str("(allow sysctl-read)\n");
  // the marker pair (the allowed one is covered by the line above)
  p.push_str(&format!(
    "(deny sysctl-read (with no-report) (sysctl-name {}))\n",
    quote_str(MARKER_SYSCTL_DENIED)
  ));
  if !plan.denied_sysctls.is_empty() {
    let names = plan
      .denied_sysctls
      .iter()
      .map(|n| match n.strip_suffix('*') {
        Some(prefix) => format!("(sysctl-name-prefix {})", quote_str(prefix)),
        None => format!("(sysctl-name {})", quote_str(n)),
      })
      .collect::<Vec<_>>()
      .join(" ");
    p.push_str(&format!("(deny sysctl-read {names})\n"));
  }
  p.push_str("(allow ipc-posix-sem)\n(allow ipc-posix-shm*)\n");
  // mach services
  let net_on = !matches!(plan.net, NetPlan::Denied);
  let reads = !plan.read.is_empty();
  let mut mach: Vec<&str> = CLI_MACH_ALLOW.to_vec();
  if net_on {
    mach.push("com.apple.dnssd.service");
  }
  if plan.system_ca {
    mach.extend([
      "com.apple.trustd",
      "com.apple.trustd.agent",
      "com.apple.SecurityServer",
    ]);
  }
  if reads {
    mach.push("com.apple.FSEvents");
  }
  p.push_str("(allow mach-lookup");
  for name in &mach {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  let refused: Vec<&&str> =
    MACH_DENY.iter().filter(|n| !mach.contains(n)).collect();
  p.push_str("(deny mach-lookup");
  for name in refused {
    p.push_str(&format!(" (global-name \"{name}\")"));
  }
  p.push_str(")\n");
  p.push_str("(deny lsopen)\n(deny appleevent-send)\n(deny job-creation)\n");
  // the network
  let resolver = "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n";
  match &plan.net {
    NetPlan::Denied => p.push_str("(deny network*)\n"),
    NetPlan::Open => {
      p.push_str("(allow network*)\n");
      p.push_str("(deny network-outbound (remote unix-socket))\n");
      p.push_str(resolver);
    }
    NetPlan::Ports { connect, bind } => {
      p.push_str("(deny network*)\n");
      match connect {
        Ports::Any => p.push_str("(allow network-outbound (remote ip))\n"),
        Ports::List { ports } => {
          for port in ports {
            p.push_str(&format!(
              "(allow network-outbound (remote ip \"*:{port}\"))\n"
            ));
          }
        }
      }
      match bind {
        Ports::Any => {
          p.push_str("(allow network-bind (local ip))\n");
          p.push_str("(allow network-inbound (local ip))\n");
        }
        Ports::List { ports } => {
          for port in ports {
            p.push_str(&format!(
              "(allow network-bind (local ip \"*:{port}\"))\n"
            ));
            p.push_str(&format!(
              "(allow network-inbound (local ip \"*:{port}\"))\n"
            ));
          }
        }
      }
      // names resolve through the system resolver (a documented residual)
      p.push_str(resolver);
    }
  }
  match &plan.unix {
    SbUnix::Denied => {}
    SbUnix::Open => {
      p.push_str("(allow network-outbound (remote unix-socket))\n");
    }
    SbUnix::Filters { filters } => {
      for f in filters {
        let path = match f {
          SbFilter::Literal(path) => {
            format!("(path-literal {})", quote_str(path))
          }
          SbFilter::Subpath(path) => format!(
            "(path-regex #\"^{}(/|$)\")",
            regex_escape(path).replace('"', "\\\"")
          ),
          SbFilter::Regex(r) => {
            format!("(path-regex #\"{}\")", r.replace('"', "\\\""))
          }
        };
        p.push_str(&format!(
          "(allow network-outbound (remote unix-socket {path}))\n"
        ));
      }
    }
  }
  p
}

/// The profile of a deny ceiling (D1 / D2, `plan.deny_only`): everything is
/// allowed by default, as with no sandbox, and only the read and write
/// denies are refused (the last matching rule wins), then the re-allows the
/// denies must not swallow. Nothing else changes: no terminal, mach,
/// LaunchServices, process or network rule, since a prompt may allow any of
/// it; children inherit the profile. The marker pair says a kernel sandbox
/// of this runtime holds the process, so a `deno` it starts finds the
/// handed-down policy instead of applying a second profile (macOS can't
/// stack one).
pub fn deny_only_profile(plan: &MacosPlan) -> String {
  let mut p = String::from("(version 1)\n(allow default)\n");
  if !plan.deny_read.is_empty() {
    p.push_str(&format!(
      "(deny file-read* {})\n",
      filters_text(&plan.deny_read)
    ));
  }
  if !plan.deny_write.is_empty() {
    p.push_str(&format!(
      "(deny file-write* {})\n",
      filters_text(&plan.deny_write)
    ));
  }
  if !plan.reallow_read.is_empty() {
    p.push_str(&format!(
      "(allow file-read* {})\n",
      filters_text(&plan.reallow_read)
    ));
  }
  if !plan.reallow_write.is_empty() {
    p.push_str(&format!(
      "(allow file-write* {})\n",
      filters_text(&plan.reallow_write)
    ));
  }
  p.push_str(&format!(
    "(deny sysctl-read (with no-report) (sysctl-name {}))\n",
    quote_str(MARKER_SYSCTL_DENIED)
  ));
  p
}

/// Where a desktop app's process may map code from besides the plan's load
/// scope and [`SYSTEM_CODE`]: the system's plug-ins (input methods, audio
/// units, Quick Look), which only an administrator installs.
const DESKTOP_SYSTEM_CODE: &[&str] = &["/Library"];

/// The Unix sockets a desktop app's process reaches whatever the grant: the
/// system log.
const DESKTOP_UNIX_SOCKETS: &[&str] = &["/private/var/run/syslog"];

/// The Seatbelt profile of a packaged desktop app whose web engine is the
/// OS web view (WKWebView), applied to the app's own process (DENEXT.md
/// item 488). Unlike [`cli_profile`] it allows by default: the window
/// server, the GPU, the pasteboard, the keychain and AppKit's services stay
/// reachable (a deny-default profile for a windowed process would have to
/// list them all). What it refuses is what the grant doesn't cover, for
/// native code and child processes as for Deno's own checks:
///
/// - file contents outside the read scope and writes outside the write
///   scope (a scope that is `All` refuses nothing), then the exact denies,
///   then the re-allows (the last matching rule wins); file metadata stays
///   readable everywhere;
/// - mapping code from outside the load scope;
/// - running programs without a run grant;
/// - IP sockets per the plan (with no network grant: the loopback
///   interface only, where the app's own servers and the page's WebSocket
///   relay listen), and Unix sockets but the plan's;
/// - LaunchServices opens, AppleEvents and launchd job submission, the
///   three ways a process asks the system to start a program outside its
///   sandbox. Opening a link in the user's browser goes through the helper
///   started before the profile applies (`crate::outside`).
///
/// Not narrowed: mach services, `sysctl` reads and signals (AppKit and
/// WebKit use them freely), which the guarantee matrix lists. WebKit's web
/// content, networking and GPU processes are XPC services launchd starts:
/// they are outside this profile and keep WebKit's own.
pub fn desktop_profile(plan: &MacosPlan) -> String {
  let mut p = String::from("(version 1)\n(allow default)\n");
  // files: metadata everywhere, contents by scope
  p.push_str("(allow file-read-metadata)\n");
  if !matches!(plan.read, SbScope::All) {
    p.push_str("(deny file-read*)\n");
    p.push_str("(allow file-read-metadata)\n");
    allow_scope(&mut p, "file-read*", &plan.read);
  }
  if !matches!(plan.write, SbScope::All) {
    p.push_str("(deny file-write*)\n");
    allow_scope(&mut p, "file-write*", &plan.write);
  }
  if !plan.deny_read.is_empty() {
    p.push_str(&format!(
      "(deny file-read* {})\n",
      filters_text(&plan.deny_read)
    ));
  }
  if !plan.deny_write.is_empty() {
    p.push_str(&format!(
      "(deny file-write* {})\n",
      filters_text(&plan.deny_write)
    ));
  }
  if !plan.reallow_read.is_empty() {
    p.push_str(&format!(
      "(allow file-read* {})\n",
      filters_text(&plan.reallow_read)
    ));
  }
  if !plan.reallow_write.is_empty() {
    p.push_str(&format!(
      "(allow file-write* {})\n",
      filters_text(&plan.reallow_write)
    ));
  }
  // the terminals, as in the cli profile: only the plan's own, never
  // ioctl-able (an app started from a shell must not type into it)
  for op in ["file-read*", "file-write*"] {
    p.push_str(&format!("(deny {op} {TTY_FILTERS})\n"));
  }
  if !plan.ttys.is_empty() {
    let literals = plan
      .ttys
      .iter()
      .map(|t| format!("(literal {})", quote_str(t)))
      .collect::<Vec<_>>()
      .join(" ");
    for op in ["file-read*", "file-write*"] {
      p.push_str(&format!("(allow {op} {literals})\n"));
    }
  }
  p.push_str(&format!("(deny file-ioctl {TTY_FILTERS})\n"));
  // code
  if let SbScope::Filters { filters } = &plan.map_exec {
    let mut map: Vec<SbFilter> = SYSTEM_CODE
      .iter()
      .chain(DESKTOP_SYSTEM_CODE)
      .map(|s| SbFilter::Subpath(s.to_string()))
      .collect();
    map.extend(filters.iter().cloned());
    p.push_str("(deny file-map-executable)\n");
    p.push_str(&format!(
      "(allow file-map-executable {})\n",
      filters_text(&map)
    ));
  }
  // processes
  if !plan.exec || !plan.exec_only.is_empty() {
    p.push_str("(deny process-exec)\n");
  }
  if plan.exec && !plan.exec_only.is_empty() {
    let only: Vec<SbFilter> = plan
      .exec_only
      .iter()
      .map(|p| SbFilter::Literal(p.clone()))
      .collect();
    p.push_str(&format!("(allow process-exec {})\n", filters_text(&only)));
  }
  // the marker pair, so a child can tell this runtime's sandbox holds it
  p.push_str(&format!(
    "(deny sysctl-read (with no-report) (sysctl-name {}))\n",
    quote_str(MARKER_SYSCTL_DENIED)
  ));
  // the ways out through the system
  p.push_str("(deny lsopen)\n(deny appleevent-send)\n(deny job-creation)\n");
  // the network
  let ip_denied = "(deny network-outbound (remote ip))\n(deny network-bind (local ip))\n(deny network-inbound (local ip))\n";
  let mut resolver = true;
  match &plan.net {
    NetPlan::Open => {}
    NetPlan::Denied => {
      // no network grant: the loopback interface only
      resolver = false;
      p.push_str(ip_denied);
      p.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
      p.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
      p.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
    }
    NetPlan::Ports { connect, bind } => {
      p.push_str(ip_denied);
      match connect {
        Ports::Any => p.push_str("(allow network-outbound (remote ip))\n"),
        Ports::List { ports } => {
          for port in ports {
            p.push_str(&format!(
              "(allow network-outbound (remote ip \"*:{port}\"))\n"
            ));
          }
        }
      }
      match bind {
        Ports::Any => {
          p.push_str("(allow network-bind (local ip))\n");
          p.push_str("(allow network-inbound (local ip))\n");
        }
        Ports::List { ports } => {
          for port in ports {
            p.push_str(&format!(
              "(allow network-bind (local ip \"*:{port}\"))\n"
            ));
            p.push_str(&format!(
              "(allow network-inbound (local ip \"*:{port}\"))\n"
            ));
          }
        }
      }
    }
  }
  // Unix sockets reach other daemons (a Docker socket runs anything): the
  // plan's only, the system log and, with network, the resolver's
  if !matches!(plan.unix, SbUnix::Open) {
    p.push_str("(deny network-outbound (remote unix-socket))\n");
    let mut sockets: Vec<&str> = DESKTOP_UNIX_SOCKETS.to_vec();
    if resolver {
      sockets.push("/private/var/run/mDNSResponder");
    }
    for path in sockets {
      p.push_str(&format!(
        "(allow network-outbound (remote unix-socket (path-literal {})))\n",
        quote_str(path)
      ));
    }
    if let SbUnix::Filters { filters } = &plan.unix {
      for f in filters {
        p.push_str(&format!(
          "(allow network-outbound (remote unix-socket {}))\n",
          unix_filter_text(f)
        ));
      }
    }
  }
  p
}

/// A Unix-socket path filter (`path-literal` / `path-regex`).
fn unix_filter_text(f: &SbFilter) -> String {
  match f {
    SbFilter::Literal(path) => format!("(path-literal {})", quote_str(path)),
    SbFilter::Subpath(path) => format!(
      "(path-regex #\"^{}(/|$)\")",
      regex_escape(path).replace('"', "\\\"")
    ),
    SbFilter::Regex(r) => {
      format!("(path-regex #\"{}\")", r.replace('"', "\\\""))
    }
  }
}

/// The reporting profile of a warn run (`--kernel-sandbox=warn`): it allows
/// everything, and reports to the unified log exactly what [`cli_profile`]
/// would refuse for the same plan. Each rule of the enforce profile keeps
/// its place (the last matching rule wins): `(deny default)` and every
/// `(deny …)` become `(allow (with report) (with message "<tag>") …)`, the
/// allows stay silent. The sandbox marker is left out (a warn run's children
/// are not confined, and must not read as held by this runtime's sandbox).
///
/// `tag` marks this run's lines in the log (see
/// [`crate::would_deny::parse_seatbelt_report`]). `quiet` paths are read and
/// written silently: what the runtime itself writes in a warn run that an
/// enforced run does elsewhere or before it confines itself (the shared
/// DENO_DIR caches, where an enforced run uses its overlay).
pub fn cli_report_profile(
  plan: &MacosPlan,
  tag: &str,
  quiet: &[String],
) -> Result<String, String> {
  if !crate::would_deny::valid_tag(tag) {
    return Err(format!("{tag:?} can't tag a profile's reports"));
  }
  let report = format!("(allow (with report) (with message \"{tag}\") ");
  let marker = format!("(sysctl-name {})", quote_str(MARKER_SYSCTL_DENIED));
  let mut out = String::new();
  for line in cli_profile(plan).lines() {
    if line == "(deny default)" {
      out.push_str(&report);
      out.push_str("default)\n");
      if !quiet.is_empty() {
        let filters: Vec<SbFilter> =
          quiet.iter().map(|p| SbFilter::Subpath(p.clone())).collect();
        out.push_str(&format!(
          "(allow file-read* file-write* {})\n",
          filters_text(&filters)
        ));
      }
    } else if let Some(rest) = line.strip_prefix("(deny ") {
      if rest.contains(&marker) {
        continue;
      }
      out.push_str(&report);
      out.push_str(rest);
      out.push('\n');
    } else {
      out.push_str(line);
      out.push('\n');
    }
  }
  Ok(out)
}

/// `profile` letting the process run exactly the program at `exe` (the
/// `sandbox-exec` fallback execs the runtime under the profile); appended,
/// so it wins over an earlier `(deny process-exec)`.
pub fn with_self_exec(profile: &str, exe: &str) -> String {
  format!(
    "{profile}(allow process-exec (literal {}))\n",
    quote_str(exe)
  )
}

/// `text` with every regular-expression metacharacter escaped.
pub fn regex_escape(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for c in text.chars() {
    if "\\.+*?()|[]{}^$".contains(c) {
      out.push('\\');
    }
    out.push(c);
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn profile_quotes_paths() {
    let p = profile(
      &[PathBuf::from("/a \"b\"/c\\d")],
      &[PathBuf::from("/home/.ssh")],
      OsNet::Denied,
    );
    assert!(p.contains("(subpath \"/a \\\"b\\\"/c\\\\d\")"), "{p}");
    assert!(p.contains("(subpath \"/home/.ssh\"))"), "{p}");
    assert!(p.contains("(deny network*)"));
    assert!(p.contains("com.apple.dnssd.service"));
    let p = profile(&[], &[], OsNet::Open);
    assert!(p.contains("(allow network*)"));
    assert!(p.contains("mDNSResponder"));
    assert!(p.contains(
      "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))"
    ));
    // a deny of a read path list that is empty still names the terminals
    assert!(p.contains("(deny file-read* (literal \"/dev/tty\")"));
    assert!(!p.contains("(allow file-link"));
  }

  // DENEXT.md item 124: deny by default, the services a script could leave
  // the sandbox through refused by name, no terminals, hard links only
  // from the roots
  #[test]
  fn profile_denies_by_default() {
    let p = profile(
      &[PathBuf::from("/nm/pkg"), PathBuf::from("/nm/.deno/.jail/x")],
      &[PathBuf::from("/home/.ssh")],
      OsNet::Denied,
    );
    assert!(p.starts_with("(version 1)\n(deny default)\n"), "{p}");
    assert!(!p.contains("(allow default)"));
    for rule in [
      "(allow file-read*)",
      "(deny file-read* (literal \"/dev/tty\") (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/pty\") (subpath \"/home/.ssh\"))",
      "(allow file-write* (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\") (literal \"/dev/null\")",
      "(allow file-link (subpath \"/nm/pkg\") (subpath \"/nm/.deno/.jail/x\"))",
      "(deny file-ioctl (literal \"/dev/tty\")",
      "(allow process-fork)",
      "(allow process-exec)",
      "(allow signal (target same-sandbox))",
      "(allow process-info* (target same-sandbox))",
      "(deny sysctl-read (sysctl-name-prefix \"kern.proc\"))",
      "(deny lsopen)",
      "(deny appleevent-send)",
      "(deny job-creation)",
      "(global-name \"com.apple.SecurityServer\")",
      "(global-name \"com.apple.coreservices.launchservicesd\")",
      "(global-name \"com.apple.pasteboard.1\")",
      "(global-name \"com.apple.dnssd.service\")",
      "(global-name \"com.apple.system.opendirectoryd.libinfo\")",
    ] {
      assert!(p.contains(rule), "missing {rule}\n{p}");
    }
    // the terminals are writable nowhere
    assert!(!p.contains("(literal \"/dev/tty\") (literal"), "{p}");
    let write = p
      .lines()
      .find(|l| l.starts_with("(allow file-write*"))
      .unwrap();
    assert!(!write.contains("tty"), "{write}");
    // the deny of the services comes after the allow list, so it wins
    let allow = p.find("(allow mach-lookup").unwrap();
    let deny = p.find("(deny mach-lookup").unwrap();
    assert!(allow < deny);
  }

  // DENEXT.md item 437: with a home policy, no file contents nor extended
  // attributes under the home directories but the allowed paths, refused as
  // missing, and the read deny after both
  #[test]
  fn profile_home_read_policy() {
    let home = HomeReadPolicy {
      homes: vec![PathBuf::from("/Users/u"), PathBuf::from("/private/tmp/h")],
      allowed: vec![
        PathBuf::from("/Users/u/proj"),
        PathBuf::from("/Users/u/.nvm"),
      ],
    };
    let write = [PathBuf::from("/Users/u/proj/node_modules/pkg")];
    let deny = [
      PathBuf::from("/Users/u/.ssh"),
      PathBuf::from("/Users/u/proj/.env"),
    ];
    let p = profile_with_home_read(
      &write,
      &deny,
      Some(&home),
      OsNet::ProxyOnly(4321),
    );
    let deny_home = p
      .find(
        "(deny file-read-data file-read-xattr (with errno ENOENT) (subpath \"/Users/u\") (subpath \"/private/tmp/h\"))",
      )
      .expect(&p);
    let allow = p
      .find("(allow file-read-data file-read-xattr (subpath \"/Users/u/proj\") (subpath \"/Users/u/.nvm\"))")
      .expect(&p);
    let deny_secrets = p
      .find("(deny file-read-data file-read-xattr (literal \"/dev/tty\")")
      .expect(&p);
    assert!(p[deny_secrets..].contains("(subpath \"/Users/u/proj/.env\"))"));
    let read_all = p.find("(allow file-read*)").unwrap();
    assert!(
      read_all < deny_home && deny_home < allow && allow < deny_secrets,
      "{p}"
    );
    // no rule for one operation alone: contents and attributes together
    for line in p.lines().filter(|l| l.contains("file-read-data")) {
      assert!(line.contains("file-read-data file-read-xattr"), "{line}");
    }
    // without the policy, no home rule, and the text of `profile`
    let p = profile_with_home_read(&write, &deny, None, OsNet::Open);
    assert!(!p.contains("file-read-data"), "{p}");
    assert!(!p.contains("errno"), "{p}");
    assert_eq!(p, profile(&write, &deny, OsNet::Open));
    // an empty allow list adds no empty rule
    let p = profile_with_home_read(
      &[],
      &[],
      Some(&HomeReadPolicy {
        homes: vec![PathBuf::from("/Users/u")],
        allowed: vec![],
      }),
      OsNet::Open,
    );
    assert!(!p.contains("(allow file-read-data"), "{p}");
  }

  // the home policy as Seatbelt runs it: a file outside the allowed paths is
  // refused as missing and its extended attributes can't be read, while the
  // allowed path reads and a denied file inside it is refused
  #[cfg(target_os = "macos")]
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  #[test]
  fn profile_home_read_policy_runs() {
    use std::os::unix::ffi::OsStrExt;
    use std::process::Command;

    // nothing to run where sandbox-exec can't (already sandboxed)
    if crate::macos::available().is_err() {
      return;
    }
    let root = std::env::temp_dir()
      .join(format!("deno_sandbox_home_read_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home/proj")).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let home = root.join("home");
    let proj = home.join("proj");
    let set_xattr = |path: &Path| {
      let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
      // SAFETY: valid C strings and a buffer of the given length
      let res = unsafe {
        libc::setxattr(
          c_path.as_ptr(),
          c"com.denext.test".as_ptr(),
          b"hello".as_ptr() as *const libc::c_void,
          5,
          0,
          0,
        )
      };
      assert_eq!(res, 0, "{}", std::io::Error::last_os_error());
    };
    for file in [home.join("notes"), proj.join("a"), proj.join("secret")] {
      std::fs::write(&file, "x").unwrap();
      set_xattr(&file);
    }
    let profile = profile_with_home_read(
      &[],
      &[proj.join("secret")],
      Some(&HomeReadPolicy {
        homes: vec![home.clone()],
        allowed: vec![proj.clone()],
      }),
      OsNet::Denied,
    );
    let run = |program: &str, arg: &str, path: &Path| {
      let mut cmd = Command::new("/usr/bin/sandbox-exec");
      cmd.args(["-p", &profile, program]);
      if !arg.is_empty() {
        cmd.arg(arg);
      }
      let out = cmd.arg(path).output().unwrap();
      (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
      )
    };
    let (ok, _, err) = run("/bin/cat", "", &home.join("notes"));
    assert!(!ok && err.contains("No such file or directory"), "{err}");
    let (ok, out, _) = run("/bin/cat", "", &proj.join("a"));
    assert!(ok && out == "x", "{out}");
    let (ok, _, err) = run("/bin/cat", "", &proj.join("secret"));
    assert!(!ok && err.contains("Operation not permitted"), "{err}");
    // `ls -l@` prints each attribute's size, -1 when it can't be read
    let (_, out, _) = run("/bin/ls", "-l@", &home.join("notes"));
    assert!(out.contains("com.denext.test\t-1"), "{out}");
    let (_, out, _) = run("/bin/ls", "-l@", &proj.join("a"));
    assert!(out.contains("com.denext.test\t5"), "{out}");
    std::fs::remove_dir_all(&root).unwrap();
  }

  // DENEXT.md item 111: only the proxy's port, no DNS
  #[test]
  fn profile_proxy_only() {
    let p = profile(&[], &[], OsNet::ProxyOnly(4321));
    let deny = p.find("(deny network*)").unwrap();
    let allow = p
      .find("(allow network-outbound (remote ip \"localhost:4321\"))")
      .unwrap();
    // later rules win in Seatbelt
    assert!(deny < allow, "{p}");
    assert!(!p.contains("mDNSResponder"), "{p}");
    assert!(!p.contains("(allow network*)"));
    assert!(!p.contains("network-inbound"));
    assert!(
      p.contains("(deny mach-lookup")
        && p.contains("(global-name \"com.apple.dnssd.service\")")
    );
    assert!(!p.contains(
      "(allow mach-lookup (global-name \"com.apple.dnssd.service\"))"
    ));
  }

  // DENEXT.md item 488: the desktop profile allows by default and refuses
  // what the grant doesn't cover, in the cli profile's order (allows, then
  // denies, then re-allows), with the three ways out through the system
  // always refused
  #[test]
  fn desktop_profile_allows_by_default_and_denies_by_scope() {
    let p = desktop_profile(&cli_plan());
    assert!(p.starts_with("(version 1)\n(allow default)\n"), "{p}");
    assert!(!p.contains("(deny default)"));
    let at = |rule: &str| {
      p.find(rule)
        .unwrap_or_else(|| panic!("missing {rule}\n{p}"))
    };
    // contents by scope, metadata everywhere
    assert!(
      at("(deny file-read*)\n") < at("(allow file-read* (subpath \"/w\"))")
    );
    assert!(
      at("(allow file-read* (subpath \"/w\"))")
        < at("(deny file-read* (subpath \"/w/.env\"))")
    );
    assert!(
      at("(deny file-read* (subpath \"/w/.env\"))")
        < at("(allow file-read* (literal \"/w/.env/deno.json\"))")
    );
    assert!(
      at("(deny file-write*)\n")
        < at("(allow file-write* (subpath \"/w/out\"))")
    );
    assert!(
      at("(allow file-write* (subpath \"/w/out\"))")
        < at("(deny file-write* (literal \"/w/out/keep\"))")
    );
    at("(allow file-read-metadata)");
    // code from the load scope and the system only
    assert!(
      at("(deny file-map-executable)\n")
        < at("(allow file-map-executable (subpath \"/usr/lib\")")
    );
    assert!(p.contains("(subpath \"/Library\") (literal \"/w/lib.dylib\"))"));
    // no run grant: nothing is executed
    at("(deny process-exec)\n");
    assert!(!p.contains("(allow process-exec"));
    // the ways out
    for rule in [
      "(deny lsopen)",
      "(deny appleevent-send)",
      "(deny job-creation)",
      "(deny file-ioctl (literal \"/dev/tty\")",
      "(deny sysctl-read (with no-report) (sysctl-name \"kern.singleuser\"))",
    ] {
      at(rule);
    }
    // no network grant: the loopback interface only, no resolver
    assert!(
      at("(deny network-outbound (remote ip))")
        < at("(allow network-outbound (remote ip \"localhost:*\"))")
    );
    at("(allow network-bind (local ip \"localhost:*\"))");
    at("(allow network-inbound (local ip \"localhost:*\"))");
    assert!(!p.contains("mDNSResponder"));
    // Unix sockets: the system log's only
    assert!(
      at("(deny network-outbound (remote unix-socket))\n")
        < at("(path-literal \"/private/var/run/syslog\")")
    );
    // mach services, signals and the plan's sysctl narrowing are not in it
    assert!(!p.contains("mach-lookup"));
    assert!(!p.contains("(target same-sandbox)"));
    assert!(!p.contains("kern.proc"));
  }

  #[test]
  fn desktop_profile_with_unscoped_grants_refuses_only_the_ways_out() {
    let mut plan = cli_plan();
    plan.read = SbScope::All;
    plan.write = SbScope::All;
    plan.deny_read.clear();
    plan.deny_write.clear();
    plan.reallow_read.clear();
    plan.map_exec = SbScope::All;
    plan.exec = true;
    plan.net = NetPlan::Open;
    plan.unix = SbUnix::Open;
    let p = desktop_profile(&plan);
    assert!(!p.contains("(deny file-read*)\n"), "{p}");
    assert!(!p.contains("(deny file-write*)\n"), "{p}");
    assert!(!p.contains("file-map-executable"), "{p}");
    assert!(!p.contains("process-exec"), "{p}");
    assert!(!p.contains("network"), "{p}");
    for rule in [
      "(deny lsopen)",
      "(deny appleevent-send)",
      "(deny job-creation)",
    ] {
      assert!(p.contains(rule), "{p}");
    }
    // a strict exec scope: those files only
    plan.exec_only = vec!["/usr/bin/git".into()];
    let p = desktop_profile(&plan);
    let deny = p.find("(deny process-exec)").unwrap();
    let allow = p
      .find("(allow process-exec (literal \"/usr/bin/git\"))")
      .unwrap();
    assert!(deny < allow, "{p}");
    // ports, and the plan's own sockets
    plan.net = NetPlan::Ports {
      connect: Ports::List { ports: vec![443] },
      bind: Ports::List { ports: vec![] },
    };
    plan.unix = SbUnix::Filters {
      filters: vec![SbFilter::Literal("/w/s.sock".into())],
    };
    let p = desktop_profile(&plan);
    assert!(p.contains("(allow network-outbound (remote ip \"*:443\"))"));
    assert!(!p.contains("(allow network-bind"), "{p}");
    assert!(p.contains("mDNSResponder"));
    assert!(p.contains(
      "(allow network-outbound (remote unix-socket (path-literal \"/w/s.sock\")))"
    ));
  }

  fn cli_plan() -> MacosPlan {
    MacosPlan {
      read: SbScope::Filters {
        filters: vec![SbFilter::Subpath("/w".into())],
      },
      write: SbScope::Filters {
        filters: vec![SbFilter::Subpath("/w/out".into())],
      },
      deny_read: vec![SbFilter::Subpath("/w/.env".into())],
      deny_write: vec![SbFilter::Literal("/w/out/keep".into())],
      reallow_read: vec![SbFilter::Literal("/w/.env/deno.json".into())],
      reallow_write: vec![],
      map_exec: SbScope::Filters {
        filters: vec![SbFilter::Literal("/w/lib.dylib".into())],
      },
      exec: false,
      exec_only: vec![],
      net: NetPlan::Denied,
      unix: SbUnix::Denied,
      denied_sysctls: vec!["kern.proc*".into(), "vm.loadavg".into()],
      system_ca: false,
      ttys: vec![],
      deny_only: false,
    }
  }

  /// A deny ceiling (D1 / D2): allow by default, the denies, the
  /// re-allows, the marker, and nothing else (no terminal, mach, process or
  /// network rule).
  #[test]
  fn deny_only_profile_refuses_only_the_denies() {
    let plan = MacosPlan {
      read: SbScope::All,
      write: SbScope::All,
      map_exec: SbScope::All,
      exec: true,
      net: NetPlan::Open,
      unix: SbUnix::Open,
      denied_sysctls: vec![],
      deny_only: true,
      ..cli_plan()
    };
    let p = cli_profile(&plan);
    assert_eq!(p, deny_only_profile(&plan));
    assert_eq!(
      p,
      concat!(
        "(version 1)\n(allow default)\n",
        "(deny file-read* (subpath \"/w/.env\"))\n",
        "(deny file-write* (literal \"/w/out/keep\"))\n",
        "(allow file-read* (literal \"/w/.env/deno.json\"))\n",
        "(deny sysctl-read (with no-report) (sysctl-name \"kern.singleuser\"))\n",
      )
    );
    for absent in [
      "(deny default)",
      "network",
      "mach-lookup",
      "process-exec",
      "lsopen",
      "file-ioctl",
      "/dev/tty",
    ] {
      assert!(!p.contains(absent), "{absent} in\n{p}");
    }
    // warn's reporting profile of it reports the denies only
    let r = cli_report_profile(&plan, "t1", &[]).unwrap();
    assert!(r.contains("(allow default)"), "{r}");
    assert!(
      r.contains("(allow (with report) (with message \"t1\") file-read* (subpath \"/w/.env\"))"),
      "{r}"
    );
    assert!(!r.contains("kern.singleuser"), "{r}");
  }

  // deny by default, allows, then denies, then re-allows (the
  // last matching rule wins); no LaunchServices, AppleEvents or launchd
  // denext (DENEXT.md item 449): a strict exec scope names its files
  #[test]
  fn cli_profile_strict_exec() {
    let mut plan = cli_plan();
    plan.exec = true;
    plan.exec_only = vec!["/bin/sh".into(), "/usr/bin/git".into()];
    let p = cli_profile(&plan);
    assert!(!p.contains("(allow process-exec)\n"), "{p}");
    let deny = p.find("(deny process-exec)\n").unwrap();
    let allow = p
      .find(
        "(allow process-exec (literal \"/bin/sh\") (literal \"/usr/bin/git\"))",
      )
      .unwrap();
    assert!(deny < allow, "{p}");
    // without a list: any program
    plan.exec_only.clear();
    assert!(cli_profile(&plan).contains("(allow process-exec)\n"));
  }

  // the marker pair: every sysctl readable but the denied one
  #[test]
  fn cli_profile_marker() {
    let p = cli_profile(&cli_plan());
    let allow = p.find("(allow sysctl-read)\n").unwrap();
    let deny = p
      .find(
        "(deny sysctl-read (with no-report) (sysctl-name \"kern.singleuser\"))\n",
      )
      .unwrap();
    assert!(allow < deny, "{p}");
    assert!(!p.contains(MARKER_SYSCTL_ALLOWED), "{p}");
  }

  // the reporting profile of a warn run: every rule in its place, each
  // refusal an allow that reports under the run's tag, nothing refused
  #[test]
  fn cli_report_profile_reports_what_enforce_refuses() {
    let plan = cli_plan();
    let enforce = cli_profile(&plan);
    let p = cli_report_profile(&plan, "denext-ks:t1", &["/cache".to_string()])
      .unwrap();
    let report = "(allow (with report) (with message \"denext-ks:t1\") ";
    assert!(
      p.starts_with(&format!(
        "(version 1)\n{report}default)\n(allow file-read* file-write* (subpath \"/cache\"))\n(allow file-read-metadata)\n"
      )),
      "{p}"
    );
    assert!(!p.contains("(deny "), "{p}");
    // the marker is the enforce profile's alone
    assert!(!p.contains(MARKER_SYSCTL_DENIED), "{p}");
    // every other rule of the enforce profile is there, in its order: an
    // allow as it was, a deny as a reporting allow
    let mut at = 0;
    for line in enforce.lines().skip(2) {
      if line.contains(MARKER_SYSCTL_DENIED) {
        continue;
      }
      let want = match line.strip_prefix("(deny ") {
        Some(rest) => format!("{report}{rest}\n"),
        None => format!("{line}\n"),
      };
      let found = p[at..]
        .find(&want)
        .unwrap_or_else(|| panic!("missing or out of order: {want}\n{p}"));
      at += found + want.len();
    }
    assert_eq!(at, p.len(), "{p}");
    // no quiet paths: no extra rule
    let bare = cli_report_profile(&plan, "t", &[]).unwrap();
    assert!(!bare.contains("file-read* file-write*"), "{bare}");
    // a tag that could end the rule early is refused
    for tag in ["", "a\") (allow default", "a b"] {
      assert!(cli_report_profile(&plan, tag, &[]).is_err(), "{tag:?}");
    }
  }

  #[test]
  fn cli_profile_order_and_refusals() {
    let p = cli_profile(&cli_plan());
    assert!(p.starts_with("(version 1)\n(deny default)\n"), "{p}");
    assert!(!p.contains("(allow default)"));
    let pos = |rule: &str| {
      p.find(rule)
        .unwrap_or_else(|| panic!("missing {rule}\n{p}"))
    };
    let allow_read = pos("(allow file-read* (subpath \"/w\"))");
    let allow_write = pos("(allow file-write* (subpath \"/w/out\"))");
    let deny_read = pos("(deny file-read* (subpath \"/w/.env\"))");
    let deny_write = pos("(deny file-write* (literal \"/w/out/keep\"))");
    let reallow = pos("(allow file-read* (literal \"/w/.env/deno.json\"))");
    assert!(allow_read < deny_read && allow_write < deny_write);
    assert!(deny_read < reallow && deny_write < reallow);
    for rule in [
      "(allow file-read-metadata)",
      "(deny lsopen)",
      "(deny appleevent-send)",
      "(deny job-creation)",
      "(deny process-exec)",
      "(deny network*)",
      "(deny sysctl-read (sysctl-name-prefix \"kern.proc\") (sysctl-name \"vm.loadavg\"))",
      "(deny file-ioctl (literal \"/dev/tty\")",
      "(deny file-read* (literal \"/dev/tty\")",
      "(allow ipc-posix-shm*)",
      "(allow signal (target same-sandbox))",
    ] {
      pos(rule);
    }
    // code maps from the load scope and the system only
    let map = p
      .lines()
      .find(|l| l.starts_with("(allow file-map-executable"))
      .unwrap();
    assert!(map.contains("(literal \"/w/lib.dylib\")"), "{map}");
    assert!(map.contains("(subpath \"/usr/lib\")"), "{map}");
    assert!(!map.contains("\"/w\")"), "{map}");
    // no network: no resolver; no system trust store: no trustd
    assert!(!p.contains("mDNSResponder"));
    let mach_allow = p
      .lines()
      .find(|l| l.starts_with("(allow mach-lookup"))
      .unwrap();
    assert!(!mach_allow.contains("dnssd"), "{mach_allow}");
    assert!(!mach_allow.contains("trustd"), "{mach_allow}");
    assert!(mach_allow.contains("com.apple.FSEvents"), "{mach_allow}");
    // the services refused by name stay refused, after the allow list
    let allow = pos("(allow mach-lookup");
    let deny = pos("(deny mach-lookup");
    assert!(allow < deny);
    assert!(p[deny..].contains("com.apple.coreservices.launchservicesd"));
  }

  #[test]
  fn cli_profile_network_and_trust() {
    let mut plan = cli_plan();
    plan.exec = true;
    plan.system_ca = true;
    plan.ttys = vec!["/dev/tty".into(), "/dev/ttys004".into()];
    plan.net = NetPlan::Ports {
      connect: Ports::List {
        ports: vec![443, 5432],
      },
      bind: Ports::List { ports: vec![8000] },
    };
    plan.unix = SbUnix::Filters {
      filters: vec![
        SbFilter::Literal("/run/a.sock".into()),
        SbFilter::Subpath("/w/socks".into()),
      ],
    };
    let p = cli_profile(&plan);
    for rule in [
      "(allow process-exec)",
      "(allow network-outbound (remote ip \"*:443\"))",
      "(allow network-outbound (remote ip \"*:5432\"))",
      "(allow network-bind (local ip \"*:8000\"))",
      "(allow network-inbound (local ip \"*:8000\"))",
      "mDNSResponder",
      "(global-name \"com.apple.dnssd.service\")",
      "(global-name \"com.apple.trustd.agent\")",
      "(global-name \"com.apple.SecurityServer\")",
      "(allow network-outbound (remote unix-socket (path-literal \"/run/a.sock\")))",
      "(allow network-outbound (remote unix-socket (path-regex #\"^/w/socks(/|$)\")))",
      "(allow file-read* (literal \"/dev/tty\") (literal \"/dev/ttys004\"))",
      "(allow file-write* (literal \"/dev/tty\") (literal \"/dev/ttys004\"))",
    ] {
      assert!(p.contains(rule), "missing {rule}\n{p}");
    }
    // only the named terminals: every other one is refused before them
    let deny = p.find("(deny file-read* (literal \"/dev/tty\")").unwrap();
    let allow = p
      .find("(allow file-read* (literal \"/dev/tty\") (literal")
      .unwrap();
    assert!(deny < allow, "{p}");
    assert!(!p.contains("(allow file-read* (literal \"/dev/tty\") (regex"));
    // trust services allowed are not also refused by name
    let deny = &p[p.find("(deny mach-lookup").unwrap()..];
    let deny = deny.lines().next().unwrap();
    assert!(!deny.contains("trustd"), "{deny}");
    assert!(!p.contains("(allow network*)"));
    plan.net = NetPlan::Open;
    plan.unix = SbUnix::Denied;
    let p = cli_profile(&plan);
    let open = p.find("(allow network*)").unwrap();
    let no_unix = p
      .find("(deny network-outbound (remote unix-socket))")
      .unwrap();
    assert!(open < no_unix);
    assert_eq!(regex_escape("/a.b(c)"), "/a\\.b\\(c\\)");
    // the fallback's own exec, after the deny it overrides
    let p = with_self_exec(&cli_profile(&cli_plan()), "/opt/deno");
    let deny = p.find("(deny process-exec)").unwrap();
    let own = p
      .find("(allow process-exec (literal \"/opt/deno\"))")
      .unwrap();
    assert!(deny < own, "{p}");
  }
}
