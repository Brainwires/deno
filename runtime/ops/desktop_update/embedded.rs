// Copyright 2018-2026 the Deno authors. MIT license.

//! The staged app's OWN version, compared with the version the manifest
//! offers before the app is staged.
//!
//! The manifest says which version an archive holds, and the OS signature
//! check says who built it, but neither says the two agree: an older build
//! signed by the same identity (or a publishing mistake) served under a newer
//! manifest version would install as that version, and skip every version in
//! between on the next check. So the version is read from the staged app
//! itself:
//!
//! - **Every platform but AppImage:** the version `deno desktop` compiles
//!   into the app (deno.json `version`, the `app_version` of the standalone
//!   metadata; the same value the running app reports as its version). It
//!   lives in the app's runtime library (`<App>.dll` next to `<App>.exe`,
//!   `<App>.so` next to the Linux executable, `libruntime.dylib` or
//!   `<exe>.dylib` in a bundle), in the data section that starts with the
//!   magic `d3n0l4nd` and a little-endian `u64` length, followed by the
//!   metadata JSON. It must equal the manifest's version (semver precedence:
//!   build metadata aside).
//! - **macOS, in addition:** the bundle's `Info.plist`
//!   `CFBundleShortVersionString`. The stock CLI writes the version's numeric
//!   `MAJOR.MINOR.PATCH` there (a prerelease can't be expressed), so that is
//!   what is compared. A binary plist is not read (the CLI writes XML); the
//!   embedded version still applies.
//! - **Windows:** the stock CLI writes no `VERSIONINFO` resource into the
//!   app's executable (it ships laufey's launcher renamed, without rcedit),
//!   so there is no PE version to read; the embedded version above is the
//!   check.
//! - **AppImage:** the runtime library is inside the image's compressed
//!   squashfs, which this runtime does not unpack; an AppImage update relies
//!   on the manifest signature and the archive's SHA-256 alone (as it does
//!   for its code signature: Linux has none).
//!
//! A staged app whose version can't be found is refused like a mismatch:
//! every app `deno desktop` packages carries it.

#![allow(
  clippy::disallowed_methods,
  reason = "reads the staged app next to the install, outside any user \
            permission sandbox, by design"
)]

use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::Path;
use std::path::PathBuf;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;
use super::layout::InstallKind;
use super::layout::InstallLayout;
use super::manifest::compare_versions;
use super::manifest::parse_version;

/// The standalone data section's magic (`cli/lib/standalone/binary.rs`).
const MAGIC: &[u8] = b"d3n0l4nd";
/// The largest metadata JSON believed (it holds argv, flags, CA data).
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
/// How much of the library is read at a time while looking for the magic.
const SCAN_CHUNK: usize = 1024 * 1024;

/// The runtime libraries an app of `kind` with the executable `exe` may
/// carry its compiled data in, most likely first (the names the stock CLI
/// gives them; see `cli/tools/desktop.rs`).
pub fn runtime_libraries(kind: InstallKind, exe: &Path) -> Vec<PathBuf> {
  match kind {
    InstallKind::MacBundle => {
      // `cef`: `<backend-executable>.dylib` next to it; `webview`:
      // `libruntime.dylib` (Contents/Frameworks, then Contents/MacOS).
      let mut libs = vec![exe.with_extension("dylib")];
      libs.extend(super::swap::bundle_runtime_path(exe));
      libs
    }
    // `<App>.exe` loads `<App>.dll`; a Linux `<App>` loads `<App>.so` (the
    // launcher strips the last extension, as `with_extension` does).
    InstallKind::AppDir if cfg!(windows) => vec![exe.with_extension("dll")],
    InstallKind::AppDir => vec![exe.with_extension("so")],
    InstallKind::AppImage => Vec::new(),
  }
}

/// The compiled metadata found in a runtime library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedMetadata {
  /// deno.json `version`, when the app was built with one.
  pub app_version: Option<String>,
}

/// Find the standalone metadata in the file at `path`: the first `MAGIC`
/// followed by a plausible length and a JSON object with the metadata's
/// required keys (the bare magic also appears in the code that looks for
/// the section, followed by anything). `None` when there is none.
pub fn read_metadata(path: &Path) -> std::io::Result<Option<EmbeddedMetadata>> {
  read_metadata_with(path, SCAN_CHUNK)
}

fn read_metadata_with(
  path: &Path,
  chunk: usize,
) -> std::io::Result<Option<EmbeddedMetadata>> {
  let mut scan = File::open(path)?;
  let mut probe = File::open(path)?;
  let mut buf = vec![0u8; chunk.max(MAGIC.len())];
  // The tail of the previous chunk, so a magic split across two is found.
  let mut window: Vec<u8> = Vec::with_capacity(buf.len() + MAGIC.len());
  // The file offset of `window[0]`.
  let mut offset: u64 = 0;
  loop {
    let n = scan.read(&mut buf)?;
    if n == 0 {
      return Ok(None);
    }
    window.extend_from_slice(&buf[..n]);
    let mut from = 0;
    while let Some(i) =
      window[from..].windows(MAGIC.len()).position(|w| w == MAGIC)
    {
      let at = offset + (from + i + MAGIC.len()) as u64;
      if let Some(meta) = metadata_at(&mut probe, at)? {
        return Ok(Some(meta));
      }
      from += i + 1;
    }
    let keep = window.len().min(MAGIC.len() - 1);
    let drop = window.len() - keep;
    window.drain(..drop);
    offset += drop as u64;
  }
}

/// The metadata at `at` (just past a magic), if that is what is there.
fn metadata_at(
  file: &mut File,
  at: u64,
) -> std::io::Result<Option<EmbeddedMetadata>> {
  file.seek(SeekFrom::Start(at))?;
  let mut len = [0u8; 8];
  if read_full(file, &mut len)? < len.len() {
    return Ok(None);
  }
  let len = u64::from_le_bytes(len);
  if !(2..=MAX_METADATA_BYTES).contains(&len) {
    return Ok(None);
  }
  let mut json = vec![0u8; len as usize];
  if read_full(file, &mut json)? < json.len() || json[0] != b'{' {
    return Ok(None);
  }
  let Ok(serde_json::Value::Object(map)) =
    serde_json::from_slice::<serde_json::Value>(&json)
  else {
    return Ok(None);
  };
  // Keys every serialized `Metadata` has, whatever else it holds.
  if !(map.contains_key("argv") && map.contains_key("entrypoint_key")) {
    return Ok(None);
  }
  Ok(Some(EmbeddedMetadata {
    app_version: map
      .get("app_version")
      .and_then(|v| v.as_str())
      .map(str::to_string),
  }))
}

/// `read`, repeated until `buf` is full or the file ends; the bytes read.
fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
  let mut got = 0;
  while got < buf.len() {
    match file.read(&mut buf[got..])? {
      0 => break,
      n => got += n,
    }
  }
  Ok(got)
}

/// The `<string>` value of `key` in an XML property list.
pub fn plist_string(xml: &str, key: &str) -> Option<String> {
  let marker = format!("<key>{key}</key>");
  let rest = xml[xml.find(&marker)? + marker.len()..].trim_start();
  let rest = rest.strip_prefix("<string>")?;
  Some(rest[..rest.find("</string>")?].trim().to_string())
}

/// The numeric `MAJOR.MINOR.PATCH` of a dotted version (missing fields are
/// 0, more than three refuse). `None` when it is not all digits.
fn numeric_core(text: &str) -> Option<[u64; 3]> {
  let parts: Vec<&str> = text.split('.').collect();
  if parts.is_empty() || parts.len() > 3 {
    return None;
  }
  let mut out = [0u64; 3];
  for (slot, part) in out.iter_mut().zip(&parts) {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
      return None;
    }
    *slot = part.parse().ok()?;
  }
  Some(out)
}

/// Refuse (`version_mismatch`) a staged app at `staged` whose own version
/// is not `offered` (see the module docs).
pub fn check_embedded_version(
  layout: &InstallLayout,
  staged: &Path,
  offered: &str,
) -> Result<(), UpdateError> {
  if layout.kind == InstallKind::AppImage {
    return Ok(());
  }
  let offered_v = parse_version(offered, "version")?;
  let exe = layout.exe_in(staged);
  let libraries = runtime_libraries(layout.kind, &exe);
  let mut found = None;
  for lib in &libraries {
    if !std::fs::symlink_metadata(lib).is_ok_and(|m| m.is_file()) {
      continue;
    }
    if let Some(meta) =
      read_metadata(lib).map_err(|e| UpdateError::io(lib.display(), e))?
    {
      found = Some((lib, meta));
      break;
    }
  }
  let Some((lib, meta)) = found else {
    return err(
      Code::VersionMismatch,
      format!(
        "the staged app carries no compiled app metadata (looked in {}), so \
         its version can't be checked",
        libraries
          .iter()
          .map(|p| p.display().to_string())
          .collect::<Vec<_>>()
          .join(", ")
      ),
    );
  };
  let Some(embedded) = meta.app_version else {
    return err(
      Code::VersionMismatch,
      format!(
        "the staged app was built without a version ({} has none), the \
         manifest offers {offered}",
        lib.display()
      ),
    );
  };
  let same = parse_version(&embedded, "the staged app's version")
    .is_ok_and(|v| compare_versions(&v, &offered_v).is_eq());
  if !same {
    return err(
      Code::VersionMismatch,
      format!(
        "the staged app is version {embedded}, but the manifest offers \
         {offered}"
      ),
    );
  }
  if layout.kind == InstallKind::MacBundle
    && let Ok(xml) =
      std::fs::read_to_string(staged.join("Contents").join("Info.plist"))
    && let Some(short) = plist_string(&xml, "CFBundleShortVersionString")
  {
    let want = [offered_v.major, offered_v.minor, offered_v.patch];
    if numeric_core(&short) != Some(want) {
      return err(
        Code::VersionMismatch,
        format!(
          "the staged bundle's CFBundleShortVersionString is {short}, but \
           the manifest offers {offered}"
        ),
      );
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A fake runtime library: noise, the bare magic (as the section lookup's
  /// code has it), then the data section.
  fn library(metadata: &serde_json::Value, noise: usize) -> Vec<u8> {
    let mut out: Vec<u8> = (0..noise).map(|i| (i % 251) as u8).collect();
    out.extend_from_slice(b"find_section(\"d3n0l4nd\")");
    out.extend_from_slice(&[0xff; 37]);
    let json = serde_json::to_vec(metadata).unwrap();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&[0u8; 64]);
    out.extend_from_slice(MAGIC);
    out
  }

  fn metadata(version: Option<&str>) -> serde_json::Value {
    let mut m = serde_json::json!({
      "argv": [],
      "entrypoint_key": "file:///main.ts",
      "seed": null,
    });
    if let Some(v) = version {
      m["app_version"] = v.into();
    }
    m
  }

  #[test]
  fn finds_the_metadata_wherever_the_chunks_split_it() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("lib");
    for noise in [0, 1, 7, 8, 9, 100, 4093] {
      std::fs::write(&path, library(&metadata(Some("2.0.0")), noise)).unwrap();
      for chunk in [8, 9, 13, 64, 4096] {
        assert_eq!(
          read_metadata_with(&path, chunk).unwrap(),
          Some(EmbeddedMetadata {
            app_version: Some("2.0.0".into())
          }),
          "noise {noise} chunk {chunk}"
        );
      }
    }
    std::fs::write(&path, library(&metadata(None), 10)).unwrap();
    assert_eq!(
      read_metadata(&path).unwrap(),
      Some(EmbeddedMetadata { app_version: None })
    );
    // Only the bare magic, or a section whose JSON is not the metadata.
    std::fs::write(&path, b"xx d3n0l4nd yy d3n0l4nd\x05\0\0\0\0\0\0\0{}{}{}")
      .unwrap();
    assert_eq!(read_metadata(&path).unwrap(), None);
    let mut lib = library(&serde_json::json!({ "app_version": "9.9.9" }), 3);
    lib.truncate(lib.len() - 1);
    std::fs::write(&path, lib).unwrap();
    assert_eq!(read_metadata(&path).unwrap(), None);
  }

  #[test]
  fn plist_values() {
    let xml = "<dict>\n  <key>CFBundleShortVersionString</key>\n  \
               <string>2.3.4</string>\n  <key>CFBundleVersion</key>\n  \
               <string>7</string>\n</dict>";
    assert_eq!(
      plist_string(xml, "CFBundleShortVersionString").as_deref(),
      Some("2.3.4")
    );
    assert_eq!(plist_string(xml, "CFBundleVersion").as_deref(), Some("7"));
    assert_eq!(plist_string(xml, "CFBundleName"), None);
    assert_eq!(numeric_core("2.3.4"), Some([2, 3, 4]));
    assert_eq!(numeric_core("1.0"), Some([1, 0, 0]));
    assert_eq!(numeric_core("1.2.3.4"), None);
    assert_eq!(numeric_core("1.x"), None);
  }

  /// A staged app directory shaped like `layout`'s, with the runtime
  /// library carrying `version` and (macOS) an Info.plist saying `short`.
  fn staged_app(
    t: &tempfile::TempDir,
    kind: InstallKind,
    version: Option<&str>,
    short: &str,
  ) -> (InstallLayout, PathBuf) {
    let parent = t.path().to_path_buf();
    let (name, exe_rel) = match kind {
      InstallKind::MacBundle => {
        ("App.app", PathBuf::from("Contents/MacOS/laufey_webview"))
      }
      _ if cfg!(windows) => ("App", PathBuf::from("App.exe")),
      _ => ("App", PathBuf::from("App")),
    };
    let layout = InstallLayout {
      kind,
      install: parent.join(name),
      parent: parent.clone(),
      name: name.into(),
      exe_rel,
    };
    let staged = layout.extract_dir().join(name);
    let exe = layout.exe_in(&staged);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"exe").unwrap();
    let lib = runtime_libraries(kind, &exe).pop().unwrap();
    std::fs::write(&lib, library(&metadata(version), 100)).unwrap();
    if kind == InstallKind::MacBundle {
      std::fs::write(
        staged.join("Contents/Info.plist"),
        format!(
          "<plist><dict><key>CFBundleShortVersionString</key>\
           <string>{short}</string></dict></plist>"
        ),
      )
      .unwrap();
    }
    (layout, staged)
  }

  #[test]
  fn the_staged_version_must_be_the_offered_one() {
    for kind in [InstallKind::AppDir, InstallKind::MacBundle] {
      let t = tempfile::tempdir().unwrap();
      let (layout, staged) = staged_app(&t, kind, Some("2.0.0-rc.1"), "2.0.0");
      check_embedded_version(&layout, &staged, "2.0.0-rc.1").unwrap();
      check_embedded_version(&layout, &staged, "2.0.0-rc.1+build.5").unwrap();
      for offered in ["2.0.0", "3.0.0", "2.0.0-rc.2"] {
        let e = check_embedded_version(&layout, &staged, offered).unwrap_err();
        assert_eq!(e.code, Code::VersionMismatch, "{kind:?} {offered}");
      }
      // Built without a version, or no metadata at all.
      let t = tempfile::tempdir().unwrap();
      let (layout, staged) = staged_app(&t, kind, None, "2.0.0");
      let e = check_embedded_version(&layout, &staged, "2.0.0").unwrap_err();
      assert_eq!(e.code, Code::VersionMismatch);
      for lib in runtime_libraries(kind, &layout.exe_in(&staged)) {
        let _ = std::fs::write(&lib, b"not a runtime");
      }
      let e = check_embedded_version(&layout, &staged, "2.0.0").unwrap_err();
      assert_eq!(e.code, Code::VersionMismatch);
      assert!(
        e.message.contains("no compiled app metadata"),
        "{}",
        e.message
      );
    }
    // macOS: the Info.plist must agree too.
    let t = tempfile::tempdir().unwrap();
    let (layout, staged) =
      staged_app(&t, InstallKind::MacBundle, Some("2.0.0"), "1.9.0");
    let e = check_embedded_version(&layout, &staged, "2.0.0").unwrap_err();
    assert_eq!(e.code, Code::VersionMismatch);
    assert!(e.message.contains("CFBundleShortVersionString"));
  }

  #[test]
  fn appimages_are_not_read() {
    let t = tempfile::tempdir().unwrap();
    let layout = InstallLayout {
      kind: InstallKind::AppImage,
      install: t.path().join("App.AppImage"),
      parent: t.path().to_path_buf(),
      name: "App.AppImage".into(),
      exe_rel: PathBuf::new(),
    };
    check_embedded_version(&layout, &t.path().join("x"), "2.0.0").unwrap();
  }
}
