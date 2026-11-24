// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use deno_terminal::colors;

use crate::sys::CliSys;

/// Environment variables that configure Deno's own runtime behavior and must
/// not be settable from an `.env` file loaded via `--env` / `--env-file`.
///
/// An `.env` file ships alongside the code it accompanies, so letting it set
/// Deno's own control variables would let that file change runtime behavior
/// the user did not opt into, for example silently enabling tunnel mode
/// (`DENO_CONNECTED`) or overriding the tunnel control endpoint
/// (`DENO_DEPLOY_TUNNEL_ENDPOINT`). Env files are meant to provide
/// configuration to the user's program, not to reconfigure Deno itself, so
/// these keys are ignored when they originate from an env file.
const ENV_FILE_DENYLIST: &[&str] =
  &["DENO_CONNECTED", "DENO_DEPLOY_TUNNEL_ENDPOINT"];

/// denext: more of Deno's own control variables an env file can't set. They
/// name what the runtime itself opens or connects to whatever the program
/// was granted (its permission audit log, its coverage folder, its
/// certificate files and folders, the `Deno.cron` scheduler's socket), or say
/// what the process runs under (a `denext dev` server, a lifecycle-script
/// jail, a permission broker). The kernel sandbox's policy and its `auto`
/// mode are built from them, as they are from the command line, so they count
/// only as the environment the user started the run with had them
/// (`DENO_KERNEL_SANDBOX` is read before env files for the same reason,
/// DENEXT.md item 420).
const ENV_FILE_DENYLIST_DENEXT: &[&str] = &[
  "DENO_AUDIT_PERMISSIONS",
  "DENO_COVERAGE_DIR",
  "DENO_CERT",
  "NODE_EXTRA_CA_CERTS",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "DENO_UNSTABLE_CRON_SOCK",
  "DENEXT_DEV_TOKEN",
  "DENO_INTERNAL_LIFECYCLE_JAIL",
  "DENO_PERMISSION_BROKER_PATH",
];

/// Whether `key` is a Deno-internal control variable that must not be set from
/// an env file. The comparison is ASCII case-insensitive because environment
/// variable lookups are case-insensitive on Windows.
fn is_denied_env_file_key(key: &OsStr) -> bool {
  match key.to_str() {
    Some(key) => ENV_FILE_DENYLIST
      .iter()
      .chain(ENV_FILE_DENYLIST_DENEXT)
      .any(|denied| key.eq_ignore_ascii_case(denied)),
    None => false,
  }
}

/// Whether `name` is a variable the dynamic loader or libc reads when a
/// program starts: `LD_*` and `DYLD_*`, `GCONV_PATH`, `GLIBC_TUNABLES`,
/// `LOCPATH` and `MALLOC_*`.
pub fn is_loader_variable(name: &OsStr) -> bool {
  let Some(name) = name.to_str() else {
    return false;
  };
  ["LD_", "DYLD_", "MALLOC_"]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    || matches!(name, "GCONV_PATH" | "GLIBC_TUNABLES" | "LOCPATH")
}

/// The loader variables ([`is_loader_variable`]) the env files set at
/// startup.
static ENV_FILE_LOADER_VARIABLES: Mutex<Vec<OsString>> = Mutex::new(Vec::new());

/// The loader variables the env files set at startup, which were not in the
/// environment this process was started with. They are for the program (it
/// reads them, its children inherit them), not for this runtime: an image
/// the runtime re-executes itself as is started without them and sets them
/// again once it runs ([`hold_env_file_loader_variables`],
/// [`restore_env_file_loader_variables`]).
#[cfg_attr(
  not(any(target_os = "linux", target_os = "macos")),
  allow(dead_code, reason = "only the kernel sandbox's re-exec uses it")
)]
pub fn env_file_loader_variables() -> Vec<OsString> {
  ENV_FILE_LOADER_VARIABLES.lock().unwrap().clone()
}

/// The variable that carries the held loader variables to the re-executed
/// image: a JSON array of `[name, value]` pairs.
pub const HELD_LOADER_VARIABLES_ENV_VAR: &str =
  "DENO_INTERNAL_ENV_FILE_LOADER_VARIABLES";

/// `env` for an image this runtime re-executes itself as: the loader
/// variables in `from_env_files` are taken out and carried in
/// [`HELD_LOADER_VARIABLES_ENV_VAR`] instead (a carrier already in `env` is
/// dropped), so the image starts without them.
#[cfg_attr(
  not(any(target_os = "linux", target_os = "macos")),
  allow(dead_code, reason = "only the kernel sandbox's re-exec uses it")
)]
pub fn hold_env_file_loader_variables(
  env: Vec<(OsString, OsString)>,
  from_env_files: &[OsString],
) -> Vec<(OsString, OsString)> {
  let mut held: Vec<(String, String)> = Vec::new();
  let mut env: Vec<_> = env
    .into_iter()
    .filter(|(name, value)| {
      if name == HELD_LOADER_VARIABLES_ENV_VAR {
        return false;
      }
      if !is_loader_variable(name) || !from_env_files.contains(name) {
        return true;
      }
      // an env file's names and values are text
      if let (Some(name), Some(value)) = (name.to_str(), value.to_str()) {
        held.push((name.to_string(), value.to_string()));
      }
      false
    })
    .collect();
  if !held.is_empty()
    && let Ok(carried) = deno_core::serde_json::to_string(&held)
  {
    env.push((HELD_LOADER_VARIABLES_ENV_VAR.into(), carried.into()));
  }
  env
}

/// Takes the loader variables the env files set out of this process's
/// environment, for an image that is started with the environment as it is
/// (the supervisor, which reads none of it).
#[cfg_attr(
  not(any(target_os = "linux", target_os = "macos")),
  allow(dead_code, reason = "only the kernel sandbox's re-exec uses it")
)]
pub fn remove_env_file_loader_variables() {
  let names = env_file_loader_variables();
  if names.is_empty() {
    return;
  }
  let process_env = deno_os::ProcessEnvGuard::lock();
  for name in names {
    process_env.remove_var(&name);
  }
}

/// The loader variables a carrier holds (`None`: no carrier, or not one).
/// Only loader variables are taken from it.
fn held_loader_variables(carried: &str) -> Option<Vec<(String, String)>> {
  let held: Vec<(String, String)> =
    deno_core::serde_json::from_str(carried).ok()?;
  Some(
    held
      .into_iter()
      .filter(|(name, _)| is_loader_variable(OsStr::new(name)))
      .collect(),
  )
}

/// In the re-executed image: sets the loader variables the image before
/// held back ([`hold_env_file_loader_variables`]), where the environment
/// doesn't have them, and removes the carrier. Runs where the env files
/// are loaded otherwise, before any thread starts.
pub fn restore_env_file_loader_variables() {
  let process_env = deno_os::ProcessEnvGuard::lock();
  let Some(carried) = process_env.var_os(HELD_LOADER_VARIABLES_ENV_VAR) else {
    return;
  };
  process_env.remove_var(HELD_LOADER_VARIABLES_ENV_VAR);
  for (name, value) in carried
    .to_str()
    .and_then(held_loader_variables)
    .unwrap_or_default()
  {
    if process_env.var_os(&name).is_none() {
      process_env.set_var(&name, &value);
    }
  }
}

pub fn handle_denied_env_file_key(
  key: &OsStr,
  file_path: &Path,
  log_level: Option<log::Level>,
) {
  #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
  if log_level.map(|l| l >= log::Level::Info).unwrap_or(true) {
    eprintln!(
      "{} Ignoring '{}' from environment file '{}': this variable controls Deno's own runtime behavior and cannot be set from an env file.",
      colors::yellow("Warning"),
      key.to_string_lossy(),
      file_path.display(),
    )
  }
}

pub fn resolve_cwd(
  initial_cwd: Option<&Path>,
) -> Result<Cow<'_, Path>, std::io::Error> {
  match initial_cwd {
    Some(initial_cwd) => Ok(Cow::Borrowed(initial_cwd)),
    #[allow(
      clippy::disallowed_methods,
      reason = "ok because the lint recommends using this method"
    )]
    None => std::env::current_dir().map(Cow::Owned).map_err(|err| {
      std::io::Error::new(
        err.kind(),
        format!("could not read current working directory: {err}"),
      )
    }),
  }
}

/// Like `resolve_cwd`, but falls back to a sensible default (the system
/// root) when the current working directory can't be determined — for
/// example when it has been unlinked. This matches Node.js semantics where
/// the REPL still starts even if the parent process's cwd was deleted.
pub fn resolve_cwd_or_fallback(initial_cwd: Option<&Path>) -> PathBuf {
  match resolve_cwd(initial_cwd) {
    Ok(cwd) => cwd.into_owned(),
    Err(_) => fallback_cwd(),
  }
}

fn fallback_cwd() -> PathBuf {
  if cfg!(windows) {
    // System drive root, e.g. `C:\`.
    std::env::var_os("SystemDrive")
      .map(|d| {
        let mut p = PathBuf::from(d);
        p.push("\\");
        p
      })
      .unwrap_or_else(|| PathBuf::from("C:\\"))
  } else {
    PathBuf::from("/")
  }
}

#[derive(Debug, Clone)]
struct WatchEnvTrackerInner {
  // Track all loaded variables and their values
  loaded_variables: HashSet<OsString>,
  // Track variables that are no longer present in any loaded file
  unused_variables: HashSet<OsString>,
  // Track original env vars that existed before we started
  original_env: HashMap<OsString, OsString>,
}

impl WatchEnvTrackerInner {
  fn new() -> Self {
    // Capture the original environment state
    let process_env = deno_os::ProcessEnvGuard::lock();
    let original_env: HashMap<OsString, OsString> =
      process_env.vars_os().into_iter().collect();

    Self {
      loaded_variables: Default::default(),
      unused_variables: Default::default(),
      original_env,
    }
  }
}

#[derive(Debug, Clone)]
pub struct WatchEnvTracker {
  inner: Arc<Mutex<WatchEnvTrackerInner>>,
}

// Global singleton instance
static WATCH_ENV_TRACKER: OnceLock<WatchEnvTracker> = OnceLock::new();

impl WatchEnvTracker {
  /// Get the global singleton instance
  pub fn snapshot() -> &'static WatchEnvTracker {
    WATCH_ENV_TRACKER.get_or_init(|| WatchEnvTracker {
      inner: Arc::new(Mutex::new(WatchEnvTrackerInner::new())),
    })
  }

  // Internal method that accepts an already-acquired lock to avoid deadlocks
  fn load_env_file_inner(
    &self,
    cwd: &Path,
    env_file: &str,
    log_level: Option<log::Level>,
    inner: &mut WatchEnvTrackerInner,
  ) {
    let (file_path, content) = match deno_dotenv::find_path_and_content(
      &CliSys::default(),
      cwd,
      env_file,
    ) {
      Ok(Some(result)) => result,
      Ok(None) => {
        handle_dotenv_not_found(env_file, log_level);
        return;
      }
      Err(err) => {
        handle_dotenv_io_error(&err, log_level);
        return;
      }
    };

    // Keep substitutions and the mutations they produce consistent with each
    // other while allowing file I/O to proceed without the process-env lock.
    let process_env = deno_os::ProcessEnvGuard::lock();
    match deno_dotenv::from_content_sanitized_iter_with_substitution(
      &CliSys::default(),
      &content,
    ) {
      Ok(iter) => {
        for item in iter {
          match item {
            Ok((key, value)) => {
              // Convert to OsString for consistency
              let key_os = OsString::from(key);
              let value_os = OsString::from(value);

              // Process-level env vars should always take precedence over env files.
              if inner.original_env.contains_key(&key_os) {
                #[allow(
                  clippy::print_stderr,
                  reason = "can't use log crate yet"
                )]
                if log_level.map(|l| l >= log::Level::Debug).unwrap_or(false) {
                  eprintln!(
                    "{} Variable '{}' already exists in the process environment, skipping value from '{}'",
                    colors::yellow("Debug"),
                    key_os.to_string_lossy(),
                    file_path.display()
                  );
                }
                continue;
              }

              // Check if this variable is already loaded from a previous file
              if inner.loaded_variables.contains(&key_os) {
                // Variable already exists from a previous file, skip it
                #[allow(
                  clippy::print_stderr,
                  reason = "can't use log crate yet"
                )]
                if log_level.map(|l| l >= log::Level::Debug).unwrap_or(false) {
                  eprintln!(
                    "{} Variable '{}' already loaded from '{}', skipping value from '{}'",
                    colors::yellow("Debug"),
                    key_os.to_string_lossy(),
                    inner
                      .loaded_variables
                      .get(&key_os)
                      .map(|k| k.to_string_lossy().to_string())
                      .unwrap_or_else(|| "unknown".to_string()),
                    file_path.display()
                  );
                }
                continue;
              }

              if is_denied_env_file_key(&key_os) {
                handle_denied_env_file_key(&key_os, &file_path, log_level);
                continue;
              }

              // Watched env reloads run between user-code isolates. Refreshing
              // libc here is sufficient for TZ because the replacement
              // isolate has not been created yet and has no cached timezone to
              // invalidate.
              process_env.set_var(&key_os, &value_os);

              // Track this variable
              inner.loaded_variables.insert(key_os.clone());
              inner.unused_variables.remove(&key_os);
            }
            Err(e) => {
              handle_dotenv_error(&e, &file_path, log_level);
            }
          }
        }
      }
      Err(e) => {
        handle_dotenv_error(&e, &file_path, log_level);
      }
    }
  }

  /// Clean up variables that are no longer present in any loaded file
  fn _cleanup_removed_variables(
    &self,
    inner: &mut WatchEnvTrackerInner,
    log_level: Option<log::Level>,
    process_env: &deno_os::ProcessEnvGuard,
  ) {
    for var_name in inner.unused_variables.iter() {
      // As with loading watched env values, cleanup runs before the
      // replacement isolate is created, so TZ changes do not need a V8
      // notification here.
      if !inner.original_env.contains_key(var_name) {
        process_env.remove_var(var_name);

        #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
        if log_level.map(|l| l >= log::Level::Debug).unwrap_or(false) {
          eprintln!(
            "{} Variable '{}' removed from environment as it's no longer present in any loaded file",
            colors::yellow("Debug"),
            var_name.to_string_lossy()
          );
        }
      } else {
        let original_value = inner.original_env.get(var_name).unwrap();
        process_env.set_var(var_name, original_value);

        #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
        if log_level.map(|l| l >= log::Level::Debug).unwrap_or(false) {
          eprintln!(
            "{} Variable '{}' restored to original value as it's no longer present in any loaded file",
            colors::yellow("Debug"),
            var_name.to_string_lossy()
          );
        }
      }
    }
  }

  // Load multiple env files in reverse order (later files take precedence over earlier ones)
  pub fn load_env_variables_from_env_files(
    &self,
    cwd: &Path,
    env_files: &[String],
    log_level: Option<log::Level>,
  ) {
    let mut inner = self.inner.lock().unwrap();

    inner.unused_variables = std::mem::take(&mut inner.loaded_variables);
    inner.loaded_variables = HashSet::new();

    for env_file_path in env_files.iter().rev() {
      self.load_env_file_inner(cwd, env_file_path, log_level, &mut inner);
    }

    let process_env = deno_os::ProcessEnvGuard::lock();
    self._cleanup_removed_variables(&mut inner, log_level, &process_env);
  }
}

pub fn load_env_variables_from_env_files(
  cwd: &Path,
  env_file_names: &[String],
  flags_log_level: Option<log::Level>,
) {
  let original_env_keys: HashSet<OsString> = {
    let process_env = deno_os::ProcessEnvGuard::lock();
    process_env
      .vars_os()
      .into_iter()
      .map(|(key, _)| key)
      .collect()
  };
  let mut loaded_keys = HashSet::new();

  for env_file_name in env_file_names.iter().rev() {
    let (env_file_path, content) = match deno_dotenv::find_path_and_content(
      &CliSys::default(),
      cwd,
      env_file_name,
    ) {
      Ok(Some(resolved)) => resolved,
      Ok(None) => {
        handle_dotenv_not_found(env_file_name, flags_log_level);
        continue;
      }
      Err(err) => {
        handle_dotenv_io_error(&err, flags_log_level);
        continue;
      }
    };
    // Keep substitutions and the mutations they produce consistent with each
    // other while allowing file I/O to proceed without the process-env lock.
    let process_env = deno_os::ProcessEnvGuard::lock();
    let iter = match deno_dotenv::from_content_sanitized_iter_with_substitution(
      &sys_traits::impls::RealSys,
      &content,
    ) {
      Ok(iter) => iter,
      Err(err) => {
        handle_dotenv_error(&err, &env_file_path, flags_log_level);
        continue;
      }
    };

    for item in iter {
      let (key, value) = match item {
        Ok(pair) => pair,
        Err(error) => {
          handle_dotenv_error(&error, &env_file_path, flags_log_level);
          break;
        }
      };

      let key_os = OsString::from(key);
      if original_env_keys.contains(&key_os) || loaded_keys.contains(&key_os) {
        continue;
      }

      if is_denied_env_file_key(&key_os) {
        handle_denied_env_file_key(&key_os, &env_file_path, flags_log_level);
        continue;
      }

      process_env.set_var(&key_os, value);
      if is_loader_variable(&key_os) {
        ENV_FILE_LOADER_VARIABLES
          .lock()
          .unwrap()
          .push(key_os.clone());
      }
      loaded_keys.insert(key_os);
    }
  }
}

pub fn handle_dotenv_error(
  error: &deno_dotenv::ParseError,
  file_path: &Path,
  log_level: Option<log::Level>,
) {
  #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
  if log_level.map(|l| l >= log::Level::Info).unwrap_or(true) {
    eprintln!(
      "{} Failed parsing value '{}' at index {} within the specified environment file.\n    at {}",
      colors::yellow("Warning"),
      error.line,
      error.index,
      file_path.display(),
    )
  }
}

pub fn handle_dotenv_io_error(
  error: &deno_dotenv::FindPathAndContentError,
  log_level: Option<log::Level>,
) {
  #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
  if log_level.map(|l| l >= log::Level::Info).unwrap_or(true) {
    eprintln!(
      "{} Error reading from environment file: {}\n    at {}",
      colors::yellow("Warning"),
      error.source,
      error.path.display(),
    )
  }
}

pub fn handle_dotenv_not_found(specifier: &str, log_level: Option<log::Level>) {
  #[allow(clippy::print_stderr, reason = "can't use log crate yet")]
  if log_level.map(|l| l >= log::Level::Info).unwrap_or(true) {
    eprintln!(
      "{} The `--env-file` flag was used, but the environment file specified '{}' was not found.",
      colors::yellow("Warning"),
      specifier,
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
      .iter()
      .map(|(name, value)| (OsString::from(name), OsString::from(value)))
      .collect()
  }

  #[test]
  fn loader_variables_are_named() {
    for name in [
      "LD_PRELOAD",
      "LD_AUDIT",
      "LD_LIBRARY_PATH",
      "DYLD_INSERT_LIBRARIES",
      "DYLD_LIBRARY_PATH",
      "GCONV_PATH",
      "GLIBC_TUNABLES",
      "LOCPATH",
      "MALLOC_CHECK_",
      "MALLOC_ARENA_MAX",
    ] {
      assert!(is_loader_variable(OsStr::new(name)), "{name}");
    }
    for name in ["PATH", "LDFLAGS", "LD", "MY_LD_PRELOAD", "ld_preload", "TZ"] {
      assert!(!is_loader_variable(OsStr::new(name)), "{name}");
    }
  }

  #[test]
  fn the_reexecuted_image_starts_without_an_env_files_loader_variables() {
    let from_env_files: Vec<OsString> = [
      "LD_PRELOAD",
      "GCONV_PATH",
      "GREETING",
      "DYLD_INSERT_LIBRARIES",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let out = hold_env_file_loader_variables(
      env(&[
        ("PATH", "/bin"),
        ("LD_PRELOAD", "/proj/a.so"),
        // set where the process was started, not by an env file
        ("LD_LIBRARY_PATH", "/opt/lib"),
        ("GCONV_PATH", "/proj/gconv"),
        ("DYLD_INSERT_LIBRARIES", "/proj/a.dylib"),
        // an env file's, but no loader variable
        ("GREETING", "hello"),
        // a carrier from somewhere else is not passed on
        (HELD_LOADER_VARIABLES_ENV_VAR, r#"[["LD_AUDIT","/x.so"]]"#),
      ]),
      &from_env_files,
    );
    let (carriers, rest): (Vec<_>, Vec<_>) = out
      .into_iter()
      .partition(|(name, _)| name == HELD_LOADER_VARIABLES_ENV_VAR);
    assert_eq!(
      rest,
      env(&[
        ("PATH", "/bin"),
        ("LD_LIBRARY_PATH", "/opt/lib"),
        ("GREETING", "hello"),
      ])
    );
    assert_eq!(carriers.len(), 1);
    assert_eq!(
      held_loader_variables(carriers[0].1.to_str().unwrap()).unwrap(),
      vec![
        ("LD_PRELOAD".to_string(), "/proj/a.so".to_string()),
        ("GCONV_PATH".to_string(), "/proj/gconv".to_string()),
        (
          "DYLD_INSERT_LIBRARIES".to_string(),
          "/proj/a.dylib".to_string()
        ),
      ]
    );

    // nothing held: no carrier, and one that was there is gone
    let out = hold_env_file_loader_variables(
      env(&[("PATH", "/bin"), (HELD_LOADER_VARIABLES_ENV_VAR, "[]")]),
      &from_env_files,
    );
    assert_eq!(out, env(&[("PATH", "/bin")]));
  }

  #[test]
  fn a_carrier_holds_loader_variables_only() {
    assert_eq!(
      held_loader_variables(r#"[["LD_PRELOAD","a"],["PATH","/evil"]]"#)
        .unwrap(),
      vec![("LD_PRELOAD".to_string(), "a".to_string())]
    );
    assert!(held_loader_variables("not json").is_none());
    assert!(held_loader_variables(r#"{"LD_PRELOAD":"a"}"#).is_none());
  }

  // denext: the variables the runtime's own files, sockets and confinement
  // are read from can't come from an env file, in any letter case; the
  // program's own variables (and the ones that only name its folders) can.
  #[test]
  fn env_files_cannot_set_the_runtime_control_variables() {
    for name in [
      "DENO_CONNECTED",
      "DENO_DEPLOY_TUNNEL_ENDPOINT",
      "DENO_AUDIT_PERMISSIONS",
      "DENO_COVERAGE_DIR",
      "DENO_CERT",
      "NODE_EXTRA_CA_CERTS",
      "SSL_CERT_FILE",
      "SSL_CERT_DIR",
      "DENO_UNSTABLE_CRON_SOCK",
      "DENEXT_DEV_TOKEN",
      "DENO_INTERNAL_LIFECYCLE_JAIL",
      "DENO_PERMISSION_BROKER_PATH",
    ] {
      assert!(is_denied_env_file_key(OsStr::new(name)), "{name}");
      let lower = name.to_ascii_lowercase();
      assert!(is_denied_env_file_key(OsStr::new(&lower)), "{lower}");
    }
    for name in [
      "PORT",
      "DATABASE_URL",
      "MY_DENO_CERT",
      "DENO_CERT_FILE",
      "DENO_DIR",
      "NO_COLOR",
      "",
    ] {
      assert!(!is_denied_env_file_key(OsStr::new(name)), "{name}");
    }
  }
}
