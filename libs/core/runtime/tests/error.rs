// Copyright 2018-2026 the Deno authors. MIT license.

use std::future::poll_fn;
use std::task::Poll;

use deno_error::JsErrorBox;
use rstest::rstest;

use crate::JsRuntime;
use crate::RuntimeOptions;
use crate::op2;

#[rstest]
#[tokio::test]
async fn callsite_cache_ignores_array_prototype(
  #[values(0, 1, 2)] index: u32,
  #[values(false, true)] eval: bool,
) {
  let mut runtime = JsRuntime::new(Default::default());
  runtime
    .execute_script(
      "setup.js",
      format!("const index = {index}; const useEval = {eval};"),
    )
    .unwrap();
  runtime
    .execute_script(
      "callsite_cache.js",
      r#"
const originalPrepareStackTrace = Error.prepareStackTrace;
Error.prepareStackTrace = (_, frames) => frames;
function capture() {
  return useEval ? eval("new Error().stack[0]") : new Error().stack[0];
}
const expected = capture();
const fileName = expected.getFileName();
const lineNumber = expected.getLineNumber();
const columnNumber = expected.getColumnNumber();
if (fileName !== (useEval ? undefined : "callsite_cache.js") ||
    expected.isEval() !== useEval) {
  throw new Error("unexpected frame type");
}
let accesses = 0;
function fail() {
  accesses++;
  throw new Error("array prototype accessed");
}
Object.defineProperty(Array.prototype, index, {
  get: fail,
  set: fail,
  configurable: true,
});
try {
  const actual = capture();
  for (let i = 0; i < 2; i++) {
    if (actual.getFileName() !== fileName ||
        actual.getLineNumber() !== lineNumber ||
        actual.getColumnNumber() !== columnNumber) {
      throw new Error("incorrect cached location");
    }
  }
} finally {
  delete Array.prototype[index];
  Error.prepareStackTrace = originalPrepareStackTrace;
}
if (accesses !== 0) throw new Error("array prototype accessed");
"#,
    )
    .unwrap();
}

/// Frames of every kind the default formatter prints differently; `stacks()`
/// returns their formatted traces.
const STACK_SHAPES: &str = r#"
class Shape {
  constructor(inner) { this.stack = inner(); }
  method() { return new Error("method").stack; }
  static make() { return new Error("static").stack; }
}
function plain() { return new Error("plain").stack; }
function strict() { "use strict"; return new Error("strict").stack; }
const holder = { renamed: Shape.prototype.method, plain };
async function asyncFrames() {
  await null;
  return new Error("async").stack;
}
async function viaPromiseAll() {
  return (await Promise.all([null, asyncFrames()]))[1];
}
// A method whose name is `name`, which a frame of its trace prints.
function named(name) {
  return { [name]() { return new Error("named").stack; } }[name];
}
async function stacks() {
  const error = new TypeError("typed");
  error.name = "";
  const noMessage = new Error();
  return [
    plain(),
    strict(),
    new Shape(plain).stack,
    new Shape(plain).method(),
    Shape.make(),
    holder.renamed(),
    holder.plain(),
    [1].map(plain)[0],
    eval("plain()"),
    eval("eval('new Error(\"nested eval\").stack')"),
    new Function("return new Error('function ctor').stack")(),
    await asyncFrames(),
    await viaPromiseAll(),
    error.stack,
    noMessage.stack,
    Reflect.apply(plain, 7, []),
    plain.call(null),
    // Names that are not well-formed UTF-16: a lone surrogate reads as
    // U+FFFD, in a short name and in one long enough for the SIMD transcoder.
    named("\ud800")(),
    named("a\udc00b")(),
    named("x".repeat(100) + "\ud800\u00e9\u4e2d")(),
    named("\ud83d\ude00 \u00e9")(),
  ];
}
"#;

async fn format_stacks(with_reader: bool) -> Vec<String> {
  let mut runtime = JsRuntime::new(Default::default());
  if !with_reader {
    deno_core::scope!(scope, runtime);
    JsRuntime::state_from(scope)
      .callsite_info_cb
      .borrow_mut()
      .take();
  }
  runtime
    .execute_script("stack_shapes.js", STACK_SHAPES)
    .unwrap();
  let promise = runtime.execute_script("run.js", "stacks()").unwrap();
  #[allow(deprecated, reason = "test code")]
  let value = runtime.resolve_value(promise).await.unwrap();
  deno_core::scope!(scope, runtime);
  let value = deno_core::v8::Local::new(scope, value);
  deno_core::serde_v8::from_v8(scope, value).unwrap()
}

// The default stack formatter reads a whole trace's call sites with one call
// of `Deno.core.callSiteInfo`; without it, it calls each CallSite method from
// Rust. Both must print the same traces.
#[tokio::test]
async fn stack_format_same_with_and_without_callsite_reader() {
  let with_reader = format_stacks(true).await;
  let without_reader = format_stacks(false).await;
  assert_eq!(with_reader.len(), 21);
  for stack in &with_reader {
    assert!(stack.contains("\n    at "), "no frames in {stack:?}");
  }
  assert!(with_reader[2].contains("at new Shape (stack_shapes.js:"));
  assert!(with_reader[3].contains("at Shape.method (stack_shapes.js:"));
  assert!(with_reader[5].contains("at Object.method [as renamed] ("));
  assert!(with_reader[8].contains("eval at stacks (stack_shapes.js:"));
  assert!(with_reader[11].contains("at async stacks (stack_shapes.js:"));
  assert!(with_reader[12].contains("at async Promise.all (index 1)"));
  // What the conversion to a Rust `String` always made of a lone surrogate.
  assert!(with_reader[17].contains("at \u{FFFD} (stack_shapes.js:"));
  assert!(with_reader[18].contains("at a\u{FFFD}b (stack_shapes.js:"));
  assert!(with_reader[19].contains(&format!(
    "at {}\u{FFFD}\u{e9}\u{4e2d} (stack_shapes.js:",
    "x".repeat(100)
  )));
  assert!(with_reader[20].contains("at \u{1F600} \u{e9} (stack_shapes.js:"));
  pretty_assertions::assert_eq!(with_reader, without_reader);
}

// The reader's results land in slots preset by Rust, never through
// `Array.prototype`.
#[rstest]
#[tokio::test]
async fn stack_format_ignores_array_prototype(
  #[values(true, false)] with_reader: bool,
) {
  let mut runtime = JsRuntime::new(Default::default());
  if !with_reader {
    deno_core::scope!(scope, runtime);
    JsRuntime::state_from(scope)
      .callsite_info_cb
      .borrow_mut()
      .take();
  }
  runtime
    .execute_script(
      "tampered.js",
      r#"
function where() { return new Error("here").stack; }
let accesses = 0;
// not an array: the test puts accessors on `Array.prototype`
const stacks = { __proto__: null };
// both stacks are taken from this one line
for (let pass = 0; pass < 2; pass++) {
  if (pass === 1) {
    for (let index = 0; index < 64; index++) {
      Object.defineProperty(Array.prototype, index, {
        get() { accesses++; throw new Error("array prototype read"); },
        set() { accesses++; throw new Error("array prototype written"); },
        configurable: true,
      });
    }
  }
  try {
    stacks[pass] = where();
  } finally {
    if (pass === 1) {
      for (let index = 0; index < 64; index++) delete Array.prototype[index];
    }
  }
}
if (!/^Error: here\n    at where \(tampered\.js:2:27\)\n    at tampered\.js:\d+:\d+$/.test(stacks[0])) {
  throw new Error(`unexpected stack: ${stacks[0]}`);
}
if (stacks[1] !== stacks[0]) throw new Error(`stack changed: ${stacks[1]}`);
if (accesses !== 0) throw new Error("array prototype accessed");
"#,
    )
    .unwrap();
}

#[tokio::test]
async fn test_error_builder() {
  #[op2(fast)]
  fn op_err() -> Result<(), JsErrorBox> {
    Err(JsErrorBox::new("DOMExceptionOperationError", "abc"))
  }

  #[op2(fast)]
  fn op_registered_err(#[buffer] buffer: &[u8]) -> Result<(), JsErrorBox> {
    if buffer.first() == Some(&1) {
      Err(JsErrorBox::new("RegisteredError", "registered message"))
    } else {
      Ok(())
    }
  }

  deno_core::extension!(test_ext, ops = [op_err, op_registered_err]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init()],
    ..Default::default()
  });
  poll_fn(move |cx| {
    runtime
      .execute_script(
        "error_builder_test.js",
        include_str!("error_builder_test.js"),
      )
      .unwrap();
    if let Poll::Ready(Err(_)) = runtime.poll_event_loop(cx, Default::default())
    {
      unreachable!();
    }
    Poll::Ready(())
  })
  .await;
}

#[tokio::test]
async fn syntax_error() {
  let mut runtime = JsRuntime::new(Default::default());
  let src = "hocuspocus(";
  let js_error = runtime.execute_script("i.js", src).unwrap_err();
  let frame = js_error.frames.first().unwrap();
  assert_eq!(frame.column_number, Some(12));
}

// Native construction is shared by op dispatch paths for registered classes.
// Verify that it restores the class and defines the expected own properties
// without consulting inherited accessors.
#[tokio::test]
async fn native_registered_error_preserves_class() {
  use std::borrow::Cow;

  use deno_error::JsErrorClass;
  use deno_error::PropertyValue;

  #[derive(Debug)]
  struct CustomError;

  impl std::fmt::Display for CustomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      write!(f, "custom message")
    }
  }

  impl std::error::Error for CustomError {}

  impl JsErrorClass for CustomError {
    fn get_class(&self) -> Cow<'static, str> {
      Cow::Borrowed("MyError")
    }

    fn get_message(&self) -> Cow<'static, str> {
      Cow::Borrowed("custom message")
    }

    fn get_additional_properties(&self) -> deno_error::AdditionalProperties {
      Box::new(std::iter::once((
        Cow::Borrowed("code"),
        PropertyValue::String(Cow::Borrowed("E_CUSTOM")),
      )))
    }

    fn get_ref(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
      self
    }
  }

  let mut runtime = JsRuntime::new(Default::default());
  runtime
    .execute_script(
      "register.js",
      r#"
      globalThis.MyError = class MyError extends Error {
        constructor(msg) {
          super(msg);
          this.name = "MyError";
        }
      };
      Deno.core.registerErrorClass("MyError", globalThis.MyError);
      globalThis.nameSetterCalls = 0;
      globalThis.codeSetterCalls = 0;
      Object.defineProperty(globalThis.MyError.prototype, "name", {
        configurable: true,
        set() {
          globalThis.nameSetterCalls++;
        },
      });
      Object.defineProperty(globalThis.MyError.prototype, "code", {
        configurable: true,
        set() {
          globalThis.codeSetterCalls++;
        },
      });
      "#,
    )
    .unwrap();

  // Build + throw the exception through the native path, then stash it on the
  // global so JS can inspect it.
  {
    deno_core::scope!(scope, runtime);
    let exception = {
      v8::tc_scope!(let tc_scope, scope);
      crate::error::throw_js_error_class(tc_scope, &CustomError);
      let exception = tc_scope
        .exception()
        .expect("throw_js_error_class should set an exception");
      v8::Global::new(tc_scope, exception)
    };
    let exception = v8::Local::new(scope, &exception);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "thrownError").unwrap();
    global.set(scope, key.into(), exception);
  }

  // Assertions throw on mismatch, failing `execute_script`.
  runtime
    .execute_script(
      "assert.js",
      r#"
      {
        const e = globalThis.thrownError;
        if (!(e instanceof globalThis.MyError)) {
          throw new Error("expected instanceof MyError, got " + e.name);
        }
        if (globalThis.nameSetterCalls !== 0) {
          throw new Error("name setter ran during native error construction");
        }
        if (globalThis.codeSetterCalls !== 0) {
          throw new Error("code setter ran during native error construction");
        }
        if (!Object.hasOwn(e, "name")) {
          throw new Error("expected own name property");
        }
        if (e.name !== "MyError") {
          throw new Error("expected name 'MyError', got " + e.name);
        }
        if (e.message !== "custom message") {
          throw new Error("expected message 'custom message', got " + e.message);
        }
        if (!Object.hasOwn(e, "code")) {
          throw new Error("expected own code property");
        }
        if (e.code !== "E_CUSTOM") {
          throw new Error("expected code 'E_CUSTOM', got " + e.code);
        }
        const keys = e[Symbol.for("errorAdditionalPropertyKeys")];
        if (!keys || keys.length !== 1 || keys[0] !== "code") {
          throw new Error("expected key ['code'], got " + keys);
        }
      }
      "#,
    )
    .unwrap();
}
