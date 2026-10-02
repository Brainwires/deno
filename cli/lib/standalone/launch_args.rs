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
/// `openURLs`): a `file:` URL that maps to a local path is a [`OpenedItem::File`],
/// anything else (including a `file:` URL with a remote host, which has no
/// local path) is an [`OpenedItem::Url`] carrying the string unchanged.
///
/// The scheme is not checked against the registered deep links: the OS only
/// routes schemes the bundle declares, and dropping a delivery would lose it.
/// Consumers still have to validate it.
pub fn classify_open_url(url: &str) -> OpenedItem {
  if let Ok(parsed) = Url::parse(url)
    && parsed.scheme() == "file"
    && let Ok(path) = deno_path_util::url_to_file_path(&parsed)
  {
    return OpenedItem::File(path);
  }
  OpenedItem::Url(url.to_string())
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
/// - A `--` ends the options: every argument after it is classified as a
///   positional one by the rules above, even one that starts with `-` or is
///   `--runtime`. The Windows scheme registration runs `"<exe>" -- "%1"`
///   (see `scheme_handler::windows::command_line`), so a link that closes
///   the quotes around `%1` can only add more positional arguments.
pub fn parse_launch_args(
  args: &[String],
  cwd: Option<&Path>,
  schemes: &[String],
  exists: impl Fn(&Path) -> bool,
) -> LaunchTargets {
  let cwd = cwd.filter(|c| c.is_absolute());
  let mut targets = LaunchTargets::default();
  let mut iter = args.iter();
  let mut options_ended = false;
  while let Some(arg) = iter.next() {
    if !options_ended {
      if arg == "--" {
        options_ended = true;
        continue;
      }
      if HOST_OPTIONS_WITH_VALUE.contains(&arg.as_str()) {
        iter.next();
        continue;
      }
      if arg.starts_with('-') {
        continue;
      }
    }
    if arg.is_empty() {
      continue;
    }
    if let Ok(url) = Url::parse(arg) {
      if schemes.iter().any(|s| s == url.scheme()) {
        targets.urls.push(arg.clone());
        continue;
      }
      if url.scheme() == "file" {
        if let Ok(path) = deno_path_util::url_to_file_path(&url)
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
    if exists(&path) {
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
    assert_eq!(
      classify_open_url("acme://open/doc/42?x=1"),
      OpenedItem::Url("acme://open/doc/42?x=1".to_string())
    );
    // Any other scheme is still a URL: the OS only routes declared schemes.
    assert_eq!(
      classify_open_url("other:thing"),
      OpenedItem::Url("other:thing".to_string())
    );
    // Not even a URL: passed through rather than dropped.
    assert_eq!(
      classify_open_url("not a url"),
      OpenedItem::Url("not a url".to_string())
    );
    #[cfg(unix)]
    {
      // Percent-encoded, as AppKit delivers it.
      assert_eq!(
        classify_open_url("file:///Users/me/My%20Notes.txt"),
        OpenedItem::File(PathBuf::from("/Users/me/My Notes.txt"))
      );
      // A remote file URL has no local path.
      assert_eq!(
        classify_open_url("file://server/share/x.txt"),
        OpenedItem::Url("file://server/share/x.txt".to_string())
      );
    }
    #[cfg(windows)]
    assert_eq!(
      classify_open_url("file:///C:/Users/me/My%20Notes.txt"),
      OpenedItem::File(PathBuf::from("C:\\Users\\me\\My Notes.txt"))
    );
  }

  #[test]
  fn arguments_after_the_terminator_are_positional() {
    let schemes = strings(&["acme"]);
    let never = |_: &Path| false;
    // Before `--`, `--runtime` takes the next argument as its value; after
    // it, `--runtime` and the arguments that follow are positional, so the
    // link after it is still a link and nothing is consumed as a value.
    let targets = parse_launch_args(
      &strings(&["--", "acme://a", "--runtime", "acme://b", "--x=acme://c"]),
      None,
      &schemes,
      never,
    );
    assert_eq!(targets.urls, strings(&["acme://a", "acme://b"]));
    // Without the terminator the same `--runtime` swallows `acme://b`.
    let targets = parse_launch_args(
      &strings(&["acme://a", "--runtime", "acme://b"]),
      None,
      &schemes,
      never,
    );
    assert_eq!(targets.urls, strings(&["acme://a"]));
    // A second `--` after the first is an ordinary (ignored) argument.
    let targets = parse_launch_args(
      &strings(&["--", "--", "acme://a"]),
      None,
      &schemes,
      never,
    );
    assert_eq!(targets.urls, strings(&["acme://a"]));
  }

  #[cfg(unix)]
  #[test]
  fn a_dash_file_after_the_terminator_is_a_file() {
    let exists = |p: &Path| p == Path::new("/home/me/-notes.txt");
    let cwd = Some(Path::new("/home/me"));
    let targets =
      parse_launch_args(&strings(&["--", "-notes.txt"]), cwd, &[], exists);
    assert_eq!(targets.files, vec![PathBuf::from("/home/me/-notes.txt")]);
    let targets =
      parse_launch_args(&strings(&["-notes.txt"]), cwd, &[], exists);
    assert!(targets.files.is_empty());
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
