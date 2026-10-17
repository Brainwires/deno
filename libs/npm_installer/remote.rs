// Copyright 2018-2026 the Deno authors. MIT license.

//! package.json dependencies that aren't registry packages: tarball URLs
//! (`"x": "https://host/x-1.0.0.tgz"`, a `file:` path to a `.tgz`) and git
//! repositories (`git+https://`, `git+ssh://`, `github:user/repo#ref`, ...).
//!
//! `deno install` fetches each into `node_modules/.deno/.remote/`, as npm
//! extracts them into `node_modules`, and the installer then treats it like a
//! `file:` directory dependency (linked at `node_modules/<alias>`, its `bin`
//! linked into `node_modules/.bin`) whose own registry dependencies are
//! installed with the rest and linked into its `node_modules`. The lockfile
//! records what each resolved to under `npmRemote`: a tarball's sha512
//! integrity (checked on every later download, as npm checks
//! `package-lock.json`'s), a git dependency's commit (checked out on every
//! later install instead of resolving the ref again).
//!
//! git runs without a shell, with its arguments validated when package.json
//! is parsed (no option-like or `ext::` URLs, no revision expressions as
//! refs), only over https, http, ssh, git and file
//! (`GIT_ALLOW_PROTOCOL`), with no terminal prompts, hooks or submodules.

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use deno_config::workspace::Workspace;
use deno_npm_cache::NpmCacheHttpClient;
use deno_npm_cache::NpmCacheHttpClientResponse;
use deno_npm_cache::NpmCacheSetting;
use deno_npmrc::ResolvedNpmRc;
use deno_package_json::PackageJsonDepValue;
use deno_package_json::PackageJsonGitCommittish;
use deno_package_json::PackageJsonRemoteDep;
use deno_package_json::PackageJsonRemoteDepKind;
use deno_resolver::lockfile::LockfileLock;
use deno_semver::StackString;
use deno_semver::Version;
use deno_semver::package::PackageNv;
use deno_semver::package::PackageReq;
use sys_traits::FsDirEntry;
use sys_traits::OpenOptions;
use url::Url;

use crate::NpmInstallerSys;

/// The directory under `node_modules/.deno` that holds them.
pub const REMOTE_PACKAGES_DIR: &str = ".remote";
const MARKER_FILE: &str = ".deno-remote.json";

/// A tarball or git dependency, fetched and extracted.
#[derive(Debug, Clone)]
pub struct FetchedRemotePkg {
  /// The dependency's name in package.json (where it is linked).
  pub alias: StackString,
  /// Its value as written (the lockfile key).
  pub specifier: String,
  /// Where its files are (`node_modules/.deno/.remote/<alias>@<key>`).
  pub dir: PathBuf,
  /// Its package.json name and version (the alias and 0.0.0 when missing).
  pub nv: PackageNv,
  /// Its own registry dependencies (`dependencies`, `optionalDependencies`).
  pub deps: Vec<(StackString, PackageReq)>,
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
pub enum RemotePackageError {
  #[error("Failed fetching '{alias}' ({specifier}): {message}")]
  Fetch {
    alias: StackString,
    specifier: String,
    message: String,
  },
}

/// What a dependency resolved to: the lockfile's value for it.
fn marker_contents(specifier: &str, resolved: &str) -> String {
  serde_json::json!({ "specifier": specifier, "resolved": resolved })
    .to_string()
}

pub struct RemotePackagesInstaller<
  'a,
  THttp: NpmCacheHttpClient,
  TSys: NpmInstallerSys,
> {
  pub http_client: &'a THttp,
  pub sys: &'a TSys,
  pub npmrc: &'a ResolvedNpmRc,
  pub lockfile: Option<&'a LockfileLock<TSys>>,
  pub node_modules_path: &'a Path,
  pub cache_setting: &'a NpmCacheSetting,
  pub production: bool,
}

/// The tarball and git dependencies of the workspace's package.json files.
pub fn workspace_remote_deps(
  workspace: &Workspace,
  production: bool,
) -> Vec<(StackString, PackageJsonRemoteDep, PathBuf)> {
  let mut result: Vec<(StackString, PackageJsonRemoteDep, PathBuf)> =
    Vec::new();
  for pkg_json in workspace.package_jsons() {
    let deps = pkg_json.resolve_local_package_json_deps();
    let dev = if production {
      None
    } else {
      Some(deps.dev_dependencies.iter())
    };
    for (alias, dep) in
      deps.dependencies.iter().chain(dev.into_iter().flatten())
    {
      let dep = match dep {
        Ok(PackageJsonDepValue::Remote(dep)) => dep.clone(),
        // `file:` to a tarball is extracted as npm extracts it
        Ok(PackageJsonDepValue::File(path)) if is_tarball_path(path) => {
          let full_path = pkg_json.dir_path().join(path);
          let Ok(url) = deno_path_util::url_from_file_path(&full_path) else {
            continue;
          };
          PackageJsonRemoteDep {
            specifier: format!("file:{path}"),
            kind: PackageJsonRemoteDepKind::Tarball(url),
          }
        }
        _ => continue,
      };
      if !result.iter().any(|(a, d, _)| a == alias && *d == dep) {
        result.push((alias.clone(), dep, pkg_json.dir_path().to_path_buf()));
      }
    }
  }
  result
}

/// A `file:` dependency that points at a package tarball.
pub fn is_tarball_path(path: &str) -> bool {
  path.ends_with(".tgz") || path.ends_with(".tar.gz") || path.ends_with(".tar")
}

impl<THttp: NpmCacheHttpClient, TSys: NpmInstallerSys>
  RemotePackagesInstaller<'_, THttp, TSys>
{
  /// Fetches (or reuses) every tarball and git dependency, records what they
  /// resolved to in the lockfile and drops the lockfile entries no
  /// package.json uses any more.
  pub async fn install(
    &self,
    workspace: &Workspace,
  ) -> Result<Vec<FetchedRemotePkg>, RemotePackageError> {
    let deps = workspace_remote_deps(workspace, self.production);
    if let Some(lockfile) = self.lockfile {
      // all of them, dev ones included, so a production install doesn't
      // drop the dev dependencies' entries
      let all = workspace_remote_deps(workspace, false);
      lockfile
        .lock()
        .retain_npm_remote(|s| all.iter().any(|(_, d, _)| d.specifier == s));
    }
    let mut fetched = Vec::with_capacity(deps.len());
    for (alias, dep, _base) in deps {
      fetched.push(self.install_one(&alias, &dep).await.map_err(
        |message| RemotePackageError::Fetch {
          alias: alias.clone(),
          specifier: dep.specifier.clone(),
          message,
        },
      )?);
    }
    // what an earlier install fetched that nothing uses any more
    let remote_dir = self
      .node_modules_path
      .join(".deno")
      .join(REMOTE_PACKAGES_DIR);
    if let Ok(entries) = self.sys.fs_read_dir(&remote_dir) {
      for entry in entries.flatten() {
        let path = entry.path();
        if !fetched.iter().any(|pkg| pkg.dir == path) {
          let _ = self.sys.fs_remove_dir_all(&path);
        }
      }
    }
    Ok(fetched)
  }

  async fn install_one(
    &self,
    alias: &StackString,
    dep: &PackageJsonRemoteDep,
  ) -> Result<FetchedRemotePkg, String> {
    let locked = self
      .lockfile
      .and_then(|l| l.lock().npm_remote().get(&dep.specifier).cloned());
    let remote_dir = self
      .node_modules_path
      .join(".deno")
      .join(REMOTE_PACKAGES_DIR);
    let offline = matches!(self.cache_setting, NpmCacheSetting::Only);
    let reload = matches!(self.cache_setting, NpmCacheSetting::ReloadAll);

    let (resolved, dir) = match &dep.kind {
      PackageJsonRemoteDepKind::Tarball(url) => {
        // A locked tarball whose files are already in place isn't
        // downloaded again; anything else is, and checked against the
        // lockfile's integrity when it has one.
        if let Some(integrity) = &locked
          && !reload
        {
          let dir = remote_dir.join(dir_name(alias, integrity));
          if self.is_in_place(&dir, &dep.specifier, integrity) {
            return self.read_fetched(alias, dep, dir);
          }
        }
        if offline && url.scheme() != "file" {
          return Err(
            "not in node_modules and --cached-only is set".to_string(),
          );
        }
        let data = self.download_tarball(url).await?;
        let integrity = match &locked {
          Some(integrity) => integrity.clone(),
          None => deno_npm_cache::tarball_integrity(&data),
        };
        let dir = remote_dir.join(dir_name(alias, &integrity));
        let nv = PackageNv {
          name: alias.clone(),
          version: Version::parse_standard("0.0.0").unwrap(),
        };
        let _ = self.sys.fs_remove_dir_all(&dir);
        deno_npm_cache::verify_and_extract_tarball_with_integrity(
          self.sys, &nv, &data, &integrity, &dir,
        )
        .map_err(|err| {
          if locked.is_some() {
            format!("{err:#} (the integrity deno.lock records for it)")
          } else {
            format!("{err:#}")
          }
        })?;
        (integrity, dir)
      }
      PackageJsonRemoteDepKind::Git { url, committish } => {
        let commit = match &locked {
          Some(commit) if is_full_commit(commit) && !reload => commit.clone(),
          _ => {
            if offline {
              return Err("--cached-only is set".to_string());
            }
            resolve_git_commit(url, committish)?
          }
        };
        if is_full_commit(&commit) {
          let dir = remote_dir.join(dir_name(alias, &commit));
          if !reload && self.is_in_place(&dir, &dep.specifier, &commit) {
            return self.read_fetched(alias, dep, dir);
          }
        }
        if offline {
          return Err(
            "not in node_modules and --cached-only is set".to_string(),
          );
        }
        log::info!(
          "{} {}#{}",
          deno_terminal::colors::green("Download"),
          redact_url_credentials(url),
          &commit[..commit.len().min(12)],
        );
        let (commit, checkout) =
          fetch_git_commit(self.sys, url, committish, &commit, &remote_dir)?;
        let dir = remote_dir.join(dir_name(alias, &commit));
        let _ = self.sys.fs_remove_dir_all(&dir);
        if let Err(err) = self.sys.fs_rename(&checkout, &dir) {
          let _ = self.sys.fs_remove_dir_all(&checkout);
          return Err(format!("{err:#}"));
        }
        (commit, dir)
      }
    };

    self.write_marker(&dir, &dep.specifier, &resolved)?;
    if let Some(lockfile) = self.lockfile {
      lockfile
        .lock()
        .insert_npm_remote(dep.specifier.clone(), resolved);
    }
    self.read_fetched(alias, dep, dir)
  }

  fn is_in_place(&self, dir: &Path, specifier: &str, resolved: &str) -> bool {
    self
      .sys
      .fs_read_to_string_lossy(dir.join(MARKER_FILE))
      .is_ok_and(|text| text == marker_contents(specifier, resolved))
  }

  fn write_marker(
    &self,
    dir: &Path,
    specifier: &str,
    resolved: &str,
  ) -> Result<(), String> {
    let mut file = self
      .sys
      .fs_open(dir.join(MARKER_FILE), &OpenOptions::new_write())
      .map_err(|err| format!("{err:#}"))?;
    file
      .write_all(marker_contents(specifier, resolved).as_bytes())
      .map_err(|err| format!("{err:#}"))
  }

  async fn download_tarball(&self, url: &Url) -> Result<Vec<u8>, String> {
    if url.scheme() == "file" {
      let path = deno_path_util::url_to_file_path(url)
        .map_err(|err| format!("{err:#}"))?;
      return self
        .sys
        .fs_read(&path)
        .map(|d| d.into_owned())
        .map_err(|err| {
          format!("could not read '{}': {err:#}", path.display())
        });
    }
    // (the http client reports the download)
    // a tarball on a configured registry gets that registry's credentials,
    // as npm sends them
    let registry_config = self.npmrc.tarball_config(url);
    let maybe_auth = match registry_config {
      Some(config) => {
        deno_npm_cache::maybe_auth_header_value_for_npm_registry(config)
          .map_err(|err| format!("{err:#}"))?
      }
      None => None,
    };
    let response = self
      .http_client
      .download_with_retries_on_any_tokio_runtime(
        url.clone(),
        maybe_auth,
        None,
        registry_config.map(|c| &**c),
        None,
      )
      .await
      .map_err(|err| format!("{err:#}"))?;
    match response {
      NpmCacheHttpClientResponse::Bytes(response) => Ok(response.bytes),
      NpmCacheHttpClientResponse::NotFound => {
        Err(format!("{} was not found (404)", url))
      }
      NpmCacheHttpClientResponse::NotModified => {
        Err(format!("unexpected 304 response for {}", url))
      }
    }
  }

  fn read_fetched(
    &self,
    alias: &StackString,
    dep: &PackageJsonRemoteDep,
    dir: PathBuf,
  ) -> Result<FetchedRemotePkg, String> {
    let pkg_json_path = dir.join("package.json");
    let text = self
      .sys
      .fs_read_to_string_lossy(&pkg_json_path)
      .map_err(|_| "it has no package.json".to_string())?;
    let pkg_json =
      deno_package_json::PackageJson::load_from_string(pkg_json_path, &text)
        .map_err(|err| format!("invalid package.json: {err:#}"))?;
    let version = pkg_json
      .version
      .as_deref()
      .and_then(|v| Version::parse_from_npm(v).ok())
      .unwrap_or_else(|| Version::parse_standard("0.0.0").unwrap());
    let nv = PackageNv {
      name: pkg_json
        .name
        .as_deref()
        .map(StackString::from)
        .unwrap_or_else(|| alias.clone()),
      version,
    };
    let mut deps = Vec::new();
    for map in [&pkg_json.dependencies, &pkg_json.optional_dependencies]
      .into_iter()
      .flatten()
    {
      for (name, value) in map {
        match PackageJsonDepValue::parse(name, value) {
          Ok(PackageJsonDepValue::Req(req)) => {
            if !deps
              .iter()
              .any(|(a, _): &(StackString, PackageReq)| a.as_str() == name)
            {
              deps.push((StackString::from(name.as_str()), req));
            }
          }
          _ => log::warn!(
            "{} Not installing '{}' ({}), a dependency of '{}' ({}): only registry packages are installed for a git or tarball dependency.",
            deno_terminal::colors::yellow("Warning"),
            name,
            value,
            alias,
            dep.specifier,
          ),
        }
      }
    }
    let has_install_scripts =
      pkg_json.scripts.as_ref().is_some_and(|s| {
        ["preinstall", "install", "postinstall"]
          .iter()
          .any(|n| s.contains_key(*n))
      }) || self.sys.fs_exists_no_err(dir.join("binding.gyp"));
    if has_install_scripts {
      log::warn!(
        "{} The install scripts of '{}' ({}) were not run: a git or tarball dependency's build scripts aren't run.",
        deno_terminal::colors::yellow("Warning"),
        alias,
        dep.specifier,
      );
    }
    Ok(FetchedRemotePkg {
      alias: alias.clone(),
      specifier: dep.specifier.clone(),
      dir,
      nv,
      deps,
    })
  }
}

/// `<alias>@<first 16 characters of the resolved value's hash>`, with a
/// scope's `/` replaced so it stays one directory.
fn dir_name(alias: &str, resolved: &str) -> String {
  let hash = twox_hash::XxHash64::oneshot(0, resolved.as_bytes());
  format!("{}@{:016x}", alias.replace('/', "+"), hash)
}

fn is_full_commit(value: &str) -> bool {
  (value.len() == 40 || value.len() == 64)
    && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_abbreviated_commit(value: &str) -> bool {
  (7..40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Hides a password in a URL shown to the user.
fn redact_url_credentials(url: &str) -> String {
  match Url::parse(url) {
    Ok(mut parsed) if parsed.password().is_some() => {
      let _ = parsed.set_password(Some("***"));
      parsed.to_string()
    }
    _ => url.to_string(),
  }
}

/// A `git` invocation that never prompts, runs no hooks or `ext::`
/// transports and reaches only allowed protocols.
fn git_command() -> Command {
  let mut command = Command::new("git");
  command
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_ALLOW_PROTOCOL", "https:http:ssh:git:file")
    .env("GCM_INTERACTIVE", "never")
    .args([
      "-c",
      "protocol.ext.allow=never",
      "-c",
      "core.fsmonitor=false",
      "-c",
      "submodule.recurse=false",
      "-c",
      "advice.detachedHead=false",
    ])
    .stdin(std::process::Stdio::null());
  command
}

fn run_git(command: &mut Command) -> Result<String, String> {
  let output = command.output().map_err(|err| {
    if err.kind() == std::io::ErrorKind::NotFound {
      "git was not found; git dependencies need git on PATH".to_string()
    } else {
      format!("could not run git: {err}")
    }
  })?;
  if output.status.success() {
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
  } else {
    Err(format!(
      "git failed: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    ))
  }
}

/// The commit `committish` names in the repository, from `git ls-remote`.
fn resolve_git_commit(
  url: &str,
  committish: &PackageJsonGitCommittish,
) -> Result<String, String> {
  if let PackageJsonGitCommittish::Ref(name) = committish
    && is_full_commit(name)
  {
    return Ok(name.to_lowercase());
  }
  let listing = run_git(git_command().args(["ls-remote", "--", url]))?;
  let refs: Vec<(&str, &str)> = listing
    .lines()
    .filter_map(|line| line.split_once('\t'))
    .collect();
  let find = |name: &str| {
    refs
      .iter()
      .find(|(_, r)| *r == name)
      .map(|(sha, _)| sha.to_string())
  };
  // a tag's commit (an annotated tag's `^{}` line), else the ref itself
  let tag_commit = |tag: &str| {
    find(&format!("refs/tags/{tag}^{{}}"))
      .or_else(|| find(&format!("refs/tags/{tag}")))
  };
  match committish {
    PackageJsonGitCommittish::Head => {
      find("HEAD").ok_or_else(|| "the repository has no HEAD".to_string())
    }
    PackageJsonGitCommittish::Ref(name) => tag_commit(name)
      .or_else(|| find(&format!("refs/heads/{name}")))
      .or_else(|| find(name))
      .or_else(|| {
        // an abbreviated commit: resolved once fetched
        is_abbreviated_commit(name).then(|| name.to_lowercase())
      })
      .ok_or_else(|| format!("no branch, tag or commit '{name}'")),
    PackageJsonGitCommittish::Semver(req) => {
      let mut best: Option<(Version, String)> = None;
      for (_, r) in &refs {
        let Some(tag) = r.strip_prefix("refs/tags/") else {
          continue;
        };
        if tag.ends_with("^{}") {
          continue;
        }
        let Ok(version) =
          Version::parse_from_npm(tag.strip_prefix('v').unwrap_or(tag))
        else {
          continue;
        };
        if req.matches(&version)
          && best.as_ref().is_none_or(|(v, _)| version > *v)
          && let Some(sha) = tag_commit(tag)
        {
          best = Some((version, sha));
        }
      }
      best
        .map(|(_, sha)| sha)
        .ok_or_else(|| format!("no tag satisfies semver:{req}"))
    }
  }
}

/// Checks `commit` (full or abbreviated) of the repository out into a new
/// directory in `parent`, without its `.git`: returns the full commit and
/// that directory.
fn fetch_git_commit(
  sys: &impl NpmInstallerSys,
  url: &str,
  committish: &PackageJsonGitCommittish,
  commit: &str,
  parent: &Path,
) -> Result<(String, PathBuf), String> {
  sys
    .fs_create_dir_all(parent)
    .map_err(|err| format!("{err:#}"))?;
  let temp = parent.join(format!(
    ".tmp-git-{}-{:016x}",
    std::process::id(),
    twox_hash::XxHash64::oneshot(0, format!("{url}#{commit}").as_bytes())
  ));
  let _ = sys.fs_remove_dir_all(&temp);
  let result = (|| {
    run_git(git_command().args(["init", "-q", "--template="]).arg(&temp))?;
    let in_temp = |args: &[&str]| {
      let mut command = git_command();
      command.arg("-C").arg(&temp).args(args);
      command
    };
    // The commit itself when the server allows it (GitHub, GitLab, a ref
    // tip anywhere), shallow; else the named ref; else everything.
    let shallow = (is_full_commit(commit)
      && run_git(&mut in_temp(&[
        "fetch", "-q", "--depth", "1", "--", url, commit,
      ]))
      .is_ok())
      || match committish {
        PackageJsonGitCommittish::Ref(name) if !is_abbreviated_commit(name) => {
          run_git(&mut in_temp(&[
            "fetch", "-q", "--depth", "1", "--", url, name,
          ]))
          .is_ok()
        }
        _ => false,
      };
    if !shallow {
      run_git(&mut in_temp(&[
        "fetch",
        "-q",
        "--",
        url,
        "+refs/heads/*:refs/remotes/origin/*",
        "+refs/tags/*:refs/tags/*",
      ]))?;
    }
    let target = if shallow { "FETCH_HEAD" } else { commit };
    let resolved = run_git(&mut in_temp(&[
      "rev-parse",
      "--verify",
      "--end-of-options",
      &format!("{target}^{{commit}}"),
    ]))?;
    let resolved = resolved.trim().to_string();
    if !resolved.starts_with(commit) {
      return Err(format!(
        "fetched commit {resolved} is not the expected {commit}"
      ));
    }
    run_git(&mut in_temp(&["checkout", "-q", "--detach", &resolved]))?;
    sys
      .fs_remove_dir_all(temp.join(".git"))
      .map_err(|err| format!("{err:#}"))?;
    Ok(resolved)
  })();
  match result {
    Ok(resolved) => Ok((resolved, temp)),
    Err(err) => {
      let _ = sys.fs_remove_dir_all(&temp);
      Err(err)
    }
  }
}

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn dir_names() {
    assert_eq!(
      dir_name("@scope/pkg", "abc"),
      format!(
        "@scope+pkg@{:016x}",
        twox_hash::XxHash64::oneshot(0, b"abc")
      )
    );
  }

  #[test]
  fn commits() {
    assert!(is_full_commit(&"a".repeat(40)));
    assert!(!is_full_commit("main"));
    assert!(is_abbreviated_commit("abc1234"));
    assert!(!is_abbreviated_commit("abc"));
  }

  #[test]
  fn redacts_passwords() {
    assert_eq!(
      redact_url_credentials("https://user:secret@example.com/x.tgz"),
      "https://user:***@example.com/x.tgz"
    );
    assert_eq!(
      redact_url_credentials("https://example.com/x.tgz"),
      "https://example.com/x.tgz"
    );
  }
}
