// Copyright 2018-2026 the Deno authors. MIT license.

//! Where the desktop runtime gets the app's page origin and identifier from.
//!
//! The origin, in order of precedence:
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
//! The identifier (the reverse-DNS `desktop.app.identifier` that names the
//! app's web data directory, see `deno_lib::standalone::app_id`) comes from
//! the metadata's `app_identifier`, else the same [`APP_CONFIG_FILE`]'s
//! `identifier`, else nowhere.
//!
//! The deep-link schemes (`desktop.app.deepLinks`, which tell a link in the
//! launch arguments from any other argument) come from the metadata's
//! `app_deep_links`, which every desktop build of a CLI that knows the field
//! writes, else the file's `deepLinks`, else none. `singleInstance` is only
//! in the file (the runtime cannot act on it; see [`APP_CONFIG_FILE`]); it
//! is validated and logged.
//!
//! A configured value that does not validate is an error: starting the app at
//! an origin the developer did not configure would silently move its
//! origin-keyed storage and break any server allow-list. So is a configured
//! origin without an identifier: two apps configured with the same origin
//! would otherwise share web storage. (`deno desktop` refuses to build that;
//! this catches a file embedded by a CLI that predates the check.)

use std::path::Path;
use std::path::PathBuf;

use deno_lib::standalone::app_id::validate_app_identifier;
use deno_lib::standalone::app_origin::APP_CONFIG_FILE;
use deno_lib::standalone::app_origin::AppConfigFile;
use deno_lib::standalone::app_origin::AppOrigin;
use deno_lib::standalone::app_origin::DEFAULT_APP_ORIGIN;
use deno_lib::standalone::app_origin::parse_app_config_file;
use deno_lib::standalone::launch_args::normalize_deep_link_schemes;

/// Where a resolved origin came from, for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppOriginSource {
  Metadata,
  ConfigFile(PathBuf),
  Default,
}

/// The resolved page origin and identifier of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAppConfig {
  pub origin: AppOrigin,
  pub origin_source: AppOriginSource,
  /// The validated reverse-DNS app identifier, if one is configured.
  pub identifier: Option<String>,
  /// The normalized deep-link schemes the app registers.
  pub deep_links: Vec<String>,
  /// `singleInstance` from the config file, if the file was read and set it.
  pub single_instance: Option<bool>,
}

/// Resolve the app origin and identifier. `read_file` reads a file from the
/// embedded file system (`None` when absent); `root` is that file system's
/// root and `entrypoint_key` the entrypoint's `/`-separated path relative to
/// it.
pub fn resolve_app_config(
  metadata_origin: Option<&str>,
  metadata_identifier: Option<&str>,
  metadata_deep_links: Option<&[String]>,
  root: &Path,
  entrypoint_key: &str,
  read_file: impl Fn(&Path) -> Option<Vec<u8>>,
) -> Result<ResolvedAppConfig, String> {
  let metadata_origin = metadata_origin
    .map(|origin| {
      AppOrigin::parse(origin)
        .map_err(|e| format!("invalid app origin {origin:?} in metadata: {e}"))
    })
    .transpose()?;
  if let Some(identifier) = metadata_identifier {
    validate_app_identifier(identifier).map_err(|e| {
      format!("invalid app identifier {identifier:?} in metadata: {e}")
    })?;
  }
  let metadata_deep_links = metadata_deep_links
    .map(|schemes| {
      normalize_deep_link_schemes(schemes)
        .map_err(|e| format!("invalid deep links in metadata: {e}"))
    })
    .transpose()?;
  // The file is only consulted for what the metadata leaves unset.
  let file = if metadata_origin.is_none()
    || metadata_identifier.is_none()
    || metadata_deep_links.is_none()
  {
    find_app_config_file(root, entrypoint_key, read_file)?
  } else {
    None
  };
  let file_origin = file
    .as_ref()
    .and_then(|(path, config)| Some((config.origin.clone()?, path.clone())));
  let (origin, origin_source) = match (metadata_origin, file_origin) {
    (Some(origin), _) => (origin, AppOriginSource::Metadata),
    (None, Some((origin, path))) => (origin, AppOriginSource::ConfigFile(path)),
    // The nearest file decides, even when it leaves `origin` unset.
    (None, None) => {
      debug_assert!(AppOrigin::parse(DEFAULT_APP_ORIGIN).is_ok());
      (AppOrigin::default_origin(), AppOriginSource::Default)
    }
  };
  let single_instance =
    file.as_ref().and_then(|(_, config)| config.single_instance);
  let deep_links = match metadata_deep_links {
    Some(schemes) => schemes,
    None => file
      .as_ref()
      .and_then(|(_, config)| config.deep_links.clone())
      .unwrap_or_default(),
  };
  let file_path = file.as_ref().map(|(path, _)| path.clone());
  let identifier = metadata_identifier
    .map(|id| id.to_string())
    .or_else(|| file.and_then(|(_, config)| config.identifier));
  if single_instance == Some(true) && identifier.is_none() {
    let from = file_path
      .map(|path| path.display().to_string())
      .unwrap_or_else(|| APP_CONFIG_FILE.to_string());
    return Err(format!(
      "singleInstance (from {from}) requires an app identifier: the \
       single-instance lock is keyed on it; set desktop.app.identifier in \
       deno.json, or \"identifier\" in {APP_CONFIG_FILE}"
    ));
  }
  if origin_source != AppOriginSource::Default && identifier.is_none() {
    let from = match &origin_source {
      AppOriginSource::ConfigFile(path) => path.display().to_string(),
      _ => "the binary metadata".to_string(),
    };
    return Err(format!(
      "app origin {origin} (from {from}) requires an app identifier: set \
       desktop.app.identifier in deno.json, or \"identifier\" in {APP_CONFIG_FILE} \
       (e.g. \"com.example.myapp\"), so that apps sharing an origin do not \
       share web storage"
    ));
  }
  Ok(ResolvedAppConfig {
    origin,
    origin_source,
    identifier,
    deep_links,
    single_instance,
  })
}

/// The nearest [`APP_CONFIG_FILE`] and its parsed contents, if any.
fn find_app_config_file(
  root: &Path,
  entrypoint_key: &str,
  read_file: impl Fn(&Path) -> Option<Vec<u8>>,
) -> Result<Option<(PathBuf, AppConfigFile)>, String> {
  for dir in config_file_dirs(root, entrypoint_key) {
    let path = join_key(&dir, APP_CONFIG_FILE);
    let Some(bytes) = read_file(&path) else {
      continue;
    };
    return match parse_app_config_file(&bytes) {
      Ok(config) => Ok(Some((path, config))),
      Err(e) => Err(format!("{}: {e}", path.display())),
    };
  }
  Ok(None)
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

  /// `resolve_app_config` rooted at `/vfs`, for brevity.
  fn resolve(
    metadata_origin: Option<&str>,
    metadata_identifier: Option<&str>,
    entrypoint_key: &str,
    read: impl Fn(&Path) -> Option<Vec<u8>>,
  ) -> Result<ResolvedAppConfig, String> {
    // A CLI that writes the deep-link field always writes it for a desktop
    // build; model that here so these tests exercise origin/identifier alone.
    resolve_app_config(
      metadata_origin,
      metadata_identifier,
      Some(&[]),
      Path::new("/vfs"),
      entrypoint_key,
      read,
    )
  }

  #[test]
  fn deep_links_from_metadata_or_the_file() {
    let read = reader(&[(
      ".deno-desktop/app.json",
      r#"{"identifier":"com.a.b","deepLinks":["Acme"],"singleInstance":true}"#,
    )]);
    // A stock CLI writes no deep-link field: the file's list applies.
    let config =
      resolve_app_config(None, None, None, Path::new("/vfs"), "main.ts", &read)
        .unwrap();
    assert_eq!(config.deep_links, vec!["acme".to_string()]);
    assert_eq!(config.single_instance, Some(true));
    // The metadata's list wins, even when empty.
    let schemes = vec!["T3Code".to_string()];
    let config = resolve_app_config(
      None,
      None,
      Some(&schemes),
      Path::new("/vfs"),
      "main.ts",
      &read,
    )
    .unwrap();
    assert_eq!(config.deep_links, vec!["t3code".to_string()]);
    // Everything in the metadata: the file is not read.
    let config = resolve_app_config(
      Some("a://a"),
      Some("com.a.a"),
      Some(&[]),
      Path::new("/vfs"),
      "main.ts",
      reader(&[(".deno-desktop/app.json", "not json")]),
    )
    .unwrap();
    assert!(config.deep_links.is_empty());
    assert_eq!(config.single_instance, None);
    // Invalid metadata schemes are an error.
    let bad = vec!["http".to_string()];
    let err = resolve_app_config(
      None,
      None,
      Some(&bad),
      Path::new("/vfs"),
      "main.ts",
      |_| None,
    )
    .unwrap_err();
    assert!(err.contains("deep links"), "{err}");
    // No file and no metadata field: no schemes.
    let config =
      resolve_app_config(None, None, None, Path::new("/vfs"), "m.ts", |_| None)
        .unwrap();
    assert!(config.deep_links.is_empty());
  }

  #[test]
  fn single_instance_requires_an_identifier() {
    let read =
      reader(&[(".deno-desktop/app.json", r#"{"singleInstance":true}"#)]);
    let err = resolve(None, None, "main.ts", &read).unwrap_err();
    assert!(err.contains("singleInstance"), "{err}");
    assert!(err.contains("identifier"), "{err}");
    // The metadata's identifier satisfies it; `false` needs nothing.
    let config = resolve(None, Some("com.a.b"), "main.ts", &read).unwrap();
    assert_eq!(config.single_instance, Some(true));
    let read =
      reader(&[(".deno-desktop/app.json", r#"{"singleInstance":false}"#)]);
    resolve(None, None, "main.ts", read).unwrap();
  }

  #[test]
  fn metadata_wins_over_the_config_file() {
    let read = reader(&[(
      ".deno-desktop/app.json",
      r#"{"origin":"b://b","identifier":"com.b.b"}"#,
    )]);
    let config =
      resolve(Some("a://a"), Some("com.a.a"), "main.ts", &read).unwrap();
    assert_eq!(config.origin.as_origin_string(), "a://a");
    assert_eq!(config.origin_source, AppOriginSource::Metadata);
    assert_eq!(config.identifier.as_deref(), Some("com.a.a"));
    // Each key falls back to the file on its own.
    let config = resolve(Some("a://a"), None, "main.ts", &read).unwrap();
    assert_eq!(config.origin.as_origin_string(), "a://a");
    assert_eq!(config.identifier.as_deref(), Some("com.b.b"));
    let config = resolve(None, Some("com.a.a"), "main.ts", &read).unwrap();
    assert_eq!(config.origin.as_origin_string(), "b://b");
    assert_eq!(config.identifier.as_deref(), Some("com.a.a"));
  }

  #[test]
  fn metadata_alone_does_not_read_the_file() {
    // Both keys in the metadata: a broken file is never parsed.
    let read = reader(&[(".deno-desktop/app.json", "not json")]);
    let config =
      resolve(Some("a://a"), Some("com.a.a"), "main.ts", read).unwrap();
    assert_eq!(config.identifier.as_deref(), Some("com.a.a"));
  }

  #[test]
  fn reads_the_nearest_config_file() {
    let read = reader(&[
      (
        ".deno-desktop/app.json",
        r#"{"origin":"root://app","identifier":"com.root.app"}"#,
      ),
      (
        "src/.deno-desktop/app.json",
        r#"{"origin":"T3Code://App","identifier":"com.t3.code"}"#,
      ),
    ]);
    let config = resolve(None, None, "src/main.ts", &read).unwrap();
    assert_eq!(config.origin.as_origin_string(), "t3code://app");
    assert_eq!(
      config.origin_source,
      AppOriginSource::ConfigFile(vfs("src/.deno-desktop/app.json"))
    );
    assert_eq!(config.identifier.as_deref(), Some("com.t3.code"));
    let config = resolve(None, None, "lib/main.ts", &read).unwrap();
    assert_eq!(config.origin.as_origin_string(), "root://app");
    assert_eq!(config.identifier.as_deref(), Some("com.root.app"));
  }

  #[test]
  fn defaults_when_nothing_is_configured() {
    let config = resolve(None, None, "main.ts", reader(&[])).unwrap();
    assert_eq!(config.origin.as_origin_string(), DEFAULT_APP_ORIGIN);
    assert_eq!(config.origin_source, AppOriginSource::Default);
    assert_eq!(config.identifier, None);
    let read = reader(&[(".deno-desktop/app.json", "{}")]);
    let config = resolve(None, None, "main.ts", read).unwrap();
    assert_eq!(config.origin.as_origin_string(), DEFAULT_APP_ORIGIN);
    assert_eq!(config.identifier, None);
    // An identifier alone keeps the default origin.
    let read =
      reader(&[(".deno-desktop/app.json", r#"{"identifier":"com.a.b"}"#)]);
    let config = resolve(None, None, "main.ts", read).unwrap();
    assert_eq!(config.origin_source, AppOriginSource::Default);
    assert_eq!(config.identifier.as_deref(), Some("com.a.b"));
  }

  #[test]
  fn a_configured_origin_requires_an_identifier() {
    let read =
      reader(&[(".deno-desktop/app.json", r#"{"origin":"t3code://app"}"#)]);
    let err = resolve(None, None, "main.ts", &read).unwrap_err();
    assert!(err.contains("requires an app identifier"), "{err}");
    assert!(err.contains("app.json"), "{err}");
    assert!(err.contains("desktop.app.identifier"), "{err}");
    // The metadata's identifier satisfies it.
    resolve(None, Some("com.t3.code"), "main.ts", &read).unwrap();
    // So does one in the file (the other tests).
    let err = resolve(Some("a://a"), None, "main.ts", reader(&[])).unwrap_err();
    assert!(err.contains("the binary metadata"), "{err}");
  }

  #[test]
  fn invalid_configuration_is_an_error() {
    assert!(
      resolve(Some("http://app"), Some("com.a.b"), "m.ts", |_| None)
        .unwrap_err()
        .contains("metadata")
    );
    let err =
      resolve(Some("a://a"), Some("notes"), "m.ts", |_| None).unwrap_err();
    assert!(err.contains("identifier"), "{err}");
    assert!(err.contains("reverse-DNS"), "{err}");
    let read = reader(&[(
      ".deno-desktop/app.json",
      r#"{"origin":"t3code://app:1","identifier":"com.t3.code"}"#,
    )]);
    let err = resolve(None, None, "main.ts", read).unwrap_err();
    assert!(err.contains("app.json"), "{err}");
    assert!(err.contains("port"), "{err}");
    let read = reader(&[(
      ".deno-desktop/app.json",
      r#"{"origin":"t3code://app","identifier":"../evil"}"#,
    )]);
    let err = resolve(None, None, "main.ts", read).unwrap_err();
    assert!(err.contains("app.json"), "{err}");
    assert!(err.contains("identifier"), "{err}");
  }
}
