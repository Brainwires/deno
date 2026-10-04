// Copyright 2018-2026 the Deno authors. MIT license.

//! Deep links and opened files of a `deno desktop` app.
//!
//! An app declares the custom URL schemes it handles in `deno.json`
//! (`desktop.app.deepLinks`, e.g. `["acme"]` for `acme://…` links). The OS
//! hands such a link, or a file opened with the app, to the app in one of two
//! shapes:
//!
//! - as a URL delivered to the running process (macOS: the `openURLs` Apple
//!   Event, which laufey forwards through `on_open_url`); a file arrives as a
//!   `file://` URL there. See [`classify_open_url`].
//! - as an argument of a new process (Windows, Linux, and a directly executed
//!   macOS binary), either at a cold start or forwarded from a second launch
//!   by laufey's single-instance lock. See [`parse_launch_args`].
//!
//! The rules live here so the CLI (which registers the schemes and bakes them
//! into the binary metadata) and `denort` (which reads them back, or from an
//! embedded `.deno-desktop/app.json`) agree.
//!
//! Everything classified here is untrusted input: any program running as the
//! same user can start the app with arbitrary arguments or open any URL with
//! it.

use std::path::Path;
use std::path::PathBuf;

use url::Url;

/// Schemes that may not be registered as deep links: registering these as an
/// app handler is almost never intended and would hijack normal browsing.
pub const RESERVED_DEEP_LINK_SCHEMES: &[&str] =
  &["http", "https", "file", "ftp", "ws", "wss"];

/// The host backend's own option that takes a value (`--runtime <path>`).
/// Its value is an existing file and must not be mistaken for an opened file.
const HOST_OPTIONS_WITH_VALUE: &[&str] = &["--runtime"];

/// Validate a deep-link URL scheme. Follows the RFC 3986 `scheme` grammar,
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`, and rejects
/// [`RESERVED_DEEP_LINK_SCHEMES`]. The error is the reason, without the
/// scheme.
pub fn validate_deep_link_scheme(scheme: &str) -> Result<(), &'static str> {
  match scheme.chars().next() {
    None => return Err("scheme is empty"),
    Some(c) if !c.is_ascii_alphabetic() => {
      return Err("scheme must start with an ASCII letter");
    }
    _ => {}
  }
  if !scheme
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
  {
    return Err("scheme may only contain letters, digits, '+', '-', and '.'");
  }
  if RESERVED_DEEP_LINK_SCHEMES.contains(&scheme) {
    return Err("scheme is reserved and cannot be used as a deep link");
  }
  Ok(())
}

/// Normalize configured deep-link schemes the way the CLI registers them:
/// trimmed, lower-cased, empty entries dropped, duplicates removed (first
/// occurrence kept), each validated with [`validate_deep_link_scheme`].
pub fn normalize_deep_link_schemes(
  schemes: &[String],
) -> Result<Vec<String>, String> {
  let mut out: Vec<String> = Vec::with_capacity(schemes.len());
  for scheme in schemes {
    let scheme = scheme.trim().to_ascii_lowercase();
    if scheme.is_empty() {
      continue;
    }
    validate_deep_link_scheme(&scheme).map_err(|reason| {
      format!("invalid deep-link scheme {scheme:?}: {reason}")
    })?;
    if !out.contains(&scheme) {
      out.push(scheme);
    }
  }
  Ok(out)
}

/// A URL the OS routed to the running app, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenedItem {
  /// Any non-`file:` URL, passed through as delivered.
  Url(String),
  /// A `file:` URL, decoded to a filesystem path.
  File(PathBuf),
}

/// Classify a URL delivered by the OS to the running app (macOS
/// `openURLs`): a `file:` URL that maps to a local path is a
/// [`OpenedItem::File`], a URL whose scheme (case-insensitive) is one of the
/// app's declared deep-link `schemes` (as normalized by
/// [`normalize_deep_link_schemes`]) is an [`OpenedItem::Url`] carrying the
/// string unchanged, and anything else is `None`: dropped.
///
/// The OS routes the schemes the bundle declares, but a bundle can declare
/// more than the app's deep links (`CFBundleURLTypes` edited after packaging,
/// another tool's entry), and `open -a <App> <url>` hands any URL to it, so
/// the declared list is checked here as on every other OS. A `file:` URL with
/// a remote host (a network share) has no local path and is dropped too.
pub fn classify_open_url(url: &str, schemes: &[String]) -> Option<OpenedItem> {
  let parsed = Url::parse(url).ok()?;
  if parsed.scheme() == "file" {
    return deno_path_util::url_to_file_path(&parsed)
      .ok()
      .filter(|path| !is_remote_path(path))
      .map(OpenedItem::File);
  }
  schemes
    .iter()
    .any(|s| s == parsed.scheme())
    .then(|| OpenedItem::Url(url.to_string()))
}

/// Whether `path` names a network location (`\\server\share`, `\\?\UNC\…`)
/// or a device namespace (`\\.\…`): checking whether it exists would make
/// Windows connect to that server and offer the user's credentials (NTLM), so
/// a launch argument naming one is never looked at.
fn is_remote_path(path: &Path) -> bool {
  #[cfg(windows)]
  {
    use std::path::Component;
    use std::path::Prefix;
    if let Some(Component::Prefix(prefix)) = path.components().next() {
      return !matches!(
        prefix.kind(),
        Prefix::Disk(_) | Prefix::VerbatimDisk(_)
      );
    }
    false
  }
  #[cfg(not(windows))]
  {
    let _ = path;
    false
  }
}

/// Deep links and files found in a process's arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchTargets {
  /// Arguments that are absolute URLs with a registered deep-link scheme, as
  /// given.
  pub urls: Vec<String>,
  /// Arguments that name an existing path (a plain path, relative ones
  /// resolved against the launch's working directory, or a `file:` URL),
  /// as absolute, lexically normalized paths.
  pub files: Vec<PathBuf>,
}

/// Find the deep links and files in `args` — a process's arguments after the
/// executable name.
///
/// - An absolute URL whose scheme (case-insensitive) is one of `schemes` (as
///   normalized by [`normalize_deep_link_schemes`]) is a URL.
/// - A `file:` URL whose local path exists is a file.
/// - Anything that names an existing path (`exists`) is a file. A relative
///   path is resolved against `cwd`; without an absolute `cwd` it is ignored.
/// - Everything else is ignored: an argument starting with `-` (a flag, which
///   also keeps option values like `--flag=/path` out), the value that
///   follows the host's `--runtime` option, and anything else.
/// - A `--` ends the options and marks an OS deep-link launch: the Windows
///   scheme registration runs `"<exe>" -- "%1"` (see
///   `scheme_handler::windows::command_line`). After it exactly one argument
///   is accepted, and only when it is a link with a declared scheme; a link
///   that closes the quotes around `%1` adds more arguments, and then
///   nothing after the `--` counts (no file the link named is opened).
/// - A network path (`\\server\share\x`, also as a `file:` URL with a
///   host) is never checked for existence or taken as a file: on Windows the
///   check alone connects to the server with the user's credentials.
pub fn parse_launch_args(
  args: &[String],
  cwd: Option<&Path>,
  schemes: &[String],
  exists: impl Fn(&Path) -> bool,
) -> LaunchTargets {
  let cwd = cwd.filter(|c| c.is_absolute());
  let mut targets = LaunchTargets::default();
  let mut iter = args.iter();
  while let Some(arg) = iter.next() {
    if arg == "--" {
      // An OS deep-link launch: one declared-scheme link, or nothing.
      if let [link] = iter.as_slice()
        && let Ok(url) = Url::parse(link)
        && schemes.iter().any(|s| s == url.scheme())
      {
        targets.urls.push(link.clone());
      }
      break;
    }
    if HOST_OPTIONS_WITH_VALUE.contains(&arg.as_str()) {
      iter.next();
      continue;
    }
    if arg.starts_with('-') || arg.is_empty() {
      continue;
    }
    if let Ok(url) = Url::parse(arg) {
      if schemes.iter().any(|s| s == url.scheme()) {
        targets.urls.push(arg.clone());
        continue;
      }
      if url.scheme() == "file" {
        if let Ok(path) = deno_path_util::url_to_file_path(&url)
          && !is_remote_path(&path)
          && exists(&path)
        {
          targets.files.push(normalize(path));
        }
        continue;
      }
    }
    let path = Path::new(arg);
    let path = if path.is_absolute() {
      path.to_path_buf()
    } else if let Some(cwd) = cwd {
      cwd.join(path)
    } else {
      continue;
    };
    let path = normalize(path);
    if !is_remote_path(&path) && exists(&path) {
      targets.files.push(path);
    }
  }
  targets
}

fn normalize(path: PathBuf) -> PathBuf {
  deno_path_util::normalize_path(std::borrow::Cow::Owned(path)).into_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn scheme_validation() {
    for ok in ["acme", "acme+x", "a-b.c", "T3Code"] {
      assert_eq!(validate_deep_link_scheme(ok), Ok(()), "{ok}");
    }
    for bad in ["", "1abc", "ac me", "ac/me", "a:b", "é"] {
      assert!(validate_deep_link_scheme(bad).is_err(), "{bad:?}");
    }
    for reserved in RESERVED_DEEP_LINK_SCHEMES {
      assert!(validate_deep_link_scheme(reserved).is_err(), "{reserved}");
    }
    assert_eq!(
      normalize_deep_link_schemes(&strings(&[" Acme ", "", "acme", "t3"]))
        .unwrap(),
      strings(&["acme", "t3"])
    );
    let err =
      normalize_deep_link_schemes(&strings(&["acme", "HTTP"])).unwrap_err();
    assert!(err.contains("\"http\""), "{err}");
    assert!(err.contains("reserved"), "{err}");
  }

  #[test]
  fn open_urls_split_files_from_links() {
    let schemes = strings(&["acme"]);
    assert_eq!(
      classify_open_url("acme://open/doc/42?x=1", &schemes),
      Some(OpenedItem::Url("acme://open/doc/42?x=1".to_string()))
    );
    assert_eq!(
      classify_open_url("ACME:upper", &schemes),
      Some(OpenedItem::Url("ACME:upper".to_string()))
    );
    // A scheme the app didn't declare (another entry in the bundle, `open
    // -a <App> <url>`) is dropped.
    for other in [
      "other:thing",
      "https://example.com/",
      "javascript:alert(1)",
      "acmex://y",
      "not a url",
      "",
    ] {
      assert_eq!(classify_open_url(other, &schemes), None, "{other:?}");
    }
    assert_eq!(classify_open_url("acme://x", &[]), None);
    #[cfg(unix)]
    {
      // Percent-encoded, as AppKit delivers it.
      assert_eq!(
        classify_open_url("file:///Users/me/My%20Notes.txt", &[]),
        Some(OpenedItem::File(PathBuf::from("/Users/me/My Notes.txt")))
      );
      // A remote file URL has no local path.
      assert_eq!(
        classify_open_url("file://server/share/x.txt", &schemes),
        None
      );
    }
    #[cfg(windows)]
    {
      assert_eq!(
        classify_open_url("file:///C:/Users/me/My%20Notes.txt", &[]),
        Some(OpenedItem::File(PathBuf::from(
          "C:\\Users\\me\\My Notes.txt"
        )))
      );
      // A network share is never an opened file.
      assert_eq!(classify_open_url("file://server/share/x.txt", &[]), None);
    }
  }

  #[test]
  fn after_the_terminator_only_one_declared_link_counts() {
    let schemes = strings(&["acme"]);
    let all = |_: &Path| true;
    // The OS deep-link launch: `"<exe>" -- "%1"`.
    let targets =
      parse_launch_args(&strings(&["--", "acme://a?x=1"]), None, &schemes, all);
    assert_eq!(targets.urls, strings(&["acme://a?x=1"]));
    assert!(targets.files.is_empty());
    // A link that broke out of its quotes added arguments: nothing after
    // the `--` counts, not even the link, and no path it named is opened.
    for args in [
      &["--", "acme://a", "C:\\secret.txt"][..],
      &["--", "acme://a", "acme://b"],
      &["--", "acme://a", "--runtime", "acme://b"],
      &["--", "/etc/passwd"],
      &["--", "-notes.txt"],
      &["--", "other://a"],
      &["--", "--", "acme://a"],
      &["--"],
    ] {
      let targets =
        parse_launch_args(&strings(args), Some(Path::new("/")), &schemes, all);
      assert_eq!(targets, LaunchTargets::default(), "{args:?}");
    }
    // What came before the `--` (the host's own options) is unaffected.
    let targets = parse_launch_args(
      &strings(&["--runtime", "/x/lib.so", "--", "acme://a"]),
      None,
      &schemes,
      all,
    );
    assert_eq!(targets.urls, strings(&["acme://a"]));
    // Without the terminator `--runtime` swallows the next argument.
    let targets = parse_launch_args(
      &strings(&["acme://a", "--runtime", "acme://b"]),
      None,
      &schemes,
      |_| false,
    );
    assert_eq!(targets.urls, strings(&["acme://a"]));
  }

  #[test]
  fn network_paths_are_never_looked_at() {
    let looked = std::cell::RefCell::new(Vec::<PathBuf>::new());
    let exists = |p: &Path| {
      looked.borrow_mut().push(p.to_path_buf());
      true
    };
    let args = if cfg!(windows) {
      strings(&[
        "\\\\server\\share\\x.txt",
        "//server/share/x.txt",
        "\\\\?\\UNC\\server\\share\\x.txt",
        "\\\\.\\pipe\\x",
        "file://server/share/x.txt",
      ])
    } else {
      strings(&["file://server/share/x.txt"])
    };
    let targets = parse_launch_args(
      &args,
      Some(Path::new("/")),
      &strings(&["acme"]),
      exists,
    );
    assert!(targets.files.is_empty(), "{targets:?}");
    assert!(looked.borrow().is_empty(), "{:?}", looked.borrow());
  }

  #[cfg(unix)]
  #[test]
  fn launch_args_unix() {
    let existing = [
      "/home/me/notes.txt",
      "/home/me/work/a b.md",
      "/opt/app/libruntime.so",
      "/tmp",
    ];
    let exists = |p: &Path| existing.iter().any(|e| Path::new(e) == p);
    let schemes = strings(&["acme", "t3code"]);
    let args = strings(&[
      "--runtime",
      "/opt/app/libruntime.so",
      "acme://open/doc/42",
      "ACME://Upper",
      "T3Code:bare",
      "https://example.com",
      "other://x",
      "--flag",
      "--flag=/home/me/notes.txt",
      "-psn_0_12345",
      "",
      "notes.txt",
      "work/../work/a b.md",
      "file:///home/me/notes.txt",
      "file:///home/me/missing.txt",
      "/home/me/missing.txt",
      "/tmp/",
      "plain words",
    ]);
    let targets =
      parse_launch_args(&args, Some(Path::new("/home/me")), &schemes, exists);
    assert_eq!(
      targets.urls,
      strings(&["acme://open/doc/42", "ACME://Upper", "T3Code:bare"])
    );
    assert_eq!(
      targets.files,
      vec![
        PathBuf::from("/home/me/notes.txt"),
        PathBuf::from("/home/me/work/a b.md"),
        PathBuf::from("/home/me/notes.txt"),
        PathBuf::from("/tmp"),
      ]
    );

    // Without a usable working directory, relative paths are ignored.
    for cwd in [None, Some(Path::new("relative"))] {
      let targets =
        parse_launch_args(&strings(&["notes.txt"]), cwd, &schemes, exists);
      assert!(targets.files.is_empty(), "{cwd:?}");
    }
    // No registered schemes: no URLs at all.
    let targets = parse_launch_args(&strings(&["acme://x"]), None, &[], exists);
    assert_eq!(targets, LaunchTargets::default());
    // `--runtime` as the last argument has no value to skip.
    let targets = parse_launch_args(
      &strings(&["acme://x", "--runtime"]),
      None,
      &schemes,
      exists,
    );
    assert_eq!(targets.urls, strings(&["acme://x"]));
  }

  #[cfg(windows)]
  #[test]
  fn launch_args_windows() {
    let existing = ["C:\\Users\\me\\notes.txt", "D:\\data"];
    let exists = |p: &Path| existing.iter().any(|e| Path::new(e) == p);
    let schemes = strings(&["acme"]);
    let args = strings(&[
      "acme://open/doc/42",
      "C:\\Users\\me\\notes.txt",
      "notes.txt",
      "D:\\data\\",
      "file:///C:/Users/me/notes.txt",
      "C:\\missing.txt",
      "/flag",
    ]);
    let targets = parse_launch_args(
      &args,
      Some(Path::new("C:\\Users\\me")),
      &schemes,
      exists,
    );
    assert_eq!(targets.urls, strings(&["acme://open/doc/42"]));
    assert_eq!(
      targets.files,
      vec![
        PathBuf::from("C:\\Users\\me\\notes.txt"),
        PathBuf::from("C:\\Users\\me\\notes.txt"),
        PathBuf::from("D:\\data"),
        PathBuf::from("C:\\Users\\me\\notes.txt"),
      ]
    );
  }
}
