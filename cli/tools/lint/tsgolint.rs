// Copyright 2018-2026 the Deno authors. MIT license.

//! Type-aware lint rules through tsgolint (denext; adapted from upstream
//! denoland/deno#35584).
//!
//! `tsgolint` (typescript-eslint's type-aware rules on typescript-go, built by
//! the oxc project) runs `no-floating-promises` and the other rules that need
//! type information. `deno lint` drives its headless protocol directly and
//! stays the host: file discovery, configuration, `// deno-lint-ignore`,
//! `ban-unused-ignore` and the reporters (pretty, compact, JSON) all apply to
//! its diagnostics, which come back through deno_lint's external-linter
//! callback.
//!
//! Opt-in: `deno lint --type-aware` or `"lint": { "typeAware": true }` in the
//! root config (`--no-type-aware` overrides the config). tsgolint is run once
//! per workspace member batch: it builds one TypeScript program per
//! `tsconfig.json` (and an inferred one, ESNext + bundler resolution + strict,
//! for files with none), so per-file runs would be far too slow.
//!
//! The binary is an exact, pinned build (`tsgolint_pin.json`: the npm tarball
//! integrity and the binary's sha256 per platform), looked up in order:
//!  1. `DENO_TSGOLINT_BIN` (a path to a tsgolint binary, used as is);
//!  2. `<dir of the deno executable>/tsgolint/tsgolint[.exe]`, what the denext
//!     CLI archive ships (also through a symlink to it), when its sha256 is the
//!     pinned one;
//!  3. denext's toolchain cache, `<DENO_DIR>/denext-deno/<version>/<target>/
//!     tsgolint/`, newest release first, under the same check;
//!  4. `<DENO_DIR>/dl/tsgolint-<version>/<platform>/`, downloaded once from the
//!     pinned tarball and verified against both pins (so it then works
//!     offline); re-verified on every use.
//!
//! When none of these yields the binary, `deno lint` fails with an error that
//! names the fixes: it never silently drops the type-aware rules it was asked
//! to run.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::LazyLock;

use deno_ast::ModuleSpecifier;
use deno_ast::SourceRange;
use deno_ast::SourceTextInfo;
use deno_config::deno_json::LintRulesConfig;
use deno_core::anyhow::Context;
use deno_core::anyhow::anyhow;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_lint::diagnostic::LintDiagnostic;
use deno_lint::diagnostic::LintDiagnosticDetails;
use deno_lint::diagnostic::LintDiagnosticRange;
use deno_lint::diagnostic::LintDocsUrl;
use serde::Deserialize;
use sha2::Digest;

use crate::cache::DenoDir;
use crate::factory::CliFactory;
use crate::http_util::HttpClient;
use crate::tsc::native::TOOLCHAIN_CACHE_NAME;
use crate::tsc::native::toolchain_version_key;
use crate::tsc::native_pin;

/// Environment variable naming a tsgolint binary to use instead of the pinned
/// one.
pub const TSGOLINT_BIN_ENV: &str = "DENO_TSGOLINT_BIN";

/// The directory, next to the `deno` executable, that the CLI archive ships
/// tsgolint in.
pub const VENDORED_DIR_NAME: &str = "tsgolint";

/// The raw pin, embedded so a build always knows its tsgolint.
pub const TSGOLINT_PIN_JSON: &str = include_str!("tsgolint_pin.json");

/// One pinned platform build.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PinnedBinary {
  /// The npm tarball (`@oxlint-tsgolint/<platform>`).
  pub tarball: String,
  /// The tarball's npm `dist.integrity` (`sha512-…`).
  pub integrity: String,
  /// The sha256 (hex) of the binary inside it.
  pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TsgolintPin {
  pub version: String,
  pub platforms: BTreeMap<String, PinnedBinary>,
}

static PIN: LazyLock<TsgolintPin> = LazyLock::new(|| {
  serde_json::from_str(TSGOLINT_PIN_JSON)
    .expect("valid cli/tools/lint/tsgolint_pin.json")
});

pub fn pin() -> &'static TsgolintPin {
  &PIN
}

/// Every rule the pinned tsgolint implements, but `require-await`: deno_lint
/// has its own rule of that name, so `require-await` always means deno_lint's.
/// They are known rule codes even when type-aware linting is off, so a
/// `// deno-lint-ignore no-floating-promises` is never an unknown code.
pub const TYPE_AWARE_RULES: &[&str] = &[
  "await-thenable",
  "consistent-return",
  "consistent-type-exports",
  "dot-notation",
  "no-array-delete",
  "no-base-to-string",
  "no-confusing-void-expression",
  "no-deprecated",
  "no-duplicate-type-constituents",
  "no-floating-promises",
  "no-for-in-array",
  "no-generated-empty-object-type",
  "no-implied-eval",
  "no-meaningless-void-operator",
  "no-misused-promises",
  "no-misused-spread",
  "no-mixed-enums",
  "no-redundant-type-constituents",
  "no-unnecessary-boolean-literal-compare",
  "no-unnecessary-condition",
  "no-unnecessary-qualifier",
  "no-unnecessary-template-expression",
  "no-unnecessary-type-arguments",
  "no-unnecessary-type-assertion",
  "no-unnecessary-type-conversion",
  "no-unnecessary-type-parameters",
  "no-unsafe-argument",
  "no-unsafe-assignment",
  "no-unsafe-call",
  "no-unsafe-enum-comparison",
  "no-unsafe-member-access",
  "no-unsafe-return",
  "no-unsafe-type-assertion",
  "no-unsafe-unary-minus",
  "no-useless-default-assignment",
  "non-nullable-type-assertion-style",
  "only-throw-error",
  "prefer-find",
  "prefer-includes",
  "prefer-nullish-coalescing",
  "prefer-optional-chain",
  "prefer-promise-reject-errors",
  "prefer-readonly",
  "prefer-readonly-parameter-types",
  "prefer-reduce-type-parameter",
  "prefer-regexp-exec",
  "prefer-return-this-type",
  "prefer-string-starts-ends-with",
  "promise-function-async",
  "related-getter-setter-pairs",
  "require-array-sort-compare",
  "restrict-plus-operands",
  "restrict-template-expressions",
  "return-await",
  "strict-boolean-expressions",
  "strict-void-return",
  "switch-exhaustiveness-check",
  "unbound-method",
  "use-unknown-in-catch-callback-variable",
];

/// The type-aware rules run by default: a conservative, high-signal subset
/// (upstream #35584's). `lint.rules.include` adds any of
/// [`TYPE_AWARE_RULES`], `lint.rules.exclude` removes one.
pub const DEFAULT_RULES: &[&str] = &[
  "await-thenable",
  "no-base-to-string",
  "no-duplicate-type-constituents",
  "no-floating-promises",
  "no-for-in-array",
  "no-implied-eval",
  "no-misused-promises",
  "no-redundant-type-constituents",
  "no-unnecessary-type-assertion",
  "only-throw-error",
  "prefer-promise-reject-errors",
  "restrict-plus-operands",
  "restrict-template-expressions",
  "unbound-method",
];

pub fn exe_name() -> &'static str {
  if cfg!(windows) {
    "tsgolint.exe"
  } else {
    "tsgolint"
  }
}

fn sha256_hex(bytes: &[u8]) -> String {
  format!("{:x}", sha2::Sha256::digest(bytes))
}

fn file_matches_sha256(path: &Path, want: &str) -> bool {
  std::fs::read(path)
    .map(|bytes| sha256_hex(&bytes) == want)
    .unwrap_or(false)
}

/// The pinned tsgolint vendored beside `exe` (`<dir>/tsgolint/tsgolint`),
/// when it is the pinned build.
fn vendored_beside(exe: &Path, want_sha256: &str) -> Option<PathBuf> {
  let candidate = exe.parent()?.join(VENDORED_DIR_NAME).join(exe_name());
  if !candidate.is_file() {
    return None;
  }
  if file_matches_sha256(&candidate, want_sha256) {
    Some(candidate)
  } else {
    log::debug!(
      "ignoring {}: not the pinned tsgolint {}",
      candidate.display(),
      pin().version
    );
    None
  }
}

/// The pinned tsgolint in denext's toolchain cache
/// (`<cache>/<version>/<target>/tsgolint/`), newest release first.
fn toolchain_cache_bin(cache: &Path, want_sha256: &str) -> Option<PathBuf> {
  let mut versions: Vec<PathBuf> = std::fs::read_dir(cache)
    .ok()?
    .flatten()
    .map(|e| e.path())
    .filter(|p| p.is_dir())
    .collect();
  versions.sort_by_cached_key(|p| toolchain_version_key(p));
  for version in versions.iter().rev() {
    let Ok(targets) = std::fs::read_dir(version) else {
      continue;
    };
    for target in targets.flatten() {
      if let Some(found) =
        vendored_beside(&target.path().join("deno"), want_sha256)
      {
        return Some(found);
      }
    }
  }
  None
}

/// A tsgolint that needs no download: the override, else the pinned build
/// beside `current_exe` (or the executable it links to), else the one in
/// denext's toolchain cache under `deno_dir_root`.
pub fn find_local(
  override_path: Option<std::ffi::OsString>,
  current_exe: Option<PathBuf>,
  deno_dir_root: Option<&Path>,
  platform: &str,
) -> Result<Option<PathBuf>, AnyError> {
  if let Some(path) = override_path.filter(|p| !p.is_empty()) {
    let path = PathBuf::from(path);
    if !path.is_file() {
      bail!(
        "{} is set to {}, which is not a file",
        TSGOLINT_BIN_ENV,
        path.display()
      );
    }
    return Ok(Some(path));
  }
  let Some(want) = pin().platforms.get(platform).map(|p| p.sha256.as_str())
  else {
    return Ok(None);
  };
  let real_exe = current_exe
    .as_deref()
    .and_then(|exe| crate::util::fs::canonicalize_path(exe).ok())
    .filter(|real| Some(real) != current_exe.as_ref());
  for exe in current_exe.iter().chain(real_exe.iter()) {
    if let Some(found) = vendored_beside(exe, want) {
      return Ok(Some(found));
    }
  }
  if let Some(root) = deno_dir_root
    && let Some(found) =
      toolchain_cache_bin(&root.join(TOOLCHAIN_CACHE_NAME), want)
  {
    log::debug!(
      "using the tsgolint from denext's toolchain cache: {}",
      found.display()
    );
    return Ok(Some(found));
  }
  Ok(None)
}

/// Where a downloaded tsgolint lives in `DENO_DIR`.
pub fn cache_dir(deno_dir: &DenoDir, platform: &str) -> PathBuf {
  deno_dir
    .dl_folder_path()
    .join(format!("tsgolint-{}", pin().version))
    .join(platform)
}

/// The binary (`package/<exe>`) from an `@oxlint-tsgolint/<platform>` npm
/// tarball.
pub fn extract_binary(tgz: &[u8], exe: &str) -> Result<Vec<u8>, AnyError> {
  let mut archive =
    tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(tgz)));
  let want = Path::new("package").join(exe);
  for entry in archive.entries()? {
    let mut entry = entry?;
    if !entry.header().entry_type().is_file() {
      continue;
    }
    if entry.path()?.as_ref() == want.as_path() {
      let mut bytes = Vec::new();
      entry.read_to_end(&mut bytes)?;
      return Ok(bytes);
    }
  }
  bail!("the tarball has no package/{exe}")
}

/// Check a downloaded tarball against both pins and return the binary.
pub fn verify_tarball(
  tgz: &[u8],
  pinned: &PinnedBinary,
  exe: &str,
) -> Result<Vec<u8>, AnyError> {
  native_pin::verify_integrity(tgz, &pinned.integrity)?;
  let binary = extract_binary(tgz, exe)?;
  let got = sha256_hex(&binary);
  if got != pinned.sha256 {
    bail!(
      "{exe} sha256 {got} does not match the pinned {}",
      pinned.sha256
    );
  }
  Ok(binary)
}

/// The binary already in `install_dir`, when it is still the pinned build
/// (checked on every use: no network needed once it is there).
fn cached_bin(install_dir: &Path, pinned: &PinnedBinary) -> Option<PathBuf> {
  let exe = install_dir.join(exe_name());
  if !exe.is_file() {
    return None;
  }
  if file_matches_sha256(&exe, &pinned.sha256) {
    Some(exe)
  } else {
    log::warn!(
      "{} is not the pinned tsgolint {}; downloading it again",
      exe.display(),
      pin().version
    );
    None
  }
}

/// Put a verified `binary` into `install_dir`: written beside the final path
/// and renamed into place, so a concurrent `deno lint` never runs a
/// half-written binary.
fn install_binary(
  install_dir: &Path,
  binary: &[u8],
  pinned: &PinnedBinary,
) -> Result<PathBuf, AnyError> {
  let exe = install_dir.join(exe_name());
  std::fs::create_dir_all(install_dir)
    .with_context(|| format!("failed to create {}", install_dir.display()))?;
  let tmp =
    install_dir.join(format!(".{}.{}.tmp", exe_name(), std::process::id()));
  std::fs::write(&tmp, binary)
    .with_context(|| format!("failed to write {}", tmp.display()))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
  }
  match std::fs::rename(&tmp, &exe) {
    Ok(()) => Ok(exe),
    Err(_) if file_matches_sha256(&exe, &pinned.sha256) => {
      let _ = std::fs::remove_file(&tmp);
      Ok(exe)
    }
    Err(err) => {
      let _ = std::fs::remove_file(&tmp);
      Err(err).with_context(|| {
        format!("failed to move tsgolint to {}", exe.display())
      })
    }
  }
}

/// Download (once) and return the pinned tsgolint in `DENO_DIR`.
pub async fn ensure_downloaded(
  deno_dir: &DenoDir,
  http_client: &HttpClient,
  platform: &str,
) -> Result<PathBuf, AnyError> {
  let pin = pin();
  let pinned = pin
    .platforms
    .get(platform)
    .ok_or_else(|| anyhow!("tsgolint_pin.json has no {platform} build"))?;
  let install_dir = cache_dir(deno_dir, platform);
  if let Some(exe) = cached_bin(&install_dir, pinned) {
    return Ok(exe);
  }
  log::info!(
    "Downloading tsgolint {} ({platform}) for type-aware linting",
    pin.version
  );
  let bytes = http_client
    .download(Url::parse(&pinned.tarball)?)
    .await
    .with_context(|| format!("failed to download {}", pinned.tarball))?;
  let binary = verify_tarball(&bytes, pinned, exe_name())
    .with_context(|| format!("refusing {}", pinned.tarball))?;
  install_binary(&install_dir, &binary, pinned)
}

/// The tsgolint `deno lint --type-aware` runs, or an error naming the fixes.
pub async fn resolve_bin(factory: &CliFactory) -> Result<PathBuf, AnyError> {
  let version = &pin().version;
  let platform = native_pin::host_platform().ok_or_else(|| {
    anyhow!(
      "Type-aware linting is not available on {} {}: tsgolint has no build for it. \
       Lint without type-aware rules: --no-type-aware.",
      std::env::consts::OS,
      std::env::consts::ARCH
    )
  })?;
  let deno_dir = factory.deno_dir()?;
  if let Some(found) = find_local(
    std::env::var_os(TSGOLINT_BIN_ENV),
    std::env::current_exe().ok(),
    Some(deno_dir.root.as_path()),
    platform,
  )? {
    return Ok(found);
  }
  let downloaded: Result<PathBuf, AnyError> = async {
    let http_client = factory.http_client_provider().get_or_create()?;
    ensure_downloaded(deno_dir, &http_client, platform).await
  }
  .await;
  downloaded.map_err(|err| {
    anyhow!(
      "Type-aware linting needs tsgolint {version}, which was not found and could not be downloaded: {err:#}\n  \
       It ships in denext's Deno toolchain: run `denext toolchain install`.\n  \
       Or set {TSGOLINT_BIN_ENV} to a tsgolint {version} binary, or lint without type-aware rules: --no-type-aware."
    )
  })
}

/// The type-aware rules to run for a member's rules config: [`DEFAULT_RULES`],
/// plus the type-aware rules `include`d, minus the `exclude`d ones.
pub fn resolve_rules(config: &LintRulesConfig) -> Vec<String> {
  let exclude = config.exclude.as_deref().unwrap_or_default();
  let include = config.include.as_deref().unwrap_or_default();
  let mut rules: Vec<String> = DEFAULT_RULES
    .iter()
    .map(|s| s.to_string())
    .filter(|r| !exclude.contains(r))
    .collect();
  for name in include {
    if TYPE_AWARE_RULES.contains(&name.as_str())
      && !rules.contains(name)
      && !exclude.contains(name)
    {
      rules.push(name.clone());
    }
  }
  rules
}

/// Whether tsgolint lints a file of this kind (a TypeScript or JavaScript
/// module; not an extensionless `--ext` file, which it can't type).
fn is_type_aware_path(path: &Path) -> bool {
  matches!(
    path
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.to_ascii_lowercase())
      .as_deref(),
    Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs")
  )
}

/// The key a file's diagnostics are stored under: its real path, with forward
/// slashes (as tsgolint reports it) and, on Windows, lowercased.
fn normalize_key(path: &Path) -> String {
  let real = crate::util::fs::canonicalize_path(path)
    .unwrap_or_else(|_| path.to_path_buf());
  normalize_str(&real.to_string_lossy())
}

fn normalize_str(s: &str) -> String {
  let s = s.replace('\\', "/");
  if cfg!(windows) { s.to_lowercase() } else { s }
}

fn hash_text(text: &str) -> u64 {
  let mut hasher = std::collections::hash_map::DefaultHasher::new();
  text.hash(&mut hasher);
  hasher.finish()
}

/// tsgolint's diagnostics for one batch of files, and the rules it ran (known,
/// enabled codes for `// deno-lint-ignore` and `ban-unused-ignore`).
#[derive(Debug, Default)]
pub struct TsgolintResults {
  by_file: HashMap<String, FileDiagnostics>,
  enabled_rules: Vec<String>,
  /// Program-level problems (e.g. an invalid `tsconfig.json`).
  program_diagnostics: Vec<String>,
}

#[derive(Debug, Default)]
struct FileDiagnostics {
  /// The hash of the text tsgolint linted (BOM stripped, as tsgolint and
  /// deno_lint both see it): its ranges are byte offsets into that text.
  text_hash: Option<u64>,
  diagnostics: Vec<TsgolintDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TsgolintDiagnostic {
  code: String,
  message: String,
  hint: Option<String>,
  /// UTF-8 byte offsets `(start, end)` into the file's text.
  range: Option<(usize, usize)>,
  labels: Vec<String>,
}

impl TsgolintResults {
  /// The enabled rule codes, for `ExternalLinterResult::rules`.
  pub fn rule_codes(&self) -> impl Iterator<Item = Cow<'static, str>> + '_ {
    self.enabled_rules.iter().map(|r| Cow::Owned(r.clone()))
  }

  /// Log the program-level problems tsgolint reported.
  pub fn warn_program_diagnostics(&self) {
    for message in &self.program_diagnostics {
      log::warn!("Type-aware lint (tsgolint): {message}");
    }
  }

  /// The diagnostics for `file_path`, anchored in its parsed source. When the
  /// text is not the one tsgolint linted (a `--fix` pass rewrote it), its
  /// offsets no longer apply: the diagnostics are placed at the start of the
  /// file, with a note to lint again, rather than dropped (deno_lint drops a
  /// diagnostic without a range).
  pub fn diagnostics_for(
    &self,
    file_path: &Path,
    specifier: &ModuleSpecifier,
    text_info: &SourceTextInfo,
  ) -> Vec<LintDiagnostic> {
    let Some(file) = self.by_file.get(&normalize_key(file_path)) else {
      return Vec::new();
    };
    let text = text_info.text_str();
    let same_text = file.text_hash == Some(hash_text(text));
    let start_pos = text_info.range().start;
    file
      .diagnostics
      .iter()
      .map(|d| {
        let exact = d.range.filter(|(start, end)| {
          same_text
            && start <= end
            && *end <= text.len()
            && text.is_char_boundary(*start)
            && text.is_char_boundary(*end)
        });
        let mut info: Vec<Cow<'static, str>> =
          d.labels.iter().map(|l| Cow::Owned(l.clone())).collect();
        if exact.is_none() && d.range.is_some() {
          info.push(Cow::Borrowed(
            "The file changed after type-aware linting (--fix); lint it again for the exact position.",
          ));
        }
        let (start, end) = exact.unwrap_or((0, 0));
        LintDiagnostic {
          specifier: specifier.clone(),
          range: Some(LintDiagnosticRange {
            range: SourceRange::new(start_pos + start, start_pos + end),
            description: None,
            text_info: text_info.clone(),
          }),
          details: LintDiagnosticDetails {
            message: d.message.clone(),
            code: d.code.clone(),
            hint: d.hint.clone(),
            fixes: vec![],
            custom_docs_url: LintDocsUrl::Custom(format!(
              "https://typescript-eslint.io/rules/{}",
              d.code
            )),
            info,
          },
        }
      })
      .collect()
  }

  /// Record the text of every file with diagnostics, as it is now.
  fn fingerprint_files(&mut self) {
    for (key, file) in &mut self.by_file {
      file.text_hash = std::fs::read_to_string(key)
        .ok()
        .map(|text| hash_text(deno_ast::strip_bom(text).as_str()));
    }
  }
}

/// The headless request: one config for every file, with `rules`.
fn payload(files: &[String], rules: &[String]) -> Vec<u8> {
  let rules: Vec<serde_json::Value> = rules
    .iter()
    .map(|name| serde_json::json!({ "name": name }))
    .collect();
  serde_json::to_vec(&serde_json::json!({
    "version": 2,
    "configs": [{ "file_paths": files, "rules": rules }],
  }))
  .unwrap()
}

/// Run tsgolint once over `paths` (from `cwd`, where it starts looking for
/// `tsconfig.json` files) with `rules`.
pub fn run(
  bin: &Path,
  cwd: &Path,
  paths: &[PathBuf],
  rules: &[String],
) -> Result<TsgolintResults, AnyError> {
  let files: Vec<String> = paths
    .iter()
    .filter(|p| is_type_aware_path(p))
    .map(|p| normalize_key(p))
    .collect();
  if rules.is_empty() || files.is_empty() {
    return Ok(TsgolintResults {
      enabled_rules: rules.to_vec(),
      ..Default::default()
    });
  }
  let cwd = crate::util::fs::canonicalize_path(cwd)
    .unwrap_or_else(|_| cwd.to_path_buf());
  let mut child = Command::new(bin)
    .arg("headless")
    .current_dir(&cwd)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .with_context(|| format!("failed to run tsgolint ({})", bin.display()))?;
  // tsgolint reads all of stdin before it writes anything, so writing the
  // whole request and then reading the output can't deadlock. A write error
  // means it exited early; its output says why.
  let write_result = child
    .stdin
    .take()
    .expect("piped stdin")
    .write_all(&payload(&files, rules));
  let output = child.wait_with_output()?;
  let parsed = parse_frames(&output.stdout, rules);
  if !output.status.success() {
    parsed?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(10).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    bail!(
      "tsgolint ({}) exited with {}{}",
      bin.display(),
      output.status,
      if tail.is_empty() {
        String::new()
      } else {
        format!(":\n{}", tail.join("\n"))
      }
    );
  }
  write_result.context("failed to write the request to tsgolint")?;
  if !output.stderr.is_empty() {
    log::debug!("tsgolint: {}", String::from_utf8_lossy(&output.stderr));
  }
  let mut results = parsed?;
  results.fingerprint_files();
  Ok(results)
}

/// Parse tsgolint's stdout: frames of a 4-byte little-endian payload length,
/// a 1-byte message type (0 error, 1 diagnostic, 2 timing) and the JSON
/// payload.
fn parse_frames(
  buf: &[u8],
  rules: &[String],
) -> Result<TsgolintResults, AnyError> {
  let mut results = TsgolintResults {
    enabled_rules: rules.to_vec(),
    ..Default::default()
  };
  let mut keys: HashMap<String, String> = HashMap::new();
  let mut i = 0;
  while i < buf.len() {
    if i + 5 > buf.len() {
      bail!("tsgolint's output ends inside a message header");
    }
    let len =
      u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
    let msg_type = buf[i + 4];
    i += 5;
    if i + len > buf.len() {
      bail!("tsgolint's output ends inside a message");
    }
    let body = &buf[i..i + len];
    i += len;
    match msg_type {
      0 => {
        let err: HeadlessError = serde_json::from_slice(body)?;
        bail!("tsgolint: {}", err.error);
      }
      1 => {
        let d: HeadlessDiagnostic = serde_json::from_slice(body)?;
        match (d.kind, d.file_path, d.rule) {
          (0, Some(file_path), Some(rule)) => {
            let key = keys
              .entry(file_path)
              .or_insert_with_key(|p| normalize_key(Path::new(p)))
              .clone();
            results.by_file.entry(key).or_default().diagnostics.push(
              TsgolintDiagnostic {
                code: rule,
                message: d.message.description,
                hint: d.message.help.filter(|h| !h.is_empty()),
                range: d.range.map(|r| (r.pos, r.end)),
                labels: d
                  .labeled_ranges
                  .into_iter()
                  .map(|l| l.label)
                  .filter(|l| !l.is_empty())
                  .collect(),
              },
            );
          }
          (_, file_path, _) => {
            let mut message = d.message.description;
            if let Some(help) = d.message.help.filter(|h| !h.is_empty()) {
              message = format!("{message}: {help}");
            }
            if let Some(file_path) = file_path {
              message = format!("{file_path}: {message}");
            }
            results.program_diagnostics.push(message);
          }
        }
      }
      // Timing (2), and anything newer: not ours to show.
      _ => {}
    }
  }
  Ok(results)
}

#[derive(Deserialize)]
struct HeadlessError {
  error: String,
}

#[derive(Deserialize)]
struct HeadlessRange {
  pos: usize,
  end: usize,
}

#[derive(Deserialize)]
struct HeadlessMessage {
  description: String,
  #[serde(default)]
  help: Option<String>,
}

#[derive(Deserialize)]
struct HeadlessLabeledRange {
  label: String,
}

#[derive(Deserialize)]
struct HeadlessDiagnostic {
  kind: u8,
  #[serde(default)]
  range: Option<HeadlessRange>,
  message: HeadlessMessage,
  #[serde(default)]
  file_path: Option<String>,
  #[serde(default)]
  rule: Option<String>,
  #[serde(default)]
  labeled_ranges: Vec<HeadlessLabeledRange>,
}

#[cfg(test)]
mod tests {
  use super::*;

  fn frame(msg_type: u8, json: &str) -> Vec<u8> {
    let body = json.as_bytes();
    let mut v = Vec::new();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.push(msg_type);
    v.extend_from_slice(body);
    v
  }

  #[test]
  fn pin_covers_every_platform() {
    let pin = pin();
    assert!(
      lazy_regex::regex!(r"^\d+\.\d+\.\d+$").is_match(&pin.version),
      "{}",
      pin.version
    );
    for (arch, os) in [
      ("x86_64", "linux"),
      ("aarch64", "linux"),
      ("x86_64", "macos"),
      ("aarch64", "macos"),
      ("x86_64", "windows"),
      ("aarch64", "windows"),
    ] {
      let platform = native_pin::platform_for(arch, os).unwrap();
      let entry = pin
        .platforms
        .get(platform)
        .unwrap_or_else(|| panic!("no {platform}"));
      assert!(entry.integrity.starts_with("sha512-"), "{platform}");
      assert_eq!(entry.sha256.len(), 64, "{platform}");
      assert!(
        entry.tarball.ends_with(&format!(
          "/@oxlint-tsgolint/{platform}/-/{platform}-{}.tgz",
          pin.version
        )),
        "{platform}: {}",
        entry.tarball
      );
    }
  }

  #[test]
  fn rule_lists_are_consistent() {
    let mut sorted = TYPE_AWARE_RULES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, TYPE_AWARE_RULES, "sorted and unique");
    assert!(!TYPE_AWARE_RULES.contains(&"require-await"));
    for rule in DEFAULT_RULES {
      assert!(TYPE_AWARE_RULES.contains(rule), "{rule}");
    }
  }

  #[test]
  fn resolve_rules_applies_include_and_exclude() {
    let rules = resolve_rules(&LintRulesConfig {
      tags: None,
      include: Some(vec![
        "no-deprecated".to_string(),
        "totally-made-up".to_string(),
        "require-await".to_string(),
      ]),
      exclude: Some(vec!["no-floating-promises".to_string()]),
    });
    assert!(rules.contains(&"no-deprecated".to_string()));
    assert!(rules.contains(&"await-thenable".to_string()));
    assert!(!rules.contains(&"no-floating-promises".to_string()));
    assert!(!rules.contains(&"totally-made-up".to_string()));
    assert!(!rules.contains(&"require-await".to_string()));
    assert_eq!(
      resolve_rules(&LintRulesConfig::default()),
      DEFAULT_RULES
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
    );
  }

  #[test]
  fn parse_frames_collects_rule_and_program_diagnostics() {
    let mut buf = Vec::new();
    buf.extend(frame(
      1,
      r#"{"kind":0,"range":{"pos":10,"end":20},"message":{"id":"x","description":"Promises must be awaited","help":"await it"},"file_path":"/a/b.ts","labeled_ranges":[{"label":"has type Promise<void>","range":{"pos":10,"end":20}}],"rule":"no-floating-promises"}"#,
    ));
    buf.extend(frame(2, r#"{"rules":[]}"#));
    buf.extend(frame(
      1,
      r#"{"kind":1,"range":{"pos":3,"end":3},"message":{"id":"tsconfig-error","description":"Invalid tsconfig","help":"'}' expected."},"file_path":"/a/tsconfig.json"}"#,
    ));
    let results =
      parse_frames(&buf, &["no-floating-promises".to_string()]).unwrap();
    let file = results.by_file.get(&normalize_str("/a/b.ts")).unwrap();
    assert_eq!(
      file.diagnostics,
      vec![TsgolintDiagnostic {
        code: "no-floating-promises".to_string(),
        message: "Promises must be awaited".to_string(),
        hint: Some("await it".to_string()),
        range: Some((10, 20)),
        labels: vec!["has type Promise<void>".to_string()],
      }]
    );
    assert_eq!(
      results.program_diagnostics,
      vec!["/a/tsconfig.json: Invalid tsconfig: '}' expected.".to_string()]
    );
    assert_eq!(
      results.rule_codes().collect::<Vec<_>>(),
      vec![Cow::Borrowed("no-floating-promises")]
    );
  }

  #[test]
  fn parse_frames_surfaces_errors_and_truncation() {
    let err = parse_frames(&frame(0, r#"{"error":"boom"}"#), &[]).unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
    let mut truncated = frame(1, r#"{"kind":0}"#);
    truncated.pop();
    assert!(parse_frames(&truncated, &[]).is_err());
    assert!(parse_frames(&[1, 0], &[]).is_err());
    assert!(parse_frames(&[], &[]).unwrap().by_file.is_empty());
  }

  #[test]
  fn diagnostics_are_placed_only_in_the_linted_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.ts");
    let text = "async function f() {}\n// é\nf();\n";
    std::fs::write(&path, format!("\u{feff}{text}")).unwrap();
    let start = text.find("f();").unwrap();
    let mut results = TsgolintResults::default();
    results.by_file.insert(
      normalize_key(&path),
      FileDiagnostics {
        text_hash: None,
        diagnostics: vec![TsgolintDiagnostic {
          code: "no-floating-promises".to_string(),
          message: "m".to_string(),
          hint: None,
          range: Some((start, start + 3)),
          labels: vec![],
        }],
      },
    );
    results.fingerprint_files();
    let specifier = ModuleSpecifier::from_file_path(&path).unwrap();

    let same = SourceTextInfo::from_string(text.to_string());
    let diagnostics = results.diagnostics_for(&path, &specifier, &same);
    assert_eq!(diagnostics.len(), 1);
    let range = diagnostics[0].range.as_ref().unwrap();
    assert_eq!(
      range.range.as_byte_range(same.range().start),
      start..start + 3
    );
    assert!(matches!(
      &diagnostics[0].details.custom_docs_url,
      LintDocsUrl::Custom(url) if url == "https://typescript-eslint.io/rules/no-floating-promises"
    ));

    assert_eq!(diagnostics[0].details.info.len(), 0);

    // A `--fix` pass changed the text: still reported (deno_lint would drop a
    // diagnostic without a range), at the start of the file, with a note.
    let changed = SourceTextInfo::from_string(format!("// fixed\n{text}"));
    let diagnostics = results.diagnostics_for(&path, &specifier, &changed);
    assert_eq!(diagnostics.len(), 1);
    let range = diagnostics[0].range.as_ref().unwrap();
    assert_eq!(range.range.as_byte_range(changed.range().start), 0..0);
    assert!(diagnostics[0].details.info[0].contains("lint it again"));

    let other = dir.path().join("b.ts");
    assert!(
      results
        .diagnostics_for(&other, &specifier, &same)
        .is_empty()
    );
  }

  #[test]
  fn only_script_files_are_sent() {
    assert!(is_type_aware_path(Path::new("/a/b.ts")));
    assert!(is_type_aware_path(Path::new("/a/b.MJS")));
    assert!(!is_type_aware_path(Path::new("/a/b")));
    assert!(!is_type_aware_path(Path::new("/a/b.json")));
  }

  fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
      Vec::new(),
      flate2::Compression::fast(),
    ));
    for (path, data) in files {
      let mut header = tar::Header::new_gnu();
      header.set_size(data.len() as u64);
      header.set_mode(0o755);
      header.set_cksum();
      builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
  }

  fn integrity_of(bytes: &[u8]) -> String {
    use base64::Engine;
    format!(
      "sha512-{}",
      base64::engine::general_purpose::STANDARD
        .encode(sha2::Sha512::digest(bytes))
    )
  }

  #[test]
  fn tarball_is_checked_against_both_pins() {
    let bytes = tgz(&[
      ("package/LICENSE", b"MIT"),
      ("package/tsgolint", b"the binary"),
      ("package/package.json", b"{}"),
    ]);
    let pinned = PinnedBinary {
      tarball: "https://example.invalid/t.tgz".to_string(),
      integrity: integrity_of(&bytes),
      sha256: sha256_hex(b"the binary"),
    };
    assert_eq!(
      verify_tarball(&bytes, &pinned, "tsgolint").unwrap(),
      b"the binary"
    );
    // The tarball is the pinned one but holds another binary.
    let wrong_binary = PinnedBinary {
      sha256: sha256_hex(b"another binary"),
      ..pinned.clone()
    };
    let err = verify_tarball(&bytes, &wrong_binary, "tsgolint").unwrap_err();
    assert!(
      err.to_string().contains("does not match the pinned"),
      "{err}"
    );
    // Another tarball.
    let wrong_tarball = PinnedBinary {
      integrity: integrity_of(b"other"),
      ..pinned.clone()
    };
    assert!(verify_tarball(&bytes, &wrong_tarball, "tsgolint").is_err());
    // No binary for this platform in it.
    assert!(verify_tarball(&bytes, &pinned, "tsgolint.exe").is_err());
  }

  #[test]
  fn installed_binary_is_reused_while_it_matches_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let install_dir = dir.path().join("dl").join("platform");
    let pinned = PinnedBinary {
      tarball: "https://example.invalid/t.tgz".to_string(),
      integrity: integrity_of(b"tarball"),
      sha256: sha256_hex(b"the binary"),
    };
    assert_eq!(cached_bin(&install_dir, &pinned), None);
    let exe = install_binary(&install_dir, b"the binary", &pinned).unwrap();
    assert_eq!(exe, install_dir.join(exe_name()));
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
      assert_eq!(mode & 0o111, 0o111);
    }
    // Offline from here on: the cached build is found without a download.
    assert_eq!(cached_bin(&install_dir, &pinned), Some(exe.clone()));
    // A changed binary is not used.
    std::fs::write(&exe, b"tampered").unwrap();
    assert_eq!(cached_bin(&install_dir, &pinned), None);
    // Nothing left behind but the binary.
    let names: Vec<_> = std::fs::read_dir(&install_dir)
      .unwrap()
      .flatten()
      .map(|e| e.file_name())
      .collect();
    assert_eq!(names, vec![std::ffi::OsString::from(exe_name())]);
  }

  #[test]
  fn local_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let platform = native_pin::host_platform().unwrap_or("linux-x64");
    // The override must exist, and is used as is.
    assert!(
      find_local(
        Some("/definitely/not/here/tsgolint".into()),
        None,
        None,
        platform
      )
      .is_err()
    );
    let custom = dir.path().join("custom-tsgolint");
    std::fs::write(&custom, b"x").unwrap();
    assert_eq!(
      find_local(Some(custom.clone().into()), None, None, platform).unwrap(),
      Some(custom)
    );
    // A tsgolint beside deno that is not the pinned build is ignored.
    let vendored = dir.path().join(VENDORED_DIR_NAME);
    std::fs::create_dir_all(&vendored).unwrap();
    std::fs::write(vendored.join(exe_name()), b"not the pinned build").unwrap();
    assert_eq!(
      find_local(None, Some(dir.path().join("deno")), None, platform).unwrap(),
      None
    );
    // Nothing in an empty toolchain cache either.
    assert_eq!(
      find_local(None, None, Some(dir.path()), platform).unwrap(),
      None
    );
  }
}
