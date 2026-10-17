// Copyright 2018-2026 the Deno authors. MIT license.

//! `deno lsp` on the TypeScript 7 (tsgo) language-service backend (denext,
//! unstable): the editor features, ported from the matching
//! `lsp_tests.rs` cases (named on each test) and adapted to tsgo's output,
//! plus the Deno-specific resolution cases (import map, `jsr:`, `npm:`,
//! Deno's lib types) that go through `deno sync-types`.
//!
//! Run with `cargo test -p integration_tests --test integration -- lsp_tsgo`; the harness
//! fetches the pinned tsgo build into `target/` on first use
//! (`test_util::lsp::tsgo_lsp_bin`). `lsp_tsgo_perf_vs_ts6` is a timing
//! comparison with the TypeScript 6 service that only runs with
//! `DENO_TSGO_LSP_PERF=1`.

use std::time::Duration;
use std::time::Instant;

use serde_json::Value;
use serde_json::json;
use test_util::TestContext;
use test_util::TestContextBuilder;
use test_util::eprintln;
use test_util::lsp::LspClient;
use test_util::test;
use test_util::url_to_uri;
use tower_lsp::lsp_types as lsp;

fn tsgo_client(context: &TestContext) -> LspClient {
  let mut client = context.new_lsp_command().set_use_tsgo(true).build();
  client.initialize_default();
  client
}

fn hover_text(res: &Value) -> String {
  res
    .pointer("/contents/value")
    .and_then(|v| v.as_str())
    .unwrap_or_default()
    .to_string()
}

fn deno_ts_messages(diagnostics: &[lsp::Diagnostic]) -> Vec<String> {
  diagnostics
    .iter()
    .filter(|d| d.source.as_deref() == Some("deno-ts"))
    .map(|d| d.message.clone())
    .collect()
}

#[test(timeout = 300)]
fn lsp_tsgo_startup_shutdown() {
  // lsp_tests.rs: lsp_startup_shutdown
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file("a.ts", "export const a = 1;\n");
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  client.shutdown();
  assert!(client.wait_exit().unwrap().success());
}

#[test(timeout = 300)]
fn lsp_tsgo_selected_by_editor_setting() {
  // `"deno.unstable": ["tsgo-lsp"]` instead of the environment variable. The
  // two backends render hover code blocks differently (` ```tsx ` for the
  // in-isolate service, ` ```typescript ` for tsgo).
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file("a.ts", "const value = 1;\nvalue;\n");
  let mut client = context
    .new_lsp_command()
    .env(
      "DENO_TSGO_LSP_BIN",
      test_util::lsp::tsgo_lsp_bin().to_string(),
    )
    .build();
  client.initialize(|builder| {
    builder.set_unstable_features(&["tsgo-lsp"]);
  });
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/hover",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("value;").start,
    }),
  );
  let text = hover_text(&res);
  assert!(text.starts_with("```typescript\n"), "{text}");
  assert!(text.contains("const value: 1"), "{text}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_diagnostics_merge_with_deno_lint() {
  // lsp_tests.rs: lsp_diagnostics_deprecated / lsp_doc_lint_diagnostics —
  // TypeScript errors now come from tsgo, attributed to `deno-ts`, next to
  // Deno's own lint diagnostics.
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "let unchanged = 1;\nconst s: string = 1;\nconsole.log(unchanged, s);\n",
  );
  let mut client = tsgo_client(&context);
  let diagnostics = client.did_open_file(&file);
  let all = diagnostics.all();
  let ts = all
    .iter()
    .find(|d| d.source.as_deref() == Some("deno-ts"))
    .unwrap_or_else(|| panic!("no deno-ts diagnostic: {all:#?}"));
  assert_eq!(ts.code, Some(lsp::NumberOrString::Number(2322)));
  assert_eq!(
    ts.message,
    "Type 'number' is not assignable to type 'string'."
  );
  assert_eq!(ts.range.start, lsp::Position::new(1, 6));
  assert!(
    all.iter().any(|d| d.source.as_deref() == Some("deno-lint")
      && d.code == Some(lsp::NumberOrString::String("prefer-const".into()))),
    "{all:#?}"
  );

  // An edit is mirrored to tsgo before the next pull.
  client.write_notification(
    "textDocument/didChange",
    json!({
      "textDocument": { "uri": file.uri(), "version": 2 },
      "contentChanges": [{
        "range": {
          "start": { "line": 1, "character": 18 },
          "end": { "line": 1, "character": 19 },
        },
        "text": "\"1\"",
      }],
    }),
  );
  let diagnostics = client.read_diagnostics();
  assert_eq!(deno_ts_messages(&diagnostics.all()), Vec::<String>::new());
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_hover() {
  // lsp_tests.rs: lsp_hover (on a project symbol; Deno's lib types are
  // covered by lsp_tsgo_deno_lib_types).
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "/** Adds two numbers. */\nexport function add(a: number, b: number) {\n  return a + b;\n}\nadd(1, 2);\n",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/hover",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("add(1").start,
    }),
  );
  let text = hover_text(&res);
  assert!(
    text.contains("function add(a: number, b: number): number"),
    "{text}"
  );
  assert!(text.contains("Adds two numbers."), "{text}");
  assert_eq!(
    res["range"],
    json!({
      "start": { "line": 4, "character": 0 },
      "end": { "line": 4, "character": 3 },
    })
  );
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_goto_definition_and_type_definition() {
  // lsp_tests.rs: lsp_cache_then_definition / lsp_goto_type_definition, on a
  // relative import of a closed file.
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  temp_dir.write(
    "lib.ts",
    "export interface Point {\n  x: number;\n}\nexport const origin: Point = { x: 0 };\n",
  );
  let file = temp_dir.source_file(
    "main.ts",
    "import { origin } from \"./lib.ts\";\nconsole.log(origin.x);\n",
  );
  let lib_uri = url_to_uri(&temp_dir.url().join("lib.ts").unwrap()).unwrap();
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/definition",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("origin.x").start,
    }),
  );
  let locations = res.as_array().unwrap();
  let first = &locations[0];
  let target = first
    .get("targetUri")
    .or_else(|| first.get("uri"))
    .and_then(|u| u.as_str())
    .unwrap();
  assert_eq!(target, lib_uri.as_str(), "{res}");
  let range = first
    .get("targetSelectionRange")
    .or_else(|| first.get("range"))
    .unwrap();
  assert_eq!(
    range["start"],
    json!({ "line": 3, "character": 13 }),
    "{res}"
  );

  let res = client.write_request(
    "textDocument/typeDefinition",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("origin.x").start,
    }),
  );
  let first = &res.as_array().unwrap()[0];
  let range = first
    .get("targetSelectionRange")
    .or_else(|| first.get("range"))
    .unwrap();
  assert_eq!(range["start"]["line"], json!(0), "{res}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_find_references() {
  // lsp_tests.rs: lsp_find_references / lsp_closed_file_find_references
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  temp_dir.write("mod.ts", "export const a = 1;\nconst b = a;\n");
  let file = temp_dir.source_file(
    "importer.ts",
    "import { a } from './mod.ts';\nconsole.log(a);\n",
  );
  let mod_uri = url_to_uri(&temp_dir.url().join("mod.ts").unwrap()).unwrap();
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/references",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("a);").start,
      "context": { "includeDeclaration": true },
    }),
  );
  let mut found = res
    .as_array()
    .unwrap()
    .iter()
    .map(|l| {
      (
        l["uri"].as_str().unwrap().to_string(),
        l["range"]["start"]["line"].as_u64().unwrap(),
      )
    })
    .collect::<Vec<_>>();
  found.sort();
  let importer = file.uri().as_str().to_string();
  let module = mod_uri.as_str().to_string();
  let mut expected = vec![
    (importer.clone(), 0),
    (importer, 1),
    (module.clone(), 0),
    (module, 1),
  ];
  expected.sort();
  assert_eq!(found, expected, "{res}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_rename() {
  // lsp_tests.rs: lsp_rename
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "let variable = 'a'; // only out of two\nconsole.log(variable);\n",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/rename",
    json!({
      "textDocument": file.identifier(),
      "position": { "line": 0, "character": 4 },
      "newName": "variable_modified",
    }),
  );
  let edits = res
    .pointer(&format!(
      "/changes/{}",
      json_pointer_escape(file.uri().as_str())
    ))
    .or_else(|| {
      res.pointer("/documentChanges/0/edits").filter(|_| {
        res["documentChanges"][0]["textDocument"]["uri"] == json!(file.uri())
      })
    })
    .unwrap_or_else(|| panic!("{res}"));
  let mut ranges = edits
    .as_array()
    .unwrap()
    .iter()
    .map(|e| {
      assert_eq!(e["newText"], json!("variable_modified"));
      (
        e["range"]["start"]["line"].as_u64().unwrap(),
        e["range"]["start"]["character"].as_u64().unwrap(),
      )
    })
    .collect::<Vec<_>>();
  ranges.sort();
  assert_eq!(ranges, vec![(0, 4), (1, 12)]);
  client.shutdown();
}

fn json_pointer_escape(s: &str) -> String {
  s.replace('~', "~0").replace('/', "~1")
}

#[test(timeout = 300)]
fn lsp_tsgo_completions_and_resolve() {
  // lsp_tests.rs: lsp_completions
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "const point = { x: 1, longName: 'a' };\npoint.\n",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let list = client.get_completion_list(
    file.uri().as_str(),
    (1, 6),
    json!({ "triggerKind": 2, "triggerCharacter": "." }),
  );
  let item = list
    .items
    .iter()
    .find(|i| i.label == "longName")
    .unwrap_or_else(|| panic!("{:#?}", list.items));
  assert!(list.items.iter().any(|i| i.label == "x"));
  // Deno wraps tsgo's item data so the resolve goes back to tsgo.
  let data = item.data.clone().unwrap();
  assert!(data.get("tsGo").is_some(), "{data}");
  let resolved = client.write_request(
    "completionItem/resolve",
    serde_json::to_value(item).unwrap(),
  );
  assert_eq!(resolved["label"], json!("longName"));
  let detail = resolved["detail"].as_str().unwrap_or_default();
  assert!(detail.contains("longName: string"), "{resolved}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_signature_help() {
  // lsp_tests.rs: lsp_signature_help
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "/**\n * Adds two numbers.\n * @param a This is a first number.\n * @param b This is a second number.\n */\nfunction add(a: number, b: number) {\n  return a + b;\n}\n\nadd(",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/signatureHelp",
    json!({
      "textDocument": file.identifier(),
      "position": { "character": 4, "line": 9 },
      "context": {
        "triggerKind": 2,
        "triggerCharacter": "(",
        "isRetrigger": false,
      },
    }),
  );
  let signature = &res["signatures"][0];
  assert_eq!(
    signature["label"],
    json!("add(a: number, b: number): number")
  );
  let doc = signature
    .pointer("/documentation/value")
    .or_else(|| signature.get("documentation"))
    .and_then(|d| d.as_str())
    .unwrap_or_default();
  assert!(doc.contains("Adds two numbers."), "{res}");
  let params = signature["parameters"].as_array().unwrap();
  assert_eq!(params.len(), 2);
  let active = res
    .get("activeParameter")
    .or_else(|| signature.get("activeParameter"))
    .and_then(|v| v.as_u64());
  assert_eq!(active, Some(0), "{res}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_code_action_quick_fix() {
  // lsp_tests.rs: lsp_code_actions (a TypeScript quick fix for a `deno-ts`
  // diagnostic: "Did you mean ...?").
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file = temp_dir.source_file(
    "file.ts",
    "const someLongName = 1;\nconsole.log(someLongNam);\n",
  );
  let mut client = tsgo_client(&context);
  let diagnostics = client.did_open_file(&file);
  let diagnostic = diagnostics
    .all()
    .into_iter()
    .find(|d| d.code == Some(lsp::NumberOrString::Number(2552)))
    .unwrap_or_else(|| panic!("{:#?}", diagnostics.all()));
  let res = client.write_request(
    "textDocument/codeAction",
    json!({
      "textDocument": file.identifier(),
      "range": diagnostic.range,
      "context": { "diagnostics": [diagnostic], "only": ["quickfix"] },
    }),
  );
  let actions = res.as_array().unwrap_or_else(|| panic!("{res}"));
  let spelling = actions
    .iter()
    .find(|a| {
      a["title"]
        .as_str()
        .is_some_and(|t| t.contains("someLongName"))
    })
    .unwrap_or_else(|| panic!("{res}"));
  let action = if spelling.get("edit").is_some() {
    spelling.clone()
  } else {
    client.write_request("codeAction/resolve", spelling.clone())
  };
  let edit = action
    .pointer(&format!(
      "/edit/changes/{}/0",
      json_pointer_escape(file.uri().as_str())
    ))
    .or_else(|| action.pointer("/edit/documentChanges/0/edits/0"))
    .unwrap_or_else(|| panic!("{action}"));
  assert_eq!(edit["newText"], json!("someLongName"));
  assert_eq!(
    edit["range"]["start"],
    json!({ "line": 1, "character": 12 })
  );
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_organize_imports() {
  // lsp_tests.rs: lsp_code_actions_organize_imports
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  temp_dir.write("a.ts", "export const d = 1;\n");
  temp_dir.write("b.ts", "export const a = 1, b = 2, c = 3;\n");
  temp_dir.write("c.ts", "export default 1;\n");
  temp_dir.write("z.ts", "export const y = 1, z = 2;\n");
  let file = temp_dir.source_file(
    "file.ts",
    r#"import { z, y } from "./z.ts";
import { c, a, b } from "./b.ts";
import { d } from "./a.ts";
import unused from "./c.ts";

console.log(b, a, c, d, y, z);
"#,
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/codeAction",
    json!({
      "textDocument": file.identifier(),
      "range": {
        "start": { "line": 0, "character": 0 },
        "end": { "line": 0, "character": 0 },
      },
      "context": { "diagnostics": [], "only": ["source.organizeImports"] },
    }),
  );
  let action = &res.as_array().unwrap_or_else(|| panic!("{res}"))[0];
  assert!(
    action["kind"]
      .as_str()
      .unwrap()
      .starts_with("source.organizeImports"),
    "{res}"
  );
  let edits = action
    .pointer(&format!(
      "/edit/changes/{}",
      json_pointer_escape(file.uri().as_str())
    ))
    .unwrap_or_else(|| panic!("{res}"));
  let new_text = edits
    .as_array()
    .unwrap()
    .iter()
    .map(|e| e["newText"].as_str().unwrap())
    .collect::<String>();
  assert_eq!(
    new_text,
    concat!(
      "import { d } from \"./a.ts\";\n",
      "import { a, b, c } from \"./b.ts\";\n",
      "import { y, z } from \"./z.ts\";\n",
    )
  );
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_keeps_deno_test_code_lens() {
  // lsp_tests.rs: lsp_code_lens_test — Deno's own features stay on.
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  let file =
    temp_dir.source_file("file_test.ts", "Deno.test(\"adds\", () => {});\n");
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  let res = client.write_request(
    "textDocument/codeLens",
    json!({ "textDocument": file.identifier() }),
  );
  let titles = res
    .as_array()
    .unwrap()
    .iter()
    .filter_map(|l| l.pointer("/command/title").and_then(|t| t.as_str()))
    .collect::<Vec<_>>();
  assert!(titles.iter().any(|t| t.contains("Run Test")), "{res}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_import_map_specifiers() {
  // lsp_tests.rs: lsp_hover_dependency_import_map — through
  // `deno sync-types`'s `paths`, an import-map alias resolves for tsgo:
  // types flow, go-to-definition lands in the aliased file, and hovering
  // the specifier itself is still Deno's own dependency hover.
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  temp_dir.write(
    "deno.json",
    r#"{ "imports": { "foo": "./lib/foo.ts", "bar/": "./lib/bar/" } }"#,
  );
  temp_dir.write("lib/foo.ts", "export const foo: number = 1;\n");
  temp_dir.write("lib/bar/baz.ts", "export const baz = \"two\";\n");
  let file = temp_dir.source_file(
    "main.ts",
    "import { foo } from \"foo\";\nimport { baz } from \"bar/baz.ts\";\nconst n: string = foo;\nconsole.log(n, baz);\n",
  );
  let mut client = tsgo_client(&context);
  let diagnostics = client.did_open_file(&file);
  // The alias resolved: the only TypeScript error is the real one.
  assert_eq!(
    deno_ts_messages(&diagnostics.all()),
    vec!["Type 'number' is not assignable to type 'string'.".to_string()]
  );
  let res = client.write_request(
    "textDocument/definition",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("baz);").start,
    }),
  );
  let target = res[0]
    .get("targetUri")
    .or_else(|| res[0].get("uri"))
    .and_then(|u| u.as_str())
    .unwrap_or_else(|| panic!("{res}"));
  assert!(target.ends_with("/lib/bar/baz.ts"), "{res}");
  let res = client.write_request(
    "textDocument/hover",
    json!({
      "textDocument": file.identifier(),
      "position": { "line": 0, "character": 22 },
    }),
  );
  assert!(hover_text(&res).contains("**Import Map**"), "{res}");
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_jsr_specifier() {
  // lsp_tests.rs: lsp_hover_jsr — a `jsr:` import's types after
  // `deno.cache` (which re-runs `deno sync-types`).
  let context = TestContextBuilder::new()
    .use_http_server()
    .use_temp_cwd()
    .build();
  let temp_dir = context.temp_dir();
  temp_dir.write("deno.json", "{}\n");
  let file = temp_dir.source_file(
    "file.ts",
    "import { add } from \"jsr:@denotest/add@1.0.0\";\nconst n: string = add(1, 2);\n",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  client.cache_specifier(file.url());
  let diagnostics = client.read_diagnostics();
  assert_eq!(
    deno_ts_messages(&diagnostics.all()),
    vec!["Type 'number' is not assignable to type 'string'.".to_string()]
  );
  let res = client.write_request(
    "textDocument/hover",
    json!({
      "textDocument": file.identifier(),
      "position": file.range_of("add(1").start,
    }),
  );
  assert!(
    hover_text(&res).contains("add(a: number, b: number): number"),
    "{res}"
  );
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_npm_specifier() {
  // lsp_tests.rs: lsp_completions_npm — an `npm:` import's types after
  // `deno.cache`.
  let context = TestContextBuilder::new()
    .use_http_server()
    .use_temp_cwd()
    .build();
  let temp_dir = context.temp_dir();
  temp_dir.write("deno.json", "{}\n");
  let file = temp_dir.source_file(
    "file.ts",
    "import { add } from \"npm:@denotest/add@1\";\nconst n: string = add(1, 2);\n",
  );
  let mut client = tsgo_client(&context);
  client.did_open_file(&file);
  client.cache_specifier(file.url());
  let diagnostics = client.read_diagnostics();
  assert_eq!(
    deno_ts_messages(&diagnostics.all()),
    vec!["Type 'number' is not assignable to type 'string'.".to_string()]
  );
  client.shutdown();
}

#[test(timeout = 300)]
fn lsp_tsgo_deno_lib_types() {
  // lsp_tests.rs: lsp_hover (`Deno.args`), plus this fork's own lib types:
  // `Deno.SQL` (lib.deno_sql.d.ts) must be known to tsgo too.
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let temp_dir = context.temp_dir();
  temp_dir.write("deno.json", "{}\n");
  let file = temp_dir.source_file(
    "file.ts",
    "console.log(Deno.args);\nconst sql: typeof Deno.SQL = Deno.SQL;\nconsole.log(sql);\n",
  );
  let mut client = tsgo_client(&context);
  let diagnostics = client.did_open_file(&file);
  assert_eq!(deno_ts_messages(&diagnostics.all()), Vec::<String>::new());
  let res = client.write_request(
    "textDocument/hover",
    json!({
      "textDocument": file.identifier(),
      "position": { "line": 0, "character": 19 },
    }),
  );
  let text = hover_text(&res);
  assert!(text.contains("Deno.args: string[]"), "{text}");
  client.shutdown();
}

/// Time to the first TypeScript diagnostics and the median completion latency
/// for one backend on `std`'s `http/file_server.ts` (a medium graph: ~130
/// modules of `tests/util/std`).
fn measure_backend(use_tsgo: bool) -> (Duration, Duration, Duration) {
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let std_dir = test_util::root_path().join("tests/util/std");
  let file_path = std_dir.join("http/file_server.ts");
  let text = file_path.read_to_string();
  let uri = file_path.uri_file();
  let started = Instant::now();
  let mut client = context
    .new_lsp_command()
    .set_use_tsgo(use_tsgo)
    .env("DENO_TSGO_LSP_SYNC_TYPES", "0")
    .set_root_dir(std_dir.clone())
    .build();
  client.initialize(|builder| {
    builder.set_root_uri(std_dir.uri_dir());
    builder.set_workspace_folders(vec![lsp::WorkspaceFolder {
      uri: std_dir.uri_dir(),
      name: "std".to_string(),
    }]);
  });
  client.did_open(json!({
    "textDocument": {
      "uri": uri,
      "languageId": "typescript",
      "version": 1,
      "text": text,
    }
  }));
  let first_diagnostics = started.elapsed();
  // Completion on a member access deep in the file.
  let line = text.lines().count() as u32;
  client.write_notification(
    "textDocument/didChange",
    json!({
      "textDocument": { "uri": uri, "version": 2 },
      "contentChanges": [{
        "range": {
          "start": { "line": line, "character": 0 },
          "end": { "line": line, "character": 0 },
        },
        "text": "\nDeno.",
      }],
    }),
  );
  client.read_diagnostics();
  let mut samples = Vec::new();
  for _ in 0..15 {
    let t = Instant::now();
    let res = client.write_request(
      "textDocument/completion",
      json!({
        "textDocument": { "uri": uri },
        "position": { "line": line + 1, "character": 5 },
        "context": { "triggerKind": 2, "triggerCharacter": "." },
      }),
    );
    samples.push(t.elapsed());
    assert!(!res.is_null());
  }
  samples.sort();
  let first_completion = samples[samples.len() - 1];
  let median_completion = samples[samples.len() / 2];
  client.shutdown();
  (first_diagnostics, first_completion, median_completion)
}

#[test(timeout = 900)]
fn lsp_tsgo_perf_vs_ts6() {
  // A timing report, not an assertion: opt in with DENO_TSGO_LSP_PERF=1.
  if std::env::var_os("DENO_TSGO_LSP_PERF").is_none() {
    return;
  }
  let (ts6_diag, ts6_slowest, ts6_median) = measure_backend(false);
  let (go_diag, go_slowest, go_median) = measure_backend(true);
  eprintln!(
    "\nbackend  first-diagnostics  completion-median  completion-slowest\n\
     ts6      {:>14.0?}  {:>17.1?}  {:>18.1?}\n\
     tsgo     {:>14.0?}  {:>17.1?}  {:>18.1?}",
    ts6_diag, ts6_median, ts6_slowest, go_diag, go_median, go_slowest
  );
}
