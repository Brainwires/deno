// Copyright 2018-2026 the Deno authors. MIT license.

//! The ops between the page context and its host: bootstrap information, the
//! hooks the host calls into the page with, the console sink and navigation
//! requests. Like every op of this crate they are only reachable from the
//! extension's own modules: `99_page_main.js` removes `Deno` and
//! `__bootstrap` before any page script runs.

use std::collections::VecDeque;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;
use deno_error::JsErrorBox;
use serde::Deserialize;
use serde::Serialize;

/// How many console messages are kept until the host takes them. A page that
/// logs in a loop pushes the oldest ones out.
const MAX_CONSOLE_MESSAGES: usize = 1000;
/// The longest console message kept, in bytes. Longer ones are cut.
const MAX_CONSOLE_MESSAGE_BYTES: usize = 16 * 1024;
/// How many navigation requests are kept until the host takes them.
const MAX_NAVIGATION_REQUESTS: usize = 64;

/// The severity of a console message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLevel {
  /// `console.debug`.
  Debug,
  /// `console.log`, `console.info` and the rest.
  Info,
  /// `console.warn`.
  Warn,
  /// `console.error`, `console.assert`, `console.trace`.
  Error,
}

/// One message a page wrote to its console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleMessage {
  /// The severity.
  pub level: ConsoleLevel,
  /// The formatted text, without the trailing newline.
  pub text: String,
}

/// A form submission a page asked for. The host performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSubmission {
  /// The resolved action URL (the form's `action`, or the submitter's
  /// `formaction`, against the document URL).
  pub action: String,
  /// `get`, `post` or `dialog`, lowercased.
  pub method: String,
  /// The encoding type (`application/x-www-form-urlencoded`,
  /// `multipart/form-data` or `text/plain`).
  pub enctype: String,
  /// The browsing context name (`target`), empty for the current one.
  pub target: String,
  /// The entry list, in tree order: name and value of each successful
  /// control, the submitter included.
  pub entries: Vec<(String, String)>,
}

/// A navigation a page asked for. `PageRuntime` never navigates: the host
/// decides what to do with the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationRequest {
  /// A hyperlink was followed (the activation behavior of `<a href>`).
  Link {
    /// The resolved URL.
    url: String,
    /// The `target` attribute, empty for the current browsing context.
    target: String,
  },
  /// A form was submitted (a submit button was activated, or `submit()` /
  /// `requestSubmit()` was called).
  Form(FormSubmission),
  /// `location.href = ...`, `location.assign()` or `location.replace()`.
  Location {
    /// The resolved URL.
    url: String,
    /// Whether the current history entry is to be replaced.
    replace: bool,
  },
  /// `location.reload()`.
  Reload,
}

/// A navigation request as `03_window.js` and `02_html.js` send it: one flat
/// object, `kind` saying which fields count.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NavigationWire {
  kind: String,
  url: String,
  target: String,
  replace: bool,
  method: String,
  enctype: String,
  entries: Vec<(String, String)>,
}

impl Default for NavigationWire {
  fn default() -> Self {
    Self {
      kind: String::new(),
      url: String::new(),
      target: String::new(),
      replace: false,
      method: "get".to_string(),
      enctype: "application/x-www-form-urlencoded".to_string(),
      entries: Vec::new(),
    }
  }
}

impl NavigationWire {
  fn into_request(self) -> Option<NavigationRequest> {
    Some(match self.kind.as_str() {
      "link" => NavigationRequest::Link {
        url: self.url,
        target: self.target,
      },
      "form" => NavigationRequest::Form(FormSubmission {
        action: self.url,
        method: self.method,
        enctype: self.enctype,
        target: self.target,
        entries: self.entries,
      }),
      "location" => NavigationRequest::Location {
        url: self.url,
        replace: self.replace,
      },
      "reload" => NavigationRequest::Reload,
      _ => return None,
    })
  }
}

/// What the page context is told about itself when it boots.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BootstrapInfo {
  pub url: String,
  pub user_agent: String,
}

/// The functions `99_page_main.js` hands to the host. They are closures over
/// the extension's module scope, never reachable from page JavaScript.
pub(crate) struct Hooks {
  pub click: v8::Global<v8::Function>,
  pub set_value: v8::Global<v8::Function>,
  pub form_state: v8::Global<v8::Function>,
  pub lifecycle: v8::Global<v8::Function>,
  pub serialize: v8::Global<v8::Function>,
}

/// Host-side state of one page, kept in `OpState`.
pub(crate) struct PageHost {
  pub info: BootstrapInfo,
  pub hooks: Option<Rc<Hooks>>,
  pub console: VecDeque<ConsoleMessage>,
  /// Navigation requests with the sequence number each was made under.
  pub navigations: VecDeque<(u64, NavigationRequest)>,
  pub next_navigation: u64,
}

impl PageHost {
  pub fn new(info: BootstrapInfo) -> Self {
    Self {
      info,
      hooks: None,
      console: VecDeque::new(),
      navigations: VecDeque::new(),
      next_navigation: 0,
    }
  }
}

#[op2]
#[serde]
pub fn op_web_page_bootstrap_info(state: &OpState) -> BootstrapInfo {
  state.borrow::<PageHost>().info.clone()
}

fn hook<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  hooks: v8::Local<'s, v8::Object>,
  name: &str,
) -> Result<v8::Global<v8::Function>, JsErrorBox> {
  let missing = || JsErrorBox::type_error(format!("Missing hook: {name}"));
  let key = v8::String::new(scope, name).ok_or_else(missing)?;
  let value = hooks.get(scope, key.into()).ok_or_else(missing)?;
  let function =
    v8::Local::<v8::Function>::try_from(value).map_err(|_| missing())?;
  Ok(v8::Global::new(scope, function))
}

/// Called once by `99_page_main.js`. A second call is refused, so the hooks
/// the host calls can never be replaced.
#[op2(fast)]
pub fn op_web_page_register_hooks<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  state: &mut OpState,
  hooks: v8::Local<'s, v8::Value>,
) -> Result<(), JsErrorBox> {
  let host = state.borrow_mut::<PageHost>();
  if host.hooks.is_some() {
    return Err(JsErrorBox::type_error("The hooks are already registered"));
  }
  let hooks = v8::Local::<v8::Object>::try_from(hooks)
    .map_err(|_| JsErrorBox::type_error("The hooks must be an object"))?;
  host.hooks = Some(Rc::new(Hooks {
    click: hook(scope, hooks, "click")?,
    set_value: hook(scope, hooks, "setValue")?,
    form_state: hook(scope, hooks, "formState")?,
    lifecycle: hook(scope, hooks, "lifecycle")?,
    serialize: hook(scope, hooks, "serialize")?,
  }));
  Ok(())
}

/// The print function of the page's `console`: nothing a page logs reaches
/// the host's stdout or stderr.
#[op2(fast)]
pub fn op_web_page_console(
  state: &mut OpState,
  #[string] message: &str,
  level: u32,
) {
  let host = state.borrow_mut::<PageHost>();
  let mut end = message.len().min(MAX_CONSOLE_MESSAGE_BYTES);
  while !message.is_char_boundary(end) {
    end -= 1;
  }
  let text = message[..end].trim_end_matches('\n').to_string();
  let level = match level {
    0 => ConsoleLevel::Debug,
    1 => ConsoleLevel::Info,
    2 => ConsoleLevel::Warn,
    _ => ConsoleLevel::Error,
  };
  if host.console.len() >= MAX_CONSOLE_MESSAGES {
    host.console.pop_front();
  }
  host.console.push_back(ConsoleMessage { level, text });
}

#[op2]
pub fn op_web_page_navigate(
  state: &mut OpState,
  #[serde] request: NavigationWire,
) {
  let host = state.borrow_mut::<PageHost>();
  let Some(request) = request.into_request() else {
    return;
  };
  if host.navigations.len() >= MAX_NAVIGATION_REQUESTS {
    host.navigations.pop_front();
  }
  let sequence = host.next_navigation;
  host.next_navigation += 1;
  host.navigations.push_back((sequence, request));
}
