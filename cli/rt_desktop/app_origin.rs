// Copyright 2018-2026 the Deno authors. MIT license.

//! Where the desktop runtime gets the app's page origin from.
//!
//! In order of precedence:
//!
//! 1. The binary metadata's `app_origin` — `desktop.app.origin` from
//!    `deno.json`, validated and normalized by the `deno desktop` that
//!    compiled the app.
//! 2. An embedded [`APP_CONFIG_FILE`] (`.deno-desktop/app.json`) in the
//!    entrypoint's directory or any directory above it, up to the root of the
//!    embedded file system. This lets an app built by a `deno desktop` that
//!    predates `desktop.app.origin` — and so rejects the key in `deno.json` —
//!    run at a configured origin with a newer runtime: the file only has to be
//!    embedded (`"compile": { "include": [".deno-desktop/app.json"] }`).
//! 3. [`DEFAULT_APP_ORIGIN`].
//!
//! A configured value that does not validate is an error: starting the app at
//! an origin the developer did not configure would silently move its
//! origin-keyed storage and break any server allow-list.

use std::path::Path;
use std::path::PathBuf;

use deno_lib::standalone::app_origin::APP_CONFIG_FILE;
use deno_lib::standalone::app_origin::AppOrigin;
use deno_lib::standalone::app_origin::DEFAULT_APP_ORIGIN;
use deno_lib::standalone::app_origin::parse_app_config_file;

/// Where a resolved origin came from, for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppOriginSource {
  Metadata,
  ConfigFile(PathBuf),
  Default,
}

/// Resolve the app origin. `read_file` reads a file from the embedded file
/// system (`None` when absent); `root` is that file system's root and
/// `entrypoint_key` the entrypoint's `/`-separated path relative to it.
pub fn resolve_app_origin(
  metadata_origin: Option<&str>,
  root: &Path,
  entrypoint_key: &str,
  read_file: impl Fn(&Path) -> Option<Vec<u8>>,
) -> Result<(AppOrigin, AppOriginSource), String> {
  if let Some(origin) = metadata_origin {
    return AppOrigin::parse(origin)
      .map(|o| (o, AppOriginSource::Metadata))
      .map_err(|e| format!("invalid app origin {origin:?} in metadata: {e}"));
  }
  for dir in config_file_dirs(root, entrypoint_key) {
    let path = join_key(&dir, APP_CONFIG_FILE);
    let Some(bytes) = read_file(&path) else {
      continue;
    };
    return match parse_app_config_file(&bytes) {
      Ok(Some(origin)) => Ok((origin, AppOriginSource::ConfigFile(path))),
      // The nearest file decides, even when it leaves `origin` unset.
      Ok(None) => Ok((AppOrigin::default_origin(), AppOriginSource::Default)),
      Err(e) => Err(format!("{}: {e}", path.display())),
    };
  }
  debug_assert!(AppOrigin::parse(DEFAULT_APP_ORIGIN).is_ok());
  Ok((AppOrigin::default_origin(), AppOriginSource::Default))
}

/// The directories searched for [`APP_CONFIG_FILE`], nearest first: the
/// entrypoint's directory, then each parent up to and including `root`. A key
/// that would climb out of `root` (`..`) yields just `root`.
fn config_file_dirs(root: &Path, entrypoint_key: &str) -> Vec<PathBuf> {
  let mut segments: Vec<&str> = entrypoint_key
    .split('/')
    .filter(|s| !s.is_empty() && *s != ".")
    .collect();
  // Drop the file name.
  segments.pop();
  if segments.contains(&"..") {
    segments.clear();
  }
  let mut dirs = Vec::with_capacity(segments.len() + 1);
  while !segments.is_empty() {
    dirs.push(join_key(root, &segments.join("/")));
    segments.pop();
  }
  dirs.push(root.to_path_buf());
  dirs
}

/// Join a `/`-separated relative key onto `base` component by component, so
/// the result uses the platform separator.
fn join_key(base: &Path, key: &str) -> PathBuf {
  let mut path = base.to_path_buf();
  for segment in key.split('/').filter(|s| !s.is_empty()) {
    path.push(segment);
  }
  path
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use super::*;

  fn vfs(key: &str) -> PathBuf {
    join_key(Path::new("/vfs"), key)
  }

  fn reader(
    files: &[(&str, &str)],
  ) -> impl Fn(&Path) -> Option<Vec<u8>> + use<> {
    let map: HashMap<PathBuf, Vec<u8>> = files
      .iter()
      .map(|(p, c)| (vfs(p), c.as_bytes().to_vec()))
      .collect();
    move |p: &Path| map.get(p).cloned()
  }

  #[test]
  fn searches_from_the_entrypoint_up_to_the_root() {
    let root = Path::new("/vfs");
    assert_eq!(
      config_file_dirs(root, "src/app/main.ts"),
      vec![vfs("src/app"), vfs("src"), vfs(""),]
    );
    assert_eq!(config_file_dirs(root, "main.ts"), vec![vfs("")]);
    assert_eq!(
      config_file_dirs(root, "./src//main.ts"),
      vec![vfs("src"), vfs("")]
    );
    assert_eq!(config_file_dirs(root, "../outside/main.ts"), vec![vfs("")]);
  }

  #[test]
  fn metadata_wins_over_the_config_file() {
    let read = reader(&[(".deno-desktop/app.json", r#"{"origin":"b://b"}"#)]);
    let (origin, source) =
      resolve_app_origin(Some("a://a"), Path::new("/vfs"), "main.ts", read)
        .unwrap();
    assert_eq!(origin.as_origin_string(), "a://a");
    assert_eq!(source, AppOriginSource::Metadata);
  }

  #[test]
  fn reads_the_nearest_config_file() {
    let read = reader(&[
      (".deno-desktop/app.json", r#"{"origin":"root://app"}"#),
      ("src/.deno-desktop/app.json", r#"{"origin":"T3Code://App"}"#),
    ]);
    let (origin, source) =
      resolve_app_origin(None, Path::new("/vfs"), "src/main.ts", &read)
        .unwrap();
    assert_eq!(origin.as_origin_string(), "t3code://app");
    assert_eq!(
      source,
      AppOriginSource::ConfigFile(vfs("src/.deno-desktop/app.json"))
    );
    let (origin, _) =
      resolve_app_origin(None, Path::new("/vfs"), "lib/main.ts", &read)
        .unwrap();
    assert_eq!(origin.as_origin_string(), "root://app");
  }

  #[test]
  fn defaults_when_nothing_is_configured() {
    let (origin, source) =
      resolve_app_origin(None, Path::new("/vfs"), "main.ts", reader(&[]))
        .unwrap();
    assert_eq!(origin.as_origin_string(), DEFAULT_APP_ORIGIN);
    assert_eq!(source, AppOriginSource::Default);
    let read = reader(&[(".deno-desktop/app.json", "{}")]);
    let (origin, _) =
      resolve_app_origin(None, Path::new("/vfs"), "main.ts", read).unwrap();
    assert_eq!(origin.as_origin_string(), DEFAULT_APP_ORIGIN);
  }

  #[test]
  fn invalid_configuration_is_an_error() {
    assert!(
      resolve_app_origin(Some("http://app"), Path::new("/vfs"), "m.ts", |_| {
        None
      })
      .unwrap_err()
      .contains("metadata")
    );
    let read =
      reader(&[(".deno-desktop/app.json", r#"{"origin":"t3code://app:1"}"#)]);
    let err =
      resolve_app_origin(None, Path::new("/vfs"), "main.ts", read).unwrap_err();
    assert!(err.contains("app.json"), "{err}");
    assert!(err.contains("port"), "{err}");
  }
}
