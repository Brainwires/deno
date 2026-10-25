// Copyright 2018-2026 the Deno authors. MIT license.

//! Confined npm lifecycle scripts (`jail-scripts`, DENEXT.md item 83).
//!
//! An approved dependency's lifecycle script normally runs with the user's
//! whole environment, their real HOME, and a `node` that is `deno run -A`. A
//! jailed script instead runs in its own process (`deno
//! __deno_internal_jailed_lifecycle_script`, the script read from stdin) with:
//!
//! - an allow-listed environment (no tokens: `npm_config_*` keys naming
//!   credentials are dropped too), and HOME, TMPDIR, DENO_DIR (and on Windows
//!   USERPROFILE / APPDATA / LOCALAPPDATA) pointing at a scratch directory
//!   under `node_modules/.deno/.jail/`;
//! - the OS confining the whole process tree, whatever the script starts
//!   (`sh`, `curl`, a compiler, `deno run -A`): writes only beneath
//!   node_modules (the scratch directory, its TMPDIR, is inside) and the
//!   package's own `write` paths (not the shared system temp directory: a
//!   project, or another program's files, can live there); no reading of credential files (`~/.ssh`,
//!   `~/.aws`, `~/.npmrc`, the project's `.npmrc` / `.env*`, ...); no network
//!   unless the package declares a download host (node-pre-gyp's `binary.host`)
//!   or the config allows it; no Unix sockets. Landlock + seccomp on Linux
//!   (`sandbox_linux`), Seatbelt on macOS (`sandbox_macos`);
//! - every `deno` started inside it clamping its permissions to the same set
//!   (`DENO_INTERNAL_LIFECYCLE_JAIL`): `-A` from the `node` shim and the bin
//!   wrappers becomes write access to those roots only, no FFI, network per
//!   the jail; package-manager subcommands (`deno install`, `x`, `add`, ...)
//!   are refused, as are `npm` / `npx` falling back to a real npm.
//!
//! Where the OS can't confine (Windows, Linux without Landlock, macOS without
//! a working `sandbox-exec`), only the environment, HOME and the Deno
//! permission clamp apply; a warning says so, and `paranoid` refuses to run
//! the script instead.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;

use deno_config::deno_json::JailNetConfig;
use deno_config::deno_json::JailPackageConfig;
use deno_config::deno_json::is_home_relative_path;
use deno_core::anyhow::Context;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_runtime::deno_permissions::PermissionsOptions;
use deno_task_shell::KillSignal;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::args::DenoSubcommand;
use crate::args::Flags;
use crate::colors;
use crate::task_runner::TaskResult;
use crate::util::fs::canonicalize_path;

/// Set in a jailed script's environment: the permissions every `deno` it
/// starts clamps itself to (JSON [`JailPermissions`]).
pub const JAIL_ENV_VAR: &str = "DENO_INTERNAL_LIFECYCLE_JAIL";
/// argv\[1\] of the jailed script's process.
pub const CHILD_ARG: &str = "__deno_internal_jailed_lifecycle_script";

/// Network access of a jailed script.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JailNet {
  #[default]
  None,
  All,
  /// The hosts the package declares; the OS can't tell hosts apart, so it
  /// allows the network, and Deno only these hosts.
  Hosts(Vec<String>),
}

impl JailNet {
  fn allowed(&self) -> bool {
    !matches!(self, JailNet::None)
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JailPermissions {
  pub write: Vec<PathBuf>,
  pub deny_write: Vec<PathBuf>,
  pub deny_read: Vec<PathBuf>,
  pub net: JailNet,
}

/// The jail of the current process, when it runs inside a jailed script.
pub fn jail_from_env() -> Option<JailPermissions> {
  static JAIL: OnceLock<Option<JailPermissions>> = OnceLock::new();
  JAIL
    .get_or_init(|| {
      #[allow(clippy::disallowed_methods, reason = "process-wide setting")]
      let value = std::env::var(JAIL_ENV_VAR).ok()?;
      match serde_json::from_str(&value) {
        Ok(jail) => Some(jail),
        // unreadable: the narrowest jail rather than none
        Err(_) => Some(JailPermissions::default()),
      }
    })
    .clone()
}

/// Inside a jailed script, refuses the subcommands that install or fetch
/// packages, or write outside the permission system.
pub fn check_subcommand(flags: &Flags) -> Result<(), AnyError> {
  if jail_from_env().is_none() {
    return Ok(());
  }
  let allowed = matches!(
    flags.subcommand,
    DenoSubcommand::Run(_)
      | DenoSubcommand::Eval(_)
      | DenoSubcommand::Task(_)
      | DenoSubcommand::Repl(_)
      | DenoSubcommand::Test(_)
      | DenoSubcommand::Bench(_)
      | DenoSubcommand::Check(_)
      | DenoSubcommand::Info(_)
      | DenoSubcommand::Types
      | DenoSubcommand::Help(_)
      | DenoSubcommand::Completions(_)
      | DenoSubcommand::Serve(_)
  );
  if !allowed {
    bail!(
      "This deno subcommand is not allowed in a jailed npm lifecycle script (jail-scripts)."
    );
  }
  Ok(())
}

fn path_within(path: &Path, roots: &[PathBuf]) -> bool {
  roots.iter().any(|root| path.starts_with(root))
}

/// Clamps a `deno` process's permissions to the jail.
pub fn clamp_permissions(
  options: &mut PermissionsOptions,
  jail: &JailPermissions,
  cwd: &Path,
) {
  let roots_text = || {
    jail
      .write
      .iter()
      .map(|p| p.to_string_lossy().into_owned())
      .collect::<Vec<_>>()
  };
  options.allow_write = match options.allow_write.take() {
    None => None,
    Some(list) if list.is_empty() => Some(roots_text()),
    Some(list) => {
      let kept = list
        .into_iter()
        .filter(|entry| {
          let path = cwd.join(entry);
          let path = canonicalize_path(&path).unwrap_or(path);
          path_within(&path, &jail.write)
        })
        .collect::<Vec<_>>();
      // an empty list would mean "everything"
      (!kept.is_empty()).then_some(kept)
    }
  };
  let clamp_net = |list: Option<Vec<String>>| match (&jail.net, list) {
    (_, None) | (JailNet::None, _) => None,
    (JailNet::All, list) => list,
    (JailNet::Hosts(hosts), Some(list)) if list.is_empty() => {
      Some(hosts.clone())
    }
    (JailNet::Hosts(hosts), Some(list)) => {
      let kept = list
        .into_iter()
        .filter(|h| hosts.contains(h))
        .collect::<Vec<_>>();
      (!kept.is_empty()).then_some(kept)
    }
  };
  options.allow_net = clamp_net(options.allow_net.take());
  // The direction-specific grants are clamped the same way, so a `deno`
  // started with `--allow-net-connect` can't reach past the jail either.
  options.allow_net_connect = clamp_net(options.allow_net_connect.take());
  options.allow_net_listen = clamp_net(options.allow_net_listen.take());
  if !jail.net.allowed() {
    options.allow_import = None;
    options.deny_import = Some(Vec::new());
  }
  options.allow_ffi = None;
  for (deny, extra) in [
    (&mut options.deny_write, &jail.deny_write),
    (&mut options.deny_read, &jail.deny_read),
  ] {
    if extra.is_empty() {
      continue;
    }
    match deny {
      // denied entirely already
      Some(list) if list.is_empty() => {}
      Some(list) => {
        list.extend(extra.iter().map(|p| p.to_string_lossy().into_owned()))
      }
      None => {
        *deny = Some(
          extra
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        )
      }
    }
  }
  options.prompt = false;
}

/// The jail of an install (`jail-scripts`).
#[derive(Debug)]
pub struct JailConfig {
  pub paranoid: bool,
  /// Asked for (`--jail-scripts`, `jail-scripts=true`, `paranoid`, a
  /// `jailScripts` object with more than `packages`) rather than on by
  /// default (DENEXT.md items 103, 108): only then does a platform the OS
  /// can't confine on get a warning.
  pub explicit: bool,
  /// `supplyChain.jailScripts.net`
  pub net: bool,
  pub packages: BTreeMap<String, JailPackageConfig>,
  /// The project's own files with secrets (`.npmrc`, `.env*`).
  pub project_secret_files: Vec<PathBuf>,
  outside_links: OnceLock<Vec<PathBuf>>,
  warned: std::sync::atomic::AtomicBool,
}

impl JailConfig {
  pub fn new(
    paranoid: bool,
    explicit: bool,
    net: bool,
    packages: BTreeMap<String, JailPackageConfig>,
    project_dirs: &[PathBuf],
    npmrc_path: Option<&Path>,
  ) -> Self {
    let mut project_secret_files = Vec::new();
    for dir in project_dirs {
      if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
          let name = entry.file_name();
          let name = name.to_string_lossy();
          if name == ".npmrc" || name == ".env" || name.starts_with(".env.") {
            project_secret_files.push(entry.path());
          }
        }
      }
    }
    if let Some(path) = npmrc_path {
      project_secret_files.push(path.to_path_buf());
    }
    let project_secret_files = project_secret_files
      .into_iter()
      .filter_map(|p| canonicalize_path(&p).ok())
      .collect();
    Self {
      paranoid,
      explicit,
      net,
      packages,
      project_secret_files,
      outside_links: Default::default(),
      warned: Default::default(),
    }
  }
}

/// What the OS can confine here, or why it can't.
fn os_sandbox() -> Result<(), String> {
  #[cfg(target_os = "linux")]
  {
    super::sandbox_linux::landlock_abi().map(|_| ())
  }
  #[cfg(target_os = "macos")]
  {
    super::sandbox_macos::available()
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    Err(format!(
      "{} has no OS sandbox Deno can apply to lifecycle scripts",
      std::env::consts::OS
    ))
  }
}

/// Credential files under the user's real home directory.
fn credential_paths() -> Vec<PathBuf> {
  const IN_HOME: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".docker",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
    ".netrc",
    ".git-credentials",
    ".pgpass",
    ".pypirc",
    ".gem/credentials",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".config/gcloud",
    ".config/gh",
    ".config/hub",
    ".config/op",
    ".config/git/credentials",
    ".local/share/keyrings",
    ".password-store",
    ".terraform.d/credentials.tfrc.json",
    ".deno/auth",
    "Library/Keychains",
    "Library/Application Support/gh",
  ];
  let Some(home) = home_dir() else {
    return Vec::new();
  };
  IN_HOME
    .iter()
    .map(|p| home.join(p))
    .filter_map(|p| canonicalize_path(&p).ok())
    .collect()
}

fn home_dir() -> Option<PathBuf> {
  #[allow(clippy::disallowed_methods, reason = "the user's real home")]
  let home = if cfg!(windows) {
    std::env::var_os("USERPROFILE")
  } else {
    std::env::var_os("HOME")
  };
  home.filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Symlinks in node_modules that lead outside the writable roots (workspace
/// members, `file:` dependencies): the Deno permission layer denies writing
/// through them (the OS layer checks real paths anyway).
fn outside_links(root_node_modules: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
  fn visit(dir: &Path, roots: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let Ok(meta) = std::fs::symlink_metadata(&path) else {
        continue;
      };
      if meta.file_type().is_symlink()
        && let Ok(target) = canonicalize_path(&path)
        && !path_within(&target, roots)
        && !out.contains(&target)
      {
        out.push(target);
      }
    }
  }
  let mut out = Vec::new();
  let mut dirs = vec![root_node_modules.to_path_buf()];
  if let Ok(entries) = std::fs::read_dir(root_node_modules.join(".deno")) {
    for entry in entries.flatten() {
      dirs.push(entry.path().join("node_modules"));
    }
  }
  for dir in dirs {
    visit(&dir, roots, &mut out);
    if let Ok(entries) = std::fs::read_dir(&dir) {
      for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('@')
          && entry.file_type().is_ok_and(|t| t.is_dir())
        {
          visit(&entry.path(), roots, &mut out);
        }
      }
    }
  }
  out
}

/// The download hosts a package declares: node-pre-gyp's `binary.host`
/// (and `staging_host` / `production_host`).
fn declared_net_hosts(package_folder: &Path) -> Vec<String> {
  let Ok(text) = std::fs::read_to_string(package_folder.join("package.json"))
  else {
    return Vec::new();
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
    return Vec::new();
  };
  let mut hosts = Vec::new();
  if let Some(binary) = json.get("binary").and_then(|b| b.as_object()) {
    for key in ["host", "staging_host", "production_host"] {
      if let Some(url) = binary.get(key).and_then(|v| v.as_str())
        && let Ok(url) = deno_core::url::Url::parse(url)
        && let Some(host) = url.host_str()
      {
        let host = match url.port() {
          Some(port) => format!("{host}:{port}"),
          None => host.to_string(),
        };
        if !hosts.contains(&host) {
          hosts.push(host);
        }
      }
    }
  }
  hosts
}

/// Network access of a package's scripts: the package's own `net` (a host
/// list is the narrower grant and wins over `jailScripts.net`), the config's,
/// or the hosts the package declares (not under paranoid: a package doesn't
/// grant itself anything there).
fn jail_net(
  config: &JailConfig,
  package: Option<&JailPackageConfig>,
  package_folder: &Path,
) -> JailNet {
  match package.and_then(|p| p.net.as_ref()) {
    Some(JailNetConfig::Hosts(hosts)) => return JailNet::Hosts(hosts.clone()),
    Some(JailNetConfig::All(true)) => return JailNet::All,
    _ if config.net => return JailNet::All,
    Some(JailNetConfig::All(false)) => return JailNet::None,
    None => {}
  }
  if config.paranoid {
    return JailNet::None;
  }
  let hosts = declared_net_hosts(package_folder);
  if hosts.is_empty() {
    JailNet::None
  } else {
    JailNet::Hosts(hosts)
  }
}

/// A package's extra `write` paths, `~` resolved against the user's home (a
/// `~` path is dropped when there is no home: never a wider grant), each
/// created when missing. A download cache a package keeps outside
/// node_modules (puppeteer's `PUPPETEER_CACHE_DIR`, playwright's
/// `PLAYWRIGHT_BROWSERS_PATH`, cypress's `CYPRESS_CACHE_FOLDER`) usually
/// doesn't exist before the first install, and the OS sandbox can only grant
/// a directory that does: Landlock opens it, and Seatbelt matches the real
/// path, which `canonicalize` only knows for an existing one.
fn package_write_paths(
  package: &JailPackageConfig,
  home: Option<&Path>,
) -> Vec<PathBuf> {
  package
    .write
    .iter()
    .filter_map(|path| {
      if is_home_relative_path(path) {
        let rest = path.strip_prefix("~").ok()?;
        Some(home?.join(rest))
      } else {
        Some(path.clone())
      }
    })
    .inspect(|path| {
      let _ = std::fs::create_dir_all(path);
    })
    .collect()
}

/// Environment variables a jailed script keeps (Windows names upper-cased).
fn keep_env_var(name: &str, net: bool) -> bool {
  const EXACT: &[&str] = &[
    "PATH",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "LANG",
    "LANGUAGE",
    "TZ",
    "USER",
    "LOGNAME",
    "USERNAME",
    "SHELL",
    "CI",
    "INIT_CWD",
    "PYTHON",
    "CC",
    "CXX",
    "AR",
    "LD",
    "CFLAGS",
    "CXXFLAGS",
    "CPPFLAGS",
    "LDFLAGS",
    "MAKE",
    "MAKEFLAGS",
    "JOBS",
    "SDKROOT",
    "DEVELOPER_DIR",
    "MACOSX_DEPLOYMENT_TARGET",
    "PKG_CONFIG_PATH",
    "NODEJS_ORG_MIRROR",
    "NODE_OPTIONS",
    "DENO_TLS_CA_STORE",
    "DENO_CERT",
    "DENO_NODE_GYP",
    "DENO_NO_PROMPT",
    "DENO_DISABLE_NODE_SHIM",
    "DENO_NODE_SHIM_ACTIVE",
    "DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD",
    // Windows
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "COMSPEC",
    "PATHEXT",
    "WINDIR",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "NUMBER_OF_PROCESSORS",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "PROGRAMDATA",
    "VSINSTALLDIR",
    "VCINSTALLDIR",
    "VCTOOLSINSTALLDIR",
    "WINDOWSSDKDIR",
    "WINDOWSSDKVERSION",
    "INCLUDE",
    "LIB",
    "LIBPATH",
  ];
  const PROXY: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
  ];
  if EXACT.contains(&name) || (net && PROXY.contains(&name)) {
    return true;
  }
  if name.starts_with("LC_") || name.starts_with("DENO_INTERNAL_") {
    return true;
  }
  let lower = name.to_ascii_lowercase();
  if lower.starts_with("npm_package_") || lower.starts_with("npm_lifecycle_") {
    return true;
  }
  // the package manager's identity (`deno/<version> npm/? ...`), which
  // install scripts read to tell package managers apart; it names no
  // credential despite the `user` in its name
  if lower == "npm_config_user_agent" {
    return true;
  }
  if lower.starts_with("npm_config_") {
    // `npm_config_//registry/:_authToken` and friends carry credentials
    const SECRET: &[&str] = &[
      "auth", "token", "pass", "secret", "cert", "key", "email", "user", "otp",
      "//",
    ];
    return !SECRET.iter().any(|s| lower.contains(s));
  }
  false
}

struct JailDirs {
  root: PathBuf,
  home: PathBuf,
  tmp: PathBuf,
  deno_dir: PathBuf,
}

impl JailDirs {
  fn create(
    root_node_modules: &Path,
    package_id: &str,
  ) -> std::io::Result<Self> {
    let name = package_id
      .chars()
      .map(|c| {
        if c.is_ascii_alphanumeric() || "@.-_".contains(c) {
          c
        } else {
          '+'
        }
      })
      .collect::<String>();
    let root = root_node_modules.join(".deno").join(".jail").join(name);
    let dirs = Self {
      home: root.join("home"),
      tmp: root.join("tmp"),
      deno_dir: root.join("deno_dir"),
      root,
    };
    for dir in [&dirs.home, &dirs.tmp, &dirs.deno_dir] {
      std::fs::create_dir_all(dir)?;
    }
    Ok(dirs)
  }
}

/// One lifecycle script to run jailed.
pub struct JailedScript<'a> {
  pub package_name: &'a str,
  pub package_id: String,
  pub package_folder: &'a Path,
  pub root_node_modules: &'a Path,
  pub task_name: &'a str,
  pub script: &'a str,
  pub init_cwd: &'a Path,
  /// The environment the executor built for the script.
  pub env_vars: &'a HashMap<OsString, OsString>,
  /// The bins the script can run by name.
  pub bins: Vec<(String, PathBuf)>,
  pub node_modules_bin_dirs: Vec<PathBuf>,
  pub kill_signal: KillSignal,
}

/// The jailed script's process reads this from stdin.
#[derive(Serialize, Deserialize)]
struct ChildSpec {
  task_name: String,
  script: String,
  cwd: PathBuf,
  init_cwd: PathBuf,
  env: Vec<(String, String)>,
  bins: Vec<(String, PathBuf)>,
  node_modules_bin_dirs: Vec<PathBuf>,
}

/// Runs one lifecycle script jailed, with its output captured.
pub async fn run_jailed_script(
  config: &JailConfig,
  s: JailedScript<'_>,
) -> Result<TaskResult, AnyError> {
  let sandbox = os_sandbox();
  if let Err(reason) = &sandbox {
    if config.paranoid {
      bail!(
        "paranoid is on, but npm:{}'s '{}' script can't be confined here: {reason}. It was not run.",
        s.package_id,
        s.task_name
      );
    }
    // on by default, the jail stays quiet where the OS can't confine: the
    // environment, HOME and the Deno clamp still apply, and a warning on
    // every install there would be noise the user can't act on
    if config.explicit
      && !config
        .warned
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      log::warn!(
        "{} jail-scripts: {reason}. Lifecycle scripts run with a scrubbed environment and a temporary HOME, and the JavaScript they run through Deno is confined, but other programs they start are not.",
        colors::yellow("Warning")
      );
    }
  }

  let dirs = JailDirs::create(s.root_node_modules, &s.package_id)
    .context("Failed to create the jail's scratch directory")?;
  let result = run_in_dirs(config, &s, &dirs, sandbox.is_ok()).await;
  let _ = std::fs::remove_dir_all(&dirs.root);
  result
}

async fn run_in_dirs(
  config: &JailConfig,
  s: &JailedScript<'_>,
  dirs: &JailDirs,
  os_sandbox: bool,
) -> Result<TaskResult, AnyError> {
  let package = config.packages.get(s.package_name);
  let canonical =
    |p: &Path| canonicalize_path(p).unwrap_or_else(|_| p.to_path_buf());

  // writable roots
  let mut write = vec![
    canonical(s.root_node_modules),
    canonical(s.package_folder),
    canonical(&dirs.root),
  ];
  if let Some(package) = package {
    write.extend(
      package_write_paths(package, home_dir().as_deref())
        .iter()
        .map(|p| canonical(p)),
    );
  }
  write.dedup();

  let net = jail_net(config, package, s.package_folder);

  let mut deny_read = credential_paths();
  deny_read.extend(config.project_secret_files.iter().cloned());
  let deny_write = config
    .outside_links
    .get_or_init(|| outside_links(&canonical(s.root_node_modules), &write))
    .clone();
  let permissions = JailPermissions {
    write: write.clone(),
    deny_write,
    deny_read: deny_read.clone(),
    net: net.clone(),
  };

  // the environment
  let mut env: HashMap<OsString, OsString> = HashMap::new();
  for (key, value) in s.env_vars {
    let name = key.to_string_lossy();
    let passed = package.is_some_and(|p| {
      p.env.iter().any(|n| {
        n == name.as_ref() || (cfg!(windows) && n.eq_ignore_ascii_case(&name))
      })
    });
    if passed || keep_env_var(&name, net.allowed()) {
      env.insert(key.clone(), value.clone());
    }
  }
  let mut set = |key: &str, value: &Path| {
    let key = if cfg!(windows) {
      key.to_ascii_uppercase()
    } else {
      key.to_string()
    };
    env.insert(key.into(), value.as_os_str().to_os_string());
  };
  set("HOME", &dirs.home);
  set("TMPDIR", &dirs.tmp);
  set("TEMP", &dirs.tmp);
  set("TMP", &dirs.tmp);
  set("DENO_DIR", &dirs.deno_dir);
  if cfg!(windows) {
    set("USERPROFILE", &dirs.home);
    set("APPDATA", &dirs.home.join("AppData").join("Roaming"));
    set("LOCALAPPDATA", &dirs.home.join("AppData").join("Local"));
  } else {
    set("XDG_CONFIG_HOME", &dirs.home.join(".config"));
    set("XDG_CACHE_HOME", &dirs.home.join(".cache"));
    set("XDG_DATA_HOME", &dirs.home.join(".local").join("share"));
  }
  env.insert("DENO_NO_UPDATE_CHECK".into(), "1".into());
  env.insert(
    JAIL_ENV_VAR.into(),
    serde_json::to_string(&permissions)?.into(),
  );

  let spec = ChildSpec {
    task_name: s.task_name.to_string(),
    script: s.script.to_string(),
    cwd: s.package_folder.to_path_buf(),
    init_cwd: s.init_cwd.to_path_buf(),
    env: env
      .iter()
      .map(|(k, v)| {
        (
          k.to_string_lossy().into_owned(),
          v.to_string_lossy().into_owned(),
        )
      })
      .collect(),
    bins: s.bins.clone(),
    node_modules_bin_dirs: s.node_modules_bin_dirs.clone(),
  };

  #[allow(clippy::disallowed_methods, reason = "spawning this executable")]
  let current_exe = std::env::current_exe()
    .and_then(|p| canonicalize_path(&p))
    .context("Failed to resolve the deno executable")?;

  #[cfg(target_os = "macos")]
  let mut command = if os_sandbox {
    let mut command =
      tokio::process::Command::new(super::sandbox_macos::SANDBOX_EXEC);
    command.arg("-p").arg(super::sandbox_macos::profile(
      &write,
      &deny_read,
      net.allowed(),
    ));
    command.arg(&current_exe);
    command
  } else {
    tokio::process::Command::new(&current_exe)
  };
  #[cfg(not(target_os = "macos"))]
  let mut command = tokio::process::Command::new(&current_exe);
  command
    .arg(CHILD_ARG)
    .current_dir(s.package_folder)
    .env_clear()
    .envs(&env)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

  #[cfg(target_os = "linux")]
  let _ruleset = if os_sandbox {
    let ruleset = super::sandbox_linux::build_ruleset(&write, &deny_read)
      .context("Failed to set up the Landlock sandbox")?;
    super::sandbox_linux::apply_to_command(
      &mut command,
      &ruleset,
      net.allowed(),
    );
    Some(ruleset)
  } else {
    None
  };
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  let _ = os_sandbox;

  let mut child = command.spawn().with_context(|| {
    format!("Failed to start the jailed '{}' script", s.task_name)
  })?;
  #[cfg(target_os = "linux")]
  drop(_ruleset);

  let spec = serde_json::to_vec(&spec)?;
  if let Some(mut stdin) = child.stdin.take() {
    // the script gets an empty stdin after the spec
    let _ = stdin.write_all(&spec).await;
    drop(stdin);
  }
  let mut stdout = child.stdout.take().unwrap();
  let mut stderr = child.stderr.take().unwrap();
  let read_out = tokio::spawn(async move {
    let mut buf = Vec::new();
    let _ = stdout.read_to_end(&mut buf).await;
    buf
  });
  let read_err = tokio::spawn(async move {
    let mut buf = Vec::new();
    let _ = stderr.read_to_end(&mut buf).await;
    buf
  });
  let status = tokio::select! {
    status = child.wait() => status?,
    _ = s.kill_signal.wait_any() => {
      let _ = child.kill().await;
      child.wait().await?
    }
  };
  let stdout = read_out.await.unwrap_or_default();
  let stderr = read_err.await.unwrap_or_default();
  let exit_code = status.code().unwrap_or(1);
  Ok(TaskResult {
    exit_code,
    stdout: Some(stdout),
    stderr: Some(stderr),
  })
}

/// The jailed script's process: reads the script from stdin and runs it with
/// the task shell, as the lifecycle-script executor would in-process.
pub async fn run_child() -> i32 {
  match run_child_inner().await {
    Ok(code) => code,
    Err(err) => {
      log::error!("{} {err:#}", colors::red_bold("error:"));
      1
    }
  }
}

async fn run_child_inner() -> Result<i32, AnyError> {
  if jail_from_env().is_none() {
    bail!("not a jailed lifecycle script");
  }
  let mut input = Vec::new();
  tokio::io::stdin().read_to_end(&mut input).await?;
  let spec: ChildSpec = serde_json::from_slice(&input)?;
  let mut commands = crate::task_runner::TaskCustomCommands::new();
  commands.insert(
    "npx".to_string(),
    std::rc::Rc::new(crate::task_runner::NpxCommand),
  );
  commands.insert(
    "npm".to_string(),
    std::rc::Rc::new(crate::task_runner::NpmCommand),
  );
  commands.insert(
    "node".to_string(),
    std::rc::Rc::new(crate::task_runner::NodeCommand),
  );
  commands.insert(
    "node-gyp".to_string(),
    std::rc::Rc::new(crate::task_runner::NodeGypCommand),
  );
  for (name, path) in spec.bins {
    commands.insert(
      name.clone(),
      std::rc::Rc::new(crate::task_runner::NodeModulesFileRunCommand {
        command_name: name,
        path,
      }),
    );
  }
  let env_vars = spec
    .env
    .into_iter()
    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
    .collect();
  let result =
    crate::task_runner::run_task(crate::task_runner::RunTaskOptions {
      task_name: &spec.task_name,
      script: &spec.script,
      cwd: spec.cwd,
      env_vars,
      custom_commands: commands,
      init_cwd: &spec.init_cwd,
      argv: &[],
      node_modules_bin_dirs: &spec.node_modules_bin_dirs,
      stdio: None,
      kill_signal: KillSignal::default(),
    })
    .await?;
  Ok(result.exit_code)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn env_allow_list() {
    for name in [
      "PATH",
      "LC_ALL",
      "npm_package_name",
      "npm_config_registry",
      "npm_config_user_agent",
    ] {
      assert!(keep_env_var(name, false), "{name}");
    }
    for name in [
      "GITHUB_TOKEN",
      "AWS_SECRET_ACCESS_KEY",
      "NPM_TOKEN",
      "SSH_AUTH_SOCK",
      "DENO_AUTH_TOKENS",
      "DENO_DIR",
      "npm_config__authToken",
      "npm_config_//registry.npmjs.org/:_authToken",
      "NPM_CONFIG__AUTH",
      "npm_config_email",
      "HTTP_PROXY",
    ] {
      assert!(!keep_env_var(name, false), "{name}");
    }
    assert!(keep_env_var("HTTPS_PROXY", true));
  }

  #[test]
  fn declared_hosts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://github.com/a/b/releases/", "staging_host": "https://s3.example.com:8443/x", "module_name": "m" } }"#,
    )
    .unwrap();
    assert_eq!(
      declared_net_hosts(dir.path()),
      vec!["github.com".to_string(), "s3.example.com:8443".to_string()]
    );
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();
    assert!(declared_net_hosts(dir.path()).is_empty());
  }

  // DENEXT.md item 108: a per-package host allow-list
  #[test]
  fn package_net() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("package.json"),
      r#"{ "binary": { "host": "https://declared.example.com/" } }"#,
    )
    .unwrap();
    let config = |net: bool, paranoid: bool| {
      JailConfig::new(paranoid, false, net, BTreeMap::new(), &[], None)
    };
    let package = |net: Option<JailNetConfig>| JailPackageConfig {
      net,
      ..Default::default()
    };
    let hosts = JailNetConfig::Hosts(vec![
      "registry.npmjs.org".to_string(),
      "github.com".to_string(),
    ]);
    let expected_hosts = JailNet::Hosts(vec![
      "registry.npmjs.org".to_string(),
      "github.com".to_string(),
    ]);
    // the listed hosts, whatever else would apply
    for (net, paranoid) in
      [(false, false), (true, false), (false, true), (true, true)]
    {
      assert_eq!(
        jail_net(
          &config(net, paranoid),
          Some(&package(Some(hosts.clone()))),
          dir.path()
        ),
        expected_hosts,
        "net {net}, paranoid {paranoid}"
      );
    }
    // the boolean forms keep their meaning
    let all = package(Some(JailNetConfig::All(true)));
    let none = package(Some(JailNetConfig::All(false)));
    assert_eq!(
      jail_net(&config(false, true), Some(&all), dir.path()),
      JailNet::All
    );
    assert_eq!(
      jail_net(&config(false, false), Some(&none), dir.path()),
      JailNet::None
    );
    assert_eq!(
      jail_net(&config(true, false), None, dir.path()),
      JailNet::All
    );
    assert_eq!(
      jail_net(&config(false, false), None, dir.path()),
      JailNet::Hosts(vec!["declared.example.com".to_string()])
    );
    assert_eq!(
      jail_net(&config(false, true), None, dir.path()),
      JailNet::None
    );
  }

  // DENEXT.md item 108: a download cache outside node_modules
  #[test]
  fn package_write() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let package = JailPackageConfig {
      write: vec![
        PathBuf::from("~/.cache/puppeteer"),
        PathBuf::from("~"),
        dir.path().join("project/.cache/ms-playwright"),
      ],
      ..Default::default()
    };
    let paths = package_write_paths(&package, Some(&home));
    assert_eq!(
      paths,
      vec![
        home.join(".cache/puppeteer"),
        home.clone(),
        dir.path().join("project/.cache/ms-playwright"),
      ]
    );
    // created, so the OS sandbox can grant them
    for path in &paths {
      assert!(path.is_dir(), "{}", path.display());
    }
    // no home: a `~` path is dropped, never widened
    assert_eq!(
      package_write_paths(&package, None),
      vec![dir.path().join("project/.cache/ms-playwright")]
    );
  }

  #[cfg(unix)]
  #[test]
  fn links_leading_outside_node_modules() {
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    let nm = root.join("node_modules");
    let member = root.join("member");
    std::fs::create_dir_all(nm.join(".deno/a@1.0.0/node_modules/a")).unwrap();
    std::fs::create_dir_all(nm.join("@scope")).unwrap();
    std::fs::create_dir_all(&member).unwrap();
    // inside node_modules: fine
    std::os::unix::fs::symlink(
      nm.join(".deno/a@1.0.0/node_modules/a"),
      nm.join("a"),
    )
    .unwrap();
    // outside: denied
    std::os::unix::fs::symlink(&member, nm.join("member")).unwrap();
    std::os::unix::fs::symlink(&member, nm.join("@scope/member")).unwrap();
    std::os::unix::fs::symlink(&root, nm.join(".deno/a@1.0.0/node_modules/up"))
      .unwrap();
    let mut links = outside_links(&nm, std::slice::from_ref(&nm));
    links.sort();
    assert_eq!(links, vec![root.clone(), member]);
  }

  #[test]
  fn clamp() {
    let jail = JailPermissions {
      write: vec![PathBuf::from("/nm")],
      deny_write: vec![PathBuf::from("/ws/member")],
      deny_read: vec![PathBuf::from("/home/u/.ssh")],
      net: JailNet::None,
    };
    let mut options = PermissionsOptions {
      allow_write: Some(vec![]),
      allow_net: Some(vec![]),
      allow_ffi: Some(vec![]),
      allow_read: Some(vec![]),
      prompt: true,
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(options.allow_write, Some(vec!["/nm".to_string()]));
    assert_eq!(options.allow_net, None);
    assert_eq!(options.allow_ffi, None);
    assert_eq!(options.allow_read, Some(vec![]));
    assert_eq!(options.deny_write, Some(vec!["/ws/member".to_string()]));
    assert_eq!(options.deny_read, Some(vec!["/home/u/.ssh".to_string()]));
    assert_eq!(options.deny_import, Some(vec![]));
    assert!(!options.prompt);

    // a write list outside the roots is dropped, never widened to "all"
    let mut options = PermissionsOptions {
      allow_write: Some(vec!["/etc".to_string()]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(options.allow_write, None);
    let mut options = PermissionsOptions {
      allow_write: Some(vec!["out".to_string(), "/etc".to_string()]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(options.allow_write, Some(vec!["out".to_string()]));

    let jail = JailPermissions {
      net: JailNet::Hosts(vec!["example.com".into()]),
      ..jail
    };
    let mut options = PermissionsOptions {
      allow_net: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(options.allow_net, Some(vec!["example.com".to_string()]));
    assert_eq!(options.deny_import, None);

    // the direction-specific grants are clamped like `allow_net`
    let mut options = PermissionsOptions {
      allow_net_connect: Some(vec![]),
      allow_net_listen: Some(vec![
        "example.com".to_string(),
        "evil.com".to_string(),
      ]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(
      options.allow_net_connect,
      Some(vec!["example.com".to_string()])
    );
    assert_eq!(
      options.allow_net_listen,
      Some(vec!["example.com".to_string()])
    );
    let jail = JailPermissions {
      net: JailNet::None,
      ..jail
    };
    let mut options = PermissionsOptions {
      allow_net_connect: Some(vec![]),
      allow_net_listen: Some(vec![]),
      ..Default::default()
    };
    clamp_permissions(&mut options, &jail, Path::new("/nm/pkg"));
    assert_eq!(options.allow_net_connect, None);
    assert_eq!(options.allow_net_listen, None);
  }
}
