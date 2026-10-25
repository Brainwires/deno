// Copyright 2018-2026 the Deno authors. MIT license.

//! The program transforms behind Vitest's module mocking under `deno test`,
//! applied when a user module (a `file:` module outside `node_modules`) of a
//! Vitest run is emitted, after the TypeScript and JSX transforms:
//!
//! - **Hoisting** (like Vitest's `hoistMocks`): in a module that calls
//!   `vi.mock()` & co., `vi.mock` / `vi.unmock` / `vi.hoisted` calls move
//!   above the module's imports, which become awaited dynamic imports placed
//!   after them, with every reference to an imported binding rewritten to a
//!   property of the imported namespace. The mocks are thereby registered
//!   before any mocked module is resolved.
//! - **Mutable exports**: every module registers a setter for its exported
//!   bindings, keyed by its namespace object, so `vi.spyOn(namespace, name)`
//!   replaces the export for every importer (Vitest allows that because its
//!   module runner keeps exports on a plain object). Exported `const`
//!   declarations become `let` for that.
//! - **Type export stubs**: a TypeScript module's type-only exports (type
//!   aliases, interfaces, `export type { .. }`), which the TypeScript
//!   transform erases, become `undefined` value exports. Code that
//!   re-exports a type without `type` (`export { T } from "./types"`, valid
//!   without `isolatedModules`) then links, as it does under Vitest's module
//!   runner and Jest's CommonJS modules.
//!
//! It also sets the JSX runtime of a Jest project's user modules to the one
//! its Jest transform uses (see [`JsxRuntime`]), and gives its modules that
//! use `require` (CommonJS in Jest) Jest's `require()` (see js/jest.js).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::swc::ast::*;
use deno_ast::swc::atoms::Atom;
use deno_ast::swc::common::DUMMY_SP;
use deno_ast::swc::common::Mark;
use deno_ast::swc::common::Span;
use deno_ast::swc::common::SyntaxContext;
use deno_ast::swc::ecma_visit::Visit;
use deno_ast::swc::ecma_visit::VisitMut;
use deno_ast::swc::ecma_visit::VisitMutWith;
use deno_ast::swc::ecma_visit::VisitWith;
use deno_ast::swc::transforms::resolver;
use deno_core::url::Url;
use deno_error::JsErrorBox;
use deno_resolver::deno_json::TranspileAndEmitOptions;
use deno_resolver::deno_json::TranspileAndEmitOptionsRc;
use deno_resolver::emit::EmitProgramTransform;

/// Bumped whenever the output of the transforms changes (part of the emit
/// cache key).
const TRANSFORM_VERSION: u64 = 4;

/// The specifier a module imports itself with to register its exports (the
/// module loader resolves it to the importing module).
pub const SELF_SPECIFIER: &str = "deno-vitest:///self";

/// The objects whose `mock` / `unmock` / `hoisted` / `doMock` / `doUnmock`
/// calls are recognized (`jest` for the Jest flavour).
const UTILS_OBJECTS: [&str; 3] = ["vi", "vitest", "jest"];
/// Hoisted above the imports.
const HOISTABLE_METHODS: [&str; 2] = ["mock", "unmock"];
/// Whose `import("./path")` argument is rewritten to `"./path"`.
const DYNAMIC_IMPORT_METHODS: [&str; 4] =
  ["mock", "unmock", "doMock", "doUnmock"];
const HOISTED_METHOD: &str = "hoisted";

/// The program transform `deno test` installs on the emitter in Vitest mode.
#[derive(Debug, Default)]
pub struct VitestTransform {
  enabled: AtomicBool,
  /// The JSX runtime for user modules, when the test config decides it.
  jsx: std::sync::RwLock<Option<JsxRuntime>>,
  /// A Jest project: modules using `require` get Jest's.
  jest: AtomicBool,
  /// A Vitest / Jest project (a config was found): user modules do not see
  /// the `Deno` global, as under Node.js.
  hide_deno: AtomicBool,
}

/// The automatic JSX runtime a Jest project's transform (babel-jest,
/// `@swc/jest`, ts-jest) compiles JSX with, which user modules then get
/// instead of Deno's default classic runtime (`React.createElement`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsxRuntime {
  pub import_source: String,
  pub development: bool,
}

impl VitestTransform {
  /// Turns the transform on (it is installed before the test files are
  /// known; it only applies once a Vitest file is part of the run).
  pub fn enable(&self) {
    self.enabled.store(true, Ordering::Relaxed);
  }

  /// Sets the JSX runtime of user modules.
  pub fn set_jsx(&self, jsx: Option<JsxRuntime>) {
    *self.jsx.write().unwrap() = jsx;
  }

  /// Marks the run as a Jest project's.
  pub fn set_jest(&self, jest: bool) {
    self.jest.store(jest, Ordering::Relaxed);
  }

  /// Hides the `Deno` global from user modules.
  pub fn set_hide_deno(&self, hide: bool) {
    self.hide_deno.store(hide, Ordering::Relaxed);
  }
}

/// Whether `specifier` is a user module (Vitest transforms user source, but
/// leaves dependencies in `node_modules` external).
pub fn is_user_module(specifier: &Url) -> bool {
  specifier.scheme() == "file" && !specifier.path().contains("/node_modules/")
}

impl EmitProgramTransform for VitestTransform {
  fn cache_key(&self, specifier: &Url, media_type: MediaType) -> Option<u64> {
    if !self.enabled.load(Ordering::Relaxed) || !is_user_module(specifier) {
      return None;
    }
    match media_type {
      MediaType::JavaScript
      | MediaType::Jsx
      | MediaType::Mjs
      | MediaType::TypeScript
      | MediaType::Mts
      | MediaType::Tsx => Some(
        TRANSFORM_VERSION * 4
          + self.jest.load(Ordering::Relaxed) as u64
          + 2 * self.hide_deno.load(Ordering::Relaxed) as u64,
      ),
      _ => None,
    }
  }

  fn transpile_options(
    &self,
    specifier: &Url,
    options: &TranspileAndEmitOptionsRc,
  ) -> Option<TranspileAndEmitOptionsRc> {
    if !is_user_module(specifier) {
      return None;
    }
    let jsx = self.jsx.read().unwrap().clone()?;
    // only replaces the classic runtime (Deno's default)
    if !matches!(
      options.transpile.jsx,
      Some(deno_ast::JsxRuntime::Classic(_))
    ) {
      return None;
    }
    let mut transpile = options.transpile.clone();
    transpile.jsx = Some(deno_ast::JsxRuntime::Automatic(
      deno_ast::JsxAutomaticOptions {
        development: jsx.development,
        import_source: Some(jsx.import_source.clone()),
      },
    ));
    let pre_computed_hash = {
      use std::hash::Hash;
      use std::hash::Hasher;
      let mut hasher = twox_hash::XxHash64::default();
      transpile.hash(&mut hasher);
      options.emit.hash(&mut hasher);
      hasher.finish()
    };
    Some(std::sync::Arc::new(TranspileAndEmitOptions {
      no_transpile: options.no_transpile,
      transpile,
      emit: options.emit.clone(),
      pre_computed_hash,
      // denext: kept apart from `deno run`'s emits of the same modules
      // (denoland/deno#30791).
      emit_cache_name: Some(options.emit_cache_name_with("vitest")),
    }))
  }

  fn transform(
    &self,
    specifier: &Url,
    original: &deno_ast::ParsedSource,
    module: &mut Module,
  ) -> Result<(), JsErrorBox> {
    let type_exports = match original.program_ref() {
      deno_ast::ProgramRef::Module(original) => type_only_exports(original),
      deno_ast::ProgramRef::Script(_) => Vec::new(),
    };
    hoist_mocks(specifier, module)?;
    mutable_exports(module);
    stub_type_exports(module, type_exports);
    if self.jest.load(Ordering::Relaxed) {
      provide_jest_require(module);
    }
    if self.hide_deno.load(Ordering::Relaxed) {
      shadow_deno_global(module);
    }
    Ok(())
  }
}

/// Gives a module that refers to `Deno` without declaring it a top-level
/// `var Deno;`, so `typeof Deno` is `"undefined"` and `Deno.*` throws in a
/// test project's user code, as under Node.js (Vitest, Jest). The global
/// itself stays: Deno's Node.js APIs and the test runtime use it; only
/// `globalThis.Deno` still reaches it. (`var`, unlike `const`, has no TDZ for
/// a function of the module called during an import cycle.)
fn shadow_deno_global(module: &mut Module) {
  struct UsesDeno(bool);
  impl Visit for UsesDeno {
    fn visit_ident(&mut self, ident: &Ident) {
      if ident.sym == "Deno" {
        self.0 = true;
      }
    }
  }
  let mut finder = UsesDeno(false);
  module.visit_with(&mut finder);
  if !finder.0 || declares_top_level(module, "Deno") {
    return;
  }
  let mut items = parse_snippet("var Deno;\n".to_string());
  items.append(&mut module.body);
  module.body = items;
}

/// Gives a module that refers to `require` without declaring it at the top
/// level (a Jest test file written for CommonJS) Jest's `require()`. (A
/// nested declaration still shadows it.)
fn provide_jest_require(module: &mut Module) {
  struct UsesRequire(bool);
  impl Visit for UsesRequire {
    fn visit_ident(&mut self, ident: &Ident) {
      if ident.sym == "require" {
        self.0 = true;
      }
    }
  }
  let mut finder = UsesRequire(false);
  module.visit_with(&mut finder);
  if !finder.0 || declares_top_level(module, "require") {
    return;
  }
  let mut items = parse_snippet(format!(
    "import {{ jestRequire as __vi_jest_require__ }} from {};\nconst require = __vi_jest_require__(import.meta.url);\n",
    js_string(JEST_MODULE_SPECIFIER)
  ));
  items.append(&mut module.body);
  module.body = items;
}

/// Whether the module declares `name` at its top level.
fn declares_top_level(module: &Module, name: &str) -> bool {
  let declares = |decl: &Decl| match decl {
    Decl::Fn(f) => f.ident.sym == name,
    Decl::Class(c) => c.ident.sym == name,
    Decl::Var(var) => var.decls.iter().any(|d| {
      let mut idents = Vec::new();
      collect_pat_idents(&d.name, &mut idents);
      idents.iter().any(|i| i.sym == name)
    }),
    _ => false,
  };
  module.body.iter().any(|item| match item {
    ModuleItem::Stmt(Stmt::Decl(decl)) => declares(decl),
    ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
      declares(&export.decl)
    }
    ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
      import.specifiers.iter().any(|spec| match spec {
        ImportSpecifier::Named(s) => s.local.sym == name,
        ImportSpecifier::Default(s) => s.local.sym == name,
        ImportSpecifier::Namespace(s) => s.local.sym == name,
      })
    }
    _ => false,
  })
}

/// The built-in module with Jest's `require()`.
const JEST_MODULE_SPECIFIER: &str = "deno-vitest:///jest.js";

/// The names a TypeScript module exports only as types: exported type
/// aliases and interfaces, `export type { .. }` (with or without a source),
/// and `export { T }` of a local type.
fn type_only_exports(original: &Module) -> Vec<String> {
  let mut local_types = HashSet::new();
  for item in &original.body {
    if let ModuleItem::Stmt(Stmt::Decl(decl)) = item {
      match decl {
        Decl::TsTypeAlias(alias) => {
          local_types.insert(alias.id.sym.clone());
        }
        Decl::TsInterface(interface) => {
          local_types.insert(interface.id.sym.clone());
        }
        _ => {}
      }
    }
  }
  let mut names = Vec::new();
  for item in &original.body {
    let ModuleItem::ModuleDecl(decl) = item else {
      continue;
    };
    match decl {
      ModuleDecl::ExportDecl(export) => match &export.decl {
        Decl::TsTypeAlias(alias) => names.push(alias.id.sym.to_string()),
        Decl::TsInterface(interface) => {
          names.push(interface.id.sym.to_string())
        }
        _ => {}
      },
      ModuleDecl::ExportNamed(named) => {
        for spec in &named.specifiers {
          let ExportSpecifier::Named(spec) = spec else {
            continue;
          };
          let name = export_name(spec.exported.as_ref().unwrap_or(&spec.orig));
          let local_type = named.src.is_none()
            && matches!(&spec.orig, ModuleExportName::Ident(orig) if local_types.contains(&orig.sym));
          if named.type_only || spec.is_type_only || local_type {
            names.push(name);
          }
        }
      }
      _ => {}
    }
  }
  names
}

/// The names the (transpiled) module exports, `export *` aside.
fn exported_names(module: &Module) -> HashSet<String> {
  let mut names = HashSet::new();
  for item in &module.body {
    let ModuleItem::ModuleDecl(decl) = item else {
      continue;
    };
    match decl {
      ModuleDecl::ExportDecl(export) => match &export.decl {
        Decl::Fn(f) => {
          names.insert(f.ident.sym.to_string());
        }
        Decl::Class(c) => {
          names.insert(c.ident.sym.to_string());
        }
        Decl::Var(var) => {
          let mut idents = Vec::new();
          for declarator in &var.decls {
            collect_pat_idents(&declarator.name, &mut idents);
          }
          names.extend(idents.into_iter().map(|i| i.sym.to_string()));
        }
        Decl::TsEnum(e) => {
          names.insert(e.id.sym.to_string());
        }
        Decl::TsModule(_)
        | Decl::TsInterface(_)
        | Decl::TsTypeAlias(_)
        | Decl::Using(_) => {}
      },
      ModuleDecl::ExportNamed(named) => {
        for spec in &named.specifiers {
          match spec {
            ExportSpecifier::Named(spec) => {
              names.insert(export_name(
                spec.exported.as_ref().unwrap_or(&spec.orig),
              ));
            }
            ExportSpecifier::Namespace(spec) => {
              names.insert(export_name(&spec.name));
            }
            ExportSpecifier::Default(spec) => {
              names.insert(spec.exported.sym.to_string());
            }
          }
        }
      }
      ModuleDecl::ExportDefaultDecl(_) | ModuleDecl::ExportDefaultExpr(_) => {
        names.insert("default".to_string());
      }
      _ => {}
    }
  }
  names
}

/// Exports `undefined` under each type-only export name the module does not
/// also export as a value (see the module docs).
fn stub_type_exports(module: &mut Module, type_exports: Vec<String>) {
  if type_exports.is_empty() {
    return;
  }
  let exported = exported_names(module);
  let mut seen = HashSet::new();
  let mut code = String::new();
  for name in type_exports {
    if exported.contains(&name) || !seen.insert(name.clone()) {
      continue;
    }
    let local = format!("__vi_type_export_{}__", seen.len() - 1);
    let exported_as = if is_identifier_name(&name) {
      name
    } else {
      js_string(&name)
    };
    code.push_str(&format!(
      "let {local};\nexport {{ {local} as {exported_as} }};\n"
    ));
  }
  if !code.is_empty() {
    module.body.extend(parse_snippet(code));
  }
}

fn str_value(s: &Str) -> String {
  s.value.to_string_lossy().into_owned()
}

fn export_name(name: &ModuleExportName) -> String {
  match name {
    ModuleExportName::Ident(ident) => ident.sym.to_string(),
    ModuleExportName::Str(s) => str_value(s),
  }
}

fn js_string(value: &str) -> String {
  deno_core::serde_json::to_string(value).unwrap()
}

/// `vi.mock(...)` & co.: the utilities object and the method name.
fn mock_api_call(call: &CallExpr) -> Option<(&Atom, &Atom)> {
  let Callee::Expr(callee) = &call.callee else {
    return None;
  };
  let Expr::Member(member) = &**callee else {
    return None;
  };
  let Expr::Ident(object) = &*member.obj else {
    return None;
  };
  let MemberProp::Ident(method) = &member.prop else {
    return None;
  };
  if !UTILS_OBJECTS.contains(&&*object.sym) {
    return None;
  }
  Some((&object.sym, &method.sym))
}

fn is_mock_api_method(method: &str) -> bool {
  method == HOISTED_METHOD
    || HOISTABLE_METHODS.contains(&method)
    || DYNAMIC_IMPORT_METHODS.contains(&method)
}

/// `vi.mock("./path")`-like name for error messages.
fn call_name(call: &CallExpr) -> String {
  let Some((object, method)) = mock_api_call(call) else {
    return "\"hoisted method\"".to_string();
  };
  let arg = match call.args.first().map(|a| &*a.expr) {
    Some(Expr::Lit(Lit::Str(s))) => js_string(&str_value(s)),
    _ => String::new(),
  };
  format!("{object}.{method}({arg})")
}

/// Whether an import comes from the module providing `vi` (it stays a
/// static import).
fn is_hoisted_module(src: &str) -> bool {
  match src {
    "vitest" | "vite-plus/test" | "@jest/globals" => true,
    _ => {
      let Some(rest) = src.strip_prefix("npm:") else {
        return false;
      };
      let rest = rest.trim_start_matches('/');
      let without_version = |name: &str| -> bool {
        rest == name
          || rest
            .strip_prefix(name)
            .is_some_and(|tail| tail.starts_with('@'))
      };
      without_version("vitest") || without_version("@jest/globals")
    }
  }
}

/// Finds whether the module calls a mocking API at all.
#[derive(Default)]
struct MockApiFinder {
  found: bool,
}

impl Visit for MockApiFinder {
  fn visit_call_expr(&mut self, call: &CallExpr) {
    if let Some((_, method)) = mock_api_call(call)
      && is_mock_api_method(method)
    {
      self.found = true;
      return;
    }
    call.visit_children_with(self);
  }
}

/// `vi.mock(import("./path"))` / `vi.mock(await import("./path"))` ->
/// `vi.mock("./path")`.
struct DynamicImportArgRewriter;

impl VisitMut for DynamicImportArgRewriter {
  fn visit_mut_call_expr(&mut self, call: &mut CallExpr) {
    call.visit_mut_children_with(self);
    let Some((_, method)) = mock_api_call(call) else {
      return;
    };
    if !DYNAMIC_IMPORT_METHODS.contains(&&**method) {
      return;
    }
    let Some(first) = call.args.first_mut() else {
      return;
    };
    let import_call = match &*first.expr {
      Expr::Call(c) => Some(c),
      Expr::Await(AwaitExpr { arg, .. }) => match &**arg {
        Expr::Call(c) => Some(c),
        _ => None,
      },
      _ => None,
    };
    let Some(import_call) = import_call else {
      return;
    };
    if !matches!(import_call.callee, Callee::Import(_)) {
      return;
    }
    if let Some(source) = import_call.args.first() {
      let source = source.expr.clone();
      first.expr = source;
    }
  }
}

/// Collects the mocking API calls (hoistable or hoisted) below a node.
#[derive(Default)]
struct NestedMockCalls {
  calls: Vec<String>,
}

impl Visit for NestedMockCalls {
  fn visit_call_expr(&mut self, call: &CallExpr) {
    if let Some((_, method)) = mock_api_call(call)
      && (HOISTABLE_METHODS.contains(&&**method) || &**method == HOISTED_METHOD)
    {
      self.calls.push(call_name(call));
    }
    call.visit_children_with(self);
  }
}

/// The hoisted call of a top-level statement, if it is one: `vi.mock(..)`,
/// `vi.hoisted(..)`, `await vi.hoisted(..)` or
/// `const x = (await) vi.hoisted(..)`.
fn hoisted_call(stmt: &Stmt) -> Option<&CallExpr> {
  fn hoisted_in_expr(expr: &Expr, allow_mock: bool) -> Option<&CallExpr> {
    let call = match expr {
      Expr::Call(call) => call,
      Expr::Await(AwaitExpr { arg, .. }) => match &**arg {
        Expr::Call(call) => call,
        _ => return None,
      },
      _ => return None,
    };
    let (_, method) = mock_api_call(call)?;
    if &**method == HOISTED_METHOD
      || (allow_mock
        && HOISTABLE_METHODS.contains(&&**method)
        && matches!(expr, Expr::Call(_)))
    {
      Some(call)
    } else {
      None
    }
  }
  match stmt {
    Stmt::Expr(ExprStmt { expr, .. }) => hoisted_in_expr(expr, true),
    Stmt::Decl(Decl::Var(var)) => {
      let init = var.decls.first()?.init.as_deref()?;
      hoisted_in_expr(init, false)
    }
    _ => None,
  }
}

/// How an imported binding is read once its import is dynamic.
#[derive(Clone)]
enum ImportedBinding {
  Namespace(Atom),
  Property(Atom, String),
}

impl ImportedBinding {
  fn to_expr(&self, span: Span) -> Expr {
    match self {
      ImportedBinding::Namespace(id) => {
        Expr::Ident(Ident::new_no_ctxt(id.clone(), span))
      }
      ImportedBinding::Property(id, name) => {
        let obj = Box::new(Expr::Ident(Ident::new_no_ctxt(id.clone(), span)));
        let prop = if is_identifier_name(name) {
          MemberProp::Ident(IdentName::new(name.as_str().into(), span))
        } else {
          MemberProp::Computed(ComputedPropName {
            span,
            expr: Box::new(Expr::Lit(Lit::Str(Str {
              span,
              value: name.as_str().into(),
              raw: None,
            }))),
          })
        };
        Expr::Member(MemberExpr { span, obj, prop })
      }
    }
  }
}

fn is_identifier_name(name: &str) -> bool {
  let mut chars = name.chars();
  chars
    .next()
    .is_some_and(|c| c == '$' || c == '_' || c.is_alphabetic())
    && chars.all(|c| c == '$' || c == '_' || c.is_alphanumeric())
}

/// Rewrites references to imported bindings.
struct ImportRefRewriter<'a> {
  bindings: &'a HashMap<(Atom, SyntaxContext), ImportedBinding>,
}

impl VisitMut for ImportRefRewriter<'_> {
  fn visit_mut_expr(&mut self, expr: &mut Expr) {
    if let Expr::Ident(ident) = expr
      && let Some(binding) = self.bindings.get(&(ident.sym.clone(), ident.ctxt))
    {
      *expr = binding.to_expr(ident.span);
      return;
    }
    expr.visit_mut_children_with(self);
  }

  fn visit_mut_prop(&mut self, prop: &mut Prop) {
    if let Prop::Shorthand(ident) = prop
      && let Some(binding) = self.bindings.get(&(ident.sym.clone(), ident.ctxt))
    {
      *prop = Prop::KeyValue(KeyValueProp {
        key: PropName::Ident(IdentName::new(ident.sym.clone(), ident.span)),
        value: Box::new(binding.to_expr(ident.span)),
      });
      return;
    }
    prop.visit_mut_children_with(self);
  }
}

/// `const <id> = await import("<src>", { with: <attributes> });`
fn dynamic_import_const(id: &Atom, decl: &ImportDecl) -> ModuleItem {
  let mut args = vec![ExprOrSpread {
    spread: None,
    expr: Box::new(Expr::Lit(Lit::Str((*decl.src).clone()))),
  }];
  if let Some(with) = &decl.with {
    args.push(ExprOrSpread {
      spread: None,
      expr: Box::new(Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props: vec![PropOrSpread::Prop(Box::new(Prop::KeyValue(
          KeyValueProp {
            key: PropName::Ident(IdentName::new("with".into(), DUMMY_SP)),
            value: Box::new(Expr::Object((**with).clone())),
          },
        )))],
      })),
    });
  }
  let call = Expr::Call(CallExpr {
    span: decl.span,
    ctxt: SyntaxContext::empty(),
    callee: Callee::Import(Import {
      span: DUMMY_SP,
      phase: ImportPhase::Evaluation,
    }),
    args,
    type_args: None,
  });
  ModuleItem::Stmt(Stmt::Decl(Decl::Var(Box::new(VarDecl {
    span: decl.span,
    ctxt: SyntaxContext::empty(),
    kind: VarDeclKind::Const,
    declare: false,
    decls: vec![VarDeclarator {
      span: DUMMY_SP,
      name: Pat::Ident(BindingIdent {
        id: Ident::new_no_ctxt(id.clone(), DUMMY_SP),
        type_ann: None,
      }),
      init: Some(Box::new(Expr::Await(AwaitExpr {
        span: decl.span,
        arg: Box::new(call),
      }))),
      definite: false,
    }],
  }))))
}

/// Parses generated code into module items without source positions.
fn parse_snippet(code: String) -> Vec<ModuleItem> {
  struct StripSpans;
  impl VisitMut for StripSpans {
    fn visit_mut_span(&mut self, span: &mut Span) {
      *span = DUMMY_SP;
    }
  }
  let parsed = deno_ast::parse_module(ParseParams {
    specifier: Url::parse("file:///deno_vitest_snippet.js").unwrap(),
    text: code.into(),
    media_type: MediaType::JavaScript,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })
  .expect("generated code parses");
  let mut module = match (*parsed.program()).clone() {
    Program::Module(module) => module,
    Program::Script(_) => unreachable!("parsed as a module"),
  };
  module.visit_mut_with(&mut StripSpans);
  module.body
}

/// Vitest's `hoistMocks`, on the transpiled module.
fn hoist_mocks(specifier: &Url, module: &mut Module) -> Result<(), JsErrorBox> {
  let mut finder = MockApiFinder::default();
  module.visit_with(&mut finder);
  if !finder.found {
    return Ok(());
  }

  module.visit_mut_with(&mut DynamicImportArgRewriter);
  // Bindings are told apart by syntax context: resolve the transpiled
  // program's scopes again (the transpiler's hygiene pass dropped them).
  module.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), false));

  // Classify the top-level items, and check that the hoisted calls are
  // only at the top level and not inside each other.
  let mut misplaced = Vec::new();
  let mut hoisted_indexes = HashSet::new();
  let mut used_utils = Vec::<Atom>::new();
  for (index, item) in module.body.iter().enumerate() {
    match item {
      ModuleItem::Stmt(stmt) => {
        if let Some(call) = hoisted_call(stmt) {
          hoisted_indexes.insert(index);
          if let Some((object, _)) = mock_api_call(call)
            && !used_utils.contains(object)
          {
            used_utils.push(object.clone());
          }
          let mut nested = NestedMockCalls::default();
          for arg in &call.args {
            arg.visit_with(&mut nested);
          }
          if let Some(inner) = nested.calls.first() {
            return Err(JsErrorBox::new(
              "SyntaxError",
              format!(
                "Cannot call {inner} inside {}: both methods are hoisted to the top of the file and not actually called inside each other.",
                call_name(call)
              ),
            ));
          }
          continue;
        }
        let mut nested = NestedMockCalls::default();
        stmt.visit_with(&mut nested);
        misplaced.extend(nested.calls);
      }
      ModuleItem::ModuleDecl(decl) => {
        // `export const x = vi.hoisted(..)` / `export default vi.mock(..)`
        let exported_call = match decl {
          ModuleDecl::ExportDecl(ExportDecl {
            decl: Decl::Var(var),
            ..
          }) => hoisted_call(&Stmt::Decl(Decl::Var(var.clone()))).cloned(),
          ModuleDecl::ExportDefaultExpr(default) => {
            hoisted_call(&Stmt::Expr(ExprStmt {
              span: DUMMY_SP,
              expr: default.expr.clone(),
            }))
            .cloned()
          }
          _ => None,
        };
        if let Some(call) = exported_call {
          let name = call_name(&call);
          let message = if name.contains(".hoisted(") {
            "Cannot export hoisted variable. You can control hoisting behavior by placing the import from this file first.".to_string()
          } else {
            let method = name.split('(').next().unwrap_or_default();
            format!(
              "Cannot export the result of \"{method}\". Remove export declaration because \"{method}\" doesn't return anything."
            )
          };
          return Err(JsErrorBox::new("SyntaxError", message));
        }
        let mut nested = NestedMockCalls::default();
        decl.visit_with(&mut nested);
        misplaced.extend(nested.calls);
      }
    }
  }
  if !misplaced.is_empty() {
    let plural = misplaced.len() > 1;
    let file = specifier
      .path_segments()
      .and_then(|mut s| s.next_back())
      .unwrap_or_default();
    let mut message = format!(
      "{} call{} in \"{file}\" {} defined outside of the module's top level scope:\n\n",
      misplaced.len(),
      if plural { "s" } else { "" },
      if plural { "were" } else { "was" },
    );
    for call in &misplaced {
      message.push_str(&format!("- {call}\n"));
    }
    message.push_str(&format!(
      "\nAlthough {} will be hoisted and executed before anything in this file. Move {} to the top level to reflect {} actual execution order.\nSee: https://vitest.dev/guide/mocking/modules#how-it-works",
      if plural {
        "they appear nested, they"
      } else {
        "it appears nested, it"
      },
      if plural { "them" } else { "it" },
      if plural { "their" } else { "its" },
    ));
    return Err(JsErrorBox::generic(message));
  }

  // Partition the body: static imports of the `vi` module stay, other
  // imports become dynamic after the hoisted calls.
  let body = std::mem::take(&mut module.body);
  let mut kept_imports = Vec::new();
  let mut hoisted = Vec::new();
  let mut dynamic_imports = Vec::new();
  let mut rest = Vec::new();
  let mut bindings = HashMap::new();
  let mut hoisted_module_imported = false;
  let mut uid = 0;
  for (index, item) in body.into_iter().enumerate() {
    if hoisted_indexes.contains(&index) {
      hoisted.push(item);
      continue;
    }
    match item {
      ModuleItem::ModuleDecl(ModuleDecl::Import(decl))
        if decl.phase == ImportPhase::Evaluation =>
      {
        if is_hoisted_module(&str_value(&decl.src)) {
          hoisted_module_imported = true;
          kept_imports.push(ModuleItem::ModuleDecl(ModuleDecl::Import(decl)));
          continue;
        }
        let id: Atom = format!("__vi_import_{uid}__").into();
        uid += 1;
        for spec in &decl.specifiers {
          let (local, binding) = match spec {
            ImportSpecifier::Named(named) => (
              &named.local,
              ImportedBinding::Property(
                id.clone(),
                named
                  .imported
                  .as_ref()
                  .map(export_name)
                  .unwrap_or_else(|| named.local.sym.to_string()),
              ),
            ),
            ImportSpecifier::Default(default) => (
              &default.local,
              ImportedBinding::Property(id.clone(), "default".to_string()),
            ),
            ImportSpecifier::Namespace(namespace) => {
              (&namespace.local, ImportedBinding::Namespace(id.clone()))
            }
          };
          bindings.insert((local.sym.clone(), local.ctxt), binding);
        }
        dynamic_imports.push(dynamic_import_const(&id, &decl));
      }
      item => rest.push(item),
    }
  }

  let mut rewriter = ImportRefRewriter {
    bindings: &bindings,
  };
  for item in hoisted.iter_mut().chain(rest.iter_mut()) {
    item.visit_mut_with(&mut rewriter);
  }

  let mut prelude = Vec::new();
  if !hoisted_module_imported && !hoisted.is_empty() {
    // `vi` is a global (`globals: true`) or imported from elsewhere
    let imported_elsewhere = used_utils
      .iter()
      .any(|name| bindings.keys().any(|(sym, _)| sym == name));
    if imported_elsewhere {
      let check = used_utils
        .iter()
        .map(|name| {
          format!("typeof globalThis[{}] === \"undefined\"", js_string(name))
        })
        .collect::<Vec<_>>()
        .join(" && ");
      prelude.extend(parse_snippet(format!(
        "if ({check}) {{ throw new Error({}); }}\n",
        js_string(
          "There are some problems in resolving the mocks API.\nYou may encounter this issue when importing the mocks API from another module other than 'vitest'.\nTo fix this issue you can either:\n- import the mocks API directly from 'vitest'\n- enable the 'globals' option"
        )
      )));
    } else {
      let mut code = String::new();
      let vitest_names = used_utils
        .iter()
        .filter(|n| &***n != "jest")
        .map(|n| n.to_string())
        .collect::<Vec<_>>();
      if !vitest_names.is_empty() {
        code.push_str(&format!(
          "import {{ {} }} from \"vitest\";\n",
          vitest_names.join(", ")
        ));
      }
      if used_utils.iter().any(|n| &**n == "jest") {
        code.push_str("import { jest } from \"@jest/globals\";\n");
      }
      prelude.extend(parse_snippet(code));
    }
  }

  module.body = prelude
    .into_iter()
    .chain(kept_imports)
    .chain(hoisted)
    .chain(dynamic_imports)
    .chain(rest)
    .collect();
  Ok(())
}

/// A top-level binding of the module.
enum Local {
  /// A function or class declaration, or a `let` / `var`.
  Mutable,
  /// A `const` declaration (the body item index), turned into a `let` when
  /// exported.
  Const(usize),
  /// An imported binding: the module specifier and imported name.
  Import(String, String),
}

/// How a setter reaches an exported binding.
enum ExportTarget {
  Local(Atom),
  /// An export of another module (namespace source index, name there).
  Delegate(usize, String),
}

fn collect_pat_idents(pat: &Pat, out: &mut Vec<Ident>) {
  match pat {
    Pat::Ident(binding) => out.push(binding.id.clone()),
    Pat::Array(array) => {
      for elem in array.elems.iter().flatten() {
        collect_pat_idents(elem, out);
      }
    }
    Pat::Rest(rest) => collect_pat_idents(&rest.arg, out),
    Pat::Object(object) => {
      for prop in &object.props {
        match prop {
          ObjectPatProp::KeyValue(kv) => collect_pat_idents(&kv.value, out),
          ObjectPatProp::Assign(assign) => out.push(assign.key.id.clone()),
          ObjectPatProp::Rest(rest) => collect_pat_idents(&rest.arg, out),
        }
      }
    }
    Pat::Assign(assign) => collect_pat_idents(&assign.left, out),
    Pat::Invalid(_) | Pat::Expr(_) => {}
  }
}

/// Makes the module's exports replaceable (see the module docs).
fn mutable_exports(module: &mut Module) {
  if !module
    .body
    .iter()
    .any(|item| matches!(item, ModuleItem::ModuleDecl(decl) if !matches!(decl, ModuleDecl::Import(_))))
  {
    return;
  }
  module.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), false));

  // The sources of re-exported bindings, imported as namespaces.
  let mut sources: Vec<String> = Vec::new();
  let mut source_index = |src: &str| {
    Some(match sources.iter().position(|s| s == src) {
      Some(index) => index,
      None => {
        sources.push(src.to_string());
        sources.len() - 1
      }
    })
  };
  // a JSON (or other non-JS) module has nothing to spy on
  let js_source = |src: &Str, with: &Option<Box<ObjectLit>>| {
    with.is_none().then(|| str_value(src))
  };

  let mut locals: HashMap<(Atom, SyntaxContext), Local> = HashMap::new();
  let add_decl =
    |decl: &Decl,
     index: usize,
     locals: &mut HashMap<(Atom, SyntaxContext), Local>| {
      match decl {
        Decl::Fn(f) => {
          locals.insert((f.ident.sym.clone(), f.ident.ctxt), Local::Mutable);
        }
        Decl::Class(c) => {
          locals.insert((c.ident.sym.clone(), c.ident.ctxt), Local::Mutable);
        }
        Decl::Var(var) => {
          let mut idents = Vec::new();
          for declarator in &var.decls {
            collect_pat_idents(&declarator.name, &mut idents);
          }
          for ident in idents {
            let local = if var.kind == VarDeclKind::Const {
              Local::Const(index)
            } else {
              Local::Mutable
            };
            locals.insert((ident.sym, ident.ctxt), local);
          }
        }
        _ => {}
      }
    };
  for (index, item) in module.body.iter().enumerate() {
    match item {
      ModuleItem::Stmt(Stmt::Decl(decl)) => add_decl(decl, index, &mut locals),
      ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
        add_decl(&export.decl, index, &mut locals)
      }
      ModuleItem::ModuleDecl(ModuleDecl::Import(import))
        if import.phase == ImportPhase::Evaluation =>
      {
        let Some(source) = js_source(&import.src, &import.with) else {
          continue;
        };
        for spec in &import.specifiers {
          match spec {
            ImportSpecifier::Named(named) => {
              let name = named
                .imported
                .as_ref()
                .map(export_name)
                .unwrap_or_else(|| named.local.sym.to_string());
              locals.insert(
                (named.local.sym.clone(), named.local.ctxt),
                Local::Import(source.clone(), name),
              );
            }
            ImportSpecifier::Default(default) => {
              locals.insert(
                (default.local.sym.clone(), default.local.ctxt),
                Local::Import(source.clone(), "default".to_string()),
              );
            }
            ImportSpecifier::Namespace(_) => {}
          }
        }
      }
      _ => {}
    }
  }

  let mut exports: Vec<(String, ExportTarget)> = Vec::new();
  let mut stars: Vec<usize> = Vec::new();
  let mut consts_to_let: HashSet<usize> = HashSet::new();
  let export_local =
    |name: String,
     ident: &Ident,
     exports: &mut Vec<(String, ExportTarget)>,
     consts_to_let: &mut HashSet<usize>,
     source_index: &mut dyn FnMut(&str) -> Option<usize>| {
      match locals.get(&(ident.sym.clone(), ident.ctxt)) {
        Some(Local::Mutable) => {
          exports.push((name, ExportTarget::Local(ident.sym.clone())))
        }
        Some(Local::Const(index)) => {
          consts_to_let.insert(*index);
          exports.push((name, ExportTarget::Local(ident.sym.clone())));
        }
        Some(Local::Import(source, imported)) => {
          if let Some(index) = source_index(source) {
            exports
              .push((name, ExportTarget::Delegate(index, imported.clone())))
          }
        }
        None => {}
      }
    };
  for item in &module.body {
    let ModuleItem::ModuleDecl(decl) = item else {
      continue;
    };
    match decl {
      ModuleDecl::ExportDecl(export) => match &export.decl {
        Decl::Fn(f) => export_local(
          f.ident.sym.to_string(),
          &f.ident,
          &mut exports,
          &mut consts_to_let,
          &mut source_index,
        ),
        Decl::Class(c) => export_local(
          c.ident.sym.to_string(),
          &c.ident,
          &mut exports,
          &mut consts_to_let,
          &mut source_index,
        ),
        Decl::Var(var) => {
          let mut idents = Vec::new();
          for declarator in &var.decls {
            collect_pat_idents(&declarator.name, &mut idents);
          }
          for ident in idents {
            export_local(
              ident.sym.to_string(),
              &ident,
              &mut exports,
              &mut consts_to_let,
              &mut source_index,
            );
          }
        }
        _ => {}
      },
      ModuleDecl::ExportNamed(named) => {
        let source = match &named.src {
          Some(src) => {
            match js_source(src, &named.with).and_then(|s| source_index(&s)) {
              Some(index) => Some(index),
              None => continue,
            }
          }
          None => None,
        };
        for spec in &named.specifiers {
          let ExportSpecifier::Named(spec) = spec else {
            continue;
          };
          let name = export_name(spec.exported.as_ref().unwrap_or(&spec.orig));
          match (source, &spec.orig) {
            (Some(source), orig) => exports
              .push((name, ExportTarget::Delegate(source, export_name(orig)))),
            (None, ModuleExportName::Ident(local)) => export_local(
              name,
              local,
              &mut exports,
              &mut consts_to_let,
              &mut source_index,
            ),
            (None, ModuleExportName::Str(_)) => {}
          }
        }
      }
      ModuleDecl::ExportAll(all) => {
        if let Some(index) =
          js_source(&all.src, &all.with).and_then(|s| source_index(&s))
        {
          stars.push(index);
        }
      }
      ModuleDecl::ExportDefaultDecl(default) => {
        let ident = match &default.decl {
          DefaultDecl::Fn(f) => f.ident.as_ref(),
          DefaultDecl::Class(c) => c.ident.as_ref(),
          DefaultDecl::TsInterfaceDecl(_) => None,
        };
        if let Some(ident) = ident {
          exports.push((
            "default".to_string(),
            ExportTarget::Local(ident.sym.clone()),
          ));
        }
      }
      _ => {}
    }
  }
  if exports.is_empty() && stars.is_empty() {
    return;
  }

  for index in consts_to_let {
    match &mut module.body[index] {
      ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => {
        var.kind = VarDeclKind::Let
      }
      ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
        decl: Decl::Var(var),
        ..
      })) => var.kind = VarDeclKind::Let,
      _ => {}
    }
  }

  let mut code =
    format!("import * as __vitest_self__ from \"{SELF_SPECIFIER}\";\n");
  for (index, source) in sources.iter().enumerate() {
    code.push_str(&format!(
      "import * as __vitest_source_{index}__ from {};\n",
      js_string(source)
    ));
  }
  code.push_str(
    "(globalThis[Symbol.for(\"deno.vitest.exports\")] ??= new WeakMap()).set(__vitest_self__, function (__vitest_name__, __vitest_value__) {\n\
     const __vitest_set__ = (namespace, name) => globalThis[Symbol.for(\"deno.vitest.exports\")].get(namespace)?.(name, __vitest_value__) ?? false;\n\
     switch (__vitest_name__) {\n",
  );
  let mut seen = HashSet::new();
  for (name, target) in &exports {
    if !seen.insert(name.clone()) {
      continue;
    }
    match target {
      ExportTarget::Local(local) => code.push_str(&format!(
        "case {}: {local} = __vitest_value__; return true;\n",
        js_string(name)
      )),
      ExportTarget::Delegate(source, imported) => code.push_str(&format!(
        "case {}: return __vitest_set__(__vitest_source_{source}__, {});\n",
        js_string(name),
        js_string(imported)
      )),
    }
  }
  code.push_str("}\n");
  for star in &stars {
    code.push_str(&format!(
      "if (__vitest_set__(__vitest_source_{star}__, __vitest_name__)) return true;\n"
    ));
  }
  code.push_str("return false;\n});\n");
  // Only the referenced sources are imported (a source whose bindings
  // were all skipped would be imported for nothing, but is harmless).
  module.body.extend(parse_snippet(code));
}

#[cfg(test)]
mod tests {
  use deno_ast::EmitOptions;
  use deno_ast::SourceMapOption;
  use deno_ast::TranspileOptions;

  use super::*;

  /// Transpiles `source` like the emitter does with the transform installed.
  fn transform(source: &str) -> Result<String, String> {
    transform_with(&VitestTransform::default(), source)
  }

  fn transform_with(
    transform: &VitestTransform,
    source: &str,
  ) -> Result<String, String> {
    let specifier = Url::parse("file:///project/a.test.ts").unwrap();
    let parsed = deno_ast::parse_module(ParseParams {
      specifier: specifier.clone(),
      text: source.into(),
      media_type: MediaType::TypeScript,
      capture_tokens: false,
      scope_analysis: false,
      maybe_syntax: None,
    })
    .unwrap();
    let options = TranspileOptions {
      jsx: None,
      ..Default::default()
    };
    let comments = parsed.comments().as_single_threaded();
    let source_map =
      deno_ast::SourceMap::single(specifier.clone(), source.to_string());
    let program = parsed
      .globals()
      .with(|marks| {
        deno_ast::fold_program(
          (*parsed.program()).clone(),
          &options,
          &source_map,
          &comments,
          marks,
          Box::new(parsed.diagnostics().iter()),
        )
      })
      .unwrap();
    let Program::Module(mut module) = program else {
      unreachable!()
    };
    parsed
      .globals()
      .with(|_| transform.transform(&specifier, &parsed, &mut module))
      .map_err(|e| e.to_string())?;
    let emitted = deno_ast::emit(
      (&Program::Module(module)).into(),
      &comments,
      &source_map,
      &EmitOptions {
        source_map: SourceMapOption::None,
        ..Default::default()
      },
    )
    .unwrap();
    Ok(emitted.text)
  }

  #[test]
  fn provides_jest_require() {
    let jest = |source: &str| {
      let transform = VitestTransform::default();
      transform.set_jest(true);
      transform_with(&transform, source).unwrap()
    };
    let out = jest("const x = require(\"./a\");\n");
    assert!(
      out.starts_with(
        "import { jestRequire as __vi_jest_require__ } from \"deno-vitest:///jest.js\";\nconst require = __vi_jest_require__(import.meta.url);\n"
      ),
      "{out}"
    );
    // a declared `require` is left alone
    let out = jest(
      "import { createRequire } from \"node:module\";\nconst require = createRequire(import.meta.url);\nrequire(\"./a\");\n",
    );
    assert!(!out.contains("__vi_jest_require__"), "{out}");
    // only in a Jest project
    let out = transform("const x = require(\"./a\");\n").unwrap();
    assert!(!out.contains("__vi_jest_require__"), "{out}");
  }

  #[test]
  fn shadows_the_deno_global() {
    let hiding = |source: &str| {
      let transform = VitestTransform::default();
      transform.set_hide_deno(true);
      transform_with(&transform, source).unwrap()
    };
    let out = hiding("export const isDeno = typeof Deno !== \"undefined\";\n");
    assert!(out.starts_with("var Deno;\n"), "{out}");
    // a module declaring its own `Deno`, or not using it, is left alone
    let out = hiding("const Deno = { x: 1 };\nexport const x = Deno.x;\n");
    assert!(!out.contains("var Deno"), "{out}");
    let out =
      hiding("export const o = { Deno: 1 };\nexport const p = o.Deno;\n");
    assert!(!out.contains("var Deno"), "{out}");
    // only in a Vitest / Jest project
    let out = transform("export const d = Deno.env;\n").unwrap();
    assert!(!out.contains("var Deno"), "{out}");
  }

  #[test]
  fn stubs_type_only_exports() {
    let out = transform(
      r#"export type A = string;
export interface B { x: number }
type C = number;
interface D {}
export { C, D as E };
export type { F } from "./f.ts";
export { type G, h } from "./g.ts";
export type V = 1;
export const V = 1;
"#,
    )
    .unwrap();
    for name in ["A", "B", "C", "E", "F", "G"] {
      assert!(out.contains(&format!(" as {name} }}")), "{name}: {out}");
    }
    // a name also exported as a value is left alone
    assert!(!out.contains(" as V }"), "{out}");
    assert!(out.contains("export { h } from \"./g.ts\""), "{out}");
    // a plain JavaScript module has nothing to stub
    let out = transform("export const a = 1;\n").unwrap();
    assert!(!out.contains("__vi_type_export_"), "{out}");
  }

  #[test]
  fn hoists_mocks_above_imports() {
    let out = transform(
      r#"import { describe, it, vi } from "vitest";
import { a, type T } from "./a.ts";
import def, * as ns from "./b.ts";
import "./side.ts";
const x: T = a();
vi.mock("./a.ts", () => ({ a: () => 1 }));
const h = vi.hoisted(() => 2);
it("x", () => { const { a: local } = { a }; return [def, ns.y, local, h]; });
"#,
    )
    .unwrap();
    assert_eq!(
      out,
      r#"import { it, vi } from "vitest";
vi.mock("./a.ts", ()=>({
    a: ()=>1
  }));
const h = vi.hoisted(()=>2);
const __vi_import_0__ = await import("./a.ts");
const __vi_import_1__ = await import("./b.ts");
const __vi_import_2__ = await import("./side.ts");
const x = __vi_import_0__.a();
it("x", ()=>{
  const { a: local } = {
    a: __vi_import_0__.a
  };
  return [
    __vi_import_1__.default,
    __vi_import_1__.y,
    local,
    h
  ];
});
"#
    );
  }

  #[test]
  fn shadowed_bindings_are_left_alone() {
    let out = transform(
      r#"import { vi } from "vitest";
import { a } from "./a.ts";
vi.mock("./a.ts");
function f(a: number) { return a; }
export const g = () => a;
"#,
    )
    .unwrap();
    assert!(out.contains("function f(a) {\n  return a;\n}"), "{out}");
    assert!(
      out.contains("export let g = ()=>__vi_import_0__.a;"),
      "{out}"
    );
  }

  #[test]
  fn adds_the_vi_import_for_globals() {
    let out = transform("vi.mock(import('./a.ts'));\nimport { a } from './a.ts';\nconsole.log(a);\n").unwrap();
    assert!(
      out.starts_with("import { vi } from \"vitest\";\nvi.mock('./a.ts');\nconst __vi_import_0__ = await import('./a.ts');\nconsole.log(__vi_import_0__.a);"),
      "{out}"
    );
    let out = transform("jest.mock('./a.ts', () => ({}));\n").unwrap();
    assert!(
      out.starts_with("import { jest } from \"@jest/globals\";\njest.mock("),
      "{out}"
    );
  }

  #[test]
  fn do_mock_rewrites_imports_without_hoisting() {
    let out = transform(
      "import { vi } from 'vitest';\nimport { a } from './a.ts';\nconsole.log(a);\nvi.doMock(import('./b.ts'), () => ({}));\n",
    )
    .unwrap();
    assert!(
      out.contains("const __vi_import_0__ = await import('./a.ts');\nconsole.log(__vi_import_0__.a);\nvi.doMock('./b.ts'"),
      "{out}"
    );
  }

  #[test]
  fn rejects_nested_hoisted_calls() {
    let err = transform(
      "import { vi, it } from 'vitest';\nit('x', () => { vi.mock('./a.ts'); });\n",
    )
    .unwrap_err();
    assert!(
      err.contains("1 call in \"a.test.ts\" was defined outside of the module's top level scope:\n\n- vi.mock(\"./a.ts\")"),
      "{err}"
    );
    let err = transform(
      "import { vi } from 'vitest';\nvi.hoisted(() => { vi.mock('./a.ts'); });\n",
    )
    .unwrap_err();
    assert!(
      err.contains("Cannot call vi.mock(\"./a.ts\") inside vi.hoisted()"),
      "{err}"
    );
    let err = transform(
      "import { vi } from 'vitest';\nexport const x = vi.hoisted(() => 1);\n",
    )
    .unwrap_err();
    assert!(err.contains("Cannot export hoisted variable"), "{err}");
  }

  #[test]
  fn modules_without_mocks_are_not_hoisted() {
    let out =
      transform("import { a } from './a.ts';\nconsole.log(a);\n").unwrap();
    assert_eq!(out, "import { a } from './a.ts';\nconsole.log(a);\n");
  }

  #[test]
  fn mutable_exports() {
    let out = transform(
      r#"import { b as imported } from "./b.ts";
export const a = 1, z = 2;
export function f() {}
class C {}
const d = 4;
export { C, d as "e-f", imported };
export { x as y } from "./x.ts";
export * from "./star.ts";
export default function named() {}
"#,
    )
    .unwrap();
    assert!(out.contains("export let a = 1, z = 2;"), "{out}");
    assert!(out.contains("let d = 4;"), "{out}");
    assert!(
      out.contains("import * as __vitest_self__ from \"deno-vitest:///self\";"),
      "{out}"
    );
    assert!(
      out.contains("import * as __vitest_source_0__ from \"./b.ts\";"),
      "{out}"
    );
    assert!(
      out.contains("import * as __vitest_source_1__ from \"./x.ts\";"),
      "{out}"
    );
    for case in [
      "case \"a\":\n      a = __vitest_value__;",
      "case \"z\":\n      z = __vitest_value__;",
      "case \"f\":\n      f = __vitest_value__;",
      "case \"C\":\n      C = __vitest_value__;",
      "case \"e-f\":\n      d = __vitest_value__;",
      "case \"imported\":\n      return __vitest_set__(__vitest_source_0__, \"b\");",
      "case \"y\":\n      return __vitest_set__(__vitest_source_1__, \"x\");",
      "case \"default\":\n      named = __vitest_value__;",
      "if (__vitest_set__(__vitest_source_2__, __vitest_name__)) return true;",
    ] {
      assert!(out.contains(case), "missing {case:?} in {out}");
    }
  }

  #[test]
  fn modules_without_exports_are_unchanged() {
    let out = transform("const a = 1;\nconsole.log(a);\n").unwrap();
    assert_eq!(out, "const a = 1;\nconsole.log(a);\n");
  }
}
