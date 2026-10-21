// Copyright 2018-2026 the Deno authors. MIT license.

//! Module augmentations for the generated stock-TypeScript project (denext).
//!
//! A `declare module "denext" { ... }` block in a module file augments the
//! module `denext` resolves to. TypeScript 6 (the in-process checker) loads
//! that module into the program through Deno's resolver even when nothing
//! imports it; the native compiler (TypeScript 7) resolves the name through
//! the generated `paths` but does not load the file it finds, so it reports
//! TS2664 ("Invalid module name in augmentation") for a module the graph
//! never imports. `deno sync-types` therefore maps every name a project
//! augments (as it maps its imports) and writes `.deno/module_augmentations.d.ts`,
//! one `import type {} from "<name>"` per augmented name the generated config
//! resolves, listed in the config's `files`: the compiler then loads each
//! augmented module, as TypeScript 6 did.

use std::collections::BTreeSet;
use std::path::Path;

use deno_ast::MediaType;
use deno_ast::ModuleSpecifier;
use deno_ast::ProgramRef;
use deno_ast::swc::ast;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::serde_json::json;

/// The generated file, under the project's `.deno/`.
pub const AUGMENTATIONS_FILE: &str = "module_augmentations.d.ts";

/// The module names `source`'s top-level `declare module "<name>"` blocks
/// augment: only names a compiler resolves as a module (not a relative path,
/// a wildcard pattern or a `node:` builtin). A declaration file without an
/// import or export is a script, whose `declare module` blocks declare
/// ambient modules rather than augment one; any other file is a module (the
/// generated config sets `moduleDetection: "force"`).
pub fn module_augmentation_names(
  specifier: &ModuleSpecifier,
  media_type: MediaType,
  source: &str,
) -> Vec<String> {
  if !source.contains("declare") || !source.contains("module") {
    return Vec::new();
  }
  if !matches!(
    media_type,
    MediaType::TypeScript
      | MediaType::Mts
      | MediaType::Cts
      | MediaType::Tsx
      | MediaType::Dts
      | MediaType::Dmts
      | MediaType::Dcts
  ) {
    return Vec::new();
  }
  let Ok(parsed) = deno_ast::parse_module(deno_ast::ParseParams {
    specifier: specifier.clone(),
    text: source.into(),
    media_type,
    capture_tokens: false,
    maybe_syntax: None,
    scope_analysis: false,
  }) else {
    return Vec::new();
  };
  let ProgramRef::Module(module) = parsed.program_ref() else {
    return Vec::new();
  };
  let is_declaration_file = matches!(
    media_type,
    MediaType::Dts | MediaType::Dmts | MediaType::Dcts
  );
  if is_declaration_file
    && !module
      .body
      .iter()
      .any(|item| matches!(item, ast::ModuleItem::ModuleDecl(_)))
  {
    return Vec::new();
  }
  let mut names = Vec::new();
  for item in &module.body {
    let ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::TsModule(decl))) =
      item
    else {
      continue;
    };
    let ast::TsModuleName::Str(name) = &decl.id else {
      continue;
    };
    let name = name.value.to_string_lossy().into_owned();
    if is_resolvable_name(&name) && !names.contains(&name) {
      names.push(name);
    }
  }
  names
}

fn is_resolvable_name(name: &str) -> bool {
  !name.is_empty()
    && !name.contains('*')
    && !name.starts_with('.')
    && !name.starts_with('/')
    && !name.starts_with("node:")
}

/// The names the local modules of `graph` augment.
pub fn graph_augmentation_names(
  graph: &deno_graph::ModuleGraph,
) -> BTreeSet<String> {
  let mut names = BTreeSet::new();
  for module in graph.modules() {
    if let deno_graph::Module::Js(js) = module
      && js.specifier.scheme() == "file"
    {
      names.extend(module_augmentation_names(
        &js.specifier,
        js.media_type,
        &js.source.text,
      ));
    }
  }
  names
}

/// Whether the generated config's `paths` map `name` (an exact key, or a
/// `prefix/*` pattern it matches).
fn is_mapped(paths: &serde_json::Map<String, Value>, name: &str) -> bool {
  paths.keys().any(|key| match key.split_once('*') {
    None => key == name,
    Some((prefix, suffix)) => {
      name.len() >= prefix.len() + suffix.len()
        && name.starts_with(prefix)
        && name.ends_with(suffix)
    }
  })
}

/// Whether an npm package `name` (or `name/subpath`) is installed in the
/// project's `node_modules`, where the compiler finds it without `paths`.
fn is_installed(project_root: &Path, name: &str) -> bool {
  let mut segments = name.split('/');
  let package = match segments.next() {
    Some(scope) if scope.starts_with('@') => match segments.next() {
      Some(rest) => format!("{scope}/{rest}"),
      None => return false,
    },
    Some(package) => package.to_string(),
    None => return false,
  };
  project_root.join("node_modules").join(package).is_dir()
}

/// Write `.deno/module_augmentations.d.ts` for the augmented `names` the
/// generated config at `tsconfig_path` resolves, and list it in the config's
/// `files` (by absolute path: `deno check` copies the base config's `files`
/// into a config in the temp directory). Without any, the file is removed.
pub fn add_augmented_module_imports(
  project_root: &Path,
  tsconfig_path: &Path,
  names: &BTreeSet<String>,
) -> Result<(), AnyError> {
  let text = std::fs::read_to_string(tsconfig_path)?;
  let mut config: Value = serde_json::from_str(&text)?;
  let file = project_root.join(".deno").join(AUGMENTATIONS_FILE);
  let entry = file.to_string_lossy().replace('\\', "/");
  let paths = config
    .pointer("/compilerOptions/paths")
    .and_then(|p| p.as_object())
    .cloned()
    .unwrap_or_default();
  let resolved: Vec<&String> = names
    .iter()
    .filter(|name| is_mapped(&paths, name) || is_installed(project_root, name))
    .collect();
  let Some(root) = config.as_object_mut() else {
    return Ok(());
  };
  if resolved.is_empty() {
    let _ = std::fs::remove_file(&file);
    if let Some(files) = root.get_mut("files").and_then(|f| f.as_array_mut()) {
      let before = files.len();
      files.retain(|f| f.as_str() != Some(entry.as_str()));
      if files.len() != before {
        std::fs::write(tsconfig_path, serde_json::to_string_pretty(&config)?)?;
      }
    }
    return Ok(());
  }
  let mut content = String::from(
    "// Auto-generated by Deno: the modules this project augments (`declare module`),\n// loaded as TypeScript 6 loads them.\n",
  );
  for name in resolved {
    content.push_str(&format!(
      "import type {{}} from {};\n",
      serde_json::to_string(name)?
    ));
  }
  std::fs::write(&file, content)?;
  let files = root.entry("files").or_insert_with(|| json!([]));
  if let Some(files) = files.as_array_mut()
    && !files.iter().any(|f| f.as_str() == Some(entry.as_str()))
  {
    files.push(json!(entry));
    std::fs::write(tsconfig_path, serde_json::to_string_pretty(&config)?)?;
  }
  Ok(())
}

#[cfg(test)]
mod test {
  use super::*;

  fn names(path: &str, source: &str) -> Vec<String> {
    let specifier =
      ModuleSpecifier::parse(&format!("file:///p/{path}")).unwrap();
    let media_type = MediaType::from_specifier(&specifier);
    module_augmentation_names(&specifier, media_type, source)
  }

  #[test]
  fn augmentations_are_top_level_declare_module_blocks() {
    assert_eq!(
      names(
        "routes.ts",
        "export type R = '/';\ndeclare module \"denext\" {\n  interface X { r: R }\n}\ndeclare module 'jsr:@std/assert' {}\ndeclare module \"denext\" {}\n",
      ),
      vec!["denext".to_string(), "jsr:@std/assert".to_string()]
    );
    // a module file without imports is still a module (`moduleDetection: "force"`)
    assert_eq!(
      names("a.ts", "declare module \"lib/sub\" {}\n"),
      vec!["lib/sub".to_string()]
    );
  }

  #[test]
  fn ambient_declarations_and_patterns_are_not_augmentations() {
    // a declaration file with no import or export declares ambient modules
    assert!(names("globals.d.ts", "declare module \"foo\" {}\n").is_empty());
    assert_eq!(
      names("aug.d.ts", "export {};\ndeclare module \"foo\" {}\n"),
      vec!["foo".to_string()]
    );
    assert!(
      names(
        "a.ts",
        "declare module \"*.css\" {}\ndeclare module \"./local.ts\" {}\ndeclare module \"node:fs\" {}\nnamespace N {}\ndeclare module Legacy {}\n// declare module \"commented\" {}\nconst s = 'declare module \"str\" {}';\n",
      )
      .is_empty()
    );
    // not TypeScript
    assert!(names("a.js", "declare module \"foo\" {}\n").is_empty());
  }

  #[test]
  fn imports_only_the_names_the_config_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".deno")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/@scope/pkg")).unwrap();
    let tsconfig = root.join(".deno/tsconfig.json");
    std::fs::write(
      &tsconfig,
      r#"{ "compilerOptions": { "paths": { "denext": ["../denext.ts"], "lib/*": ["../lib/*"] } }, "files": ["/x/a.d.ts"] }"#,
    )
    .unwrap();
    let names: BTreeSet<String> =
      ["denext", "lib/sub.ts", "@scope/pkg/sub", "unknown"]
        .into_iter()
        .map(String::from)
        .collect();
    add_augmented_module_imports(root, &tsconfig, &names).unwrap();
    let file = root.join(".deno").join(AUGMENTATIONS_FILE);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("import type {} from \"denext\";\n"));
    assert!(content.contains("import type {} from \"lib/sub.ts\";\n"));
    assert!(content.contains("import type {} from \"@scope/pkg/sub\";\n"));
    assert!(!content.contains("unknown"), "{content}");
    let config: Value =
      serde_json::from_str(&std::fs::read_to_string(&tsconfig).unwrap())
        .unwrap();
    let entry = file.to_string_lossy().replace('\\', "/");
    assert_eq!(config["files"], json!(["/x/a.d.ts", entry]));

    // no augmentation left: the file and its entry go
    add_augmented_module_imports(root, &tsconfig, &BTreeSet::new()).unwrap();
    assert!(!file.exists());
    let config: Value =
      serde_json::from_str(&std::fs::read_to_string(&tsconfig).unwrap())
        .unwrap();
    assert_eq!(config["files"], json!(["/x/a.d.ts"]));
  }
}
