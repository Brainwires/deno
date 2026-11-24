// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;

mod bitmaprenderer;
pub mod byow;
mod canvas;

deno_core::extension!(
  deno_canvas,
  deps = [deno_webidl, deno_web, deno_webgpu],
  ops = [
    op_init_canvas,
    canvas::op_offscreen_canvas_transfer,
    canvas::op_offscreen_canvas_receive,
  ],
  objects = [
    bitmaprenderer::ImageBitmapRenderingContext,
    canvas::OffscreenCanvas,
    byow::UnsafeWindowSurface,
  ],
  lazy_loaded_esm = ["01_canvas.js"],
  lazy_loaded_js = ["02_surface.js"],
);

// denext: `OffscreenCanvas` and its `"bitmaprenderer"` context alone, for a
// runtime that must not reach a GPU (one that runs web pages): registered
// in place of `deno_canvas`, it needs no `deno_webgpu` extension (none of
// its ops and interfaces), `getContext("webgpu")` answers `null` (the
// context is not supported), and there is no `Deno.UnsafeWindowSurface`.
// The module keeps its specifier, `ext:deno_canvas/01_canvas.js`. What it
// draws with is CPU memory (`image::DynamicImage`); the `ImageBitmap`
// interface it exchanges is `deno_image`'s.
deno_core::extension!(
  deno_canvas_offscreen,
  deps = [deno_webidl, deno_web],
  ops = [op_init_canvas],
  objects = [
    bitmaprenderer::ImageBitmapRenderingContext,
    canvas::OffscreenCanvas,
  ],
  lazy_loaded_esm = ["ext:deno_canvas/01_canvas.js" = "01_canvas.js"],
  state = |state| {
    state.put(canvas::NoWebGpuContext);
  },
);

#[op2(fast)]
pub fn op_init_canvas(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
  blob: v8::Local<v8::Value>,
) {
  state.put(canvas::BlobHandle(v8::Global::new(scope, blob.cast())));
}

#[cfg(test)]
mod tests {
  use super::*;

  // denext: what a runtime without a GPU registers: no `deno_webgpu`
  // dependency, no window surface, the same module specifier.
  #[test]
  fn the_offscreen_extension_needs_no_webgpu() {
    let full = deno_canvas::init();
    assert!(full.deps.contains(&"deno_webgpu"));
    assert!(full.op_state_fn.is_none());

    let offscreen = deno_canvas_offscreen::init();
    assert_eq!(offscreen.name, "deno_canvas_offscreen");
    assert_eq!(offscreen.deps, ["deno_webidl", "deno_web"]);
    assert_eq!(offscreen.ops.len(), full.ops.len());
    assert!(offscreen.objects.len() < full.objects.len());
    assert!(offscreen.lazy_loaded_js_files.is_empty());
    let specifiers: Vec<_> = offscreen
      .lazy_loaded_esm_files
      .iter()
      .map(|file| file.specifier)
      .collect();
    assert_eq!(specifiers, ["ext:deno_canvas/01_canvas.js"]);
    let mut state = OpState::new(None);
    (offscreen.op_state_fn.unwrap())(&mut state);
    assert!(state.has::<canvas::NoWebGpuContext>());
  }
}
