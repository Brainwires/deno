// Copyright 2018-2026 the Deno authors. MIT license.

#![allow(clippy::disallowed_methods, reason = "build code")]

fn main() {
  // Skip building from docs.rs.
  if std::env::var_os("DOCS_RS").is_some() {
    return;
  }

  // For cdylib targets, we must explicitly export NAPI symbols so that
  // native addons (e.g. next-swc) can resolve them via dlsym.
  // The print_linker_flags() functions use cargo:rustc-link-arg-bin
  // which only applies to binary targets, so we emit cdylib-specific
  // linker args here instead.
  print_cdylib_napi_linker_flags();
}

fn print_cdylib_napi_linker_flags() {
  let symbols_file_name = match std::env::consts::OS {
    "android" | "freebsd" | "openbsd" => {
      "generated_symbol_exports_list_linux.def".to_string()
    }
    os => format!("generated_symbol_exports_list_{}.def", os),
  };

  // Path relative to this build script's Cargo.toml
  let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
  let symbols_path = std::path::Path::new(&manifest_dir)
    .join("../../ext/napi")
    .join(&symbols_file_name)
    .canonicalize()
    .expect("Missing NAPI symbols list");

  println!("cargo:rerun-if-changed={}", symbols_path.display());

  #[cfg(target_os = "macos")]
  println!(
    "cargo:rustc-cdylib-link-arg=-Wl,-exported_symbols_list,{}",
    symbols_path.display(),
  );

  #[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd"
  ))]
  {
    // No `--export-dynamic-symbol-list` here (denort's executable needs one;
    // this cdylib doesn't): every NAPI/uv function in the list is a Rust
    // `#[no_mangle]` / `#[export_name]` function, which rustc's cdylib version
    // script exports anyway, and lld keeps a symbol the list names
    // preemptible even under `-Bsymbolic` (below), so the runtime's own napi_*
    // calls would still bind in the global scope first. self_binding.py checks
    // that every name in the list stays exported.
    //
    // V8 (via rusty_v8's `use_custom_libcxx`) statically links its own
    // libc++/libc++abi into this cdylib. A desktop backend (laufey) `dlopen`s
    // us into a process that already uses the system libstdc++. Without hiding
    // our bundled C++ runtime symbols, ELF interposition binds our internal
    // `__cxa_guard_acquire` (and friends) to the host's libstdc++, whose guard
    // layout differs — the runtime aborts at static init with
    // "libc++abi: __cxa_guard_acquire failed to acquire mutex" before
    // `Deno.serve` ever runs (denoland/deno#35381).
    //
    // `--exclude-libs,ALL` localizes every static-archive symbol (including the
    // bundled libc++abi) so our calls resolve to our own copy. The NAPI/uv
    // exports and the `laufey_runtime_*` entry points are Rust `#[no_mangle]`
    // functions, which rustc's version script exports, so both stay visible.
    println!("cargo:rustc-cdylib-link-arg=-Wl,--exclude-libs,ALL");

    // `--exclude-libs` hides C/C++ archive symbols, but rustc's own cdylib
    // version script still exports every `#[no_mangle]` function of every
    // Rust crate linked in: the brotli crate's C API (`ffi-api`, which
    // deno_node's `node:zlib` calls), temporal_rs, the napi_* functions,
    // rusty_v8's callbacks. A call from this library to one of those went
    // through a dynamic relocation the loader resolves in the global scope
    // first, so in a webview app (WebKitGTK and libsoup load the system
    // libbrotlidec before the host dlopen()s us) `node:zlib`'s Brotli called
    // the system `BrotliDecoderDecompressStream` with the Rust decoder's
    // state and spun at 100% CPU. `-Bsymbolic` binds every reference to a
    // symbol defined here inside the library at link time (it covers
    // `-Bsymbolic-functions` and data too; lld sets no `DF_SYMBOLIC` for it);
    // the exports stay for dlsym and N-API addons. The hosts dlopen() us RTLD_LOCAL, so our exports never
    // take another library's calls either. denext_runtime.yml checks the
    // result (.github/denext-runtime/self_binding.py, DENEXT.md item 187).
    println!("cargo:rustc-cdylib-link-arg=-Wl,-Bsymbolic");
  }
}
