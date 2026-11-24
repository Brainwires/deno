# deno_canvas

`OffscreenCanvas` with two contexts, `"bitmaprenderer"`
(`ImageBitmapRenderingContext`) and `"webgpu"` (`deno_webgpu`'s
`GPUCanvasContext`), and `Deno.UnsafeWindowSurface`, a native window's surface
for either. There is no 2D context (`CanvasRenderingContext2D`), and no WebGL.

Two extensions:

- `deno_canvas`: all of it. Needs the `deno_webgpu` extension.
- `deno_canvas_offscreen`: `OffscreenCanvas` and `"bitmaprenderer"` only, for a
  runtime that must not reach a GPU (one that runs web pages). It needs
  `deno_webidl` and `deno_web`, registers no window surface,
  `getContext("webgpu")` answers `null`, and the module keeps its specifier
  (`ext:deno_canvas/01_canvas.js`). The canvas is CPU memory; the `ImageBitmap`
  interface it takes and returns is `deno_image`'s. The crate still links
  `deno_webgpu` (the context types are its own).
