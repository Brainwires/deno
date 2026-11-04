// Copyright 2018-2026 the Deno authors. MIT license.

pub trait IsBuiltInNodeModuleChecker: std::fmt::Debug {
  /// e.g. `is_builtin_node_module("assert")`
  fn is_builtin_node_module(&self, module_name: &str) -> bool;
}

/// An implementation of IsBuiltInNodeModuleChecker that uses
/// the list of built-in node_modules that are supported by Deno
/// in the `deno_node` crate (ext/node).
#[derive(Debug)]
pub struct DenoIsBuiltInNodeModuleChecker;

impl IsBuiltInNodeModuleChecker for DenoIsBuiltInNodeModuleChecker {
  #[inline(always)]
  fn is_builtin_node_module(&self, module_name: &str) -> bool {
    DENO_SUPPORTED_BUILTIN_NODE_MODULES
      .binary_search(&module_name)
      .is_ok()
  }
}

/// The built-in modules of `DENO_SUPPORTED_BUILTIN_NODE_MODULES` that, as in
/// Node (`schemelessBlockList` in `lib/internal/bootstrap/realm.js`), are only
/// built-ins with the `node:` scheme: a bare `vfs` names a package. Keep in sync
/// with `schemelessBlockList` in `ext/node/polyfills/01_require.js`.
pub static NODE_SCHEME_ONLY_BUILTIN_NODE_MODULES: &[&str] =
  &["sqlite", "test", "test/reporters", "vfs"];

/// Whether `module_name` is a built-in only with the `node:` scheme, so a bare
/// specifier of that name is not resolved to it.
pub fn is_node_scheme_only_builtin(module_name: &str) -> bool {
  NODE_SCHEME_ONLY_BUILTIN_NODE_MODULES.contains(&module_name)
}

/// Collection of built-in node_modules supported by Deno.
pub static DENO_SUPPORTED_BUILTIN_NODE_MODULES: &[&str] = &[
  // NOTE(bartlomieju): keep this list in sync with `ext/node/polyfills/01_require.js`
  "_http_agent",
  "_http_common",
  "_http_outgoing",
  "_http_server",
  "_stream_duplex",
  "_stream_passthrough",
  "_stream_readable",
  "_stream_transform",
  "_stream_writable",
  "_tls_common",
  "_tls_wrap",
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "diagnostics_channel",
  "dns",
  "dns/promises",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "http2",
  "https",
  "inspector",
  "inspector/promises",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "punycode",
  "querystring",
  "readline",
  "readline/promises",
  "repl",
  "sqlite",
  "stream",
  "stream/consumers",
  "stream/iter",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "sys",
  "test",
  "test/reporters",
  "timers",
  "timers/promises",
  "tls",
  "trace_events",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vfs",
  "vm",
  "wasi",
  "worker_threads",
  "zlib",
  "zlib/iter",
];

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn test_builtins_are_sorted() {
    let mut builtins_list = DENO_SUPPORTED_BUILTIN_NODE_MODULES.to_vec();
    builtins_list.sort();
    assert_eq!(DENO_SUPPORTED_BUILTIN_NODE_MODULES, builtins_list);
  }

  #[test]
  fn scheme_only_builtins_are_builtins() {
    for name in NODE_SCHEME_ONLY_BUILTIN_NODE_MODULES {
      assert!(DenoIsBuiltInNodeModuleChecker.is_builtin_node_module(name));
      assert!(is_node_scheme_only_builtin(name));
    }
    assert!(!is_node_scheme_only_builtin("fs"));
    assert!(!is_node_scheme_only_builtin("stream/iter"));
  }
}
