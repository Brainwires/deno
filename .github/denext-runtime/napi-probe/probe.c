// Copyright 2018-2026 the Deno authors. MIT license.
//
// A minimal Node-API addon for the denext runtime smoke test. It declares the
// few Node-API functions it uses itself (no node headers) and resolves them the
// two ways real addons do:
//
//   - linked: imported at load time, the way node-gyp / cmake-js addons are
//     built (an undefined symbol on macOS/Linux; on Windows a delay-loaded
//     import of `node.exe` that node-gyp's `win_delay_load_hook` redirects to
//     the host executable, `GetModuleHandle(NULL)`), and
//   - looked up at run time from the host process, the way napi-rs and neon
//     addons do (`dlopen(NULL)` + `dlsym`, `GetProcAddress` on the executable).
//
// Deno Desktop runs the Deno runtime as a shared library inside a host
// executable, so both only work when that library's Node-API symbols are
// visible process-wide (and, on Windows, from the executable).
//
// exports.add(a, b)  -> a + b, through linked Node-API calls
// exports.lookup()   -> "napi <version>", through Node-API functions looked up
//                       from the host process at call time

#include <stddef.h>
#include <stdint.h>

typedef struct napi_env__* napi_env;
typedef struct napi_value__* napi_value;
typedef struct napi_callback_info__* napi_callback_info;
typedef int napi_status;
typedef napi_value (*napi_callback)(napi_env env, napi_callback_info info);

#ifdef _WIN32
#define PROBE_EXPORT __declspec(dllexport)
#define PROBE_IMPORT __declspec(dllimport)
#else
#define PROBE_EXPORT __attribute__((visibility("default")))
#define PROBE_IMPORT
#endif

PROBE_IMPORT napi_status napi_create_function(napi_env env, const char* name,
                                              size_t length, napi_callback cb,
                                              void* data, napi_value* result);
PROBE_IMPORT napi_status napi_set_named_property(napi_env env,
                                                 napi_value object,
                                                 const char* name,
                                                 napi_value value);
PROBE_IMPORT napi_status napi_get_cb_info(napi_env env,
                                          napi_callback_info info,
                                          size_t* argc, napi_value* argv,
                                          napi_value* this_arg, void** data);
PROBE_IMPORT napi_status napi_get_value_int32(napi_env env, napi_value value,
                                              int32_t* result);
PROBE_IMPORT napi_status napi_create_int32(napi_env env, int32_t value,
                                           napi_value* result);

typedef napi_status (*get_version_fn)(napi_env env, uint32_t* result);
typedef napi_status (*create_string_fn)(napi_env env, const char* str,
                                        size_t length, napi_value* result);

#ifdef _WIN32
#include <windows.h>
#include <delayimp.h>

// node-gyp's src/win_delay_load_hook.cc: resolve the delay-loaded `node.exe`
// import against the host executable, whatever it is called.
static FARPROC WINAPI load_exe_hook(unsigned int event, DelayLoadInfo* info) {
  if (event != dliNotePreLoadLibrary) return NULL;
  if (_stricmp(info->szDll, "node.exe") != 0) return NULL;
  return (FARPROC)GetModuleHandleA(NULL);
}
const PfnDliHook __pfnDliNotifyHook2 = load_exe_hook;

static void* host_symbol(const char* name) {
  // libloading's `Library::this()` (napi-rs, neon) on Windows.
  return (void*)GetProcAddress(GetModuleHandleW(NULL), name);
}
#else
#include <dlfcn.h>

static void* host_symbol(const char* name) {
  // libloading's `Library::this()` (napi-rs, neon) on Unix.
  void* self = dlopen(NULL, RTLD_LAZY);
  void* sym = self ? dlsym(self, name) : NULL;
  return sym;
}
#endif

static napi_value add(napi_env env, napi_callback_info info) {
  size_t argc = 2;
  napi_value argv[2];
  int32_t a = 0, b = 0;
  napi_value result = NULL;
  if (napi_get_cb_info(env, info, &argc, argv, NULL, NULL) != 0) return NULL;
  if (argc < 2) return NULL;
  napi_get_value_int32(env, argv[0], &a);
  napi_get_value_int32(env, argv[1], &b);
  napi_create_int32(env, a + b, &result);
  return result;
}

static napi_value lookup(napi_env env, napi_callback_info info) {
  get_version_fn get_version = (get_version_fn)host_symbol("napi_get_version");
  create_string_fn create_string =
      (create_string_fn)host_symbol("napi_create_string_utf8");
  uint32_t version = 0;
  char text[32] = "napi ";
  size_t n = 5;
  char digits[11];
  size_t d = 0;
  napi_value result = NULL;
  (void)info;
  if (get_version == NULL || create_string == NULL) {
    // Report the failure as a value so the smoke test can show it.
    napi_create_int32(env, -1, &result);
    return result;
  }
  get_version(env, &version);
  do {
    digits[d++] = (char)('0' + version % 10);
    version /= 10;
  } while (version != 0 && d < sizeof digits);
  while (d > 0) text[n++] = digits[--d];
  create_string(env, text, n, &result);
  return result;
}

PROBE_EXPORT napi_value napi_register_module_v1(napi_env env,
                                                napi_value exports) {
  napi_value fn;
  napi_create_function(env, "add", 3, add, NULL, &fn);
  napi_set_named_property(env, exports, "add", fn);
  napi_create_function(env, "lookup", 6, lookup, NULL, &fn);
  napi_set_named_property(env, exports, "lookup", fn);
  return exports;
}
