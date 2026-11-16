// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use deno_error::JsErrorBox;
use deno_runtime::deno_permissions::OpenAccessKind;
use deno_runtime::deno_permissions::PermissionsContainer;
use parking_lot::Mutex;
use sys_traits::FsReadLink;

use crate::sys::DenoLibSys;

#[derive(Debug)]
pub enum NpmRegistryReadPermissionCheckerMode {
  Byonm,
  Global(PathBuf),
  Local(PathBuf),
}

#[derive(Debug)]
pub struct NpmRegistryReadPermissionChecker<TSys: DenoLibSys> {
  sys: TSys,
  cache: Mutex<HashMap<PathBuf, PathBuf>>,
  mode: NpmRegistryReadPermissionCheckerMode,
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(inherit)]
#[error("failed canonicalizing '{path}'")]
struct EnsureRegistryReadPermissionError {
  path: PathBuf,
  #[source]
  #[inherit]
  source: std::io::Error,
}

const MAX_DANGLING_LINK_DEPTH: usize = 40;

/// Canonicalizes the nearest existing entry, then appends any unresolved path
/// components, explicitly following dangling symlinks and junctions. This
/// differs from `deno_path_util::fs::canonicalize_path_maybe_not_exists`, which
/// treats a dangling link's name as an ordinary unresolved component.
fn canonicalize_path_maybe_not_exists_following_dangling_links<TSys>(
  sys: &TSys,
  path: &Path,
) -> Result<PathBuf, std::io::Error>
where
  TSys: sys_traits::FsCanonicalize + sys_traits::FsMetadata + FsReadLink,
{
  canonicalize_path_maybe_not_exists_following_dangling_links_inner(
    sys,
    path,
    &mut HashSet::new(),
  )
}

fn canonicalize_path_maybe_not_exists_following_dangling_links_inner<TSys>(
  sys: &TSys,
  mut path: &Path,
  seen_links: &mut HashSet<PathBuf>,
) -> Result<PathBuf, std::io::Error>
where
  TSys: sys_traits::FsCanonicalize + sys_traits::FsMetadata + FsReadLink,
{
  let mut names_stack = Vec::new();
  loop {
    match sys.fs_symlink_metadata(path) {
      Ok(_) => {
        let mut canonicalized_path = match sys.fs_canonicalize(path) {
          Ok(path) => path,
          Err(err) if err.kind() == ErrorKind::NotFound => {
            let normalized_path =
              deno_path_util::normalize_path(Cow::Borrowed(path)).into_owned();
            if seen_links.len() >= MAX_DANGLING_LINK_DEPTH
              || !seen_links.insert(normalized_path)
            {
              return Err(std::io::Error::other(
                "too many levels of symbolic links",
              ));
            }
            let target = sys.fs_read_link(path)?;
            let target = if target.is_absolute() {
              target
            } else {
              path.parent().unwrap_or_else(|| Path::new(".")).join(target)
            };
            let target =
              deno_path_util::normalize_path(Cow::Owned(target)).into_owned();
            canonicalize_path_maybe_not_exists_following_dangling_links_inner(
              sys, &target, seen_links,
            )?
          }
          Err(err) => return Err(err),
        };
        for name in names_stack.into_iter().rev() {
          canonicalized_path = canonicalized_path.join(name);
        }
        return Ok(
          deno_path_util::normalize_path(Cow::Owned(canonicalized_path))
            .into_owned(),
        );
      }
      Err(err) if err.kind() == ErrorKind::NotFound => {
        names_stack.push(match path.file_name() {
          Some(name) => name.to_owned(),
          None => return Err(err),
        });
        path = match path.parent() {
          Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
          Some(parent) => parent,
          None => return Err(err),
        };
      }
      Err(err) => return Err(err),
    }
  }
}

/// Hides from module resolution a package.json that a read deny covers
/// (`--deny-read`, `--ignore-read`, a read denied at a prompt), by its path
/// as named or the path it resolves to, so resolution goes on as if it were
/// missing (DENEXT.md item 439). Resolution reads package.json files without
/// the allow check (as before); only the denies apply.
///
/// Resolution probes a package.json in every ancestor directory, and most of
/// those don't exist: a file that doesn't exist has nothing to hide (the
/// loader finds none either), so it costs one `realpath` and no permission
/// check. For a file that exists, the verdict is cached by its resolved path
/// (with the path as named, which the name check covers), for as long as the
/// read denies stay the same; a read denied at a prompt later clears the
/// cache.
///
/// The check resolves the path and the loader then opens it by name, as
/// every path-based check does (`check_resolved_path`): a link changed in
/// between is followed unchecked. Closing that needs the read to go through
/// the descriptor the check opened (`deno_permissions::checked_open`), which
/// the node resolver's file system interface doesn't offer yet.
#[derive(Debug)]
pub struct DenyReadPackageJsonFilter<TSys> {
  sys: TSys,
  permissions: PermissionsContainer,
  verdicts: Mutex<PackageJsonVerdicts>,
}

#[derive(Debug, Default)]
struct PackageJsonVerdicts {
  /// The read-deny state the verdicts were made under
  /// (`PermissionsContainer::read_deny_generation`).
  generation: usize,
  /// `(path as named, resolved path)` → hidden.
  hidden: HashMap<(PathBuf, PathBuf), bool>,
}

impl<TSys> DenyReadPackageJsonFilter<TSys> {
  pub fn new(sys: TSys, permissions: PermissionsContainer) -> Self {
    Self {
      sys,
      permissions,
      verdicts: Default::default(),
    }
  }
}

impl<TSys> node_resolver::PackageJsonReadFilter
  for DenyReadPackageJsonFilter<TSys>
where
  TSys: sys_traits::FsCanonicalize
    + sys_traits::FsMetadata
    + FsReadLink
    + std::fmt::Debug
    + Send
    + Sync,
{
  fn is_hidden(&self, path: &Path) -> bool {
    if !self.permissions.has_read_deny_rules() {
      return false;
    }
    let path = deno_path_util::normalize_path(Cow::Borrowed(path));
    let resolved = match self.sys.fs_canonicalize(&path) {
      Ok(resolved) => resolved,
      // Nothing there (or a link to nothing): the loader finds no
      // package.json either, hidden or not.
      Err(err) if err.kind() == ErrorKind::NotFound => return false,
      Err(_) => {
        // Not resolvable the plain way: check it as before, uncached.
        return self.is_denied(&path, || {
          canonicalize_path_maybe_not_exists_following_dangling_links(
            &self.sys, &path,
          )
          .ok()
        });
      }
    };
    let generation = self.permissions.read_deny_generation();
    let key = (path.into_owned(), resolved);
    {
      let mut verdicts = self.verdicts.lock();
      if verdicts.generation != generation {
        verdicts.generation = generation;
        verdicts.hidden.clear();
      }
      if let Some(hidden) = verdicts.hidden.get(&key) {
        return *hidden;
      }
    }
    let hidden = self.is_denied(&key.0, || Some(key.1.clone()));
    if hidden {
      log::debug!(
        "{} is hidden from module resolution by a read deny",
        key.0.display()
      );
    }
    self.verdicts.lock().hidden.insert(key, hidden);
    hidden
  }
}

impl<TSys> DenyReadPackageJsonFilter<TSys> {
  /// Whether a read deny covers `path` as named or the path it resolves to.
  fn is_denied(
    &self,
    path: &Path,
    resolve: impl FnOnce() -> Option<PathBuf>,
  ) -> bool {
    if self.permissions.check_read_not_denied(path, None).is_err() {
      return true;
    }
    match resolve() {
      Some(resolved) if resolved != path => self
        .permissions
        .check_read_not_denied(&resolved, None)
        .is_err(),
      _ => false,
    }
  }
}

fn path_has_node_modules_component(path: &Path) -> bool {
  path.components().any(|c| c.as_os_str() == "node_modules")
}

/// Returns whether the prefix of `path` at the configured registry depth
/// resolves to the canonical registry root. Windows may spell an existing
/// ancestor using an 8.3 short name or different casing, so a component-wise
/// lexical prefix check alone is insufficient there.
fn path_uses_canonical_registry_prefix(
  path: &Path,
  registry_path: &Path,
  registry_path_canon: &Path,
  canonicalize: impl FnOnce(&Path) -> Option<PathBuf>,
) -> bool {
  let Some(extra_component_count) = path
    .components()
    .count()
    .checked_sub(registry_path.components().count())
  else {
    return false;
  };
  let Some(candidate_registry_path) =
    path.ancestors().nth(extra_component_count)
  else {
    return false;
  };
  canonicalize(candidate_registry_path)
    .is_some_and(|path| path == registry_path_canon)
}

impl<TSys: DenoLibSys> NpmRegistryReadPermissionChecker<TSys> {
  pub fn new(sys: TSys, mode: NpmRegistryReadPermissionCheckerMode) -> Self {
    Self {
      sys,
      cache: Default::default(),
      mode,
    }
  }

  /// The deny half of `ensure_read_permission` for a module that the module
  /// loader reads straight from a `node_modules` directory (an ES module or
  /// JSON `import()` of a file in a package): in a BYONM project such a load
  /// is exempt from the allow scopes, never from `--deny-read` on its real
  /// path or its spelling (DENEXT.md item 435). The other modes keep their
  /// registry rules and are not checked here. The module is read by name
  /// after the check (the note on [`DenyReadPackageJsonFilter`]).
  pub fn ensure_module_not_denied(
    &self,
    permissions: &PermissionsContainer,
    path: &Path,
  ) -> Result<(), JsErrorBox>
  where
    TSys: FsReadLink,
  {
    if !matches!(self.mode, NpmRegistryReadPermissionCheckerMode::Byonm)
      || permissions.query_read_all()
    {
      return Ok(());
    }
    let path = deno_path_util::normalize_path(Cow::Borrowed(path));
    if let Ok(path_canon) =
      canonicalize_path_maybe_not_exists_following_dangling_links(
        &self.sys, &path,
      )
    {
      permissions
        .check_read_not_denied(&path_canon, Some("import()"))
        .map_err(JsErrorBox::from_err)?;
    }
    permissions
      .check_read_not_denied(&path, Some("import()"))
      .map_err(JsErrorBox::from_err)
  }

  #[must_use = "the resolved return value to mitigate time-of-check to time-of-use issues"]
  pub fn ensure_read_permission<'a>(
    &self,
    permissions: &mut PermissionsContainer,
    path: Cow<'a, Path>,
  ) -> Result<Cow<'a, Path>, JsErrorBox>
  where
    TSys: FsReadLink,
  {
    if permissions.query_read_all() {
      return Ok(path); // skip permissions checks below
    }

    match &self.mode {
      NpmRegistryReadPermissionCheckerMode::Byonm => {
        // Normalize the path to collapse `.` and `..` components before
        // checking for a `node_modules` ancestor. Otherwise a traversal path
        // such as `./node_modules/../../../etc/passwd` would slip through the
        // check and be read without `--allow-read`.
        let path = deno_path_util::normalize_path(path);
        if !path_has_node_modules_component(&path) {
          return permissions
            .check_open(path, OpenAccessKind::Read, None)
            .map(|p| p.into_path())
            .map_err(JsErrorBox::from_err);
        }
        // A `node_modules` component in the spelling is not enough: a link
        // inside `node_modules` (`node_modules/pkg -> ../../outside`) would
        // otherwise make any file it reaches readable without `--allow-read`
        // and past `--deny-read`. Keep the exemption only when the real path
        // is still inside a `node_modules` directory (a pnpm `.pnpm` store
        // link is); check anything else like an ordinary read.
        match canonicalize_path_maybe_not_exists_following_dangling_links(
          &self.sys, &path,
        ) {
          // Exempt from the allow scopes, never from a deny: a
          // `--deny-read` covering the real path or the spelling refuses it,
          // as it refuses `fs.readFileSync` (DENEXT.md item 435).
          Ok(path_canon) if path_has_node_modules_component(&path_canon) => {
            permissions
              .check_read_not_denied(&path_canon, None)
              .and_then(|()| permissions.check_read_not_denied(&path, None))
              .map_err(JsErrorBox::from_err)?;
            Ok(path)
          }
          Ok(path_canon) => permissions
            .check_open(Cow::Owned(path_canon), OpenAccessKind::Read, None)
            .map(|p| p.into_path())
            .map_err(JsErrorBox::from_err),
          // The real path is unknown (an unreadable ancestor, a link loop):
          // the spelling has to be granted itself.
          Err(_) => permissions
            .check_open(path, OpenAccessKind::Read, None)
            .map(|p| p.into_path())
            .map_err(JsErrorBox::from_err),
        }
      }
      NpmRegistryReadPermissionCheckerMode::Global(registry_path)
      | NpmRegistryReadPermissionCheckerMode::Local(registry_path) => {
        let mut path = path;
        let has_parent_dir = path
          .components()
          .any(|c| matches!(c, std::path::Component::ParentDir));
        // Allow reading only when the lexical path is in the registry, or on
        // Windows when an equivalent spelling of that registry prefix is.
        let is_path_in_registry =
          !has_parent_dir && path.starts_with(registry_path);
        let could_use_canonical_registry_prefix = cfg!(windows)
          && !has_parent_dir
          && path.components().count() >= registry_path.components().count();

        if is_path_in_registry || could_use_canonical_registry_prefix {
          let mut cache = self.cache.lock();
          let mut canonicalize =
            |path: &Path| -> Result<Option<PathBuf>, JsErrorBox> {
              match cache.get(path) {
                Some(canon) => Ok(Some(canon.clone())),
                None => match self.sys.fs_canonicalize(path) {
                  Ok(canon) => {
                    cache.insert(path.to_path_buf(), canon.clone());
                    Ok(Some(canon))
                  }
                  Err(e) => {
                    if e.kind() == ErrorKind::NotFound {
                      return Ok(None);
                    }
                    Err(JsErrorBox::from_err(
                      EnsureRegistryReadPermissionError {
                        path: path.to_path_buf(),
                        source: e,
                      },
                    ))
                  }
                },
              }
            };
          if let Some(registry_path_canon) = canonicalize(registry_path)? {
            let is_path_in_registry = is_path_in_registry
              || (could_use_canonical_registry_prefix
                && path_uses_canonical_registry_prefix(
                  &path,
                  registry_path,
                  &registry_path_canon,
                  |path| canonicalize(path).ok().flatten(),
                ));
            if is_path_in_registry {
              let path_canon = match canonicalize(&path)? {
                Some(path_canon) => Some(path_canon),
                None => {
                  // Resolution errors must not disclose host filesystem state
                  // before the ordinary read permission check below.
                  canonicalize_path_maybe_not_exists_following_dangling_links(
                    &self.sys, &path,
                  )
                  .ok()
                }
              };
              if let Some(path_canon) = path_canon {
                if path_canon.starts_with(&registry_path_canon) {
                  return Ok(Cow::Owned(path_canon));
                }
                path = Cow::Owned(path_canon);
              }
            }
          }
        }

        permissions
          .check_open(path, OpenAccessKind::Read, None)
          .map(|p| p.into_path())
          .map_err(JsErrorBox::from_err)
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use sys_traits::FsCanonicalize;
  use sys_traits::FsCreateDirAll;
  use sys_traits::FsSymlinkDir;

  use super::*;

  fn create_dangling_link_chain(
    sys: &sys_traits::impls::InMemorySys,
    root: &str,
    length: usize,
  ) {
    sys.fs_create_dir_all(root).unwrap();
    for index in 0..length {
      let link = PathBuf::from(format!("{root}/{index}"));
      let target = if index + 1 == length {
        PathBuf::from(format!("{root}/missing"))
      } else {
        PathBuf::from(format!("{root}/{}", index + 1))
      };
      sys.fs_symlink_dir(&target, &link).unwrap();
    }
  }

  #[test]
  fn follows_bounded_dangling_link_chain() {
    let sys = sys_traits::impls::InMemorySys::default();
    create_dangling_link_chain(&sys, "/links", MAX_DANGLING_LINK_DEPTH);

    assert_eq!(
      canonicalize_path_maybe_not_exists_following_dangling_links(
        &sys,
        Path::new("/links/0"),
      )
      .unwrap(),
      PathBuf::from("/links/missing"),
    );
  }

  #[test]
  fn rejects_excessive_dangling_link_chain() {
    let sys = sys_traits::impls::InMemorySys::default();
    create_dangling_link_chain(&sys, "/links", MAX_DANGLING_LINK_DEPTH + 1);

    let err = canonicalize_path_maybe_not_exists_following_dangling_links(
      &sys,
      Path::new("/links/0"),
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Other);
  }

  /// Counts the file system lookups the filter makes.
  #[derive(Debug, Default)]
  struct CountingSys {
    calls: std::sync::atomic::AtomicUsize,
  }

  impl CountingSys {
    fn take(&self) -> usize {
      self.calls.swap(0, std::sync::atomic::Ordering::SeqCst)
    }
    fn count(&self) {
      self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
  }

  impl sys_traits::BaseFsCanonicalize for CountingSys {
    fn base_fs_canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
      self.count();
      sys_traits::impls::RealSys.fs_canonicalize(path)
    }
  }

  impl sys_traits::BaseFsMetadata for CountingSys {
    type Metadata =
      <sys_traits::impls::RealSys as sys_traits::BaseFsMetadata>::Metadata;
    fn base_fs_metadata(&self, path: &Path) -> std::io::Result<Self::Metadata> {
      self.count();
      sys_traits::BaseFsMetadata::base_fs_metadata(
        &sys_traits::impls::RealSys,
        path,
      )
    }
    fn base_fs_symlink_metadata(
      &self,
      path: &Path,
    ) -> std::io::Result<Self::Metadata> {
      self.count();
      sys_traits::BaseFsMetadata::base_fs_symlink_metadata(
        &sys_traits::impls::RealSys,
        path,
      )
    }
  }

  impl sys_traits::BaseFsReadLink for CountingSys {
    fn base_fs_read_link(&self, path: &Path) -> std::io::Result<PathBuf> {
      self.count();
      sys_traits::impls::RealSys.fs_read_link(path)
    }
  }

  // DENEXT.md item 439's filter: a package.json that doesn't exist costs one
  // lookup, an existing one's verdict is cached, and the denies still hold
  // by name and through a link.
  #[test]
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  fn package_json_filter_is_cheap_for_missing_files_and_caches_verdicts() {
    use std::sync::Arc;

    use deno_runtime::deno_permissions::Permissions;
    use deno_runtime::deno_permissions::PermissionsOptions;
    use deno_runtime::deno_permissions::RuntimePermissionDescriptorParser;
    use node_resolver::PackageJsonReadFilter;

    // A fresh directory, removed at the end (this crate has no tempfile).
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
      fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
      }
    }
    let base = std::env::temp_dir()
      .join(format!("deno-lib-pkg-json-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let _remove = RemoveOnDrop(base.clone());
    let root = std::fs::canonicalize(&base).unwrap();
    let secret = root.join("secret");
    std::fs::create_dir_all(&secret).unwrap();
    std::fs::write(secret.join("package.json"), "{}").unwrap();
    let app = root.join("app");
    let deep = app.join("a/b/c/d/e/f/g/h");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(app.join("package.json"), "{}").unwrap();
    let link = root.join("link");
    std::fs::create_dir_all(&link).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
      secret.join("package.json"),
      link.join("package.json"),
    )
    .unwrap();

    let parser =
      RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
    let permissions = PermissionsContainer::new(
      Arc::new(RuntimePermissionDescriptorParser::new(
        sys_traits::impls::RealSys,
      )),
      Permissions::from_options(
        &parser,
        &PermissionsOptions {
          deny_read: Some(vec![secret.to_string_lossy().into_owned()]),
          ..Default::default()
        },
      )
      .unwrap(),
    );
    let filter =
      DenyReadPackageJsonFilter::new(CountingSys::default(), permissions);
    // Missing, however deep: one lookup.
    assert!(!filter.is_hidden(&deep.join("package.json")));
    assert_eq!(filter.sys.take(), 1);
    // Existing and allowed: resolved once, then cached.
    assert!(!filter.is_hidden(&app.join("package.json")));
    assert!(!filter.is_hidden(&app.join("package.json")));
    assert_eq!(filter.sys.take(), 2);
    // Denied by name, and through a link, every time.
    for _ in 0..2 {
      assert!(filter.is_hidden(&secret.join("package.json")));
      #[cfg(unix)]
      assert!(filter.is_hidden(&link.join("package.json")));
    }
  }

  #[test]
  fn recognizes_canonical_registry_prefix_alias() {
    let sys = sys_traits::impls::InMemorySys::default();
    sys.fs_create_dir_all("/real/root/node_modules").unwrap();
    sys.fs_create_dir_all("/alias/root").unwrap();
    sys.fs_create_dir_all("/other/root/node_modules").unwrap();
    sys
      .fs_symlink_dir("/real/root/node_modules", "/alias/root/node_modules")
      .unwrap();

    let registry_path = Path::new("/real/root/node_modules");
    let canonicalize = |path: &Path| sys.fs_canonicalize(path).ok();
    assert!(path_uses_canonical_registry_prefix(
      Path::new("/alias/root/node_modules/pkg/missing"),
      registry_path,
      registry_path,
      canonicalize,
    ));
    assert!(!path_uses_canonical_registry_prefix(
      Path::new("/other/root/node_modules/pkg/missing"),
      registry_path,
      registry_path,
      canonicalize,
    ));
  }
}
