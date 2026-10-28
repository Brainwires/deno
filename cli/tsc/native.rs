// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;

use deno_core::anyhow;
use deno_core::error::AnyError;
use deno_core::serde_json::Value;
use regex::Regex;

use crate::tsc::Diagnostic;
use crate::tsc::DiagnosticCategory;
use crate::tsc::Position;
use crate::tsc::native_pin;

/// The role in `typescript_pins.json` (see [`native_pin`]) of the native
/// TypeScript compiler (`typescript@7`, "tsgo") that type checking runs.
/// `tools/update_typescript_pin.ts check <version>` is the only supported way
/// to change it: a given release always type-checks with a known compiler.
pub const CHECK_ROLE: &str = "check";

/// The pinned TypeScript version type checking runs (folded into the check
/// cache's hash).
pub fn typescript_version() -> &'static str {
  &native_pin::pin(CHECK_ROLE).version
}

/// Environment variable naming a native `tsc` executable to use instead of the
/// one vendored next to `deno` (denext).
pub const DENO_TSGO_BIN_ENV: &str = "DENO_TSGO_BIN";

/// The cache directory (below `DENO_DIR`) where denext's managed toolchain
/// installs the `deno` executable, one `<version>/<target>/` per release.
pub(crate) const TOOLCHAIN_CACHE_NAME: &str = "denext-deno";

/// Where to look for the compiler, in order: `DENO_TSGO_BIN`; the
/// `typescript-check/` directory beside the running executable, what denext's
/// CLI archives ship (also through a symlink to it, e.g.
/// `denext toolchain link`); then denext's toolchain cache
/// (`<DENO_DIR>/denext-deno/<version>/<target>/typescript-check/`, newest
/// release first), for a fork build run from elsewhere such as
/// `target/debug/deno`. Never downloaded: when none is found, an error names
/// `denext toolchain install`.
fn resolve_native_tsc(
  override_path: Option<std::ffi::OsString>,
  current_exe: Option<PathBuf>,
  deno_dir_root: Option<&Path>,
) -> Result<PathBuf, AnyError> {
  if let Some(found) = native_pin::find_local(
    CHECK_ROLE,
    DENO_TSGO_BIN_ENV,
    override_path,
    current_exe.clone(),
  )? {
    return Ok(found);
  }
  let real_exe = current_exe
    .as_deref()
    .and_then(|exe| crate::util::fs::canonicalize_path(exe).ok())
    .filter(|real| Some(real) != current_exe.as_ref());
  if let Some(found) =
    native_pin::find_local(CHECK_ROLE, DENO_TSGO_BIN_ENV, None, real_exe)?
  {
    return Ok(found);
  }
  if let Some(root) = deno_dir_root
    && let Some(found) = toolchain_cache_tsc(&root.join(TOOLCHAIN_CACHE_NAME))
  {
    log::debug!(
      "using the native tsc from denext's toolchain cache: {}",
      found.display()
    );
    return Ok(found);
  }
  let version = typescript_version();
  anyhow::bail!(
    "The native TypeScript compiler (typescript@{version}) was not found next to {}.\n  \
     It ships in denext's Deno toolchain: run `denext toolchain install`.\n  \
     Or set {DENO_TSGO_BIN_ENV} to a tsc {version} binary, or type-check with the \
     in-process TypeScript 6.0.3 instead: --check-engine=tsc (DENO_CHECK_ENGINE=tsc).",
    current_exe
      .as_deref()
      .and_then(Path::parent)
      .map(|d| d.display().to_string())
      .unwrap_or_else(|| "the deno executable".to_string())
  )
}

/// A vendored compiler in denext's toolchain cache:
/// `<cache>/<version>/<target>/typescript-check/tsc`, newest release first
/// (by semver: `3.0.0-rc.10` after `3.0.0-rc.9`, after `2.9.7-denext.22`).
fn toolchain_cache_tsc(cache: &Path) -> Option<PathBuf> {
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
      // `find_local` looks beside an executable: point it at one in `target`.
      if let Ok(Some(found)) = native_pin::find_local(
        CHECK_ROLE,
        DENO_TSGO_BIN_ENV,
        None,
        Some(target.path().join("deno")),
      ) {
        return Some(found);
      }
    }
  }
  None
}

/// A toolchain cache directory's sort key: its name as a version (`None`,
/// which sorts first, for a name that is not one), then the name.
pub(crate) fn toolchain_version_key(
  dir: &Path,
) -> (Option<deno_semver::Version>, std::ffi::OsString) {
  let name = dir.file_name().unwrap_or_default().to_os_string();
  let version = name
    .to_str()
    .and_then(|n| deno_semver::Version::parse_standard(n).ok());
  (version, name)
}

/// The native `tsc` to run: never downloaded. See [`resolve_native_tsc`] for
/// the lookup order and the error when it is missing.
pub fn find_native_tsc(
  deno_dir_root: Option<&Path>,
) -> Result<PathBuf, AnyError> {
  resolve_native_tsc(
    std::env::var_os(DENO_TSGO_BIN_ENV),
    std::env::current_exe().ok(),
    deno_dir_root,
  )
}

/// Run the native `tsc` at `tsc_path` against `tsconfig_path` and capture its
/// output. `--pretty false` yields the stable
/// `path(line,col): error TS####: message` format; `--diagnostics` appends a
/// stats block.
pub async fn run_native_tsc(
  tsc_path: &Path,
  tsconfig_path: &Path,
  project_root: &Path,
) -> Result<std::process::Output, AnyError> {
  let mut command = tokio::process::Command::new(tsc_path);
  command
    .arg("--project")
    .arg(tsconfig_path)
    .arg("--noEmit")
    // denext: the pretty form also carries each diagnostic's exact span and
    // its related information (parse_tsc_pretty_diagnostics).
    .arg("--pretty")
    .arg("true")
    .arg("--diagnostics")
    .current_dir(project_root)
    // The native compiler is written in Go, whose `os.Getwd` trusts the `PWD`
    // environment variable over `getcwd()`. `current_dir` calls `chdir` but
    // does not update the inherited `PWD`, so a symlinked launch directory
    // (e.g. `/tmp` -> `/private/tmp` on macOS) would leave `PWD` pointing at
    // the symlink and make tsc report every path relative to it. Pin `PWD` to
    // the same directory we chdir'd into.
    .env("PWD", project_root)
    // `output()` would default stdin to null; `spawn()` doesn't.
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  // A check can run while the program spawns its own children (a dynamic
  // import under `--check`): on Windows, never while one of those holds its
  // child's pipe ends, which this spawn would inherit.
  #[cfg(windows)]
  let child =
    deno_subprocess_windows::with_exclusive_inheritance(|| command.spawn());
  #[cfg(not(windows))]
  let child = command.spawn();
  let child =
    child.map_err(|e| anyhow::anyhow!("failed to run native tsc: {e}"))?;
  child
    .wait_with_output()
    .await
    .map_err(|e| anyhow::anyhow!("failed to run native tsc: {e}"))
}

/// A single `path(line,col): error TS####: message` line from `tsc --pretty
/// false`. Continuation lines (indented elaborations) are folded into the
/// preceding diagnostic's message.
static DIAGNOSTIC_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(
    r"^(?P<file>.+?)\((?P<line>\d+),(?P<col>\d+)\): (?P<cat>error|warning|message) TS(?P<code>\d+): (?P<msg>.*)$",
  )
  .unwrap()
});

/// The position-less form (`error TS####: message`), e.g. `TS18003` (no
/// inputs) or a `TS5###` config error.
static DIAGNOSTIC_NO_POS_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"^(?P<cat>error|warning|message) TS(?P<code>\d+): (?P<msg>.*)$")
    .unwrap()
});

/// Whether `tsc` output contains any recognizable diagnostic line. Used to tell
/// a genuine failure (tsc printed nothing parseable) from an expected non-zero
/// exit after we filtered every diagnostic it did print.
pub fn output_has_diagnostics(output: &str) -> bool {
  output.lines().any(|l| {
    let l = ANSI_RE.replace_all(l, "");
    DIAGNOSTIC_RE.is_match(&l)
      || DIAGNOSTIC_NO_POS_RE.is_match(&l)
      || PRETTY_HEADER_RE.is_match(&l)
  })
}

/// Parse `tsc --pretty false` (or `--pretty true`, see
/// [`parse_tsc_pretty_diagnostics`]) output into Deno diagnostics, remapping the
/// compiler's mirror paths back onto the specifiers the user wrote.
pub fn parse_tsc_diagnostics(
  output: &str,
  project_root: &Path,
) -> Vec<Diagnostic> {
  let mut diagnostics: Vec<Diagnostic> = Vec::new();
  // Cache of source files read to reconstruct the underlined snippet. The
  // compiler reports positions but not the source line itself, so we read it
  // back from the same file it referred to (which is always on disk: the
  // project sources, or the materialized jsr:/http(s): mirrors).
  let mut source_cache: HashMap<PathBuf, Option<Vec<String>>> = HashMap::new();
  // Reverse map (mirror path -> original URL) so remote diagnostics render the
  // specifier the user wrote, with the correct scheme and port.
  let remote_map = build_remote_url_map(project_root);

  let pretty = is_pretty_output(output);
  if pretty {
    diagnostics = parse_tsc_pretty_diagnostics(
      output,
      project_root,
      &remote_map,
      &mut source_cache,
    );
  }
  for line in output.lines().filter(|_| !pretty) {
    if let Some(caps) = DIAGNOSTIC_RE.captures(line) {
      let line_no: u64 = caps["line"].parse().unwrap_or(1);
      let col_no: u64 = caps["col"].parse().unwrap_or(1);
      // tsc positions are 1-based; Deno's `Position` is 0-based and adds one
      // back when rendering.
      let start = Position {
        line: line_no.saturating_sub(1),
        character: col_no.saturating_sub(1),
      };
      let source_line = read_source_line(
        &mut source_cache,
        &project_root.join(&caps["file"]),
        start.line as usize,
      );
      // The grep format has no end position, so underline the token that
      // starts at the reported column (an identifier, a quoted specifier, or a
      // single caret) as a best-effort span.
      let end = source_line.as_deref().map(|sl| Position {
        line: start.line,
        character: start.character
          + underline_len(sl, start.character as usize),
      });
      diagnostics.push(make_diagnostic(
        &caps["cat"],
        caps["code"].parse().unwrap_or(0),
        Some(remap_path(&caps["file"], project_root, &remote_map)),
        Some(start),
        end,
        source_line,
        caps["msg"].to_string(),
      ));
    } else if let Some(caps) = DIAGNOSTIC_NO_POS_RE.captures(line) {
      diagnostics.push(make_diagnostic(
        &caps["cat"],
        caps["code"].parse().unwrap_or(0),
        None,
        None,
        None,
        None,
        caps["msg"].to_string(),
      ));
    } else if line.starts_with([' ', '\t']) && !line.trim().is_empty() {
      // Indented elaboration for the previous diagnostic.
      if let Some(last) = diagnostics.last_mut() {
        let message = last.message_text.get_or_insert_with(String::new);
        message.push('\n');
        message.push_str(line.trim_end());
      }
    }
    // Any other line (blank lines, a trailing "Found N errors" summary) is
    // ignored.
  }

  // `--all` type-checks remote modules for genuine type errors, but Deno doesn't
  // apply the `noImplicitOverride` style rule to code that isn't the user's, so
  // drop override-modifier diagnostics (the TS411x family) reported in a
  // non-local (remote/jsr/npm) module. Local `file://` modules keep them.
  diagnostics.retain(|d| {
    // TS4114..=4116 are the `noImplicitOverride` family (a Deno default we don't
    // apply to code that isn't the user's). TS4113 ("member cannot have an
    // 'override' modifier because it is not declared in the base class") is an
    // unconditional error and must never be dropped.
    !matches!(d.code, 4114..=4116)
      || d
        .file_name
        .as_deref()
        .is_none_or(|f| f.starts_with("file:"))
  });

  diagnostics
}

/// The header of a diagnostic in `tsc --pretty true` output (ANSI stripped):
/// `path:line:col - error TS####: message`.
static PRETTY_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(
    r"^(?P<file>[^\s].*?):(?P<line>\d+):(?P<col>\d+) - (?P<cat>error|warning|message) TS(?P<code>\d+): (?P<msg>.*)$",
  )
  .unwrap()
});

/// A related-information entry under a pretty diagnostic, indented by two:
/// `  path:line:col - message`.
static PRETTY_RELATED_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"^  (?P<file>[^\s].*?):(?P<line>\d+):(?P<col>\d+) - (?P<msg>.*)$")
    .unwrap()
});

/// A source line of a pretty code frame: `<indent><line number> <source>`.
static PRETTY_FRAME_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"^(?P<indent> *)(?P<num>\d+) (?P<src>.*)$").unwrap()
});

static ANSI_RE: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

/// Whether `output` is `tsc --pretty true` output (as `run_native_tsc` asks
/// for), rather than the `--pretty false` form.
fn is_pretty_output(output: &str) -> bool {
  output.contains('\x1b')
    || output.lines().any(|l| {
      let l = ANSI_RE.replace_all(l, "");
      PRETTY_HEADER_RE.is_match(&l) || PRETTY_RELATED_RE.is_match(&l)
    })
}

/// The underlined span of the code frame that follows `lines[at]` (a header or
/// related line): its 0-based start character and length, from the first
/// source line and the `~` run under it.
fn pretty_frame_span(lines: &[String], at: usize) -> Option<(u64, u64)> {
  let mut i = at + 1;
  // the frame may follow a blank line
  while i < lines.len() && lines[i].trim().is_empty() {
    i += 1;
  }
  let caps = PRETTY_FRAME_LINE_RE.captures(lines.get(i)?)?;
  let prefix = caps["indent"].len() + caps["num"].len() + 1;
  let underline = lines.get(i + 1)?;
  let first = underline.find('~')?;
  if first < prefix {
    return None;
  }
  let len = underline[first..].chars().take_while(|&c| c == '~').count();
  Some((underline[prefix..first].chars().count() as u64, len as u64))
}

/// Parse `tsc --pretty true` output: like [`parse_tsc_diagnostics`] for
/// `--pretty false`, plus the exact underlined span of each diagnostic and its
/// related information ("The expected type comes from ... declared here"),
/// which only the pretty form reports (denext: the in-process checker shows
/// both, and denext's diagnostic explanations read them).
fn parse_tsc_pretty_diagnostics(
  output: &str,
  project_root: &Path,
  remote_map: &HashMap<String, String>,
  source_cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
) -> Vec<Diagnostic> {
  let lines: Vec<String> = output
    .lines()
    .map(|l| ANSI_RE.replace_all(l, "").into_owned())
    .collect();
  let mut diagnostics: Vec<Diagnostic> = Vec::new();
  // Whether the lines after the current header still continue its message
  // chain (they do until the first blank line or code frame).
  let mut in_chain = false;
  let located =
    |file: &str,
     line: &str,
     col: &str,
     span: Option<(u64, u64)>,
     cache: &mut HashMap<PathBuf, Option<Vec<String>>>| {
      let start = Position {
        line: line.parse::<u64>().unwrap_or(1).saturating_sub(1),
        character: col.parse::<u64>().unwrap_or(1).saturating_sub(1),
      };
      let source_line =
        read_source_line(cache, &project_root.join(file), start.line as usize);
      let len = match span {
        Some((_, len)) if len > 0 => Some(len),
        _ => source_line
          .as_deref()
          .map(|sl| underline_len(sl, start.character as usize)),
      };
      let end = len.map(|len| Position {
        line: start.line,
        character: start.character + len,
      });
      (
        Some(remap_path(file, project_root, remote_map)),
        start,
        end,
        source_line,
      )
    };
  for (i, line) in lines.iter().enumerate() {
    if let Some(caps) = PRETTY_HEADER_RE.captures(line)
      && is_check_config(&caps["file"])
    {
      // denext: an option's diagnostic located in the config Deno wrote for
      // this run (the user's `tsconfig.json` re-serialized on top of the
      // generated one) has no line of the user's own: shown without one.
      diagnostics.push(make_diagnostic(
        &caps["cat"],
        caps["code"].parse().unwrap_or(0),
        None,
        None,
        None,
        None,
        caps["msg"].to_string(),
      ));
      in_chain = true;
    } else if let Some(caps) = PRETTY_HEADER_RE.captures(line) {
      let span = pretty_frame_span(&lines, i);
      let (file_name, start, end, source_line) = located(
        &caps["file"],
        &caps["line"],
        &caps["col"],
        span,
        source_cache,
      );
      diagnostics.push(make_diagnostic(
        &caps["cat"],
        caps["code"].parse().unwrap_or(0),
        file_name,
        Some(start),
        end,
        source_line,
        caps["msg"].to_string(),
      ));
      in_chain = true;
    } else if let Some(caps) = DIAGNOSTIC_NO_POS_RE.captures(line) {
      diagnostics.push(make_diagnostic(
        &caps["cat"],
        caps["code"].parse().unwrap_or(0),
        None,
        None,
        None,
        None,
        caps["msg"].to_string(),
      ));
      in_chain = true;
    } else if line.trim().is_empty() {
      in_chain = false;
    } else if let Some(caps) = PRETTY_RELATED_RE.captures(line)
      && let Some(last) = diagnostics.last_mut()
    {
      let span = pretty_frame_span(&lines, i);
      let (file_name, start, end, source_line) = located(
        &caps["file"],
        &caps["line"],
        &caps["col"],
        span,
        source_cache,
      );
      let related = make_diagnostic(
        "message",
        0,
        file_name,
        Some(start),
        end,
        source_line,
        caps["msg"].to_string(),
      );
      // A location in the generated config ("matched by 'files' here") is the
      // config Deno wrote, not the user's code: not shown.
      if !caps["file"].ends_with(".json") {
        last
          .related_information
          .get_or_insert_with(Vec::new)
          .push(related);
      }
      in_chain = false;
    } else if in_chain
      && line.starts_with([' ', '\t'])
      && let Some(last) = diagnostics.last_mut()
    {
      let message = last.message_text.get_or_insert_with(String::new);
      message.push('\n');
      message.push_str(line.trim_end());
    }
    // Code frames, the "Found N errors" summary and the `--diagnostics`
    // statistics are not diagnostics.
  }
  diagnostics
}

/// Whether `file` (as the compiler prints it) is a config `deno check` wrote
/// for the run: `deno-check-root-*.tsconfig.json` in the project root (or
/// `deno-check-*.tsconfig.json` in the temp dir) or `.deno/tsconfig.json`.
fn is_check_config(file: &str) -> bool {
  let file = file.replace('\\', "/");
  let name = file.rsplit('/').next().unwrap_or(&file);
  (name.starts_with("deno-check-") && name.ends_with(".tsconfig.json"))
    || file.ends_with(".deno/tsconfig.json")
}

/// Compiler statistics reported by `tsc --diagnostics` (how many files/lines
/// were checked, timing, memory). Captured for an upcoming user-facing
/// "Checked N files" summary; currently only logged at debug level.
#[derive(Debug, Default)]
pub struct TscStats {
  pub files: Option<u64>,
  pub lines: Option<u64>,
  pub identifiers: Option<u64>,
  pub symbols: Option<u64>,
  pub types: Option<u64>,
  pub memory_used: Option<String>,
  pub check_time: Option<String>,
  pub total_time: Option<String>,
}

/// Parse the `Key:  value` stats block emitted by `tsc --diagnostics`. Lines
/// that aren't recognized stat keys (including the diagnostics themselves) are
/// ignored.
pub fn parse_tsc_stats(output: &str) -> TscStats {
  let mut stats = TscStats::default();
  for line in output.lines() {
    let Some((key, value)) = line.split_once(':') else {
      continue;
    };
    let value = value.trim();
    if value.is_empty() {
      continue;
    }
    match key.trim() {
      "Files" => stats.files = value.parse().ok(),
      "Lines" => stats.lines = value.parse().ok(),
      "Identifiers" => stats.identifiers = value.parse().ok(),
      "Symbols" => stats.symbols = value.parse().ok(),
      "Types" => stats.types = value.parse().ok(),
      "Memory used" => stats.memory_used = Some(value.to_string()),
      "Check time" => stats.check_time = Some(value.to_string()),
      "Total time" => stats.total_time = Some(value.to_string()),
      _ => {}
    }
  }
  stats
}

/// Read line `line_idx` (0-based) of `path`, caching the file's lines. Returns
/// `None` if the file can't be read or the line is out of range.
fn read_source_line(
  cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
  path: &Path,
  line_idx: usize,
) -> Option<String> {
  let lines = cache.entry(path.to_path_buf()).or_insert_with(|| {
    std::fs::read_to_string(path)
      .ok()
      .map(|s| s.lines().map(str::to_string).collect())
  });
  lines.as_ref()?.get(line_idx).cloned()
}

/// Best-effort length (in characters) of the token to underline starting at
/// `start_char` on `line`: a quoted string (including its quotes), an
/// identifier run, or a single character otherwise.
fn underline_len(line: &str, start_char: usize) -> u64 {
  let chars: Vec<char> = line.chars().collect();
  let Some(&first) = chars.get(start_char) else {
    return 1;
  };
  if matches!(first, '\'' | '"' | '`') {
    if let Some(offset) =
      chars[start_char + 1..].iter().position(|&c| c == first)
    {
      return offset as u64 + 2; // opening quote + contents + closing quote
    }
    return 1;
  }
  if is_ident_char(first) {
    let len = chars[start_char..]
      .iter()
      .take_while(|&&c| is_ident_char(c))
      .count();
    return len as u64;
  }
  1
}

fn is_ident_char(c: char) -> bool {
  c.is_alphanumeric() || c == '_' || c == '$'
}

fn make_diagnostic(
  category: &str,
  code: u64,
  file_name: Option<String>,
  start: Option<Position>,
  end: Option<Position>,
  source_line: Option<String>,
  message_text: String,
) -> Diagnostic {
  Diagnostic {
    category: match category {
      "warning" => DiagnosticCategory::Warning,
      "message" => DiagnosticCategory::Message,
      _ => DiagnosticCategory::Error,
    },
    code,
    start,
    end,
    original_source_start: None,
    message_text: Some(message_text),
    message_chain: None,
    source: None,
    source_line,
    file_name,
    related_information: None,
    reports_deprecated: None,
    reports_unnecessary: None,
    other: Default::default(),
    missing_specifier: None,
  }
}

/// Build a reverse map from a mirror-relative path (the part after
/// `.deno/remote/`, i.e. `<host>__<port>/<path>`) to the original `http(s):`
/// URL, read from the generated tsconfig's `paths`. The mirror layout doesn't
/// encode the URL scheme, so this recovers the exact specifier (scheme + port)
/// for diagnostic display. Returns an empty map if the config can't be read.
fn build_remote_url_map(project_root: &Path) -> HashMap<String, String> {
  let config = project_root.join(".deno").join("tsconfig.json");
  let Ok(text) = std::fs::read_to_string(&config) else {
    return HashMap::new();
  };
  let Ok(value) = deno_core::serde_json::from_str::<Value>(&text) else {
    return HashMap::new();
  };
  build_remote_url_map_from_config(&value)
}

/// The pure mapping used by [`build_remote_url_map`]: given a parsed generated
/// tsconfig, extract the `http(s):` `paths` entries as `<mirror-rel> -> <url>`.
/// Split out so it can be unit-tested without touching the filesystem.
fn build_remote_url_map_from_config(config: &Value) -> HashMap<String, String> {
  let mut map = HashMap::new();
  let Some(paths) = config
    .get("compilerOptions")
    .and_then(|c| c.get("paths"))
    .and_then(|p| p.as_object())
  else {
    return map;
  };
  for (url, targets) in paths {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
      continue;
    }
    if let Some(rel) = targets
      .as_array()
      .and_then(|a| a.first())
      .and_then(|v| v.as_str())
      .and_then(|t| t.strip_prefix("./remote/"))
    {
      map.insert(rel.to_string(), url.clone());
    }
  }
  map
}

/// Best-effort remap of a path reported by the native compiler back onto the
/// original module specifier. `deno check` runs tsc against a generated
/// tsconfig whose `paths` point jsr:/http(s): dependencies into mirror
/// directories, so the compiler reports those mirror paths rather than the
/// specifiers the user wrote.
fn remap_path(
  raw: &str,
  project_root: &Path,
  remote_map: &HashMap<String, String>,
) -> String {
  let normalized = raw.replace('\\', "/");

  // http(s): dependencies mirrored under `.deno/remote/<host>/<path>`. The
  // mirror layout drops the URL scheme (and encodes `:port` as `__port`), so
  // recover the exact original URL from the generated tsconfig's `paths`; fall
  // back to a best-effort `https://` reconstruction when it isn't found.
  if let Some(rest) = normalized.split(".deno/remote/").nth(1) {
    if let Some(url) = remote_map.get(rest) {
      return url.clone();
    }
    return format!("https://{rest}");
  }

  // jsr: dependencies installed under `node_modules/@jsr/<scope>__<name>/...`.
  if let Some(rest) = normalized.split("node_modules/@jsr/").nth(1)
    && let Some((pkg, sub)) = rest.split_once('/')
    && let Some((scope, name)) = pkg.split_once("__")
  {
    return format!("jsr:@{scope}/{name}/{sub}");
  }

  // Other npm dependencies under `node_modules/<pkg>/...`.
  if let Some(rest) = normalized.split("node_modules/").nth(1) {
    return format!("npm:{rest}");
  }

  // A file in the project itself. tsc reports it relative to the project root
  // (the process cwd); render it as an absolute `file://` URL like the rest of
  // Deno's diagnostics.
  let path = Path::new(raw);
  let absolute = if path.is_absolute() {
    path.to_path_buf()
  } else {
    project_root.join(path)
  };
  match deno_path_util::url_from_file_path(&absolute) {
    Ok(url) => url.to_string(),
    Err(_) => raw.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_diagnostics_positioned() {
    // `project_root` doesn't exist on disk, so `source_line`/`end` stay `None`;
    // the parse of code/category/position/message/path is what's under test.
    // Use an absolute temp dir so the local-file -> `file://` URL mapping works
    // on Windows too (a bare "/project" isn't absolute there).
    let root = std::env::temp_dir();
    let diags = parse_tsc_diagnostics(
      "mod.ts(2,7): error TS2322: Type 'string' is not assignable to type 'number'.\n",
      &root,
    );
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d.category, DiagnosticCategory::Error);
    assert_eq!(d.code, 2322);
    // 1-based (2,7) becomes 0-based (1,6).
    let start = d.start.as_ref().unwrap();
    assert_eq!((start.line, start.character), (1, 6));
    let file_name = d.file_name.as_deref().unwrap();
    assert!(file_name.starts_with("file://"), "{file_name}");
    assert!(file_name.ends_with("/mod.ts"), "{file_name}");
  }

  #[test]
  fn parse_diagnostics_warning_and_no_position() {
    let diags = parse_tsc_diagnostics(
      "a.ts(1,1): warning TS6133: 'x' is declared but never used.\nerror TS18003: No inputs were found in config file.\n",
      Path::new("/p"),
    );
    assert_eq!(diags.len(), 2);
    assert_eq!(diags[0].category, DiagnosticCategory::Warning);
    assert_eq!(diags[0].code, 6133);
    assert_eq!(diags[1].code, 18003);
    // The position-less form has no file or start.
    assert!(diags[1].file_name.is_none());
    assert!(diags[1].start.is_none());
  }

  #[test]
  fn parse_diagnostics_folds_elaboration() {
    let diags = parse_tsc_diagnostics(
      "a.ts(1,1): error TS2322: Type 'A' is not assignable to type 'B'.\n  Types of property 'x' are incompatible.\n",
      Path::new("/p"),
    );
    assert_eq!(diags.len(), 1);
    assert_eq!(
      diags[0].message_text.as_deref(),
      Some(
        "Type 'A' is not assignable to type 'B'.\n  Types of property 'x' are incompatible."
      )
    );
  }

  #[test]
  fn parse_diagnostics_ignores_stats_and_summary() {
    let diags = parse_tsc_diagnostics(
      "a.ts(1,1): error TS1: boom\nFiles:             10\nFound 1 error.\n",
      Path::new("/p"),
    );
    assert_eq!(diags.len(), 1);
  }

  #[test]
  fn remap_path_variants() {
    // Absolute temp dir so the local-file case yields a valid `file://` URL on
    // Windows (the mirror cases below are pure string transforms).
    let root = std::env::temp_dir();
    let root = root.as_path();
    let empty = HashMap::new();
    assert_eq!(
      remap_path(
        ".deno/remote/html.spec.whatwg.org/entities.json",
        root,
        &empty
      ),
      "https://html.spec.whatwg.org/entities.json"
    );
    // With a reverse map, the exact URL (scheme + port) is recovered.
    let mut remote_map = HashMap::new();
    remote_map.insert(
      "localhost__4545/subdir/type_error.ts".to_string(),
      "http://localhost:4545/subdir/type_error.ts".to_string(),
    );
    assert_eq!(
      remap_path(
        ".deno/remote/localhost__4545/subdir/type_error.ts",
        root,
        &remote_map
      ),
      "http://localhost:4545/subdir/type_error.ts"
    );
    assert_eq!(
      remap_path("node_modules/@jsr/std__assert/mod.ts", root, &empty),
      "jsr:@std/assert/mod.ts"
    );
    assert_eq!(
      remap_path("node_modules/chalk/index.d.ts", root, &empty),
      "npm:chalk/index.d.ts"
    );
    // A backslash-separated jsr mirror path (Windows form) still remaps.
    assert_eq!(
      remap_path("node_modules\\@jsr\\std__fmt/colors.ts", root, &empty),
      "jsr:@std/fmt/colors.ts"
    );
    // A project-local file becomes an absolute file URL.
    let mapped = remap_path("src/mod.ts", root, &empty);
    assert!(mapped.starts_with("file://"), "{mapped}");
    assert!(mapped.ends_with("/src/mod.ts"), "{mapped}");
  }

  #[test]
  fn build_remote_url_map_reads_http_paths() {
    // Only `http(s):` keys whose target lives under `./remote/` are extracted,
    // keyed by the mirror-relative path; other `paths` entries are ignored.
    let config = deno_core::serde_json::json!({
      "compilerOptions": {
        "paths": {
          "https://deno.land/std/fmt/colors.ts": [
            "./remote/deno.land/std/fmt/colors.ts"
          ],
          "http://localhost:4545/mod.ts": ["./remote/localhost__4545/mod.ts"],
          "npm:chalk": ["./npm/chalk"],
        }
      }
    });
    let map = build_remote_url_map_from_config(&config);
    assert_eq!(map.len(), 2);
    assert_eq!(
      map.get("deno.land/std/fmt/colors.ts").map(String::as_str),
      Some("https://deno.land/std/fmt/colors.ts")
    );
    assert_eq!(
      map.get("localhost__4545/mod.ts").map(String::as_str),
      Some("http://localhost:4545/mod.ts")
    );
  }

  #[test]
  fn build_remote_url_map_no_paths() {
    let config = deno_core::serde_json::json!({ "compilerOptions": {} });
    assert!(build_remote_url_map_from_config(&config).is_empty());
  }

  #[test]
  fn underline_len_tokens() {
    // Identifier run.
    assert_eq!(underline_len("const foo = 1;", 6), 3);
    // Quoted specifier, including both quotes.
    assert_eq!(underline_len("import x from \"graphviz\";", 14), 10);
    // Single non-identifier character.
    assert_eq!(underline_len("= 1", 0), 1);
    // Out of range.
    assert_eq!(underline_len("abc", 10), 1);
  }

  #[test]
  fn parse_stats_block() {
    let stats = parse_tsc_stats(
      "a.ts(1,1): error TS1: boom\nFiles:             1396\nLines:           314608\nCheck time:      0.546s\nTotal time:      0.729s\nMemory used:    555106K\n",
    );
    assert_eq!(stats.files, Some(1396));
    assert_eq!(stats.lines, Some(314608));
    assert_eq!(stats.check_time.as_deref(), Some("0.546s"));
    assert_eq!(stats.total_time.as_deref(), Some("0.729s"));
    assert_eq!(stats.memory_used.as_deref(), Some("555106K"));
  }
}

#[cfg(all(test, unix))]
mod resolve_tests {
  use std::os::unix::fs::PermissionsExt;

  use super::*;

  /// A stand-in `typescript-check/tsc` in `dir` that prints
  /// `Version <version>`.
  fn fake_tsc(dir: &Path, version: &str) -> PathBuf {
    let tsgo = dir.join(native_pin::vendored_dir_name(CHECK_ROLE));
    std::fs::create_dir_all(&tsgo).unwrap();
    let path = tsgo.join("tsc");
    std::fs::write(&path, format!("#!/bin/sh\necho Version {version}\n"))
      .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
      .unwrap();
    path
  }

  #[test]
  fn vendored_sibling_of_the_pinned_version_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let tsc = fake_tsc(dir.path(), typescript_version());
    let found =
      resolve_native_tsc(None, Some(dir.path().join("deno")), None).unwrap();
    assert_eq!(found, tsc);
  }

  #[test]
  fn vendored_sibling_through_a_symlinked_exe_is_used() {
    let real = tempfile::tempdir().unwrap();
    let tsc = fake_tsc(real.path(), typescript_version());
    std::fs::write(real.path().join("deno"), "").unwrap();
    let link_dir = tempfile::tempdir().unwrap();
    let link = link_dir.path().join("deno");
    std::os::unix::fs::symlink(real.path().join("deno"), &link).unwrap();
    let found = resolve_native_tsc(None, Some(link), None).unwrap();
    assert_eq!(
      crate::util::fs::canonicalize_path(&found).unwrap(),
      crate::util::fs::canonicalize_path(&tsc).unwrap()
    );
  }

  #[test]
  fn sibling_of_another_version_is_ignored_and_the_error_names_the_toolchain() {
    let dir = tempfile::tempdir().unwrap();
    fake_tsc(dir.path(), "6.0.3");
    let err = resolve_native_tsc(None, Some(dir.path().join("deno")), None)
      .unwrap_err()
      .to_string();
    assert!(err.contains("denext toolchain install"), "{err}");
    assert!(err.contains(DENO_TSGO_BIN_ENV), "{err}");
    assert!(err.contains("--check-engine=tsc"), "{err}");
  }

  #[test]
  fn toolchain_cache_is_searched_newest_first() {
    let exe_dir = tempfile::tempdir().unwrap();
    let deno_dir = tempfile::tempdir().unwrap();
    let cache = deno_dir.path().join(TOOLCHAIN_CACHE_NAME);
    fake_tsc(&cache.join("2.9.7-denext.21").join("linux-x86_64"), "6.0.3");
    let newest = fake_tsc(
      &cache.join("2.9.7-denext.22").join("linux-x86_64"),
      typescript_version(),
    );
    let found = resolve_native_tsc(
      None,
      Some(exe_dir.path().join("deno")),
      Some(deno_dir.path()),
    )
    .unwrap();
    assert_eq!(found, newest);
  }

  #[test]
  fn toolchain_cache_is_ordered_by_semver() {
    for (older, newer) in [
      ("2.9.7-denext.22", "3.0.0-rc.1"),
      ("3.0.0-rc.9", "3.0.0-rc.10"),
      ("2.9.7-denext.9", "2.9.7-denext.10"),
    ] {
      let exe_dir = tempfile::tempdir().unwrap();
      let deno_dir = tempfile::tempdir().unwrap();
      let cache = deno_dir.path().join(TOOLCHAIN_CACHE_NAME);
      // every candidate is usable: only the order decides
      fake_tsc(
        &cache.join("not-a-version").join("linux-x86_64"),
        typescript_version(),
      );
      fake_tsc(
        &cache.join(older).join("linux-x86_64"),
        typescript_version(),
      );
      let newest = fake_tsc(
        &cache.join(newer).join("linux-x86_64"),
        typescript_version(),
      );
      let found = resolve_native_tsc(
        None,
        Some(exe_dir.path().join("deno")),
        Some(deno_dir.path()),
      )
      .unwrap();
      assert_eq!(found, newest, "{newer} is newer than {older}");
    }
  }

  #[test]
  fn override_wins_over_the_sibling() {
    let dir = tempfile::tempdir().unwrap();
    fake_tsc(dir.path(), typescript_version());
    let other = tempfile::tempdir().unwrap();
    let custom = fake_tsc(other.path(), "7.1.0");
    let found = resolve_native_tsc(
      Some(custom.clone().into_os_string()),
      Some(dir.path().join("deno")),
      None,
    )
    .unwrap();
    assert_eq!(found, custom);
  }

  #[test]
  fn override_that_is_not_a_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = resolve_native_tsc(
      Some(dir.path().join("missing").into_os_string()),
      None,
      None,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains(DENO_TSGO_BIN_ENV), "{err}");
  }
}

#[cfg(test)]
mod pretty_tests {
  use super::*;

  #[test]
  fn pretty_output_carries_span_chain_and_related_information() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("a.ts"),
      "const theme: { mode: \"light\" | \"dark\" } = { mode: \"blue\" };\n",
    )
    .unwrap();
    let output = concat!(
      "\x1b[96ma.ts\x1b[0m:\x1b[93m1\x1b[0m:\x1b[93m45\x1b[0m - \x1b[91merror\x1b[0m\x1b[90m TS2322: \x1b[0mType '\"blue\"' is not assignable to type '\"dark\" | \"light\"'.\n",
      "  Second line of the chain.\n",
      "\n",
      "\x1b[7m1\x1b[0m const theme: { mode: \"light\" | \"dark\" } = { mode: \"blue\" };\n",
      "\x1b[7m \x1b[0m \x1b[91m                                            ~~~~\x1b[0m\n",
      "\n",
      "  \x1b[96ma.ts\x1b[0m:\x1b[93m1\x1b[0m:\x1b[93m16\x1b[0m - The expected type comes from property 'mode' which is declared here\n",
      "    \x1b[7m1\x1b[0m const theme: { mode: \"light\" | \"dark\" } = { mode: \"blue\" };\n",
      "    \x1b[7m \x1b[0m \x1b[96m               ~~~~\x1b[0m\n",
      "\n",
      "\n",
      "Found 1 error in a.ts\x1b[90m:1\x1b[0m\n",
      "\n",
      "Files:                         1\n",
    );
    assert!(output_has_diagnostics(output));
    let diags = parse_tsc_diagnostics(output, dir.path());
    assert_eq!(diags.len(), 1, "{diags:?}");
    let d = &diags[0];
    assert_eq!(d.code, 2322);
    assert_eq!(
      d.message_text.as_deref(),
      Some(
        "Type '\"blue\"' is not assignable to type '\"dark\" | \"light\"'.\n  Second line of the chain."
      )
    );
    let start = d.start.as_ref().unwrap();
    assert_eq!((start.line, start.character), (0, 44));
    // the exact `~~~~` span, not a guess
    assert_eq!(d.end.as_ref().unwrap().character, 48);
    let related = d.related_information.as_ref().unwrap();
    assert_eq!(related.len(), 1);
    assert!(
      related[0]
        .message_text
        .as_deref()
        .unwrap()
        .starts_with("The expected type comes from property 'mode'")
    );
    let rstart = related[0].start.as_ref().unwrap();
    assert_eq!((rstart.line, rstart.character), (0, 15));
    assert_eq!(related[0].end.as_ref().unwrap().character, 19);
    assert!(related[0].file_name.as_deref().unwrap().ends_with("/a.ts"));
  }

  #[test]
  fn pretty_diagnostic_in_the_check_config_has_no_location() {
    let dir = tempfile::tempdir().unwrap();
    let output = concat!(
      "\x1b[96mdeno-check-root-Ab12Cd.tsconfig.json\x1b[0m:\x1b[93m3\x1b[0m:\x1b[93m5\x1b[0m - \x1b[91merror\x1b[0m\x1b[90m TS5102: \x1b[0mOption 'baseUrl' has been removed. Please remove it from your configuration.\n",
      "  Use '\"paths\": {\"*\": [\"./*\"]}' instead.\n",
      "\n",
      "\x1b[7m3\x1b[0m     \"baseUrl\": \".\",\n",
      "\x1b[7m \x1b[0m \x1b[91m    ~~~~~~~~~\x1b[0m\n",
      "\n",
      "Found 1 error.\n",
    );
    let diags = parse_tsc_diagnostics(output, dir.path());
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, 5102);
    assert!(diags[0].file_name.is_none());
    assert!(diags[0].start.is_none());
    assert_eq!(
      diags[0].message_text.as_deref(),
      Some(
        "Option 'baseUrl' has been removed. Please remove it from your configuration.\n  Use '\"paths\": {\"*\": [\"./*\"]}' instead."
      )
    );
    assert!(is_check_config("/tmp/deno-check-x1.tsconfig.json"));
    assert!(is_check_config("../.deno/tsconfig.json"));
    assert!(!is_check_config("tsconfig.json"));
    assert!(!is_check_config("src/deno-check.ts"));
  }

  #[test]
  fn pretty_position_less_diagnostic_keeps_its_chain_and_drops_config_locations()
   {
    let dir = tempfile::tempdir().unwrap();
    let output = concat!(
      "\x1b[91merror\x1b[0m\x1b[90m TS6053: \x1b[0mFile '/p/sub/a.d.ts' not found.\n",
      "  The file is in the program because:\n",
      "    Part of 'files' list in tsconfig.json\n",
      "  \x1b[96mtsconfig.json\x1b[0m:\x1b[93m1\x1b[0m:\x1b[93m55\x1b[0m - File is matched by 'files' list specified here.\n",
      "    \x1b[7m1\x1b[0m {\"files\":[\"sub/a.d.ts\"]}\n",
      "\n",
      "Found 1 error.\n",
    );
    assert!(output_has_diagnostics(output));
    let diags = parse_tsc_diagnostics(output, dir.path());
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, 6053);
    assert_eq!(
      diags[0].message_text.as_deref(),
      Some(
        "File '/p/sub/a.d.ts' not found.\n  The file is in the program because:\n    Part of 'files' list in tsconfig.json"
      )
    );
    assert!(diags[0].related_information.is_none());
  }
}
