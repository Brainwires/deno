// Copyright 2018-2026 the Deno authors. MIT license.

//! Helpers for the V8 `host_defined_options` PrimitiveArray attached to a
//! script's origin. Index 0 of the array stores a `Uint32` "kind" the
//! runtime can read back from the dynamic-import host callback to decide
//! how to handle `import()` calls originating from the script.

use v8::PinScope;

use crate::JsRuntime;

/// Index of the kind tag inside the host-defined-options PrimitiveArray.
pub const HOST_DEFINED_OPTIONS_KIND_INDEX: usize = 0;
pub const HOST_DEFINED_OPTIONS_KEY_INDEX: usize = 1;

/// Kind tags written at [`HOST_DEFINED_OPTIONS_KIND_INDEX`].
pub mod host_defined_options_kind {
  /// Script created by `node:vm` (`vm.Script`, `vm.runInThisContext`,
  /// `vm.compileFunction`, `vm.SourceTextModule`) without an
  /// `importModuleDynamically` callback. When the dynamic-import host
  /// callback sees this marker it rejects the import with
  /// `ERR_VM_DYNAMIC_IMPORT_CALLBACK_MISSING`.
  pub const VM_DYNAMIC_IMPORT_MISSING: u32 = 1;
  /// Script created by `node:vm` with a user-provided
  /// `importModuleDynamically` callback. Index 1 stores the script's
  /// callback key, a `Symbol` that the embedder's dispatcher (see
  /// [`super::register_vm_dynamic_import_dispatcher`]) maps to the callback
  /// weakly, so the callback lives exactly as long as the compiled code.
  pub const VM_DYNAMIC_IMPORT_CALLBACK: u32 = 2;
}

/// Build a host-defined-options PrimitiveArray with the given kind tag.
pub fn create_host_defined_options_with_kind<'s>(
  scope: &mut PinScope<'s, '_>,
  kind: u32,
) -> v8::Local<'s, v8::Data> {
  let arr = v8::PrimitiveArray::new(scope, 1);
  let value = v8::Integer::new_from_unsigned(scope, kind);
  arr.set(scope, HOST_DEFINED_OPTIONS_KIND_INDEX, value.into());
  arr.into()
}

/// Build a host-defined-options PrimitiveArray with a kind tag and registry key.
pub fn create_host_defined_options_with_kind_and_key<'s>(
  scope: &mut PinScope<'s, '_>,
  kind: u32,
  key: u32,
) -> v8::Local<'s, v8::Data> {
  let arr = v8::PrimitiveArray::new(scope, 2);
  let kind_value = v8::Integer::new_from_unsigned(scope, kind);
  arr.set(scope, HOST_DEFINED_OPTIONS_KIND_INDEX, kind_value.into());
  let key_value = v8::Integer::new_from_unsigned(scope, key);
  arr.set(scope, HOST_DEFINED_OPTIONS_KEY_INDEX, key_value.into());
  arr.into()
}

/// Read the kind tag from a host-defined-options value. Returns `None`
/// when the value isn't a non-empty PrimitiveArray whose first element
/// is a numeric primitive (matching what [`create_host_defined_options_with_kind`]
/// writes).
pub fn read_host_defined_options_kind(
  scope: &mut PinScope<'_, '_>,
  host_defined_options: v8::Local<v8::Data>,
) -> Option<u32> {
  // V8's HostImportModuleDynamicallyCallback contract is that
  // `host_defined_options` is always a `v8::PrimitiveArray` (V8 supplies an
  // empty one when the embedder didn't set any). rusty_v8 lacks a checked
  // `TryFrom<Data> for PrimitiveArray` impl, so we cast unchecked; the
  // resulting `length()` is 0 when V8 supplied the empty fallback, and the
  // `Uint32` check below safely returns `None` for the embedder's other
  // PrimitiveArray shapes (e.g. `[Boolean(true)]`).
  // SAFETY: `Local<PrimitiveArray>` is layout-compatible with `Local<Data>`
  // (see `impl_deref!` / `impl_from!` in the v8 crate), and V8 guarantees
  // the input is a PrimitiveArray.
  let arr: v8::Local<v8::PrimitiveArray> = unsafe {
    std::mem::transmute::<v8::Local<v8::Data>, v8::Local<v8::PrimitiveArray>>(
      host_defined_options,
    )
  };
  if arr.length() == HOST_DEFINED_OPTIONS_KIND_INDEX {
    return None;
  }
  let primitive = arr.get(scope, HOST_DEFINED_OPTIONS_KIND_INDEX);
  let value: v8::Local<v8::Value> = primitive.into();
  let int = v8::Local::<v8::Uint32>::try_from(value).ok()?;
  Some(int.value())
}

/// Read the registry key from a host-defined-options value.
pub fn read_host_defined_options_key(
  scope: &mut PinScope<'_, '_>,
  host_defined_options: v8::Local<v8::Data>,
) -> Option<u32> {
  let arr: v8::Local<v8::PrimitiveArray> = unsafe {
    std::mem::transmute::<v8::Local<v8::Data>, v8::Local<v8::PrimitiveArray>>(
      host_defined_options,
    )
  };
  if arr.length() <= HOST_DEFINED_OPTIONS_KEY_INDEX {
    return None;
  }
  let primitive = arr.get(scope, HOST_DEFINED_OPTIONS_KEY_INDEX);
  let value: v8::Local<v8::Value> = primitive.into();
  let int = v8::Local::<v8::Uint32>::try_from(value).ok()?;
  Some(int.value())
}

/// Build a host-defined-options PrimitiveArray with the given kind tag and a
/// `Symbol` key at [`HOST_DEFINED_OPTIONS_KEY_INDEX`].
pub fn create_host_defined_options_with_kind_and_symbol<'s>(
  scope: &mut PinScope<'s, '_>,
  kind: u32,
  key: v8::Local<'s, v8::Symbol>,
) -> v8::Local<'s, v8::Data> {
  let arr = v8::PrimitiveArray::new(scope, 2);
  let kind_value = v8::Integer::new_from_unsigned(scope, kind);
  arr.set(scope, HOST_DEFINED_OPTIONS_KIND_INDEX, kind_value.into());
  arr.set(scope, HOST_DEFINED_OPTIONS_KEY_INDEX, key.into());
  arr.into()
}

/// Read the `Symbol` key from a host-defined-options value.
pub fn read_host_defined_options_symbol<'s>(
  scope: &mut PinScope<'s, '_>,
  host_defined_options: v8::Local<'s, v8::Data>,
) -> Option<v8::Local<'s, v8::Symbol>> {
  // SAFETY: see `read_host_defined_options_kind`.
  let arr: v8::Local<v8::PrimitiveArray> = unsafe {
    std::mem::transmute::<v8::Local<v8::Data>, v8::Local<v8::PrimitiveArray>>(
      host_defined_options,
    )
  };
  if arr.length() <= HOST_DEFINED_OPTIONS_KEY_INDEX {
    return None;
  }
  let primitive = arr.get(scope, HOST_DEFINED_OPTIONS_KEY_INDEX);
  let value: v8::Local<v8::Value> = primitive.into();
  v8::Local::<v8::Symbol>::try_from(value).ok()
}

/// Set the function that `import()` in a `node:vm` script compiled with an
/// `importModuleDynamically` callback calls, as
/// `dispatcher(key, specifier, importAttributes)`, where `key` is the
/// script's host-defined-options `Symbol`. The embedder keeps the callbacks
/// in a `WeakMap` keyed by that symbol, as Node.js does: the compiled code
/// holds the key, so a callback (and the script object its closure
/// captures) is collected with the code. A per-runtime table of strong
/// references leaked every such script. Subsequent calls replace the
/// dispatcher.
pub fn register_vm_dynamic_import_dispatcher(
  scope: &mut PinScope<'_, '_>,
  dispatcher: v8::Local<v8::Function>,
) {
  let state = JsRuntime::state_from(scope);
  *state.vm_dynamic_import_dispatcher.borrow_mut() =
    Some(v8::Global::new(scope, dispatcher));
}

/// Register a fallback for V8's host-initialize-import-meta callback that
/// runs when the module is not tracked by the module map. `node:vm` uses
/// this to populate `import.meta` for `SourceTextModule` instances. The
/// callback is per-isolate; subsequent calls overwrite the previous value.
pub fn register_external_module_import_meta_cb(
  scope: &mut PinScope<'_, '_>,
  callback: crate::runtime::ExternalModuleImportMetaCb,
) {
  let state = JsRuntime::state_from(scope);
  state.external_module_import_meta_cb.set(Some(callback));
}
