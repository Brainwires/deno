// Copyright 2018-2026 the Deno authors. MIT license.

//! The tree manifest of a Windows app: every file but the executable that
//! carries it, and the app's identity and version, under the executable's
//! Authenticode signature (5.0-gate review A, M2; the denext.8 audit, F2).
//!
//! On Windows the OS check ([`super::oscheck::verify_windows`]) verifies the
//! Authenticode signature of every PE image of a staged app. Every other file
//! (`laufey-launch.json` with the window's bridge origins and inspectable
//! policy, CEF's `.pak` / `icudtl.dat` / `v8_context_snapshot.bin`, sidecar
//! JavaScript, `.deno-desktop-app`) carries no signature of its own, so the
//! update key alone protected it: whoever held that key could change them
//! under an app the signing identity never saw. And the OS check says only
//! who signed each PE image, not which release or which product it came
//! from: with the loose files alone listed, that key could still combine the
//! publisher's signed files (one release's runtime DLL, which carries the
//! version, under another release's executable and loose files, or another
//! product's) into an app every check passed.
//!
//! So the packager writes a manifest, `{ "v": 1, "appId", "version",
//! "files": [{ "path", "size", "sha256" }] }` (paths relative to the app
//! directory, `/`-separated), of every regular file but the executable
//! itself, PE images included (the DLLs as signed, so the packager signs them
//! first), into the main executable as the `RT_RCDATA` resource
//! [`RESOURCE_NAME`] BEFORE it signs it: the executable's Authenticode
//! signature covers the manifest, and with it everything else. At update
//! time, once the OS check passed, the manifest must name this app
//! (`appId`: the identifier the running app was built with, and the one its
//! own manifest names) and the version offered (`version`), and the staged
//! tree must match it exactly ([`compare`]): every file listed with the
//! same size and SHA-256, none missing, none unlisted, the executable
//! unlisted (it cannot hold its own hash: its signature is its check), and
//! no symlink or reparse point anywhere. The tree is read by the same walk
//! that records the digest the helper re-checks before it swaps
//! ([`super::staging::Staged::listing`]), and the manifest is read from the
//! very executable bytes that walk hashed ([`verify_staged_tree`]).
//!
//! No downgrade of protection: when the RUNNING executable carries a
//! manifest, a staged app without one is refused. A running app without one
//! (a release from before the manifest) accepts its first update without
//! it, so the manifest rolls out in one release.

#![allow(
  clippy::disallowed_methods,
  reason = "reads the staged app next to the install, outside any user \
            permission sandbox, by design"
)]

use std::collections::BTreeMap;
use std::path::Path;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;

/// The `RT_RCDATA` resource the manifest is stored in.
pub const RESOURCE_NAME: &str = "DENEXT_TREE";

/// The largest manifest read (a bundle lists a few hundred files).
pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

/// The largest executable read whole to find the manifest (the app's
/// executable is the small backend binary; the runtime is a DLL beside it).
pub const MAX_EXE_BYTES: u64 = 512 * 1024 * 1024;

/// One entry of a staged tree, as the digest walk read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEntry {
  /// A regular file: its size, SHA-256 (lowercase hex), and whether it has
  /// a PE image's headers ([`super::oscheck::pe_files`]'s test).
  File {
    rel: String,
    size: u64,
    sha256: String,
    pe: bool,
  },
  /// A symlink (on Windows also a junction / name-surrogate reparse point).
  Symlink { rel: String },
}

/// Whether a file's bytes have a PE image's headers, decided from its bytes
/// as they stream past: `MZ` at 0 and `PE\0\0` at the offset the DOS
/// header's `e_lfanew` names. The same test as `oscheck::is_pe_image`.
#[derive(Debug, Default)]
pub struct PeProbe {
  head: Vec<u8>,
  pos: u64,
  sig: [u8; 4],
  sig_seen: u8,
}

impl PeProbe {
  fn sig_at(&self) -> Option<u64> {
    (self.head.len() == 64).then(|| {
      u64::from(u32::from_le_bytes([
        self.head[60],
        self.head[61],
        self.head[62],
        self.head[63],
      ]))
    })
  }

  pub fn feed(&mut self, buf: &[u8]) {
    let start = self.pos;
    if self.head.len() < 64 {
      let n = (64 - self.head.len()).min(buf.len());
      self.head.extend_from_slice(&buf[..n]);
    }
    if let Some(at) = self.sig_at() {
      for i in 0..4u64 {
        let p = at + i;
        if p >= start && p - start < buf.len() as u64 {
          self.sig[i as usize] = buf[(p - start) as usize];
          self.sig_seen |= 1 << i;
        }
      }
    }
    self.pos = start + buf.len() as u64;
  }

  pub fn is_pe(&self) -> bool {
    let Some(at) = self.sig_at() else {
      return false;
    };
    if &self.head[..2] != b"MZ" {
      return false;
    }
    // A signature inside the DOS header is read from it; one past it, from
    // the bytes recorded as they streamed past.
    (0..4u64).all(|i| {
      let p = at + i;
      let byte = if p < 64 {
        Some(self.head[p as usize])
      } else if self.sig_seen & (1 << i) != 0 {
        Some(self.sig[i as usize])
      } else {
        None
      };
      byte == Some(b"PE\0\0"[i as usize])
    })
  }
}

/// A file's bytes as the digest walk reads them: SHA-256, length and the
/// PE probe in one pass.
#[derive(Default)]
pub struct FileSink {
  hash: sha2::Sha256,
  probe: PeProbe,
}

impl std::io::Write for FileSink {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    sha2::Digest::update(&mut self.hash, buf);
    self.probe.feed(buf);
    Ok(buf.len())
  }
  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

impl FileSink {
  /// The SHA-256 (lowercase hex) and whether the file is a PE image.
  pub fn finish(self) -> (String, bool) {
    let pe = self.probe.is_pe();
    (
      faster_hex::hex_string(&sha2::Digest::finalize(self.hash)),
      pe,
    )
  }
}

/// A parsed manifest: the app's identifier and version, and path → (size,
/// SHA-256).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeManifest {
  app_id: String,
  version: String,
  files: BTreeMap<String, (u64, String)>,
}

impl TreeManifest {
  /// The `desktop.app.identifier` of the app the manifest was written for.
  pub fn app_id(&self) -> &str {
    &self.app_id
  }

  /// The version of the app the manifest was written for.
  pub fn version(&self) -> &str {
    &self.version
  }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestJson {
  v: u32,
  #[serde(rename = "appId")]
  app_id: String,
  version: String,
  files: Vec<FileJson>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileJson {
  path: String,
  size: u64,
  sha256: String,
}

/// Why `path` cannot be a manifest path: relative, `/`-separated, no empty,
/// `.` or `..` component, none of `\`, `:` or NUL.
fn path_problem(path: &str) -> Option<&'static str> {
  if path.is_empty() {
    return Some("an empty path");
  }
  if path.contains(['\\', ':', '\0']) {
    return Some("a path with `\\`, `:` or NUL");
  }
  if path
    .split('/')
    .any(|c| c.is_empty() || c == "." || c == "..")
  {
    return Some("a path that is absolute or has an empty, `.` or `..` part");
  }
  None
}

fn is_sha256_hex(s: &str) -> bool {
  s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parse a manifest's bytes (strict: version 1, known fields only, an app
/// identifier and a semantic version, valid unique paths, lowercase-hex
/// SHA-256s).
pub fn parse_manifest(bytes: &[u8]) -> Result<TreeManifest, String> {
  if bytes.len() > MAX_MANIFEST_BYTES {
    return Err(format!("larger than {MAX_MANIFEST_BYTES} bytes"));
  }
  let json: ManifestJson =
    serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
  if json.v != 1 {
    return Err(format!("unknown version {}", json.v));
  }
  if json.app_id.is_empty()
    || json
      .app_id
      .chars()
      .any(|c| c.is_control() || c.is_whitespace())
  {
    return Err(format!("{:?} is not an app identifier", json.app_id));
  }
  super::manifest::parse_version(&json.version, "the manifest's version")
    .map_err(|e| e.message)?;
  let mut files = BTreeMap::new();
  for f in json.files {
    if let Some(why) = path_problem(&f.path) {
      return Err(format!("{why}: {:?}", f.path));
    }
    if !is_sha256_hex(&f.sha256) {
      return Err(format!("{}: not a lowercase hex SHA-256", f.path));
    }
    if files.insert(f.path.clone(), (f.size, f.sha256)).is_some() {
      return Err(format!("{} is listed twice", f.path));
    }
  }
  Ok(TreeManifest {
    app_id: json.app_id,
    version: json.version,
    files,
  })
}

/// The [`RESOURCE_NAME`] `RT_RCDATA` resource of a PE image, if it has one.
/// An image that does not parse, or a resource in more than one language,
/// is an error.
pub fn read_manifest_resource(pe: &[u8]) -> Result<Option<Vec<u8>>, String> {
  use editpe::ResourceEntry;
  use editpe::ResourceEntryName;
  use editpe::constants::RT_RCDATA;
  let image = editpe::Image::parse(pe).map_err(|e| e.to_string())?;
  let Some(dir) = image.resource_directory() else {
    return Ok(None);
  };
  let Some(rcdata) = dir
    .root()
    .get(ResourceEntryName::ID(RT_RCDATA as u32))
    .and_then(ResourceEntry::as_table)
  else {
    return Ok(None);
  };
  // Resource names are case-insensitive (FindResource upper-cases them).
  let mut found = rcdata.entries().into_iter().filter(|n| {
    n.to_string()
      .is_some_and(|s| s.eq_ignore_ascii_case(RESOURCE_NAME))
  });
  let Some(name) = found.next() else {
    return Ok(None);
  };
  if found.next().is_some() {
    return Err(format!("more than one {RESOURCE_NAME} resource"));
  }
  let Some(langs) = rcdata.get(name).and_then(ResourceEntry::as_table) else {
    return Err(format!("the {RESOURCE_NAME} resource is not a directory"));
  };
  let entries = langs.entries();
  let [lang] = entries.as_slice() else {
    return Err(format!(
      "the {RESOURCE_NAME} resource must have exactly one language"
    ));
  };
  match langs.get(*lang).and_then(ResourceEntry::as_data) {
    Some(data) => Ok(Some(data.data().to_vec())),
    None => Err(format!("the {RESOURCE_NAME} resource holds no data")),
  }
}

/// Compare a staged tree with its manifest: every regular file but the
/// executable `exe_rel` (the manifest's carrier) listed with its size and
/// SHA-256. See the module docs.
pub fn compare(
  manifest: &TreeManifest,
  exe_rel: &str,
  entries: &[TreeEntry],
) -> Result<(), String> {
  if manifest.files.contains_key(exe_rel) {
    return Err(format!("{exe_rel} is listed in its own manifest"));
  }
  let mut staged: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
  let mut seen = std::collections::BTreeSet::new();
  for e in entries {
    match e {
      TreeEntry::Symlink { rel } => {
        return Err(format!("{rel} is a symlink or reparse point"));
      }
      TreeEntry::File {
        rel, size, sha256, ..
      } => {
        // Two names read as one (a name that is not valid Unicode) would
        // hide a file from the comparison.
        if !seen.insert(rel.as_str()) {
          return Err(format!("{rel} appears twice in the staged app"));
        }
        if rel != exe_rel {
          staged.insert(rel, (*size, sha256));
        }
      }
    }
  }
  for (path, (size, sha)) in &manifest.files {
    match staged.remove(path.as_str()) {
      None => {
        return Err(format!("{path} is listed but missing in the staged app"));
      }
      Some((s, h)) if s != *size || h != sha => {
        return Err(format!("{path} differs from the signed manifest"));
      }
      Some(_) => {}
    }
  }
  if let Some(extra) = staged.keys().next() {
    return Err(format!("{extra} is not in the signed manifest"));
  }
  Ok(())
}

/// `rel` as the digest walk spells it: components joined by `/`.
pub fn walk_rel(rel: &Path) -> String {
  rel
    .components()
    .map(|c| c.as_os_str().to_string_lossy().into_owned())
    .collect::<Vec<_>>()
    .join("/")
}

/// Read a whole executable (bounded), for its manifest.
fn read_exe(path: &Path) -> Result<Vec<u8>, UpdateError> {
  use std::io::Read;
  let f = std::fs::File::open(path)
    .map_err(|e| UpdateError::io(path.display(), e))?;
  let mut bytes = Vec::new();
  f.take(MAX_EXE_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|e| UpdateError::io(path.display(), e))?;
  if bytes.len() as u64 > MAX_EXE_BYTES {
    return err(
      Code::OsSignature,
      format!("{} is too large to read its tree manifest", path.display()),
    );
  }
  Ok(bytes)
}

/// The Windows tree check of a staged app (see the module docs), after its
/// OS check passed. `running_exe` is the installed app's executable,
/// `staged` the staged app's directory, `exe_rel` its executable relative to
/// that directory (`/`-separated), `entries` the staged tree as the digest
/// walk read it, `app_id` this app's identifier when the caller knows it
/// (`stage()` from the updater's configuration, the helper from the
/// installed app's compiled metadata; the running executable's own manifest
/// is compared as well), and `version` the version offered.
pub fn verify_staged_tree(
  running_exe: &Path,
  staged: &Path,
  exe_rel: &str,
  entries: &[TreeEntry],
  app_id: Option<&str>,
  version: Option<&str>,
) -> Result<(), UpdateError> {
  let refuse = |why: String| {
    err::<()>(Code::OsSignature, format!("the staged app's files: {why}"))
  };
  // The running executable's manifest, if it carries one (a release from
  // before the manifest carries none): parsed in full, so one that can't
  // be read fails closed like unreadable resources.
  let running = match read_manifest_resource(&read_exe(running_exe)?) {
    Ok(Some(raw)) => match parse_manifest(&raw) {
      Ok(m) => Some(m),
      Err(e) => {
        return refuse(format!(
          "the running executable's tree manifest can't be read ({e})"
        ));
      }
    },
    Ok(None) => None,
    Err(e) => {
      return refuse(format!(
        "the running executable's resources can't be read ({e})"
      ));
    }
  };
  let walked = entries.iter().find_map(|e| match e {
    TreeEntry::File { rel, sha256, .. } if rel == exe_rel => Some(sha256),
    _ => None,
  });
  if walked.is_none() {
    // Not in the walk under its exact name. A new version that changes only
    // the case of the executable's name is found by the case-insensitive
    // file system (reading `exe_rel` below would read it), but the walk
    // spells it as packed: say so, rather than that it changed.
    let names = entries.iter().filter_map(|e| match e {
      TreeEntry::File { rel, .. } => Some(rel.as_str()),
      TreeEntry::Symlink { .. } => None,
    });
    if let Some(found) = case_only_rename(exe_rel, names) {
      return err(
        Code::BundleMismatch,
        case_only_rename_message(exe_rel, found),
      );
    }
    return refuse(format!("{exe_rel} is not in the staged app"));
  }
  // The manifest is read from bytes whose SHA-256 must be the one the walk
  // recorded: the executable checked is the one the digest pins.
  let exe_path = staged.join(exe_rel);
  let bytes = read_exe(&exe_path)?;
  let sha = {
    use sha2::Digest;
    faster_hex::hex_string(&sha2::Sha256::digest(&bytes))
  };
  if walked != Some(&sha) {
    return refuse(format!("{exe_rel} changed while the update was checked"));
  }
  let staged_manifest = match read_manifest_resource(&bytes) {
    Ok(m) => m,
    Err(e) => return refuse(format!("{exe_rel}'s tree manifest: {e}")),
  };
  let Some(raw) = staged_manifest else {
    if running.is_some() {
      return refuse(format!(
        "{exe_rel} carries no signed tree manifest ({RESOURCE_NAME}), and \
         the running app does: its files would be protected by the update \
         key alone"
      ));
    }
    return Ok(());
  };
  let manifest = match parse_manifest(&raw) {
    Ok(m) => m,
    Err(e) => return refuse(format!("the signed tree manifest: {e}")),
  };
  // The manifest names this app: the identifier the caller knows, and the
  // one the running executable's own manifest names.
  if let Some(id) = app_id
    && manifest.app_id != id
  {
    return refuse(format!(
      "the signed tree manifest is for app {:?}, this app is {id:?}",
      manifest.app_id
    ));
  }
  if let Some(running) = &running
    && running.app_id != manifest.app_id
  {
    return refuse(format!(
      "the signed tree manifest is for app {:?}, the running app's names {:?}",
      manifest.app_id, running.app_id
    ));
  }
  // And the version offered: a manifest of another release of this app,
  // under this release's executable, is another release's file set.
  let Some(version) = version else {
    return refuse("no version is offered to compare the manifest with".into());
  };
  let same_version = super::manifest::parse_version(version, "version")
    .and_then(|offered| {
      super::manifest::parse_version(
        &manifest.version,
        "the manifest's version",
      )
      .map(|m| super::manifest::compare_versions(&m, &offered).is_eq())
    });
  if !same_version.unwrap_or(false) {
    return refuse(format!(
      "the signed tree manifest is for version {}, the update offers {version}",
      manifest.version
    ));
  }
  compare(&manifest, exe_rel, entries).or_else(refuse)
}

/// The name in `names` that is `want` with only its case changed, if there is
/// exactly one (`/`-separated paths relative to the app directory).
pub fn case_only_rename<'a>(
  want: &str,
  names: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
  let folded = want.to_lowercase();
  let mut found = names
    .into_iter()
    .filter(|n| *n != want && n.to_lowercase() == folded);
  let first = found.next()?;
  found.next().is_none().then_some(first)
}

/// Why an update whose executable's name changed only in case is refused.
///
/// It is refused rather than accepted as the same file (which Windows'
/// case-insensitive file system would open): the install is launched by the
/// name it was installed under (its shortcuts, its registration, the
/// helper's relaunch), a directory with per-directory case sensitivity on
/// (`fsutil file setCaseSensitiveInfo`, which WSL turns on) would then hold
/// no executable of that name, and an app that can't start never runs its
/// trial, so it would not roll back either. An update keeps the name.
pub fn case_only_rename_message(exe_rel: &str, found: &str) -> String {
  format!(
    "the update renames the app's executable {exe_rel} to {found} (only the \
     case differs); an update must keep the executable's name exactly, as \
     the install is launched by it (package the new version with the name \
     {exe_rel})"
  )
}

/// [`verify_staged_tree`] for this platform: on Windows, against the
/// install's executable; elsewhere nothing (macOS seals every file of the
/// bundle in its code signature; Linux has no OS signature).
pub fn verify_for_platform(
  layout: &super::layout::InstallLayout,
  staged: &Path,
  entries: &[TreeEntry],
  app_id: Option<&str>,
  version: Option<&str>,
) -> Result<(), UpdateError> {
  if !cfg!(windows) {
    return Ok(());
  }
  verify_staged_tree(
    &layout.exe(),
    staged,
    &walk_rel(&layout.exe_rel),
    entries,
    app_id,
    version,
  )
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  fn sha(b: &[u8]) -> String {
    use sha2::Digest;
    faster_hex::hex_string(&sha2::Sha256::digest(b))
  }

  fn file(rel: &str, body: &[u8], pe: bool) -> TreeEntry {
    TreeEntry::File {
      rel: rel.into(),
      size: body.len() as u64,
      sha256: sha(body),
      pe,
    }
  }

  /// The fixture app every manifest here is written for.
  const APP_ID: &str = "com.example.app";
  /// The version those manifests (and the updates offering them) carry.
  const VERSION: &str = "2.0.0";

  fn manifest(files: &[(&str, &[u8])]) -> TreeManifest {
    parse_manifest(&manifest_json(files)).unwrap()
  }

  #[test]
  fn the_probe_matches_the_pe_header_test() {
    let mut pe = vec![0u8; 0x200];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    let probe = |bytes: &[u8], chunk: usize| {
      let mut p = PeProbe::default();
      for c in bytes.chunks(chunk.max(1)) {
        p.feed(c);
      }
      p.is_pe()
    };
    for chunk in [1, 3, 63, 64, 65, 0x81, 4096] {
      assert!(probe(&pe, chunk), "chunk {chunk}");
    }
    // The signature inside the DOS header (e_lfanew < 64; it cannot
    // straddle the header's end without overlapping e_lfanew itself).
    let mut low = vec![0u8; 80];
    low[..2].copy_from_slice(b"MZ");
    low[0x3c..0x40].copy_from_slice(&0x20u32.to_le_bytes());
    low[0x20..0x24].copy_from_slice(b"PE\0\0");
    assert!(probe(&low, 7));
    // Not PE: no MZ, a short file, a signature past the end, a wrong one.
    assert!(!probe(b"{\"bridgeOrigins\":[]}", 4));
    assert!(!probe(&pe[..64], 64));
    assert!(!probe(&pe[..0x82], 64));
    let mut bad = pe.clone();
    bad[0x83] = 1;
    assert!(!probe(&bad, 64));
    let mut no_mz = pe.clone();
    no_mz[0] = b'Z';
    assert!(!probe(&no_mz, 64));
  }

  #[test]
  fn manifests_are_parsed_strictly() {
    // `{"v":1,"appId":"com.example.app","version":"2.0.0","files":[...]}`.
    let with = |files: &str| {
      format!(
        r#"{{"v":1,"appId":"{APP_ID}","version":"{VERSION}","files":[{files}]}}"#
      )
    };
    let ok = with(&format!(
      r#"{{"path":"a/b.json","size":2,"sha256":"{}"}}"#,
      sha(b"{}")
    ));
    let m = parse_manifest(ok.as_bytes()).unwrap();
    assert_eq!(m.files.len(), 1);
    assert_eq!(m.app_id(), APP_ID);
    assert_eq!(m.version(), VERSION);
    let h = sha(b"x");
    let entry = |path: &str, sha: &str| {
      format!(r#"{{"path":"{path}","size":1,"sha256":"{sha}"}}"#)
    };
    for bad in [
      with("").replace(r#""v":1"#, r#""v":2"#),
      with("").replace("]}", r#"],"extra":1}"#),
      r#"{"v":1}"#.to_string(),
      "not json".to_string(),
      // The unreleased draft without the app and version, an empty or
      // odd identifier, a version that is not one.
      r#"{"v":1,"files":[]}"#.to_string(),
      with("").replace(r#""appId":"com.example.app","#, ""),
      with("").replace(r#""version":"2.0.0","#, ""),
      with("").replace(APP_ID, ""),
      with("").replace(APP_ID, "com.example app"),
      with("").replace(VERSION, "latest"),
      with(&entry("/abs", &h)),
      with(&entry("a/../b", &h)),
      with(&entry("a//b", &h)),
      with(&entry("./a", &h)),
      with(&entry("a\\\\b", &h)),
      with(&entry("a:b", &h)),
      with(&entry("", &h)),
      with(&entry("a", &h.to_uppercase())),
      with(&entry("a", "abc")),
      with(&format!("{},{}", entry("a", &h), entry("a", &h))),
      with(&entry("a", &h).replace("}", r#","x":1}"#)),
    ] {
      assert!(parse_manifest(bad.as_bytes()).is_err(), "{bad}");
    }
    assert!(parse_manifest(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
  }

  #[test]
  fn the_tree_must_match_the_manifest_exactly() {
    let launch: &[u8] = br#"{"bridgeOrigins":["app://x"]}"#;
    let pak: &[u8] = b"PAK";
    let dll: &[u8] = b"MZ..";
    let m = manifest(&[
      ("App.dll", dll),
      ("laufey-launch.json", launch),
      ("res/a.pak", pak),
    ]);
    let good = vec![
      file("App.exe", b"MZ..", true),
      file("App.dll", dll, true),
      file("laufey-launch.json", launch, false),
      file("res/a.pak", pak, false),
    ];
    compare(&m, "App.exe", &good).unwrap();

    // Changed (content, same size), resized, missing, extra.
    let mut changed = good.clone();
    changed[2] = file(
      "laufey-launch.json",
      br#"{"bridgeOrigins":["evil://"]}"#,
      false,
    );
    assert!(
      compare(&m, "App.exe", &changed)
        .unwrap_err()
        .contains("differs")
    );
    let mut flipped = good.clone();
    let mut b = launch.to_vec();
    b[3] ^= 1;
    flipped[2] = file("laufey-launch.json", &b, false);
    assert!(
      compare(&m, "App.exe", &flipped)
        .unwrap_err()
        .contains("differs")
    );
    let missing: Vec<_> = good
      .iter()
      .filter(
        |e| !matches!(e, TreeEntry::File { rel, .. } if rel == "res/a.pak"),
      )
      .cloned()
      .collect();
    assert!(
      compare(&m, "App.exe", &missing)
        .unwrap_err()
        .contains("missing")
    );
    let mut extra = good.clone();
    extra.push(file("sidecar/evil.js", b"x", false));
    assert!(
      compare(&m, "App.exe", &extra)
        .unwrap_err()
        .contains("not in the signed")
    );
    // Whether a listed file has a PE header makes no difference: its bytes
    // are what is listed.
    let mut as_pe = good.clone();
    as_pe[3] = file("res/a.pak", pak, true);
    compare(&m, "App.exe", &as_pe).unwrap();
    // A symlink anywhere, and a name read twice.
    let mut link = good.clone();
    link.push(TreeEntry::Symlink {
      rel: "res/link".into(),
    });
    assert!(
      compare(&m, "App.exe", &link)
        .unwrap_err()
        .contains("symlink")
    );
    let mut twice = good.clone();
    twice.push(file("res/a.pak", pak, false));
    assert!(
      compare(&m, "App.exe", &twice)
        .unwrap_err()
        .contains("twice")
    );
    // The executable is the manifest's carrier: never listed, and a
    // manifest that lists it is not one the packager wrote.
    let listed = manifest(&[("App.exe", b"MZ.."), ("App.dll", dll)]);
    assert!(
      compare(&listed, "App.exe", &good[..2])
        .unwrap_err()
        .contains("own manifest")
    );
  }

  /// The manifest binds the app, the version and every file but the
  /// executable that carries it, PE images included: with only the loose
  /// files listed, whoever held the update key could serve any mix of the
  /// publisher's signed builds (one release's runtime DLL under another's
  /// executable and loose files, or another product's), and every PE image
  /// would pass the OS check while the loose files matched the manifest of
  /// the executable they came with.
  #[test]
  fn the_manifest_binds_the_app_version_and_every_file_but_the_executable() {
    let launch: &[u8] = br#"{"bridgeOrigins":["app://x"]}"#;
    let dll: &[u8] = b"MZ.runtime";
    // The draft without `appId` / `version` (never released) is refused.
    assert!(parse_manifest(br#"{"v":1,"files":[]}"#).is_err());
    let m =
      manifest(&[("App.runtime.dll", dll), ("laufey-launch.json", launch)]);
    assert_eq!((m.app_id(), m.version()), (APP_ID, VERSION));
    let good = vec![
      file("App.exe", b"MZ..", true),
      file("App.runtime.dll", dll, true),
      file("laufey-launch.json", launch, false),
    ];
    compare(&m, "App.exe", &good).unwrap();
    // The runtime DLL of another release (signed by the same publisher):
    // refused.
    let mut other = good.clone();
    other[1] = file("App.runtime.dll", b"MZ.other", true);
    assert!(
      compare(&m, "App.exe", &other)
        .unwrap_err()
        .contains("differs")
    );
    // A PE image the manifest does not list (another product's DLL, an
    // addon): refused.
    let mut extra = good.clone();
    extra.push(file("addon.node", b"MZ", true));
    assert!(
      compare(&m, "App.exe", &extra)
        .unwrap_err()
        .contains("not in the signed")
    );

    // Over real files: the manifest must name this app and the version
    // offered, and agree with the running executable's own manifest.
    let t = tempfile::tempdir().unwrap();
    let staged = t.path().join("App");
    let signed = pe_with_resource(
      RESOURCE_NAME,
      &manifest_json(&[("laufey-launch.json", launch)]),
      &[1033],
    );
    write(&staged, "App.exe", &signed);
    write(&staged, "laufey-launch.json", launch);
    let running = t.path().join("running.exe");
    std::fs::write(&running, &signed).unwrap();
    let entries = super::super::swap::tree_listing(&staged).unwrap().1;
    let check = |app_id: Option<&str>, version: Option<&str>| {
      verify_staged_tree(
        &running, &staged, "App.exe", &entries, app_id, version,
      )
    };
    check(Some(APP_ID), Some(VERSION)).unwrap();
    // The helper may not know the app's identifier (no compiled metadata):
    // the running executable's manifest still has to agree.
    check(None, Some(VERSION)).unwrap();
    for (app_id, version, needle) in [
      (Some("com.example.other"), Some(VERSION), "for app"),
      (Some(APP_ID), Some("3.0.0"), "for version"),
      (Some(APP_ID), None, "no version"),
      (Some(APP_ID), Some("latest"), "for version"),
    ] {
      let e = check(app_id, version).unwrap_err();
      assert_eq!(e.code, Code::OsSignature, "{}", e.message);
      assert!(e.message.contains(needle), "{}", e.message);
    }
    // A running app whose own manifest names another app: refused, whatever
    // the caller knows.
    let other =
      String::from_utf8(manifest_json(&[("laufey-launch.json", launch)]))
        .unwrap()
        .replace(APP_ID, "com.example.other");
    std::fs::write(
      &running,
      pe_with_resource(RESOURCE_NAME, other.as_bytes(), &[1033]),
    )
    .unwrap();
    let e = check(None, Some(VERSION)).unwrap_err();
    assert!(e.message.contains("running app's names"), "{}", e.message);
    // A running manifest that can't be parsed (the unreleased draft) fails
    // closed.
    std::fs::write(
      &running,
      pe_with_resource(RESOURCE_NAME, br#"{"v":1,"files":[]}"#, &[1033]),
    )
    .unwrap();
    let e = check(Some(APP_ID), Some(VERSION)).unwrap_err();
    assert!(e.message.contains("can't be read"), "{}", e.message);
  }

  /// A minimal PE32+ image (one `.text` section), as in the bundler's tests.
  pub(crate) fn minimal_pe() -> Vec<u8> {
    let mut b = vec![0u8; 0x400];
    let put16 = |b: &mut Vec<u8>, at: usize, v: u16| {
      b[at..at + 2].copy_from_slice(&v.to_le_bytes())
    };
    let put32 = |b: &mut Vec<u8>, at: usize, v: u32| {
      b[at..at + 4].copy_from_slice(&v.to_le_bytes())
    };
    b[0..2].copy_from_slice(b"MZ");
    put32(&mut b, 0x3c, 0x40);
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    put16(&mut b, coff, 0x8664);
    put16(&mut b, coff + 2, 1);
    put16(&mut b, coff + 16, 240);
    put16(&mut b, coff + 18, 0x0022);
    let opt = coff + 20;
    put16(&mut b, opt, 0x20b);
    put32(&mut b, opt + 4, 0x200);
    put32(&mut b, opt + 16, 0x1000);
    put32(&mut b, opt + 20, 0x1000);
    b[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    put32(&mut b, opt + 32, 0x1000);
    put32(&mut b, opt + 36, 0x200);
    put16(&mut b, opt + 40, 6);
    put16(&mut b, opt + 48, 6);
    put32(&mut b, opt + 56, 0x2000);
    put32(&mut b, opt + 60, 0x200);
    put16(&mut b, opt + 68, 2);
    put16(&mut b, opt + 70, 0x8160);
    for (i, v) in [0x100000u64, 0x1000, 0x100000, 0x1000].iter().enumerate() {
      b[opt + 72 + i * 8..opt + 80 + i * 8].copy_from_slice(&v.to_le_bytes());
    }
    put32(&mut b, opt + 108, 16);
    let sec = opt + 240;
    b[sec..sec + 5].copy_from_slice(b".text");
    put32(&mut b, sec + 8, 0x200);
    put32(&mut b, sec + 12, 0x1000);
    put32(&mut b, sec + 16, 0x200);
    put32(&mut b, sec + 20, 0x200);
    put32(&mut b, sec + 36, 0x6000_0020);
    b[0x200] = 0xc3;
    b
  }

  /// `minimal_pe` with `data` as the `RT_RCDATA` resource `name`, in each of
  /// `langs`.
  pub(crate) fn pe_with_resource(
    name: &str,
    data: &[u8],
    langs: &[u32],
  ) -> Vec<u8> {
    use editpe::ResourceData;
    use editpe::ResourceEntry;
    use editpe::ResourceEntryName;
    use editpe::ResourceTable;
    use editpe::constants::RT_RCDATA;
    let base = minimal_pe();
    let mut image = editpe::Image::parse(base.as_slice()).unwrap();
    let mut rd = editpe::ResourceDirectory::default();
    rd.set_manifest("<assembly manifestVersion=\"1.0\"/>")
      .unwrap();
    let mut lang_table = ResourceTable::default();
    for lang in langs {
      let mut d = ResourceData::default();
      d.set_data(data.to_vec());
      lang_table.insert(ResourceEntryName::ID(*lang), ResourceEntry::Data(d));
    }
    let mut names = ResourceTable::default();
    names.insert(
      ResourceEntryName::from_string(name),
      ResourceEntry::Table(lang_table),
    );
    rd.root_mut().insert(
      ResourceEntryName::ID(RT_RCDATA as u32),
      ResourceEntry::Table(names),
    );
    image.set_resource_directory(rd).unwrap();
    let mut out = Vec::new();
    image.write_writer(&mut out).unwrap();
    out
  }

  #[test]
  fn the_manifest_resource_round_trips() {
    let body = &manifest_json(&[]);
    let pe = pe_with_resource(RESOURCE_NAME, body, &[1033]);
    assert_eq!(
      read_manifest_resource(&pe).unwrap().as_deref(),
      Some(&body[..])
    );
    // Names are case-insensitive, as FindResource reads them.
    let lower = pe_with_resource("denext_tree", body, &[0]);
    assert_eq!(
      read_manifest_resource(&lower).unwrap().as_deref(),
      Some(&body[..])
    );
    // Another RT_RCDATA name, no resources at all: none.
    let other = pe_with_resource("OTHER", body, &[1033]);
    assert_eq!(read_manifest_resource(&other).unwrap(), None);
    assert_eq!(read_manifest_resource(&minimal_pe()).unwrap(), None);
    // Two languages are ambiguous; a non-PE is an error.
    let two = pe_with_resource(RESOURCE_NAME, body, &[0, 1033]);
    assert!(read_manifest_resource(&two).is_err());
    assert!(read_manifest_resource(b"{}").is_err());
  }

  /// An executable written by denext's packager (`src/build/pe-resources.ts`
  /// `writePeResources` over `minimal_pe`, with a version resource and the
  /// manifest `{"v":1,"appId":"com.example.app","version":"2.0.0","files":
  /// [{"path":"App.dll",...},{"path":"laufey-launch.json",...}]}`): the two
  /// implementations agree on where the manifest lives and what it holds.
  #[test]
  fn reads_a_manifest_the_packager_wrote() {
    let pe = include_bytes!("testdata/tree_manifest_packaged.exe");
    let raw = read_manifest_resource(pe).unwrap().unwrap();
    let m = parse_manifest(&raw).unwrap();
    assert_eq!((m.app_id(), m.version()), (APP_ID, VERSION));
    let launch = br#"{"bridgeOrigins":["app://x"]}"#;
    compare(
      &m,
      "App.exe",
      &[
        file("App.dll", b"MZ.dll", true),
        file("laufey-launch.json", launch, false),
      ],
    )
    .unwrap();
  }

  fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
  }

  pub(crate) fn manifest_json(files: &[(&str, &[u8])]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
      "v": 1,
      "appId": APP_ID,
      "version": VERSION,
      "files": files.iter().map(|(p, b)| serde_json::json!({
        "path": p, "size": b.len(), "sha256": sha(b),
      })).collect::<Vec<_>>(),
    }))
    .unwrap()
  }

  /// The whole check over real files: the staged executable's resource, the
  /// tree as the digest walk reads it, and the running app's manifest.
  #[test]
  fn verify_staged_tree_end_to_end() {
    let launch: &[u8] = br#"{"bridgeOrigins":["app://x"]}"#;
    let t = tempfile::tempdir().unwrap();
    let running = t.path().join("running.exe");
    let staged = t.path().join("App");
    let signed = pe_with_resource(
      RESOURCE_NAME,
      &manifest_json(&[("laufey-launch.json", launch)]),
      &[1033],
    );
    write(&staged, "App.exe", &signed);
    write(&staged, "laufey-launch.json", launch);
    let listing =
      |root: &Path| super::super::swap::tree_listing(root).unwrap().1;

    // A running app with a manifest, a matching staged app.
    std::fs::write(&running, &signed).unwrap();
    verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &listing(&staged),
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap();

    // One byte of the launch policy flipped: refused.
    let mut evil = launch.to_vec();
    evil[5] ^= 1;
    write(&staged, "laufey-launch.json", &evil);
    let e = verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &listing(&staged),
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
    write(&staged, "laufey-launch.json", launch);

    // An unlisted sidecar script: refused.
    write(&staged, "sidecar/x.js", b"evil()");
    assert!(
      verify_staged_tree(
        &running,
        &staged,
        "App.exe",
        &listing(&staged),
        Some(APP_ID),
        Some(VERSION),
      )
      .is_err()
    );
    std::fs::remove_dir_all(staged.join("sidecar")).unwrap();

    // The executable changed after the walk: refused.
    let walked = listing(&staged);
    write(&staged, "App.exe", &minimal_pe());
    assert!(
      verify_staged_tree(
        &running,
        &staged,
        "App.exe",
        &walked,
        Some(APP_ID),
        Some(VERSION)
      )
      .is_err()
    );

    // A staged app without a manifest under a running app with one: refused
    // (no downgrade of protection).
    let e = verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &listing(&staged),
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap_err();
    assert!(
      e.message.contains("no signed tree manifest"),
      "{}",
      e.message
    );

    // A running app without one accepts it (the first update that adds it).
    std::fs::write(&running, minimal_pe()).unwrap();
    verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &listing(&staged),
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap();
    // ... and a staged app that has one is held to it.
    write(&staged, "App.exe", &signed);
    write(&staged, "laufey-launch.json", &evil);
    assert!(
      verify_staged_tree(
        &running,
        &staged,
        "App.exe",
        &listing(&staged),
        Some(APP_ID),
        Some(VERSION),
      )
      .is_err()
    );

    // A running executable whose resources can't be read fails closed.
    std::fs::write(&running, b"not a PE").unwrap();
    write(&staged, "laufey-launch.json", launch);
    assert!(
      verify_staged_tree(
        &running,
        &staged,
        "App.exe",
        &listing(&staged),
        Some(APP_ID),
        Some(VERSION),
      )
      .is_err()
    );
  }

  /// A new version whose executable differs from the installed one only in
  /// the case of its name is refused with that reason (it used to read "changed
  /// while the update was checked", which it hadn't).
  #[test]
  fn a_case_only_rename_of_the_executable_is_refused_clearly() {
    let launch: &[u8] = b"{}";
    let t = tempfile::tempdir().unwrap();
    let staged = t.path().join("App");
    let signed = pe_with_resource(
      RESOURCE_NAME,
      &manifest_json(&[("laufey-launch.json", launch)]),
      &[1033],
    );
    write(&staged, "app.exe", &signed);
    write(&staged, "laufey-launch.json", launch);
    let running = t.path().join("running.exe");
    std::fs::write(&running, &signed).unwrap();
    let entries = super::super::swap::tree_listing(&staged).unwrap().1;
    let e = verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &entries,
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::BundleMismatch, "{}", e.message);
    assert!(
      e.message
        .contains("renames the app's executable App.exe to app.exe")
        && e.message.contains("only the case differs"),
      "{}",
      e.message
    );
    assert!(!e.message.contains("changed while"), "{}", e.message);

    // No executable at all, under any case: refused as missing.
    std::fs::remove_file(staged.join("app.exe")).unwrap();
    let entries = super::super::swap::tree_listing(&staged).unwrap().1;
    let e = verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &entries,
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
    assert!(
      e.message.contains("App.exe is not in the staged app"),
      "{}",
      e.message
    );
  }

  #[test]
  fn case_only_renames_are_found_once() {
    let names = ["a/App.exe", "a/x.json"];
    assert_eq!(case_only_rename("a/app.exe", names), Some("a/App.exe"));
    assert_eq!(case_only_rename("A/APP.EXE", names), Some("a/App.exe"));
    // The exact name is not a rename; another name is not either.
    assert_eq!(case_only_rename("a/App.exe", names), None);
    assert_eq!(case_only_rename("a/b.exe", names), None);
    // Two names that differ only in case (a case-sensitive directory): no
    // single answer.
    assert_eq!(case_only_rename("app.exe", ["APP.exe", "App.exe"]), None);
  }

  #[cfg(unix)]
  #[test]
  fn a_symlink_in_the_staged_tree_is_refused() {
    let launch: &[u8] = b"{}";
    let t = tempfile::tempdir().unwrap();
    let staged = t.path().join("App");
    let signed = pe_with_resource(
      RESOURCE_NAME,
      &manifest_json(&[("laufey-launch.json", launch)]),
      &[1033],
    );
    write(&staged, "App.exe", &signed);
    write(&staged, "laufey-launch.json", launch);
    std::os::unix::fs::symlink("laufey-launch.json", staged.join("l")).unwrap();
    let running = t.path().join("running.exe");
    std::fs::write(&running, &signed).unwrap();
    let entries = super::super::swap::tree_listing(&staged).unwrap().1;
    let e = verify_staged_tree(
      &running,
      &staged,
      "App.exe",
      &entries,
      Some(APP_ID),
      Some(VERSION),
    )
    .unwrap_err();
    assert!(e.message.contains("symlink"), "{}", e.message);
  }
}
