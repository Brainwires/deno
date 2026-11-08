// Copyright 2018-2026 the Deno authors. MIT license.

//! Recognises native addons built on Node's V8 C++ ABI (NAN, `NODE_MODULE`,
//! `NODE_MODULE_INIT`, raw `v8::` / `node::` symbols) rather than Node-API, so
//! `op_napi_open` can refuse them with a clear error instead of an opaque
//! loader message.

use std::path::Path;

/// The npm package a `.node` file belongs to, read from its path: the
/// component after the last `node_modules` directory (both layouts: a plain
/// `node_modules/re2/...` and the isolated `.deno/re2@1.27.0/node_modules/re2`),
/// or, in the global npm cache, the component after the registry host
/// (`npm/registry.npmjs.org/re2/1.27.0/...`). Scoped names keep their scope.
pub fn npm_package_name(path: &Path) -> Option<String> {
  let parts: Vec<String> = path
    .components()
    .map(|c| c.as_os_str().to_string_lossy().into_owned())
    .collect();
  let start = if let Some(i) = parts.iter().rposition(|p| p == "node_modules") {
    i + 1
  } else {
    // `<DENO_DIR>/npm/<registry host>/<name>/<version>/...`
    let i = parts.iter().rposition(|p| p == "npm")?;
    i + 2
  };
  let first = parts.get(start)?;
  if first.starts_with('@') {
    let second = parts.get(start + 1)?;
    Some(format!("{first}/{second}"))
  } else if first.is_empty() || first.starts_with('.') {
    None
  } else {
    Some(first.clone())
  }
}

/// Whether a (mangled) symbol name belongs to Node's V8 C++ ABI: a symbol in
/// the `v8::` or `node::` namespace (functions, `vtable for`, `typeinfo`), or
/// the legacy `node_module_register` / `node_register_module_v<N>` entry points.
fn is_v8_abi_symbol(sym: &str) -> bool {
  // Mach-O prefixes every C symbol with one more underscore.
  let sym = sym
    .strip_prefix('_')
    .filter(|s| s.starts_with("_Z"))
    .unwrap_or(sym);
  if let Some(mangled) = sym.strip_prefix("_Z") {
    return mangled.contains("N2v8") || mangled.contains("N4node");
  }
  let sym = sym.trim_start_matches('_');
  sym == "node_module_register" || sym.starts_with("node_register_module_v")
}

/// The first V8 / Node C++ ABI symbol named in a loader error, if any. Linux
/// says `undefined symbol: _ZN2v8...`, macOS `symbol not found in flat
/// namespace '__ZN2v8...'` (or `Symbol not found: __ZN2v8...`), musl
/// `Error relocating ...: _ZN2v8...: symbol not found`.
pub fn v8_abi_symbol_in_error(message: &str) -> Option<String> {
  message
    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
    .find(|token| is_v8_abi_symbol(token))
    .map(|token| {
      // Show the symbol as the C++ toolchain names it (no Mach-O underscore).
      match token.strip_prefix('_') {
        Some(rest) if rest.starts_with("_Z") => rest.to_string(),
        _ if token.starts_with("_Z") => token.to_string(),
        _ => token.trim_start_matches('_').to_string(),
      }
    })
}

/// The range of `NODE_MODULE_VERSION` values probed for a `NODE_MODULE_INIT`
/// (`node_register_module_v<N>`) export. Node 26 is 147; the slack covers
/// releases to come and Electron's own numbers (in the 100s).
pub fn node_module_versions() -> std::ops::RangeInclusive<u32> {
  1..=400
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn package_name_from_path() {
    let p = |s: &str| npm_package_name(Path::new(s));
    assert_eq!(
      p("/app/node_modules/re2/build/Release/re2.node").as_deref(),
      Some("re2")
    );
    assert_eq!(
      p("/app/node_modules/.deno/@datadog+pprof@5.20.0/node_modules/@datadog/pprof/prebuilds/linux-x64/node-127.node")
        .as_deref(),
      Some("@datadog/pprof")
    );
    assert_eq!(
      p("/home/me/.cache/deno/npm/registry.npmjs.org/re2/1.27.0/build/Release/re2.node")
        .as_deref(),
      Some("re2")
    );
    assert_eq!(p("/tmp/addon.node"), None);
  }

  #[test]
  fn symbol_from_loader_errors() {
    let linux = "/app/node_modules/re2/build/Release/re2.node: undefined symbol: _ZN2v87Isolate10GetCurrentEv";
    assert_eq!(
      v8_abi_symbol_in_error(linux).as_deref(),
      Some("_ZN2v87Isolate10GetCurrentEv")
    );
    let macos = "dlopen(/app/x.node, 0x0001): symbol not found in flat namespace '__ZN4node6Buffer4DataEN2v85LocalINS1_5ValueEEE'";
    assert_eq!(
      v8_abi_symbol_in_error(macos).as_deref(),
      Some("_ZN4node6Buffer4DataEN2v85LocalINS1_5ValueEEE")
    );
    let vtable = "undefined symbol: _ZTVN2v86ObjectE";
    assert_eq!(
      v8_abi_symbol_in_error(vtable).as_deref(),
      Some("_ZTVN2v86ObjectE")
    );
    assert_eq!(
      v8_abi_symbol_in_error("undefined symbol: _ZN3foo3barEv"),
      None
    );
    assert_eq!(v8_abi_symbol_in_error("undefined symbol: uv_fs_open"), None);
    assert_eq!(
      v8_abi_symbol_in_error("Symbol not found: _node_module_register")
        .as_deref(),
      Some("node_module_register")
    );
  }
}
