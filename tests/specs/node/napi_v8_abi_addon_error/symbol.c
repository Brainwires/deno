// Copyright 2018-2026 the Deno authors. MIT license.

// An addon calling V8's C++ API whose symbols the loader binds at load time
// (built with `-z now` on Linux, chained fixups on macOS), so `dlopen` itself
// fails on the missing `v8::Isolate::GetCurrent()`.

extern void* _ZN2v87Isolate10GetCurrentEv(void);

__attribute__((visibility("default"))) void* touch_isolate(void) {
  return _ZN2v87Isolate10GetCurrentEv();
}
