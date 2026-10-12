// Copyright 2018-2026 the Deno authors. MIT license.

//! JSX in `.js` files. Jest's default transform (babel-jest) and the other
//! JSX-capable ones (`@swc/jest`, esbuild-jest, ...) compile JSX in `.js`
//! files, so Jest projects commonly have it there; Vite only does when its
//! `esbuild.loader` (or `oxc.lang`) is `"jsx"` for the file. Deno parses a
//! `.js` module as plain JavaScript, so the file fetcher's
//! [`LocalContentTypeOverride`] gives such user modules the JSX content type
//! (and media type) instead.

use std::path::PathBuf;

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::url::Url;
use deno_resolver::file_fetcher::LocalContentTypeOverride;
use regex::Regex;

use super::config::LoadedAliasFind;
use super::config::LoadedJsxInJs;
use super::glob;
use super::transform::is_user_module;

/// The `.js` files of one project that are parsed as JSX.
#[derive(Debug)]
struct Rule {
  root: PathBuf,
  /// All selected files, or only those that are not plain JavaScript.
  always: bool,
  include: Vec<Filter>,
  exclude: Vec<Filter>,
}

/// A Vite filter entry.
#[derive(Debug)]
enum Filter {
  Glob(glob::Glob),
  Regex(Regex),
}

impl Filter {
  fn new(find: &LoadedAliasFind) -> Result<Self, AnyError> {
    Ok(match find {
      LoadedAliasFind::String(pattern) => Self::Glob(
        glob::Glob::new(pattern)
          .map_err(|err| anyhow!("Invalid glob \"{pattern}\": {err}"))?,
      ),
      LoadedAliasFind::Regex { regex, flags } => {
        Self::Regex(super::js_regex_with_flags(regex, flags)?)
      }
    })
  }

  fn matches(&self, path: &str) -> bool {
    match self {
      Self::Glob(glob) => glob.is_match(path),
      Self::Regex(regex) => regex.is_match(path),
    }
  }
}

/// Parses the selected `.js` user modules of a run's projects as JSX.
#[derive(Debug, Default)]
pub struct JsxInJs {
  rules: Vec<Rule>,
}

impl JsxInJs {
  /// Adds a project's rule (the first rule whose root holds a file decides).
  pub fn add(&mut self, loaded: &LoadedJsxInJs) -> Result<(), AnyError> {
    let compile = |filters: &[LoadedAliasFind]| {
      filters
        .iter()
        .map(Filter::new)
        .collect::<Result<Vec<_>, _>>()
    };
    self.rules.push(Rule {
      root: PathBuf::from(&loaded.root),
      always: loaded.mode == "always",
      include: compile(&loaded.include)?,
      exclude: compile(&loaded.exclude)?,
    });
    Ok(())
  }

  pub fn is_empty(&self) -> bool {
    self.rules.is_empty()
  }

  /// Whether the `.js` user module `specifier` with `text` is parsed as JSX.
  fn parses_as_jsx(&self, specifier: &Url, text: &str) -> bool {
    if !is_user_module(specifier) || !specifier.path().ends_with(".js") {
      return false;
    }
    let Ok(path) = deno_path_util::url_to_file_path(specifier) else {
      return false;
    };
    let Some(rule) = self.rules.iter().find(|r| path.starts_with(&r.root))
    else {
      return false;
    };
    let absolute = path.to_string_lossy().replace('\\', "/");
    if !rule.include.is_empty()
      && !rule.include.iter().any(|f| f.matches(&absolute))
    {
      return false;
    }
    if rule.exclude.iter().any(|f| f.matches(&absolute)) {
      return false;
    }
    rule.always || only_parses_as_jsx(specifier, text)
  }
}

impl LocalContentTypeOverride for JsxInJs {
  fn content_type(&self, specifier: &Url, source: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(source).ok()?;
    self
      .parses_as_jsx(specifier, text)
      .then(|| "text/jsx".to_string())
  }
}

/// Whether `text` fails to parse as plain JavaScript but parses as JSX.
fn only_parses_as_jsx(specifier: &Url, text: &str) -> bool {
  let parses = |media_type| {
    deno_ast::parse_module(ParseParams {
      specifier: specifier.clone(),
      text: text.into(),
      media_type,
      capture_tokens: false,
      scope_analysis: false,
      maybe_syntax: None,
    })
    .is_ok_and(|parsed| parsed.diagnostics().is_empty())
  };
  !parses(MediaType::JavaScript) && parses(MediaType::Jsx)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rules(loaded: &[LoadedJsxInJs]) -> JsxInJs {
    let mut jsx = JsxInJs::default();
    for loaded in loaded {
      jsx.add(loaded).unwrap();
    }
    jsx
  }

  fn loaded(mode: &str, include: Vec<LoadedAliasFind>) -> LoadedJsxInJs {
    LoadedJsxInJs {
      root: "/project".to_string(),
      mode: mode.to_string(),
      include,
      exclude: vec![LoadedAliasFind::String(
        "/project/src/legacy/**".to_string(),
      )],
    }
  }

  fn url(path: &str) -> Url {
    Url::parse(&format!("file://{path}")).unwrap()
  }

  const JSX: &str = "export const el = <div id=\"a\">hi</div>;\n";
  const PLAIN: &str = "export const lt = (a, b) => a < b;\n";

  #[test]
  fn always() {
    let jsx = rules(&[loaded("always", Vec::new())]);
    assert!(jsx.parses_as_jsx(&url("/project/src/a.js"), JSX));
    assert!(jsx.parses_as_jsx(&url("/project/src/a.js"), PLAIN));
    // only `.js` user modules under the root, minus the excluded ones
    assert!(!jsx.parses_as_jsx(&url("/project/src/a.mjs"), JSX));
    assert!(!jsx.parses_as_jsx(&url("/elsewhere/a.js"), JSX));
    assert!(!jsx.parses_as_jsx(&url("/project/node_modules/p/a.js"), JSX));
    assert!(!jsx.parses_as_jsx(&url("/project/src/legacy/a.js"), JSX));
  }

  #[test]
  fn fallback() {
    let jsx = rules(&[loaded("fallback", Vec::new())]);
    assert!(jsx.parses_as_jsx(&url("/project/src/a.js"), JSX));
    assert!(!jsx.parses_as_jsx(&url("/project/src/a.js"), PLAIN));
    // neither JavaScript nor JSX: left to report the JavaScript error
    assert!(!jsx.parses_as_jsx(&url("/project/src/a.js"), "export const ="));
  }

  #[test]
  fn vite_include() {
    let jsx = rules(&[loaded(
      "always",
      vec![LoadedAliasFind::Regex {
        regex: r"src/.*\.jsx?$".to_string(),
        flags: String::new(),
      }],
    )]);
    assert!(jsx.parses_as_jsx(&url("/project/src/a.js"), PLAIN));
    assert!(!jsx.parses_as_jsx(&url("/project/test/a.js"), JSX));
    // Vite's default include has no `.js`
    let jsx = rules(&[loaded(
      "always",
      vec![LoadedAliasFind::Regex {
        regex: r"\.(m?ts|[jt]sx)$".to_string(),
        flags: String::new(),
      }],
    )]);
    assert!(!jsx.parses_as_jsx(&url("/project/src/a.js"), JSX));
  }
}
