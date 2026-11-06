// Copyright 2018-2026 the Deno authors. MIT license.
//!  This example shows you how to define ops in Rust and then call them from
//!  JavaScript.

use deno_core::Extension;
use deno_core::JsRuntime;
use deno_core::RuntimeOptions;

fn main() {
  // A `JsRuntime` must be created inside a tokio runtime context: V8's
  // delayed tasks are scheduled on it.
  let tokio_runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let _tokio_guard = tokio_runtime.enter();

  let my_ext = Extension {
    name: "my_ext",
    middleware_fn: Some(Box::new(|op| match op.name {
      "op_print" => op.disable(),
      _ => op,
    })),
    ..Default::default()
  };

  // Initialize a runtime instance
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![my_ext],
    ..Default::default()
  });

  // Deno.core.print() will now be a NOP
  runtime
    .execute_script("<usage>", r#"Deno.core.print("I'm broken")"#)
    .unwrap();
}
