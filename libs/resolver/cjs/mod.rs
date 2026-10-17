// Copyright 2018-2026 the Deno authors. MIT license.

use deno_maybe_sync::MaybeDashMap;
use deno_media_type::MediaType;
use node_resolver::InNpmPackageChecker;
use node_resolver::PackageJsonResolverRc;
use node_resolver::ResolutionMode;
use node_resolver::errors::PackageJsonLoadError;
use rustc_hash::FxHashSet;
use sys_traits::FsMetadata;
use sys_traits::FsRead;
use url::Url;

use crate::npm::is_synthetic_module;

pub mod analyzer;
#[cfg(feature = "deno_ast")]
mod esm_only;

#[allow(clippy::disallowed_types, reason = "definition")]
pub type CjsTrackerRc<TInNpmPackageChecker, TSys> =
  deno_maybe_sync::MaybeArc<CjsTracker<TInNpmPackageChecker, TSys>>;

/// Keeps track of what module specifiers were resolved as CJS.
///
/// Modules that are `.js`, `.ts`, `.jsx`, and `tsx` are only known to
/// be CJS or ESM after they're loaded based on their contents. So these
/// files will be "maybe CJS" until they're loaded.
#[derive(Debug)]
pub struct CjsTracker<
  TInNpmPackageChecker: InNpmPackageChecker,
  TSys: FsRead + FsMetadata,
> {
  is_cjs_resolver: IsCjsResolver<TInNpmPackageChecker, TSys>,
  known: MaybeDashMap<Url, ResolutionMode>,
  require_modules: FxHashSet<Url>,
}

impl<TInNpmPackageChecker: InNpmPackageChecker, TSys: FsRead + FsMetadata>
  CjsTracker<TInNpmPackageChecker, TSys>
{
  pub fn new(
    in_npm_pkg_checker: TInNpmPackageChecker,
    pkg_json_resolver: PackageJsonResolverRc<TSys>,
    mode: IsCjsResolutionMode,
    require_modules: Vec<Url>,
  ) -> Self {
    Self {
      is_cjs_resolver: IsCjsResolver::new(
        in_npm_pkg_checker,
        pkg_json_resolver,
        mode,
      ),
      known: Default::default(),
      require_modules: require_modules.into_iter().collect(),
    }
  }

  /// Checks whether the file might be treated as CJS, but it's not for sure
  /// yet because the source hasn't been loaded to see whether it contains
  /// imports or exports.
  pub fn is_maybe_cjs(
    &self,
    specifier: &Url,
    media_type: MediaType,
  ) -> Result<bool, PackageJsonLoadError> {
    self.treat_as_cjs_with_is_script(specifier, media_type, None)
  }

  /// Checks whether a file loaded via `require()` should be compiled as
  /// CommonJS before falling back to ESM syntax detection.
  pub fn is_maybe_cjs_from_require(
    &self,
    specifier: &Url,
    media_type: MediaType,
  ) -> Result<bool, PackageJsonLoadError> {
    self
      .is_cjs_resolver
      .check_for_require(specifier, media_type)
      .map(|mode| mode == ResolutionMode::Require)
  }

  /// Mark a file as being known CJS or ESM.
  pub fn set_is_known_script(&self, specifier: &Url, is_script: bool) {
    let new_value = if is_script {
      ResolutionMode::Require
    } else {
      ResolutionMode::Import
    };
    // block to really ensure dashmap is not borrowed while trying to insert
    {
      if let Some(value) = self.known.get(specifier) {
        // you shouldn't be insert a value in here that's
        // already known and is a different value than what
        // was previously determined
        debug_assert_eq!(*value, new_value);
        return;
      }
    }
    self.known.insert(specifier.clone(), new_value);
  }

  /// Gets whether the file is CJS. If true, this is for sure
  /// cjs because `is_script` is provided.
  ///
  /// `is_script` should be `true` when the contents of the file at the
  /// provided specifier are known to be a script and not an ES module.
  pub fn is_cjs_with_known_is_script(
    &self,
    specifier: &Url,
    media_type: MediaType,
    is_script: bool,
  ) -> Result<bool, PackageJsonLoadError> {
    self.treat_as_cjs_with_is_script(specifier, media_type, Some(is_script))
  }

  fn treat_as_cjs_with_is_script(
    &self,
    specifier: &Url,
    media_type: MediaType,
    is_script: Option<bool>,
  ) -> Result<bool, PackageJsonLoadError> {
    let kind = match self
      .get_known_mode_with_is_script(specifier, media_type, is_script)
    {
      Some(kind) => kind,
      None => self
        .is_cjs_resolver
        .check_based_on_pkg_json(specifier, media_type)?,
    };
    Ok(kind == ResolutionMode::Require)
  }

  /// Gets the referrer for the specified module specifier.
  ///
  /// Generally the referrer should already be tracked by calling
  /// `is_cjs_with_known_is_script` before calling this method.
  pub fn get_referrer_kind(&self, specifier: &Url) -> ResolutionMode {
    if specifier.scheme() != "file" {
      return ResolutionMode::Import;
    }
    self
      .get_known_mode(specifier, MediaType::from_specifier(specifier))
      .unwrap_or(ResolutionMode::Import)
  }

  fn get_known_mode(
    &self,
    specifier: &Url,
    media_type: MediaType,
  ) -> Option<ResolutionMode> {
    self.get_known_mode_with_is_script(specifier, media_type, None)
  }

  fn get_known_mode_with_is_script(
    &self,
    specifier: &Url,
    media_type: MediaType,
    is_script: Option<bool>,
  ) -> Option<ResolutionMode> {
    let is_from_require = self.require_modules.contains(specifier);
    self.is_cjs_resolver.get_known_mode_with_is_script(
      specifier,
      media_type,
      is_script,
      is_from_require,
      &self.known,
    )
  }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum IsCjsResolutionMode {
  /// Requires an explicit `"type": "commonjs"` in the package.json.
  ExplicitTypeCommonJs,
  /// Implicitly uses `"type": "commonjs"` if no `"type"` is specified.
  ImplicitTypeCommonJs,
  /// `ImplicitTypeCommonJs` for untyped JavaScript (`.js`, `.jsx`) that
  /// refers to a CommonJS binding (`require`, `module.exports`, `exports`,
  /// `__dirname`, `__filename`), else `ExplicitTypeCommonJs`.
  ImplicitTypeCommonJsUntyped,
  /// Does not respect `"type": "commonjs"` and always treats ambiguous files as ESM.
  #[default]
  Disabled,
}

impl IsCjsResolutionMode {
  /// The default mode for a project: an ambiguous JavaScript file (`.js`,
  /// `.jsx` without `"type"` in its package.json, or with no package.json)
  /// that uses CommonJS (`require`, `module.exports`, `exports`,
  /// `__dirname`, `__filename`) and no ES module syntax (`import`/`export`,
  /// `import.meta`, top-level `await`) is CommonJS, as Node's
  /// `--experimental-detect-module` (on by default since Node 22.7) runs it.
  /// That is the default for a project with a package.json and for a file
  /// run with no config file at all. Every other ambiguous file keeps Deno's
  /// ES module default: TypeScript unless its package.json says
  /// `"type": "commonjs"`, and JavaScript that never touches CommonJS (where
  /// Node's CommonJS would differ from ESM only in sloppy-mode details such
  /// as top-level `this` and `var`), so Deno's config-less scripts and tests
  /// run as before. A project configured only by a deno.json keeps ESM for
  /// every file.
  ///
  /// `opted_out` (`--no-detect-cjs` / `DENO_NO_DETECT_CJS=1`) restores the
  /// previous defaults: only `"type": "commonjs"` makes a file CommonJS, and
  /// only when there is a package.json. `forced` (`--unstable-detect-cjs`,
  /// `"unstable": ["detect-cjs"]`, a node-shim launch) always detects.
  pub fn for_project(
    has_package_json: bool,
    has_deno_json: bool,
    opted_out: bool,
    forced: bool,
  ) -> Self {
    if forced {
      Self::ImplicitTypeCommonJs
    } else if opted_out {
      if has_package_json {
        Self::ExplicitTypeCommonJs
      } else {
        Self::Disabled
      }
    } else if has_package_json || !has_deno_json {
      Self::ImplicitTypeCommonJsUntyped
    } else {
      Self::Disabled
    }
  }
}

/// Resolves whether a module is CJS or ESM.
#[derive(Debug)]
pub struct IsCjsResolver<
  TInNpmPackageChecker: InNpmPackageChecker,
  TSys: FsRead + FsMetadata,
> {
  in_npm_pkg_checker: TInNpmPackageChecker,
  pkg_json_resolver: PackageJsonResolverRc<TSys>,
  mode: IsCjsResolutionMode,
  /// What an ambiguous file's text says about CommonJS, per file.
  text_hints: MaybeDashMap<Url, TextHints>,
}

impl<TInNpmPackageChecker: InNpmPackageChecker, TSys: FsRead + FsMetadata>
  IsCjsResolver<TInNpmPackageChecker, TSys>
{
  pub fn new(
    in_npm_pkg_checker: TInNpmPackageChecker,
    pkg_json_resolver: PackageJsonResolverRc<TSys>,
    mode: IsCjsResolutionMode,
  ) -> Self {
    Self {
      in_npm_pkg_checker,
      pkg_json_resolver,
      mode,
      text_hints: Default::default(),
    }
  }

  /// Gets the resolution mode for a module in the LSP.
  pub fn get_lsp_resolution_mode(
    &self,
    specifier: &Url,
    is_script: Option<bool>,
  ) -> ResolutionMode {
    if specifier.scheme() != "file" {
      return ResolutionMode::Import;
    }
    match MediaType::from_specifier(specifier) {
      MediaType::Mts | MediaType::Mjs | MediaType::Dmts => ResolutionMode::Import,
      MediaType::Cjs | MediaType::Cts | MediaType::Dcts => ResolutionMode::Require,
      MediaType::Dts => {
        // dts files are always determined based on the package.json because
        // they contain imports/exports even when considered CJS
        self
          .check_based_on_pkg_json(specifier, MediaType::Dts)
          .unwrap_or(ResolutionMode::Import)
      }
      MediaType::Wasm |
      MediaType::Json => ResolutionMode::Import,
      MediaType::JavaScript
      | MediaType::Jsx
      | MediaType::TypeScript
      | MediaType::Tsx
      // treat these as unknown
      | MediaType::Css
      | MediaType::Html
      | MediaType::Jsonc
      | MediaType::Json5
      | MediaType::Markdown
      | MediaType::SourceMap
      | MediaType::Sql
      | MediaType::Unknown => {
        match is_script {
          Some(true) => self.check_based_on_pkg_json(specifier, MediaType::from_specifier(specifier)).unwrap_or(ResolutionMode::Import),
          Some(false) | None => ResolutionMode::Import,
        }
      }
    }
  }

  fn get_known_mode_with_is_script(
    &self,
    specifier: &Url,
    media_type: MediaType,
    is_script: Option<bool>,
    is_from_require: bool,
    known_cache: &MaybeDashMap<Url, ResolutionMode>,
  ) -> Option<ResolutionMode> {
    if specifier.scheme() != "file" {
      return Some(ResolutionMode::Import);
    }

    match media_type {
      MediaType::Mts | MediaType::Mjs | MediaType::Dmts => Some(ResolutionMode::Import),
      MediaType::Cjs | MediaType::Cts | MediaType::Dcts => Some(ResolutionMode::Require),
      MediaType::Dts => {
        // dts files are always determined based on the package.json because
        // they contain imports/exports even when considered CJS
        if let Some(value) = known_cache.get(specifier).map(|v| *v) {
          Some(value)
        } else {
          let value =
            self.check_based_on_pkg_json(specifier, media_type).ok();
          if let Some(value) = value {
            known_cache.insert(specifier.clone(), value);
          }
          Some(value.unwrap_or(ResolutionMode::Import))
        }
      }
      MediaType::Wasm |
      MediaType::Json => Some(ResolutionMode::Import),
      MediaType::JavaScript
      | MediaType::Jsx
      | MediaType::TypeScript
      | MediaType::Tsx
      // treat these as unknown
      | MediaType::Css
      | MediaType::Html
      | MediaType::Jsonc
      | MediaType::Json5
      | MediaType::Markdown
      | MediaType::SourceMap
      | MediaType::Sql
      | MediaType::Unknown => {
        if is_from_require {
          return Some(ResolutionMode::Require);
        }

        if let Some(value) = known_cache.get(specifier).map(|v| *v) {
          if value == ResolutionMode::Require && is_script == Some(false) {
            // we now know this is actually esm
            known_cache.insert(specifier.clone(), ResolutionMode::Import);
            Some(ResolutionMode::Import)
          } else {
            Some(value)
          }
        } else if is_script == Some(false) {
          // we know this is esm
          known_cache.insert(specifier.clone(), ResolutionMode::Import);
          Some(ResolutionMode::Import)
        } else {
          None
        }
      }
    }
  }

  /// Whether an ambiguous file of `media_type` (its content's, so
  /// `--ext=ts` on a `.js` file is TypeScript) with no `"type"` is CommonJS
  /// (unless it turns out to use ES module syntax).
  ///
  /// Either way a file that only an ES module can run (a top-level `const
  /// require`, an `await` outside functions; see `esm_only`) is ESM, as Node
  /// retries it as one.
  fn is_implicit_for(&self, specifier: &Url, media_type: MediaType) -> bool {
    match self.mode {
      IsCjsResolutionMode::ImplicitTypeCommonJs => {
        !self.text_hints(specifier, media_type, false).esm_only
      }
      IsCjsResolutionMode::ImplicitTypeCommonJsUntyped => {
        if !matches!(media_type, MediaType::JavaScript | MediaType::Jsx) {
          return false;
        }
        let hints = self.text_hints(specifier, media_type, true);
        hints.uses_commonjs && !hints.esm_only
      }
      IsCjsResolutionMode::ExplicitTypeCommonJs
      | IsCjsResolutionMode::Disabled => false,
    }
  }

  /// A cheap textual check (as `deno eval` does) for a CommonJS binding
  /// (when `need_commonjs`), then, for a file that may be CommonJS, a parse
  /// for what makes it ESM-only. Only consulted for a file that parsed with no
  /// ES module syntax, so a match in a comment or string at worst runs a
  /// script as CommonJS.
  fn text_hints(
    &self,
    specifier: &Url,
    media_type: MediaType,
    need_commonjs: bool,
  ) -> TextHints {
    if let Some(value) = self.text_hints.get(specifier).map(|v| *v) {
      return value;
    }
    let text =
      deno_path_util::url_to_file_path(specifier)
        .ok()
        .and_then(|path| {
          self
            .pkg_json_resolver
            .sys()
            .fs_read_to_string_lossy(&path)
            .ok()
        });
    let value = match text {
      Some(text) => {
        let uses_commonjs = text_uses_commonjs(&text);
        let esm_only = (uses_commonjs || !need_commonjs)
          && text_is_esm_only(specifier, media_type, &text);
        TextHints {
          uses_commonjs,
          esm_only,
        }
      }
      None => TextHints::default(),
    };
    self.text_hints.insert(specifier.clone(), value);
    value
  }

  /// Whether Node's `package.json`-based CJS/ESM rules apply to this file.
  ///
  /// `in_npm_pkg_checker` only recognizes the npm packages that belong to the
  /// *current* resolution root: the global npm cache dir, or the workspace's
  /// local `node_modules` dir. A file can live in a `node_modules` tree that
  /// belongs to neither — most notably `deno x`, which runs an installed bin
  /// out of `deno_x_cache/.../node_modules/` in a subprocess that resolves
  /// against the user's cwd config. Such a file is an npm package file no
  /// matter which config the process happened to load, and whether it is CJS
  /// must not depend on that, so fall back to a `node_modules` path check.
  ///
  /// Synthetic modules (`deno eval`, piped stdin) are excluded: they have no
  /// file on disk and are only anchored at the cwd so that their imports
  /// resolve, so a cwd inside `node_modules` must not make them npm files.
  fn node_cjs_rules_apply(&self, specifier: &Url) -> bool {
    if self.in_npm_pkg_checker.in_npm_package(specifier) {
      return true;
    }
    specifier.scheme() == "file"
      && specifier.path().contains("/node_modules/")
      && !is_synthetic_module(specifier)
  }

  fn check_based_on_pkg_json(
    &self,
    specifier: &Url,
    media_type: MediaType,
  ) -> Result<ResolutionMode, PackageJsonLoadError> {
    if self.node_cjs_rules_apply(specifier) {
      let Ok(path) = deno_path_util::url_to_file_path(specifier) else {
        return Ok(ResolutionMode::Require);
      };
      if let Some(pkg_json) =
        self.pkg_json_resolver.get_package_scope_config(&path)?
      {
        let is_file_location_cjs = pkg_json.typ != "module";
        Ok(if is_file_location_cjs || path.extension().is_none() {
          ResolutionMode::Require
        } else {
          ResolutionMode::Import
        })
      } else {
        Ok(ResolutionMode::Require)
      }
    } else if self.mode != IsCjsResolutionMode::Disabled {
      let Ok(path) = deno_path_util::url_to_file_path(specifier) else {
        return Ok(ResolutionMode::Import);
      };
      if let Some(pkg_json) =
        self.pkg_json_resolver.get_package_scope_config(&path)?
      {
        let is_cjs_type = pkg_json.typ == "commonjs"
          || self.is_implicit_for(specifier, media_type)
            && pkg_json.typ == "none";
        Ok(if is_cjs_type {
          ResolutionMode::Require
        } else {
          ResolutionMode::Import
        })
      } else if self.is_implicit_for(specifier, media_type) {
        Ok(ResolutionMode::Require)
      } else {
        Ok(ResolutionMode::Import)
      }
    } else {
      Ok(ResolutionMode::Import)
    }
  }

  fn check_for_require(
    &self,
    specifier: &Url,
    media_type: MediaType,
  ) -> Result<ResolutionMode, PackageJsonLoadError> {
    if specifier.scheme() != "file" {
      return Ok(ResolutionMode::Import);
    }

    match media_type {
      MediaType::Mts | MediaType::Mjs | MediaType::Dmts => {
        Ok(ResolutionMode::Import)
      }
      MediaType::Cjs | MediaType::Cts | MediaType::Dcts => {
        Ok(ResolutionMode::Require)
      }
      MediaType::Wasm | MediaType::Json => Ok(ResolutionMode::Import),
      MediaType::Dts
      | MediaType::JavaScript
      | MediaType::Jsx
      | MediaType::TypeScript
      | MediaType::Tsx
      | MediaType::Css
      | MediaType::Html
      | MediaType::Jsonc
      | MediaType::Json5
      | MediaType::Markdown
      | MediaType::SourceMap
      | MediaType::Sql
      | MediaType::Unknown => {
        let Ok(path) = deno_path_util::url_to_file_path(specifier) else {
          return Ok(ResolutionMode::Import);
        };
        let Some(pkg_json) =
          self.pkg_json_resolver.get_package_scope_config(&path)?
        else {
          return Ok(ResolutionMode::Require);
        };
        Ok(if pkg_json.typ == "module" && path.extension().is_some() {
          ResolutionMode::Import
        } else {
          ResolutionMode::Require
        })
      }
    }
  }
}

/// What an ambiguous file's text says about CommonJS.
#[derive(Debug, Default, Clone, Copy)]
struct TextHints {
  /// It refers to one of CommonJS's module bindings.
  uses_commonjs: bool,
  /// Compiled as CommonJS it would throw an error only CommonJS throws, so
  /// Node runs it as an ES module.
  esm_only: bool,
}

#[cfg(feature = "deno_ast")]
fn text_is_esm_only(
  specifier: &Url,
  media_type: MediaType,
  text: &str,
) -> bool {
  esm_only::is_esm_only(specifier, media_type, text)
}

#[cfg(not(feature = "deno_ast"))]
fn text_is_esm_only(
  _specifier: &Url,
  _media_type: MediaType,
  _text: &str,
) -> bool {
  false
}

/// Whether JavaScript source refers to one of CommonJS's module bindings.
fn text_uses_commonjs(text: &str) -> bool {
  const NEEDLES: &[&str] = &[
    "require(",
    "require.",
    "module.exports",
    "__dirname",
    "__filename",
  ];
  NEEDLES.iter().any(|needle| text.contains(needle))
    || contains_word(text, "exports")
}

/// Whether `word` occurs in `text` as a whole identifier (`exports.a`,
/// `Object.defineProperty(exports, ...)`, but not `reexports`).
fn contains_word(text: &str, word: &str) -> bool {
  let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
  text.match_indices(word).any(|(i, _)| {
    let before = text[..i].chars().next_back();
    let after = text[i + word.len()..].chars().next();
    !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
  })
}

#[cfg(test)]
mod uses_commonjs_tests {
  use super::text_uses_commonjs;

  #[test]
  fn detects_commonjs_bindings() {
    assert!(text_uses_commonjs("const fs = require(\"fs\");"));
    assert!(text_uses_commonjs("module.exports = 1;"));
    assert!(text_uses_commonjs("exports.a = 1;"));
    assert!(text_uses_commonjs("console.log(__dirname);"));
    assert!(text_uses_commonjs("require.resolve(\"x\");"));
    assert!(text_uses_commonjs(
      "Object.defineProperty(exports, \"__esModule\", { value: true });"
    ));
    assert!(!text_uses_commonjs("const reexports = 1;"));
    assert!(!text_uses_commonjs("console.log(typeof this);"));
    assert!(!text_uses_commonjs("Deno.bench(() => {});"));
  }
}
