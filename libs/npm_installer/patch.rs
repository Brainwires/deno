// Copyright 2018-2026 the Deno authors. MIT license.

//! Applies unified-diff (`.patch`) files to an extracted npm package, mirroring
//! the `patchedDependencies` workflow of pnpm/bun and the `patch-package` tool.
//!
//! Patches are applied to the per-project `node_modules` copy of a package
//! (never to the shared global cache), after the package has been cloned out of
//! the cache. Because that clone is made of hardlinks back into the global
//! cache, each patched file is rewritten by removing the link and writing a
//! fresh file so the global cache is left untouched.

use std::path::Path;
use std::path::PathBuf;

use sys_traits::FsRead;
use sys_traits::FsRemoveFile;
use sys_traits::FsWrite;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum ApplyPatchError {
  #[class(inherit)]
  #[error("Failed to read patch file '{}'.", path.display())]
  ReadPatch {
    path: PathBuf,
    #[source]
    #[inherit]
    source: std::io::Error,
  },
  #[class(type)]
  #[error("Failed to parse patch file '{}'.", path.display())]
  ParsePatch {
    path: PathBuf,
    #[source]
    source: diffy::ParsePatchError,
  },
  #[class(type)]
  #[error("Patch '{}' does not name a target file.", path.display())]
  MissingTarget { path: PathBuf },
  #[class(type)]
  #[error("Patch '{}' names '{target}', which is outside the package.", path.display())]
  TargetOutsidePackage { path: PathBuf, target: String },
  #[class(inherit)]
  #[error("Failed to read '{}' while applying patch '{}'.", target.display(), patch.display())]
  ReadTarget {
    target: PathBuf,
    patch: PathBuf,
    #[source]
    #[inherit]
    source: std::io::Error,
  },
  #[class(type)]
  #[error("Failed to apply patch '{}' to '{}'.", patch.display(), target.display())]
  Apply {
    target: PathBuf,
    patch: PathBuf,
    #[source]
    source: diffy::ApplyError,
  },
  #[class(inherit)]
  #[error("Failed to write '{}' while applying patch '{}'.", target.display(), patch.display())]
  WriteTarget {
    target: PathBuf,
    patch: PathBuf,
    #[source]
    #[inherit]
    source: std::io::Error,
  },
}

/// Applies the unified diff at `patch_path` to the package extracted at
/// `package_dir`. The patch may touch multiple files.
pub fn apply_patch<TSys: FsRead + FsWrite + FsRemoveFile>(
  sys: &TSys,
  package_dir: &Path,
  patch_path: &Path,
) -> Result<(), ApplyPatchError> {
  let contents = sys.fs_read_to_string(patch_path).map_err(|source| {
    ApplyPatchError::ReadPatch {
      path: patch_path.to_path_buf(),
      source,
    }
  })?;

  for file_diff in split_file_diffs(&contents) {
    let patch = diffy::Patch::from_str(file_diff).map_err(|source| {
      ApplyPatchError::ParsePatch {
        path: patch_path.to_path_buf(),
        source,
      }
    })?;

    let target_rel = patch
      .modified()
      .or_else(|| patch.original())
      .map(strip_diff_prefix)
      .ok_or_else(|| ApplyPatchError::MissingTarget {
        path: patch_path.to_path_buf(),
      })?;
    // A patch only rewrites files inside its package.
    let target_rel_path = Path::new(target_rel);
    if !target_rel_path
      .components()
      .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
      return Err(ApplyPatchError::TargetOutsidePackage {
        path: patch_path.to_path_buf(),
        target: target_rel.to_string(),
      });
    }
    let target = package_dir.join(target_rel_path);

    let original = sys.fs_read_to_string(&target).map_err(|source| {
      ApplyPatchError::ReadTarget {
        target: target.clone(),
        patch: patch_path.to_path_buf(),
        source,
      }
    })?;
    let patched = diffy::apply(&original, &patch).map_err(|source| {
      ApplyPatchError::Apply {
        target: target.clone(),
        patch: patch_path.to_path_buf(),
        source,
      }
    })?;

    // Break the hardlink back into the global cache before writing: remove the
    // existing name, then write a fresh file so the cache stays pristine.
    let _ = sys.fs_remove_file(&target);
    sys
      .fs_write(&target, patched.as_bytes())
      .map_err(|source| ApplyPatchError::WriteTarget {
        target: target.clone(),
        patch: patch_path.to_path_buf(),
        source,
      })?;
  }

  Ok(())
}

/// Splits a (possibly multi-file) git/unified diff into one slice per file.
///
/// pnpm, bun and patch-package all emit git-style diffs where each file begins
/// with a `diff --git` line; we slice on that and hand the `--- `/`+++ `/`@@`
/// portion to the diff applier. A plain `diff -u` patch (no `diff --git`
/// header) is treated as a single-file diff.
fn split_file_diffs(contents: &str) -> Vec<&str> {
  let mut sections = Vec::new();
  let mut start: Option<usize> = None;
  let mut search_from = 0;
  while let Some(rel) = contents[search_from..].find("diff --git ") {
    let idx = search_from + rel;
    // only treat it as a header when at the start of a line
    if idx == 0 || contents.as_bytes()[idx - 1] == b'\n' {
      if let Some(prev) = start.take() {
        sections.push(&contents[prev..idx]);
      }
      start = Some(idx);
    }
    search_from = idx + "diff --git ".len();
  }
  match start {
    Some(prev) => sections.push(&contents[prev..]),
    None if !contents.trim().is_empty() => sections.push(contents),
    None => {}
  }
  sections
    .into_iter()
    .filter_map(|section| {
      // diffy parses starting at the `--- ` header, so drop any leading
      // `diff --git`/`index`/`new file mode` git metadata lines.
      section.find("--- ").map(|pos| &section[pos..])
    })
    .collect()
}

/// Strips a leading `a/` or `b/` path prefix that git-style diffs add to file
/// names, and trims any trailing tab-separated timestamp.
fn strip_diff_prefix(name: &str) -> &str {
  let name = name.split('\t').next().unwrap_or(name);
  name
    .strip_prefix("a/")
    .or_else(|| name.strip_prefix("b/"))
    .unwrap_or(name)
}

/// Warns about `patchedDependencies` entries for an installed package that
/// apply to none of its installed versions (a patch for `pkg@1.0.0` when
/// `pkg@2.0.0` is installed), once per entry and process.
pub fn warn_unapplied_patches(
  patches: &deno_config::deno_json::PatchedDependencies,
  snapshot: &deno_npm::resolution::NpmResolutionSnapshot,
) {
  use std::collections::HashSet;
  use std::sync::Mutex;

  if patches.is_empty() {
    return;
  }
  static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
  let installed = snapshot
    .all_packages_for_every_system()
    .map(|p| &p.id.nv)
    .collect::<Vec<_>>();
  let unapplied = patches.unapplied(installed);
  if unapplied.is_empty() {
    return;
  }
  let mut warned = WARNED.lock().unwrap();
  let warned = warned.get_or_insert_with(HashSet::new);
  for (key, versions) in unapplied {
    if !warned.insert(key.to_string()) {
      continue;
    }
    let versions = versions
      .iter()
      .map(|v| v.to_string())
      .collect::<Vec<_>>()
      .join(", ");
    log::warn!(
      "{} \"patchedDependencies\" entry \"{}\" was not applied: no installed version matches it (installed: {}).",
      deno_terminal::colors::yellow("Warning"),
      key,
      versions,
    );
  }
}

#[cfg(test)]
mod tests {
  use sys_traits::FsCreateDirAll;

  use super::*;

  #[test]
  fn refuses_a_target_outside_the_package() {
    let sys = sys_traits::impls::InMemorySys::default();
    sys.fs_create_dir_all("/pkg").unwrap();
    let patch =
      "--- a/../../outside.js\n+++ b/../../outside.js\n@@ -1 +1 @@\n-a\n+b\n";
    sys.fs_write("/p.patch", patch).unwrap();
    let err =
      apply_patch(&sys, Path::new("/pkg"), Path::new("/p.patch")).unwrap_err();
    assert!(
      matches!(err, ApplyPatchError::TargetOutsidePackage { .. }),
      "{err:?}"
    );
  }

  #[test]
  fn applies_a_git_style_patch() {
    let sys = sys_traits::impls::InMemorySys::default();
    sys.fs_create_dir_all("/pkg").unwrap();
    sys.fs_write("/pkg/index.js", "a\n").unwrap();
    let patch = "diff --git a/index.js b/index.js\nindex 1..2 100644\n--- a/index.js\n+++ b/index.js\n@@ -1 +1 @@\n-a\n+b\n";
    sys.fs_write("/p.patch", patch).unwrap();
    apply_patch(&sys, Path::new("/pkg"), Path::new("/p.patch")).unwrap();
    assert_eq!(sys.fs_read_to_string("/pkg/index.js").unwrap(), "b\n");
  }
}
