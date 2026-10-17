// Copyright 2018-2026 the Deno authors. MIT license.

//! Node's retry as ES module for an ambiguous file that only an ES module can
//! run.
//!
//! Node (`--experimental-detect-module`, the default since 22.7) compiles an
//! ambiguous file as CommonJS first. When that fails with one of the errors
//! that only CommonJS throws, it runs the file as an ES module instead:
//!
//! - a top-level `let` / `const` / `class` (or `using`) declaration of one of
//!   the CommonJS wrapper's parameters (`require`, `module`, `exports`,
//!   `__filename`, `__dirname`): "Identifier 'require' has already been
//!   declared";
//! - an `await` outside any function: "await is only valid in async
//!   functions and the top level bodies of modules".
//!
//! swc parses such a file as a script when its `await` sits in a nested
//! top-level block (`if (x) { await y; }`, a `for` body), so the parser's own
//! script/module verdict misses it; this walks the program instead.

use deno_ast::MediaType;
use deno_ast::ModuleSpecifier;
use deno_ast::ParseParams;
use deno_ast::ProgramRef;
use deno_ast::swc::ast::ArrowExpr;
use deno_ast::swc::ast::AwaitExpr;
use deno_ast::swc::ast::Class;
use deno_ast::swc::ast::Constructor;
use deno_ast::swc::ast::Decl;
use deno_ast::swc::ast::ForOfStmt;
use deno_ast::swc::ast::Function;
use deno_ast::swc::ast::GetterProp;
use deno_ast::swc::ast::ModuleItem;
use deno_ast::swc::ast::ObjectPatProp;
use deno_ast::swc::ast::Pat;
use deno_ast::swc::ast::SetterProp;
use deno_ast::swc::ast::StaticBlock;
use deno_ast::swc::ast::Stmt;
use deno_ast::swc::ast::UsingDecl;
use deno_ast::swc::ast::VarDeclKind;
use deno_ast::swc::ecma_visit::Visit;
use deno_ast::swc::ecma_visit::VisitWith;

/// The parameters of Node's CommonJS module wrapper.
const WRAPPER_NAMES: &[&str] =
  &["require", "module", "exports", "__filename", "__dirname"];

/// Whether `text` would fail to compile as CommonJS only because it is an ES
/// module (see the module docs). A file that doesn't parse is left to the
/// loader to report.
pub fn is_esm_only(
  specifier: &ModuleSpecifier,
  media_type: MediaType,
  text: &str,
) -> bool {
  // Both checks need the word in the text; skip the parse otherwise.
  let maybe_tla = text.contains("await");
  let maybe_redeclared = WRAPPER_NAMES.iter().any(|name| text.contains(name));
  if !maybe_tla && !maybe_redeclared {
    return false;
  }
  let Ok(parsed) = deno_ast::parse_program(ParseParams {
    specifier: specifier.clone(),
    text: text.into(),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  }) else {
    return false;
  };
  match parsed.program_ref() {
    ProgramRef::Script(script) => stmts_are_esm_only(script.body.iter()),
    ProgramRef::Module(module) => {
      stmts_are_esm_only(module.body.iter().filter_map(|item| match item {
        ModuleItem::Stmt(stmt) => Some(stmt),
        ModuleItem::ModuleDecl(_) => None,
      }))
    }
  }
}

fn stmts_are_esm_only<'a>(stmts: impl Iterator<Item = &'a Stmt>) -> bool {
  let mut finder = TopLevelAwaitFinder { found: false };
  for stmt in stmts {
    if redeclares_wrapper_name(stmt) {
      return true;
    }
    stmt.visit_with(&mut finder);
    if finder.found {
      return true;
    }
  }
  false
}

/// A top-level declaration that CommonJS's wrapper function rejects: a
/// lexical binding of one of its parameters (`var` and `function` may
/// redeclare a parameter).
fn redeclares_wrapper_name(stmt: &Stmt) -> bool {
  let Stmt::Decl(decl) = stmt else {
    return false;
  };
  match decl {
    Decl::Var(var) => {
      var.kind != VarDeclKind::Var
        && var.decls.iter().any(|d| pat_binds_wrapper_name(&d.name))
    }
    Decl::Using(using) => {
      using.decls.iter().any(|d| pat_binds_wrapper_name(&d.name))
    }
    Decl::Class(class) => is_wrapper_name(&class.ident.sym),
    _ => false,
  }
}

fn is_wrapper_name(name: &str) -> bool {
  WRAPPER_NAMES.contains(&name)
}

fn pat_binds_wrapper_name(pat: &Pat) -> bool {
  match pat {
    Pat::Ident(ident) => is_wrapper_name(&ident.id.sym),
    Pat::Array(array) => {
      array.elems.iter().flatten().any(pat_binds_wrapper_name)
    }
    Pat::Object(object) => object.props.iter().any(|prop| match prop {
      ObjectPatProp::KeyValue(kv) => pat_binds_wrapper_name(&kv.value),
      ObjectPatProp::Assign(assign) => is_wrapper_name(&assign.key.sym),
      ObjectPatProp::Rest(rest) => pat_binds_wrapper_name(&rest.arg),
    }),
    Pat::Assign(assign) => pat_binds_wrapper_name(&assign.left),
    Pat::Rest(rest) => pat_binds_wrapper_name(&rest.arg),
    Pat::Expr(_) | Pat::Invalid(_) => false,
  }
}

/// Finds an `await` (or `for await`, `await using`) outside every function,
/// at any block depth.
struct TopLevelAwaitFinder {
  found: bool,
}

impl Visit for TopLevelAwaitFinder {
  fn visit_await_expr(&mut self, _: &AwaitExpr) {
    self.found = true;
  }

  fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
    if stmt.is_await {
      self.found = true;
    } else {
      stmt.visit_children_with(self);
    }
  }

  fn visit_using_decl(&mut self, using: &UsingDecl) {
    if using.is_await {
      self.found = true;
    } else {
      using.visit_children_with(self);
    }
  }

  // Function bodies have their own `await` rules.
  fn visit_function(&mut self, _: &Function) {}
  fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
  fn visit_constructor(&mut self, _: &Constructor) {}
  fn visit_getter_prop(&mut self, _: &GetterProp) {}
  fn visit_setter_prop(&mut self, _: &SetterProp) {}
  fn visit_static_block(&mut self, _: &StaticBlock) {}
  // Class bodies: methods are functions, and an `await` in a field
  // initializer is a syntax error either way. Computed keys are rare enough
  // to leave out.
  fn visit_class(&mut self, _: &Class) {}
}

#[cfg(test)]
mod test {
  use super::*;

  fn esm_only(text: &str) -> bool {
    let specifier = ModuleSpecifier::parse("file:///a.js").unwrap();
    is_esm_only(&specifier, MediaType::JavaScript, text)
  }

  #[test]
  fn redeclared_wrapper_names() {
    assert!(esm_only("const require = (m) => m;\nrequire('x');"));
    assert!(esm_only("let module = {};"));
    assert!(esm_only("class exports {}"));
    assert!(esm_only("const { __dirname } = globalThis;"));
    assert!(esm_only("const [a, __filename] = [1, 2];"));
    assert!(esm_only("const { a: { b: require } } = x;"));
    // `var` and `function` may redeclare a parameter
    assert!(!esm_only("var require = 1;"));
    assert!(!esm_only("function require() {}"));
    // a nested scope is not the wrapper's scope
    assert!(!esm_only("{ const require = 1; }"));
    assert!(!esm_only("function f() { const module = 1; }"));
    assert!(!esm_only("const fs = require('fs');"));
  }

  #[test]
  fn top_level_await_at_any_block_depth() {
    assert!(esm_only("await 1;"));
    assert!(esm_only("{ await 1; }"));
    assert!(esm_only("if (true) {\n  await Promise.resolve(1);\n}"));
    assert!(esm_only("for (const x of [1]) { await x; }"));
    assert!(esm_only("try { await f(); } catch {}"));
    assert!(esm_only("for await (const x of y) {}"));
    assert!(esm_only("const v = cond ? await a : b;"));
    assert!(!esm_only("async function f() { await 1; }"));
    assert!(!esm_only("const f = async () => { await 1; };"));
    assert!(!esm_only("const o = { async m() { await 1; } };"));
    assert!(!esm_only("class A { async m() { await 1; } }"));
    assert!(!esm_only("const await_ = 1; module.exports = await_;"));
  }
}
