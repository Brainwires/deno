// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;

use deno_path_util::normalize_path;
use sys_traits::FsDirEntry;
use sys_traits::FsMetadata;
use sys_traits::FsMetadataValue;
use sys_traits::FsRead;
use sys_traits::FsReadDir;

use super::FilePatterns;
use crate::glob::FilePatternsMatch;
use crate::glob::PathKind;
use crate::glob::PathOrPattern;
use crate::glob::gitignore::DirGitIgnores;
use crate::glob::gitignore::GitIgnoreTree;

#[derive(Debug, Clone)]
pub struct WalkEntry<'a> {
  pub path: &'a Path,
  pub metadata: &'a dyn FsMetadataValue,
  pub patterns: &'a FilePatterns,
}

/// Collects file paths that satisfy the given predicate, by recursively walking `files`.
/// If the walker visits a path that is listed in `ignore`, it skips descending into the directory.
pub struct FileCollector<TFilter: Fn(WalkEntry) -> bool> {
  file_filter: TFilter,
  ignore_git_folder: bool,
  ignore_node_modules: bool,
  vendor_folder: Option<PathBuf>,
  use_gitignore: bool,
}

impl<TFilter: Fn(WalkEntry) -> bool> FileCollector<TFilter> {
  pub fn new(file_filter: TFilter) -> Self {
    Self {
      file_filter,
      ignore_git_folder: false,
      ignore_node_modules: false,
      vendor_folder: None,
      use_gitignore: false,
    }
  }

  pub fn ignore_node_modules(mut self) -> Self {
    self.ignore_node_modules = true;
    self
  }

  pub fn set_vendor_folder(mut self, vendor_folder: Option<PathBuf>) -> Self {
    self.vendor_folder = vendor_folder;
    self
  }

  pub fn ignore_git_folder(mut self) -> Self {
    self.ignore_git_folder = true;
    self
  }

  pub fn use_gitignore(mut self) -> Self {
    self.use_gitignore = true;
    self
  }

  pub fn collect_file_patterns<TSys: FsRead + FsMetadata + FsReadDir>(
    &self,
    sys: &TSys,
    file_patterns: &FilePatterns,
  ) -> Vec<PathBuf> {
    fn is_pattern_matched(
      maybe_git_ignore: Option<&DirGitIgnores>,
      path: &Path,
      is_dir: bool,
      file_patterns: &FilePatterns,
    ) -> bool {
      let path_kind = match is_dir {
        true => PathKind::Directory,
        false => PathKind::File,
      };
      match file_patterns.matches_path_detail(path, path_kind) {
        FilePatternsMatch::Passed => {
          // check gitignore
          let is_gitignored = maybe_git_ignore
            .as_ref()
            .map(|git_ignore| git_ignore.is_ignored(path, is_dir))
            .unwrap_or(false);
          !is_gitignored
        }
        FilePatternsMatch::PassedOptedOutExclude => true,
        FilePatternsMatch::Excluded => false,
      }
    }

    let mut maybe_git_ignores = if self.use_gitignore {
      // Override explicitly specified include paths in the
      // .gitignore file. This does not apply to globs because
      // that is way too complicated to reason about.
      let include_paths = file_patterns
        .include
        .as_ref()
        .map(|include| {
          include
            .inner()
            .iter()
            .filter_map(|path_or_pattern| {
              if let PathOrPattern::Path(p) = path_or_pattern {
                Some(p.clone())
              } else {
                None
              }
            })
            .collect::<Vec<_>>()
        })
        .unwrap_or_default();
      Some(GitIgnoreTree::new(sys, include_paths))
    } else {
      None
    };
    let mut target_files = Vec::new();
    let mut visited_paths: HashSet<PathBuf> = HashSet::default();
    let file_patterns_by_base = file_patterns.split_by_base();
    for file_patterns in file_patterns_by_base {
      let specified_path = normalize_path(Cow::Borrowed(&file_patterns.base));
      let mut pending_dirs = VecDeque::new();
      let mut handle_entry =
        |path: PathBuf,
         metadata: &dyn FsMetadataValue,
         pending_dirs: &mut VecDeque<PathBuf>| {
          let maybe_gitignore =
            maybe_git_ignores.as_mut().and_then(|git_ignores| {
              if metadata.file_type().is_dir() {
                git_ignores.get_resolved_git_ignore_for_dir(&path)
              } else {
                git_ignores.get_resolved_git_ignore_for_file(&path)
              }
            });
          if !is_pattern_matched(
            maybe_gitignore.as_deref(),
            &path,
            metadata.file_type().is_dir(),
            &file_patterns,
          ) {
            // ignore
          } else if metadata.file_type().is_dir() {
            // allow the user to opt out of ignoring by explicitly specifying the dir
            let opt_out_ignore = specified_path == path;
            // Don't descend into a directory no include pattern can match
            // under: expanding workspace members (`packages/*/deno.json`)
            // otherwise walked every member's whole tree, `.venv` and build
            // output included (denoland/deno#36668).
            let should_ignore_dir = !opt_out_ignore
              && (self.is_ignored_dir(&path)
                || !file_patterns.can_match_under_dir(&path));
            if !should_ignore_dir && visited_paths.insert(path.clone()) {
              pending_dirs.push_back(path);
            }
          } else if (self.file_filter)(WalkEntry {
            path: &path,
            metadata,
            patterns: &file_patterns,
          }) && visited_paths.insert(path.clone())
          {
            target_files.push(path);
          }
        };

      if let Ok(metadata) = sys.fs_metadata(&specified_path) {
        handle_entry(
          specified_path.to_path_buf(),
          &metadata,
          &mut pending_dirs,
        );
      }

      // use an iterator in order to minimize the number of file system operations
      while let Some(next_dir) = pending_dirs.pop_front() {
        let Ok(entries) = sys.fs_read_dir(&next_dir) else {
          continue;
        };
        for entry in entries {
          let Ok(entry) = entry else {
            continue;
          };
          let Ok(metadata) = entry.metadata() else {
            continue;
          };
          handle_entry(entry.path().into_owned(), &metadata, &mut pending_dirs)
        }
      }
    }
    target_files
  }

  fn is_ignored_dir(&self, path: &Path) -> bool {
    path
      .file_name()
      .map(|dir_name| {
        let dir_name = dir_name.to_string_lossy().to_lowercase();

        match dir_name.as_str() {
          "node_modules" => self.ignore_node_modules,
          ".git" => self.ignore_git_folder,
          _ => false,
        }
      })
      .unwrap_or(false)
      || self.is_vendor_folder(path)
  }

  fn is_vendor_folder(&self, path: &Path) -> bool {
    self
      .vendor_folder
      .as_ref()
      .map(|vendor_folder| path == *vendor_folder)
      .unwrap_or(false)
  }
}

#[cfg(test)]
mod test {
  use sys_traits::BaseFsMetadata;
  use sys_traits::BaseFsRead;
  use sys_traits::BaseFsReadDir;
  use sys_traits::FsCanonicalize;
  use sys_traits::FsCreateDirAll;
  use sys_traits::FsWrite;
  use sys_traits::impls::RealSys;

  use super::*;
  use crate::glob::FilePatterns;
  use crate::glob::PathOrPattern;
  use crate::glob::PathOrPatternSet;

  fn create_files(dir_path: &Path, files: &[&str]) {
    RealSys.fs_create_dir_all(dir_path).unwrap();
    for f in files {
      RealSys.fs_write(dir_path.join(f), "").unwrap();
    }
  }

  #[test]
  fn test_collect_files() {
    let t = tempfile::tempdir().unwrap();

    // dir.ts
    // ├── a.ts
    // ├── b.js
    // ├── child
    // |   ├── .git
    // |   |   └── git.js
    // |   ├── node_modules
    // |   |   └── node_modules.js
    // |   ├── vendor
    // |   |   └── vendor.js
    // │   ├── e.mjs
    // │   ├── f.mjsx
    // │   ├── .foo.TS
    // │   └── README.md
    // ├── c.tsx
    // ├── d.jsx
    // └── ignore
    //     ├── g.d.ts
    //     └── .gitignore

    let root_dir_path = t.path().join("dir.ts");
    create_files(&root_dir_path, &["a.ts", "b.js", "c.tsx", "d.jsx"]);

    let child_dir_path = root_dir_path.join("child");
    create_files(
      &child_dir_path,
      &["e.mjs", "f.mjsx", ".foo.TS", "README.md"],
    );

    let node_modules_dir = child_dir_path.join("node_modules");
    create_files(&node_modules_dir, &["node_modules.js"]);

    let git_dir = child_dir_path.join(".git");
    create_files(&git_dir, &["git.js"]);

    let vendor_dir = child_dir_path.join("vendor");
    create_files(&vendor_dir, &["vendor.js"]);

    let ignore_dir_path = root_dir_path.join("ignore");
    create_files(&ignore_dir_path, &["g.d.ts", ".gitignore"]);

    let file_patterns = FilePatterns {
      base: root_dir_path.to_path_buf(),
      include: None,
      exclude: PathOrPatternSet::new(vec![PathOrPattern::Path(
        ignore_dir_path.to_path_buf(),
      )]),
    };
    let file_collector = FileCollector::new(|e| {
      // exclude dotfiles
      e.path
        .file_name()
        .and_then(|f| f.to_str())
        .map(|f| !f.starts_with('.'))
        .unwrap_or(false)
    });

    let result = file_collector.collect_file_patterns(&RealSys, &file_patterns);
    let expected = [
      "README.md",
      "a.ts",
      "b.js",
      "c.tsx",
      "d.jsx",
      "e.mjs",
      "f.mjsx",
      "git.js",
      "node_modules.js",
      "vendor.js",
    ];
    let mut file_names = result
      .into_iter()
      .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
      .collect::<Vec<_>>();
    file_names.sort();
    assert_eq!(file_names, expected);

    // test ignoring the .git and node_modules folder
    let file_collector = file_collector
      .ignore_git_folder()
      .ignore_node_modules()
      .set_vendor_folder(Some(vendor_dir.to_path_buf()));
    let result = file_collector.collect_file_patterns(&RealSys, &file_patterns);
    let expected = [
      "README.md",
      "a.ts",
      "b.js",
      "c.tsx",
      "d.jsx",
      "e.mjs",
      "f.mjsx",
    ];
    let mut file_names = result
      .into_iter()
      .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
      .collect::<Vec<_>>();
    file_names.sort();
    assert_eq!(file_names, expected);

    // test opting out of ignoring by specifying the dir
    let file_patterns = FilePatterns {
      base: root_dir_path.to_path_buf(),
      include: Some(PathOrPatternSet::new(vec![
        PathOrPattern::Path(root_dir_path.to_path_buf()),
        PathOrPattern::Path(node_modules_dir.to_path_buf()),
      ])),
      exclude: PathOrPatternSet::new(vec![PathOrPattern::Path(
        ignore_dir_path.to_path_buf(),
      )]),
    };
    let result = file_collector.collect_file_patterns(&RealSys, &file_patterns);
    let expected = [
      "README.md",
      "a.ts",
      "b.js",
      "c.tsx",
      "d.jsx",
      "e.mjs",
      "f.mjsx",
      "node_modules.js",
    ];
    let mut file_names = result
      .into_iter()
      .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
      .collect::<Vec<_>>();
    file_names.sort();
    assert_eq!(file_names, expected);
  }

  /// A sys that records every directory the collector reads.
  struct ReadDirLog {
    read: std::cell::RefCell<Vec<PathBuf>>,
  }

  impl BaseFsRead for ReadDirLog {
    fn base_fs_read(&self, path: &Path) -> std::io::Result<Cow<'static, [u8]>> {
      RealSys.base_fs_read(path)
    }
  }

  impl BaseFsMetadata for ReadDirLog {
    type Metadata = <RealSys as BaseFsMetadata>::Metadata;

    fn base_fs_metadata(&self, path: &Path) -> std::io::Result<Self::Metadata> {
      RealSys.base_fs_metadata(path)
    }

    fn base_fs_symlink_metadata(
      &self,
      path: &Path,
    ) -> std::io::Result<Self::Metadata> {
      RealSys.base_fs_symlink_metadata(path)
    }
  }

  impl BaseFsReadDir for ReadDirLog {
    type ReadDirEntry = <RealSys as BaseFsReadDir>::ReadDirEntry;

    fn base_fs_read_dir(
      &self,
      path: &Path,
    ) -> std::io::Result<
      Box<dyn Iterator<Item = std::io::Result<Self::ReadDirEntry>>>,
    > {
      self.read.borrow_mut().push(path.to_path_buf());
      RealSys.base_fs_read_dir(path)
    }
  }

  // denoland/deno#36668: expanding workspace member globs
  // (`packages/*/deno.json`) walked every matched member's whole tree. A
  // directory no include pattern can match under is not read, and the
  // members found are the same: nested `**` globs, negated members and
  // explicit member paths included.
  #[test]
  fn member_globs_do_not_walk_member_trees() {
    let t = tempfile::tempdir().unwrap();
    let root = RealSys.fs_canonicalize(t.path()).unwrap();
    create_files(&root.join("packages/alpha"), &["deno.json"]);
    create_files(&root.join("packages/alpha/src/deep"), &["x.ts"]);
    create_files(&root.join("packages/alpha/.venv/lib/site"), &["y.py"]);
    create_files(&root.join("packages/beta"), &["deno.json"]);
    create_files(&root.join("packages/beta/dist/a/b"), &["c.js"]);
    create_files(&root.join("packages/gamma"), &["README.md"]);
    create_files(&root.join("packages/skip"), &["deno.json"]);
    create_files(&root.join("apps/web/sub"), &["deno.json"]);
    create_files(&root.join("apps/web/sub/inner"), &["deno.json"]);
    create_files(&root.join("tools/cli"), &["deno.json"]);
    create_files(&root.join("tools/cli/lib"), &["z.ts"]);
    create_files(&root.join("unrelated/tree"), &["deno.json"]);

    let include = [
      "packages/*/deno.json",
      "apps/**/deno.json",
      "tools/cli/deno.json",
      "!packages/skip/deno.json",
    ]
    .into_iter()
    .map(|p| PathOrPattern::from_relative(&root, p).unwrap())
    .collect();
    let file_patterns = FilePatterns {
      base: root.clone(),
      include: Some(PathOrPatternSet::new(include)),
      exclude: Default::default(),
    };
    let sys = ReadDirLog {
      read: Default::default(),
    };
    let mut found = FileCollector::new(|_| true)
      .ignore_git_folder()
      .ignore_node_modules()
      .collect_file_patterns(&sys, &file_patterns)
      .into_iter()
      .map(|p| {
        p.strip_prefix(&root)
          .unwrap()
          .to_string_lossy()
          .replace('\\', "/")
      })
      .collect::<Vec<_>>();
    found.sort();
    assert_eq!(
      found,
      [
        "apps/web/sub/deno.json",
        "apps/web/sub/inner/deno.json",
        "packages/alpha/deno.json",
        "packages/beta/deno.json",
        "tools/cli/deno.json",
      ]
    );
    let read = sys.read.into_inner();
    for dir in [
      "packages/alpha/src",
      "packages/alpha/.venv",
      "packages/beta/dist",
      "tools/cli/lib",
      "unrelated",
    ] {
      assert!(
        !read.iter().any(|r| r.starts_with(root.join(dir))),
        "read {dir}: {read:?}"
      );
    }
  }
}
