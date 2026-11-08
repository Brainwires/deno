// Copyright 2018-2026 the Deno authors. MIT license.

// What `NODE_MODULE_INIT()` expands to: no constructor, an exported
// `node_register_module_v<NODE_MODULE_VERSION>` that Node calls with V8
// handles after `dlopen`.

extern void* _ZN2v87Isolate10GetCurrentEv(void);

__attribute__((visibility("default"))) void node_register_module_v147(
    void* exports, void* module, void* context) {
  (void)exports;
  (void)module;
  (void)context;
  _ZN2v87Isolate10GetCurrentEv();
}
