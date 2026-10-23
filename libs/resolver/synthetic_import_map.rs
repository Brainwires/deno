// Copyright 2018-2026 the Deno authors. MIT license.

//! The workspace's synthetic import map: the root config's import map with
//! every other config (workspace members, `links` packages, auto-linked config
//! folders) layered in as a scope of its own.
//!
//! This is `import_map::ext::create_synthetic_import_map` (import_map 0.25.0)
//! with one change: a path is written relative to the base map's directory
//! only when that relative form resolves back to the same URL. The crate used
//! `Url::make_relative` unconditionally, and `make_relative` knows nothing of
//! Windows drives: from `file:///C:/root/` it spells `file:///D:/x` as
//! `../D:/x`, which resolves to `file:///C:/D:/x`. Every value, the member's
//! scope key and each rewritten scope key broke that way, so a workspace
//! member, a linked package or an auto-linked folder on another drive, or a
//! member on the root's drive mapping a module on another drive, silently
//! resolved to a path that doesn't exist. Across drives (compared without
//! regard to case), hosts (UNC shares) or anywhere the relative form would not
//! round-trip, the absolute URL is written instead.

use import_map::ext::ImportMapConfig;
use serde_json::Map;
use serde_json::Value;
use url::Url;

/// Builds the synthetic import map. Returns the base map's URL (the map's base)
/// and its value.
pub fn create_synthetic_import_map(
  mut base_import_map: ImportMapConfig,
  children_import_maps: Vec<ImportMapConfig>,
) -> (Url, Value) {
  let mut synth_imports = Map::new();
  let mut synth_scopes = Map::new();

  let base_dir = pop_last_segment(&base_import_map.base_url);

  if let Value::Object(base_value) = &mut base_import_map.import_map_value {
    if let Some(Value::Object(base_imports)) = base_value.remove("imports") {
      synth_imports = base_imports;
    }
    if let Some(Value::Object(base_scopes)) = base_value.remove("scopes") {
      synth_scopes = base_scopes;
    }
  }

  for child in children_import_maps {
    let member_dir = pop_last_segment(&child.base_url);
    let Value::Object(mut import_map_obj) = child.import_map_value else {
      continue;
    };

    let mut member_scope = Map::new();
    if let Some(Value::Object(imports)) = import_map_obj.remove("imports") {
      rebase_specifier_map(&base_dir, &member_dir, imports, &mut member_scope);
    }
    combine_object(
      &mut synth_scopes,
      scope_key(&base_dir, &member_dir),
      Value::Object(member_scope),
    );

    if let Some(Value::Object(scopes)) = import_map_obj.remove("scopes") {
      for (scope_name, scope_obj) in scopes {
        // A scope key such as "./fizz/" is the member's own; key it by its
        // place relative to the base map instead.
        let Ok(scope_dir) = member_dir.join(&scope_name) else {
          combine_object(&mut synth_scopes, scope_name, scope_obj);
          continue;
        };
        if scope_dir.scheme() != "file" {
          // remote scopes are kept as written
          combine_object(&mut synth_scopes, scope_name, scope_obj);
          continue;
        }
        let mut new_scope = Map::new();
        if let Value::Object(scope_obj) = scope_obj {
          rebase_specifier_map(
            &base_dir,
            &member_dir,
            scope_obj,
            &mut new_scope,
          );
        }
        combine_object(
          &mut synth_scopes,
          scope_key(&base_dir, &scope_dir),
          Value::Object(new_scope),
        );
      }
    }
  }

  let mut import_map = Map::new();
  if !synth_imports.is_empty() {
    import_map.insert("imports".to_string(), Value::Object(synth_imports));
  }
  if !synth_scopes.is_empty() {
    import_map.insert("scopes".to_string(), Value::Object(synth_scopes));
  }
  (base_import_map.base_url, Value::Object(import_map))
}

/// Copies a member's specifier map into `out`, its addresses rebased from the
/// member's directory onto the base map's.
fn rebase_specifier_map(
  base_dir: &Url,
  member_dir: &Url,
  specifiers: Map<String, Value>,
  out: &mut Map<String, Value>,
) {
  for (key, value) in specifiers {
    let Some(value_str) = value.as_str() else {
      continue;
    };
    let Ok(value_url) = member_dir.join(value_str) else {
      // not a URL or a path: the import map parser reports it
      out.insert(key, value);
      continue;
    };
    let address = if value_url.scheme() != "file" {
      // `npm:`, `jsr:`, `https:` and other absolute addresses as written
      value_str.to_string()
    } else {
      match relative_to(base_dir, &value_url) {
        Some(relative) => format!("./{relative}"),
        None => value_url.to_string(),
      }
    };
    out.insert(key, Value::String(address));
  }
}

/// The scope key for the directory `dir`: relative to the base map's directory
/// where that round-trips, otherwise the absolute URL; with a trailing slash
/// either way.
fn scope_key(base_dir: &Url, dir: &Url) -> String {
  match relative_to(base_dir, dir) {
    Some(relative) => {
      format!("./{}/", relative.strip_suffix('/').unwrap_or(&relative))
    }
    None => {
      let dir = dir.as_str();
      if dir.ends_with('/') {
        dir.to_string()
      } else {
        format!("{dir}/")
      }
    }
  }
}

/// `target` relative to `base_dir`, or `None` where no relative form names it:
/// another scheme or host, another Windows drive, or a relative form that
/// would resolve somewhere else.
fn relative_to(base_dir: &Url, target: &Url) -> Option<String> {
  if base_dir.scheme() != target.scheme() || base_dir.host() != target.host() {
    return None;
  }
  match (windows_drive(base_dir), windows_drive(target)) {
    (Some(a), Some(b)) if !a.eq_ignore_ascii_case(b) => return None,
    (Some(_), None) | (None, Some(_)) => return None,
    _ => {}
  }
  let relative = base_dir.make_relative(target)?;
  // `make_relative` compares segments literally (`c:` vs `C:` is a parent
  // step to it), and `..` never pops a drive letter: keep only a relative
  // form that resolves back to the target.
  let resolved = base_dir.join(&format!("./{relative}")).ok()?;
  (resolved == *target).then_some(relative)
}

/// The drive of a Windows `file:` URL (`file:///C:/...` -> `C:`).
fn windows_drive(url: &Url) -> Option<&str> {
  if url.scheme() != "file" {
    return None;
  }
  let first = url.path_segments()?.next()?;
  let mut chars = first.chars();
  match (chars.next(), chars.next(), chars.next()) {
    (Some(letter), Some(':' | '|'), None) if letter.is_ascii_alphabetic() => {
      Some(first)
    }
    _ => None,
  }
}

fn pop_last_segment(url: &Url) -> Url {
  let mut url = url.clone();
  if let Ok(mut segments) = url.path_segments_mut() {
    segments.pop();
  }
  // popping a segment drops the trailing separator
  // (https://github.com/servo/rust-url/issues/912)
  if !url.path().ends_with('/') {
    url.set_path(&format!("{}/", url.path()));
  }
  url
}

fn combine_object(
  base: &mut Map<String, Value>,
  property_name: String,
  addition: Value,
) {
  if let Some(base_property) = base.get_mut(&property_name) {
    if let Some(base_property_obj) = base_property.as_object_mut() {
      if let Value::Object(addition) = addition {
        for (key, value) in addition {
          combine_object(base_property_obj, key, value);
        }
      }
    } else {
      base.insert(property_name, addition);
    }
  } else {
    base.insert(property_name, addition);
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  fn config(base_url: &str, value: Value) -> ImportMapConfig {
    ImportMapConfig {
      base_url: Url::parse(base_url).unwrap(),
      import_map_value: value,
    }
  }

  /// Parses the synthetic map and resolves `specifier` from `referrer`.
  fn resolver(
    base: ImportMapConfig,
    children: Vec<ImportMapConfig>,
  ) -> impl Fn(&str, &str) -> String {
    let (url, value) = create_synthetic_import_map(base, children);
    let import_map =
      import_map::parse_from_value(url, value).unwrap().import_map;
    move |specifier, referrer| {
      import_map
        .resolve(specifier, &Url::parse(referrer).unwrap())
        .map(|url| url.to_string())
        .unwrap_or_else(|err| format!("error: {err}"))
    }
  }

  // The crate's own tests, unchanged in their expectations: on one drive the
  // output is exactly what import_map's version produced.
  #[test]
  fn same_output_as_the_crate_on_one_drive() {
    let base = config(
      "file:///import_map.json",
      json!({
        "imports": {
          "@std/assert/": "deno:/@std/assert/",
          "express": "npm:express@4"
        },
        "scopes": {
          "https://raw.githubusercontent.com/example/": {
            "https://deno.land/std/": "https://deno.land/std@0.177.0/"
          },
          "./foo/": {
            "root": "./other.js",
            "override": "./overwritten.js"
          }
        }
      }),
    );
    let children = vec![
      config(
        "file:///foo/deno.json",
        json!({
          "imports": {
            "~/": "./",
            "foo/": "./bar/",
            "override": "./overwritten.js",
            "moment": "npm:moment@5",
          },
          "scopes": {
            "./fizz/": { "express": "npm:express@5" },
            "https://raw.githubusercontent.com/example/": {
              "https://example.com/": "https://deno.land/std@0.177.0/"
            },
            "https://raw.githubusercontent.com/other/": {
              "https://deno.land/std/": "https://deno.land/std@0.177.0/"
            }
          },
        }),
      ),
      config(
        "file:///bar/deno.json",
        json!({
          "imports": { "@/": "./", "secret_mod/": "./some_mod/" },
          "scopes": { "./fizz/": { "foo/": "./buzz/" } },
        }),
      ),
    ];
    let ours = create_synthetic_import_map(
      config("file:///import_map.json", base.import_map_value.clone()),
      children
        .iter()
        .map(|c| config(c.base_url.as_str(), c.import_map_value.clone()))
        .collect(),
    );
    let theirs = import_map::ext::create_synthetic_import_map(base, children);
    assert_eq!(ours, theirs);
    assert_eq!(
      ours.1,
      json!({
        "imports": {
          "@std/assert/": "deno:/@std/assert/",
          "express": "npm:express@4"
        },
        "scopes": {
          "./foo/": {
            "~/": "./foo/",
            "foo/": "./foo/bar/",
            "override": "./foo/overwritten.js",
            "moment": "npm:moment@5",
            "root": "./other.js",
          },
          "./foo/fizz/": { "express": "npm:express@5" },
          "./bar/": { "@/": "./bar/", "secret_mod/": "./bar/some_mod/" },
          "./bar/fizz/": { "foo/": "./bar/buzz/" },
          "https://raw.githubusercontent.com/other/": {
            "https://deno.land/std/": "https://deno.land/std@0.177.0/"
          },
          "https://raw.githubusercontent.com/example/": {
            "https://deno.land/std/": "https://deno.land/std@0.177.0/",
            "https://example.com/": "https://deno.land/std@0.177.0/",
          },
        },
      })
    );
  }

  #[test]
  fn a_member_value_on_another_drive_stays_absolute() {
    // `%TEMP%\mono\deno.json` = `{"workspace":["./web"]}`, and
    // `web\deno.json` maps `lib` to a module on a `subst` drive.
    let resolve = resolver(
      config(
        "file:///C:/Users/u/AppData/Local/Temp/mono/deno.json",
        json!({}),
      ),
      vec![config(
        "file:///C:/Users/u/AppData/Local/Temp/mono/web/deno.json",
        json!({ "imports": {
          "lib": "file:///Q:/lib/mod.ts",
          "lib/": "file:///Q:/lib/",
          "local": "./local.ts",
        } }),
      )],
    );
    let referrer = "file:///C:/Users/u/AppData/Local/Temp/mono/web/main.ts";
    assert_eq!(resolve("lib", referrer), "file:///Q:/lib/mod.ts");
    assert_eq!(resolve("lib/x.ts", referrer), "file:///Q:/lib/x.ts");
    assert_eq!(
      resolve("local", referrer),
      "file:///C:/Users/u/AppData/Local/Temp/mono/web/local.ts"
    );
  }

  #[test]
  fn a_member_on_another_drive_is_scoped_absolutely() {
    let (_, value) = create_synthetic_import_map(
      config("file:///C:/work/app/deno.json", json!({})),
      vec![config(
        "file:///D:/lib/deno.json",
        json!({
          "imports": { "inner": "./sub/b.ts", "dep": "jsr:@std/path@1" },
          "scopes": { "./vendor/": { "x": "./vendor/x.ts" } },
        }),
      )],
    );
    assert_eq!(
      value,
      json!({ "scopes": {
        "file:///D:/lib/": {
          "inner": "file:///D:/lib/sub/b.ts",
          "dep": "jsr:@std/path@1",
        },
        "file:///D:/lib/vendor/": { "x": "file:///D:/lib/vendor/x.ts" },
      } })
    );
    let resolve = resolver(
      config(
        "file:///C:/work/app/deno.json",
        json!({ "imports": { "lib": "file:///D:/lib/mod.ts" } }),
      ),
      vec![
        config(
          "file:///D:/lib/deno.json",
          json!({
            "imports": { "inner": "./sub/b.ts", "dep": "jsr:@std/path@1" },
            "scopes": { "./vendor/": { "x": "./vendor/x.ts" } },
          }),
        ),
        config(
          "file:///C:/work/shared/deno.json",
          json!({ "imports": { "same": "./same.ts" } }),
        ),
      ],
    );
    assert_eq!(
      resolve("inner", "file:///D:/lib/mod.ts"),
      "file:///D:/lib/sub/b.ts"
    );
    assert_eq!(resolve("dep", "file:///D:/lib/mod.ts"), "jsr:@std/path@1");
    assert_eq!(
      resolve("x", "file:///D:/lib/vendor/a.ts"),
      "file:///D:/lib/vendor/x.ts"
    );
    assert_eq!(
      resolve("lib", "file:///C:/work/app/main.ts"),
      "file:///D:/lib/mod.ts"
    );
    // the same drive keeps its relative scope
    assert_eq!(
      resolve("same", "file:///C:/work/shared/mod.ts"),
      "file:///C:/work/shared/same.ts"
    );
  }

  #[test]
  fn a_root_on_another_drive_than_its_member() {
    // the root config on a `subst` drive, a member on `C:`
    let resolve = resolver(
      config("file:///Q:/mono/deno.json", json!({})),
      vec![config(
        "file:///C:/src/web/deno.json",
        json!({ "imports": { "a": "./a.ts" } }),
      )],
    );
    assert_eq!(
      resolve("a", "file:///C:/src/web/main.ts"),
      "file:///C:/src/web/a.ts"
    );
  }

  #[test]
  fn drive_letters_in_another_case_are_one_drive() {
    // `c:` and `C:` name one drive; the relative form `make_relative` gives
    // between them does not resolve back, so the absolute URL is written.
    let resolve = resolver(
      config("file:///c:/work/deno.json", json!({})),
      vec![config(
        "file:///C:/work/web/deno.json",
        json!({ "imports": { "a": "./a.ts" } }),
      )],
    );
    assert_eq!(
      resolve("a", "file:///C:/work/web/main.ts"),
      "file:///C:/work/web/a.ts"
    );
  }

  #[test]
  fn a_member_on_a_unc_share() {
    // another host: the crate's `make_relative(..).unwrap()` panicked here
    let resolve = resolver(
      config("file:///C:/work/deno.json", json!({})),
      vec![config(
        "file://server/share/lib/deno.json",
        json!({ "imports": { "a": "./a.ts" } }),
      )],
    );
    assert_eq!(
      resolve("a", "file://server/share/lib/mod.ts"),
      "file://server/share/lib/a.ts"
    );
  }

  #[test]
  fn relative_only_where_it_round_trips() {
    let rel = |base: &str, target: &str| {
      relative_to(&Url::parse(base).unwrap(), &Url::parse(target).unwrap())
    };
    assert_eq!(
      rel("file:///C:/app/", "file:///C:/lib/x.ts").as_deref(),
      Some("../lib/x.ts")
    );
    assert_eq!(rel("file:///C:/app/", "file:///D:/lib/x.ts"), None);
    assert_eq!(rel("file:///C:/app/", "file:///c:/app/x.ts"), None);
    assert_eq!(rel("file:///home/app/", "file:///C:/x.ts"), None);
    assert_eq!(rel("file:///C:/app/", "file://host/share/x.ts"), None);
    assert_eq!(
      rel("file://host/share/", "file://host/other/x.ts").as_deref(),
      Some("../other/x.ts")
    );
    assert_eq!(
      rel("file:///home/app/", "file:///home/lib/").as_deref(),
      Some("../lib/")
    );
    assert_eq!(rel("file:///home/app/", "https://example.com/x.ts"), None);
  }

  #[test]
  fn windows_drives() {
    let drive =
      |url: &str| windows_drive(&Url::parse(url).unwrap()).map(str::to_owned);
    assert_eq!(drive("file:///C:/work/"), Some("C:".to_owned()));
    assert_eq!(drive("file:///d:/"), Some("d:".to_owned()));
    assert_eq!(drive("file:///home/user/"), None);
    assert_eq!(drive("file:///CC:/x"), None);
    assert_eq!(drive("https://example.com/C:/"), None);
  }
}
