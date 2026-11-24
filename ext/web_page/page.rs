// Copyright 2018-2026 the Deno authors. MIT license.

//! `PageRuntime`: one page context.

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use deno_core::JsRuntime;
use deno_core::PollEventLoopOptions;
use deno_core::RuntimeOptions;
use deno_core::error::CoreErrorKind;
use deno_core::error::JsError;
use deno_core::v8;
use deno_fetch::CreateHttpClientOptions;
use deno_fetch::deny_addresses::AddressDenyList;
use deno_permissions::Permissions;
use deno_permissions::PermissionsContainer;
use deno_permissions::PermissionsOptions;
use deno_permissions::RuntimePermissionDescriptorParser;
use thalora_dom::Document;
use thalora_dom::NodeId;
use url::Url;

use crate::dom_ops::PageDom;
use crate::host_ops::BootstrapInfo;
use crate::host_ops::ConsoleMessage;
use crate::host_ops::Hooks;
use crate::host_ops::NavigationRequest;
use crate::host_ops::PageHost;

/// The addresses a page may not connect to unless the host says otherwise:
/// the classes `ext/fetch/deny_addresses.rs` names.
pub const DEFAULT_DENY_ADDRESSES: &[&str] =
  &["loopback", "private", "link-local", "metadata"];

/// The `net_allow` entry that allows every host (the deny list still
/// applies to the addresses they resolve to).
pub const NET_ALLOW_ANY: &str = "*";

/// The longest script `execute_script` and `evaluate_json` take, in bytes
/// (V8 cannot hold a longer string).
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;

/// How many uncaught errors are kept until the host takes them.
const MAX_UNCAUGHT_ERRORS: usize = 256;

/// What a page runtime is built from.
#[derive(Debug, Clone)]
pub struct PageOptions {
  /// The document URL: `location`, `document.URL`, the base of relative
  /// URLs and the origin web storage is keyed by.
  pub url: Url,
  /// The markup the document is parsed from.
  pub html: String,
  /// `navigator.userAgent` and the `User-Agent` of the page's requests.
  pub user_agent: String,
  /// The hosts the page may connect to, as `--allow-net` descriptors
  /// (`example.com`, `example.com:8443`, `10.0.0.0/8`, ...). Empty allows
  /// none; [`NET_ALLOW_ANY`] allows every host. Every other permission is
  /// denied, with no prompt.
  pub net_allow: Vec<String>,
  /// The addresses the page's connections refuse, as `denyAddresses`
  /// entries (classes or CIDR blocks), checked after DNS resolution and on
  /// every redirect. `None` is [`DEFAULT_DENY_ADDRESSES`]; an empty list
  /// lifts the check (test builds with loopback servers).
  pub deny_addresses: Option<Vec<String>>,
  /// The profile's storage directory. `localStorage` lives in a
  /// subdirectory named after the page's origin; `None`, or an opaque
  /// origin, leaves `localStorage` unavailable.
  pub storage_dir: Option<PathBuf>,
  /// The V8 heap limit of the page, in bytes. Reaching it ends the page
  /// ([`PageError::OutOfMemory`]), not the process.
  pub heap_limit_bytes: Option<usize>,
  /// Seeds `crypto.getRandomValues` and `crypto.randomUUID` (deterministic
  /// runs).
  pub seed: Option<u64>,
}

impl PageOptions {
  /// Options for `html` at `url`: no network, the default deny list, no
  /// storage, no heap limit.
  pub fn new(url: Url, html: impl Into<String>) -> Self {
    Self {
      url,
      html: html.into(),
      user_agent: String::new(),
      net_allow: Vec::new(),
      deny_addresses: None,
      storage_dir: None,
      heap_limit_bytes: None,
      seed: None,
    }
  }
}

/// A JavaScript error: what was thrown, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsErrorInfo {
  /// The message, as V8 reports an uncaught exception
  /// (`Uncaught TypeError: ...`).
  pub message: String,
  /// The stack, when the thrown value had one.
  pub stack: Option<String>,
}

impl From<JsError> for JsErrorInfo {
  fn from(error: JsError) -> Self {
    Self {
      message: error.exception_message,
      stack: error.stack,
    }
  }
}

impl std::fmt::Display for JsErrorInfo {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &self.stack {
      Some(stack) => f.write_str(stack),
      None => f.write_str(&self.message),
    }
  }
}

/// Why a page runtime call failed.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
  /// `PageRuntime::new` was called outside a tokio runtime. V8's delayed
  /// tasks and the page's timers are scheduled on it.
  #[error("A page runtime must be created inside a tokio runtime")]
  NoTokioRuntime,
  /// An option could not be used (a bad `net_allow` or `deny_addresses`
  /// entry).
  #[error("Invalid page option: {0}")]
  InvalidOptions(String),
  /// The page context could not be set up.
  #[error("The page context failed to start: {0}")]
  Bootstrap(String),
  /// JavaScript threw and nothing caught it.
  #[error("{0}")]
  Js(JsErrorInfo),
  /// Execution was ended through `terminate_handle()`.
  #[error("The page's execution was terminated")]
  Terminated,
  /// The page reached its heap limit. The runtime is finished: every later
  /// call fails the same way.
  #[error("The page ran out of memory")]
  OutOfMemory,
  /// The node id does not name a live node of this page's document.
  #[error("The node does not exist")]
  InvalidNode,
  /// `set_value` was called on a node that has no value to set.
  #[error("The node is not a form control")]
  NotAFormControl,
  /// The script is longer than V8 can hold.
  #[error("The script is too long")]
  SourceTooLarge,
  /// The event loop failed for a reason other than a JavaScript error.
  #[error("The page's event loop failed: {0}")]
  Runtime(String),
}

/// How `run_until_idle` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idle {
  /// Nothing is left to run: no pending op, timer or task.
  Quiescent,
  /// The budget ran out with work still pending (an interval, a long
  /// request, a timer further out).
  BudgetExhausted,
}

/// What a click did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickOutcome {
  /// A listener called `preventDefault()`: no default action ran.
  pub default_prevented: bool,
  /// The navigation the click asked for, if any: a followed link, or a form
  /// submission (a submit button was clicked and no `submit` listener
  /// canceled it). The host performs it.
  pub navigation: Option<NavigationRequest>,
}

/// The live state of a form control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormState {
  /// The control's current value (what the user would see), which the
  /// `value` content attribute does not reflect once it changed.
  pub value: String,
  /// The checkedness of a checkbox or radio button, the selectedness of an
  /// option; false for everything else.
  pub checked: bool,
}

#[derive(Clone, Copy)]
enum Hook {
  Click,
  SetValue,
  FormState,
  Lifecycle,
  Serialize,
}

enum HookArg<'a> {
  Number(u32),
  Text(&'a str),
  Value(&'a v8::Global<v8::Value>),
}

#[derive(Clone, Copy)]
enum Want {
  Bool,
  Text,
}

enum HookValue {
  Bool(bool),
  Text(Option<String>),
}

/// One page: a V8 isolate with the web platform, a DOM over
/// [`thalora_dom::Document`], and nothing else. No `Deno` namespace, no
/// Node, no file system, environment, subprocess, FFI or listening socket
/// ops: they are not registered, so no bug in page JavaScript can reach
/// them.
///
/// A `PageRuntime` is bound to the thread that created it, and must be
/// created and used inside a (current-thread) tokio runtime.
pub struct PageRuntime {
  // Dropped before the runtime: these hold V8 handles.
  hooks: Rc<Hooks>,
  dom: Rc<RefCell<Document>>,
  out_of_memory: Arc<AtomicBool>,
  uncaught: Vec<JsErrorInfo>,
  runtime: JsRuntime,
}

fn build_permissions(
  net_allow: &[String],
) -> Result<PermissionsContainer, PageError> {
  let parser = Arc::new(RuntimePermissionDescriptorParser::new(
    sys_traits::impls::RealSys,
  ));
  // An empty allow list means "everything" to deno_permissions, so "no
  // network" has to be the absence of the list.
  let allow_net = if net_allow.iter().any(|entry| entry == NET_ALLOW_ANY) {
    Some(Vec::new())
  } else if net_allow.is_empty() {
    None
  } else {
    Some(net_allow.to_vec())
  };
  let deny_all = || Some(Vec::new());
  let options = PermissionsOptions {
    allow_net,
    deny_env: deny_all(),
    deny_ffi: deny_all(),
    deny_read: deny_all(),
    deny_run: deny_all(),
    deny_sys: deny_all(),
    deny_write: deny_all(),
    deny_import: deny_all(),
    prompt: false,
    ..Default::default()
  };
  let permissions = Permissions::from_options(parser.as_ref(), &options)
    .map_err(|err| PageError::InvalidOptions(format!("net_allow: {err}")))?;
  Ok(PermissionsContainer::new(parser, permissions))
}

fn build_deny_list(
  deny_addresses: Option<&[String]>,
) -> Result<Option<Arc<AddressDenyList>>, PageError> {
  let entries: Vec<String> = match deny_addresses {
    Some(entries) => entries.to_vec(),
    None => DEFAULT_DENY_ADDRESSES
      .iter()
      .map(|entry| entry.to_string())
      .collect(),
  };
  let list = AddressDenyList::parse(&entries).map_err(|err| {
    PageError::InvalidOptions(format!("deny_addresses: {err}"))
  })?;
  Ok((!list.is_empty()).then(|| Arc::new(list)))
}

/// The directory of an origin's storage under `base`: the origin's ASCII
/// serialization with every byte outside `[A-Za-z0-9.-]` written as `_xx`,
/// which no two origins share. `None` for an opaque origin.
fn origin_storage_dir(base: &Path, url: &Url) -> Option<PathBuf> {
  let origin = url.origin();
  if !origin.is_tuple() {
    return None;
  }
  let mut name = String::new();
  for byte in origin.ascii_serialization().bytes() {
    if byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-' {
      name.push(char::from(byte));
    } else {
      name.push_str(&format!("_{byte:02x}"));
    }
  }
  Some(base.join(name))
}

/// A script name V8 and deno_core accept: 7-bit ASCII.
fn script_name(name: &str) -> String {
  name
    .chars()
    .map(|c| {
      if c.is_ascii() && !c.is_ascii_control() {
        c
      } else {
        '_'
      }
    })
    .collect()
}

impl PageRuntime {
  /// Parses `options.html` into a document and boots the page context
  /// around it. No script of the page has run when this returns.
  pub fn new(options: PageOptions) -> Result<Self, PageError> {
    if tokio::runtime::Handle::try_current().is_err() {
      return Err(PageError::NoTokioRuntime);
    }

    let permissions = build_permissions(&options.net_allow)?;
    let deny_addresses = build_deny_list(options.deny_addresses.as_deref())?;

    // rustls wants a process-wide crypto provider before a TLS client is
    // built. The deno CLI installs one at startup; an embedder that has not
    // gets the same one here (aws-lc-rs), and one already installed is kept.
    if deno_tls::rustls::crypto::CryptoProvider::get_default().is_none() {
      let _ = deno_tls::rustls::crypto::aws_lc_rs::default_provider()
        .install_default();
    }

    // The page's only HTTP client. `deno_fetch` uses a client found in the
    // op state before it would build its own, so every `fetch()` goes
    // through this one and its deny list.
    let client = deno_fetch::create_http_client(
      &options.user_agent,
      CreateHttpClientOptions {
        root_cert_store: Some(deno_tls::create_default_root_cert_store()),
        permissions: Some(permissions.clone()),
        resolved_deny_check_kind: deno_fetch::dns::ResolvedDenyCheckKind::Net,
        deny_addresses,
        ..Default::default()
      },
    )
    .map_err(|err| PageError::Bootstrap(err.to_string()))?;

    let dom = Rc::new(RefCell::new(Document::parse_html(&options.html)));
    let host = PageHost::new(BootstrapInfo {
      url: options.url.to_string(),
      user_agent: options.user_agent.clone(),
    });
    let storage_dir = options
      .storage_dir
      .as_deref()
      .and_then(|base| origin_storage_dir(base, &options.url));

    let create_params = options
      .heap_limit_bytes
      .map(|limit| v8::CreateParams::default().heap_limits(0, limit));

    let mut runtime = JsRuntime::try_new(RuntimeOptions {
      extensions: vec![
        deno_webidl::deno_webidl::init(),
        deno_web::deno_web::init(
          deno_web::BlobStore::default_arc(),
          Some(options.url.clone()),
          false,
          Default::default(),
        ),
        deno_fetch::deno_fetch::init(deno_fetch::Options {
          user_agent: options.user_agent.clone(),
          ..Default::default()
        }),
        deno_webstorage::deno_webstorage::init(storage_dir),
        deno_crypto::deno_crypto::init(options.seed),
        crate::deno_web_page::init(
          PageDom(dom.clone()),
          host,
          permissions,
          client,
        ),
      ],
      create_params,
      ..Default::default()
    })
    .map_err(|err| PageError::Bootstrap(err.to_string()))?;

    let out_of_memory = Arc::new(AtomicBool::new(false));
    if options.heap_limit_bytes.is_some() {
      let flag = out_of_memory.clone();
      let handle = runtime.v8_isolate().thread_safe_handle();
      runtime.add_near_heap_limit_callback(move |current, _initial| {
        // V8 calls this instead of aborting the process. End the page, and
        // give V8 the room it needs to unwind.
        flag.store(true, Ordering::SeqCst);
        handle.terminate_execution();
        current.saturating_mul(2)
      });
    }

    let hooks = runtime
      .op_state()
      .borrow()
      .borrow::<PageHost>()
      .hooks
      .clone()
      .ok_or_else(|| {
        PageError::Bootstrap("the page context registered no hooks".into())
      })?;

    Ok(Self {
      hooks,
      dom,
      out_of_memory,
      uncaught: Vec::new(),
      runtime,
    })
  }

  /// Runs a classic script in the page's global scope, then the microtasks
  /// it queued. An exception nothing caught comes back as
  /// [`PageError::Js`], with its message and stack.
  pub fn execute_script(
    &mut self,
    name: &str,
    source: String,
  ) -> Result<(), PageError> {
    self.run_script(name, source).map(|_| ())
  }

  /// Evaluates a script and returns its completion value as JSON:
  /// `undefined` becomes `null`, and a value JSON cannot express (a
  /// function, a symbol, a cyclic object, a BigInt) becomes its string form.
  /// A promise is not awaited.
  pub fn evaluate_json(
    &mut self,
    source: String,
  ) -> Result<serde_json::Value, PageError> {
    let value = self.run_script("<evaluate>", source)?;
    let text = match self.call_hook(
      Hook::Serialize,
      &[HookArg::Value(&value)],
      Want::Text,
    )? {
      HookValue::Text(Some(text)) => text,
      _ => return Ok(serde_json::Value::Null),
    };
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
  }

  /// Pumps the event loop until nothing is pending (ops, timers, tasks) or
  /// `budget` has passed, whichever comes first. An uncaught error in a
  /// task does not stop the pumping: it is kept for
  /// [`PageRuntime::take_uncaught_errors`].
  ///
  /// The budget is checked between tasks. A task that never returns is not
  /// interrupted by it: that is what [`PageRuntime::terminate_handle`] is
  /// for.
  pub async fn run_until_idle(
    &mut self,
    budget: Duration,
  ) -> Result<Idle, PageError> {
    self.ensure_alive()?;
    let deadline = tokio::time::Instant::now() + budget;
    loop {
      let result = tokio::time::timeout_at(
        deadline,
        self.runtime.run_event_loop(PollEventLoopOptions::default()),
      )
      .await;
      match result {
        Err(_elapsed) => return Ok(Idle::BudgetExhausted),
        Ok(Ok(())) => return Ok(Idle::Quiescent),
        Ok(Err(err)) => match err.into_kind() {
          CoreErrorKind::Js(js_error) => match self.classify(*js_error) {
            PageError::Js(info) => self.record_uncaught(info),
            fatal => return Err(fatal),
          },
          other => return Err(PageError::Runtime(other.to_string())),
        },
      }
    }
  }

  /// Gives `f` the page's document, as it is now: every mutation page
  /// JavaScript made is in it. `f` must not call back into this runtime.
  pub fn with_document<R>(&self, f: impl FnOnce(&Document) -> R) -> R {
    f(&self.dom.borrow())
  }

  /// Clicks a node: dispatches a trusted `click` at it and runs the default
  /// action, as a user's click would. A checkbox or radio button changes
  /// state and fires `input` and `change`; a submit button submits its form
  /// and a link is followed, both reported in the outcome for the host to
  /// perform.
  pub fn click(&mut self, node: NodeId) -> Result<ClickOutcome, PageError> {
    self.ensure_alive()?;
    self.ensure_node(node)?;
    let first_new = self.host(|host| host.next_navigation);
    let not_canceled = self.call_hook(
      Hook::Click,
      &[
        HookArg::Number(node.index()),
        HookArg::Number(node.generation()),
      ],
      Want::Bool,
    )?;
    // Of the navigations the click started, the last one wins, as in a
    // browser. Earlier requests stay queued for the host.
    let navigation = self.host(|host| {
      let mut last = None;
      while host
        .navigations
        .back()
        .is_some_and(|(sequence, _)| *sequence >= first_new)
      {
        if let Some((_, request)) = host.navigations.pop_back() {
          last.get_or_insert(request);
        }
      }
      last
    });
    Ok(ClickOutcome {
      default_prevented: matches!(not_canceled, HookValue::Bool(false)),
      navigation,
    })
  }

  /// Sets the value of an `<input>`, `<textarea>` or `<select>` as typing
  /// or choosing would, then fires `input` and `change` at it.
  pub fn set_value(
    &mut self,
    node: NodeId,
    value: &str,
  ) -> Result<(), PageError> {
    self.ensure_alive()?;
    self.ensure_node(node)?;
    match self.call_hook(
      Hook::SetValue,
      &[
        HookArg::Number(node.index()),
        HookArg::Number(node.generation()),
        HookArg::Text(value),
      ],
      Want::Bool,
    )? {
      HookValue::Bool(true) => Ok(()),
      _ => Err(PageError::NotAFormControl),
    }
  }

  /// The live value and checkedness of a form control (`<input>`,
  /// `<textarea>`, `<select>`, `<option>`, `<button>`), which the document's
  /// attributes do not show. `None` for any other node.
  pub fn form_state(
    &mut self,
    node: NodeId,
  ) -> Result<Option<FormState>, PageError> {
    self.ensure_alive()?;
    self.ensure_node(node)?;
    let HookValue::Text(Some(text)) = self.call_hook(
      Hook::FormState,
      &[
        HookArg::Number(node.index()),
        HookArg::Number(node.generation()),
      ],
      Want::Text,
    )?
    else {
      return Ok(None);
    };
    // The hook answers "0" or "1" (the checkedness) followed by the value.
    let checked = text.starts_with('1');
    let value = text.get(1..).unwrap_or_default().to_string();
    Ok(Some(FormState { value, checked }))
  }

  /// The document finished parsing: `readyState` becomes `interactive` and
  /// `DOMContentLoaded` fires at the document. The host calls this once
  /// the page's scripts have run.
  pub fn fire_dom_content_loaded(&mut self) -> Result<(), PageError> {
    self.lifecycle(0)
  }

  /// The page finished loading: `readyState` becomes `complete` and `load`
  /// fires at the window.
  pub fn fire_load(&mut self) -> Result<(), PageError> {
    self.lifecycle(1)
  }

  /// A handle that ends the page's running JavaScript from another thread
  /// (`terminate_execution()`): what a watchdog holds. The call that was
  /// running fails with [`PageError::Terminated`], and the runtime stays
  /// usable.
  pub fn terminate_handle(&mut self) -> v8::IsolateHandle {
    self.runtime.v8_isolate().thread_safe_handle()
  }

  /// What the page wrote to its console since the last call.
  pub fn take_console_messages(&mut self) -> Vec<ConsoleMessage> {
    self.host(|host| host.console.drain(..).collect())
  }

  /// The errors nothing caught in event loop tasks (timers, promise jobs,
  /// event listeners) since the last call. Errors reported through the
  /// page's `error` event are written to the console instead.
  pub fn take_uncaught_errors(&mut self) -> Vec<JsErrorInfo> {
    std::mem::take(&mut self.uncaught)
  }

  /// The navigations the page asked for outside a `click()` (a script
  /// setting `location.href`, calling `form.submit()`, ...) since the last
  /// call, oldest first.
  pub fn take_navigation_requests(&mut self) -> Vec<NavigationRequest> {
    self.host(|host| {
      host
        .navigations
        .drain(..)
        .map(|(_, request)| request)
        .collect()
    })
  }

  /// The name of every op registered in this runtime, cppgc object members
  /// included. Page JavaScript reaches none of them directly; the test
  /// suite holds the list against an allowlist.
  pub fn op_names(&self) -> Vec<&'static str> {
    self.runtime.op_names()
  }

  fn lifecycle(&mut self, stage: u32) -> Result<(), PageError> {
    self.ensure_alive()?;
    self
      .call_hook(Hook::Lifecycle, &[HookArg::Number(stage)], Want::Bool)
      .map(|_| ())
  }

  fn host<R>(&mut self, f: impl FnOnce(&mut PageHost) -> R) -> R {
    let state = self.runtime.op_state();
    let mut state = state.borrow_mut();
    f(state.borrow_mut::<PageHost>())
  }

  fn ensure_alive(&self) -> Result<(), PageError> {
    if self.out_of_memory.load(Ordering::SeqCst) {
      return Err(PageError::OutOfMemory);
    }
    Ok(())
  }

  fn ensure_node(&self, node: NodeId) -> Result<(), PageError> {
    if self.dom.borrow().is_valid(node) {
      Ok(())
    } else {
      Err(PageError::InvalidNode)
    }
  }

  fn record_uncaught(&mut self, info: JsErrorInfo) {
    if self.uncaught.len() >= MAX_UNCAUGHT_ERRORS {
      self.uncaught.remove(0);
    }
    self.uncaught.push(info);
  }

  /// Turns an error deno_core reported into ours, and leaves the isolate
  /// able to run JavaScript again (an ended execution stays "terminating"
  /// until it is canceled).
  fn classify(&mut self, error: JsError) -> PageError {
    self.runtime.v8_isolate().cancel_terminate_execution();
    if self.out_of_memory.load(Ordering::SeqCst) {
      PageError::OutOfMemory
    } else if error.exception_message.ends_with("execution terminated") {
      PageError::Terminated
    } else {
      PageError::Js(error.into())
    }
  }

  /// Runs the microtasks the last script or hook queued, as a browser does
  /// when the JavaScript stack empties.
  fn checkpoint(&mut self) -> Result<(), PageError> {
    {
      // With the page's context entered, as deno_core runs its own.
      deno_core::scope!(scope, self.runtime);
      scope.perform_microtask_checkpoint();
    }
    let isolate = self.runtime.v8_isolate();
    if isolate.is_execution_terminating() {
      isolate.cancel_terminate_execution();
      return Err(if self.out_of_memory.load(Ordering::SeqCst) {
        PageError::OutOfMemory
      } else {
        PageError::Terminated
      });
    }
    Ok(())
  }

  fn run_script(
    &mut self,
    name: &str,
    source: String,
  ) -> Result<v8::Global<v8::Value>, PageError> {
    self.ensure_alive()?;
    if source.len() > MAX_SOURCE_BYTES {
      return Err(PageError::SourceTooLarge);
    }
    match self.runtime.execute_script(script_name(name), source) {
      Ok(value) => {
        self.checkpoint()?;
        Ok(value)
      }
      Err(error) => Err(self.classify(*error)),
    }
  }

  fn call_hook(
    &mut self,
    hook: Hook,
    args: &[HookArg<'_>],
    want: Want,
  ) -> Result<HookValue, PageError> {
    let hooks = self.hooks.clone();
    let function = match hook {
      Hook::Click => &hooks.click,
      Hook::SetValue => &hooks.set_value,
      Hook::FormState => &hooks.form_state,
      Hook::Lifecycle => &hooks.lifecycle,
      Hook::Serialize => &hooks.serialize,
    };
    let result: Result<HookValue, Box<JsError>> = {
      deno_core::scope!(scope, self.runtime);
      v8::tc_scope!(let scope, scope);
      let function = v8::Local::new(scope, function);
      let mut argv: Vec<v8::Local<v8::Value>> = Vec::with_capacity(args.len());
      let mut too_long = false;
      for arg in args {
        argv.push(match arg {
          HookArg::Number(number) => {
            v8::Integer::new_from_unsigned(scope, *number).into()
          }
          HookArg::Text(text) => match v8::String::new(scope, text) {
            Some(string) => string.into(),
            None => {
              too_long = true;
              v8::undefined(scope).into()
            }
          },
          HookArg::Value(value) => v8::Local::new(scope, *value),
        });
      }
      if too_long {
        return Err(PageError::SourceTooLarge);
      }
      let receiver = v8::undefined(scope).into();
      match function.call(scope, receiver, &argv) {
        Some(value) => Ok(match want {
          Want::Bool => HookValue::Bool(value.is_true()),
          Want::Text => HookValue::Text(
            value.is_string().then(|| value.to_rust_string_lossy(scope)),
          ),
        }),
        None => {
          // No exception object means execution was terminated.
          let exception = scope
            .exception()
            .unwrap_or_else(|| v8::undefined(scope).into());
          Err(deno_core::error::exception_to_err(
            scope, exception, false, true,
          ))
        }
      }
    };
    match result {
      Ok(value) => {
        self.checkpoint()?;
        Ok(value)
      }
      Err(error) => Err(self.classify(*error)),
    }
  }
}
