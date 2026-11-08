// Copyright 2018-2026 the Deno authors. MIT license.

// What NAN's `NODE_MODULE(name, init)` expands to: a static constructor that
// hands a `node_module` with `nm_version` = NODE_MODULE_VERSION to
// `node_module_register`. Its init would call into V8's C++ API.

typedef struct node_module {
  int nm_version;
  unsigned int nm_flags;
  void* nm_dso_handle;
  const char* nm_filename;
  void* nm_register_func;
  void* nm_context_register_func;
  const char* nm_modname;
  void* nm_priv;
  void* nm_link;
} node_module;

extern void node_module_register(void* mod);
extern void* _ZN2v87Isolate10GetCurrentEv(void);

static void init(void* exports, void* module, void* priv) {
  (void)exports;
  (void)module;
  (void)priv;
  _ZN2v87Isolate10GetCurrentEv();
}

static node_module _module = {
    147, 0, 0, __FILE__, (void*)init, 0, "register", 0, 0,
};

static void register_module(void) __attribute__((constructor));
static void register_module(void) { node_module_register(&_module); }
