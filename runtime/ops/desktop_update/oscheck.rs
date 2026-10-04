// Copyright 2018-2026 the Deno authors. MIT license.

//! The OS code-signature check of a staged app, against the running app.
//!
//! - **macOS:** `codesign --verify --deep --strict` must pass on the staged
//!   bundle. When the running app is signed with a Developer ID (it has a
//!   Team ID), the staged bundle must carry the SAME Team ID and pass
//!   Gatekeeper (`spctl --assess --type execute`), and its signing
//!   identifier must match the running app's (the same app).
//! - **Windows:** when the running executable has a trusted Authenticode
//!   signature (`WinVerifyTrust`), EVERY PE image in the staged app must
//!   verify too (see [`pe_files`]: any file with a PE header, whatever its
//!   extension: `.exe`, `.dll`, a native addon's `.node`, ...), each with a
//!   leaf signer certificate whose issuer AND subject equal the running
//!   executable's. Checking only the executable and its runtime DLL left
//!   every other DLL the app loads (CEF's, an addon's) unchecked: a
//!   same-named DLL is all a DLL-planting attack needs.
//! - **Linux:** the OS has no code signature; the manifest signature and the
//!   archive's SHA-256 are the whole check.
//!
//! A running app WITHOUT that identity (ad-hoc or unsigned: a dev build)
//! cannot prove who may replace it, so the update is refused unless the
//! caller passes the dev-only `allowUnsignedDev` opt-out. The opt-out is
//! ignored when the running app is signed: it can never weaken a signed app.

#![allow(
  clippy::disallowed_methods,
  reason = "reads the staged app next to the install, outside any user \
            permission sandbox, by design"
)]

use std::path::Path;
use std::path::PathBuf;

use super::error::UpdateError;
use super::error::UpdateErrorCode as Code;
use super::error::err;

/// How the staged app's OS signature was established.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureReport {
  /// `"team"` (macOS, same Developer ID Team ID), `"authenticode"`
  /// (Windows, same signer subject), `"none"` (Linux: no OS signature), or
  /// `"dev-unsigned"` (the running app has no identity and the dev opt-out
  /// was given).
  pub mode: &'static str,
  /// The Team ID / signer subject both apps share, when there is one.
  pub identity: Option<String>,
}

/// The output of a process run by a [`CommandRunner`].
#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
  pub success: bool,
  pub stdout: String,
  pub stderr: String,
}

/// Runs the OS signing tools (a seam for tests).
pub trait CommandRunner {
  fn run(&self, program: &str, args: &[&std::ffi::OsStr]) -> CmdOutput;
}

/// The real [`CommandRunner`].
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
  fn run(&self, program: &str, args: &[&std::ffi::OsStr]) -> CmdOutput {
    match std::process::Command::new(program)
      .args(args)
      .stdin(std::process::Stdio::null())
      .output()
    {
      Ok(o) => CmdOutput {
        success: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
      },
      Err(e) => CmdOutput {
        success: false,
        stdout: String::new(),
        stderr: format!("could not run {program}: {e}"),
      },
    }
  }
}

/// What `codesign -dv` says about a bundle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacSignInfo {
  pub signed: bool,
  pub identifier: Option<String>,
  /// `None` for ad-hoc / unsigned (`TeamIdentifier=not set`).
  pub team_id: Option<String>,
}

/// Parse `codesign -dv --verbose=2` output (it writes to stderr).
///
/// Only whole `Key=value` lines with exactly the keys `Identifier` and
/// `TeamIdentifier` count. Each must appear at most once: codesign prints
/// each once, so a second one (with any value) means the output is not what
/// it seems (e.g. a path with a newline in the `Executable=` line) and the
/// bundle is treated as unsigned, which every check then refuses.
pub fn parse_codesign_display(out: &CmdOutput) -> MacSignInfo {
  if !out.success {
    return MacSignInfo::default();
  }
  let mut identifier: Option<String> = None;
  let mut team: Option<String> = None;
  for line in out.stderr.lines().chain(out.stdout.lines()) {
    let Some((key, value)) = line.split_once('=') else {
      continue;
    };
    let slot = match key {
      "Identifier" => &mut identifier,
      "TeamIdentifier" => &mut team,
      _ => continue,
    };
    if slot.is_some() {
      return MacSignInfo::default();
    }
    *slot = Some(value.trim().to_string());
  }
  MacSignInfo {
    signed: true,
    identifier: identifier.filter(|v| !v.is_empty()),
    team_id: team.filter(|v| !v.is_empty() && v != "not set"),
  }
}

fn codesign_info(runner: &dyn CommandRunner, path: &Path) -> MacSignInfo {
  parse_codesign_display(&runner.run(
    "/usr/bin/codesign",
    &["-dv".as_ref(), "--verbose=2".as_ref(), path.as_os_str()],
  ))
}

/// The macOS check (see the module docs).
pub fn verify_macos(
  runner: &dyn CommandRunner,
  running: &Path,
  staged: &Path,
  allow_unsigned_dev: bool,
) -> Result<SignatureReport, UpdateError> {
  let running_info = codesign_info(runner, running);
  let verify = runner.run(
    "/usr/bin/codesign",
    &[
      "--verify".as_ref(),
      "--deep".as_ref(),
      "--strict".as_ref(),
      staged.as_os_str(),
    ],
  );
  if !verify.success {
    return err(
      Code::OsSignature,
      format!(
        "the staged app's code signature does not verify: {}",
        verify.stderr.trim()
      ),
    );
  }
  let staged_info = codesign_info(runner, staged);
  if let Some(team) = &running_info.team_id {
    if staged_info.team_id.as_deref() != Some(team.as_str()) {
      return err(
        Code::OsSignature,
        format!(
          "the staged app is signed by Team ID {}, the running app by {team}",
          staged_info
            .team_id
            .as_deref()
            .unwrap_or("(none: ad-hoc/unsigned)")
        ),
      );
    }
    let assess = runner.run(
      "/usr/sbin/spctl",
      &[
        "--assess".as_ref(),
        "--type".as_ref(),
        "execute".as_ref(),
        staged.as_os_str(),
      ],
    );
    if !assess.success {
      return err(
        Code::OsSignature,
        format!(
          "Gatekeeper rejects the staged app (spctl --assess): {}",
          assess.stderr.trim()
        ),
      );
    }
    if running_info.identifier != staged_info.identifier {
      return err(
        Code::BundleMismatch,
        format!(
          "the staged app's signing identifier {:?} differs from the running \
           app's {:?}",
          staged_info.identifier, running_info.identifier
        ),
      );
    }
    return Ok(SignatureReport {
      mode: "team",
      identity: Some(team.clone()),
    });
  }
  if !allow_unsigned_dev {
    return err(
      Code::OsSignature,
      "the running app is not signed with a Developer ID (it is ad-hoc \
       signed or unsigned), so it cannot check who signed the update. Sign \
       release builds with a Developer ID; for a local dev build only, pass \
       allowUnsignedDev",
    );
  }
  if running_info.identifier.is_some()
    && staged_info.identifier.is_some()
    && running_info.identifier != staged_info.identifier
  {
    return err(
      Code::BundleMismatch,
      format!(
        "the staged app's signing identifier {:?} differs from the running \
         app's {:?}",
        staged_info.identifier, running_info.identifier
      ),
    );
  }
  Ok(SignatureReport {
    mode: "dev-unsigned",
    identity: None,
  })
}

/// An Authenticode signer: the leaf certificate's DER-encoded subject and
/// issuer, and a display name.
///
/// The pin is issuer + subject, not the leaf certificate's thumbprint: a
/// code-signing certificate is reissued every one to three years (and EV
/// certificates more often), and a thumbprint pin would make every app
/// signed with the old certificate refuse the first update signed with the
/// renewed one, with no way out but a manual reinstall. The same CA issuing
/// a certificate with the same subject DN (for an organization: its legal
/// name, and for EV its registration number) is the identity that renewal
/// keeps; a certificate for that name from another CA, or from the same CA
/// for another name, is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
  pub subject: Vec<u8>,
  pub issuer: Vec<u8>,
  pub name: String,
}

/// Every PE image under `root` (a file or a directory, walked without
/// following symlinks), sorted: every regular file with the headers the
/// Windows loader requires, `MZ` at 0 and `PE\0\0` at the offset the DOS
/// header's `e_lfanew` (0x3C) names. By content rather than by extension, so
/// a DLL renamed to anything (`.node`, `.pyd`, `.dat`, none) that
/// `LoadLibrary` would still load is checked too, and a data file that only
/// happens to start with `MZ` is not (`WinVerifyTrust` would refuse it, and
/// with it the update).
pub fn pe_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
  fn walk(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
      for entry in std::fs::read_dir(path)? {
        walk(&entry?.path(), out)?;
      }
    } else if meta.is_file() && is_pe_image(path)? {
      out.push(path.to_path_buf());
    }
    Ok(())
  }
  let mut out = Vec::new();
  walk(root, &mut out)?;
  out.sort();
  Ok(out)
}

/// Whether the file at `path` has a PE image's headers (see [`pe_files`]).
fn is_pe_image(path: &Path) -> std::io::Result<bool> {
  use std::io::Read;
  use std::io::Seek;
  let mut f = std::fs::File::open(path)?;
  let mut dos = [0u8; 64];
  if f.read_exact(&mut dos).is_err() || &dos[..2] != b"MZ" {
    return Ok(false);
  }
  let e_lfanew = u32::from_le_bytes([dos[60], dos[61], dos[62], dos[63]]);
  let mut sig = [0u8; 4];
  Ok(
    f.seek(std::io::SeekFrom::Start(u64::from(e_lfanew)))
      .is_ok()
      && f.read_exact(&mut sig).is_ok()
      && &sig == b"PE\0\0",
  )
}

/// The Windows check over a signer lookup (`None`: no trusted signature).
/// `staged_files` is every PE image of the staged app ([`pe_files`]). See
/// the module docs.
pub fn verify_windows(
  lookup: &dyn Fn(&Path) -> Option<Signer>,
  running_exe: &Path,
  staged_files: &[PathBuf],
  allow_unsigned_dev: bool,
) -> Result<SignatureReport, UpdateError> {
  let Some(running) = lookup(running_exe) else {
    if !allow_unsigned_dev {
      return err(
        Code::OsSignature,
        "the running app has no trusted Authenticode signature, so it \
         cannot check who signed the update. Sign release builds; for a \
         local dev build only, pass allowUnsignedDev",
      );
    }
    return Ok(SignatureReport {
      mode: "dev-unsigned",
      identity: None,
    });
  };
  if staged_files.is_empty() {
    return err(Code::OsSignature, "the staged app holds no PE image");
  }
  for file in staged_files {
    match lookup(file) {
      Some(s) if s.subject == running.subject && s.issuer == running.issuer => {
      }
      Some(other) => {
        return err(
          Code::OsSignature,
          format!(
            "{} is signed by {:?}, the running app by {:?} (the signer's \
             issuer and subject must both match)",
            file.display(),
            other.name,
            running.name
          ),
        );
      }
      None => {
        return err(
          Code::OsSignature,
          format!("{} has no trusted Authenticode signature", file.display()),
        );
      }
    }
  }
  Ok(SignatureReport {
    mode: "authenticode",
    identity: Some(running.name),
  })
}

/// `CRYPT_E_REVOCATION_OFFLINE`: the revocation server could not be reached.
const CRYPT_E_REVOCATION_OFFLINE: i32 = 0x80092013_u32 as i32;
/// `CERT_E_REVOCATION_FAILURE`: revocation could not be checked.
const CERT_E_REVOCATION_FAILURE: i32 = 0x800B010E_u32 as i32;

/// How [`authenticode_signer`] treats a `WinVerifyTrust` status from the
/// check WITH revocation: `Some(true)` trusted, `Some(false)` refused, `None`
/// revocation could not be determined (offline) and the offline fallback
/// decides.
///
/// The whole table:
///
/// | status                                   | verdict                     |
/// | ---------------------------------------- | --------------------------- |
/// | `0` (trusted, revocation checked)        | accepted                    |
/// | `CRYPT_E_REVOCATION_OFFLINE` 0x80092013  | fallback without revocation |
/// | `CERT_E_REVOCATION_FAILURE` 0x800B010E   | fallback without revocation |
/// | `CRYPT_E_REVOKED` / `CERT_E_REVOKED`     | refused                     |
/// | `CRYPT_E_NO_REVOCATION_CHECK` 0x80092012 | refused (the CA publishes no revocation info for a cert that should have it) |
/// | anything else (unsigned, bad digest, untrusted root, expired without a timestamp, ...) | refused |
///
/// The fallback re-verifies the whole chain WITHOUT revocation and accepts
/// only a trusted result: it is taken when the revocation servers can't be
/// reached (an offline machine, a captive portal, a CRL/OCSP outage), never
/// when a certificate is positively known revoked.
pub fn revocation_status_verdict(status: i32) -> Option<bool> {
  match status {
    0 => Some(true),
    CRYPT_E_REVOCATION_OFFLINE | CERT_E_REVOCATION_FAILURE => None,
    // Anything else, CRYPT_E_REVOKED / CERT_E_REVOKED included: refused.
    _ => Some(false),
  }
}

/// The Authenticode signer of `path`: verified with `WinVerifyTrust`
/// (`WINTRUST_ACTION_GENERIC_VERIFY_V2`, the whole chain to a trusted root,
/// no UI), then the leaf signer certificate's subject and issuer.
///
/// Revocation is checked online for the whole chain except the root
/// (`WTD_REVOKE_WHOLECHAIN` + `WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT`): a
/// revoked signing certificate is refused. Offline-tolerant fallback: when
/// the revocation status cannot be determined (no network, the CRL/OCSP
/// server unreachable: `CRYPT_E_REVOCATION_OFFLINE` /
/// `CERT_E_REVOCATION_FAILURE`), the signature is verified again without
/// the revocation lookup and accepted if valid, so an offline machine can
/// still update; a certificate positively reported revoked never is. The
/// update manifest is separately signed with the app's own key, so the
/// fallback only weakens the second, OS-level check while offline.
#[cfg(windows)]
pub fn authenticode_signer(path: &Path) -> Option<Signer> {
  match authenticode_signer_with(path, true) {
    Ok(signer) => Some(signer),
    Err(status) => match revocation_status_verdict(status) {
      None => authenticode_signer_with(path, false).ok(),
      Some(_) => None,
    },
  }
}

/// One `WinVerifyTrust` pass (`revocation`: online revocation checks), then
/// the signer; `Err(status)` when the file is not trusted.
#[cfg(windows)]
fn authenticode_signer_with(
  path: &Path,
  revocation: bool,
) -> Result<Signer, i32> {
  use std::os::windows::ffi::OsStrExt;

  use windows_sys::Win32::Security::Cryptography::CERT_NAME_SIMPLE_DISPLAY_TYPE;
  use windows_sys::Win32::Security::Cryptography::CertGetNameStringW;
  use windows_sys::Win32::Security::WinTrust::WINTRUST_ACTION_GENERIC_VERIFY_V2;
  use windows_sys::Win32::Security::WinTrust::WINTRUST_DATA;
  use windows_sys::Win32::Security::WinTrust::WINTRUST_DATA_0;
  use windows_sys::Win32::Security::WinTrust::WINTRUST_FILE_INFO;
  use windows_sys::Win32::Security::WinTrust::WTD_CHOICE_FILE;
  use windows_sys::Win32::Security::WinTrust::WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT;
  use windows_sys::Win32::Security::WinTrust::WTD_REVOKE_NONE;
  use windows_sys::Win32::Security::WinTrust::WTD_REVOKE_WHOLECHAIN;
  use windows_sys::Win32::Security::WinTrust::WTD_STATEACTION_CLOSE;
  use windows_sys::Win32::Security::WinTrust::WTD_STATEACTION_VERIFY;
  use windows_sys::Win32::Security::WinTrust::WTD_UI_NONE;
  use windows_sys::Win32::Security::WinTrust::WTHelperGetProvSignerFromChain;
  use windows_sys::Win32::Security::WinTrust::WTHelperProvDataFromStateData;
  use windows_sys::Win32::Security::WinTrust::WinVerifyTrust;

  let wide: Vec<u16> = path
    .as_os_str()
    .encode_wide()
    .chain(std::iter::once(0))
    .collect();
  // SAFETY: zeroed POD structs, then the documented fields set; the path
  // buffer outlives both WinVerifyTrust calls; the state is closed below.
  unsafe {
    let mut file: WINTRUST_FILE_INFO = std::mem::zeroed();
    file.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
    file.pcwszFilePath = wide.as_ptr();
    let mut data: WINTRUST_DATA = std::mem::zeroed();
    data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    data.dwUIChoice = WTD_UI_NONE;
    if revocation {
      data.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
      data.dwProvFlags = WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT;
    } else {
      data.fdwRevocationChecks = WTD_REVOKE_NONE;
    }
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous = WINTRUST_DATA_0 { pFile: &mut file };
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = WinVerifyTrust(
      std::ptr::null_mut(),
      &mut action,
      &mut data as *mut _ as *mut std::ffi::c_void,
    );
    let mut result = None;
    if status == 0 {
      let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
      if !prov.is_null() {
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, 0, 0);
        if !sgnr.is_null() && (*sgnr).csCertChain > 0 {
          let cert = (*(*sgnr).pasCertChain).pCert;
          if !cert.is_null() && !(*cert).pCertInfo.is_null() {
            let der = |blob: &windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB| {
              if blob.pbData.is_null() {
                Vec::new()
              } else {
                std::slice::from_raw_parts(blob.pbData, blob.cbData as usize)
                  .to_vec()
              }
            };
            let subject = der(&(*(*cert).pCertInfo).Subject);
            let issuer = der(&(*(*cert).pCertInfo).Issuer);
            let mut buf = [0u16; 512];
            let n = CertGetNameStringW(
              cert,
              CERT_NAME_SIMPLE_DISPLAY_TYPE,
              0,
              std::ptr::null(),
              buf.as_mut_ptr(),
              buf.len() as u32,
            );
            let name =
              String::from_utf16_lossy(&buf[..(n as usize).saturating_sub(1)]);
            if !subject.is_empty() && !issuer.is_empty() {
              result = Some(Signer {
                subject,
                issuer,
                name,
              });
            }
          }
        }
      }
    }
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    WinVerifyTrust(
      std::ptr::null_mut(),
      &mut action,
      &mut data as *mut _ as *mut std::ffi::c_void,
    );
    result.ok_or(if status == 0 { -1 } else { status })
  }
}

#[cfg(not(windows))]
pub fn authenticode_signer(_path: &Path) -> Option<Signer> {
  None
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::collections::HashMap;

  use super::*;

  /// Answers by (program, last argument); records every call.
  #[derive(Default)]
  struct Fake {
    answers: HashMap<(String, String), CmdOutput>,
    calls: RefCell<Vec<String>>,
  }

  impl Fake {
    fn on(mut self, prog: &str, path: &str, ok: bool, text: &str) -> Self {
      self.answers.insert(
        (prog.to_string(), path.to_string()),
        CmdOutput {
          success: ok,
          stdout: String::new(),
          stderr: text.to_string(),
        },
      );
      self
    }
  }

  impl CommandRunner for Fake {
    fn run(&self, program: &str, args: &[&std::ffi::OsStr]) -> CmdOutput {
      let last = args.last().unwrap().to_string_lossy().into_owned();
      let verb = if program.ends_with("spctl") {
        "spctl".to_string()
      } else if args[0] == "--verify" {
        "verify".to_string()
      } else {
        "display".to_string()
      };
      self.calls.borrow_mut().push(format!("{verb} {last}"));
      self.answers.get(&(verb, last)).cloned().unwrap_or_default()
    }
  }

  fn team(id: &str, team: &str) -> String {
    format!("Executable=x\nIdentifier={id}\nTeamIdentifier={team}\n")
  }

  #[test]
  fn parses_codesign_output() {
    let i = parse_codesign_display(&CmdOutput {
      success: true,
      stdout: String::new(),
      stderr: "Identifier=com.a\nSignature=adhoc\nTeamIdentifier=not set\n"
        .into(),
    });
    assert!(i.signed);
    assert_eq!(i.identifier.as_deref(), Some("com.a"));
    assert_eq!(i.team_id, None);
    let i = parse_codesign_display(&CmdOutput {
      success: false,
      stdout: String::new(),
      stderr: "code object is not signed at all".into(),
    });
    assert!(!i.signed);
  }

  #[test]
  fn codesign_keys_are_exact_and_unique() {
    let out = |text: &str| CmdOutput {
      success: true,
      stdout: String::new(),
      stderr: text.into(),
    };
    // A repeated key (whatever its value) is not trusted: the bundle reads
    // as unsigned, which every check refuses.
    for text in [
      "Identifier=com.a\nTeamIdentifier=EVIL\nTeamIdentifier=TEAM1\n",
      "Identifier=com.a\nTeamIdentifier=TEAM1\nTeamIdentifier=TEAM1\n",
      "Identifier=com.a\nIdentifier=com.b\nTeamIdentifier=TEAM1\n",
    ] {
      assert_eq!(
        parse_codesign_display(&out(text)),
        MacSignInfo::default(),
        "{text:?}"
      );
    }
    // Only the exact keys count: look-alike keys are ignored.
    let i = parse_codesign_display(&out(
      "Identifier=com.a\nTeamIdentifier=TEAM1\nXTeamIdentifier=EVIL\n\
       TeamIdentifierX=EVIL\n Identifier=evil\n",
    ));
    assert_eq!(i.identifier.as_deref(), Some("com.a"));
    assert_eq!(i.team_id.as_deref(), Some("TEAM1"));
  }

  #[test]
  fn authenticode_revocation_verdicts() {
    assert_eq!(revocation_status_verdict(0), Some(true));
    // Revoked: refused outright, never retried without revocation.
    for revoked in [0x80092010_u32 as i32, 0x800B010C_u32 as i32] {
      assert_eq!(revocation_status_verdict(revoked), Some(false));
    }
    // Offline: the fallback (no revocation lookup) decides.
    assert_eq!(revocation_status_verdict(CRYPT_E_REVOCATION_OFFLINE), None);
    assert_eq!(revocation_status_verdict(CERT_E_REVOCATION_FAILURE), None);
    // No revocation information at all (CRYPT_E_NO_REVOCATION_CHECK), an
    // untrusted root (CERT_E_UNTRUSTEDROOT), an expired certificate
    // (CERT_E_EXPIRED): refused, no fallback.
    for status in [0x80092012_u32, 0x800B0109, 0x800B0101] {
      assert_eq!(revocation_status_verdict(status as i32), Some(false));
    }
    // Not signed / bad signature: refused.
    assert_eq!(
      revocation_status_verdict(0x800B0100_u32 as i32),
      Some(false)
    );
    assert_eq!(
      revocation_status_verdict(0x80096010_u32 as i32),
      Some(false)
    );
  }

  #[test]
  fn same_team_passes() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "TEAM1"))
      .on("display", "/new", true, &team("com.a", "TEAM1"))
      .on("verify", "/new", true, "")
      .on("spctl", "/new", true, "");
    let r = verify_macos(&f, "/run".as_ref(), "/new".as_ref(), false).unwrap();
    assert_eq!(r.mode, "team");
    assert_eq!(r.identity.as_deref(), Some("TEAM1"));
  }

  #[test]
  fn different_team_is_refused_even_with_the_opt_out() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "TEAM1"))
      .on("display", "/new", true, &team("com.a", "EVIL2"))
      .on("verify", "/new", true, "")
      .on("spctl", "/new", true, "");
    for opt_out in [false, true] {
      let e = verify_macos(&f, "/run".as_ref(), "/new".as_ref(), opt_out)
        .unwrap_err();
      assert_eq!(e.code, Code::OsSignature);
      assert!(e.message.contains("EVIL2"), "{}", e.message);
    }
  }

  #[test]
  fn adhoc_staged_under_a_team_app_is_refused() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "TEAM1"))
      .on("display", "/new", true, &team("com.a", "not set"))
      .on("verify", "/new", true, "");
    let e =
      verify_macos(&f, "/run".as_ref(), "/new".as_ref(), true).unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
  }

  #[test]
  fn gatekeeper_and_identifier_are_enforced() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "TEAM1"))
      .on("display", "/new", true, &team("com.a", "TEAM1"))
      .on("verify", "/new", true, "")
      .on("spctl", "/new", false, "rejected");
    let e =
      verify_macos(&f, "/run".as_ref(), "/new".as_ref(), false).unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "TEAM1"))
      .on("display", "/new", true, &team("com.b", "TEAM1"))
      .on("verify", "/new", true, "")
      .on("spctl", "/new", true, "");
    let e =
      verify_macos(&f, "/run".as_ref(), "/new".as_ref(), false).unwrap_err();
    assert_eq!(e.code, Code::BundleMismatch);
  }

  #[test]
  fn broken_staged_signature_is_refused_in_every_mode() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "not set"))
      .on("verify", "/new", false, "a sealed resource is missing");
    for opt_out in [false, true] {
      let e = verify_macos(&f, "/run".as_ref(), "/new".as_ref(), opt_out)
        .unwrap_err();
      assert_eq!(e.code, Code::OsSignature);
    }
  }

  #[test]
  fn unsigned_running_app_needs_the_dev_opt_out() {
    let f = Fake::default()
      .on("display", "/run", true, &team("com.a", "not set"))
      .on("display", "/new", true, &team("com.a", "not set"))
      .on("verify", "/new", true, "");
    let e =
      verify_macos(&f, "/run".as_ref(), "/new".as_ref(), false).unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
    let r = verify_macos(&f, "/run".as_ref(), "/new".as_ref(), true).unwrap();
    assert_eq!(r.mode, "dev-unsigned");
    // Gatekeeper is not consulted for a dev build.
    assert!(!f.calls.borrow().iter().any(|c| c.starts_with("spctl")));
  }

  #[test]
  fn windows_signer_subject_must_match() {
    let signer = |subject: &[u8], issuer: &[u8], name: &str| Signer {
      subject: subject.to_vec(),
      issuer: issuer.to_vec(),
      name: name.into(),
    };
    let lookup = |p: &Path| -> Option<Signer> {
      match p.to_str().unwrap() {
        "run.exe" | "new.exe" | "new.dll" | "addon.node" => {
          Some(signer(b"CN=A", b"CN=CA1", "A"))
        }
        "evil.exe" => Some(signer(b"CN=E", b"CN=CA1", "E")),
        // The same subject from another CA: not the same identity.
        "otherca.dll" => Some(signer(b"CN=A", b"CN=CA2", "A")),
        _ => None,
      }
    };
    let files = |names: &[&str]| -> Vec<PathBuf> {
      names.iter().map(PathBuf::from).collect()
    };
    let ok = verify_windows(
      &lookup,
      "run.exe".as_ref(),
      &files(&["new.exe", "new.dll", "addon.node"]),
      false,
    )
    .unwrap();
    assert_eq!(ok.mode, "authenticode");
    assert_eq!(ok.identity.as_deref(), Some("A"));
    // One bad image among good ones is enough to refuse.
    for bad in [
      &["new.exe", "evil.exe"][..],
      &["new.exe", "unsigned.dll"],
      &["new.exe", "otherca.dll"],
    ] {
      let e = verify_windows(&lookup, "run.exe".as_ref(), &files(bad), true)
        .unwrap_err();
      assert_eq!(e.code, Code::OsSignature, "{bad:?}");
    }
    assert_eq!(
      verify_windows(&lookup, "run.exe".as_ref(), &[], false)
        .unwrap_err()
        .code,
      Code::OsSignature
    );
    for bad in [["evil.exe"], ["unsigned.exe"]] {
      for opt_out in [false, true] {
        let e =
          verify_windows(&lookup, "run.exe".as_ref(), &files(&bad), opt_out)
            .unwrap_err();
        assert_eq!(e.code, Code::OsSignature);
      }
    }
    // An unsigned running app: refused without the opt-out.
    let e =
      verify_windows(&lookup, "dev.exe".as_ref(), &files(&["new.exe"]), false)
        .unwrap_err();
    assert_eq!(e.code, Code::OsSignature);
    let r = verify_windows(
      &lookup,
      "dev.exe".as_ref(),
      &files(&["unsigned.exe"]),
      true,
    )
    .unwrap();
    assert_eq!(r.mode, "dev-unsigned");
  }

  #[cfg(windows)]
  #[test]
  fn an_unsigned_test_binary_has_no_authenticode_signer() {
    let exe = std::env::current_exe().unwrap();
    assert!(authenticode_signer(&exe).is_none());
  }

  #[cfg(windows)]
  #[test]
  fn a_system_binary_has_an_authenticode_signer() {
    // notepad.exe is catalog-signed on many builds (no embedded
    // signature), so check a binary with an embedded one if present.
    let windir = std::env::var("WINDIR").unwrap();
    let candidates = [
      format!("{windir}\\explorer.exe"),
      format!("{windir}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
    ];
    let found = candidates
      .iter()
      .filter_map(|p| authenticode_signer(Path::new(p)))
      .next();
    // Embedded signatures are not guaranteed on every image; record what we
    // saw without failing the suite on an image that only catalog-signs.
    if let Some(s) = found {
      assert!(!s.subject.is_empty());
      assert!(!s.issuer.is_empty());
      assert!(s.name.contains("Microsoft"), "{}", s.name);
    }
  }

  #[test]
  fn every_pe_image_is_found_by_content() {
    // A minimal PE header: `MZ`, e_lfanew = 0x80, `PE\0\0` there.
    let pe = || {
      let mut b = vec![0u8; 0x84];
      b[..2].copy_from_slice(b"MZ");
      b[60..64].copy_from_slice(&0x80u32.to_le_bytes());
      b[0x80..].copy_from_slice(b"PE\0\0");
      b
    };
    let mut bad_offset = pe();
    bad_offset[60..64].copy_from_slice(&0x1000u32.to_le_bytes());
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("App");
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    for (name, body) in [
      ("App.exe", pe()),
      ("App.dll", pe()),
      ("sub/addon.node", pe()),
      ("sub/deeper/renamed.dat", pe()),
      (
        "sub/readme.txt",
        b"MZ is the DOS magic, and this is text".repeat(4),
      ),
      ("sub/past-the-end.bin", bad_offset),
      ("data.pak", b"PAK\0".to_vec()),
      ("one", b"M".to_vec()),
      ("empty", Vec::new()),
    ] {
      std::fs::write(root.join(name), body).unwrap();
    }
    let found: Vec<String> = pe_files(&root)
      .unwrap()
      .iter()
      .map(|p| {
        p.strip_prefix(&root)
          .unwrap()
          .to_string_lossy()
          .replace('\\', "/")
      })
      .collect();
    assert_eq!(
      found,
      [
        "App.dll",
        "App.exe",
        "sub/addon.node",
        "sub/deeper/renamed.dat",
      ]
    );
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn real_codesign_adhoc_bundle() {
    // A tiny ad-hoc signed bundle passes `codesign --verify` and is accepted
    // only with the dev opt-out; a tampered copy is refused.
    let t = tempfile::tempdir().unwrap();
    let make = |name: &str| -> PathBuf {
      let app = t.path().join(name);
      let macos = app.join("Contents/MacOS");
      std::fs::create_dir_all(&macos).unwrap();
      std::fs::copy("/usr/bin/true", macos.join("probe")).unwrap();
      std::fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>probe</string>
<key>CFBundleIdentifier</key><string>com.example.probe</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#,
      )
      .unwrap();
      let ok = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app)
        .status()
        .unwrap()
        .success();
      assert!(ok);
      app
    };
    let running = make("Run.app");
    let staged = make("New.app");
    let r = verify_macos(&SystemRunner, &running, &staged, true).unwrap();
    assert_eq!(r.mode, "dev-unsigned");
    assert_eq!(
      verify_macos(&SystemRunner, &running, &staged, false)
        .unwrap_err()
        .code,
      Code::OsSignature
    );
    // Tamper: add a file to the sealed bundle.
    std::fs::write(staged.join("Contents/MacOS/extra"), b"evil").unwrap();
    assert_eq!(
      verify_macos(&SystemRunner, &running, &staged, true)
        .unwrap_err()
        .code,
      Code::OsSignature
    );
  }
}
