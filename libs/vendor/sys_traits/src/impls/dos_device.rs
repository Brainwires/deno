// denext (DENEXT.md item 357): Windows paths that name a DOS device.
//
// Before Windows 11, Win32 path parsing turns any path whose last component is
// a DOS device name (`CON`, `NUL`, `COM1`, ..., with or without an extension:
// `C:\dir\con.mjs`, `C:\dir\aux`) into that device (`\\.\CON`), so such a file
// can't be opened, created or deleted through an ordinary path (Windows 11
// opens `C:\dir\con.mjs` as a file). Node passes every fs path through
// `path.toNamespacedPath()` (`\\?\C:\dir\con.mjs`), which skips Win32 path
// parsing, so it opens these as files everywhere. `dos_device_safe_path` does
// the same, but only for a path that names a device, so every other path
// reaches the OS exactly as it did.

use std::borrow::Cow;
use std::path::Path;

/// Returns `path` in its namespaced (`\\?\`) form when a component of it is a
/// DOS device name, so Windows opens it as a file, as Node does; any other path
/// (and any path on other platforms) is returned unchanged.
pub fn dos_device_safe_path(path: &Path) -> Cow<'_, Path> {
  #[cfg(windows)]
  {
    let Some(s) = path.to_str() else {
      return Cow::Borrowed(path);
    };
    match namespace_dos_device_path(s, || {
      std::env::current_dir()
        .ok()
        .and_then(|dir| dir.to_str().map(|s| s.to_string()))
    }) {
      Some(namespaced) => Cow::Owned(std::path::PathBuf::from(namespaced)),
      None => Cow::Borrowed(path),
    }
  }
  #[cfg(not(windows))]
  {
    Cow::Borrowed(path)
  }
}

/// The string logic behind [`dos_device_safe_path`], following Windows path
/// rules on every platform (so it is tested everywhere).
///
/// Returns `None` when `path` needs no change: no component is a DOS device
/// name, or it is already a `\\?\`, `\\.\` or `\??\` path (a deliberate device
/// path such as `\\.\NUL` stays one). Otherwise returns the absolute `\\?\`
/// path with `/` turned into `\` and `.` / `..` resolved, as Node's
/// `path.toNamespacedPath()` does: `\\?\C:\dir\con.mjs` for a drive path,
/// `\\?\UNC\server\share\con.mjs` for a UNC path. A relative or root-relative
/// path is resolved against `cwd()`; a drive-relative one (`C:con`) and one
/// that can't be resolved are returned as `None`.
pub fn namespace_dos_device_path(
  path: &str,
  cwd: impl FnOnce() -> Option<String>,
) -> Option<String> {
  let is_sep = |c: char| c == '\\' || c == '/';
  let b = path.as_bytes();
  // Already namespaced, or a device path: leave it alone.
  if b.len() >= 4
    && is_sep(b[0] as char)
    && (is_sep(b[1] as char) || b[1] == b'?')
    && (b[2] == b'?' || b[2] == b'.')
    && is_sep(b[3] as char)
  {
    return None;
  }
  if !path.split(is_sep).any(is_dos_device_name) {
    return None;
  }

  let (prefix, rest) = match split_prefix(path) {
    Prefix::Absolute { prefix, rest } => (prefix, rest.to_string()),
    Prefix::RootRelative => {
      let cwd = cwd()?;
      match split_prefix(&cwd) {
        Prefix::Absolute { prefix, .. } => (prefix, path.to_string()),
        _ => return None,
      }
    }
    Prefix::Relative => {
      let cwd = cwd()?;
      match split_prefix(&cwd) {
        Prefix::Absolute { prefix, rest } => (prefix, format!("{rest}\\{path}")),
        _ => return None,
      }
    }
    Prefix::DriveRelative | Prefix::Invalid => return None,
  };

  let mut components: Vec<&str> = Vec::new();
  for component in rest.split(is_sep) {
    match component {
      "" | "." => {}
      ".." => {
        components.pop();
      }
      c => components.push(c),
    }
  }
  let mut out = format!("\\\\?\\{prefix}");
  if components.is_empty() {
    out.push('\\');
  }
  for c in components {
    out.push('\\');
    out.push_str(c);
  }
  Some(out)
}

enum Prefix<'a> {
  /// `C:\rest` (prefix `C:`) or `\\server\share\rest` (prefix
  /// `UNC\server\share`).
  Absolute { prefix: String, rest: &'a str },
  /// `\rest`: absolute on the current drive.
  RootRelative,
  /// `C:rest`: relative to drive C's own current directory.
  DriveRelative,
  /// `rest`
  Relative,
  /// `\\server` with no share.
  Invalid,
}

fn split_prefix(path: &str) -> Prefix<'_> {
  let is_sep = |c: char| c == '\\' || c == '/';
  let b = path.as_bytes();
  if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
    return if b.len() == 2 || !is_sep(b[2] as char) {
      Prefix::DriveRelative
    } else {
      Prefix::Absolute {
        prefix: path[..2].to_string(),
        rest: &path[2..],
      }
    };
  }
  if b.len() >= 2 && is_sep(b[0] as char) && is_sep(b[1] as char) {
    let mut parts = path[2..].splitn(3, is_sep);
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    if server.is_empty() || share.is_empty() {
      return Prefix::Invalid;
    }
    let rest_start = 2 + server.len() + 1 + share.len();
    return Prefix::Absolute {
      prefix: format!("UNC\\{server}\\{share}"),
      rest: &path[rest_start..],
    };
  }
  if !b.is_empty() && is_sep(b[0] as char) {
    return Prefix::RootRelative;
  }
  Prefix::Relative
}

/// Whether Win32 path parsing treats `component` as a DOS device: its name up
/// to the first `.` or `:`, without trailing spaces, is `CON`, `PRN`, `AUX`,
/// `NUL`, or `COM` / `LPT` followed by a digit or a superscript 1-3 (case
/// insensitive).
fn is_dos_device_name(component: &str) -> bool {
  let end = component.find(['.', ':']).unwrap_or(component.len());
  let base = component[..end].trim_end_matches(' ');
  let mut chars = base.chars();
  let name: String = chars.by_ref().take(3).collect();
  let name = name.to_ascii_uppercase();
  let tail: Vec<char> = chars.collect();
  match name.as_str() {
    "CON" | "PRN" | "AUX" | "NUL" => tail.is_empty(),
    "COM" | "LPT" => {
      tail.len() == 1
        && (tail[0].is_ascii_digit() || matches!(tail[0], '¹' | '²' | '³'))
    }
    _ => false,
  }
}

#[cfg(test)]
mod tests {
  use super::is_dos_device_name;
  use super::namespace_dos_device_path;

  fn ns(path: &str) -> Option<String> {
    namespace_dos_device_path(path, || Some("C:\\work\\dir".to_string()))
  }

  #[test]
  fn device_names() {
    for name in [
      "con", "CON", "Con", "con.mjs", "CON.txt", "con.tar.gz", "con ",
      "con  .mjs", "con:", "nul", "NUL.json", "prn", "aux", "aux.js", "com1",
      "COM9.log", "lpt1", "LPT3.txt", "com0", "lpt0", "com¹", "LPT²",
    ] {
      assert!(is_dos_device_name(name), "{name}");
    }
    for name in [
      "", "co", "conn", "con1", "console", "icon", "xcon.mjs", "com", "com10",
      "lpt", "nul1", "auxiliary", "_con", "con_", "comA", "con$",
    ] {
      assert!(!is_dos_device_name(name), "{name}");
    }
  }

  #[test]
  fn other_paths_are_left_alone() {
    assert_eq!(ns("C:\\work\\a.mjs"), None);
    assert_eq!(ns("C:/work/console.mjs"), None);
    assert_eq!(ns("relative\\icon.png"), None);
    assert_eq!(ns("\\\\server\\share\\file"), None);
    assert_eq!(ns(""), None);
  }

  #[test]
  fn device_and_namespaced_paths_are_left_alone() {
    assert_eq!(ns("\\\\.\\CON"), None);
    assert_eq!(ns("\\\\.\\pipe\\con"), None);
    assert_eq!(ns("//./NUL"), None);
    assert_eq!(ns("\\\\?\\C:\\dir\\con.mjs"), None);
    assert_eq!(ns("\\??\\C:\\dir\\con.mjs"), None);
  }

  #[test]
  fn drive_paths() {
    assert_eq!(
      ns("D:\\a\\deno\\test\\.tmp.163\\con.mjs").as_deref(),
      Some("\\\\?\\D:\\a\\deno\\test\\.tmp.163\\con.mjs")
    );
    assert_eq!(
      ns("C:/work/node_modules/con").as_deref(),
      Some("\\\\?\\C:\\work\\node_modules\\con")
    );
    assert_eq!(
      ns("C:\\work\\.\\x\\..\\node_modules\\con\\index.mjs").as_deref(),
      Some("\\\\?\\C:\\work\\node_modules\\con\\index.mjs")
    );
    assert_eq!(
      ns("C:\\work\\nul.txt\\").as_deref(),
      Some("\\\\?\\C:\\work\\nul.txt")
    );
    assert_eq!(ns("C:\\..\\..\\con").as_deref(), Some("\\\\?\\C:\\con"));
  }

  #[test]
  fn unc_paths() {
    assert_eq!(
      ns("\\\\server\\share\\dir\\aux.js").as_deref(),
      Some("\\\\?\\UNC\\server\\share\\dir\\aux.js")
    );
    assert_eq!(
      ns("//server/share/../con").as_deref(),
      Some("\\\\?\\UNC\\server\\share\\con")
    );
    // A UNC path with no share can't be namespaced.
    assert_eq!(ns("\\\\con"), None);
  }

  #[test]
  fn relative_paths_resolve_against_cwd() {
    assert_eq!(
      ns("con.mjs").as_deref(),
      Some("\\\\?\\C:\\work\\dir\\con.mjs")
    );
    assert_eq!(
      ns(".\\sub/../node_modules/con").as_deref(),
      Some("\\\\?\\C:\\work\\dir\\node_modules\\con")
    );
    assert_eq!(ns("..\\..\\..\\nul").as_deref(), Some("\\\\?\\C:\\nul"));
    assert_eq!(ns("\\tmp\\con.txt").as_deref(), Some("\\\\?\\C:\\tmp\\con.txt"));
    let unc_cwd = || Some("\\\\srv\\share\\proj".to_string());
    assert_eq!(
      namespace_dos_device_path("con.mjs", unc_cwd).as_deref(),
      Some("\\\\?\\UNC\\srv\\share\\proj\\con.mjs")
    );
    assert_eq!(
      namespace_dos_device_path("\\con.mjs", unc_cwd).as_deref(),
      Some("\\\\?\\UNC\\srv\\share\\con.mjs")
    );
  }

  #[test]
  fn unresolvable_paths_are_left_alone() {
    // Drive-relative: resolved against drive C's own current directory, which
    // only Win32 knows.
    assert_eq!(ns("C:con.mjs"), None);
    // No current directory to resolve against.
    assert_eq!(namespace_dos_device_path("con.mjs", || None), None);
  }
}
