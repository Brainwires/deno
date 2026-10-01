// Copyright 2018-2026 the Deno authors. MIT license.

//! Desktop window management ops for `deno compile --desktop`.
//!
//! These ops are included in the V8 snapshot so their external references
//! are stable. When `DesktopApi` is not present in OpState (non-desktop
//! builds), the ops silently no-op.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use deno_core::FromV8;
use deno_core::OpState;
use deno_core::ToV8;
use deno_core::cppgc::SameObject;
use deno_core::op2;
use deno_core::v8;

/// Thread-safe intermediate value type for crossing the WEF ↔ Deno boundary.
/// Converts directly to V8 values without going through serde.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopValue {
  Null,
  Bool(bool),
  Int(i32),
  Double(f64),
  String(String),
  List(Vec<DesktopValue>),
  Dict(Vec<(String, DesktopValue)>),
  Binary(Vec<u8>),
}

impl<'a> ToV8<'a> for DesktopValue {
  type Error = std::convert::Infallible;

  fn to_v8(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(match self {
      DesktopValue::Null => v8::null(scope).into(),
      DesktopValue::Bool(b) => v8::Boolean::new(scope, b).into(),
      DesktopValue::Int(i) => v8::Integer::new(scope, i).into(),
      DesktopValue::Double(d) => v8::Number::new(scope, d).into(),
      DesktopValue::String(s) => v8::String::new(scope, &s).unwrap().into(),
      DesktopValue::List(l) => {
        let arr = v8::Array::new(scope, l.len() as i32);
        for (i, v) in l.into_iter().enumerate() {
          let val = v.to_v8(scope)?;
          arr.set_index(scope, i as u32, val);
        }
        arr.into()
      }
      DesktopValue::Dict(d) => {
        let obj = v8::Object::new(scope);
        for (k, v) in d {
          let key: v8::Local<v8::Value> =
            v8::String::new(scope, &k).unwrap().into();
          let val = v.to_v8(scope)?;
          obj.set(scope, key, val);
        }
        obj.into()
      }
      DesktopValue::Binary(b) => {
        let len = b.len();
        let store = v8::ArrayBuffer::new_backing_store_from_vec(b);
        let ab = v8::ArrayBuffer::with_backing_store(scope, &store.into());
        v8::Uint8Array::new(scope, ab, 0, len).unwrap().into()
      }
    })
  }
}

// Serde support so `DesktopValue` can ride inside `#[serde]` op payloads
// (`DesktopEvent::BindCall` args, `op_desktop_resolve_bind_call` results).
// Unlike `serde_json::Value`, `Binary` maps to serde bytes, which serde_v8
// materializes as a `Uint8Array` — this is what lets binding arguments and
// return values carry binary data (see denoland/deno#36498).
//
// The view type is not preserved. serde_v8 routes every `ArrayBufferView`
// and `ArrayBuffer` to `visit_byte_buf`, so a `Float64Array`, `Int32Array`,
// `DataView` or bare `ArrayBuffer` arrives on the other side as raw bytes
// (a `Uint8Array` going out, an `ArrayBuffer` in the renderer's own glue).
// Callers that need the original view must carry the type themselves. This
// is lossy, but only relative to a transport that never worked: before
// #36498 any of these was a hard `invalid type: byte array` error.
impl serde::Serialize for DesktopValue {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    use serde::ser::SerializeMap;
    use serde::ser::SerializeSeq;
    match self {
      DesktopValue::Null => serializer.serialize_unit(),
      DesktopValue::Bool(b) => serializer.serialize_bool(*b),
      DesktopValue::Int(i) => serializer.serialize_i32(*i),
      DesktopValue::Double(d) => serializer.serialize_f64(*d),
      DesktopValue::String(s) => serializer.serialize_str(s),
      DesktopValue::List(l) => {
        let mut seq = serializer.serialize_seq(Some(l.len()))?;
        for v in l {
          seq.serialize_element(v)?;
        }
        seq.end()
      }
      DesktopValue::Dict(d) => {
        let mut map = serializer.serialize_map(Some(d.len()))?;
        for (k, v) in d {
          map.serialize_entry(k, v)?;
        }
        map.end()
      }
      DesktopValue::Binary(b) => serializer.serialize_bytes(b),
    }
  }
}

impl<'de> serde::Deserialize<'de> for DesktopValue {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    struct ValueVisitor {
      depth: usize,
    }

    /// Threads the current nesting depth through nested values, which a
    /// bare `Deserialize` impl has nowhere to carry.
    struct ValueSeed {
      depth: usize,
    }

    impl<'de> serde::de::DeserializeSeed<'de> for ValueSeed {
      type Value = DesktopValue;

      fn deserialize<D>(self, deserializer: D) -> Result<DesktopValue, D::Error>
      where
        D: serde::Deserializer<'de>,
      {
        deserializer.deserialize_any(ValueVisitor { depth: self.depth })
      }
    }

    impl<'de> serde::de::Visitor<'de> for ValueVisitor {
      type Value = DesktopValue;

      fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON-compatible value or binary data")
      }

      fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(DesktopValue::Bool(v))
      }

      fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(match i32::try_from(v) {
          Ok(i) => DesktopValue::Int(i),
          Err(_) => DesktopValue::Double(v as f64),
        })
      }

      fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(match i32::try_from(v) {
          Ok(i) => DesktopValue::Int(i),
          Err(_) => DesktopValue::Double(v as f64),
        })
      }

      fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(DesktopValue::Double(v))
      }

      fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(DesktopValue::String(v.to_owned()))
      }

      fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(DesktopValue::String(v))
      }

      fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E> {
        Ok(DesktopValue::Binary(v.to_vec()))
      }

      fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E> {
        Ok(DesktopValue::Binary(v))
      }

      fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(DesktopValue::Null)
      }

      fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(DesktopValue::Null)
      }

      fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
      where
        D: serde::Deserializer<'de>,
      {
        use serde::de::DeserializeSeed;
        ValueSeed { depth: self.depth }.deserialize(deserializer)
      }

      fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
      where
        A: serde::de::SeqAccess<'de>,
      {
        let depth = self.depth.checked_add(1).filter(|d| *d <= MAX_DEPTH);
        let Some(depth) = depth else {
          return Err(nesting_too_deep());
        };
        let mut list = Vec::new();
        while let Some(v) = seq.next_element_seed(ValueSeed { depth })? {
          list.push(v);
        }
        Ok(DesktopValue::List(list))
      }

      fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
      where
        A: serde::de::MapAccess<'de>,
      {
        let depth = self.depth.checked_add(1).filter(|d| *d <= MAX_DEPTH);
        let Some(depth) = depth else {
          return Err(nesting_too_deep());
        };
        let mut dict = Vec::new();
        while let Some(k) = map.next_key::<String>()? {
          dict.push((k, map.next_value_seed(ValueSeed { depth })?));
        }
        Ok(DesktopValue::Dict(dict))
      }
    }

    deserializer.deserialize_any(ValueVisitor { depth: 0 })
  }
}

/// Nesting depth accepted anywhere a `DesktopValue` is built from data the
/// runtime didn't produce itself.
///
/// Every conversion in and out of `DesktopValue` recurses once per level, so
/// without a bound a deeply nested value walks the runtime thread off the end
/// of its stack instead of surfacing an error to the caller. Matches
/// `serde_json`'s own recursion limit.
///
/// Two paths have to honour it, in opposite directions:
///
/// - JS → Rust, enforced by the `Deserialize` impl below. A self-referential
///   value returned from a binding handler (`const o = {}; o.self = o; return
///   o`) is the realistic source.
/// - renderer → Rust, enforced by `laufey_value_to_desktop_value` in
///   `cli/rt_desktop`, which converts renderer-supplied binding *arguments*
///   before the deserializer is ever involved. A `laufey::Value` can't be
///   cyclic — something upstream would have had to resolve the cycle to build
///   it — but its depth is still whatever the renderer sent.
///
/// Bounding both entry points is what makes `DesktopValue::to_v8` safe: it
/// recurses too, and only ever walks a value that arrived through one of them.
pub const MAX_DEPTH: usize = 128;

fn nesting_too_deep<E: serde::de::Error>() -> E {
  serde::de::Error::custom(format!(
    "binding value nested deeper than {MAX_DEPTH} levels (cyclic?)"
  ))
}

/// Wraps a `Result<DesktopValue, DesktopValue>` from `execute_js`.
/// Converts to `{ ok: true, value }` or `{ ok: false, value }`.
pub struct ExecuteJsResult(pub Result<DesktopValue, DesktopValue>);

impl<'a> ToV8<'a> for ExecuteJsResult {
  type Error = std::convert::Infallible;

  fn to_v8(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    let obj = v8::Object::new(scope);

    let ok_key: v8::Local<v8::Value> =
      v8::String::new(scope, "ok").unwrap().into();
    let value_key: v8::Local<v8::Value> =
      v8::String::new(scope, "value").unwrap().into();

    let (ok, val) = match self.0 {
      Ok(v) => (true, v.to_v8(scope)?),
      Err(v) => (false, v.to_v8(scope)?),
    };

    obj.set(scope, ok_key, v8::Boolean::new(scope, ok).into());
    obj.set(scope, value_key, val);
    Ok(obj.into())
  }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDevtoolsOptions {
  pub renderer: Option<bool>,
  pub deno: Option<bool>,
}

/// A single event type that flows from the laufey backend to the Deno runtime.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DesktopEvent {
  #[serde(rename_all = "camelCase")]
  AppMenuClick { window_id: u32, id: String },
  #[serde(rename_all = "camelCase")]
  ContextMenuClick { window_id: u32, id: String },
  #[serde(rename_all = "camelCase")]
  KeyboardEvent {
    window_id: u32,
    r#type: String,
    key: String,
    code: String,
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
    repeat: bool,
  },
  #[serde(rename_all = "camelCase")]
  BindCall {
    window_id: u32,
    name: String,
    args: Vec<DesktopValue>,
    call_id: u32,
  },
  #[serde(rename_all = "camelCase")]
  MouseClick {
    window_id: u32,
    state: String,
    button: i32,
    client_x: f64,
    client_y: f64,
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
    click_count: i32,
  },
  #[serde(rename_all = "camelCase")]
  MouseMove {
    window_id: u32,
    client_x: f64,
    client_y: f64,
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
  },
  #[serde(rename_all = "camelCase")]
  Wheel {
    window_id: u32,
    delta_x: f64,
    delta_y: f64,
    delta_mode: i32,
    client_x: f64,
    client_y: f64,
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
  },
  #[serde(rename_all = "camelCase")]
  CursorEnterLeave {
    window_id: u32,
    entered: bool,
    client_x: f64,
    client_y: f64,
    shift: bool,
    control: bool,
    alt: bool,
    meta: bool,
  },
  #[serde(rename_all = "camelCase")]
  FocusChanged { window_id: u32, focused: bool },
  #[serde(rename_all = "camelCase")]
  WindowResize {
    window_id: u32,
    width: i32,
    height: i32,
  },
  #[serde(rename_all = "camelCase")]
  WindowMove { window_id: u32, x: i32, y: i32 },
  #[serde(rename_all = "camelCase")]
  PageLoad { window_id: u32 },
  #[serde(rename_all = "camelCase")]
  CloseRequested { window_id: u32 },
  /// The window's maximized / minimized / fullscreen state changed (after
  /// the OS applied it). DESKTOP_JS derives `maximize`, `unmaximize`,
  /// `minimize`, `restore`, `enterfullscreen` and `leavefullscreen` from the
  /// difference.
  #[serde(rename_all = "camelCase")]
  WindowState {
    window_id: u32,
    state: WindowStateInfo,
    previous: WindowStateInfo,
  },
  /// Displays were added, removed, rearranged or rescaled, or a work area
  /// changed (`Deno.desktop` "displaychanged").
  DisplayChanged,
  #[serde(rename_all = "camelCase")]
  RuntimeError {
    message: String,
    stack: Option<String>,
  },
  #[serde(rename_all = "camelCase")]
  DockMenuClick { id: String },
  #[serde(rename_all = "camelCase")]
  DockReopen { has_visible_windows: bool },
  /// A URL the OS routed to the running app (macOS: a deep link with a scheme
  /// the bundle declares, through the `openURLs` Apple Event), passed through
  /// as delivered. Emitted through [`DesktopLaunchInbox`], so a URL that
  /// arrives before the app listens (a launch link) is held until it does.
  #[serde(rename_all = "camelCase")]
  OpenUrl { url: String },
  /// A file opened with the running app (macOS: Finder "Open With", a double
  /// click on a claimed file type, a drop on the Dock icon; AppKit delivers
  /// it as a `file://` URL, decoded here to a path). Emitted through
  /// [`DesktopLaunchInbox`].
  #[serde(rename_all = "camelCase")]
  OpenFile { path: String },
  /// The app was launched again while running, with laufey's
  /// single-instance lock on: the second process forwarded its arguments
  /// (after the executable name) and working directory, and exited. `urls`
  /// and `files` are the deep links and existing paths found in `args`.
  /// Emitted through [`DesktopLaunchInbox`].
  #[serde(rename_all = "camelCase")]
  SecondInstance {
    args: Vec<String>,
    cwd: String,
    urls: Vec<String>,
    files: Vec<String>,
  },
  #[serde(rename_all = "camelCase")]
  TrayClick { tray_id: u32 },
  #[serde(rename_all = "camelCase")]
  TrayDoubleClick { tray_id: u32 },
  #[serde(rename_all = "camelCase")]
  TrayMenuClick { tray_id: u32, id: String },
  #[serde(rename_all = "camelCase")]
  NotificationShow { notification_id: u32 },
  #[serde(rename_all = "camelCase")]
  NotificationClick { notification_id: u32 },
  #[serde(rename_all = "camelCase")]
  NotificationClose { notification_id: u32 },
  #[serde(rename_all = "camelCase")]
  NotificationError { notification_id: u32 },
}

/// Capacity of the runtime-bound event channel. A misbehaving renderer could
/// otherwise flood mouse-move / wheel events fast enough to OOM the runtime
/// (the channel was previously unbounded). When full, low-priority events
/// (motion / wheel) are dropped via `try_send` and a warning is logged.
const DESKTOP_EVENT_CHANNEL_CAPACITY: usize = 1024;

type DesktopEventRx =
  tokio::sync::Mutex<tokio::sync::mpsc::Receiver<DesktopEvent>>;
pub type DesktopEventTx = tokio::sync::mpsc::Sender<DesktopEvent>;

pub struct DesktopEventReceiver(pub Arc<DesktopEventRx>);
#[derive(Clone)]
pub struct DesktopEventSender(pub DesktopEventTx);

impl DesktopEventSender {
  /// Send an event, dropping it on backpressure rather than blocking or
  /// allocating. Use this for high-frequency events (mouse move, wheel).
  pub fn try_send(&self, event: DesktopEvent) {
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
      self.0.try_send(event)
    {
      // Log once per overflow burst would be ideal, but a plain warn is fine
      // here — this only fires on pathological event rates.
      log::warn!(
        "desktop event channel full; dropping event (renderer producing events faster than runtime can drain)"
      );
    }
  }
}

pub fn create_desktop_event_channel()
-> (DesktopEventSender, DesktopEventReceiver) {
  let (tx, rx) = tokio::sync::mpsc::channel(DESKTOP_EVENT_CHANNEL_CAPACITY);
  (
    DesktopEventSender(tx),
    DesktopEventReceiver(Arc::new(tokio::sync::Mutex::new(rx))),
  )
}

/// Most launch events (URLs, files, second-instance launches) of one kind
/// held while nothing listens for them. Beyond this the oldest are dropped:
/// the newest is the one the user is waiting on.
const MAX_PENDING_LAUNCH_EVENTS: usize = 64;

/// The kinds of launch event JS subscribes to, by their DOM event type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchEventKind {
  OpenUrl,
  OpenFile,
  SecondInstance,
}

impl LaunchEventKind {
  fn from_event_type(event_type: &str) -> Option<Self> {
    match event_type {
      "openurl" => Some(Self::OpenUrl),
      "openfile" => Some(Self::OpenFile),
      "secondinstance" => Some(Self::SecondInstance),
      _ => None,
    }
  }
}

#[derive(Default)]
struct LaunchInboxState {
  tx: Option<DesktopEventTx>,
  /// Deep links and files from the process's own arguments.
  launch_urls: Vec<String>,
  launch_files: Vec<String>,
  /// Whether JS took the launch snapshot (`Deno.desktop.launchUrls`).
  launch_taken: bool,
  pending_urls: std::collections::VecDeque<String>,
  pending_files: std::collections::VecDeque<String>,
  pending_second_instances: std::collections::VecDeque<DesktopEvent>,
  subscribed_urls: bool,
  subscribed_files: bool,
  subscribed_second_instances: bool,
}

/// Deep links, opened files and second-instance launches on their way to
/// `Deno.desktop`.
///
/// The backend delivers them from the moment the runtime registers its
/// handlers, which is before the app's main module has run, let alone added
/// a listener. So they are buffered here, per kind, until JS subscribes to
/// that kind (the first `openurl` / `openfile` / `secondinstance` listener or
/// `on…` handler on `Deno.desktop`), which drains the buffer into events.
/// After that each one is sent straight into the desktop event channel.
///
/// Launch arguments and the URLs and files that arrived before the app read
/// `Deno.desktop.launchUrls` / `launchFiles` form the launch snapshot
/// instead: reading it takes the buffered URLs and files, so each delivery
/// reaches the app exactly once, as part of the snapshot or as an event.
#[derive(Clone)]
pub struct DesktopLaunchInbox(Arc<std::sync::Mutex<LaunchInboxState>>);

/// The launch snapshot handed to JS once, for `Deno.desktop.launchUrls` and
/// `Deno.desktop.launchFiles`.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTargetsSnapshot {
  pub urls: Vec<String>,
  pub files: Vec<String>,
}

impl DesktopLaunchInbox {
  /// `launch_urls` / `launch_files` are the deep links and files in the
  /// process's own arguments (a cold start).
  pub fn new(
    tx: DesktopEventTx,
    launch_urls: Vec<String>,
    launch_files: Vec<String>,
  ) -> Self {
    Self(Arc::new(std::sync::Mutex::new(LaunchInboxState {
      tx: Some(tx),
      launch_urls,
      launch_files,
      ..Default::default()
    })))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, LaunchInboxState> {
    self
      .0
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// A non-file URL routed to the running app.
  pub fn open_url(&self, url: String) {
    let mut state = self.lock();
    if state.subscribed_urls {
      send_launch_event(&state, DesktopEvent::OpenUrl { url });
    } else {
      push_bounded(&mut state.pending_urls, url, "openurl");
    }
  }

  /// A file opened with the running app.
  pub fn open_file(&self, path: String) {
    let mut state = self.lock();
    if state.subscribed_files {
      send_launch_event(&state, DesktopEvent::OpenFile { path });
    } else {
      push_bounded(&mut state.pending_files, path, "openfile");
    }
  }

  /// A launch forwarded by a second instance.
  pub fn second_instance(
    &self,
    args: Vec<String>,
    cwd: String,
    urls: Vec<String>,
    files: Vec<String>,
  ) {
    let event = DesktopEvent::SecondInstance {
      args,
      cwd,
      urls,
      files,
    };
    let mut state = self.lock();
    if state.subscribed_second_instances {
      send_launch_event(&state, event);
    } else {
      push_bounded(
        &mut state.pending_second_instances,
        event,
        "secondinstance",
      );
    }
  }

  /// The launch snapshot: the process's own deep links and files, plus the
  /// URLs and files delivered so far that no listener has taken. Empty after
  /// the first call.
  pub fn take_launch_targets(&self) -> LaunchTargetsSnapshot {
    let mut state = self.lock();
    if state.launch_taken {
      return LaunchTargetsSnapshot::default();
    }
    state.launch_taken = true;
    let mut urls = std::mem::take(&mut state.launch_urls);
    urls.extend(state.pending_urls.drain(..));
    let mut files = std::mem::take(&mut state.launch_files);
    files.extend(state.pending_files.drain(..));
    LaunchTargetsSnapshot { urls, files }
  }

  /// Subscribe JS to one kind of launch event (by DOM event type), returning
  /// what was buffered for it, oldest first. Later deliveries of that kind go
  /// through the event channel. Unknown types and repeated subscriptions
  /// return nothing.
  pub fn subscribe(&self, event_type: &str) -> Vec<DesktopEvent> {
    let Some(kind) = LaunchEventKind::from_event_type(event_type) else {
      return Vec::new();
    };
    let mut state = self.lock();
    match kind {
      LaunchEventKind::OpenUrl => {
        state.subscribed_urls = true;
        state
          .pending_urls
          .drain(..)
          .map(|url| DesktopEvent::OpenUrl { url })
          .collect()
      }
      LaunchEventKind::OpenFile => {
        state.subscribed_files = true;
        state
          .pending_files
          .drain(..)
          .map(|path| DesktopEvent::OpenFile { path })
          .collect()
      }
      LaunchEventKind::SecondInstance => {
        state.subscribed_second_instances = true;
        state.pending_second_instances.drain(..).collect()
      }
    }
  }
}

fn push_bounded<T>(
  queue: &mut std::collections::VecDeque<T>,
  item: T,
  event_type: &str,
) {
  if queue.len() >= MAX_PENDING_LAUNCH_EVENTS {
    queue.pop_front();
    log::warn!(
      "desktop: more than {MAX_PENDING_LAUNCH_EVENTS} \"{event_type}\" events \
       arrived before the app listened for them; dropping the oldest"
    );
  }
  queue.push_back(item);
}

fn send_launch_event(state: &LaunchInboxState, event: DesktopEvent) {
  let Some(tx) = &state.tx else {
    return;
  };
  // Called on the backend's UI thread, which must not block. These events are
  // rare; the channel is only full when the runtime has stopped draining it.
  if let Err(e) = tx.try_send(event) {
    log::warn!("desktop: dropping a launch event: {e}");
  }
}

/// Who handles a deep-link scheme, as `Deno.desktop.getSchemeOwner()`
/// resolves it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemeOwnerInfo {
  /// `"self"`, `"other"` or `"none"`.
  pub owner: &'static str,
  /// What identifies the current handler, for display.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub handler: Option<String>,
}

/// The result of `Deno.desktop.registerScheme()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemeRegisterInfo {
  /// Whether this app handles the scheme afterwards.
  pub registered: bool,
  /// The handler afterwards: `"self"`, `"other"` or `"none"`.
  pub owner: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub handler: Option<String>,
  /// Why the app does not handle the scheme, when it doesn't.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
}

/// The OS registration of the app's deep-link schemes, behind
/// `Deno.desktop.getSchemeOwner()` / `registerScheme()`. Implemented by the
/// desktop runtime (denort_desktop), which puts an
/// `Arc<dyn DesktopSchemeHandlers>` in the op state.
pub trait DesktopSchemeHandlers: Send + Sync + 'static {
  /// Normalize `scheme` and check it is one of the app's declared deep-link
  /// schemes. The error is the message of the `TypeError` the call rejects
  /// with. Cheap: called on the JS thread.
  fn check_scheme(&self, scheme: &str) -> Result<String, String>;
  /// The scheme's current handler. Blocking (registry reads, LaunchServices,
  /// files): called on the blocking pool.
  fn scheme_owner(&self, scheme: &str) -> SchemeOwnerInfo;
  /// Register the app for the scheme: when nobody handles it, to refresh
  /// the app's own registration, or, with `force`, over another app.
  /// Blocking: called on the blocking pool.
  fn register_scheme(&self, scheme: &str, force: bool) -> SchemeRegisterInfo;
}

/// How long a scheme query or registration may take before the call rejects
/// (a backend runs at most a couple of short, individually bounded
/// subprocesses).
const SCHEME_HANDLER_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(30);

/// What `Deno.desktop.passkeys.capabilities()` resolves with — the shape of
/// `@clerk/electron-passkeys`' `capabilities()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyCapabilitiesInfo {
  pub platform_authenticator: bool,
  pub security_keys: bool,
}

/// Native passkeys (WebAuthn through the OS platform authenticator), behind
/// `Deno.desktop.passkeys`. Implemented by the desktop runtime
/// (denort_desktop, over laufey), which puts an `Arc<dyn DesktopPasskeys>` in
/// the op state.
///
/// Options and results are JSON strings in the `@clerk/electron-passkeys`
/// wire format, passed through untouched: laufey's backend parses the options
/// strictly and resolves every request with one envelope,
/// `{"ok":true,"credential":{...}}` or
/// `{"ok":false,"error":{"code","message"}}`.
pub trait DesktopPasskeys: Send + Sync + 'static {
  /// May block briefly (Windows asks the WebAuthn service): called on the
  /// blocking pool.
  fn capabilities(&self) -> PasskeyCapabilitiesInfo;
  /// Start a registration (`create`) or authentication ceremony anchored to
  /// `window_id` (0: the focused window). The request is made when this is
  /// called; the future resolves with the envelope and never fails.
  fn request(
    &self,
    create: bool,
    window_id: u32,
    options_json: String,
  ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send>>;
}

/// The envelope a runtime without native passkeys answers with (the text of
/// laufey's own not_supported answer).
pub const PASSKEY_NOT_SUPPORTED_ENVELOPE: &str = r#"{"ok":false,"error":{"code":"not_supported","message":"Native passkeys are not supported on this platform."}}"#;

/// A pending call from the webview to a bound Deno function.
pub struct PendingBindCall {
  pub name: String,
  pub args: Vec<DesktopValue>,
  pub response: tokio::sync::oneshot::Sender<Result<DesktopValue, String>>,
}

type PendingBindResponsesMap =
  HashMap<u32, tokio::sync::oneshot::Sender<Result<DesktopValue, String>>>;

#[derive(Clone)]
pub struct PendingBindResponses(
  pub Arc<std::sync::Mutex<PendingBindResponsesMap>>,
);

impl PendingBindResponses {
  pub fn new() -> Self {
    Self::default()
  }
}

impl Default for PendingBindResponses {
  fn default() -> Self {
    Self(Arc::new(std::sync::Mutex::new(HashMap::new())))
  }
}

static BIND_CALL_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Assign a call_id for a bind call and register its response sender.
/// Returns the call_id to embed in the `DesktopEvent::BindCall`.
pub fn register_bind_call(
  responses: &PendingBindResponses,
  response: tokio::sync::oneshot::Sender<Result<DesktopValue, String>>,
) -> u32 {
  let call_id = BIND_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
  responses.0.lock().unwrap().insert(call_id, response);
  call_id
}

/// Trait for desktop window operations. Implemented by the desktop
/// runtime (denort_desktop) to bridge to the laufey backend.
///
/// All per-window methods take a `window_id` identifying the target window.
/// A window's state (`BrowserWindow.isMaximized()` etc.).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowStateInfo {
  pub maximized: bool,
  pub minimized: bool,
  pub fullscreen: bool,
}

/// What `BrowserWindow` state methods ask the backend to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
  Maximize,
  Unmaximize,
  Minimize,
  Restore,
  EnterFullscreen,
  LeaveFullscreen,
}

/// A rectangle in the backend's screen space (the `getPosition` space).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRect {
  pub x: i32,
  pub y: i32,
  pub width: i32,
  pub height: i32,
}

/// One display (`Deno.desktop.screens()`).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenInfo {
  pub id: i64,
  pub bounds: DesktopRect,
  pub work_area: DesktopRect,
  pub scale_factor: f64,
  pub is_primary: bool,
}

/// `Deno.desktop.windowCapabilities()`: what this backend can do on this OS.
/// A setter for something reported `false` changes nothing (and returns
/// `false` where it returns a boolean).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCapabilitiesInfo {
  pub state: bool,
  pub state_events: bool,
  pub size_constraints: bool,
  pub screens: bool,
  pub display_events: bool,
  pub title_bar_hidden: bool,
  pub title_bar_hidden_inset: bool,
  pub window_button_position: bool,
  pub mica: bool,
  pub acrylic: bool,
  pub tabbed: bool,
  pub vibrancy: bool,
  pub normal_bounds: bool,
  pub keep_alive: bool,
  pub set_position: bool,
}

/// The smallest part of a window (or all of it, when smaller) that must
/// overlap a screen's work area for [`ensure_on_screen`] to leave it where it
/// is: enough of the title bar to grab it.
pub const ON_SCREEN_MIN_WIDTH: i32 = 64;
pub const ON_SCREEN_MIN_HEIGHT: i32 = 32;

/// Keep a restored window reachable. Returns `rect` unchanged when at least
/// a 64x32 part of it (all of it, if smaller) overlaps the work area of some
/// screen; otherwise (a monitor that is gone, a resolution that shrank) the
/// window moves to the primary screen's work area (the first screen when
/// none is marked primary), shrunk to fit and centered. No screens (a
/// backend that can't list them) leaves `rect` alone.
pub fn ensure_on_screen(
  rect: DesktopRect,
  screens: &[ScreenInfo],
) -> DesktopRect {
  if screens.is_empty() {
    return rect;
  }
  let need_w = ON_SCREEN_MIN_WIDTH.min(rect.width.max(1));
  let need_h = ON_SCREEN_MIN_HEIGHT.min(rect.height.max(1));
  let visible = screens.iter().any(|s| {
    let wa = s.work_area;
    let left = rect.x.max(wa.x) as i64;
    let top = rect.y.max(wa.y) as i64;
    let right =
      (rect.x as i64 + rect.width as i64).min(wa.x as i64 + wa.width as i64);
    let bottom =
      (rect.y as i64 + rect.height as i64).min(wa.y as i64 + wa.height as i64);
    right - left >= need_w as i64 && bottom - top >= need_h as i64
  });
  if visible {
    return rect;
  }
  let target = screens
    .iter()
    .find(|s| s.is_primary)
    .unwrap_or(&screens[0])
    .work_area;
  let width = rect.width.min(target.width).max(1);
  let height = rect.height.min(target.height).max(1);
  DesktopRect {
    x: target.x + (target.width - width) / 2,
    y: target.y + (target.height - height) / 2,
    width,
    height,
  }
}

/// How long a close the user asked for waits for the app's `close` listeners
/// to answer before it happens anyway (a blocked or crashed runtime must not
/// leave a window that cannot be closed).
pub const CLOSE_REPLY_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(5);

/// What to do with a window after a close-request event was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecision {
  /// Close the window now.
  Close,
  /// A listener called `preventDefault()`: keep it open.
  Keep,
  /// No close is pending for that window (already answered, timed out or
  /// closed): do nothing.
  Ignore,
}

/// The close requests waiting for the app's answer, keyed by window, each
/// with a generation so a timer started for an earlier request can't close a
/// window whose later request is still pending.
#[derive(Default)]
pub struct PendingCloses {
  inner: std::sync::Mutex<(u64, HashMap<u32, u64>)>,
}

impl PendingCloses {
  /// Record a close request; returns the token its timeout must present.
  pub fn begin(&self, window_id: u32) -> u64 {
    let mut guard = self.inner.lock().unwrap();
    guard.0 += 1;
    let token = guard.0;
    guard.1.insert(window_id, token);
    token
  }

  /// The app answered (`prevented` = a listener called `preventDefault()`).
  pub fn reply(&self, window_id: u32, prevented: bool) -> CloseDecision {
    if self.inner.lock().unwrap().1.remove(&window_id).is_none() {
      return CloseDecision::Ignore;
    }
    if prevented {
      CloseDecision::Keep
    } else {
      CloseDecision::Close
    }
  }

  /// The timeout for request `token` fired: close only if that very request
  /// is still unanswered.
  pub fn expire(&self, window_id: u32, token: u64) -> CloseDecision {
    let mut guard = self.inner.lock().unwrap();
    if guard.1.get(&window_id) == Some(&token) {
      guard.1.remove(&window_id);
      CloseDecision::Close
    } else {
      CloseDecision::Ignore
    }
  }

  /// The window closed some other way (`close()`, quit): forget it.
  pub fn forget(&self, window_id: u32) {
    self.inner.lock().unwrap().1.remove(&window_id);
  }
}

/// What can reveal the hidden bootstrap window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealTrigger {
  /// Its first navigation finished loading.
  FirstLoad,
  /// The safety net that shows it when the load never finishes.
  Fallback,
}

/// Whether the runtime may reveal the bootstrap window on its own.
///
/// Never when `desktop.initialWindow.showOnFirstLoad` is false (a tray-only
/// app), never once app code has called `show()`, `hide()` or `close()` on it
/// (the app owns its visibility from then on), and only once.
pub fn should_reveal_initial_window(
  _trigger: RevealTrigger,
  show_on_first_load: bool,
  app_controlled: bool,
  already_revealed: bool,
) -> bool {
  show_on_first_load && !app_controlled && !already_revealed
}

pub trait DesktopApi: Send + Sync + 'static {
  /// Create a new window with the given dimensions and return its ID.
  ///
  /// `frameless` drops the title bar and standard window chrome.
  /// `no_activate` makes the window a floating, non-activating utility panel
  /// (used for tray / menu-bar popovers): it floats above normal windows and
  /// does not steal key focus from the foreground app when shown.
  /// `transparent` gives the window a transparent background so the page's own
  /// alpha composites against whatever is behind the window. These are all
  /// creation-time properties and cannot be changed afterwards.
  fn create_window(
    &self,
    width: i32,
    height: i32,
    frameless: bool,
    no_activate: bool,
    transparent_titlebar: bool,
    transparent: bool,
  ) -> u32;
  /// Close a specific window.
  fn close_window(&self, window_id: u32);
  /// Returns true if the given window has been closed (either via
  /// `close_window` or because the OS window was destroyed).
  fn is_closed(&self, window_id: u32) -> bool;

  fn set_title(&self, window_id: u32, title: &str);

  fn get_window_size(&self, window_id: u32) -> (i32, i32);
  fn set_window_size(&self, window_id: u32, width: i32, height: i32);
  /// Chrome-inclusive size (`window.outerWidth` / `outerHeight`).
  fn get_window_outer_size(&self, window_id: u32) -> (i32, i32);

  /// Physical pixels per DIP for this window (`window.devicePixelRatio`).
  fn get_window_scale_factor(&self, window_id: u32) -> f64;

  fn get_window_position(&self, window_id: u32) -> (i32, i32);
  fn set_window_position(&self, window_id: u32, x: i32, y: i32);
  /// Content-view origin in the same space as `get_window_position`.
  fn get_window_inner_position(&self, window_id: u32) -> (i32, i32);

  fn is_resizable(&self, window_id: u32) -> bool;
  fn set_resizable(&self, window_id: u32, resizable: bool);

  fn is_always_on_top(&self, window_id: u32) -> bool;
  fn set_always_on_top(&self, window_id: u32, always_on_top: bool);

  /// Overall window opacity in `0.0..=1.0` (1.0 == fully opaque). Fades the
  /// whole window uniformly (chrome included), unlike the `transparent`
  /// creation flag which honors the page's per-pixel alpha.
  fn get_window_opacity(&self, window_id: u32) -> f64;
  fn set_window_opacity(&self, window_id: u32, opacity: f64);

  fn is_visible(&self, window_id: u32) -> bool;
  fn show(&self, window_id: u32);
  fn hide(&self, window_id: u32);
  fn focus(&self, window_id: u32);

  fn bind(&self, window_id: u32, name: &str);
  fn unbind(&self, window_id: u32, name: &str);

  fn navigate(&self, window_id: u32, url: &str);
  /// End the app the way closing the last window does (laufey `quit`):
  /// remaining windows close without a `close` event.
  fn quit(&self);

  // --- Window state, constraints, screens and chrome (laufey API 38). The
  // defaults are a backend that can do none of it.

  fn window_capabilities(&self) -> WindowCapabilitiesInfo {
    WindowCapabilitiesInfo::default()
  }
  fn set_window_state(&self, _window_id: u32, _action: WindowAction) {}
  fn get_window_state(&self, _window_id: u32) -> WindowStateInfo {
    WindowStateInfo::default()
  }
  /// `[min_width, min_height, max_width, max_height]`, 0 = no limit.
  fn set_size_constraints(&self, _window_id: u32, _constraints: [i32; 4]) {}
  fn get_size_constraints(&self, _window_id: u32) -> [i32; 4] {
    [0; 4]
  }
  fn screens(&self) -> Vec<ScreenInfo> {
    Vec::new()
  }
  fn window_screen_id(&self, _window_id: u32) -> Option<i64> {
    None
  }
  /// 0 default, 1 hidden, 2 hidden inset.
  fn set_titlebar_style(&self, _window_id: u32, _style: i32) -> bool {
    false
  }
  fn set_traffic_light_position(
    &self,
    _window_id: u32,
    _position: Option<(i32, i32)>,
  ) -> bool {
    false
  }
  /// laufey `LAUFEY_BACKDROP_*` and, for vibrancy, `LAUFEY_VIBRANCY_*`.
  fn set_backdrop(
    &self,
    _window_id: u32,
    _backdrop: i32,
    _material: i32,
  ) -> bool {
    false
  }
  /// Normal bounds as (x, y, content width, content height).
  fn get_normal_bounds(&self, _window_id: u32) -> Option<(i32, i32, i32, i32)> {
    None
  }
  fn set_quit_on_last_window_closed(&self, _quit: bool) {}
  /// The app answered a close request (see [`PendingCloses`]).
  fn close_reply(&self, _window_id: u32, _prevented: bool) {}
  fn set_application_menu(&self, window_id: u32, menu: Vec<MenuItem>);
  fn show_context_menu(
    &self,
    window_id: u32,
    x: i32,
    y: i32,
    menu: Vec<MenuItem>,
  );

  /// Best-effort fetch of the OS-level window/display handles for the
  /// given window. Returning `Err` instead of panicking matters because
  /// this trait method is reachable from a v8 op handler but its
  /// implementation is invoked across the laufey C ABI; an unwind through
  /// that boundary would be UB.
  fn get_raw_window_handle(
    &self,
    window_id: u32,
  ) -> Result<
    (
      raw_window_handle::RawWindowHandle,
      raw_window_handle::RawDisplayHandle,
    ),
    deno_error::JsErrorBox,
  >;

  fn open_devtools(&self, window_id: u32, renderer: bool, deno: bool);

  fn execute_js(
    &self,
    window_id: u32,
    script: &str,
    callback: Box<
      dyn FnOnce(Result<DesktopValue, DesktopValue>) + Send + 'static,
    >,
  );

  fn alert(&self, title: &str, message: &str);
  /// Show a modal confirm dialog. Blocks the calling thread until the
  /// user dismisses it; the platform's modal run loop pumps OS events
  /// while the dialog is up so other windows continue to render and
  /// respond.
  fn confirm(&self, title: &str, message: &str) -> bool;
  /// Show a modal prompt dialog. Returns the entered text on confirm,
  /// `None` on cancel. Blocking semantics as `confirm`.
  fn prompt(
    &self,
    title: &str,
    message: &str,
    default_value: &str,
  ) -> Option<String>;

  /// Read the system clipboard's plain-text content. Returns `None` if the
  /// clipboard is empty, holds no text, or the backend has no clipboard
  /// support.
  fn read_clipboard_text(&self) -> Option<String>;
  /// Replace the system clipboard's content with `text`. An empty string
  /// clears the clipboard.
  fn write_clipboard_text(&self, text: &str);

  /// Set a short text badge on the app's dock / taskbar icon. An empty
  /// string clears the badge.
  fn set_dock_badge(&self, text: &str);
  /// Bounce the dock icon (macOS) or the closest native analog. `critical`
  /// maps to a continuous bounce; otherwise a single bounce.
  fn bounce_dock(&self, critical: bool);
  /// Set a custom right-click menu on the app's dock icon (macOS only).
  /// `None` clears any menu previously set.
  fn set_dock_menu(&self, menu: Option<Vec<MenuItem>>);
  /// Show or hide the app's dock icon (macOS activation policy).
  fn set_dock_visible(&self, visible: bool);

  /// Returns `0` if the backend doesn't support tray icons.
  fn create_tray(&self) -> u32;
  /// Destroy a tray icon previously created with `create_tray`.
  fn destroy_tray(&self, tray_id: u32);
  /// Set the tray icon image from PNG-encoded bytes.
  fn set_tray_icon(&self, tray_id: u32, png_bytes: &[u8]);
  /// Set the tray icon used in OS dark mode. `None` clears it.
  fn set_tray_icon_dark(&self, tray_id: u32, png_bytes: Option<&[u8]>);
  /// Set the tooltip shown on hover. `None` clears it.
  fn set_tray_tooltip(&self, tray_id: u32, text: Option<&str>);
  /// Set the right-click context menu on the tray icon. `None` clears
  /// any menu previously set.
  fn set_tray_menu(&self, tray_id: u32, menu: Option<Vec<MenuItem>>);
  /// The tray icon's screen rectangle `(x, y, width, height)` in the same
  /// top-left-origin coordinate space as window positions, or `None` if the
  /// icon has no on-screen position yet or the backend can't report it. Used
  /// to anchor a popover window under the icon.
  fn get_tray_bounds(&self, tray_id: u32) -> Option<(i32, i32, i32, i32)>;

  /// Show an OS notification. Returns the notification id (`0` if the
  /// backend doesn't support system notifications). Events for this
  /// notification (`Show`, `Click`, `Close`, `Error`) are delivered via
  /// the desktop event channel keyed by the returned id.
  fn show_notification(
    &self,
    title: &str,
    body: Option<&str>,
    icon: Option<&[u8]>,
    tag: Option<&str>,
    silent: Option<bool>,
    require_interaction: Option<bool>,
  ) -> u32;
  /// Dismiss a notification previously shown via `show_notification`.
  /// No-op if the id is unknown or already dismissed.
  fn close_notification(&self, notification_id: u32);

  /// Request OS authorization to show notifications. If the user has not
  /// yet decided, this triggers a system prompt; otherwise the cached
  /// decision is returned without a re-prompt. The callback fires on the
  /// UI thread with one of [`PermissionState::Granted`],
  /// [`PermissionState::Denied`], [`PermissionState::Prompt`] (rare —
  /// happens if the user dismissed the prompt without deciding) or
  /// [`PermissionState::Unsupported`] (backend / platform has no
  /// permission model — e.g. an unbundled macOS process, Linux libnotify).
  fn request_notification_permission(
    &self,
    cb: Box<dyn FnOnce(PermissionState) + Send + 'static>,
  );
  /// Query the current authorization state without prompting. Same status
  /// codes as [`request_notification_permission`].
  fn query_notification_permission(
    &self,
    cb: Box<dyn FnOnce(PermissionState) + Send + 'static>,
  );
}

/// Authorization state for a capability that the OS (or a runtime
/// component) gates. Mirrors the Web Permissions API state set with an
/// extra `Unsupported` variant for environments where the capability has
/// no permission model at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
  Granted,
  Denied,
  Prompt,
  Unsupported,
}

/// Stores the window ID of the initial window created during runtime init.
/// The first `BrowserWindow` constructor takes this ID to wrap the existing
/// window; subsequent constructors create new windows.
pub struct InitialWindowId(pub std::sync::Mutex<Option<u32>>);

/// The compiled app's name (from deno.json `desktop.app.name`, falling back to
/// the output file name). Used as the default window title so a window the app
/// doesn't explicitly title shows the app name instead of the backend's
/// internal default (`laufey_webview`).
pub struct DesktopAppName(pub String);

struct BrowserWindow {
  api: Arc<dyn DesktopApi>,
  window_id: u32,
  surface: SameObject<deno_canvas::byow::UnsafeWindowSurface>,
  /// Set when JS has taken a `getNativeWindow()` surface. Once a webgpu
  /// surface holds the underlying raw window handles, destroying the OS
  /// window underneath it would dangle those handles in wgpu-internal state
  /// (use-after-free at present). We refuse to destroy the window in that
  /// case and only hide it; the surface keeps the window alive until JS
  /// releases the BrowserWindow (cppgc) and with it the surface.
  surface_taken: std::cell::Cell<bool>,
  /// The frame around the page (outer size minus page size) last seen while
  /// the window was in the normal state. `getNormalBounds` adds it to the
  /// backend's normal (page) size: measured while maximized or fullscreen
  /// the frame can be gone (window managers drop the borders of a maximized
  /// window), which would undersize the restored bounds.
  normal_chrome: std::cell::Cell<Option<(i32, i32)>>,
}

// SAFETY: we're sure this can be GCed
unsafe impl deno_core::GarbageCollected for BrowserWindow {
  fn trace(&self, _visitor: &mut deno_core::v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"BrowserWindow"
  }
}

impl deno_core::Resource for BrowserWindow {
  fn name(&self) -> Cow<'_, str> {
    "BrowserWindow".into()
  }
}

struct EventTargetSetup {
  brand: v8::Global<v8::Value>,
  set_event_target_data: v8::Global<v8::Value>,
}

#[op2]
impl BrowserWindow {
  #[constructor]
  fn new(
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
    #[scoped] options: Option<BrowserWindowOptions>,
  ) -> v8::Global<v8::Value> {
    let api = state
      .try_borrow::<Arc<dyn DesktopApi>>()
      .expect("desktop mode enabled")
      .clone();

    // Use the initial window if this is the first BrowserWindow,
    // otherwise create a new one.
    let window_id = state
      .try_borrow::<InitialWindowId>()
      .and_then(|iw| iw.0.lock().unwrap().take())
      .unwrap_or_else(|| {
        let width = options.as_ref().and_then(|o| o.width).unwrap_or(800);
        let height = options.as_ref().and_then(|o| o.height).unwrap_or(600);
        let frameless =
          options.as_ref().and_then(|o| o.frameless).unwrap_or(false);
        let no_activate = options
          .as_ref()
          .and_then(|o| o.no_activate)
          .unwrap_or(false);
        let transparent_titlebar = options
          .as_ref()
          .and_then(|o| o.transparent_titlebar)
          .unwrap_or(false);
        let transparent = options
          .as_ref()
          .and_then(|o| o.transparent)
          .unwrap_or(false);
        api.create_window(
          width,
          height,
          frameless,
          no_activate,
          transparent_titlebar,
          transparent,
        )
      });

    // Default the window title to the app name when the app doesn't set one,
    // so the window shows e.g. "MyApp" instead of the backend's internal
    // default (`laufey_webview`). An explicit `title` option below overrides
    // this, and a page that sets `document.title` overrides it at the OS level.
    if options.as_ref().and_then(|o| o.title.as_ref()).is_none()
      && let Some(name) = state.try_borrow::<DesktopAppName>()
      && !name.0.is_empty()
    {
      api.set_title(window_id, &name.0);
    }

    if let Some(options) = &options {
      if let Some(title) = &options.title {
        api.set_title(window_id, title);
      }
      api.set_window_size(
        window_id,
        options.width.unwrap_or(800),
        options.height.unwrap_or(600),
      );
      if let (Some(x), Some(y)) = (options.x, options.y) {
        // A position saved on a monitor that is gone lands on-screen.
        let (width, height) = api.get_window_outer_size(window_id);
        let rect = place_on_screen(
          api.as_ref(),
          DesktopRect {
            x,
            y,
            width,
            height,
          },
        );
        api.set_window_position(window_id, rect.x, rect.y);
      }
      if let Some(resizable) = options.resizable {
        api.set_resizable(window_id, resizable);
      }
      if let Some(always_on_top) = options.always_on_top {
        api.set_always_on_top(window_id, always_on_top);
      }
      if let Some(opacity) = options.opacity {
        api.set_window_opacity(window_id, opacity);
      }
    }

    let window = BrowserWindow {
      api,
      window_id,
      surface: SameObject::new(),
      surface_taken: std::cell::Cell::new(false),
      normal_chrome: std::cell::Cell::new(None),
    };
    let window = deno_core::cppgc::make_cppgc_object(scope, window);
    let event_target_setup = state.borrow::<EventTargetSetup>();
    let webidl_brand = v8::Local::new(scope, event_target_setup.brand.clone());
    window.set(scope, webidl_brand, webidl_brand);
    let set_event_target_data =
      v8::Local::new(scope, event_target_setup.set_event_target_data.clone())
        .cast::<v8::Function>();
    let null = v8::null(scope);
    set_event_target_data.call(scope, null.into(), &[window.into()]);
    let window = window.cast::<v8::Value>();

    v8::Global::new(scope, window)
  }

  #[getter]
  fn window_id(&self) -> u32 {
    self.window_id
  }

  // Keep the native primitive separate from DESKTOP_JS's public `bind`
  // wrapper. The deferred fast-call upgrade may replace this symbol-backed
  // method without overwriting the wrapper.
  #[fast]
  #[symbol("Deno_privateDesktopBind")]
  fn bind(&self, #[string] name: &str) {
    self.api.bind(self.window_id, name);
  }

  #[fast]
  #[symbol("Deno_privateDesktopUnbind")]
  fn unbind(&self, #[string] name: &str) {
    self.api.unbind(self.window_id, name);
  }

  #[fast]
  fn set_title(&self, #[string] title: &str) {
    self.api.set_title(self.window_id, title);
  }

  fn get_size(&self) -> (i32, i32) {
    self.api.get_window_size(self.window_id)
  }

  #[fast]
  #[getter]
  fn inner_width(&self) -> i32 {
    self.api.get_window_size(self.window_id).0
  }

  #[fast]
  #[getter]
  fn inner_height(&self) -> i32 {
    self.api.get_window_size(self.window_id).1
  }

  #[fast]
  #[getter]
  fn outer_width(&self) -> i32 {
    self.api.get_window_outer_size(self.window_id).0
  }

  #[fast]
  #[getter]
  fn outer_height(&self) -> i32 {
    self.api.get_window_outer_size(self.window_id).1
  }

  #[fast]
  #[getter]
  fn device_pixel_ratio(&self) -> f64 {
    self.api.get_window_scale_factor(self.window_id)
  }

  #[fast]
  fn set_size(&self, #[smi] width: i32, #[smi] height: i32) {
    self.api.set_window_size(self.window_id, width, height);
  }

  fn get_position(&self) -> (i32, i32) {
    self.api.get_window_position(self.window_id)
  }

  #[fast]
  #[getter]
  fn screen_x(&self) -> i32 {
    self.api.get_window_position(self.window_id).0
  }

  #[fast]
  #[getter]
  fn screen_y(&self) -> i32 {
    self.api.get_window_position(self.window_id).1
  }

  #[fast]
  #[getter]
  fn screen_left(&self) -> i32 {
    self.api.get_window_position(self.window_id).0
  }

  #[fast]
  #[getter]
  fn screen_top(&self) -> i32 {
    self.api.get_window_position(self.window_id).1
  }

  fn get_inner_position(&self) -> (i32, i32) {
    self.api.get_window_inner_position(self.window_id)
  }

  #[fast]
  fn set_position(&self, #[smi] x: i32, #[smi] y: i32) {
    let (width, height) = self.api.get_window_outer_size(self.window_id);
    let rect = place_on_screen(
      self.api.as_ref(),
      DesktopRect {
        x,
        y,
        width,
        height,
      },
    );
    self.api.set_window_position(self.window_id, rect.x, rect.y);
  }

  // --- State (laufey API 38) ---

  #[fast]
  fn maximize(&self) {
    note_normal_chrome(self.api.as_ref(), self.window_id, &self.normal_chrome);
    self
      .api
      .set_window_state(self.window_id, WindowAction::Maximize);
  }

  #[fast]
  fn unmaximize(&self) {
    self
      .api
      .set_window_state(self.window_id, WindowAction::Unmaximize);
  }

  #[fast]
  fn minimize(&self) {
    note_normal_chrome(self.api.as_ref(), self.window_id, &self.normal_chrome);
    self
      .api
      .set_window_state(self.window_id, WindowAction::Minimize);
  }

  #[fast]
  fn restore(&self) {
    self
      .api
      .set_window_state(self.window_id, WindowAction::Restore);
  }

  #[fast]
  fn set_full_screen(&self, flag: bool) {
    if flag {
      note_normal_chrome(
        self.api.as_ref(),
        self.window_id,
        &self.normal_chrome,
      );
    }
    self.api.set_window_state(
      self.window_id,
      if flag {
        WindowAction::EnterFullscreen
      } else {
        WindowAction::LeaveFullscreen
      },
    );
  }

  #[fast]
  fn is_maximized(&self) -> bool {
    self.api.get_window_state(self.window_id).maximized
  }

  #[fast]
  fn is_minimized(&self) -> bool {
    self.api.get_window_state(self.window_id).minimized
  }

  #[fast]
  fn is_full_screen(&self) -> bool {
    self.api.get_window_state(self.window_id).fullscreen
  }

  // --- Size constraints ---

  #[fast]
  fn set_minimum_size(&self, #[smi] width: i32, #[smi] height: i32) {
    let mut c = self.api.get_size_constraints(self.window_id);
    c[0] = width.max(0);
    c[1] = height.max(0);
    self.api.set_size_constraints(self.window_id, c);
  }

  fn get_minimum_size(&self) -> (i32, i32) {
    let c = self.api.get_size_constraints(self.window_id);
    (c[0], c[1])
  }

  #[fast]
  fn set_maximum_size(&self, #[smi] width: i32, #[smi] height: i32) {
    let mut c = self.api.get_size_constraints(self.window_id);
    c[2] = width.max(0);
    c[3] = height.max(0);
    self.api.set_size_constraints(self.window_id, c);
  }

  fn get_maximum_size(&self) -> (i32, i32) {
    let c = self.api.get_size_constraints(self.window_id);
    (c[2], c[3])
  }

  // --- Bounds ---

  /// The outer frame: `getPosition()` + `outerWidth` / `outerHeight`.
  #[serde]
  fn get_bounds(&self) -> DesktopRect {
    note_normal_chrome(self.api.as_ref(), self.window_id, &self.normal_chrome);
    outer_bounds(self.api.as_ref(), self.window_id)
  }

  /// The page area: `getInnerPosition()` + `getSize()`.
  #[serde]
  fn get_content_bounds(&self) -> DesktopRect {
    let (x, y) = self.api.get_window_inner_position(self.window_id);
    let (width, height) = self.api.get_window_size(self.window_id);
    DesktopRect {
      x,
      y,
      width,
      height,
    }
  }

  /// Like `getBounds()`, for the bounds the window returns to when it
  /// leaves the maximized / minimized / fullscreen state.
  #[serde]
  fn get_normal_bounds(&self) -> DesktopRect {
    let (chrome_w, chrome_h) = note_normal_chrome(
      self.api.as_ref(),
      self.window_id,
      &self.normal_chrome,
    );
    match self.api.get_normal_bounds(self.window_id) {
      Some((x, y, w, h)) => DesktopRect {
        x,
        y,
        width: w + chrome_w,
        height: h + chrome_h,
      },
      None => outer_bounds(self.api.as_ref(), self.window_id),
    }
  }

  /// Set the outer frame (missing fields keep their value). The result is
  /// kept on-screen (see `ensure_on_screen`).
  fn set_bounds(&self, #[scoped] bounds: BoundsOptions) {
    let current = outer_bounds(self.api.as_ref(), self.window_id);
    let (inner_w, inner_h) = self.api.get_window_size(self.window_id);
    let chrome_w = (current.width - inner_w).max(0);
    let chrome_h = (current.height - inner_h).max(0);
    let target = DesktopRect {
      x: bounds.x.unwrap_or(current.x),
      y: bounds.y.unwrap_or(current.y),
      width: bounds.width.unwrap_or(current.width),
      height: bounds.height.unwrap_or(current.height),
    };
    let rect = place_on_screen(self.api.as_ref(), target);
    if rect.width != current.width || rect.height != current.height {
      self.api.set_window_size(
        self.window_id,
        (rect.width - chrome_w).max(1),
        (rect.height - chrome_h).max(1),
      );
    }
    if rect.x != current.x || rect.y != current.y {
      self.api.set_window_position(self.window_id, rect.x, rect.y);
    }
  }

  /// The id of the display the window is on (`Deno.desktop.screens()`), or
  /// 0 when unknown (display ids are never 0).
  #[fast]
  #[symbol("Deno_privateDesktopScreenId")]
  fn screen_id(&self) -> f64 {
    self.api.window_screen_id(self.window_id).unwrap_or(0) as f64
  }

  // --- Chrome (string mapping in DESKTOP_JS) ---

  #[fast]
  #[symbol("Deno_privateDesktopTitleBarStyle")]
  fn title_bar_style(&self, #[smi] style: i32) -> bool {
    self.api.set_titlebar_style(self.window_id, style)
  }

  #[fast]
  #[symbol("Deno_privateDesktopWindowButtonPosition")]
  fn window_button_position(
    &self,
    reset: bool,
    #[smi] x: i32,
    #[smi] y: i32,
  ) -> bool {
    self.api.set_traffic_light_position(
      self.window_id,
      if reset { None } else { Some((x, y)) },
    )
  }

  #[fast]
  #[symbol("Deno_privateDesktopBackdrop")]
  fn backdrop(&self, #[smi] backdrop: i32, #[smi] material: i32) -> bool {
    self.api.set_backdrop(self.window_id, backdrop, material)
  }

  #[fast]
  fn is_resizable(&self) -> bool {
    self.api.is_resizable(self.window_id)
  }

  #[fast]
  fn set_resizable(&self, resizable: bool) {
    self.api.set_resizable(self.window_id, resizable);
  }

  #[fast]
  fn is_always_on_top(&self) -> bool {
    self.api.is_always_on_top(self.window_id)
  }

  #[fast]
  fn set_always_on_top(&self, always_on_top: bool) {
    self.api.set_always_on_top(self.window_id, always_on_top);
  }

  #[fast]
  fn get_opacity(&self) -> f64 {
    self.api.get_window_opacity(self.window_id)
  }

  #[fast]
  fn set_opacity(&self, opacity: f64) {
    self.api.set_window_opacity(self.window_id, opacity);
  }

  #[fast]
  fn is_closed(&self) -> bool {
    self.api.is_closed(self.window_id)
  }

  #[fast]
  fn close(&self) {
    if self.surface_taken.get() {
      // A WebGPU surface is referencing this window's native handles.
      // Destroying the OS window now would dangle those handles. Hide
      // instead; cleanup happens when the BrowserWindow is GC'd.
      log::warn!(
        "BrowserWindow.close(): a WebGPU surface is still attached; hiding window instead of destroying it"
      );
      self.api.hide(self.window_id);
      return;
    }
    self.api.close_window(self.window_id);
  }

  #[fast]
  fn is_visible(&self) -> bool {
    self.api.is_visible(self.window_id)
  }

  #[fast]
  fn show(&self) {
    self.api.show(self.window_id);
  }

  #[fast]
  fn hide(&self) {
    self.api.hide(self.window_id);
  }

  #[fast]
  fn focus(&self) {
    self.api.focus(self.window_id);
  }

  #[fast]
  fn navigate(&self, #[string] url: &str) {
    self.api.navigate(self.window_id, url);
  }

  fn open_devtools(
    &self,
    #[serde] options: Option<OpenDevtoolsOptions>,
  ) -> Result<(), deno_error::JsErrorBox> {
    let (renderer, deno) = match options {
      Some(opts) => (opts.renderer.unwrap_or(true), opts.deno.unwrap_or(true)),
      None => (true, true),
    };
    if !renderer && !deno {
      return Err(deno_error::JsErrorBox::type_error(
        "At least one of 'renderer' or 'deno' must be true",
      ));
    }
    self.api.open_devtools(self.window_id, renderer, deno);
    Ok(())
  }

  #[fast]
  fn reload(&self) {
    self
      .api
      .execute_js(self.window_id, "location.reload()", Box::new(|_| {}));
  }

  async fn execute_js(
    &self,
    #[string] script: String,
  ) -> Result<ExecuteJsResult, deno_error::JsErrorBox> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    self.api.execute_js(
      self.window_id,
      &script,
      Box::new(move |result| {
        let _ = tx.send(result);
      }),
    );
    let result = rx.await.map_err(|_| {
      deno_error::JsErrorBox::generic("execute_js callback dropped")
    })?;
    Ok(ExecuteJsResult(result))
  }

  fn set_application_menu(&self, #[serde] menu: Vec<MenuItem>) {
    self.api.set_application_menu(self.window_id, menu);
  }

  fn show_context_menu(
    &self,
    #[smi] x: i32,
    #[smi] y: i32,
    #[serde] menu: Vec<MenuItem>,
  ) {
    self.api.show_context_menu(self.window_id, x, y, menu);
  }

  fn get_native_window(
    &self,
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
  ) -> Result<v8::Global<v8::Object>, deno_error::JsErrorBox> {
    let instance = state
      .try_borrow::<deno_webgpu::Instance>()
      .ok_or_else(|| {
        deno_error::JsErrorBox::type_error(
          "Cannot create surface outside of WebGPU context. Did you forget to call `navigator.gpu.requestAdapter()`?",
        )
      })?
      .clone();

    let api = self.api.clone();
    let window_id = self.window_id;

    // Hoisted out of the `surface.try_get` closure so the
    // `get_raw_window_handle` failure path can bubble before we ever
    // touch wgpu (and can't unwind across the laufey C ABI).
    let (win_handle, display_handle) = api.get_raw_window_handle(window_id)?;

    let result = self.surface.try_get(scope, move |_| {
      // SAFETY: The raw handles are valid for the lifetime of the OS window.
      // `BrowserWindow.close()` is suppressed (downgraded to hide) once a
      // surface has been taken (`surface_taken`), and the OS window outlives
      // both the cached `SameObject<UnsafeWindowSurface>` and the
      // BrowserWindow itself, so the handles remain valid for the surface's
      // lifetime.
      let surface_id = unsafe {
        instance
          .instance_create_surface(Some(display_handle), win_handle, None)
          .map_err(|e| {
            deno_error::JsErrorBox::generic(format!(
              "failed to create wgpu surface: {e}"
            ))
          })?
      };
      let (width, height) = api.get_window_size(window_id);
      Ok::<_, deno_error::JsErrorBox>(deno_canvas::byow::UnsafeWindowSurface {
        data: std::rc::Rc::new(RefCell::new(
          deno_webgpu::canvas::SurfaceData {
            id: surface_id,
            width: width as u32,
            height: height as u32,
            instance,
          },
        )),
        active_context: Default::default(),
      })
    })?;
    // Only suppress close() once the surface is actually live. If
    // surface creation failed above, the window is still safe to close.
    self.surface_taken.set(true);
    Ok(result)
  }
}

#[derive(FromV8)]
struct BoundsOptions {
  x: Option<i32>,
  y: Option<i32>,
  width: Option<i32>,
  height: Option<i32>,
}

/// The outer frame of a window: position + chrome-inclusive size.
/// The frame around the page right now, recorded as the normal-state frame
/// when the window is in the normal state. Returns the frame to use for
/// normal bounds: the recorded one, else the current one.
fn note_normal_chrome(
  api: &dyn DesktopApi,
  window_id: u32,
  cache: &std::cell::Cell<Option<(i32, i32)>>,
) -> (i32, i32) {
  normal_chrome(
    api.get_window_outer_size(window_id),
    api.get_window_size(window_id),
    &api.get_window_state(window_id),
    cache,
  )
}

/// The decision behind [`note_normal_chrome`], without the backend.
fn normal_chrome(
  outer: (i32, i32),
  inner: (i32, i32),
  state: &WindowStateInfo,
  cache: &std::cell::Cell<Option<(i32, i32)>>,
) -> (i32, i32) {
  let now = ((outer.0 - inner.0).max(0), (outer.1 - inner.1).max(0));
  if !state.maximized && !state.minimized && !state.fullscreen {
    cache.set(Some(now));
    return now;
  }
  cache.get().unwrap_or(now)
}

fn outer_bounds(api: &dyn DesktopApi, window_id: u32) -> DesktopRect {
  let (x, y) = api.get_window_position(window_id);
  let (width, height) = api.get_window_outer_size(window_id);
  DesktopRect {
    x,
    y,
    width,
    height,
  }
}

/// [`ensure_on_screen`] against the backend's screens, when it can place
/// windows at all (it can't on Wayland).
fn place_on_screen(api: &dyn DesktopApi, rect: DesktopRect) -> DesktopRect {
  if !api.window_capabilities().set_position {
    return rect;
  }
  ensure_on_screen(rect, &api.screens())
}

#[derive(FromV8)]
struct BrowserWindowOptions {
  title: Option<String>,
  width: Option<i32>,
  height: Option<i32>,
  x: Option<i32>,
  y: Option<i32>,
  resizable: Option<bool>,
  always_on_top: Option<bool>,
  opacity: Option<f64>,
  frameless: Option<bool>,
  no_activate: Option<bool>,
  transparent_titlebar: Option<bool>,
  transparent: Option<bool>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuItem {
  Item {
    label: String,
    id: Option<String>,
    accelerator: Option<String>,
    enabled: bool,
    /// Checkmark next to the item. All platforms.
    #[serde(default)]
    checked: bool,
    /// PNG-encoded image bytes shown next to the label. macOS and
    /// Windows only.
    #[serde(with = "serde_bytes", default)]
    icon: Option<Vec<u8>>,
    /// Tooltip shown on hover. macOS only.
    tooltip: Option<String>,
  },
  Submenu {
    label: String,
    items: Vec<MenuItem>,
  },
  Separator,
  Role {
    role: String,
  },
}

/// State for the auto-update system, placed into OpState at init.
pub struct AutoUpdateState {
  /// Path to the currently running dylib on disk.
  pub dylib_path: std::path::PathBuf,
  /// App version from metadata (deno.json `version` field).
  pub app_version: Option<String>,
  /// Whether we rolled back from a failed update on this launch.
  pub rolled_back: bool,
}

/// Hex-decoded length of a SHA-256 digest.
const SHA256_HEX_LEN: usize = 64;

fn dylib_magic_ok(bytes: &[u8]) -> bool {
  if bytes.len() < 4 {
    return false;
  }
  let m = &bytes[..4];
  // Mach-O (32/64 BE/LE), Mach-O fat, ELF, PE/COFF (MZ).
  matches!(
    m,
    [0xFE, 0xED, 0xFA, 0xCE]
      | [0xFE, 0xED, 0xFA, 0xCF]
      | [0xCE, 0xFA, 0xED, 0xFE]
      | [0xCF, 0xFA, 0xED, 0xFE]
      | [0xCA, 0xFE, 0xBA, 0xBE]
      | [0xCA, 0xFE, 0xBA, 0xBF]
      | [0x7F, b'E', b'L', b'F']
  ) || m.starts_with(b"MZ")
}

#[allow(
  clippy::disallowed_methods,
  reason = "privileged auto-update op writes the live dylib outside any user's sandbox by design"
)]
#[op2(fast)]
pub fn op_desktop_apply_patch(
  state: &mut OpState,
  #[buffer] patch_bytes: &[u8],
  #[string] expected_sha256: &str,
) -> Result<(), deno_error::JsErrorBox> {
  let update_state =
    state.try_borrow::<AutoUpdateState>().ok_or_else(|| {
      deno_error::JsErrorBox::generic("Auto-update state not initialized")
    })?;
  let dylib_path = &update_state.dylib_path;

  // Verify the patch bytes against the SHA-256 declared in the manifest before
  // we trust them with `bspatch`. Without this, anyone who can MITM the patch
  // download (or compromise the release host) could deliver arbitrary native
  // code. The hash itself is only as trustworthy as the manifest delivery
  // (TLS) and, when configured, the manifest signature checked in JS.
  let expected_sha256 = expected_sha256.trim().to_ascii_lowercase();
  if expected_sha256.len() != SHA256_HEX_LEN
    || !expected_sha256.chars().all(|c| c.is_ascii_hexdigit())
  {
    return Err(deno_error::JsErrorBox::generic(
      "Auto-update: manifest is missing a valid SHA-256 patch hash",
    ));
  }
  let actual_sha256 = {
    use sha2::Digest;
    faster_hex::hex_string(&sha2::Sha256::digest(patch_bytes)).to_lowercase()
  };
  if actual_sha256 != expected_sha256 {
    return Err(deno_error::JsErrorBox::generic(format!(
      "Auto-update: patch SHA-256 mismatch (expected {expected_sha256}, got {actual_sha256})"
    )));
  }

  let original = std::fs::read(dylib_path).map_err(|e| {
    deno_error::JsErrorBox::generic(format!(
      "Failed to read dylib at {}: {}",
      dylib_path.display(),
      e
    ))
  })?;

  let patcher = qbsdiff::Bspatch::new(patch_bytes).map_err(|e| {
    deno_error::JsErrorBox::generic(format!("Invalid patch: {}", e))
  })?;
  let target_size = patcher.hint_target_size() as usize;
  let mut patched = Vec::with_capacity(target_size);
  patcher
    .apply(&original, std::io::Cursor::new(&mut patched))
    .map_err(|e| {
      deno_error::JsErrorBox::generic(format!("bspatch failed: {}", e))
    })?;

  // Sanity-check the patched bytes look like a real native binary. This
  // doesn't make the file safe to load (the hash check above does that), but
  // it catches a malformed or empty payload before we stage the swap and
  // shrinks the window where rename(2) into place could fail and leave the
  // app without a working dylib.
  if !dylib_magic_ok(&patched) {
    return Err(deno_error::JsErrorBox::generic(
      "Auto-update: patched dylib does not look like a native binary",
    ));
  }

  let update_path = dylib_path.with_extension(format!(
    "{}.update",
    dylib_path.extension().unwrap_or_default().to_string_lossy()
  ));
  std::fs::write(&update_path, &patched).map_err(|e| {
    deno_error::JsErrorBox::generic(format!(
      "Failed to write update to {}: {}",
      update_path.display(),
      e
    ))
  })?;

  log::info!(
    "Update written to {}. Will be applied on next launch.",
    update_path.display()
  );

  Ok(())
}

/// Verify an Ed25519 signature over `message` using the base64-encoded
/// 32-byte public key and base64-encoded 64-byte signature. Inner pure
/// function so it's directly callable from unit tests (the `#[op2]`
/// wrapper replaces the surface name with an `OpDecl`).
fn verify_ed25519_b64(
  public_key_b64: &str,
  signature_b64: &str,
  message: &[u8],
) -> bool {
  use base64::Engine;
  let engine = base64::engine::general_purpose::STANDARD;
  let Ok(pk_bytes) = engine.decode(public_key_b64.trim()) else {
    return false;
  };
  let Ok(sig_bytes) = engine.decode(signature_b64.trim()) else {
    return false;
  };
  let Ok(pk_arr): Result<[u8; 32], _> = pk_bytes.as_slice().try_into() else {
    return false;
  };
  let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else {
    return false;
  };
  let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr)
  else {
    return false;
  };
  let signature = ed25519_dalek::Signature::from_bytes(&sig_arr);
  use ed25519_dalek::Verifier;
  verifying_key.verify(message, &signature).is_ok()
}

/// Verify an Ed25519 signature over `message` using the base64-encoded
/// 32-byte public key and base64-encoded 64-byte signature. Used by the JS
/// auto-update path to validate `latest.json` before fetching any patch.
#[op2(fast)]
pub fn op_desktop_verify_ed25519(
  #[string] public_key_b64: &str,
  #[string] signature_b64: &str,
  #[buffer] message: &[u8],
) -> bool {
  verify_ed25519_b64(public_key_b64, signature_b64, message)
}

#[op2]
#[serde]
async fn op_desktop_recv_event(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<DesktopEvent> {
  let rx = {
    let s = state.borrow();
    s.try_borrow::<DesktopEventReceiver>().map(|r| r.0.clone())
  };
  if let Some(rx) = rx {
    rx.lock().await.recv().await
  } else {
    std::future::pending().await
  }
}

#[allow(
  clippy::disallowed_methods,
  reason = "privileged auto-update sentinel write next to the dylib, outside any user sandbox"
)]
#[op2(fast)]
pub fn op_desktop_confirm_update(state: &mut OpState) {
  if let Some(s) = state.try_borrow::<AutoUpdateState>() {
    let ext = s
      .dylib_path
      .extension()
      .unwrap_or_default()
      .to_string_lossy();
    // The sentinel only has meaning while a freshly-applied update's
    // `.backup` exists (backup-without-sentinel on next boot → rollback).
    // Writing it unconditionally put a stray file inside the app bundle's
    // `Contents/MacOS/` on every first launch, which invalidates the
    // bundle's code signature and even blocks re-signing (#36418).
    let backup = s.dylib_path.with_extension(format!("{}.backup", ext));
    if backup.exists() {
      let sentinel = s.dylib_path.with_extension(format!("{}.update-ok", ext));
      let _ = std::fs::write(&sentinel, b"ok");
    }
  }
}

#[op2]
fn op_desktop_resolve_bind_call(
  state: &mut OpState,
  #[smi] call_id: u32,
  #[serde] result: DesktopValue,
) {
  if let Some(responses) = state.try_borrow::<PendingBindResponses>()
    && let Some(tx) = responses.0.lock().unwrap().remove(&call_id)
  {
    let _ = tx.send(Ok(result));
  }
}

#[op2(fast)]
fn op_desktop_reject_bind_call(
  state: &mut OpState,
  #[smi] call_id: u32,
  #[string] error: String,
) {
  if let Some(responses) = state.try_borrow::<PendingBindResponses>()
    && let Some(tx) = responses.0.lock().unwrap().remove(&call_id)
  {
    let _ = tx.send(Err(error));
  }
}

/// The launch snapshot behind `Deno.desktop.launchUrls` / `launchFiles`.
/// JS calls it once and caches the result; empty outside a desktop app.
#[op2]
#[serde]
fn op_desktop_take_launch_targets(
  state: &mut OpState,
) -> LaunchTargetsSnapshot {
  state
    .try_borrow::<DesktopLaunchInbox>()
    .map(|inbox| inbox.take_launch_targets())
    .unwrap_or_default()
}

/// Subscribe to one kind of launch event (`"openurl"`, `"openfile"`,
/// `"secondinstance"`), returning the events buffered for it; see
/// [`DesktopLaunchInbox`].
#[op2]
#[serde]
fn op_desktop_subscribe_launch_events(
  state: &mut OpState,
  #[string] event_type: &str,
) -> Vec<DesktopEvent> {
  state
    .try_borrow::<DesktopLaunchInbox>()
    .map(|inbox| inbox.subscribe(event_type))
    .unwrap_or_default()
}

fn scheme_handlers(
  state: &std::rc::Rc<std::cell::RefCell<OpState>>,
  scheme: &str,
) -> Result<(Arc<dyn DesktopSchemeHandlers>, String), deno_error::JsErrorBox> {
  let handlers = state
    .borrow()
    .try_borrow::<Arc<dyn DesktopSchemeHandlers>>()
    .cloned()
    .ok_or_else(|| {
      deno_error::JsErrorBox::generic(
        "deep-link scheme registration is not available in this runtime",
      )
    })?;
  let scheme = handlers
    .check_scheme(scheme)
    .map_err(deno_error::JsErrorBox::type_error)?;
  Ok((handlers, scheme))
}

/// Wait for a blocking scheme-handler call, bounded by
/// [`SCHEME_HANDLER_TIMEOUT`].
async fn await_scheme_call<T>(
  call: deno_core::unsync::JoinHandle<T>,
) -> Result<T, deno_error::JsErrorBox> {
  match tokio::time::timeout(SCHEME_HANDLER_TIMEOUT, call).await {
    Ok(Ok(value)) => Ok(value),
    Ok(Err(_join)) => Err(deno_error::JsErrorBox::generic(
      "the deep-link scheme lookup failed",
    )),
    Err(_elapsed) => Err(deno_error::JsErrorBox::generic(format!(
      "the deep-link scheme lookup did not complete within {}s",
      SCHEME_HANDLER_TIMEOUT.as_secs()
    ))),
  }
}

/// `Deno.desktop.getSchemeOwner(scheme)`: who handles one of the app's
/// declared deep-link schemes.
#[op2]
#[serde]
async fn op_desktop_get_scheme_owner(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] scheme: String,
) -> Result<SchemeOwnerInfo, deno_error::JsErrorBox> {
  let (handlers, scheme) = scheme_handlers(&state, &scheme)?;
  await_scheme_call(deno_core::unsync::spawn_blocking(move || {
    handlers.scheme_owner(&scheme)
  }))
  .await
}

/// `Deno.desktop.registerScheme(scheme, { force })`.
#[op2]
#[serde]
async fn op_desktop_register_scheme(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] scheme: String,
  force: bool,
) -> Result<SchemeRegisterInfo, deno_error::JsErrorBox> {
  let (handlers, scheme) = scheme_handlers(&state, &scheme)?;
  await_scheme_call(deno_core::unsync::spawn_blocking(move || {
    handlers.register_scheme(&scheme, force)
  }))
  .await
}

fn desktop_passkeys(
  state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<Arc<dyn DesktopPasskeys>> {
  state
    .borrow()
    .try_borrow::<Arc<dyn DesktopPasskeys>>()
    .cloned()
}

/// `Deno.desktop.passkeys.capabilities()`.
#[op2]
#[serde]
async fn op_desktop_passkey_capabilities(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<PasskeyCapabilitiesInfo, deno_error::JsErrorBox> {
  let Some(passkeys) = desktop_passkeys(&state) else {
    return Ok(PasskeyCapabilitiesInfo::default());
  };
  deno_core::unsync::spawn_blocking(move || passkeys.capabilities())
    .await
    .map_err(|_| {
      deno_error::JsErrorBox::generic("the passkey capability query failed")
    })
}

/// `Deno.desktop.passkeys.create()` / `.get()`: the JSON envelope, never an
/// exception (the JS side validates the argument types).
#[op2]
#[string]
async fn op_desktop_passkey_request(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  create: bool,
  #[smi] window_id: u32,
  #[string] options_json: String,
) -> String {
  match desktop_passkeys(&state) {
    Some(passkeys) => passkeys.request(create, window_id, options_json).await,
    None => PASSKEY_NOT_SUPPORTED_ENVELOPE.to_string(),
  }
}

/// `Deno.desktop.screens()`.
#[op2]
#[serde]
fn op_desktop_screens(state: &mut OpState) -> Vec<ScreenInfo> {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.screens())
    .unwrap_or_default()
}

/// `Deno.desktop.windowCapabilities()`.
#[op2]
#[serde]
fn op_desktop_window_capabilities(
  state: &mut OpState,
) -> WindowCapabilitiesInfo {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.window_capabilities())
    .unwrap_or_default()
}

/// `Deno.desktop.quit()`, once no listener canceled it.
#[op2(fast)]
fn op_desktop_quit(state: &mut OpState) {
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.quit();
  }
}

/// `Deno.desktop.quitOnLastWindowClosed = …`.
#[op2(fast)]
fn op_desktop_set_quit_on_last_window_closed(state: &mut OpState, quit: bool) {
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.set_quit_on_last_window_closed(quit);
  }
}

/// DESKTOP_JS's answer to a `closeRequested` event: whether a `close`
/// listener called `preventDefault()`.
#[op2(fast)]
fn op_desktop_close_reply(
  state: &mut OpState,
  #[smi] window_id: u32,
  prevented: bool,
) {
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.close_reply(window_id, prevented);
  }
}

#[op2(fast)]
pub fn op_desktop_init(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
  webidl_brand: v8::Local<v8::Value>,
  set_event_target_data: v8::Local<v8::Value>,
) {
  state.put(EventTargetSetup {
    brand: v8::Global::new(scope, webidl_brand),
    set_event_target_data: v8::Global::new(scope, set_event_target_data),
  });
}

#[op2(fast)]
fn op_desktop_alert(
  state: &mut OpState,
  #[string] title: &str,
  #[string] message: &str,
) {
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.alert(title, message);
  }
}

/// True while an error dialog is on screen. Single-flight at the native
/// boundary: this op sits on `core.ops` and survives `removeImportedOps()`,
/// so it is reachable by any code in the runtime, not only the error handler
/// that respects its own `_exiting` flag. Without a guard here, a loop
/// calling it directly would park an unbounded number of pool threads, each
/// in a modal dialog nobody may ever dismiss.
static ERROR_DIALOG_SHOWING: std::sync::atomic::AtomicBool =
  std::sync::atomic::AtomicBool::new(false);

/// Clears [`ERROR_DIALOG_SHOWING`] on every exit path, including a panic
/// inside `DesktopApi::alert` — a plain `store` after the call would leave
/// the flag stuck at `true` for the rest of the process, suppressing every
/// later error dialog.
struct ErrorDialogGuard;

impl Drop for ErrorDialogGuard {
  fn drop(&mut self) {
    ERROR_DIALOG_SHOWING.store(false, std::sync::atomic::Ordering::SeqCst);
  }
}

/// How long to wait for the error dialog before exiting anyway.
///
/// The wait is what holds the process open long enough for the dialog to be
/// read, but it must not be unbounded: when the dialog can't be seen or
/// clicked (hidden window, no display, a CI runner) nothing would ever
/// resolve it, and an unhandled rejection would leave the process up for
/// good. The message reaches stderr before the dialog is ever requested, so
/// timing out loses nothing except the click.
const ERROR_DIALOG_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(60);

/// Non-blocking variant of `op_desktop_alert` for runtime error reporting.
///
/// `DesktopApi::alert` blocks its calling thread until the dialog is
/// dismissed. Calling it straight from the `error`/`unhandledrejection`
/// handlers parked the JS thread, freezing the whole runtime — timers,
/// servers, signal handlers — until someone clicked the dialog, and forever
/// if nobody could (hidden window, headless child; #36393). SIGTERM was
/// ignored too, since its handler is JS on the parked thread.
///
/// Running the dialog on the blocking pool and awaiting it keeps the event
/// loop running while it is up. The returned promise is load-bearing: it is
/// what holds the process open until the dialog is dismissed. The JS caller
/// `preventDefault()`s the event and exits once this resolves — a
/// fire-and-forget op would instead let the runtime tear down the instant
/// the handler returned, cutting the dialog off before it appeared.
///
/// Thread-safety: `DesktopApi::alert` may be called from any thread. In a
/// packaged desktop app the Deno runtime already runs on its own
/// `deno-desktop-runtime` thread (`run_on_runtime_thread` in
/// `cli/rt_desktop`), never the laufey UI thread, so every existing
/// `op_desktop_alert` call is already an off-UI-thread call; the backend
/// marshals the dialog onto the UI thread and blocks the caller until it is
/// dismissed (the core dump in #36393 shows exactly that — JS thread parked
/// in `ShowDialog`, GTK thread in `gtk_dialog_run`). The pool thread used
/// here is in the same position the JS thread was, not a new kind of caller.
#[op2]
async fn op_desktop_alert_async(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] title: String,
  #[string] message: String,
) {
  use std::sync::atomic::Ordering;
  let api = {
    let s = state.borrow();
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  let Some(api) = api else {
    // No backend wired up (snapshot build or non-desktop runtime). There is
    // no dialog to wait for, so resolve immediately and let the caller get
    // on with exiting.
    return;
  };
  if ERROR_DIALOG_SHOWING.swap(true, Ordering::SeqCst) {
    // A dialog is already up. The message has already reached stderr, so
    // dropping this one loses nothing — and resolving rather than queueing
    // keeps a caller in a loop from parking a thread per call.
    return;
  }
  let dialog = deno_core::unsync::spawn_blocking(move || {
    let _guard = ErrorDialogGuard;
    api.alert(&title, &message);
  });
  // Three ways out, all of which must let the caller exit: dismissed, the
  // backend panicked (join error), or nobody could click it.
  //
  // The timeout bounds the *wait*, not the dialog. A `spawn_blocking` task
  // can't be cancelled, so on timeout the pool thread stays inside
  // `api.alert` until it returns — which in that case is never — and
  // `ErrorDialogGuard` therefore never drops, leaving
  // `ERROR_DIALOG_SHOWING` set for the rest of the process. That is the
  // safe direction: it means no later caller can park a second thread. The
  // caller exits either way, which is the point.
  let _ = tokio::time::timeout(ERROR_DIALOG_TIMEOUT, dialog).await;
}

struct ErrorReportConfig {
  url: String,
  app_version: Option<String>,
}

static ERROR_REPORT_CONFIG: OnceLock<ErrorReportConfig> = OnceLock::new();

/// Store the error reporting URL and app version so the panic hook can
/// send reports without access to OpState.
pub fn set_error_report_config(url: String, app_version: Option<String>) {
  let _ = ERROR_REPORT_CONFIG.set(ErrorReportConfig { url, app_version });
}

/// Returns the error reporting URL and app version, if configured.
pub fn error_report_config() -> Option<(&'static str, Option<&'static str>)> {
  ERROR_REPORT_CONFIG
    .get()
    .map(|c| (c.url.as_str(), c.app_version.as_deref()))
}

/// Stash of the `OpState` HTTP client for the panic-hook path. The panic
/// hook can't reach `OpState`, so we capture a client when the runtime
/// initializes error reporting and reuse it from both code paths. This
/// keeps a single TLS configuration (the user's roots) — earlier the panic
/// path constructed an ad-hoc `reqwest`/`fetch` client that bypassed it.
static ERROR_REPORT_CLIENT: OnceLock<deno_fetch::Client> = OnceLock::new();

/// Capture the OpState HTTP client for use by the panic hook.
pub fn set_error_report_client(client: deno_fetch::Client) {
  let _ = ERROR_REPORT_CLIENT.set(client);
}

#[allow(
  clippy::disallowed_methods,
  reason = "best-effort panic-hook error-report append; path is operator-configured via `error_reporting_url` and FileSystem trait isn't reachable from a panic hook"
)]
fn append_to_file(path: &Path, body: &str) {
  let mut line = body.to_string();
  line.push('\n');
  let _ = std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
    .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

fn post_error_report(client: deno_fetch::Client, url: String, body: String) {
  let _ = std::thread::spawn(move || {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
      .enable_io()
      .enable_time()
      .build()
    else {
      return;
    };
    runtime.block_on(async move {
      let Ok(uri) = url.parse::<http::Uri>() else {
        return;
      };
      let mut req = http::Request::new(deno_fetch::ReqBody::full(body.into()));
      *req.method_mut() = http::Method::POST;
      *req.uri_mut() = uri;
      req.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
      );
      let _ = client.send(req).await;
    });
  })
  .join();
}

/// Send a JSON error report to the given URL. Best-effort — never panics.
/// Accepts only `file://` and `https://`. Plain `http://` is rejected:
/// error reports usually carry stack traces and runtime context, so
/// anyone on-path could read them. A bare path (or any unparseable
/// string) is also rejected — previously such inputs were silently
/// treated as a local file path, which let a malformed metadata field
/// land error reports at an attacker-chosen location on disk.
pub fn send_error_report(url: &str, body: &str) {
  let Ok(parsed) = deno_core::url::Url::parse(url) else {
    log::warn!(
      "desktop: error_reporting_url is not a valid URL ({:?}); dropping report",
      url,
    );
    return;
  };

  match parsed.scheme() {
    "file" => {
      // `url_to_file_path` rejects `file://host/...` URLs (non-local),
      // so a local path is the only way to reach `append_to_file`.
      let Ok(path) = deno_path_util::url_to_file_path(&parsed) else {
        log::warn!(
          "desktop: error_reporting_url file:// URL is not a local path ({:?}); dropping report",
          url,
        );
        return;
      };
      append_to_file(&path, body);
    }
    "https" => {
      let Some(client) = ERROR_REPORT_CLIENT.get().cloned() else {
        log::warn!(
          "desktop: error-report HTTP client not initialized; dropping report"
        );
        return;
      };
      post_error_report(client, parsed.to_string(), body.to_string());
    }
    other => {
      log::warn!(
        "desktop: refusing to send error report over '{other}' (file:// or https:// only); dropping report",
      );
    }
  }
}

#[op2(fast)]
fn op_desktop_send_error_report(state: &mut OpState, #[string] body: &str) {
  // The report destination is operator config — it is baked into the app at
  // build time (`error_reporting_url`) and stored in `ERROR_REPORT_CONFIG`.
  // It is deliberately NOT accepted from JS: this op is exposed on
  // `core.ops` and survives `removeImportedOps()`, so any (untrusted) code
  // in the runtime can call it. Trusting a caller-supplied URL would turn
  // this into an unrestricted file-append (`file://`) or network-POST
  // (`https://`) primitive that bypasses the `--allow-write`/`--allow-net`
  // permission checks every other fs/net op performs.
  let Some((url, _)) = error_report_config() else {
    // No reporting URL configured (e.g. plain `deno run`, or a desktop app
    // that didn't set one) — there is nowhere to send, so do nothing.
    return;
  };
  // Make sure the panic-hook path has a client too. The OpState client is
  // the one configured with the user's TLS roots/permissions, so we share
  // it across both code paths instead of creating an ad-hoc client.
  if ERROR_REPORT_CLIENT.get().is_none()
    && let Ok(client) = deno_fetch::get_or_create_client_from_state(state)
  {
    set_error_report_client(client);
  }
  send_error_report(url, body);
}

#[op2(fast)]
fn op_desktop_confirm(state: &mut OpState, #[string] message: &str) -> bool {
  // Sync op: web `confirm()` returns a boolean, not a Promise. The
  // backend's `confirm` blocks the calling thread inside the platform's
  // modal run loop (NSAlert runModal / MessageBoxW / gtk_dialog_run /
  // rfd) which itself pumps OS events, so other windows stay responsive
  // while the dialog is up.
  match state.try_borrow::<Arc<dyn DesktopApi>>() {
    Some(api) => api.confirm("", message),
    None => false,
  }
}

#[op2]
#[string]
fn op_desktop_prompt(
  state: &mut OpState,
  #[string] message: &str,
  #[string] default_value: Option<String>,
) -> Option<String> {
  // See `op_desktop_confirm` for the sync-blocking rationale.
  match state.try_borrow::<Arc<dyn DesktopApi>>() {
    Some(api) => {
      api.prompt("", message, default_value.as_deref().unwrap_or(""))
    }
    None => None,
  }
}

/// How long to wait on a clipboard call before giving up.
///
/// On X11 and Wayland there is no central clipboard store: the call is
/// serviced by whichever application owns the selection, and
/// `gtk_clipboard_wait_for_text` has no timeout of its own. The blocking pool
/// these calls run on is shared and bounded, so an unbounded wait turns one
/// unresponsive peer into starvation for every other blocking task in the
/// runtime. Short enough not to strand a caller, long enough that a merely
/// slow owner still succeeds.
const CLIPBOARD_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(5);

/// Read the clipboard's text off the JS thread.
///
/// `DesktopApi::read_clipboard_text` is synchronous, and on X11/Wayland the
/// clipboard has no central store: the read is serviced by whichever
/// application currently owns the selection, and `gtk_clipboard_wait_for_text`
/// has no timeout. An unresponsive owner therefore blocks the caller for as
/// long as it likes. Running that on the JS thread would freeze the entire
/// runtime — timers, servers, signal handlers — which is the same failure
/// mode as the error dialog in #36393, and the `Promise` this op returns to
/// `navigator.clipboard.readText()` would have made it look impossible.
///
/// Thread-safety: in a packaged desktop app the runtime already runs on its
/// own `deno-desktop-runtime` thread (`run_on_runtime_thread` in
/// `cli/rt_desktop`), never the laufey UI thread, so the existing call was
/// already an off-UI-thread one that the backend marshals; the pool thread
/// used here is in the same position, not a new kind of caller.
#[op2]
#[string]
async fn op_desktop_read_clipboard_text(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<Option<String>, deno_error::JsErrorBox> {
  let api = {
    let s = state.borrow();
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  let Some(api) = api else {
    return Ok(None);
  };
  // The runtime's bounded blocking pool, not a fresh thread per call:
  // `readText()` is an ordinary API an app may poll on an interval, and
  // nothing here rate-limits it.
  //
  // The pool being bounded is also why the timeout matters. A read is
  // serviced by whichever app owns the selection and can block for as long
  // as that app likes, and a `spawn_blocking` task can't be cancelled — so
  // without a bound on the wait, enough hung reads would occupy pool threads
  // permanently and starve every other blocking task in the runtime, not
  // just the caller. Timing out doesn't reclaim the thread, but it stops the
  // caller adding more of them behind an unbounded await.
  //
  // A join error (a backend that panics mid-read) yields `None` too, rather
  // than leaving the caller's promise pending forever.
  let read =
    deno_core::unsync::spawn_blocking(move || api.read_clipboard_text());
  // Reject rather than resolve on either failure. Resolving would hand back
  // `""`, which is exactly what a genuinely empty clipboard returns, so a
  // caller could not tell "nothing was copied" from "the owning app is
  // wedged" — and `if (await navigator.clipboard.readText())` would quietly
  // take the empty branch. The spec rejects here too.
  //
  // The two failures get different messages: a panic inside the backend has
  // nothing to do with a timeout or with another application, and pointing
  // someone at their window manager for it would be an actively wrong
  // diagnosis.
  match tokio::time::timeout(CLIPBOARD_TIMEOUT, read).await {
    Ok(Ok(text)) => Ok(text),
    Ok(Err(_join)) => Err(clipboard_failed("read")),
    Err(_elapsed) => Err(clipboard_unavailable("read")),
  }
}

/// The error a clipboard op rejects with when the backend call itself failed
/// — i.e. it panicked, so the blocking task's join returned an error. Kept
/// distinct from [`clipboard_unavailable`]: nothing timed out and no other
/// application was involved.
fn clipboard_failed(op: &str) -> deno_error::JsErrorBox {
  deno_error::JsErrorBox::generic(format!("clipboard {op} failed"))
}

/// The error a clipboard op rejects with when the call didn't finish in time.
///
/// Names the unresponsive-owner case specifically: on X11/Wayland the call is
/// serviced by whichever application owns the selection, and that being stuck
/// is the one thing a user can actually act on. Only for the timeout — see
/// [`clipboard_failed`] for a backend that failed outright.
fn clipboard_unavailable(op: &str) -> deno_error::JsErrorBox {
  deno_error::JsErrorBox::generic(format!(
    "clipboard {op} did not complete within {}s - the application that owns \
     the clipboard may be unresponsive",
    CLIPBOARD_TIMEOUT.as_secs()
  ))
}

/// Write the clipboard's text off the JS thread. Blocking semantics — and the
/// reason for going through the blocking pool — as
/// `op_desktop_read_clipboard_text`.
#[op2]
async fn op_desktop_write_clipboard_text(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] text: String,
) -> Result<(), deno_error::JsErrorBox> {
  let api = {
    let s = state.borrow();
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  let Some(api) = api else {
    return Ok(());
  };
  let write = deno_core::unsync::spawn_blocking(move || {
    api.write_clipboard_text(&text);
  });
  // `writeText()`'s whole contract is that resolution means the write
  // happened, so discarding the timeout here would make an app report
  // "Copied!" in precisely the case the timeout exists to catch.
  match tokio::time::timeout(CLIPBOARD_TIMEOUT, write).await {
    Ok(Ok(())) => Ok(()),
    Ok(Err(_join)) => Err(clipboard_failed("write")),
    Err(_elapsed) => Err(clipboard_unavailable("write")),
  }
}

fn permission_state_to_web_string(state: PermissionState) -> &'static str {
  // Web Permissions API state values; `Notification.requestPermission`
  // additionally maps `Prompt` → `"default"` per the Notifications spec.
  match state {
    PermissionState::Granted => "granted",
    PermissionState::Denied => "denied",
    PermissionState::Prompt => "prompt",
    PermissionState::Unsupported => "unsupported",
  }
}

#[op2]
#[string]
async fn op_desktop_request_notification_permission(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> String {
  let api = {
    let s = state.borrow();
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  let Some(api) = api else {
    // No backend wired up (snapshot build or non-desktop runtime).
    return "unsupported".to_string();
  };
  let (tx, rx) = tokio::sync::oneshot::channel::<PermissionState>();
  api.request_notification_permission(Box::new(move |state| {
    let _ = tx.send(state);
  }));
  // If the backend forgets to invoke the callback (programmer error in a
  // hypothetical custom backend), the channel drops and `recv` returns
  // `Err` — surface that as "unsupported" so JS gets a stable result.
  permission_state_to_web_string(
    rx.await.unwrap_or(PermissionState::Unsupported),
  )
  .to_string()
}

#[op2]
#[string]
async fn op_desktop_query_notification_permission(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> String {
  let api = {
    let s = state.borrow();
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  let Some(api) = api else {
    return "unsupported".to_string();
  };
  let (tx, rx) = tokio::sync::oneshot::channel::<PermissionState>();
  api.query_notification_permission(Box::new(move |state| {
    let _ = tx.send(state);
  }));
  permission_state_to_web_string(
    rx.await.unwrap_or(PermissionState::Unsupported),
  )
  .to_string()
}

struct Dock {
  api: Arc<dyn DesktopApi>,
}

// SAFETY: we're sure this can be GCed
unsafe impl deno_core::GarbageCollected for Dock {
  fn trace(&self, _visitor: &mut deno_core::v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Dock"
  }
}

impl deno_core::Resource for Dock {
  fn name(&self) -> Cow<'_, str> {
    "Dock".into()
  }
}

#[op2]
impl Dock {
  #[constructor]
  fn new(
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
  ) -> v8::Global<v8::Value> {
    let api = state
      .try_borrow::<Arc<dyn DesktopApi>>()
      .expect("desktop mode enabled")
      .clone();

    let dock = Dock { api };
    let dock = deno_core::cppgc::make_cppgc_object(scope, dock);
    let event_target_setup = state.borrow::<EventTargetSetup>();
    let webidl_brand = v8::Local::new(scope, event_target_setup.brand.clone());
    dock.set(scope, webidl_brand, webidl_brand);
    let set_event_target_data =
      v8::Local::new(scope, event_target_setup.set_event_target_data.clone())
        .cast::<v8::Function>();
    let null = v8::null(scope);
    set_event_target_data.call(scope, null.into(), &[dock.into()]);
    let dock = dock.cast::<v8::Value>();

    v8::Global::new(scope, dock)
  }

  #[fast]
  fn set_badge(&self, #[string] text: &str) {
    self.api.set_dock_badge(text);
  }

  #[fast]
  fn bounce(&self, critical: bool) {
    self.api.bounce_dock(critical);
  }

  fn set_menu(&self, #[serde] menu: Option<Vec<MenuItem>>) {
    self.api.set_dock_menu(menu);
  }

  #[fast]
  fn set_visible(&self, visible: bool) {
    self.api.set_dock_visible(visible);
  }
}

struct Tray {
  api: Arc<dyn DesktopApi>,
  tray_id: u32,
}

// SAFETY: we're sure this can be GCed
unsafe impl deno_core::GarbageCollected for Tray {
  fn trace(&self, _visitor: &mut deno_core::v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Tray"
  }
}

impl deno_core::Resource for Tray {
  fn name(&self) -> Cow<'_, str> {
    "Tray".into()
  }
}

#[op2]
impl Tray {
  #[constructor]
  fn new(
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
  ) -> v8::Global<v8::Value> {
    let api = state
      .try_borrow::<Arc<dyn DesktopApi>>()
      .expect("desktop mode enabled")
      .clone();

    let tray_id = api.create_tray();
    let tray = Tray { api, tray_id };
    let tray = deno_core::cppgc::make_cppgc_object(scope, tray);
    let event_target_setup = state.borrow::<EventTargetSetup>();
    let webidl_brand = v8::Local::new(scope, event_target_setup.brand.clone());
    tray.set(scope, webidl_brand, webidl_brand);
    let set_event_target_data =
      v8::Local::new(scope, event_target_setup.set_event_target_data.clone())
        .cast::<v8::Function>();
    let null = v8::null(scope);
    set_event_target_data.call(scope, null.into(), &[tray.into()]);
    let tray = tray.cast::<v8::Value>();

    v8::Global::new(scope, tray)
  }

  #[getter]
  fn tray_id(&self) -> u32 {
    self.tray_id
  }

  #[fast]
  fn set_icon(&self, #[buffer] png_bytes: &[u8]) {
    self.api.set_tray_icon(self.tray_id, png_bytes);
  }

  fn set_icon_dark(&self, #[buffer] png_bytes: Option<&[u8]>) {
    self.api.set_tray_icon_dark(self.tray_id, png_bytes);
  }

  fn set_tooltip(&self, #[string] text: Option<String>) {
    self.api.set_tray_tooltip(self.tray_id, text.as_deref());
  }

  fn set_menu(&self, #[serde] menu: Option<Vec<MenuItem>>) {
    self.api.set_tray_menu(self.tray_id, menu);
  }

  #[serde]
  fn get_bounds(&self) -> Option<TrayBounds> {
    self
      .api
      .get_tray_bounds(self.tray_id)
      .map(|(x, y, width, height)| TrayBounds {
        x,
        y,
        width,
        height,
      })
  }

  // DESKTOP_JS exposes the public `destroy` wrapper that also updates its tray
  // registry. Keep the native primitive on a distinct symbol-backed slot.
  #[fast]
  #[symbol("Deno_privateDesktopTrayDestroy")]
  fn destroy(&self) {
    self.api.destroy_tray(self.tray_id);
  }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayBounds {
  x: i32,
  y: i32,
  width: i32,
  height: i32,
}

struct Notification {
  api: Arc<dyn DesktopApi>,
  notification_id: u32,
  title: String,
  body: String,
  icon: String,
  tag: String,
  dir: String,
  lang: String,
  badge: String,
  silent: Option<bool>,
  require_interaction: bool,
  data: v8::Global<v8::Value>,
}

// SAFETY: we're sure this can be GCed
unsafe impl deno_core::GarbageCollected for Notification {
  fn trace(&self, _visitor: &mut deno_core::v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Notification"
  }
}

impl deno_core::Resource for Notification {
  fn name(&self) -> Cow<'_, str> {
    "Notification".into()
  }
}

#[derive(FromV8)]
struct NotificationConstructorOptions {
  body: Option<String>,
  icon: Option<String>,
  tag: Option<String>,
  dir: Option<String>,
  lang: Option<String>,
  badge: Option<String>,
  silent: Option<bool>,
  require_interaction: Option<bool>,
  data: Option<v8::Global<v8::Value>>,
}

#[op2]
impl Notification {
  #[constructor]
  fn new(
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
    #[string] title: String,
    #[scoped] options: Option<NotificationConstructorOptions>,
    #[buffer] icon_bytes: Option<&[u8]>,
  ) -> v8::Global<v8::Value> {
    let api = state
      .try_borrow::<Arc<dyn DesktopApi>>()
      .expect("desktop mode enabled")
      .clone();

    let options = options.unwrap_or(NotificationConstructorOptions {
      body: None,
      icon: None,
      tag: None,
      dir: None,
      lang: None,
      badge: None,
      silent: None,
      require_interaction: None,
      data: None,
    });

    let notification_id = api.show_notification(
      &title,
      options.body.as_deref(),
      icon_bytes,
      options.tag.as_deref(),
      options.silent,
      options.require_interaction,
    );

    let data = options.data.unwrap_or_else(|| {
      let null: v8::Local<v8::Value> = v8::null(scope).into();
      v8::Global::new(scope, null)
    });

    let notification = Notification {
      api,
      notification_id,
      title,
      body: options.body.unwrap_or_default(),
      icon: options.icon.unwrap_or_default(),
      tag: options.tag.unwrap_or_default(),
      dir: options.dir.unwrap_or_else(|| "auto".to_string()),
      lang: options.lang.unwrap_or_default(),
      badge: options.badge.unwrap_or_default(),
      silent: options.silent,
      require_interaction: options.require_interaction.unwrap_or(false),
      data,
    };
    let notification = deno_core::cppgc::make_cppgc_object(scope, notification);
    let event_target_setup = state.borrow::<EventTargetSetup>();
    let webidl_brand = v8::Local::new(scope, event_target_setup.brand.clone());
    notification.set(scope, webidl_brand, webidl_brand);
    let set_event_target_data =
      v8::Local::new(scope, event_target_setup.set_event_target_data.clone())
        .cast::<v8::Function>();
    let null = v8::null(scope);
    set_event_target_data.call(scope, null.into(), &[notification.into()]);
    let notification = notification.cast::<v8::Value>();

    v8::Global::new(scope, notification)
  }

  #[getter]
  fn notification_id(&self) -> u32 {
    self.notification_id
  }

  #[getter]
  #[string]
  fn title(&self) -> String {
    self.title.clone()
  }

  #[getter]
  #[string]
  fn body(&self) -> String {
    self.body.clone()
  }

  #[getter]
  #[string]
  fn icon(&self) -> String {
    self.icon.clone()
  }

  #[getter]
  #[string]
  fn tag(&self) -> String {
    self.tag.clone()
  }

  #[getter]
  #[string]
  fn dir(&self) -> String {
    self.dir.clone()
  }

  #[getter]
  #[string]
  fn lang(&self) -> String {
    self.lang.clone()
  }

  #[getter]
  #[string]
  fn badge(&self) -> String {
    self.badge.clone()
  }

  #[getter]
  fn silent<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> v8::Local<'a, v8::Value> {
    match self.silent {
      Some(b) => v8::Boolean::new(scope, b).into(),
      None => v8::null(scope).into(),
    }
  }

  #[fast]
  #[getter]
  fn require_interaction(&self) -> bool {
    self.require_interaction
  }

  #[getter]
  fn data(&self) -> v8::Global<v8::Value> {
    self.data.clone()
  }

  #[fast]
  fn close(&self) {
    if self.notification_id != 0 {
      self.api.close_notification(self.notification_id);
    }
  }
}

deno_core::extension!(
  deno_desktop,
  ops = [
    op_desktop_apply_patch,
    op_desktop_verify_ed25519,
    op_desktop_confirm_update,
    op_desktop_init,
    op_desktop_recv_event,
    op_desktop_take_launch_targets,
    op_desktop_subscribe_launch_events,
    op_desktop_get_scheme_owner,
    op_desktop_register_scheme,
    op_desktop_passkey_capabilities,
    op_desktop_passkey_request,
    op_desktop_resolve_bind_call,
    op_desktop_reject_bind_call,
    op_desktop_alert,
    op_desktop_alert_async,
    op_desktop_confirm,
    op_desktop_prompt,
    op_desktop_read_clipboard_text,
    op_desktop_write_clipboard_text,
    op_desktop_send_error_report,
    op_desktop_request_notification_permission,
    op_desktop_query_notification_permission,
    op_desktop_screens,
    op_desktop_window_capabilities,
    op_desktop_quit,
    op_desktop_set_quit_on_last_window_closed,
    op_desktop_close_reply,
  ],
  objects = [BrowserWindow, Dock, Tray, Notification],
);

#[cfg(test)]
mod tests {
  use deno_core::serde_json;
  use deno_core::serde_json::json;

  use super::BrowserWindow;
  use super::DesktopEvent;
  use super::DesktopValue;
  use super::MenuItem;
  use super::PASSKEY_NOT_SUPPORTED_ENVELOPE;
  use super::PasskeyCapabilitiesInfo;
  use super::PendingBindResponses;
  use super::PermissionState;
  use super::Tray;
  use super::dylib_magic_ok;
  use super::permission_state_to_web_string;
  use super::register_bind_call;
  use super::verify_ed25519_b64;

  // These tests pin the wire format that the DESKTOP_JS event-loop
  // IIFE consumes. Changing any of these shapes is a breaking change
  // for in-renderer event listeners — the assertions below should
  // fail if you change a field name or remove a `#[serde(rename_all =
  // "camelCase")]` so you find out at test time, not at runtime in the
  // packaged app.

  #[test]
  fn js_wrapped_desktop_methods_use_private_symbols() {
    for (object, methods) in [
      (
        BrowserWindow::DECL,
        &["Deno_privateDesktopBind", "Deno_privateDesktopUnbind"][..],
      ),
      (Tray::DECL, &["Deno_privateDesktopTrayDestroy"][..]),
    ] {
      for name in methods {
        let method = object
          .methods
          .iter()
          .find(|method| method.name == *name)
          .unwrap_or_else(|| panic!("missing method {name}"));
        assert!(
          method.symbol_for,
          "{name} must not share a string property with its DESKTOP_JS wrapper"
        );
        let _ = method.fast_fn();
      }
    }

    for (object, public_names) in [
      (BrowserWindow::DECL, &["bind", "unbind"][..]),
      (Tray::DECL, &["destroy"][..]),
    ] {
      for name in public_names {
        assert!(
          object.methods.iter().all(|method| method.name != *name),
          "native method must not collide with the public {name} wrapper"
        );
      }
    }
  }

  #[test]
  fn menu_item_wire_shape_new_fields_are_optional() {
    // Pre-existing callers only pass label/enabled (+ optional id and
    // accelerator); checked/icon/tooltip must default rather than error.
    let item: MenuItem = serde_json::from_value(json!({
      "item": { "label": "Save", "enabled": true }
    }))
    .unwrap();
    match item {
      MenuItem::Item {
        checked,
        icon,
        tooltip,
        ..
      } => {
        assert!(!checked);
        assert!(icon.is_none());
        assert!(tooltip.is_none());
      }
      _ => panic!("expected Item"),
    }

    let item: MenuItem = serde_json::from_value(json!({
      "item": {
        "label": "Mute",
        "enabled": true,
        "checked": true,
        "icon": [0x89, 0x50, 0x4E, 0x47],
        "tooltip": "Silence notifications",
      }
    }))
    .unwrap();
    match item {
      MenuItem::Item {
        checked,
        icon,
        tooltip,
        ..
      } => {
        assert!(checked);
        assert_eq!(icon.as_deref(), Some(&[0x89u8, 0x50, 0x4E, 0x47][..]));
        assert_eq!(tooltip.as_deref(), Some("Silence notifications"));
      }
      _ => panic!("expected Item"),
    }
  }

  #[test]
  fn app_menu_click_wire_shape() {
    let v = serde_json::to_value(DesktopEvent::AppMenuClick {
      window_id: 7,
      id: "file.quit".to_string(),
    })
    .unwrap();
    assert_eq!(
      v,
      json!({
        "kind": "appMenuClick",
        "windowId": 7,
        "id": "file.quit",
      })
    );
  }

  #[test]
  fn keyboard_event_camelcases_and_keeps_type() {
    let v = serde_json::to_value(DesktopEvent::KeyboardEvent {
      window_id: 1,
      r#type: "keydown".to_string(),
      key: "a".to_string(),
      code: "KeyA".to_string(),
      shift: true,
      control: false,
      alt: false,
      meta: true,
      repeat: false,
    })
    .unwrap();
    // `type` (a Rust keyword, written `r#type`) must serialize as
    // `"type"` — the renderer reads it as `e.type` per Web spec.
    assert_eq!(v["type"], "keydown");
    assert_eq!(v["kind"], "keyboardEvent");
    assert_eq!(v["windowId"], 1);
    assert_eq!(v["shift"], true);
    assert_eq!(v["meta"], true);
  }

  #[test]
  fn mouse_click_uses_client_xy() {
    let v = serde_json::to_value(DesktopEvent::MouseClick {
      window_id: 1,
      state: "released".to_string(),
      button: 0,
      client_x: 10.5,
      client_y: 20.25,
      shift: false,
      control: false,
      alt: false,
      meta: false,
      click_count: 1,
    })
    .unwrap();
    assert_eq!(v["kind"], "mouseClick");
    // The renderer reads `e.clientX` / `e.clientY` per the Web spec.
    // Snake-case names here would silently break the JS side.
    assert_eq!(v["clientX"], 10.5);
    assert_eq!(v["clientY"], 20.25);
    assert_eq!(v["clickCount"], 1);
  }

  #[test]
  fn window_resize_wire_shape() {
    let v = serde_json::to_value(DesktopEvent::WindowResize {
      window_id: 1,
      width: 800,
      height: 600,
    })
    .unwrap();
    assert_eq!(
      v,
      json!({
        "kind": "windowResize",
        "windowId": 1,
        "width": 800,
        "height": 600,
      })
    );
  }

  #[test]
  fn window_state_and_display_wire_shapes() {
    let v = serde_json::to_value(DesktopEvent::WindowState {
      window_id: 3,
      state: super::WindowStateInfo {
        maximized: true,
        minimized: false,
        fullscreen: false,
      },
      previous: super::WindowStateInfo::default(),
    })
    .unwrap();
    assert_eq!(
      v,
      json!({
        "kind": "windowState",
        "windowId": 3,
        "state": { "maximized": true, "minimized": false, "fullscreen": false },
        "previous": { "maximized": false, "minimized": false, "fullscreen": false },
      })
    );
    assert_eq!(
      serde_json::to_value(DesktopEvent::DisplayChanged).unwrap(),
      json!({ "kind": "displayChanged" })
    );
  }

  #[test]
  fn screen_and_capabilities_wire_shapes() {
    let screen = super::ScreenInfo {
      id: 69734272,
      bounds: super::DesktopRect {
        x: 0,
        y: 0,
        width: 1440,
        height: 900,
      },
      work_area: super::DesktopRect {
        x: 0,
        y: 25,
        width: 1440,
        height: 875,
      },
      scale_factor: 2.0,
      is_primary: true,
    };
    assert_eq!(
      serde_json::to_value(screen).unwrap(),
      json!({
        "id": 69734272,
        "bounds": { "x": 0, "y": 0, "width": 1440, "height": 900 },
        "workArea": { "x": 0, "y": 25, "width": 1440, "height": 875 },
        "scaleFactor": 2.0,
        "isPrimary": true,
      })
    );
    let caps = serde_json::to_value(super::WindowCapabilitiesInfo {
      state: true,
      tabbed: true,
      window_button_position: true,
      ..Default::default()
    })
    .unwrap();
    let keys: Vec<&str> = caps
      .as_object()
      .unwrap()
      .keys()
      .map(|k| k.as_str())
      .collect();
    for key in [
      "state",
      "stateEvents",
      "sizeConstraints",
      "screens",
      "displayEvents",
      "titleBarHidden",
      "titleBarHiddenInset",
      "windowButtonPosition",
      "mica",
      "acrylic",
      "tabbed",
      "vibrancy",
      "normalBounds",
      "keepAlive",
      "setPosition",
    ] {
      assert!(keys.contains(&key), "missing {key}");
    }
    assert_eq!(caps["tabbed"], json!(true));
    assert_eq!(caps["mica"], json!(false));
  }

  fn rect(x: i32, y: i32, width: i32, height: i32) -> super::DesktopRect {
    super::DesktopRect {
      x,
      y,
      width,
      height,
    }
  }

  fn screen(
    id: i64,
    work: super::DesktopRect,
    primary: bool,
  ) -> super::ScreenInfo {
    super::ScreenInfo {
      id,
      bounds: work,
      work_area: work,
      scale_factor: 1.0,
      is_primary: primary,
    }
  }

  #[test]
  fn normal_chrome_keeps_the_frame_seen_while_normal() {
    let cache = std::cell::Cell::new(None);
    let normal = super::WindowStateInfo::default();
    let maximized = super::WindowStateInfo {
      maximized: true,
      ..Default::default()
    };
    // Maximized before anything was seen: the current frame is all there is.
    assert_eq!(
      super::normal_chrome((800, 600), (800, 600), &maximized, &cache),
      (0, 0)
    );
    assert_eq!(cache.get(), None);
    // Normal: the frame is recorded.
    assert_eq!(
      super::normal_chrome((650, 454), (640, 426), &normal, &cache),
      (10, 28)
    );
    // Maximized under a window manager that drops the borders: the
    // recorded frame wins, so the restored bounds aren't undersized.
    assert_eq!(
      super::normal_chrome((1280, 1024), (1280, 1024), &maximized, &cache),
      (10, 28)
    );
    // Normal again with another frame: re-recorded.
    assert_eq!(
      super::normal_chrome((640, 452), (640, 424), &normal, &cache),
      (0, 28)
    );
  }

  #[test]
  fn ensure_on_screen_keeps_reachable_windows() {
    use super::ensure_on_screen;
    let screens = [
      screen(1, rect(0, 25, 1440, 875), true),
      screen(2, rect(1440, 0, 1920, 1080), false),
    ];
    // Fully on a screen, on the secondary, straddling both.
    for r in [
      rect(100, 100, 800, 600),
      rect(2000, 100, 800, 600),
      rect(1200, 100, 800, 600),
    ] {
      assert_eq!(ensure_on_screen(r, &screens), r);
    }
    // Mostly off-screen but a 64x32 corner is still inside: left alone.
    let r = rect(-736, 25 + 875 - 32, 800, 600);
    assert_eq!(ensure_on_screen(r, &screens), r);
    // No screens known: nothing to check against.
    let r = rect(-5000, -5000, 10, 10);
    assert_eq!(ensure_on_screen(r, &[]), r);
  }

  #[test]
  fn ensure_on_screen_moves_windows_from_a_missing_monitor() {
    use super::ensure_on_screen;
    // The saved bounds were on a monitor to the right that is gone.
    let screens = [screen(1, rect(0, 25, 1440, 875), true)];
    let moved = ensure_on_screen(rect(2000, 100, 800, 600), &screens);
    assert_eq!(moved, rect(320, 162, 800, 600));
    // Only a sliver (less than 64x32) shows: moved too.
    let moved = ensure_on_screen(rect(1400, 100, 800, 600), &screens);
    assert_eq!(moved, rect(320, 162, 800, 600));
    // Bigger than the work area: shrunk to fit.
    let moved = ensure_on_screen(rect(-4000, 0, 3000, 2000), &screens);
    assert_eq!(moved, rect(0, 25, 1440, 875));
    // The primary screen is the target even when it is not first.
    let screens = [
      screen(2, rect(-1920, 0, 1920, 1080), false),
      screen(1, rect(0, 0, 1000, 1000), true),
    ];
    let moved = ensure_on_screen(rect(5000, 5000, 200, 100), &screens);
    assert_eq!(moved, rect(400, 450, 200, 100));
  }

  #[test]
  fn pending_closes_answer_once() {
    use super::CloseDecision;
    use super::PendingCloses;
    let p = PendingCloses::default();
    // Not canceled: close.
    p.begin(1);
    assert_eq!(p.reply(1, false), CloseDecision::Close);
    // Answered already: the timeout does nothing.
    assert_eq!(p.reply(1, false), CloseDecision::Ignore);
    // Canceled: keep, and the timeout must not close it later.
    let token = p.begin(2);
    assert_eq!(p.reply(2, true), CloseDecision::Keep);
    assert_eq!(p.expire(2, token), CloseDecision::Ignore);
    // Never answered: the timeout closes it.
    let token = p.begin(3);
    assert_eq!(p.expire(3, token), CloseDecision::Close);
    assert_eq!(p.reply(3, false), CloseDecision::Ignore);
    // A stale timer from an earlier request leaves a newer one pending.
    let old = p.begin(4);
    assert_eq!(p.reply(4, true), CloseDecision::Keep);
    let new = p.begin(4);
    assert_eq!(p.expire(4, old), CloseDecision::Ignore);
    assert_eq!(p.expire(4, new), CloseDecision::Close);
    // close() settles a pending request.
    let token = p.begin(5);
    p.forget(5);
    assert_eq!(p.expire(5, token), CloseDecision::Ignore);
    assert_eq!(p.reply(5, false), CloseDecision::Ignore);
    assert_eq!(
      super::CLOSE_REPLY_TIMEOUT,
      std::time::Duration::from_secs(5)
    );
  }

  #[test]
  fn initial_window_reveal_rules() {
    use super::RevealTrigger::*;
    use super::should_reveal_initial_window as reveal;
    // The default: revealed once, by whichever comes first.
    assert!(reveal(FirstLoad, true, false, false));
    assert!(reveal(Fallback, true, false, false));
    assert!(!reveal(Fallback, true, false, true));
    assert!(!reveal(FirstLoad, true, false, true));
    // A tray-only app (showOnFirstLoad: false): never, not even by the
    // 10 s fallback.
    assert!(!reveal(FirstLoad, false, false, false));
    assert!(!reveal(Fallback, false, false, false));
    // The app hid / showed / closed it before the first load: it owns its
    // visibility.
    assert!(!reveal(FirstLoad, true, true, false));
    assert!(!reveal(Fallback, true, true, false));
  }

  #[test]
  fn dock_reopen_camelcase_payload() {
    let v = serde_json::to_value(DesktopEvent::DockReopen {
      has_visible_windows: true,
    })
    .unwrap();
    assert_eq!(v["kind"], "dockReopen");
    assert_eq!(v["hasVisibleWindows"], true);
    // The snake_case variant must NOT exist — DESKTOP_JS reads the
    // camelCased name.
    assert!(v.get("has_visible_windows").is_none());
  }

  #[test]
  fn runtime_error_omits_stack_when_none() {
    let with_stack = serde_json::to_value(DesktopEvent::RuntimeError {
      message: "boom".to_string(),
      stack: Some("at foo".to_string()),
    })
    .unwrap();
    assert_eq!(with_stack["message"], "boom");
    assert_eq!(with_stack["stack"], "at foo");

    let no_stack = serde_json::to_value(DesktopEvent::RuntimeError {
      message: "boom".to_string(),
      stack: None,
    })
    .unwrap();
    // None should serialize as JSON null (not be omitted), matching
    // what the JS handler currently expects.
    assert_eq!(no_stack["stack"], serde_json::Value::Null);
  }

  #[test]
  fn notification_variants_share_field_name() {
    // All four notification variants must use the same `notificationId`
    // key so the JS handler can route by `kind` alone.
    for ev in [
      DesktopEvent::NotificationShow {
        notification_id: 42,
      },
      DesktopEvent::NotificationClick {
        notification_id: 42,
      },
      DesktopEvent::NotificationClose {
        notification_id: 42,
      },
      DesktopEvent::NotificationError {
        notification_id: 42,
      },
    ] {
      let v = serde_json::to_value(&ev).unwrap();
      assert_eq!(v["notificationId"], 42, "for variant {ev:?}");
    }
  }

  // --- Every remaining DesktopEvent variant gets a kind pin ---

  fn kind_of(ev: DesktopEvent) -> String {
    serde_json::to_value(ev).unwrap()["kind"]
      .as_str()
      .expect("kind must be a string")
      .to_string()
  }

  #[test]
  fn every_variant_has_camelcase_kind() {
    // The kind discriminator is what DESKTOP_JS switches on. A
    // misspelled or accidentally renamed variant would make its events
    // silently no-op in the renderer. Pin every kind name.
    assert_eq!(
      kind_of(DesktopEvent::AppMenuClick {
        window_id: 0,
        id: "".into()
      }),
      "appMenuClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::ContextMenuClick {
        window_id: 0,
        id: "".into()
      }),
      "contextMenuClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::KeyboardEvent {
        window_id: 0,
        r#type: "".into(),
        key: "".into(),
        code: "".into(),
        shift: false,
        control: false,
        alt: false,
        meta: false,
        repeat: false,
      }),
      "keyboardEvent"
    );
    assert_eq!(
      kind_of(DesktopEvent::BindCall {
        window_id: 0,
        name: "".into(),
        args: vec![],
        call_id: 0,
      }),
      "bindCall"
    );
    assert_eq!(
      kind_of(DesktopEvent::MouseClick {
        window_id: 0,
        state: "".into(),
        button: 0,
        client_x: 0.0,
        client_y: 0.0,
        shift: false,
        control: false,
        alt: false,
        meta: false,
        click_count: 0,
      }),
      "mouseClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::MouseMove {
        window_id: 0,
        client_x: 0.0,
        client_y: 0.0,
        shift: false,
        control: false,
        alt: false,
        meta: false,
      }),
      "mouseMove"
    );
    assert_eq!(
      kind_of(DesktopEvent::Wheel {
        window_id: 0,
        delta_x: 0.0,
        delta_y: 0.0,
        delta_mode: 0,
        client_x: 0.0,
        client_y: 0.0,
        shift: false,
        control: false,
        alt: false,
        meta: false,
      }),
      "wheel"
    );
    assert_eq!(
      kind_of(DesktopEvent::CursorEnterLeave {
        window_id: 0,
        entered: false,
        client_x: 0.0,
        client_y: 0.0,
        shift: false,
        control: false,
        alt: false,
        meta: false,
      }),
      "cursorEnterLeave"
    );
    assert_eq!(
      kind_of(DesktopEvent::FocusChanged {
        window_id: 0,
        focused: false
      }),
      "focusChanged"
    );
    assert_eq!(
      kind_of(DesktopEvent::WindowResize {
        window_id: 0,
        width: 0,
        height: 0
      }),
      "windowResize"
    );
    assert_eq!(
      kind_of(DesktopEvent::WindowMove {
        window_id: 0,
        x: 0,
        y: 0
      }),
      "windowMove"
    );
    assert_eq!(kind_of(DesktopEvent::PageLoad { window_id: 0 }), "pageLoad");
    assert_eq!(
      kind_of(DesktopEvent::CloseRequested { window_id: 0 }),
      "closeRequested"
    );
    assert_eq!(
      kind_of(DesktopEvent::RuntimeError {
        message: "".into(),
        stack: None
      }),
      "runtimeError"
    );
    assert_eq!(
      kind_of(DesktopEvent::DockMenuClick { id: "".into() }),
      "dockMenuClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::DockReopen {
        has_visible_windows: false
      }),
      "dockReopen"
    );
    assert_eq!(kind_of(DesktopEvent::TrayClick { tray_id: 0 }), "trayClick");
    assert_eq!(
      kind_of(DesktopEvent::TrayDoubleClick { tray_id: 0 }),
      "trayDoubleClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::TrayMenuClick {
        tray_id: 0,
        id: "".into()
      }),
      "trayMenuClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::NotificationShow { notification_id: 0 }),
      "notificationShow"
    );
    assert_eq!(
      kind_of(DesktopEvent::NotificationClick { notification_id: 0 }),
      "notificationClick"
    );
    assert_eq!(
      kind_of(DesktopEvent::NotificationClose { notification_id: 0 }),
      "notificationClose"
    );
    assert_eq!(
      kind_of(DesktopEvent::NotificationError { notification_id: 0 }),
      "notificationError"
    );
    assert_eq!(kind_of(DesktopEvent::OpenUrl { url: "".into() }), "openUrl");
    assert_eq!(
      kind_of(DesktopEvent::OpenFile { path: "".into() }),
      "openFile"
    );
    assert_eq!(
      kind_of(DesktopEvent::SecondInstance {
        args: vec![],
        cwd: "".into(),
        urls: vec![],
        files: vec![],
      }),
      "secondInstance"
    );
  }

  #[test]
  fn launch_event_payloads() {
    let v = serde_json::to_value(DesktopEvent::SecondInstance {
      args: vec!["acme://x".into(), "--flag".into()],
      cwd: "/home/me".into(),
      urls: vec!["acme://x".into()],
      files: vec![],
    })
    .unwrap();
    assert_eq!(
      v,
      json!({
        "kind": "secondInstance",
        "args": ["acme://x", "--flag"],
        "cwd": "/home/me",
        "urls": ["acme://x"],
        "files": [],
      })
    );
    let v = serde_json::to_value(DesktopEvent::OpenFile {
      path: "/a b".into(),
    })
    .unwrap();
    assert_eq!(v, json!({ "kind": "openFile", "path": "/a b" }));
  }

  fn inbox_with_channel(
    launch_urls: Vec<String>,
    launch_files: Vec<String>,
  ) -> (
    super::DesktopLaunchInbox,
    tokio::sync::mpsc::Receiver<DesktopEvent>,
  ) {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    (
      super::DesktopLaunchInbox::new(tx, launch_urls, launch_files),
      rx,
    )
  }

  fn drain(rx: &mut tokio::sync::mpsc::Receiver<DesktopEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
      out.push(serde_json::to_value(ev).unwrap().to_string());
    }
    out
  }

  #[test]
  fn launch_inbox_buffers_until_subscribed() {
    let (inbox, mut rx) = inbox_with_channel(vec![], vec![]);
    // Nothing listens yet: held, not sent.
    inbox.open_url("acme://cold".into());
    inbox.open_file("/tmp/a.txt".into());
    inbox.second_instance(
      vec!["acme://2".into()],
      "/".into(),
      vec!["acme://2".into()],
      vec![],
    );
    assert!(drain(&mut rx).is_empty());

    // Subscribing to one kind drains only that kind, oldest first.
    inbox.open_url("acme://cold2".into());
    let pending = inbox.subscribe("openurl");
    assert_eq!(
      pending
        .into_iter()
        .map(|e| serde_json::to_value(e).unwrap()["url"].clone())
        .collect::<Vec<_>>(),
      vec![json!("acme://cold"), json!("acme://cold2")]
    );
    // A second subscription gets nothing again.
    assert!(inbox.subscribe("openurl").is_empty());
    // Later URLs go straight into the channel; files still wait.
    inbox.open_url("acme://warm".into());
    inbox.open_file("/tmp/b.txt".into());
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("acme://warm"), "{sent:?}");

    let files = inbox.subscribe("openfile");
    assert_eq!(files.len(), 2);
    let second = inbox.subscribe("secondinstance");
    assert_eq!(second.len(), 1);
    inbox.second_instance(vec![], "/".into(), vec![], vec![]);
    assert_eq!(drain(&mut rx).len(), 1);
    // Unknown types subscribe to nothing.
    assert!(inbox.subscribe("click").is_empty());
  }

  #[test]
  fn launch_snapshot_takes_argv_and_unclaimed_deliveries_once() {
    let (inbox, mut rx) =
      inbox_with_channel(vec!["acme://argv".into()], vec!["/argv/file".into()]);
    inbox.open_url("acme://early".into());
    inbox.open_file("/early/file".into());
    let snapshot = inbox.take_launch_targets();
    assert_eq!(
      snapshot,
      super::LaunchTargetsSnapshot {
        urls: vec!["acme://argv".into(), "acme://early".into()],
        files: vec!["/argv/file".into(), "/early/file".into()],
      }
    );
    // Taken once: the snapshot never repeats, and what it took is not
    // delivered again as an event.
    assert_eq!(
      inbox.take_launch_targets(),
      super::LaunchTargetsSnapshot::default()
    );
    assert!(inbox.subscribe("openurl").is_empty());
    assert!(inbox.subscribe("openfile").is_empty());
    // A URL after the snapshot and before any listener is an event.
    let (inbox, _rx2) = inbox_with_channel(vec![], vec![]);
    let _ = inbox.take_launch_targets();
    inbox.open_url("acme://late".into());
    assert_eq!(inbox.subscribe("openurl").len(), 1);
    assert!(drain(&mut rx).is_empty());
  }

  #[test]
  fn launch_inbox_is_bounded() {
    let (inbox, _rx) = inbox_with_channel(vec![], vec![]);
    for i in 0..(super::MAX_PENDING_LAUNCH_EVENTS + 3) {
      inbox.open_url(format!("acme://{i}"));
    }
    let pending = inbox.subscribe("openurl");
    assert_eq!(pending.len(), super::MAX_PENDING_LAUNCH_EVENTS);
    // The oldest were dropped.
    assert_eq!(
      serde_json::to_value(&pending[0]).unwrap()["url"],
      "acme://3"
    );
  }

  // --- BindCall.args round-trip ---

  #[test]
  fn bind_call_args_passes_through_arbitrary_values() {
    // BindCall carries `Vec<DesktopValue>` as `args`. We must serialize
    // it transparently (not nested under "args.value" or with a Some()
    // wrapper) so the renderer sees exactly what was passed.
    let ev = DesktopEvent::BindCall {
      window_id: 1,
      name: "greet".into(),
      args: vec![DesktopValue::Dict(vec![
        ("name".into(), DesktopValue::String("ada".into())),
        ("n".into(), DesktopValue::Int(42)),
      ])],
      call_id: 7,
    };
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(v["args"][0]["name"], "ada");
    assert_eq!(v["args"][0]["n"], 42);
    assert_eq!(v["callId"], 7);
    assert_eq!(v["windowId"], 1);
  }

  #[test]
  fn desktop_value_binary_serializes_as_bytes() {
    // `Binary` must reach serde as `serialize_bytes` — serde_v8 turns that
    // into a `Uint8Array` on the JS side, which is what lets bindings carry
    // binary data (denoland/deno#36498). serde_cbor-style formats would show
    // this directly; through serde_json, serialize_bytes lands as an array
    // of numbers rather than an error or an objectified map.
    let j =
      serde_json::to_value(DesktopValue::Binary(vec![1, 2, 3, 255])).unwrap();
    assert_eq!(j, json!([1, 2, 3, 255]));
  }

  #[test]
  fn desktop_value_json_roundtrip_and_binary_deserialize() {
    // Non-binary values keep plain JSON semantics in both directions.
    let v = DesktopValue::Dict(vec![
      ("ok".into(), DesktopValue::Bool(true)),
      ("n".into(), DesktopValue::Int(42)),
      ("f".into(), DesktopValue::Double(1.5)),
      (
        "list".into(),
        DesktopValue::List(vec![
          DesktopValue::Null,
          DesktopValue::String("hi".into()),
        ]),
      ),
    ]);
    let j = serde_json::to_value(&v).unwrap();
    assert_eq!(
      j,
      json!({"ok": true, "n": 42, "f": 1.5, "list": [null, "hi"]})
    );
    let back: DesktopValue = serde_json::from_value(j).unwrap();
    assert_eq!(back, v);

    // Integers outside i32 degrade to Double (laufey's Int is i32-wide).
    let big: DesktopValue =
      serde_json::from_value(json!(5_000_000_000_i64)).unwrap();
    assert_eq!(big, DesktopValue::Double(5_000_000_000.0));

    // A deserializer that produces bytes (serde_v8 for Uint8Array /
    // ArrayBuffer views) must map to Binary, not error like
    // serde_json::Value's visitor did (denoland/deno#36498).
    struct Bytes(Vec<u8>);
    impl<'de> serde::Deserializer<'de> for Bytes {
      type Error = serde_json::Error;
      fn deserialize_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
      ) -> Result<V::Value, Self::Error> {
        visitor.visit_byte_buf(self.0)
      }
      serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
      }
    }
    let bin: DesktopValue =
      serde::Deserialize::deserialize(Bytes(vec![9, 8, 7])).unwrap();
    assert_eq!(bin, DesktopValue::Binary(vec![9, 8, 7]));
  }

  #[test]
  fn desktop_value_rejects_nesting_past_the_depth_limit() {
    // The visitor recurses per level, so an unbounded value — a cyclic
    // object returned from a binding handler is the realistic source —
    // would run the runtime thread out of stack. It must surface an error
    // to the caller instead.
    //
    // Built as a `Value` rather than parsed from text: serde_json's own
    // parser has a recursion limit that would fire first and mask the
    // guard under test. `from_value` applies no limit of its own.
    fn nest_lists(depth: usize) -> serde_json::Value {
      let mut v = json!(1);
      for _ in 0..depth {
        v = serde_json::Value::Array(vec![v]);
      }
      v
    }

    let err =
      serde_json::from_value::<DesktopValue>(nest_lists(super::MAX_DEPTH + 1))
        .unwrap_err();
    assert!(
      err.to_string().contains("nested deeper than"),
      "expected the depth guard to reject, got: {err}"
    );

    // Nesting within the limit still deserializes, so the guard isn't
    // simply rejecting anything structured.
    serde_json::from_value::<DesktopValue>(nest_lists(super::MAX_DEPTH))
      .expect("nesting within the limit must still deserialize");
  }

  #[test]
  fn desktop_value_depth_limit_counts_maps_too() {
    // `visit_map` carries its own guard; a value nested through objects
    // rather than arrays must be bounded the same way.
    let mut v = json!(1);
    for _ in 0..super::MAX_DEPTH + 1 {
      let mut obj = serde_json::Map::new();
      obj.insert("a".to_string(), v);
      v = serde_json::Value::Object(obj);
    }
    let err = serde_json::from_value::<DesktopValue>(v).unwrap_err();
    assert!(
      err.to_string().contains("nested deeper than"),
      "expected the depth guard to reject, got: {err}"
    );
  }

  // --- error dialog single-flight ---

  #[test]
  fn error_dialog_flag_is_single_flight_and_panic_safe() {
    use std::sync::atomic::Ordering;

    use super::ERROR_DIALOG_SHOWING;
    use super::ErrorDialogGuard;

    // One test, not two: `ERROR_DIALOG_SHOWING` is a process-global static
    // and cargo runs tests on parallel threads within one binary, so two
    // tests asserting on exact `swap` results would interleave and flake.
    // Anything else added here has to join this test rather than sit
    // alongside it.
    ERROR_DIALOG_SHOWING.store(false, Ordering::SeqCst);

    // First caller takes the slot; everyone arriving while it's held is
    // turned away rather than parking another thread in a modal dialog.
    assert!(!ERROR_DIALOG_SHOWING.swap(true, Ordering::SeqCst));
    assert!(ERROR_DIALOG_SHOWING.swap(true, Ordering::SeqCst));
    assert!(ERROR_DIALOG_SHOWING.swap(true, Ordering::SeqCst));

    // Dropping the guard frees the slot for the next error.
    {
      let _guard = ErrorDialogGuard;
    }
    assert!(
      !ERROR_DIALOG_SHOWING.load(Ordering::SeqCst),
      "the guard must clear the flag on the normal path"
    );

    // And it clears it while unwinding too. A plain `store` after the call
    // would leave the flag stuck at `true` if `DesktopApi::alert` panicked,
    // suppressing every later error dialog for the life of the process.
    ERROR_DIALOG_SHOWING.store(false, Ordering::SeqCst);
    let panicked = std::panic::catch_unwind(|| {
      let _guard = ErrorDialogGuard;
      assert!(!ERROR_DIALOG_SHOWING.swap(true, Ordering::SeqCst));
      panic!("backend blew up mid-dialog");
    });
    assert!(
      panicked.is_err(),
      "the panic must propagate, not be swallowed"
    );
    assert!(
      !ERROR_DIALOG_SHOWING.load(Ordering::SeqCst),
      "the flag must be clear again so later errors can still show a dialog"
    );

    ERROR_DIALOG_SHOWING.store(false, Ordering::SeqCst);
  }

  // --- permission_state_to_web_string ---

  #[test]
  fn permission_state_strings_match_web_api() {
    // These exact strings are surfaced to JS via `Notification.permission`
    // and `navigator.permissions.query(...).state`. The Web Permissions
    // API specifies "granted" / "denied" / "prompt" verbatim; renaming
    // any of them silently breaks feature detection in user code.
    assert_eq!(
      permission_state_to_web_string(PermissionState::Granted),
      "granted"
    );
    assert_eq!(
      permission_state_to_web_string(PermissionState::Denied),
      "denied"
    );
    assert_eq!(
      permission_state_to_web_string(PermissionState::Prompt),
      "prompt"
    );
    // "unsupported" is wef-specific (the spec has no such state); DESKTOP_JS
    // maps it to a TypeError throw from requestPermission.
    assert_eq!(
      permission_state_to_web_string(PermissionState::Unsupported),
      "unsupported"
    );
  }

  // --- dylib_magic_ok ---

  #[test]
  fn dylib_magic_accepts_native_formats() {
    // 32/64-bit Mach-O, both endians.
    assert!(dylib_magic_ok(&[0xFE, 0xED, 0xFA, 0xCE]));
    assert!(dylib_magic_ok(&[0xFE, 0xED, 0xFA, 0xCF]));
    assert!(dylib_magic_ok(&[0xCE, 0xFA, 0xED, 0xFE]));
    assert!(dylib_magic_ok(&[0xCF, 0xFA, 0xED, 0xFE]));
    // Fat Mach-O (universal binary).
    assert!(dylib_magic_ok(&[0xCA, 0xFE, 0xBA, 0xBE]));
    assert!(dylib_magic_ok(&[0xCA, 0xFE, 0xBA, 0xBF]));
    // ELF (Linux).
    assert!(dylib_magic_ok(&[0x7F, b'E', b'L', b'F']));
    // PE/COFF (Windows): starts with "MZ".
    assert!(dylib_magic_ok(b"MZ\x90\x00rest_of_pe_header"));
  }

  #[test]
  fn dylib_magic_rejects_non_binaries() {
    // Plain text — what a malformed bspatch result might decode to.
    assert!(!dylib_magic_ok(b"not a dylib"));
    // Empty / too short.
    assert!(!dylib_magic_ok(b""));
    assert!(!dylib_magic_ok(b"M"));
    assert!(!dylib_magic_ok(b"MZ"));
    assert!(!dylib_magic_ok(b"MZ\x90"));
    // Random gibberish.
    assert!(!dylib_magic_ok(&[0xDE, 0xAD, 0xBE, 0xEF]));
    // The wrapper check is the last line of defence — failure here means
    // we'd write garbage as the staged dylib.
  }

  // --- op_desktop_verify_ed25519 ---
  //
  // The op is the trust anchor for auto-update: the manifest is signed
  // and verified against a baked-in pubkey before any patch hash is
  // trusted. A regression that returns true for invalid input would
  // turn the whole auto-update path into "fetch + apply arbitrary code".

  fn keypair_from_seed(
    seed: &[u8; 32],
  ) -> (ed25519_dalek::SigningKey, ed25519_dalek::VerifyingKey) {
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    let vk = sk.verifying_key();
    (sk, vk)
  }

  fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
  }

  #[test]
  fn verify_ed25519_accepts_real_signature() {
    use ed25519_dalek::Signer;
    let (sk, vk) = keypair_from_seed(&[1u8; 32]);
    let message = b"deno desktop update v1.2.3";
    let sig = sk.sign(message);
    let ok =
      verify_ed25519_b64(&b64(&vk.to_bytes()), &b64(&sig.to_bytes()), message);
    assert!(ok, "signature over message must verify");
  }

  #[test]
  fn verify_ed25519_rejects_tampered_message() {
    use ed25519_dalek::Signer;
    let (sk, vk) = keypair_from_seed(&[1u8; 32]);
    let original = b"deno desktop update v1.2.3";
    let sig = sk.sign(original);
    // Flip a single byte of the message — a correct verifier must reject.
    let tampered = b"deno desktop update v1.2.4";
    let ok =
      verify_ed25519_b64(&b64(&vk.to_bytes()), &b64(&sig.to_bytes()), tampered);
    assert!(!ok, "tampered message must fail verification");
  }

  #[test]
  fn verify_ed25519_rejects_wrong_key() {
    use ed25519_dalek::Signer;
    let (sk_a, _) = keypair_from_seed(&[1u8; 32]);
    let (_, vk_b) = keypair_from_seed(&[2u8; 32]);
    let message = b"hi";
    let sig = sk_a.sign(message);
    let ok = verify_ed25519_b64(
      &b64(&vk_b.to_bytes()),
      &b64(&sig.to_bytes()),
      message,
    );
    assert!(!ok, "signature from key A must NOT verify under key B");
  }

  #[test]
  fn verify_ed25519_rejects_malformed_inputs() {
    let message = b"hi";
    // Empty key.
    assert!(!verify_ed25519_b64("", &b64(&[0u8; 64]), message));
    // Empty sig.
    assert!(!verify_ed25519_b64(&b64(&[0u8; 32]), "", message));
    // Wrong-length key.
    assert!(!verify_ed25519_b64(
      &b64(&[0u8; 31]),
      &b64(&[0u8; 64]),
      message
    ));
    assert!(!verify_ed25519_b64(
      &b64(&[0u8; 33]),
      &b64(&[0u8; 64]),
      message
    ));
    // Wrong-length sig.
    assert!(!verify_ed25519_b64(
      &b64(&[0u8; 32]),
      &b64(&[0u8; 63]),
      message
    ));
    assert!(!verify_ed25519_b64(
      &b64(&[0u8; 32]),
      &b64(&[0u8; 65]),
      message
    ));
    // Invalid base64.
    assert!(!verify_ed25519_b64(
      "!!! not base64 !!!",
      &b64(&[0u8; 64]),
      message
    ));
    assert!(!verify_ed25519_b64(
      &b64(&[0u8; 32]),
      "@@@ not base64 @@@",
      message
    ));
  }

  // --- register_bind_call + PendingBindResponses ---

  // The op_desktop_resolve_bind_call / op_desktop_reject_bind_call ops
  // both reduce to map.remove(&call_id).map(|tx| tx.send(...)). We
  // exercise the underlying state machine directly here — the ops
  // themselves are #[op2(fast)] wrappers and aren't callable from a
  // unit test, but the bug surface is the map manipulation, not the
  // tiny op2 wrapper.

  fn resolve(responses: &PendingBindResponses, id: u32, v: DesktopValue) {
    if let Some(tx) = responses.0.lock().unwrap().remove(&id) {
      let _ = tx.send(Ok(v));
    }
  }

  fn reject(responses: &PendingBindResponses, id: u32, e: String) {
    if let Some(tx) = responses.0.lock().unwrap().remove(&id) {
      let _ = tx.send(Err(e));
    }
  }

  #[tokio::test]
  async fn bind_call_resolve_round_trips_value() {
    let responses = PendingBindResponses::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let id = register_bind_call(&responses, tx);
    // The runtime resolves with a DesktopValue.
    resolve(
      &responses,
      id,
      DesktopValue::Dict(vec![
        ("ok".into(), DesktopValue::Bool(true)),
        ("n".into(), DesktopValue::Int(42)),
      ]),
    );
    let v = rx.await.expect("oneshot recv").expect("Ok variant");
    assert_eq!(
      v,
      DesktopValue::Dict(vec![
        ("ok".into(), DesktopValue::Bool(true)),
        ("n".into(), DesktopValue::Int(42)),
      ])
    );
    // After resolve, the map entry is gone.
    assert!(
      responses.0.lock().unwrap().is_empty(),
      "responses map must be drained after resolve"
    );
  }

  #[tokio::test]
  async fn bind_call_reject_delivers_error_string() {
    let responses = PendingBindResponses::new();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let id = register_bind_call(&responses, tx);
    reject(&responses, id, "binding threw".to_string());
    let e = rx.await.unwrap().expect_err("must be Err");
    assert_eq!(e, "binding threw");
    assert!(responses.0.lock().unwrap().is_empty());
  }

  #[test]
  fn bind_call_ids_are_unique_across_concurrent_registers() {
    // The id counter is a single AtomicU32 shared across calls.
    // Registering many at once must produce distinct ids — duplicates
    // would silently route a renderer response to the wrong pending
    // call.
    let responses = PendingBindResponses::new();
    let ids: Vec<u32> = (0..50)
      .map(|_| {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        register_bind_call(&responses, tx)
      })
      .collect();
    let mut seen: std::collections::HashSet<u32> =
      std::collections::HashSet::new();
    for id in &ids {
      assert!(seen.insert(*id), "duplicate bind call id: {id}");
    }
    // All 50 are registered in the map.
    assert_eq!(responses.0.lock().unwrap().len(), 50);
  }

  #[test]
  fn bind_call_unknown_id_resolve_is_noop() {
    let responses = PendingBindResponses::new();
    // No entry registered — resolve with a random id must not panic
    // and must not affect any state.
    resolve(&responses, 999_999, DesktopValue::Null);
    reject(&responses, 999_999, "x".to_string());
    assert!(responses.0.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn bind_call_dropped_receiver_doesnt_panic_resolve() {
    // The renderer may give up on a bind call before the Deno side
    // resolves it (window closed). The resolve path uses `let _ = tx.send(...)`
    // explicitly because the receiver might be gone; we pin that
    // behaviour here so a future refactor doesn't reintroduce a
    // .unwrap() that would crash the runtime.
    let responses = PendingBindResponses::new();
    let (tx, rx) =
      tokio::sync::oneshot::channel::<Result<DesktopValue, String>>();
    let id = register_bind_call(&responses, tx);
    drop(rx);
    resolve(&responses, id, DesktopValue::Null);
    // If we reach this line without panicking, the test passes.
  }

  #[test]
  fn verify_ed25519_trims_whitespace_on_b64_inputs() {
    use ed25519_dalek::Signer;
    let (sk, vk) = keypair_from_seed(&[1u8; 32]);
    let message = b"trim me";
    let sig = sk.sign(message);
    let pk = format!("  {}\n", b64(&vk.to_bytes()));
    let sg = format!("\t{}\n", b64(&sig.to_bytes()));
    // The op trims the base64 before decoding so manifest JSON with
    // pretty-printed whitespace (or trailing newlines from `\n` literals)
    // still verifies.
    assert!(verify_ed25519_b64(&pk, &sg, message));
  }

  // The JS bridge hands these to @clerk/electron unchanged: the capability
  // keys and the not_supported envelope must keep the
  // @clerk/electron-passkeys shapes.
  #[test]
  fn passkey_capabilities_serialize_like_clerk() {
    let v = serde_json::to_value(PasskeyCapabilitiesInfo {
      platform_authenticator: true,
      security_keys: false,
    })
    .unwrap();
    assert_eq!(
      v,
      json!({ "platformAuthenticator": true, "securityKeys": false })
    );
  }

  #[test]
  fn passkey_not_supported_envelope_is_clerk_shaped() {
    let v: serde_json::Value =
      serde_json::from_str(PASSKEY_NOT_SUPPORTED_ENVELOPE).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["error"]["code"], json!("not_supported"));
    assert!(v["error"]["message"].is_string());
  }
}
