// Copyright 2018-2026 the Deno authors. MIT license.

//! The module registry side of Vitest's module mocking under `deno test`.
//!
//! Every worker's module loader owns a [`VitestMocks`], shared with the
//! worker's ops. The Vitest runtime (`js/mocker.js`) registers each mocked
//! module by its resolved URL; from then on the loader resolves any import
//! of that URL from user code to a `deno-vitest:///mock?id=<n>` module whose
//! source the runtime generates (from the factory's exports, an automocked
//! module or a `__mocks__` file). Resolving to the URL the specifier finally
//! resolves to means `./db`, `@/db` and `../db.ts` all hit the same mock, for
//! the test file and every module it (transitively) imports. Like Vitest,
//! imports from dependencies in `node_modules` (external to Vitest) are never
//! redirected.
//!
//! `vi.resetModules()` bumps a generation: user modules resolved afterwards
//! get a `__vitest_gen=<n>` query, so they are fresh module instances (the
//! loader serves them from the module without the query).
//!
//! Each test file runs in its own worker, so each test file has its own
//! registry, like Vitest isolates files by default.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Waker;

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::swc::ast::ExportSpecifier;
use deno_ast::swc::ast::ImportSpecifier;
use deno_ast::swc::ast::ModuleDecl;
use deno_ast::swc::ast::ModuleExportName;
use deno_core::OpState;
use deno_core::futures::FutureExt;
use deno_core::futures::channel::oneshot;
use deno_core::futures::future::LocalBoxFuture;
use deno_core::op2;
use deno_core::url::Url;
use deno_error::JsErrorBox;

use super::SCHEME;
use super::transform::SELF_SPECIFIER;
use super::transform::is_user_module;

/// The query parameter that makes a user module a fresh instance after
/// `vi.resetModules()`.
const GENERATION_PARAM: &str = "__vitest_gen";
const ACTUAL_PREFIX: &str = "deno-vitest:///actual?url=";
const MOCK_PATH: &str = "/mock";
const VIRTUAL_PATH_PREFIX: &str = "/virtual/";

/// Resolves a specifier from a referrer like an import does (without mocks).
pub type ResolveFn = Rc<dyn Fn(&str, &str) -> Result<Url, JsErrorBox>>;
/// The emitted code of a module of the worker's graph.
pub type SourceFn = Rc<dyn Fn(&Url) -> Option<Arc<str>>>;

/// The names a module imports by name from one specifier (a namespace
/// import, `export *` or a dynamic import gets every name the mock has).
type ImportedNames = BTreeSet<String>;

#[derive(Default)]
pub struct VitestMocks {
  /// Set once the worker's Vitest runtime installed its mocker.
  enabled: Cell<bool>,
  /// Mocked modules: resolved URL -> mock id.
  mocks: RefCell<HashMap<Url, u32>>,
  generation: Cell<u32>,
  resolve: RefCell<Option<ResolveFn>>,
  source: RefCell<Option<SourceFn>>,
  /// Per referrer: the names imported from each specifier.
  imports: RefCell<HashMap<Url, Rc<HashMap<String, ImportedNames>>>>,
  next_request: Cell<u32>,
  pending: RefCell<VecDeque<(u32, String)>>,
  waker: RefCell<Option<Waker>>,
  senders: RefCell<HashMap<u32, oneshot::Sender<Result<String, String>>>>,
  /// The export names of the mocks whose exports are known.
  keys: RefCell<HashMap<u32, Rc<Vec<String>>>>,
}

impl std::fmt::Debug for VitestMocks {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("VitestMocks")
      .field("enabled", &self.enabled.get())
      .field("mocks", &self.mocks.borrow())
      .field("generation", &self.generation.get())
      .finish()
  }
}

impl VitestMocks {
  /// Installs the loader's resolution and graph lookup.
  pub fn set_loader(&self, resolve: ResolveFn, source: SourceFn) {
    *self.resolve.borrow_mut() = Some(resolve);
    *self.source.borrow_mut() = Some(source);
  }

  /// Whether module loads may wait for the worker's JavaScript (so the
  /// event loop must run while modules load).
  pub fn is_enabled(&self) -> bool {
    self.enabled.get()
  }

  /// Resolves `specifier` with `resolve` (the loader's resolution), applying
  /// the mocks, the module generation and the transform's self import.
  pub fn resolve<E>(
    &self,
    specifier: &str,
    referrer: &str,
    resolve: impl FnOnce(&str, &str) -> Result<Url, E>,
  ) -> Result<Url, E> {
    if specifier == SELF_SPECIFIER
      && let Ok(referrer) = Url::parse(referrer)
    {
      return Ok(referrer);
    }
    if !self.enabled.get() {
      return resolve(specifier, referrer);
    }
    if let Some(actual) = specifier.strip_prefix(ACTUAL_PREFIX)
      && let Ok(url) = Url::parse(&percent_decode(actual))
    {
      return Ok(self.with_generation(url));
    }
    let referrer_url = Url::parse(referrer).ok();
    // A fresh module instance resolves its imports like its original.
    let base_referrer = referrer_url
      .as_ref()
      .filter(|r| has_generation(r))
      .map(strip_generation);
    let resolved = resolve(
      specifier,
      base_referrer
        .as_ref()
        .map(|r| r.as_str())
        .unwrap_or(referrer),
    )?;
    Ok(self.redirect(specifier, referrer_url.as_ref(), resolved))
  }

  fn redirect(
    &self,
    specifier: &str,
    referrer: Option<&Url>,
    resolved: Url,
  ) -> Url {
    let Some(referrer) = referrer else {
      return resolved;
    };
    // Dependencies are external to Vitest: their imports are never mocked
    // (nor are Vitest's own modules'). Vite's virtual asset modules can be.
    if !is_user_module(referrer)
      || (resolved.scheme() == SCHEME
        && !resolved.path().starts_with(VIRTUAL_PATH_PREFIX))
    {
      return resolved;
    }
    let base = strip_generation(&resolved);
    if base == strip_generation(referrer) {
      return resolved;
    }
    let mock_id = {
      let mocks = self.mocks.borrow();
      if mocks.is_empty() {
        None
      } else {
        mocks.get(&base).copied().or_else(|| {
          // a dynamic import of a package keeps its `npm:` specifier
          if base.scheme() == "npm" {
            let file = self.resolve_unmocked(specifier, referrer.as_str())?;
            mocks.get(&file).copied()
          } else {
            None
          }
        })
      }
    };
    match mock_id {
      Some(id) => mock_url(id, self.imported_names(referrer, specifier)),
      None => self.with_generation(resolved),
    }
  }

  fn resolve_unmocked(&self, specifier: &str, referrer: &str) -> Option<Url> {
    let resolve = self.resolve.borrow().clone()?;
    resolve(specifier, referrer).ok()
  }

  fn with_generation(&self, mut url: Url) -> Url {
    let generation = self.generation.get();
    if generation == 0 || !is_user_module(&url) || has_generation(&url) {
      return url;
    }
    url
      .query_pairs_mut()
      .append_pair(GENERATION_PARAM, &generation.to_string());
    url
  }

  /// The names `referrer` imports from `specifier`.
  fn imported_names(&self, referrer: &Url, specifier: &str) -> ImportedNames {
    let referrer = strip_generation(referrer);
    let cached = self.imports.borrow().get(&referrer).cloned();
    let imports = match cached {
      Some(imports) => imports,
      None => {
        let source = self.source.borrow().clone();
        let imports = Rc::new(
          source
            .and_then(|source| source(&referrer))
            .map(|text| parse_imported_names(&referrer, text))
            .unwrap_or_default(),
        );
        self
          .imports
          .borrow_mut()
          .insert(referrer.clone(), imports.clone());
        imports
      }
    };
    imports.get(specifier).cloned().unwrap_or_default()
  }

  /// The source of a `deno-vitest:///mock` module: generated here when the
  /// mock's exports are known (its factory ran), else by the worker's Vitest
  /// runtime, which may run the factory first.
  pub fn load_mock(&self, specifier: &Url) -> Option<MockSource> {
    if specifier.path() != MOCK_PATH {
      return None;
    }
    let params = specifier.query_pairs().collect::<Vec<_>>();
    let id = params
      .iter()
      .find(|(k, _)| k == "id")
      .and_then(|(_, v)| v.parse::<u32>().ok());
    if let Some(keys) = id.and_then(|id| self.keys.borrow().get(&id).cloned()) {
      let mut names = params
        .iter()
        .filter(|(k, _)| k == "name")
        .map(|(_, v)| v.to_string())
        .collect::<Vec<_>>();
      for key in keys.iter() {
        if !names.contains(key) {
          names.push(key.clone());
        }
      }
      return Some(MockSource::Ready(mock_module_source(id.unwrap(), &names)));
    }
    let request = self.next_request.get();
    self.next_request.set(request + 1);
    let (sender, receiver) = oneshot::channel();
    self.senders.borrow_mut().insert(request, sender);
    self
      .pending
      .borrow_mut()
      .push_back((request, specifier.to_string()));
    if let Some(waker) = self.waker.borrow_mut().take() {
      waker.wake();
    }
    let specifier = specifier.clone();
    Some(MockSource::Pending(
      async move {
        match receiver.await {
          Ok(Ok(source)) => Ok(source),
          Ok(Err(message)) => Err(JsErrorBox::generic(message)),
          Err(_) => Err(JsErrorBox::generic(format!(
            "The Vitest mock module {specifier} was not provided"
          ))),
        }
      }
      .boxed_local(),
    ))
  }
}

/// The source of a mock module, see [`VitestMocks::load_mock`].
pub enum MockSource {
  Ready(String),
  Pending(LocalBoxFuture<'static, Result<String, JsErrorBox>>),
}

/// A mock module exporting `names` of mock `id`'s exports, each a binding
/// the mocker can replace (`js/mocker.js` generates the same).
fn mock_module_source(id: u32, names: &[String]) -> String {
  let mut code = String::from(
    "const __vitest_mocker__ = globalThis[Symbol.for(\"deno.vitest.mocker\")];\n",
  );
  code.push_str(&format!(
    "const __vitest_module__ = __vitest_mocker__.peek({id});\n"
  ));
  let mut cases = String::new();
  for (index, name) in names.iter().enumerate() {
    let name = deno_core::serde_json::to_string(name).unwrap();
    code.push_str(&format!(
      "let __{index} = __vitest_module__[{name}];\nexport {{ __{index} as {name} }};\n"
    ));
    cases.push_str(&format!("case {name}: __{index} = value; return true; "));
  }
  code.push_str(&format!(
    "import * as __vitest_self__ from \"{SELF_SPECIFIER}\";\n__vitest_mocker__.bind({id}, __vitest_self__, (name, value) => {{ switch (name) {{ {cases}}} return false; }});\n"
  ));
  code
}

fn mock_url(id: u32, names: ImportedNames) -> Url {
  let mut url = Url::parse(&format!("{SCHEME}://{MOCK_PATH}")).unwrap();
  {
    let mut query = url.query_pairs_mut();
    query.append_pair("id", &id.to_string());
    for name in names {
      query.append_pair("name", &name);
    }
  }
  url
}

fn has_generation(url: &Url) -> bool {
  url.query().is_some() && url.query_pairs().any(|(k, _)| k == GENERATION_PARAM)
}

/// The URL without the generation query parameter.
pub fn strip_generation(url: &Url) -> Url {
  if !has_generation(url) {
    return url.clone();
  }
  let pairs = url
    .query_pairs()
    .filter(|(k, _)| k != GENERATION_PARAM)
    .map(|(k, v)| (k.into_owned(), v.into_owned()))
    .collect::<Vec<_>>();
  let mut url = url.clone();
  if pairs.is_empty() {
    url.set_query(None);
  } else {
    url.query_pairs_mut().clear().extend_pairs(pairs);
  }
  url
}

/// The module a fresh instance (after `vi.resetModules()`) is served from.
pub fn generation_base(url: &Url) -> Option<Url> {
  if url.scheme() == "file" && has_generation(url) {
    Some(strip_generation(url))
  } else {
    None
  }
}

fn percent_decode(text: &str) -> String {
  deno_core::url::form_urlencoded::parse(format!("x={text}").as_bytes())
    .next()
    .map(|(_, v)| v.into_owned())
    .unwrap_or_default()
}

/// The names a module statically imports, by specifier (from its emitted
/// code, which has the imports the JSX transform adds).
fn parse_imported_names(
  specifier: &Url,
  text: Arc<str>,
) -> HashMap<String, ImportedNames> {
  let mut result = HashMap::<String, ImportedNames>::new();
  let Ok(parsed) = deno_ast::parse_module(ParseParams {
    specifier: specifier.clone(),
    text,
    media_type: MediaType::JavaScript,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  }) else {
    return result;
  };
  let mut add = |src: String, name: Option<String>| {
    let names = result.entry(src).or_default();
    if let Some(name) = name {
      names.insert(name);
    }
  };
  let export_name = |name: &ModuleExportName| match name {
    ModuleExportName::Ident(ident) => ident.sym.to_string(),
    ModuleExportName::Str(s) => s.value.to_string_lossy().into_owned(),
  };
  for item in parsed.program_ref().body() {
    let deno_ast::ModuleItemRef::ModuleDecl(decl) = item else {
      continue;
    };
    match decl {
      ModuleDecl::Import(import) if !import.type_only => {
        let src = import.src.value.to_string_lossy().into_owned();
        if import.specifiers.is_empty() {
          add(src, None);
          continue;
        }
        for spec in &import.specifiers {
          match spec {
            ImportSpecifier::Named(named) if !named.is_type_only => add(
              src.clone(),
              Some(
                named
                  .imported
                  .as_ref()
                  .map(export_name)
                  .unwrap_or_else(|| named.local.sym.to_string()),
              ),
            ),
            ImportSpecifier::Named(_) => {}
            ImportSpecifier::Default(_) => {
              add(src.clone(), Some("default".to_string()))
            }
            ImportSpecifier::Namespace(_) => add(src.clone(), None),
          }
        }
      }
      ModuleDecl::ExportNamed(named) if !named.type_only => {
        let Some(src) = &named.src else {
          continue;
        };
        let src = src.value.to_string_lossy().into_owned();
        for spec in &named.specifiers {
          match spec {
            ExportSpecifier::Named(spec) if !spec.is_type_only => {
              add(src.clone(), Some(export_name(&spec.orig)))
            }
            ExportSpecifier::Named(_) => {}
            ExportSpecifier::Namespace(_) | ExportSpecifier::Default(_) => {
              add(src.clone(), None)
            }
          }
        }
      }
      ModuleDecl::ExportAll(all) if !all.type_only => {
        add(all.src.value.to_string_lossy().into_owned(), None)
      }
      _ => {}
    }
  }
  result
}

fn mocks(state: &OpState) -> Result<Rc<VitestMocks>, JsErrorBox> {
  state
    .try_borrow::<Rc<VitestMocks>>()
    .cloned()
    .ok_or_else(|| JsErrorBox::generic("Vitest module mocking is unavailable"))
}

/// Enables mocking for the worker (called once by the Vitest runtime).
#[op2(fast)]
pub fn op_vitest_mocks_enable(state: &mut OpState) -> Result<(), JsErrorBox> {
  mocks(state)?.enabled.set(true);
  Ok(())
}

/// Resolves `specifier` from `importer` like an import does, without the
/// mocks. Returns `null` when it does not resolve.
#[op2]
#[string]
pub fn op_vitest_mock_resolve(
  state: &mut OpState,
  #[string] specifier: &str,
  #[string] importer: &str,
) -> Result<Option<String>, JsErrorBox> {
  let mocks = mocks(state)?;
  let importer = Url::parse(importer)
    .map(|url| strip_generation(&url).to_string())
    .unwrap_or_else(|_| importer.to_string());
  Ok(
    mocks
      .resolve_unmocked(specifier, &importer)
      .map(|url| strip_generation(&url).to_string()),
  )
}

/// Registers mock `id` for the module `url` (`id` 0 removes the mock).
#[op2(fast)]
pub fn op_vitest_mock_set(
  state: &mut OpState,
  #[string] url: &str,
  id: u32,
) -> Result<(), JsErrorBox> {
  let mocks = mocks(state)?;
  let url = Url::parse(url).map_err(JsErrorBox::from_err)?;
  if id == 0 {
    mocks.mocks.borrow_mut().remove(&url);
  } else {
    mocks.mocks.borrow_mut().insert(url, id);
  }
  Ok(())
}

/// Records the export names of mock `id` once its exports are known, so its
/// modules load without waiting for the worker's JavaScript.
#[op2]
pub fn op_vitest_mock_keys(
  state: &mut OpState,
  id: u32,
  #[serde] keys: Vec<String>,
) -> Result<(), JsErrorBox> {
  mocks(state)?.keys.borrow_mut().insert(id, Rc::new(keys));
  Ok(())
}

/// Starts a new module generation (`vi.resetModules()`).
#[op2(fast)]
pub fn op_vitest_mocks_set_generation(
  state: &mut OpState,
  generation: u32,
) -> Result<(), JsErrorBox> {
  mocks(state)?.generation.set(generation);
  Ok(())
}

/// Waits for the next mock module the loader needs the source of.
#[op2]
#[serde]
pub async fn op_vitest_mocks_poll(
  state: Rc<RefCell<OpState>>,
) -> Result<(u32, String), JsErrorBox> {
  let mocks = mocks(&state.borrow())?;
  std::future::poll_fn(|cx| {
    if let Some(request) = mocks.pending.borrow_mut().pop_front() {
      return std::task::Poll::Ready(Ok(request));
    }
    *mocks.waker.borrow_mut() = Some(cx.waker().clone());
    std::task::Poll::Pending
  })
  .await
}

/// Provides the source of a requested mock module (or the error loading it
/// fails with).
#[op2]
pub fn op_vitest_mocks_respond(
  state: &mut OpState,
  id: u32,
  #[string] source: Option<String>,
  #[string] error: Option<String>,
) -> Result<(), JsErrorBox> {
  let mocks = mocks(state)?;
  if let Some(sender) = mocks.senders.borrow_mut().remove(&id) {
    let _ = sender.send(match (source, error) {
      (Some(source), None) => Ok(source),
      (_, error) => Err(error.unwrap_or_else(|| "Unknown error".to_string())),
    });
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
  }

  #[test]
  fn generations() {
    let a = url("file:///p/a.ts?__vitest_gen=2");
    assert!(has_generation(&a));
    assert_eq!(strip_generation(&a), url("file:///p/a.ts"));
    assert_eq!(
      strip_generation(&url("file:///p/a.ts?x=1&__vitest_gen=2")),
      url("file:///p/a.ts?x=1")
    );
    assert_eq!(generation_base(&a), Some(url("file:///p/a.ts")));
    assert_eq!(generation_base(&url("file:///p/a.ts")), None);
    let mocks = VitestMocks::default();
    mocks.generation.set(3);
    assert_eq!(
      mocks.with_generation(url("file:///p/a.ts")),
      url("file:///p/a.ts?__vitest_gen=3")
    );
    // dependencies and other schemes keep their instance
    assert_eq!(
      mocks.with_generation(url("file:///p/node_modules/x/i.js")),
      url("file:///p/node_modules/x/i.js")
    );
    assert_eq!(mocks.with_generation(a.clone()), a);
  }

  fn resolve_relative(specifier: &str, referrer: &str) -> Result<Url, ()> {
    url(referrer).join(specifier).map_err(|_| ())
  }

  #[test]
  fn redirects_mocked_modules_for_user_code() {
    let mocks = VitestMocks::default();
    let source: SourceFn = Rc::new(|referrer: &Url| {
      (referrer.path() == "/p/user.ts").then(|| {
        Arc::from(
          "import { a } from './db.ts';\nimport d from './db.ts';\nimport * as ns from './other.ts';\n",
        )
      })
    });
    mocks.set_loader(
      Rc::new(|s: &str, r: &str| Ok(url(r).join(s).unwrap())),
      source,
    );
    mocks.mocks.borrow_mut().insert(url("file:///p/db.ts"), 1);
    mocks
      .mocks
      .borrow_mut()
      .insert(url("file:///p/other.ts"), 2);
    // not enabled yet
    assert_eq!(
      mocks.resolve("./db.ts", "file:///p/user.ts", resolve_relative),
      Ok(url("file:///p/db.ts"))
    );
    mocks.enabled.set(true);
    assert_eq!(
      mocks.resolve("./db.ts", "file:///p/user.ts", resolve_relative),
      Ok(url("deno-vitest:///mock?id=1&name=a&name=default"))
    );
    assert_eq!(
      mocks.resolve("./other.ts", "file:///p/user.ts", resolve_relative),
      Ok(url("deno-vitest:///mock?id=2"))
    );
    // dependencies see the real module
    assert_eq!(
      mocks.resolve(
        "../../db.ts",
        "file:///p/node_modules/x/i.js",
        resolve_relative
      ),
      Ok(url("file:///p/db.ts"))
    );
    // the module itself, and the actual module
    assert_eq!(
      mocks.resolve(SELF_SPECIFIER, "file:///p/db.ts", resolve_relative),
      Ok(url("file:///p/db.ts"))
    );
    assert_eq!(
      mocks.resolve(
        "deno-vitest:///actual?url=file%3A%2F%2F%2Fp%2Fdb.ts",
        "deno-vitest:///mocker.js",
        resolve_relative
      ),
      Ok(url("file:///p/db.ts"))
    );
    // fresh instances after resetModules, resolving like the original
    mocks.generation.set(1);
    assert_eq!(
      mocks.resolve(
        "./b.ts",
        "file:///p/a.ts?__vitest_gen=1",
        resolve_relative
      ),
      Ok(url("file:///p/b.ts?__vitest_gen=1"))
    );
    assert_eq!(
      mocks.resolve(
        "./db.ts",
        "file:///p/a.ts?__vitest_gen=1",
        resolve_relative
      ),
      Ok(url("deno-vitest:///mock?id=1"))
    );
  }
}
