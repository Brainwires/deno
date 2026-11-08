// Copyright 2018-2026 the Deno authors. MIT license.
//! This mod provides DenoError to unify errors across Deno.
use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use color_print::cformat;
use color_print::cstr;
use deno_core::error::JsError;
use deno_core::error::format_frame;
use deno_core::url::Url;
use deno_terminal::colors;

/// Set to `true` when user code assigns to `Object.prototype.__proto__` while
/// the accessor is disabled (the default, unless `--unsafe-proto`).
/// The assignment itself stays a silent no-op so fragile packages keep working
/// (see denoland/deno#34730 / #34772, where throwing broke Playwright); this
/// flag only lets the uncaught-error formatter nudge toward the escape hatch.
/// Written by `op_proto_set_attempted`, read by `get_suggestions_for_terminal_errors`.
pub static PROTO_SET_ATTEMPTED: AtomicBool = AtomicBool::new(false);

/// Set to `true` when user code reads `Object.prototype.__proto__` while the
/// accessor is disabled (the read returns `undefined`). Unlike a write, a read
/// crashes right at the access site (e.g. `who.__proto__.constructor` throws on
/// the same line), so on its own a read flag is too noisy to nudge on: programs
/// routinely probe `__proto__` for feature detection without anything going
/// wrong. The error formatter therefore only suggests the escape hatch when
/// this flag is set *and* the crashing error mentions `__proto__`.
/// Written by `op_proto_get_attempted`, read by `get_suggestions_for_terminal_errors`.
pub static PROTO_GET_ATTEMPTED: AtomicBool = AtomicBool::new(false);

const MAX_FORMAT_ERROR_DEPTH: usize = 64;

#[derive(Debug, Clone)]
struct ErrorReference<'a> {
  from: &'a JsError,
  to: &'a JsError,
}

#[derive(Debug, Clone)]
struct IndexedErrorReference<'a> {
  reference: ErrorReference<'a>,
  index: usize,
}

#[derive(Debug)]
enum FixSuggestionKind {
  Info,
  Hint,
  Docs,
}

#[derive(Debug)]
enum FixSuggestionMessage<'a> {
  Single(&'a str),
  Multiline(&'a [&'a str]),
}

#[derive(Debug)]
pub struct FixSuggestion<'a> {
  kind: FixSuggestionKind,
  message: FixSuggestionMessage<'a>,
}

impl<'a> FixSuggestion<'a> {
  pub fn info(message: &'a str) -> Self {
    Self {
      kind: FixSuggestionKind::Info,
      message: FixSuggestionMessage::Single(message),
    }
  }

  pub fn info_multiline(messages: &'a [&'a str]) -> Self {
    Self {
      kind: FixSuggestionKind::Info,
      message: FixSuggestionMessage::Multiline(messages),
    }
  }

  pub fn hint(message: &'a str) -> Self {
    Self {
      kind: FixSuggestionKind::Hint,
      message: FixSuggestionMessage::Single(message),
    }
  }

  pub fn hint_multiline(messages: &'a [&'a str]) -> Self {
    Self {
      kind: FixSuggestionKind::Hint,
      message: FixSuggestionMessage::Multiline(messages),
    }
  }

  pub fn docs(url: &'a str) -> Self {
    Self {
      kind: FixSuggestionKind::Docs,
      message: FixSuggestionMessage::Single(url),
    }
  }
}

struct AnsiColors;

impl deno_core::error::ErrorFormat for AnsiColors {
  fn fmt_element(
    element: deno_core::error::ErrorElement,
    in_extension_code: bool,
    s: &str,
  ) -> std::borrow::Cow<'_, str> {
    if in_extension_code {
      return colors::dimmed_gray(s).to_string().into();
    }
    use deno_core::error::ErrorElement::*;
    match element {
      Anonymous | NativeFrame | FileName | EvalOrigin => {
        colors::cyan(s).to_string().into()
      }
      LineNumber | ColumnNumber => colors::yellow(s).to_string().into(),
      FunctionName | PromiseAll => colors::italic_bold(s).to_string().into(),
      WorkingDirPath => colors::dimmed_gray(s).to_string().into(),
      PlainText => s.into(),
    }
  }
}

fn source_line_prefix(source_line: &str, column_utf16: u64) -> Option<String> {
  let mut current_utf16 = 0;
  let mut prefix = String::new();

  for c in source_line.chars() {
    if current_utf16 == column_utf16 {
      return Some(prefix);
    }

    let next_utf16 = current_utf16 + c.len_utf16() as u64;
    if next_utf16 > column_utf16 {
      return None;
    }

    if c == '\t' {
      prefix.push('\t');
    } else {
      for _ in 0..c.len_utf16() {
        prefix.push(' ');
      }
    }
    current_utf16 = next_utf16;
  }

  (current_utf16 == column_utf16).then_some(prefix)
}

/// Take an optional source line and associated information to format it into
/// a pretty printed version of that line.
fn format_maybe_source_line(
  source_line: Option<&str>,
  column_number: Option<i64>,
  is_error: bool,
  level: usize,
) -> String {
  if source_line.is_none() || column_number.is_none() {
    return "".to_string();
  }

  let source_line = source_line.unwrap();
  // sometimes source_line gets set with an empty string, which then outputs
  // an empty source line when displayed, so need just short circuit here.
  if source_line.is_empty() {
    return "".to_string();
  }
  if source_line.contains("Couldn't format source line: ") {
    return format!("\n{source_line}");
  }

  let column_number = column_number.unwrap();
  let prefix = column_number
    .checked_sub(1)
    .and_then(|column| u64::try_from(column).ok())
    .and_then(|column| source_line_prefix(source_line, column));
  let Some(mut s) = prefix else {
    return format!(
      "\n{} Couldn't format source line: Column {} is out of bounds (source may have changed at runtime)",
      colors::yellow("Warning"),
      column_number,
    );
  };

  s.push('^');
  let color_underline = if is_error {
    colors::red(&s).to_string()
  } else {
    colors::cyan(&s).to_string()
  };

  let indent = format!("{:indent$}", "", indent = level);

  format!("\n{indent}{source_line}\n{indent}{color_underline}")
}

fn find_recursive_cause(js_error: &JsError) -> Option<ErrorReference<'_>> {
  let mut history = Vec::<&JsError>::with_capacity(MAX_FORMAT_ERROR_DEPTH);

  let mut current_error: &JsError = js_error;

  while history.len() < MAX_FORMAT_ERROR_DEPTH
    && let Some(cause) = &current_error.cause
  {
    history.push(current_error);

    if let Some(seen) = history.iter().find(|&el| cause.is_same_error(el)) {
      return Some(ErrorReference {
        from: current_error,
        to: seen,
      });
    } else {
      current_error = cause;
    }
  }

  None
}

fn format_aggregated_error(
  aggregated_errors: &Vec<JsError>,
  circular_reference_index: usize,
  initial_cwd: Option<&Url>,
  filter_frames: bool,
  depth: usize,
) -> String {
  let mut s = String::new();
  let mut nested_circular_reference_index = circular_reference_index;

  for js_error in aggregated_errors {
    let aggregated_circular = find_recursive_cause(js_error);
    if aggregated_circular.is_some() {
      nested_circular_reference_index += 1;
    }
    let error_string = format_js_error_inner(
      js_error,
      aggregated_circular.map(|reference| IndexedErrorReference {
        reference,
        index: nested_circular_reference_index,
      }),
      false,
      filter_frames,
      vec![],
      initial_cwd,
      depth + 1,
    );

    for line in error_string.trim_start_matches("Uncaught ").lines() {
      write!(s, "\n    {line}").unwrap();
    }
  }

  s
}

fn stack_frame_is_ext(frame: &deno_core::error::JsStackFrame) -> bool {
  frame
    .file_name
    .as_ref()
    .map(|file_name| {
      file_name.starts_with("ext:") || file_name.starts_with("node:")
    })
    .unwrap_or(false)
}

fn format_js_error_inner(
  js_error: &JsError,
  circular: Option<IndexedErrorReference>,
  include_source_code: bool,
  filter_frames: bool,
  suggestions: Vec<FixSuggestion>,
  initial_cwd: Option<&Url>,
  depth: usize,
) -> String {
  if depth >= MAX_FORMAT_ERROR_DEPTH {
    return colors::cyan("[Error details truncated]").to_string();
  }

  let mut s = String::new();

  s.push_str(&js_error.exception_message);

  if let Some(circular) = &circular
    && js_error.is_same_error(circular.reference.to)
  {
    write!(s, " {}", colors::cyan(format!("<ref *{}>", circular.index)))
      .unwrap();
  }

  if let Some(aggregated) = &js_error.aggregated {
    let aggregated_message = format_aggregated_error(
      aggregated,
      circular
        .as_ref()
        .map(|circular| circular.index)
        .unwrap_or(0),
      initial_cwd,
      filter_frames,
      depth,
    );
    s.push_str(&aggregated_message);
  }

  if js_error.stack_is_custom {
    // The error carries a custom `.stack` (e.g. from Effect or fiber runtimes)
    // that our structured frames don't capture. Preserve it, matching Node.js.
    // The first line of `.stack` is the "Name: message" header, which we
    // already rendered as `exception_message`, so skip it and emit the rest.
    if let Some(stack) = &js_error.stack {
      for line in stack.lines().skip(1) {
        write!(s, "\n{line}").unwrap();
      }
    }
  } else {
    let column_number = js_error
      .source_line_frame_index
      .and_then(|i| js_error.frames.get(i).unwrap().column_number);
    s.push_str(&format_maybe_source_line(
      if include_source_code {
        js_error.source_line.as_deref()
      } else {
        None
      },
      column_number,
      true,
      0,
    ));

    let at_dimmed = Cow::Owned(colors::dimmed_gray("at ").to_string());
    let at_normal = Cow::Borrowed("at ");
    for frame in &js_error.frames {
      let is_ext = stack_frame_is_ext(frame);
      if filter_frames
        && is_ext
        && let Some(fn_name) = &frame.function_name
        && (fn_name.starts_with("__node_internal_")
          || fn_name == "eventLoopTick"
          || fn_name == "denoErrorToNodeError"
          || fn_name == "__drainNextTickAndMacrotasks")
      {
        continue;
      }
      write!(
        s,
        "\n    {}{}",
        if is_ext { &at_dimmed } else { &at_normal },
        format_frame::<AnsiColors>(frame, initial_cwd)
      )
      .unwrap();
    }
  }
  if let Some(cause) = &js_error.cause {
    let is_caused_by_circular = circular
      .as_ref()
      .map(|circular| js_error.is_same_error(circular.reference.from))
      .unwrap_or(false);

    let error_string = if is_caused_by_circular {
      colors::cyan(format!("[Circular *{}]", circular.unwrap().index))
        .to_string()
    } else {
      format_js_error_inner(
        cause,
        circular,
        false,
        false,
        vec![],
        initial_cwd,
        depth + 1,
      )
    };

    write!(
      s,
      "\nCaused by: {}",
      error_string.trim_start_matches("Uncaught ")
    )
    .unwrap();
  }
  if !suggestions.is_empty() {
    write!(s, "\n\n").unwrap();
    for (index, suggestion) in suggestions.iter().enumerate() {
      write!(s, "    ").unwrap();
      match suggestion.kind {
        FixSuggestionKind::Hint => {
          write!(s, "{} ", colors::cyan("hint:")).unwrap()
        }
        FixSuggestionKind::Info => {
          write!(s, "{} ", colors::yellow("info:")).unwrap()
        }
        FixSuggestionKind::Docs => {
          write!(s, "{} ", colors::green("docs:")).unwrap()
        }
      };
      match suggestion.message {
        FixSuggestionMessage::Single(msg) => {
          if matches!(suggestion.kind, FixSuggestionKind::Docs) {
            write!(s, "{}", cformat!("<u>{}</>", msg)).unwrap();
          } else {
            write!(s, "{}", msg).unwrap();
          }
        }
        FixSuggestionMessage::Multiline(messages) => {
          for (idx, message) in messages.iter().enumerate() {
            if idx != 0 {
              writeln!(s).unwrap();
              write!(s, "          ").unwrap();
            }
            write!(s, "{}", message).unwrap();
          }
        }
      }

      if index != (suggestions.len() - 1) {
        writeln!(s).unwrap();
      }
    }
  }

  s
}

fn get_suggestions_for_terminal_errors(e: &JsError) -> Vec<FixSuggestion<'_>> {
  let mut suggestions = get_message_suggestions(e);
  // A `__proto__` *write* silently no-ops and the breakage surfaces downstream
  // at an unrelated-looking line, so any later crash is reason enough to point
  // at the escape hatch. A `__proto__` *read* instead returns `undefined` and
  // blows up right at the access site, so we only nudge when `__proto__` is on
  // the crashing line/message, to avoid bothering programs that merely probed
  // `__proto__` once (feature detection) and then crashed for another reason.
  let info = if PROTO_SET_ATTEMPTED.load(Ordering::Relaxed) {
    Some(cstr!(
      "This program assigned to <u>Object.prototype.__proto__</>, which Deno disables by default."
    ))
  } else if PROTO_GET_ATTEMPTED.load(Ordering::Relaxed)
    && error_mentions_proto(e)
  {
    Some(cstr!(
      "This program read <u>Object.prototype.__proto__</>, which Deno disables by default (it returns <i>undefined</>)."
    ))
  } else {
    None
  };
  if let Some(info) = info {
    suggestions.push(FixSuggestion::info(info));
    suggestions.push(FixSuggestion::hint(cstr!(
      "If this caused the error, run again with <u>--unsafe-proto</> to restore it."
    )));
  }
  suggestions
}

fn error_mentions_proto(e: &JsError) -> bool {
  e.source_line
    .as_deref()
    .is_some_and(|l| l.contains("__proto__"))
    || e
      .message
      .as_deref()
      .is_some_and(|m| m.contains("__proto__"))
}

fn get_message_suggestions(e: &JsError) -> Vec<FixSuggestion<'_>> {
  get_message_suggestions_inner(e, 0)
}

fn get_message_suggestions_inner(
  e: &JsError,
  depth: usize,
) -> Vec<FixSuggestion<'_>> {
  if depth >= MAX_FORMAT_ERROR_DEPTH {
    return vec![];
  }

  if let Some(msg) = &e.message {
    if msg.contains("module is not defined")
      || msg.contains("exports is not defined")
      || msg.contains("require is not defined")
    {
      if let Some(file_name) =
        e.frames.first().and_then(|f| f.file_name.as_ref())
        && (file_name.ends_with(".mjs") || file_name.ends_with(".mts"))
      {
        return vec![];
      }
      return vec![
        FixSuggestion::info_multiline(&[
          cstr!(
            "Deno supports CommonJS modules in <u>.cjs</> files, or when the closest"
          ),
          cstr!(
            "<u>package.json</> has a <i>\"type\": \"commonjs\"</> option."
          ),
        ]),
        FixSuggestion::hint_multiline(&[
          "Rewrite this module to ESM,",
          cstr!("or change the file extension to <u>.cjs</u>,"),
          cstr!(
            "or add <u>package.json</> next to the file with <i>\"type\": \"commonjs\"</> option,"
          ),
          cstr!(
            "or pass <i>--unstable-detect-cjs</> flag to detect CommonJS when loading."
          ),
        ]),
        FixSuggestion::docs("https://docs.deno.com/go/commonjs"),
      ];
    } else if msg.contains("__filename is not defined") {
      return vec![
        FixSuggestion::info(cstr!(
          "<u>__filename</> global is not available in ES modules."
        )),
        FixSuggestion::hint(cstr!("Use <u>import.meta.filename</> instead.")),
      ];
    } else if msg.contains("__dirname is not defined") {
      return vec![
        FixSuggestion::info(cstr!(
          "<u>__dirname</> global is not available in ES modules."
        )),
        FixSuggestion::hint(cstr!("Use <u>import.meta.dirname</> instead.")),
      ];
    } else if msg.contains("openKv is not a function") {
      return vec![
        FixSuggestion::info("Deno.openKv() is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-kv` flag to enable this API.",
        ),
      ];
    } else if msg.contains("bundle is not a function") {
      return vec![
        FixSuggestion::info("Deno.bundle() is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-bundle` flag to enable this API.",
        ),
      ];
    } else if msg.contains("cron is not a function") {
      return vec![
        FixSuggestion::info("Deno.cron() is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-cron` flag to enable this API.",
        ),
      ];
    } else if msg.contains("WebSocketStream is not defined") {
      return vec![
        FixSuggestion::info("new WebSocketStream() is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-net` flag to enable this API.",
        ),
      ];
    } else if msg.contains("window is not defined") {
      return vec![
        FixSuggestion::info("window global is not available in Deno 2."),
        FixSuggestion::hint("Replace `window` with `globalThis`."),
      ];
    } else if msg.contains("UnsafeWindowSurface is not a constructor") {
      return vec![
        FixSuggestion::info("Deno.UnsafeWindowSurface is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-webgpu` flag to enable this API.",
        ),
      ];
    } else if msg.contains("QuicEndpoint is not a constructor") {
      return vec![
        FixSuggestion::info("listenQuic is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-net` flag to enable this API.",
        ),
      ];
    } else if msg.contains("connectQuic is not a function") {
      return vec![
        FixSuggestion::info("connectQuic is an unstable API."),
        FixSuggestion::hint(
          "Run again with `--unstable-net` flag to enable this API.",
        ),
      ];
    } else if msg.contains("invalid peer certificate: UnknownIssuer") {
      // The certificate chain isn't trusted by Deno's CA store (the default is
      // Mozilla's bundle). This commonly happens with `mkcert` dev certs or a
      // corporate TLS proxy, whose root is in the OS trust store but not
      // Mozilla's. See denoland/deno#25366.
      return vec![
        FixSuggestion::info(
          "The TLS certificate could not be verified against Deno's trusted certificate authorities.",
        ),
        FixSuggestion::hint_multiline(&[
          "If the certificate is trusted by your operating system (for example, issued",
          cstr!(
            "by <u>mkcert</> or a corporate proxy), run again with <u>DENO_TLS_CA_STORE=mozilla,system</>."
          ),
        ]),
        FixSuggestion::hint(cstr!(
          "Otherwise pass <u>--unsafely-ignore-certificate-errors</> to bypass verification (insecure)."
        )),
      ];
    } else if msg.contains("client error (Connect): invalid peer certificate") {
      return vec![FixSuggestion::hint(
        "Run again with the `--unsafely-ignore-certificate-errors` flag to bypass certificate errors.",
      )];
    // `isolated-vm` is a native addon built directly on V8's C++ internals,
    // which Deno does not expose. It fails either with a `Cannot find module
    // './out/isolated_vm'` error (when the addon was never built, the most
    // commonly reported case) or, if built, with the legacy native addon ABI
    // error from `ext/napi`. Either way it cannot run in Deno, so point users
    // at the supported isolation primitives. See denoland/deno#25130.
    } else if (msg.contains("isolated_vm") || msg.contains("isolated-vm"))
      && (msg.contains("Cannot find module")
        || msg.contains("legacy Node.js native addon API"))
    {
      return vec![
        FixSuggestion::info_multiline(&[
          "`isolated-vm` is a native addon built directly on V8's C++ internals,",
          "which Deno does not expose, so it cannot be loaded in Deno.",
        ]),
        FixSuggestion::hint_multiline(&[
          "To run code in a separate isolate, use a `Worker`: it executes in its",
          "own isolate and thread and can be sandboxed via the `deno.permissions`",
          "option. For in-process sandboxing, the `node:vm` module is also available.",
        ]),
      ];
    // Try to capture errors like:
    // ```
    // Uncaught Error: Cannot find module '../build/Release/canvas.node'
    // Require stack:
    // - /.../deno/npm/registry.npmjs.org/canvas/2.11.2/lib/bindings.js
    // - /.../.cache/deno/npm/registry.npmjs.org/canvas/2.11.2/lib/canvas.js
    // ```
    // as well as errors thrown by the `bindings` npm package (used by
    // libxmljs and many other native addons), like:
    // ```
    // Uncaught Error: Could not locate the bindings file. Tried:
    //  → /.../node_modules/libxmljs/build/xmljs.node
    //  → /.../node_modules/libxmljs/build/Release/xmljs.node
    // ```
    } else if (msg.contains("Cannot find module")
      && msg.contains("Require stack")
      && msg.contains(".node'"))
      || (msg.contains("Could not locate the bindings file")
        && msg.contains(".node"))
    {
      return vec![
        FixSuggestion::info_multiline(&[
          "Trying to execute an npm package using Node-API addons,",
          "these packages require local `node_modules` directory to be present.",
        ]),
        FixSuggestion::hint_multiline(&[
          "Add `\"nodeModulesDir\": \"auto\" option to `deno.json`, and then run",
          "`deno install --allow-scripts=npm:<package> --entrypoint <script>` to setup `node_modules` directory.",
        ]),
      ];
    // Captures the error thrown by `ext/napi` when a native addon was built
    // against the legacy Node.js native addon ABI (the `NODE_MODULE` macro /
    // `nan`) instead of Node-API. Such addons link against V8's C++ internals,
    // which Deno does not expose, so they cannot be loaded. See
    // denoland/deno#26034 (better-sqlite3) and denoland/deno#26656.
    } else if msg.contains("legacy Node.js native addon API") {
      // `better-sqlite3` is by far the most commonly reported offender, so
      // point users straight at drop-in Node-API alternatives.
      if msg.contains("better-sqlite3") || msg.contains("better_sqlite3") {
        return vec![
          FixSuggestion::info_multiline(&[
            "`better-sqlite3` is built on the legacy V8/nan native addon ABI,",
            "which depends on V8 internals that Deno does not expose.",
          ]),
          FixSuggestion::hint_multiline(&[
            "Use a Node-API based alternative instead, such as the built-in",
            "`node:sqlite` module, or the `npm:libsql` / `npm:@libsql/client`",
            "packages (the latter expose a `better-sqlite3`-compatible API).",
          ]),
        ];
      }
      return vec![
        FixSuggestion::info_multiline(&[
          "This native addon uses the legacy V8/nan addon ABI, which depends",
          "on V8 internals that Deno does not expose. Only Node-API (N-API)",
          "addons can be loaded by Deno.",
        ]),
        FixSuggestion::hint_multiline(&[
          "Switch to a package that uses Node-API (N-API), or ask the addon's",
          "authors to migrate it from the legacy `NODE_MODULE`/`nan` ABI.",
        ]),
      ];
    } else if msg.contains("document is not defined") {
      return vec![
        FixSuggestion::info(cstr!(
          "<u>document</> global is not available in Deno."
        )),
        FixSuggestion::hint_multiline(&[
          cstr!(
            "Use a library like <u>happy-dom</>, <u>deno_dom</>, <u>linkedom</> or <u>JSDom</>"
          ),
          cstr!(
            "and setup the <u>document</> global according to the library documentation."
          ),
        ]),
      ];
    }
  }

  // The actionable detail may live on the cause chain rather than the
  // top-level message — e.g. transport-level fetch failures are surfaced as
  // `TypeError: fetch failed` with the underlying error (such as a TLS
  // certificate failure) in `.cause`.
  if let Some(cause) = &e.cause {
    return get_message_suggestions_inner(cause, depth + 1);
  }

  vec![]
}

static SHOULD_FILTER_FRAMES: LazyLock<bool> =
  LazyLock::new(|| std::env::var("DENO_NO_FILTER_FRAMES").is_err());

/// Format a [`JsError`] for terminal output.
pub fn format_js_error(
  js_error: &JsError,
  initial_cwd: Option<&Url>,
) -> String {
  let circular =
    find_recursive_cause(js_error).map(|reference| IndexedErrorReference {
      reference,
      index: 1,
    });
  let suggestions = get_suggestions_for_terminal_errors(js_error);
  format_js_error_inner(
    js_error,
    circular,
    true,
    *SHOULD_FILTER_FRAMES,
    suggestions,
    initial_cwd,
    0,
  )
}

/// denext: set when the program runs in Node mode (`--unstable-node-errors`,
/// which the `node` shim and the node-args translation pass), so the CLI
/// reports an uncaught error with [`format_js_error_node`] instead of
/// [`format_js_error`].
pub static NODE_ERROR_FORMAT: AtomicBool = AtomicBool::new(false);

/// Whether uncaught errors are reported in Node's format (see
/// [`NODE_ERROR_FORMAT`]).
pub fn node_error_format() -> bool {
  NODE_ERROR_FORMAT.load(Ordering::Relaxed)
}

const NODE_UNHANDLED_REJECTION_MESSAGE: &str = "This error originated either by throwing inside of an async function without a catch block, or by rejecting a promise which was not handled with .catch(). The promise rejected with the reason";

/// denext: a [`JsError`] that ended the program, in the text Node.js prints to
/// stderr for an uncaught exception or an unhandled rejection: the location,
/// the source line and a caret, the error's `stack`, and the `Node.js v<ver>`
/// footer; a rejection with a value that is not an `Error` is Node's
/// `UnhandledPromiseRejection` report. No colour, as Node's when stderr is not
/// a terminal.
///
/// `read_file` reads a source file when the error carries no source line (a
/// syntax error from `vm`, a CommonJS module): the caller does the file system
/// access.
pub fn format_js_error_node(
  js_error: &JsError,
  node_version: &str,
  read_file: &dyn Fn(&std::path::Path) -> Option<String>,
) -> String {
  let footer = format!("\n\nNode.js v{node_version}\n");
  let in_promise = js_error
    .exception_message
    .starts_with("Uncaught (in promise) ");
  let is_error = js_error.name.is_some() || js_error.stack.is_some();
  if !is_error {
    // A thrown or rejected value that is not an `Error`.
    let value = js_error
      .exception_message
      .trim_start_matches("Uncaught (in promise) ")
      .trim_start_matches("Uncaught ");
    if in_promise {
      return format!(
        "UnhandledPromiseRejection: {NODE_UNHANDLED_REJECTION_MESSAGE} \"{value}\". {{\n  code: 'ERR_UNHANDLED_REJECTION'\n}}{footer}"
      );
    }
    return format!(
      "\n{value}\n(Use `node --trace-uncaught ...` to show where the exception was thrown){footer}"
    );
  }

  let body = match &js_error.stack {
    Some(stack) => stack.clone(),
    None => match (&js_error.name, &js_error.message) {
      (Some(name), Some(message)) if !message.is_empty() => {
        format!("{name}: {message}")
      }
      (Some(name), _) => name.clone(),
      (None, _) => js_error.exception_message.clone(),
    },
  };
  let body = node_stack_paths(&body, read_file);

  let header = node_error_source(js_error, in_promise, read_file)
    .map(|header| format!("{header}\n\n"))
    .unwrap_or_default();
  format!("{header}{body}{footer}")
}

/// `<location>:<line>\n<source line>\n<caret>` for the frame the error points
/// at, as Node prints it above the stack.
fn node_error_source(
  js_error: &JsError,
  in_promise: bool,
  read_file: &dyn Fn(&std::path::Path) -> Option<String>,
) -> Option<String> {
  let index = js_error.source_line_frame_index.unwrap_or(0);
  let frame = js_error.frames.get(index)?;
  let file_name = frame.file_name.as_deref()?;
  let line_number = frame.line_number?;
  let column = usize::try_from(frame.column_number.unwrap_or(1) - 1).ok()?;
  let file_path = node_file_path(file_name);
  let source_line = match &js_error.source_line {
    Some(line) => line.clone(),
    None => {
      let path = file_path.as_ref()?;
      let text = read_file(path)?;
      let line_index = usize::try_from(line_number - 1).ok()?;
      text.lines().nth(line_index)?.to_string()
    }
  };
  let location = match &file_path {
    Some(path) if !is_esm_path(path, read_file) => path.display().to_string(),
    _ => file_name.to_string(),
  };

  // The column is in UTF-16 code units.
  let mut units = 0;
  let mut prefix_end = source_line.len();
  for (byte_index, ch) in source_line.char_indices() {
    if units >= column {
      prefix_end = byte_index;
      break;
    }
    units += ch.len_utf16();
  }
  let mut prefix = &source_line[..prefix_end];
  // A synchronous throw: Node points at the `throw` statement, while the
  // error's own position is where it was constructed (`throw new Error()`).
  // A rejection points at the construction, as here.
  if !in_promise && js_error.name.as_deref() != Some("SyntaxError") {
    let trimmed = prefix.trim_end();
    if let Some(before) = trimmed.strip_suffix("throw")
      && !before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    {
      prefix = before;
    }
  }
  let caret: String = prefix
    .chars()
    .map(|c| if c == '\t' { '\t' } else { ' ' })
    .collect();
  Some(format!("{location}:{line_number}\n{source_line}\n{caret}^"))
}

/// The path of a `file:` URL or an absolute path, which Node names a
/// CommonJS module by.
fn node_file_path(file_name: &str) -> Option<std::path::PathBuf> {
  if file_name.starts_with("file:") {
    Url::parse(file_name).ok()?.to_file_path().ok()
  } else {
    let path = std::path::Path::new(file_name);
    path.is_absolute().then(|| path.to_path_buf())
  }
}

/// Whether Node loads the file as an ES module (and so names it by its
/// `file:` URL): `.mjs` / `.mts`, or a `.js` / `.ts` file whose nearest
/// `package.json` has `"type": "module"`.
fn is_esm_path(
  path: &std::path::Path,
  read_file: &dyn Fn(&std::path::Path) -> Option<String>,
) -> bool {
  match path.extension().and_then(|e| e.to_str()) {
    Some("mjs" | "mts") => return true,
    Some("cjs" | "cts") => return false,
    _ => {}
  }
  let mut dir = path.parent();
  while let Some(current) = dir {
    if let Some(text) = read_file(&current.join("package.json")) {
      return serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|pkg| pkg.get("type")?.as_str().map(|t| t == "module"))
        .unwrap_or(false);
    }
    dir = current.parent();
  }
  false
}

/// The stack with each CommonJS module's `file:` URL as its path, as in a
/// Node stack trace.
fn node_stack_paths(
  stack: &str,
  read_file: &dyn Fn(&std::path::Path) -> Option<String>,
) -> String {
  let mut out = String::with_capacity(stack.len());
  for (i, line) in stack.split('\n').enumerate() {
    if i > 0 {
      out.push('\n');
    }
    let Some(start) = line
      .find("file://")
      .filter(|_| line.trim_start().starts_with("at "))
    else {
      out.push_str(line);
      continue;
    };
    // `file:///a/b.js:1:2` up to `)` or the end of the line
    let rest = &line[start..];
    let end = rest.find(')').unwrap_or(rest.len());
    let location = &rest[..end];
    let url = location
      .rsplitn(3, ':')
      .nth(2)
      .filter(|url| url.len() > "file://".len())
      .unwrap_or(location);
    match node_file_path(url) {
      Some(path) if !is_esm_path(&path, read_file) => {
        out.push_str(&line[..start]);
        out.push_str(&path.display().to_string());
        out.push_str(&rest[url.len()..]);
      }
      _ => out.push_str(line),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use test_util::strip_ansi_codes;

  use super::*;

  fn js_error_with_cause(message: String, cause: Option<JsError>) -> JsError {
    JsError {
      name: Some("Error".to_string()),
      message: Some(message.clone()),
      stack: None,
      cause: cause.map(Box::new),
      exception_message: format!("Uncaught Error: {message}"),
      frames: vec![],
      source_line: None,
      source_line_frame_index: None,
      aggregated: None,
      additional_properties: vec![],
      stack_is_custom: false,
    }
  }

  #[test]
  fn test_format_none_source_line() {
    let actual = format_maybe_source_line(None, None, false, 0);
    assert_eq!(actual, "");
  }

  #[test]
  fn test_format_some_source_line() {
    let actual =
      format_maybe_source_line(Some("console.log('foo');"), Some(9), true, 0);
    assert_eq!(
      strip_ansi_codes(&actual),
      "\nconsole.log(\'foo\');\n        ^"
    );
  }

  #[test]
  fn test_format_source_line_utf16_column() {
    let actual = format_maybe_source_line(Some("\t😀foo();"), Some(4), true, 0);
    assert_eq!(strip_ansi_codes(&actual), "\n\t😀foo();\n\t  ^");
  }

  #[test]
  fn test_format_source_line_invalid_utf16_column() {
    let actual = format_maybe_source_line(Some("😀foo();"), Some(2), true, 0);
    assert_eq!(
      strip_ansi_codes(&actual),
      "\nWarning Couldn't format source line: Column 2 is out of bounds (source may have changed at runtime)"
    );
  }

  fn node_error(
    exception_message: &str,
    stack: Option<&str>,
    file: &str,
    line: i64,
    column: i64,
    source_line: Option<&str>,
  ) -> JsError {
    JsError {
      name: stack.map(|_| "TypeError".to_string()),
      message: stack.map(|_| "boom".to_string()),
      stack: stack.map(str::to_string),
      cause: None,
      exception_message: exception_message.to_string(),
      frames: vec![deno_core::error::JsStackFrame::from_location(
        Some(file.to_string()),
        Some(line),
        Some(column),
      )],
      source_line: source_line.map(str::to_string),
      source_line_frame_index: Some(0),
      aggregated: None,
      additional_properties: vec![],
      stack_is_custom: false,
    }
  }

  fn no_files(_: &std::path::Path) -> Option<String> {
    None
  }

  #[cfg(unix)]
  #[test]
  fn node_format_sync_throw_points_at_the_throw() {
    let error = node_error(
      "Uncaught TypeError: boom",
      Some(
        "TypeError: boom\n    at Object.<anonymous> (file:///app/main.js:2:9)",
      ),
      "file:///app/main.js",
      2,
      9,
      Some("  throw new TypeError(\"boom\");"),
    );
    assert_eq!(
      format_js_error_node(&error, "26.5.1", &no_files),
      "/app/main.js:2\n  throw new TypeError(\"boom\");\n  ^\n\nTypeError: boom\n    at Object.<anonymous> (/app/main.js:2:9)\n\nNode.js v26.5.1\n"
    );
  }

  #[cfg(unix)]
  #[test]
  fn node_format_rejection_points_at_the_construction() {
    let error = node_error(
      "Uncaught (in promise) TypeError: boom",
      Some("TypeError: boom\n    at file:///app/main.mjs:1:16"),
      "file:///app/main.mjs",
      1,
      16,
      Some("Promise.reject(new TypeError(\"boom\"));"),
    );
    assert_eq!(
      format_js_error_node(&error, "26.5.1", &no_files),
      "file:///app/main.mjs:1\nPromise.reject(new TypeError(\"boom\"));\n               ^\n\nTypeError: boom\n    at file:///app/main.mjs:1:16\n\nNode.js v26.5.1\n"
    );
  }

  #[cfg(unix)]
  #[test]
  fn node_format_type_module_package_keeps_urls() {
    let error = node_error(
      "Uncaught TypeError: boom",
      Some("TypeError: boom\n    at file:///app/main.js:1:7"),
      "file:///app/main.js",
      1,
      7,
      None,
    );
    let files = |path: &std::path::Path| match path.to_str()? {
      "/app/package.json" => Some(r#"{ "type": "module" }"#.to_string()),
      "/app/main.js" => Some("throw new TypeError(\"boom\");\n".to_string()),
      _ => None,
    };
    assert_eq!(
      format_js_error_node(&error, "26.5.1", &files),
      "file:///app/main.js:1\nthrow new TypeError(\"boom\");\n^\n\nTypeError: boom\n    at file:///app/main.js:1:7\n\nNode.js v26.5.1\n"
    );
  }

  #[test]
  fn node_format_values_that_are_not_errors() {
    let rejected = JsError {
      exception_message: "Uncaught (in promise) 42".to_string(),
      frames: vec![],
      source_line_frame_index: None,
      ..node_error("", None, "", 1, 1, None)
    };
    assert_eq!(
      format_js_error_node(&rejected, "26.5.1", &no_files),
      "UnhandledPromiseRejection: This error originated either by throwing inside of an async function without a catch block, or by rejecting a promise which was not handled with .catch(). The promise rejected with the reason \"42\". {\n  code: 'ERR_UNHANDLED_REJECTION'\n}\n\nNode.js v26.5.1\n"
    );
    let thrown = JsError {
      exception_message: "Uncaught str".to_string(),
      ..rejected
    };
    assert_eq!(
      format_js_error_node(&thrown, "26.5.1", &no_files),
      "\nstr\n(Use `node --trace-uncaught ...` to show where the exception was thrown)\n\nNode.js v26.5.1\n"
    );
  }

  #[test]
  fn test_format_js_error_truncates_deep_cause_chain() {
    let mut error = js_error_with_cause("0".to_string(), None);
    for i in 1..(MAX_FORMAT_ERROR_DEPTH + 5) {
      error = js_error_with_cause(i.to_string(), Some(error));
    }

    let formatted = format_js_error(&error, None);
    let formatted = strip_ansi_codes(&formatted);
    assert!(formatted.contains("[Error details truncated]"));
  }
}
