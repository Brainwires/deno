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
use deno_error::JsErrorBox;

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
      // Own data properties (`CreateDataProperty`), never `[[Set]]`: the
      // value comes from the page (binding arguments, `executeJs` results),
      // and a `__proto__` key assigned with `[[Set]]` replaced the object's
      // prototype, while a setter the app (or a dependency) put on
      // `Object.prototype` / `Array.prototype` would have run for every key.
      DesktopValue::List(l) => {
        let arr = v8::Array::new(scope, l.len() as i32);
        for (i, v) in l.into_iter().enumerate() {
          let val = v.to_v8(scope)?;
          let index: v8::Local<v8::Name> =
            v8::Integer::new_from_unsigned(scope, i as u32)
              .to_string(scope)
              .unwrap()
              .into();
          arr.create_data_property(scope, index, val);
        }
        arr.into()
      }
      DesktopValue::Dict(d) => {
        let obj = v8::Object::new(scope);
        for (k, v) in d {
          let Some(key) = v8::String::new(scope, &k) else {
            continue;
          };
          let val = v.to_v8(scope)?;
          obj.create_data_property(scope, key.into(), val);
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
  /// The context menu `showContextMenu` opened closed, after its click if
  /// one was chosen (laufey API 41).
  #[serde(rename_all = "camelCase")]
  ContextMenuClose { window_id: u32 },
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
    /// The serialized origin of the document that called (laufey API 44),
    /// already admitted by [`bind_call_allowed`].
    origin: String,
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
  /// The window is closed for good (destroyed, or hidden and kept for a
  /// WebGPU surface): DESKTOP_JS forgets its per-window state (the window
  /// registry, bound functions, pressed buttons), which it kept for the life
  /// of the process.
  #[serde(rename_all = "camelCase")]
  WindowClosed { window_id: u32 },
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
  /// What `Deno.desktop.platformFeatures()` reports may have changed
  /// (laufey API 45: on Linux a tray host appeared or went away):
  /// `Deno.desktop` "platformfeatureschanged".
  PlatformFeaturesChanged,
  /// What `Deno.desktop.titleBarPreferences()` reports changed (laufey API
  /// 47: the user moved the window buttons, or changed the double-click
  /// action, colour scheme or accent colour): `Deno.desktop`
  /// "titlebarpreferenceschanged".
  TitleBarPreferencesChanged,
  /// Files dragged over / dropped on a window (laufey API 39). `phase` is
  /// `"enter"`, `"over"`, `"leave"` or `"drop"`; `paths` is `None` for
  /// `"leave"` and, on backends that reveal the paths only on the drop, for
  /// `"enter"` / `"over"`; `count` is the number of files.
  #[serde(rename_all = "camelCase")]
  FileDrop {
    window_id: u32,
    phase: String,
    x: f64,
    y: f64,
    paths: Option<Vec<String>>,
    count: usize,
  },
  /// The system clipboard changed (`Deno.desktop.clipboard` "change"); only
  /// while the app listens.
  ClipboardChange,
  /// A registered global shortcut was pressed (laufey API 40;
  /// `Deno.desktop.shortcuts` "shortcut"). `accelerator` is the canonical
  /// form the registration resolved with.
  #[serde(rename_all = "camelCase")]
  Shortcut { accelerator: String },
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
  /// The body of a live notification was clicked. An action button is
  /// [`DesktopEvent::NotificationAction`] (laufey API 41; before, actions
  /// were folded into this event).
  #[serde(rename_all = "camelCase")]
  NotificationClick { notification_id: u32 },
  /// An action button of a live notification was clicked (laufey API 41).
  #[serde(rename_all = "camelCase")]
  NotificationAction {
    notification_id: u32,
    action: String,
  },
  /// A click on a notification that no live `Notification` object owns:
  /// one an earlier run posted, a scheduled one, or the click that launched
  /// the app (`launch`; laufey API 41). `data` is the notification's data
  /// as JSON text. Emitted through [`DesktopLaunchInbox`].
  #[serde(rename_all = "camelCase")]
  NotificationResponse {
    tag: String,
    action: Option<String>,
    data: Option<String>,
    launch: bool,
  },
  #[serde(rename_all = "camelCase")]
  NotificationClose { notification_id: u32 },
  #[serde(rename_all = "camelCase")]
  NotificationError { notification_id: u32 },
}

/// How many events may wait in the desktop event queue before the ones that
/// can be dropped are: pointer motion and wheel events (and bound-function
/// calls, which are refused with "event channel saturated"). A misbehaving
/// renderer could otherwise flood motion fast enough to OOM the runtime.
const DESKTOP_EVENT_CHANNEL_CAPACITY: usize = 1024;

/// The queue between the backend's threads and the runtime's event loop
/// (`op_desktop_recv_event`).
///
/// It used to be one bounded channel every event went through with
/// `try_send`, so once a burst of motion filled it, *any* event was lost:
/// a `contextMenuClose` dropped that way left `showContextMenu()` pending
/// forever and the next menu refused as busy, a lost `closeRequested` or
/// `pageLoad` desynced the window state the same way. Now:
/// - consecutive pointer-motion, wheel, resize and move events of one window
///   are coalesced into the latest (wheel deltas add up), so a flood of them
///   takes one slot;
/// - only motion and wheel events are dropped when the queue is full, and
///   bound-function calls are refused (the page's call rejects);
/// - every other event is always delivered, in order.
pub struct DesktopEventQueue {
  state: std::sync::Mutex<EventQueueState>,
  notify: tokio::sync::Notify,
}

#[derive(Default)]
struct EventQueueState {
  events: std::collections::VecDeque<DesktopEvent>,
  /// The receiving runtime is gone.
  closed: bool,
}

/// How the queue treats an event when it is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventDelivery {
  /// Dropped (pointer motion, wheel).
  Lossy,
  /// Refused, so the sender can answer (a bound-function call).
  Refusable,
  /// Always queued.
  Always,
}

impl DesktopEvent {
  fn delivery(&self) -> EventDelivery {
    match self {
      DesktopEvent::MouseMove { .. } | DesktopEvent::Wheel { .. } => {
        EventDelivery::Lossy
      }
      DesktopEvent::BindCall { .. } => EventDelivery::Refusable,
      _ => EventDelivery::Always,
    }
  }

  /// Fold `next` into `self` when it is the same kind of continuous event
  /// for the same window; gives `next` back otherwise.
  fn coalesce(&mut self, next: DesktopEvent) -> Option<DesktopEvent> {
    use DesktopEvent::*;
    let same = match (&*self, &next) {
      (MouseMove { window_id: a, .. }, MouseMove { window_id: b, .. })
      | (
        WindowResize { window_id: a, .. },
        WindowResize { window_id: b, .. },
      )
      | (WindowMove { window_id: a, .. }, WindowMove { window_id: b, .. }) => {
        a == b
      }
      (
        Wheel {
          window_id: a,
          delta_mode: am,
          ..
        },
        Wheel {
          window_id: b,
          delta_mode: bm,
          ..
        },
      ) => a == b && am == bm,
      _ => false,
    };
    if !same {
      return Some(next);
    }
    let merged = match (&*self, next) {
      (
        Wheel {
          delta_x: ax,
          delta_y: ay,
          ..
        },
        Wheel {
          window_id,
          delta_x,
          delta_y,
          delta_mode,
          client_x,
          client_y,
          shift,
          control,
          alt,
          meta,
        },
      ) => Wheel {
        window_id,
        delta_x: ax + delta_x,
        delta_y: ay + delta_y,
        delta_mode,
        client_x,
        client_y,
        shift,
        control,
        alt,
        meta,
      },
      (_, next) => next,
    };
    *self = merged;
    None
  }
}

impl DesktopEventQueue {
  fn lock(&self) -> std::sync::MutexGuard<'_, EventQueueState> {
    self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn push(
    &self,
    event: DesktopEvent,
  ) -> Result<(), tokio::sync::mpsc::error::TrySendError<DesktopEvent>> {
    use tokio::sync::mpsc::error::TrySendError;
    let mut state = self.lock();
    if state.closed {
      return Err(TrySendError::Closed(event));
    }
    let event = match state.events.back_mut() {
      Some(last) => match last.coalesce(event) {
        None => return Ok(()),
        Some(event) => event,
      },
      None => event,
    };
    if state.events.len() >= DESKTOP_EVENT_CHANNEL_CAPACITY
      && event.delivery() != EventDelivery::Always
    {
      return Err(TrySendError::Full(event));
    }
    state.events.push_back(event);
    drop(state);
    self.notify.notify_one();
    Ok(())
  }

  /// The next event, waiting for one; `None` once closed and drained.
  pub async fn recv(&self) -> Option<DesktopEvent> {
    loop {
      let notified = self.notify.notified();
      {
        let mut state = self.lock();
        if let Some(event) = state.events.pop_front() {
          return Some(event);
        }
        if state.closed {
          return None;
        }
      }
      notified.await;
    }
  }

  /// The next event if one is queued.
  pub fn try_recv(&self) -> Option<DesktopEvent> {
    self.lock().events.pop_front()
  }

  fn close(&self) {
    self.lock().closed = true;
    self.notify.notify_one();
  }
}

/// The sending side of the [`DesktopEventQueue`] (the backend's threads).
#[derive(Clone)]
pub struct DesktopEventTx(Arc<DesktopEventQueue>);

impl DesktopEventTx {
  /// Queue an event without blocking; see [`DesktopEventQueue`] for what is
  /// coalesced, dropped or refused.
  pub fn try_send(
    &self,
    event: DesktopEvent,
  ) -> Result<(), tokio::sync::mpsc::error::TrySendError<DesktopEvent>> {
    self.0.push(event)
  }

  /// A handle that does not keep the queue alive: for callbacks the backend
  /// may hold for the rest of the process (a binding's handler).
  pub fn downgrade(&self) -> WeakDesktopEventTx {
    WeakDesktopEventTx(Arc::downgrade(&self.0))
  }
}

/// See [`DesktopEventTx::downgrade`].
#[derive(Clone)]
pub struct WeakDesktopEventTx(std::sync::Weak<DesktopEventQueue>);

impl WeakDesktopEventTx {
  pub fn upgrade(&self) -> Option<DesktopEventTx> {
    self.0.upgrade().map(DesktopEventTx)
  }
}

/// The receiving side (the runtime). Dropping it closes the queue.
pub struct DesktopEventReceiver(pub Arc<DesktopEventQueue>);

impl Drop for DesktopEventReceiver {
  fn drop(&mut self) {
    self.0.close();
  }
}

#[derive(Clone)]
pub struct DesktopEventSender(pub DesktopEventTx);

impl DesktopEventSender {
  /// Send an event without blocking, logging one the queue drops or refuses.
  pub fn try_send(&self, event: DesktopEvent) {
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
      self.0.try_send(event)
    {
      log::warn!(
        "desktop event queue full; dropping a pointer event (renderer producing events faster than runtime can drain)"
      );
    }
  }
}

pub fn create_desktop_event_channel()
-> (DesktopEventSender, DesktopEventReceiver) {
  let queue = Arc::new(DesktopEventQueue {
    state: std::sync::Mutex::new(EventQueueState::default()),
    notify: tokio::sync::Notify::new(),
  });
  (
    DesktopEventSender(DesktopEventTx(queue.clone())),
    DesktopEventReceiver(queue),
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
  NotificationResponse,
}

impl LaunchEventKind {
  fn from_event_type(event_type: &str) -> Option<Self> {
    match event_type {
      "openurl" => Some(Self::OpenUrl),
      "openfile" => Some(Self::OpenFile),
      "secondinstance" => Some(Self::SecondInstance),
      "notificationresponse" => Some(Self::NotificationResponse),
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
  pending_notification_responses: std::collections::VecDeque<DesktopEvent>,
  subscribed_urls: bool,
  subscribed_files: bool,
  subscribed_second_instances: bool,
  subscribed_notification_responses: bool,
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
  /// Notification responses that arrived before the app listened: the
  /// click that launched it.
  pub notifications: Vec<NotificationResponseInfo>,
}

/// A [`DesktopEvent::NotificationResponse`] in the launch snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationResponseInfo {
  pub tag: String,
  pub action: Option<String>,
  pub data: Option<String>,
  pub launch: bool,
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

  /// A click on a notification no live `Notification` owns (laufey API
  /// 41); see [`DesktopEvent::NotificationResponse`]. One with a field over
  /// the limits a notification is posted with (tag 256 bytes, data 4 KiB;
  /// action id 1 KiB, laufey's cap) is dropped: laufey already drops clicks
  /// it never posted (its arguments carry a per-install MAC), so this is a
  /// second line, and the data stays untrusted input either way.
  pub fn notification_response(
    &self,
    tag: String,
    action: Option<String>,
    data: Option<String>,
    launch: bool,
  ) {
    if tag.len() > MAX_NOTIFICATION_TAG_BYTES
      || data
        .as_ref()
        .is_some_and(|d| d.len() > MAX_NOTIFICATION_DATA_BYTES)
      || action
        .as_ref()
        .is_some_and(|a| a.len() > MAX_NOTIFICATION_ACTION_BYTES)
    {
      log::debug!(
        "desktop: dropped a notification response over the size limits"
      );
      return;
    }
    let event = DesktopEvent::NotificationResponse {
      tag,
      action,
      data,
      launch,
    };
    let mut state = self.lock();
    if state.subscribed_notification_responses {
      send_launch_event(&state, event);
    } else {
      push_bounded(
        &mut state.pending_notification_responses,
        event,
        "notificationresponse",
      );
    }
  }

  /// The launch snapshot: the process's own deep links and files, plus the
  /// URLs, files and notification responses delivered so far that no
  /// listener has taken. Empty after the first call.
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
    let notifications = state
      .pending_notification_responses
      .drain(..)
      .filter_map(|event| match event {
        DesktopEvent::NotificationResponse {
          tag,
          action,
          data,
          launch,
        } => Some(NotificationResponseInfo {
          tag,
          action,
          data,
          launch,
        }),
        _ => None,
      })
      .collect();
    LaunchTargetsSnapshot {
      urls,
      files,
      notifications,
    }
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
      LaunchEventKind::NotificationResponse => {
        state.subscribed_notification_responses = true;
        state.pending_notification_responses.drain(..).collect()
      }
    }
  }
}

/// The longest action id a notification response may carry (laufey's cap on
/// a click's action id); the tag and data limits are
/// MAX_NOTIFICATION_TAG_BYTES / MAX_NOTIFICATION_DATA_BYTES.
const MAX_NOTIFICATION_ACTION_BYTES: usize = 1024;

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

/// What `Deno.desktop.authSession.capabilities()` resolves with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionCapabilitiesInfo {
  /// An OS auth session exists (macOS 10.15+).
  pub supported: bool,
  /// `ephemeral: true` is honored.
  pub ephemeral: bool,
  /// An https callback URL works (macOS 14.4+, with an associated domain).
  pub https_callback: bool,
}

/// How a `Deno.desktop.authSession.start()` ended: the callback URL, or an
/// error `code` (`cancelled`, `not_supported`, `invalid`, `busy`, `failed`)
/// and `message`, which the JS side turns into a rejection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionOutcome {
  pub ok: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub url: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub code: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub message: Option<String>,
}

impl AuthSessionOutcome {
  pub fn success(url: String) -> Self {
    Self {
      ok: true,
      url: Some(url),
      code: None,
      message: None,
    }
  }

  pub fn error(code: &str, message: impl Into<String>) -> Self {
    Self {
      ok: false,
      url: None,
      code: Some(code.to_string()),
      message: Some(message.into()),
    }
  }
}

/// The message a runtime without an OS auth session answers with.
pub const AUTH_SESSION_NOT_SUPPORTED_MESSAGE: &str = "this platform has no OS auth session; open the system browser and receive the redirect through a loopback or custom-scheme listener (RFC 8252)";

/// OS auth sessions (`ASWebAuthenticationSession` on macOS), behind
/// `Deno.desktop.authSession`. Implemented by the desktop runtime
/// (denort_desktop, over laufey), which puts an `Arc<dyn DesktopAuthSession>`
/// in the op state.
pub trait DesktopAuthSession: Send + Sync + 'static {
  fn capabilities(&self) -> AuthSessionCapabilitiesInfo;
  /// Start a session at `url` ending at `callback` (a custom scheme or an
  /// https URL), anchored to `window_id` (0: the key window). The session
  /// starts when this is called; the future never fails.
  fn start(
    &self,
    window_id: u32,
    url: String,
    callback: String,
    ephemeral: bool,
  ) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = AuthSessionOutcome> + Send>,
  >;
  /// End the running session as `cancelled` (the app gave up on it): its
  /// sheet closes and its `start` future resolves with the `cancelled`
  /// outcome, exactly once. Returns false, and does nothing, when no session
  /// is running (always so where sessions are not supported).
  fn cancel(&self) -> bool;
}

/// Running native code on the app's UI thread, behind
/// `Deno.desktop.runOnMainThread`. Implemented by the desktop runtime (over
/// laufey's `dispatch_ui_task`), which puts an `Arc<dyn DesktopMainThread>`
/// in the op state.
pub trait DesktopMainThread: Send + Sync + 'static {
  /// Call `function(context)`, a C function `void* (*)(void*)`, on the UI
  /// thread. Resolves with its pointer-sized return value, or with an error
  /// message when the UI thread is gone (the app is quitting) and the
  /// function was not called.
  ///
  /// # Safety
  ///
  /// `function` must be the address of a function with that signature that
  /// is safe to call on the UI thread with `context`.
  unsafe fn call(
    &self,
    function: usize,
    context: usize,
  ) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<usize, String>> + Send>,
  >;
}

/// Which documents may call a binding, beyond the app's own (see
/// [`bind_call_allowed`]): `BrowserWindow.bind(name, fn, { origins })`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BindOrigins {
  /// Only the app's own origins (the default).
  #[default]
  App,
  /// Any document, `null` (opaque) origins included (`origins: "*"`).
  Any,
  /// The app's origins and these serialized origins (`origins: [...]`).
  List(Vec<String>),
}

/// The most origins one binding may list.
const MAX_BIND_ORIGINS: usize = 64;

impl BindOrigins {
  /// `options.origins` as DESKTOP_JS's `bind()` hands it to the (fast)
  /// native method: `""` (not given), `"*"`, or the listed origins, one per
  /// line (an origin has no line break).
  fn from_spec(spec: &str) -> Result<Self, deno_error::JsErrorBox> {
    match spec {
      "" => Ok(BindOrigins::App),
      "*" => Ok(BindOrigins::Any),
      list => {
        let list: Vec<&str> = list.split('\n').collect();
        if list.len() > MAX_BIND_ORIGINS {
          return Err(deno_error::JsErrorBox::type_error(format!(
            "a binding lists at most {MAX_BIND_ORIGINS} origins"
          )));
        }
        let mut out = Vec::with_capacity(list.len());
        for origin in list {
          out.push(serialize_bind_origin(origin).ok_or_else(|| {
            deno_error::JsErrorBox::type_error(format!(
              "not an origin (scheme://host[:port], or \"null\"): {origin:?}"
            ))
          })?);
        }
        Ok(BindOrigins::List(out))
      }
    }
  }
}

/// `origin` as browsers serialize it (lowercase scheme and host, the port
/// only when it isn't the scheme's default): what laufey reports for the
/// calling document, so the two compare exactly. `"null"` stays `"null"`.
/// `None` for anything with a path, query, credentials, or not a URL.
pub fn serialize_bind_origin(origin: &str) -> Option<String> {
  if origin == "null" {
    return Some(origin.to_string());
  }
  if origin.contains('\0') || origin.ends_with('/') {
    return None;
  }
  let url = deno_core::url::Url::parse(origin).ok()?;
  if url.path() != "/" && !url.path().is_empty()
    || url.query().is_some()
    || url.fragment().is_some()
    || !url.username().is_empty()
    || url.password().is_some()
  {
    return None;
  }
  let host = url.host_str()?;
  Some(match url.port() {
    Some(port) => format!("{}://{host}:{port}", url.scheme()),
    None => format!("{}://{host}", url.scheme()),
  })
}

/// Whether a document at `origin` (laufey's serialization of the calling
/// document's origin) may call a binding: one of the app's own `trusted`
/// origins (the app origin, and a development run's dev server), or one the
/// binding opted into. A page the app navigated to elsewhere (a remote site,
/// an identity provider) kept every binding before. An empty origin (a
/// backend that reports none) is never trusted.
pub fn bind_call_allowed(
  origin: &str,
  trusted: &[String],
  extra: &BindOrigins,
) -> bool {
  if origin.is_empty() {
    return matches!(extra, BindOrigins::Any);
  }
  if trusted.iter().any(|t| t == origin) {
    return true;
  }
  match extra {
    BindOrigins::App => false,
    BindOrigins::Any => true,
    BindOrigins::List(list) => list.iter().any(|o| o == origin),
  }
}

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
  /// The `dragenter` / `dragover` / `dragleave` / `drop` window events fire
  /// (laufey API 39).
  pub file_drop: bool,
  /// `dragenter` / `dragover` already carry the paths (WebView2 reveals them
  /// only on the drop).
  pub file_drop_enter_paths: bool,
  /// `BrowserWindow.startDrag` works.
  pub file_drag_out: bool,
  /// `Deno.desktop.dialog` works.
  pub file_dialogs: bool,
  /// One open dialog can pick files and directories (macOS).
  pub file_dialog_files_and_directories: bool,
  /// A dialog given a window is modal to it.
  pub file_dialog_modal: bool,
}

/// `Deno.desktop.clipboard.capabilities()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardCapabilitiesInfo {
  pub text: bool,
  pub html: bool,
  pub image: bool,
  pub formats: bool,
  /// The clipboard's `"change"` event fires.
  pub change_events: bool,
}

/// `Deno.desktop.shortcuts.capabilities()` and friends (laufey API 40).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemCapabilitiesInfo {
  /// `Deno.desktop.shortcuts.register` can bind system-wide shortcuts.
  pub global_shortcuts: bool,
  /// The user approves each shortcut and may pick another trigger (the XDG
  /// GlobalShortcuts portal on Wayland).
  pub shortcuts_user_binds: bool,
  /// `Deno.desktop.launchAtLogin` works.
  pub launch_at_login: bool,
  /// The DevTools controls work (a web engine is present).
  pub devtools: bool,
}

/// What `op_desktop_register_shortcut` resolves with: `status` is `"ok"`,
/// `"invalid"`, `"conflict"`, `"already_registered"`, `"not_supported"`,
/// `"denied"` or `"failed"`; `accelerator` is the canonical form for `"ok"`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShortcutRegisterInfo {
  pub status: &'static str,
  pub accelerator: Option<String>,
}

impl ShortcutRegisterInfo {
  pub fn ok(accelerator: String) -> Self {
    Self {
      status: "ok",
      accelerator: Some(accelerator),
    }
  }
  pub fn err(status: &'static str) -> Self {
    Self {
      status,
      accelerator: None,
    }
  }
}

/// Launch-at-login states (`Deno.desktop.launchAtLogin.get()`):
/// `"enabled"`, `"disabled"`, `"requires-approval"`, `"not-supported"`.
pub const LOGIN_ITEM_STATES: [&str; 4] =
  ["enabled", "disabled", "requires-approval", "not-supported"];

/// `Deno.desktop.menuCapabilities()` (laufey API 41).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuCapabilitiesInfo {
  /// `setApplicationMenu` shows a menu (bar).
  pub app_menu: bool,
  /// App-menu items' accelerators fire them from the keyboard.
  pub accelerators: bool,
  pub context_menu: bool,
  /// `showContextMenu` resolves (and "contextmenuclose" fires) when the
  /// menu closes.
  pub context_closed: bool,
  pub icons: bool,
  pub tooltips: bool,
}

/// `Deno.desktop.notifications.capabilities()` (laufey API 41).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationCapabilitiesInfo {
  pub show: bool,
  /// `schedule()` delivers at the time, at least while the app runs.
  pub schedule: bool,
  /// The OS delivers a scheduled notification while the app isn't running.
  pub schedule_persists: bool,
  pub actions: bool,
  pub clicks: bool,
  /// A click while the app isn't running launches it and is delivered.
  pub cold_start: bool,
}

/// An action button (`{ action, title }`, the Web Notifications shape).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationActionInfo {
  pub action: String,
  pub title: String,
}

/// What a notification shows (`new Notification()` and
/// `Deno.desktop.notifications.schedule()`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NotificationRequest {
  pub title: String,
  pub body: Option<String>,
  pub icon: Option<Vec<u8>>,
  pub tag: Option<String>,
  pub silent: Option<bool>,
  pub require_interaction: Option<bool>,
  pub actions: Vec<NotificationActionInfo>,
  /// The notification's `data` as JSON text, handed back with clicks.
  pub data: Option<String>,
  /// Unix time in milliseconds to deliver at (scheduled).
  pub schedule_at_ms: Option<i64>,
}

/// `Deno.desktop.notifications.schedule(options)` as JS sends it.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationScheduleOptions {
  pub title: String,
  pub body: Option<String>,
  pub tag: String,
  pub at: f64,
  #[serde(default)]
  pub actions: Vec<NotificationActionInfo>,
  pub data: Option<String>,
  pub silent: Option<bool>,
  pub require_interaction: Option<bool>,
}

/// A pending scheduled notification
/// (`Deno.desktop.notifications.getScheduled()`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledNotificationInfo {
  pub tag: String,
  pub title: String,
  pub body: String,
  /// Unix time in milliseconds.
  pub at: i64,
  pub data: Option<String>,
  pub actions: Vec<NotificationActionInfo>,
}

/// Longest tag / data laufey accepts (`LAUFEY_NOTIFICATION_MAX_*`).
pub const MAX_NOTIFICATION_TAG_BYTES: usize = 256;
pub const MAX_NOTIFICATION_DATA_BYTES: usize = 4096;

/// A boxed future the desktop runtime resolves later (a drag out, a dialog).
pub type DesktopFuture<T> =
  std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

/// How `BrowserWindow.startDrag` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragOutcome {
  /// A target took the files.
  Dropped,
  /// The user cancelled, or dropped where nothing took them.
  Cancelled,
  /// The drag never started.
  Failed,
}

impl DragOutcome {
  pub fn as_str(self) -> &'static str {
    match self {
      DragOutcome::Dropped => "dropped",
      DragOutcome::Cancelled => "cancelled",
      DragOutcome::Failed => "failed",
    }
  }
}

/// One filter of a file dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFilterInfo {
  pub name: String,
  pub extensions: Vec<String>,
}

/// A file dialog request from `Deno.desktop.dialog` (validated by the JS
/// side; the backend checks it again).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileDialogRequest {
  /// A save dialog (otherwise open).
  pub save: bool,
  /// The window the dialog is modal to; 0 for an app-level dialog.
  pub window_id: u32,
  pub title: Option<String>,
  pub default_path: Option<String>,
  pub button_label: Option<String>,
  pub filters: Vec<FileFilterInfo>,
  /// Open: pick files.
  pub files: bool,
  /// Open: pick directories.
  pub directories: bool,
  /// Open: allow several.
  pub multiple: bool,
  pub show_hidden: bool,
}

/// How a file dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDialogOutcome {
  Accepted(Vec<String>),
  Cancelled,
  /// Another file dialog was open.
  Busy,
  Failed,
}

/// What `op_desktop_file_dialog_wait` resolves with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileDialogResultInfo {
  /// `"accepted"`, `"cancelled"`, `"busy"` or `"failed"`.
  pub status: &'static str,
  pub paths: Vec<String>,
}

impl From<FileDialogOutcome> for FileDialogResultInfo {
  fn from(o: FileDialogOutcome) -> Self {
    match o {
      FileDialogOutcome::Accepted(paths) => FileDialogResultInfo {
        status: if paths.is_empty() {
          "cancelled"
        } else {
          "accepted"
        },
        paths,
      },
      FileDialogOutcome::Cancelled => FileDialogResultInfo {
        status: "cancelled",
        paths: Vec::new(),
      },
      FileDialogOutcome::Busy => FileDialogResultInfo {
        status: "busy",
        paths: Vec::new(),
      },
      FileDialogOutcome::Failed => FileDialogResultInfo {
        status: "failed",
        paths: Vec::new(),
      },
    }
  }
}

/// The open file dialogs of this runtime, by the id the JS side holds: the
/// backend's dialog id (for cancel) and the outcome, until awaited.
#[derive(Default)]
pub struct FileDialogTable {
  next: u32,
  entries: HashMap<u32, (u32, Option<DesktopFuture<FileDialogOutcome>>)>,
}

impl FileDialogTable {
  pub fn insert(
    &mut self,
    dialog_id: u32,
    outcome: DesktopFuture<FileDialogOutcome>,
  ) -> u32 {
    self.next = self.next.wrapping_add(1).max(1);
    while self.entries.contains_key(&self.next) {
      self.next = self.next.wrapping_add(1).max(1);
    }
    self.entries.insert(self.next, (dialog_id, Some(outcome)));
    self.next
  }

  fn take_outcome(
    &mut self,
    rid: u32,
  ) -> Option<DesktopFuture<FileDialogOutcome>> {
    self.entries.get_mut(&rid).and_then(|e| e.1.take())
  }

  fn dialog_id(&self, rid: u32) -> Option<u32> {
    self.entries.get(&rid).map(|e| e.0)
  }

  fn remove(&mut self, rid: u32) {
    self.entries.remove(&rid);
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

/// Most paths one `startDrag` takes (laufey's LAUFEY_MAX_DROP_PATHS).
pub const MAX_DRAG_PATHS: usize = 4096;

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
/// leave a window that cannot be closed). Time the JavaScript thread spends
/// in a synchronous dialog (`alert()` / `confirm()` / `prompt()`) does not
/// count: a listener that asks the user "Discard changes?" is answering, not
/// hung, however long the user takes to read it.
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

/// What a close request's timer should do when it wakes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTimeout {
  /// The request is still unanswered after [`CLOSE_REPLY_TIMEOUT`] of
  /// counted time: close the window.
  Close,
  /// The request was answered or replaced: the timer is done.
  Ignore,
  /// Not yet (a synchronous dialog ran or is running): check again after
  /// this long.
  Wait(std::time::Duration),
}

/// The close requests waiting for the app's answer, keyed by window, each
/// with a generation so a timer started for an earlier request can't close a
/// window whose later request is still pending; and the synchronous dialogs
/// the JavaScript thread is in, whose time the timeout does not count.
#[derive(Default)]
pub struct PendingCloses {
  inner: std::sync::Mutex<PendingClosesState>,
}

#[derive(Default)]
struct PendingClosesState {
  next_token: u64,
  /// Window -> (token, when the request began, dialog time at that moment).
  pending: HashMap<u32, (u64, std::time::Instant, std::time::Duration)>,
  /// Synchronous dialogs open on the JavaScript thread (nested ones count).
  dialogs: u32,
  /// Total time spent in finished dialogs.
  dialog_time: std::time::Duration,
  /// When the outermost open dialog started.
  dialog_since: Option<std::time::Instant>,
}

impl PendingClosesState {
  /// Total dialog time up to `now`, the open dialog's included.
  fn dialog_time_at(&self, now: std::time::Instant) -> std::time::Duration {
    self.dialog_time
      + self
        .dialog_since
        .map(|since| now.saturating_duration_since(since))
        .unwrap_or_default()
  }
}

impl PendingCloses {
  /// Record a close request; returns the token its timeout must present.
  pub fn begin(&self, window_id: u32) -> u64 {
    self.begin_at(window_id, std::time::Instant::now())
  }

  fn begin_at(&self, window_id: u32, now: std::time::Instant) -> u64 {
    let mut guard = self.inner.lock().unwrap();
    guard.next_token += 1;
    let token = guard.next_token;
    let dialog_time = guard.dialog_time_at(now);
    guard.pending.insert(window_id, (token, now, dialog_time));
    token
  }

  /// The app answered (`prevented` = a listener called `preventDefault()`).
  pub fn reply(&self, window_id: u32, prevented: bool) -> CloseDecision {
    if self
      .inner
      .lock()
      .unwrap()
      .pending
      .remove(&window_id)
      .is_none()
    {
      return CloseDecision::Ignore;
    }
    if prevented {
      CloseDecision::Keep
    } else {
      CloseDecision::Close
    }
  }

  /// The timer of request `token` woke up: close only if that very request
  /// is still unanswered after [`CLOSE_REPLY_TIMEOUT`] of time not spent in
  /// a synchronous dialog.
  pub fn check_timeout(&self, window_id: u32, token: u64) -> CloseTimeout {
    self.check_timeout_at(window_id, token, std::time::Instant::now())
  }

  fn check_timeout_at(
    &self,
    window_id: u32,
    token: u64,
    now: std::time::Instant,
  ) -> CloseTimeout {
    let mut guard = self.inner.lock().unwrap();
    let Some(&(pending, began, dialog_at_begin)) =
      guard.pending.get(&window_id)
    else {
      return CloseTimeout::Ignore;
    };
    if pending != token {
      return CloseTimeout::Ignore;
    }
    if guard.dialogs > 0 {
      // Paused: look again once the dialog may be over.
      return CloseTimeout::Wait(std::time::Duration::from_millis(250));
    }
    let paused = guard.dialog_time_at(now).saturating_sub(dialog_at_begin);
    let counted = now.saturating_duration_since(began).saturating_sub(paused);
    if counted >= CLOSE_REPLY_TIMEOUT {
      guard.pending.remove(&window_id);
      CloseTimeout::Close
    } else {
      CloseTimeout::Wait(CLOSE_REPLY_TIMEOUT - counted)
    }
  }

  /// The JavaScript thread entered a synchronous dialog.
  pub fn dialog_began(&self) {
    self.dialog_began_at(std::time::Instant::now());
  }

  fn dialog_began_at(&self, now: std::time::Instant) {
    let mut guard = self.inner.lock().unwrap();
    guard.dialogs += 1;
    if guard.dialogs == 1 {
      guard.dialog_since = Some(now);
    }
  }

  /// The JavaScript thread left a synchronous dialog.
  pub fn dialog_ended(&self) {
    self.dialog_ended_at(std::time::Instant::now());
  }

  fn dialog_ended_at(&self, now: std::time::Instant) {
    let mut guard = self.inner.lock().unwrap();
    guard.dialogs = guard.dialogs.saturating_sub(1);
    if guard.dialogs == 0
      && let Some(since) = guard.dialog_since.take()
    {
      guard.dialog_time += now.saturating_duration_since(since);
    }
  }

  /// The window closed some other way (`close()`, quit): forget it.
  pub fn forget(&self, window_id: u32) {
    self.inner.lock().unwrap().pending.remove(&window_id);
  }
}

/// laufey's C ABI takes NUL-terminated strings, so a JS string with an
/// embedded NUL (`"a\0b"`) cannot cross it: the laufey crate panicked on one,
/// and a panic there exits the whole app. Every op that hands a string to the
/// backend refuses one first, with a TypeError naming the argument.
fn reject_nul(what: &str, value: &str) -> Result<(), deno_error::JsErrorBox> {
  if value.contains('\0') {
    Err(deno_error::JsErrorBox::type_error(format!(
      "{what} must not contain a NUL character"
    )))
  } else {
    Ok(())
  }
}

fn reject_nul_opt(
  what: &str,
  value: Option<&str>,
) -> Result<(), deno_error::JsErrorBox> {
  value.map_or(Ok(()), |v| reject_nul(what, v))
}

/// [`reject_nul`] over a menu: labels, ids, accelerators, tooltips, roles.
fn reject_nul_in_menu(
  items: &[MenuItem],
) -> Result<(), deno_error::JsErrorBox> {
  for item in items {
    match item {
      MenuItem::Item {
        label,
        id,
        accelerator,
        tooltip,
        ..
      } => {
        reject_nul("a menu item label", label)?;
        reject_nul_opt("a menu item id", id.as_deref())?;
        reject_nul_opt("a menu item accelerator", accelerator.as_deref())?;
        reject_nul_opt("a menu item tooltip", tooltip.as_deref())?;
      }
      MenuItem::Submenu { label, items } => {
        reject_nul("a submenu label", label)?;
        reject_nul_in_menu(items)?;
      }
      MenuItem::Separator => {}
      MenuItem::Role { role } => reject_nul("a menu role", role)?,
    }
  }
  Ok(())
}

/// [`reject_nul`] over a value handed back to the page (a bound function's
/// result): every string and object key.
fn reject_nul_in_value(
  value: &DesktopValue,
) -> Result<(), deno_error::JsErrorBox> {
  match value {
    DesktopValue::String(s) => reject_nul("a string in the result", s),
    DesktopValue::List(items) => items.iter().try_for_each(reject_nul_in_value),
    DesktopValue::Dict(entries) => entries.iter().try_for_each(|(k, v)| {
      reject_nul("a key in the result", k)?;
      reject_nul_in_value(v)
    }),
    DesktopValue::Null
    | DesktopValue::Bool(_)
    | DesktopValue::Int(_)
    | DesktopValue::Double(_)
    | DesktopValue::Binary(_) => Ok(()),
  }
}

fn reject_nul_in_notification(
  request: &NotificationRequest,
) -> Result<(), deno_error::JsErrorBox> {
  reject_nul("a notification title", &request.title)?;
  reject_nul_opt("a notification body", request.body.as_deref())?;
  reject_nul_opt("a notification tag", request.tag.as_deref())?;
  reject_nul_opt("notification data", request.data.as_deref())?;
  for action in &request.actions {
    reject_nul("a notification action", &action.action)?;
    reject_nul("a notification action title", &action.title)?;
  }
  Ok(())
}

fn reject_nul_in_file_dialog(
  request: &FileDialogRequest,
) -> Result<(), deno_error::JsErrorBox> {
  reject_nul_opt("the dialog title", request.title.as_deref())?;
  reject_nul_opt("the dialog defaultPath", request.default_path.as_deref())?;
  reject_nul_opt("the dialog buttonLabel", request.button_label.as_deref())?;
  for filter in &request.filters {
    reject_nul("a filter name", &filter.name)?;
    for ext in &filter.extensions {
      reject_nul("a filter extension", ext)?;
    }
  }
  Ok(())
}

/// For text that must reach the backend whatever it holds (an error
/// message on its way to a dialog or to the page): NUL becomes U+FFFD.
fn replace_nul(text: String) -> String {
  if text.contains('\0') {
    text.replace('\0', "\u{FFFD}")
  } else {
    text
  }
}

/// How a window is closed for real (an answered or timed-out close request,
/// `close()`, DevTools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeClose {
  /// Destroy the OS window.
  Destroy,
  /// A WebGPU surface holds the window's native handles, and destroying the
  /// OS window under it would leave the surface pointing at freed memory:
  /// hide it instead, kept until the process ends, and count it as closed.
  /// `quit` is set when that was the last open window and the app quits on
  /// its last window closing (the hidden window would otherwise keep the
  /// app running).
  HideAndKeep { quit: bool },
}

/// See [`NativeClose`]. `others_open`: whether another window is still open
/// once this one is closed.
pub fn native_close_action(
  surface_attached: bool,
  quit_on_last_window_closed: bool,
  others_open: bool,
) -> NativeClose {
  if !surface_attached {
    return NativeClose::Destroy;
  }
  NativeClose::HideAndKeep {
    quit: quit_on_last_window_closed && !others_open,
  }
}

/// Marks a synchronous dialog on the JavaScript thread for the duration of
/// the call (see [`CLOSE_REPLY_TIMEOUT`]).
struct SyncDialog<'a>(&'a dyn DesktopApi);

impl<'a> SyncDialog<'a> {
  fn begin(api: &'a dyn DesktopApi) -> Self {
    api.sync_dialog_began();
    Self(api)
  }
}

impl Drop for SyncDialog<'_> {
  fn drop(&mut self) {
    self.0.sync_dialog_ended();
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

/// A dialog the backend has no way to show here (laufey API 45): nothing
/// was shown, which is not the user cancelling. `alert()` / `confirm()` /
/// `prompt()` throw `Deno.errors.NotSupported` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogUnsupported;

/// The `NotSupported` error a page's `alert()` / `confirm()` / `prompt()`
/// gets when no dialog can be shown here.
fn dialog_unsupported_error(what: &str) -> JsErrorBox {
  JsErrorBox::new(
    "NotSupported",
    format!(
      "{what} can't be shown here: the backend has no way to show a dialog \
       (on Linux: no kdialog, zenity or GTK display)"
    ),
  )
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

  /// Expose binding `name` to the window's page, callable from the documents
  /// [`bind_call_allowed`] admits for `origins`.
  fn bind(&self, window_id: u32, name: &str, origins: BindOrigins);
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

  // --- Drag and drop, file dialogs, rich clipboard (laufey API 39) ---
  //
  // The defaults are a runtime without any of it: drags and dialogs fail,
  // and the clipboard is text only.

  /// Drag `paths` out of the window (laufey `start_file_drag`). The drag is
  /// requested when this is called; the future resolves when it ends.
  fn start_file_drag(
    &self,
    _window_id: u32,
    _paths: Vec<String>,
    _icon_png: Option<Vec<u8>>,
  ) -> DesktopFuture<DragOutcome> {
    Box::pin(async { DragOutcome::Failed })
  }
  /// Show a file dialog. Returns the backend's dialog id (0 when the request
  /// was answered at once) and its outcome; the dialog is requested when this
  /// is called, and the runtime thread never blocks on it.
  fn show_file_dialog(
    &self,
    _request: FileDialogRequest,
  ) -> (u32, DesktopFuture<FileDialogOutcome>) {
    (0, Box::pin(async { FileDialogOutcome::Failed }))
  }
  /// Close an open dialog as cancelled. False when it isn't open.
  fn cancel_file_dialog(&self, _dialog_id: u32) -> bool {
    false
  }
  fn clipboard_capabilities(&self) -> ClipboardCapabilitiesInfo {
    ClipboardCapabilitiesInfo {
      text: true,
      ..Default::default()
    }
  }
  /// Blocking (runs on the blocking pool), like `read_clipboard_text`.
  fn read_clipboard_html(&self) -> Option<String> {
    None
  }
  fn write_clipboard_html(&self, _html: &str, _text: Option<&str>) -> bool {
    false
  }
  /// PNG bytes.
  fn read_clipboard_image(&self) -> Option<Vec<u8>> {
    None
  }
  fn write_clipboard_image(&self, _png: &[u8]) -> bool {
    false
  }
  /// MIME types; `None` when the backend can't tell.
  fn read_clipboard_formats(&self) -> Option<Vec<String>> {
    None
  }
  /// Start / stop delivering [`DesktopEvent::ClipboardChange`] (macOS polls
  /// the pasteboard only while on).
  fn set_clipboard_watch(&self, _on: bool) {}

  // --- Global shortcuts, launch at login, DevTools (laufey API 40) ---
  //
  // The defaults are a runtime without any of it.

  fn system_capabilities(&self) -> SystemCapabilitiesInfo {
    SystemCapabilitiesInfo::default()
  }

  // --- Platform features (laufey API 45) ---

  /// What this session provides (the backend's `platform_features` JSON
  /// object: the tray host, the Secret Service, the notification server,
  /// the session type, the portal versions, the cookie store). `None` when
  /// the backend can't say. Blocking: the first call on Linux may wait a
  /// few seconds for xdg-desktop-portal to start, so ops call it off the
  /// JavaScript thread.
  fn platform_features(&self) -> Option<String> {
    None
  }

  /// Whether the backend has a secure store (laufey API 47: the Secret
  /// Service on Linux, the Keychain on macOS; CEF and WebView).
  fn secret_store_supported(&self) -> bool {
    false
  }

  /// A secure-store call (laufey API 47). Blocking (for at most about
  /// `timeout_ms`, which the backend bounds an unlock prompt with): ops call
  /// it on the blocking pool.
  fn secret_request(&self, _request: &SecretRequest) -> SecretOutcome {
    SecretOutcome::Unsupported
  }

  /// How the user set up title bars (the backend's `title_bar_preferences`
  /// JSON object, laufey API 47): the buttons on each side, the double-click
  /// action, the colour scheme, the accent colour, the title bar font.
  /// `None` when the backend can't say. Blocking: on Linux the first call
  /// may wait for xdg-desktop-portal to start, so ops call it off the
  /// JavaScript thread.
  fn title_bar_preferences(&self) -> Option<String> {
    None
  }

  /// Why a tray icon can't be shown here (the tray part of the probe only;
  /// never waits for the portal), `None` when one can or the backend can't
  /// say.
  fn tray_unavailable_reason(&self) -> Option<String> {
    None
  }
  /// Bind a system-wide shortcut. Presses arrive as
  /// [`DesktopEvent::Shortcut`]. The request is made when this is called.
  fn register_shortcut(
    &self,
    _accelerator: &str,
  ) -> DesktopFuture<ShortcutRegisterInfo> {
    Box::pin(async { ShortcutRegisterInfo::err("not_supported") })
  }
  /// Release a shortcut (any spelling). False if it wasn't registered.
  fn unregister_shortcut(&self, _accelerator: &str) -> bool {
    false
  }
  fn unregister_all_shortcuts(&self) {}
  /// The canonical accelerators registered, in registration order.
  fn list_shortcuts(&self) -> Vec<String> {
    Vec::new()
  }
  /// The canonical form of an accelerator, or `None` if it doesn't parse.
  fn canonical_accelerator(&self, _accelerator: &str) -> Option<String> {
    None
  }
  /// One of [`LOGIN_ITEM_STATES`]. Blocking (runs on the blocking pool).
  fn launch_at_login(&self) -> &'static str {
    "not-supported"
  }
  /// Turn launch at login on or off: the state afterwards, or the OS's
  /// error message. Blocking (runs on the blocking pool).
  fn set_launch_at_login(
    &self,
    _enabled: bool,
  ) -> Result<&'static str, String> {
    Ok("not-supported")
  }
  fn close_devtools(&self, _window_id: u32) {}
  fn is_devtools_open(&self, _window_id: u32) -> bool {
    false
  }
  /// Whether DevTools can open: for a window, its engine's setting read
  /// back; for 0, the launch setting (`LAUFEY_INSPECTABLE`).
  fn devtools_enabled(&self, _window_id: u32) -> bool {
    false
  }
  fn set_application_menu(&self, window_id: u32, menu: Vec<MenuItem>);
  fn show_context_menu(
    &self,
    window_id: u32,
    x: i32,
    y: i32,
    menu: Vec<MenuItem>,
  );

  /// A WebGPU surface now holds this window's native handles: from here on
  /// no path may destroy the OS window (see [`native_close_action`]).
  fn note_surface_attached(&self, _window_id: u32) {}

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

  /// Show a modal alert. `Err(DialogUnsupported)` when the backend has no
  /// way to show it here (laufey API 45: Winit on Linux without kdialog,
  /// zenity or a GTK display): nothing was shown.
  fn alert(&self, title: &str, message: &str) -> Result<(), DialogUnsupported>;
  /// The JavaScript thread entered / left a synchronous `alert()` /
  /// `confirm()` / `prompt()`: a pending close request's
  /// [`CLOSE_REPLY_TIMEOUT`] does not count that time.
  fn sync_dialog_began(&self) {}
  fn sync_dialog_ended(&self) {}
  /// Show a modal confirm dialog. Blocks the calling thread until the
  /// user dismisses it; the platform's modal run loop pumps OS events
  /// while the dialog is up so other windows continue to render and
  /// respond.
  /// `Err(DialogUnsupported)` as for `alert`; `Ok(false)` is a cancel.
  fn confirm(
    &self,
    title: &str,
    message: &str,
  ) -> Result<bool, DialogUnsupported>;
  /// Show a modal prompt dialog. Returns the entered text on confirm,
  /// `Ok(None)` on cancel, `Err(DialogUnsupported)` as for `alert`.
  /// Blocking semantics as `confirm`.
  fn prompt(
    &self,
    title: &str,
    message: &str,
    default_value: &str,
  ) -> Result<Option<String>, DialogUnsupported>;

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
  /// notification (`Show`, `Click`, `Action`, `Close`, `Error`) are
  /// delivered via the desktop event channel keyed by the returned id.
  fn show_notification(&self, request: &NotificationRequest) -> u32;
  /// Schedule a notification for `request.schedule_at_ms` (laufey API 41).
  /// Its clicks arrive as [`DesktopEvent::NotificationResponse`]. False when
  /// the backend refused it.
  fn schedule_notification(&self, _request: &NotificationRequest) -> bool {
    false
  }
  /// The pending scheduled notifications, soonest first (laufey API 41).
  fn list_scheduled_notifications(
    &self,
  ) -> DesktopFuture<Vec<ScheduledNotificationInfo>> {
    Box::pin(async { Vec::new() })
  }
  /// Cancel the scheduled notification `tag` and remove delivered ones with
  /// that tag (laufey API 41).
  fn cancel_notification(&self, _tag: &str) {}
  fn notification_capabilities(&self) -> NotificationCapabilitiesInfo {
    NotificationCapabilitiesInfo::default()
  }
  fn menu_capabilities(&self) -> MenuCapabilitiesInfo {
    MenuCapabilitiesInfo::default()
  }
  /// Ask for quiet ("provisional") notification authorization, which macOS
  /// grants without a prompt (laufey API 41); elsewhere the same as
  /// [`DesktopApi::request_notification_permission`].
  fn request_provisional_notification_permission(
    &self,
    cb: Box<dyn FnOnce(PermissionState) + Send + 'static>,
  ) {
    self.request_notification_permission(cb);
  }
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

/// Stores the window ID of the initial window created during runtime init,
/// with the creation-time attributes it was created with.
/// The first `BrowserWindow` constructor whose options agree with those
/// attributes takes this ID to wrap the existing window (see
/// [`adopts_initial_window`]); other constructors create new windows.
pub struct InitialWindowId(
  pub std::sync::Mutex<Option<u32>>,
  pub InitialWindowAttributes,
);

/// The attributes of a window that are fixed when it is created (the backend
/// cannot change them afterwards): what `desktop.initialWindow` configured
/// for the bootstrap window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InitialWindowAttributes {
  pub frameless: bool,
  pub no_activate: bool,
  pub transparent_titlebar: bool,
  pub transparent: bool,
}

/// Whether a `new BrowserWindow(options)` may adopt the bootstrap window
/// instead of creating one. It may only when every creation-time attribute
/// the options ask for is what the bootstrap window already has: adopting it
/// regardless silently dropped `frameless` / `noActivate` / `transparent` /
/// `transparentTitlebar`, so a tray panel (`attachPanel`) came out as an
/// ordinary framed, focus-stealing window. An attribute the options leave
/// unset keeps the configured `initialWindow` value.
fn adopts_initial_window(
  initial: &InitialWindowAttributes,
  options: Option<&BrowserWindowOptions>,
) -> bool {
  let Some(o) = options else {
    return true;
  };
  let agrees = |asked: Option<bool>, has: bool| asked.is_none_or(|v| v == has);
  agrees(o.frameless, initial.frameless)
    && agrees(o.no_activate, initial.no_activate)
    && agrees(o.transparent_titlebar, initial.transparent_titlebar)
    && agrees(o.transparent, initial.transparent)
}

/// The native API for a `Deno.desktop` constructor. Workers have no desktop
/// backend (the classes are also kept out of worker scope); constructing one
/// there threw a Rust panic that took the whole app down.
fn constructor_api(
  state: &OpState,
  class: &str,
) -> Result<Arc<dyn DesktopApi>, deno_error::JsErrorBox> {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .cloned()
    .ok_or_else(|| {
      deno_error::JsErrorBox::new(
        "NotSupported",
        format!("{class} is only available in the main scope of a desktop app"),
      )
    })
}

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
  set_event_target_data: v8::Global<v8::Function>,
}

/// The backend, the webidl brand and `setEventTargetData`.
type ClassPrerequisites = (
  Arc<dyn DesktopApi>,
  v8::Global<v8::Value>,
  v8::Global<v8::Function>,
);

/// What a native class constructor needs before it creates anything: the
/// desktop backend and DESKTOP_JS's event-target setup. The classes sit on
/// `core.ops` and survive `removeImportedOps()` (NOT_IMPORTED_OPS), so a
/// plain `deno run` can construct them: there they throw `NotSupported`
/// instead of panicking the process.
fn class_prerequisites(
  state: &OpState,
  class: &str,
) -> Result<ClassPrerequisites, JsErrorBox> {
  let api = constructor_api(state, class)?;
  let setup = state.try_borrow::<EventTargetSetup>().ok_or_else(|| {
    JsErrorBox::new(
      "NotSupported",
      format!("{class} is only available once the desktop runtime is set up"),
    )
  })?;
  Ok((
    api,
    setup.brand.clone(),
    setup.set_event_target_data.clone(),
  ))
}

/// Brands a freshly made native object as an EventTarget, as DESKTOP_JS set
/// it up in `op_desktop_init`.
fn init_event_target<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  object: v8::Local<'s, v8::Object>,
  brand: &v8::Global<v8::Value>,
  set_event_target_data: &v8::Global<v8::Function>,
) {
  let brand = v8::Local::new(scope, brand);
  object.set(scope, brand, brand);
  let set_event_target_data = v8::Local::new(scope, set_event_target_data);
  let null = v8::null(scope);
  set_event_target_data.call(scope, null.into(), &[object.into()]);
}

#[op2]
impl BrowserWindow {
  #[constructor]
  fn new(
    state: &OpState,
    scope: &mut v8::PinScope<'_, '_>,
    #[scoped] options: Option<BrowserWindowOptions>,
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (api, brand, set_event_target_data) =
      class_prerequisites(state, "BrowserWindow")?;
    // Before anything is created: a title with a NUL used to be refused only
    // after the window existed (and left it open).
    if let Some(title) = options.as_ref().and_then(|o| o.title.as_deref()) {
      reject_nul("the window title", title)?;
    }

    // Use the initial window if this is the first BrowserWindow whose
    // creation-time options it satisfies, otherwise create a new one (the
    // bootstrap window then stays available to a later BrowserWindow).
    let window_id = state
      .try_borrow::<InitialWindowId>()
      .filter(|iw| adopts_initial_window(&iw.1, options.as_ref()))
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
      // Only the dimensions the options give: an adopted bootstrap window
      // keeps its `initialWindow` size for the others (a `{ title }` alone
      // used to shrink it to 800x600), and a new window was already created
      // at its size.
      if options.width.is_some() || options.height.is_some() {
        let (width, height) = api.get_window_size(window_id);
        api.set_window_size(
          window_id,
          options.width.unwrap_or(width),
          options.height.unwrap_or(height),
        );
      }
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

    // The frame a normal window adds around its content, noted now while
    // the new window is normal, so getNormalBounds() of a window the user
    // put into fullscreen (or maximized) before the app asked anything still
    // adds the title bar back.
    let normal_chrome = std::cell::Cell::new(None);
    note_normal_chrome(api.as_ref(), window_id, &normal_chrome);
    let window = BrowserWindow {
      api,
      window_id,
      surface: SameObject::new(),
      surface_taken: std::cell::Cell::new(false),
      normal_chrome,
    };
    let window = deno_core::cppgc::make_cppgc_object(scope, window);
    init_event_target(scope, window, &brand, &set_event_target_data);
    let window = window.cast::<v8::Value>();

    Ok(v8::Global::new(scope, window))
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
  fn bind(
    &self,
    #[string] name: &str,
    #[string] origins: &str,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul("the binding name", name)?;
    let origins = BindOrigins::from_spec(origins)?;
    self.api.bind(self.window_id, name, origins);
    Ok(())
  }

  #[fast]
  #[symbol("Deno_privateDesktopUnbind")]
  fn unbind(&self, #[string] name: &str) -> Result<(), deno_error::JsErrorBox> {
    reject_nul("the binding name", name)?;
    self.api.unbind(self.window_id, name);
    Ok(())
  }

  #[fast]
  fn set_title(
    &self,
    #[string] title: &str,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul("the window title", title)?;
    self.api.set_title(self.window_id, title);
    Ok(())
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
    // Always a real close: the window counts as closed, and the app quits
    // when it was the last one. A window a WebGPU surface holds is hidden and
    // kept instead of destroyed by the shared close path
    // (`note_surface_attached`, `native_close_action`); this used to only
    // hide it here, so `isClosed()` stayed false and a last window closed
    // this way never quit the app.
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
  fn navigate(
    &self,
    #[string] url: &str,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul("the URL", url)?;
    self.api.navigate(self.window_id, url);
    Ok(())
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

  /// Close this window's DevTools (laufey API 40).
  #[fast]
  fn close_devtools(&self) {
    self.api.close_devtools(self.window_id);
  }

  /// Whether this window's DevTools are open (laufey API 40).
  #[fast]
  fn is_devtools_open(&self) -> bool {
    self.api.is_devtools_open(self.window_id)
  }

  /// Whether this window's engine lets DevTools open (laufey API 40).
  #[fast]
  fn is_devtools_enabled(&self) -> bool {
    self.api.devtools_enabled(self.window_id)
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
    reject_nul("the script", &script)?;
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

  fn set_application_menu(
    &self,
    #[serde] menu: Vec<MenuItem>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_in_menu(&menu)?;
    self.api.set_application_menu(self.window_id, menu);
    Ok(())
  }

  fn show_context_menu(
    &self,
    #[smi] x: i32,
    #[smi] y: i32,
    #[serde] menu: Vec<MenuItem>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_in_menu(&menu)?;
    self.api.show_context_menu(self.window_id, x, y, menu);
    Ok(())
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
      // Once a surface has been taken (`note_surface_attached` below) every
      // close path hides the window instead of destroying it
      // (`native_close_action`), and the OS window outlives
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
    // And the closes that don't go through `close()`: the user's (an
    // answered or timed-out close request), DevTools.
    self.api.note_surface_attached(self.window_id);
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

#[derive(FromV8, Default)]
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

  // Patching a file inside a macOS `.app` breaks the bundle's code signature
  // (and its notarization): the next launch fails Gatekeeper, or the patched
  // code runs under a broken seal. Full-app updates (`Deno.desktop.updater`)
  // replace the whole signed bundle instead, so the in-place patch is refused
  // there. Elsewhere (no OS code signature on Linux; Windows checks a
  // signature only at install time) the legacy patch path is kept.
  if cfg!(target_os = "macos")
    && dylib_path
      .ancestors()
      .any(|p| p.extension().is_some_and(|e| e == "app"))
  {
    return Err(deno_error::JsErrorBox::generic(
      "Deno.autoUpdate cannot patch the runtime inside a macOS .app bundle: \
       it would break the bundle's code signature. Use Deno.desktop.updater \
       (full signed-bundle updates) instead",
    ));
  }

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
  // Outside a desktop app there is no event source: answer null (the end of
  // the stream, which DESKTOP_JS's loop stops on) instead of a promise that
  // never settles. The op sits on `core.ops` (NOT_IMPORTED_OPS), and a
  // pending, ref'd op would keep a plain `deno run` alive forever.
  let rx = rx?;
  rx.recv().await
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
) -> Result<(), deno_error::JsErrorBox> {
  // Checked before the call is taken: on a TypeError DESKTOP_JS rejects
  // the call with it instead.
  reject_nul_in_value(&result)?;
  if let Some(responses) = state.try_borrow::<PendingBindResponses>()
    && let Some(tx) = responses.0.lock().unwrap().remove(&call_id)
  {
    let _ = tx.send(Ok(result));
  }
  Ok(())
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
    // The rejection must reach the page whatever the message holds.
    let _ = tx.send(Err(replace_nul(error)));
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
  // Taking a scheme from the app that owns it needs `--allow-sys`; claiming
  // one nobody owns (what the runtime does at startup anyway) does not.
  if force {
    check_desktop_integration(&state.borrow())?;
  }
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

fn desktop_auth_session(
  state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<Arc<dyn DesktopAuthSession>> {
  state
    .borrow()
    .try_borrow::<Arc<dyn DesktopAuthSession>>()
    .cloned()
}

/// `Deno.desktop.authSession.capabilities()`.
#[op2]
#[serde]
fn op_desktop_auth_session_capabilities(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> AuthSessionCapabilitiesInfo {
  desktop_auth_session(&state)
    .map(|a| a.capabilities())
    .unwrap_or_default()
}

/// `Deno.desktop.authSession.start()`: the outcome, never an exception (the
/// JS side validates the argument types and turns an error outcome into a
/// rejection).
#[op2]
#[serde]
async fn op_desktop_auth_session_start(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[smi] window_id: u32,
  #[string] url: String,
  #[string] callback: String,
  ephemeral: bool,
) -> AuthSessionOutcome {
  match desktop_auth_session(&state) {
    Some(auth) => auth.start(window_id, url, callback, ephemeral).await,
    None => AuthSessionOutcome::error(
      "not_supported",
      AUTH_SESSION_NOT_SUPPORTED_MESSAGE,
    ),
  }
}

/// `Deno.desktop.authSession.cancel()`: true when a running session was
/// ended (its `start()` rejects with code `cancelled`), false when none was
/// running or the runtime has no OS auth sessions.
#[op2(fast)]
fn op_desktop_auth_session_cancel(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> bool {
  auth_session_cancel(&state)
}

fn auth_session_cancel(
  state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> bool {
  desktop_auth_session(state).is_some_and(|a| a.cancel())
}

/// `Deno.desktop.runOnMainThread(fn, context)`: calls the native function on
/// the UI thread and resolves with its return value (a decimal string the JS
/// side turns into a bigint). Full trust: it needs `--allow-ffi`, like
/// calling the pointer through `Deno.UnsafeFnPointer`.
#[op2(stack_trace)]
#[string]
fn op_desktop_run_on_main_thread(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  function: *mut std::ffi::c_void,
  context: *mut std::ffi::c_void,
) -> Result<
  impl std::future::Future<Output = Result<String, deno_error::JsErrorBox>> + use<>,
  deno_error::JsErrorBox,
> {
  let main_thread = {
    let mut state = state.borrow_mut();
    state
      .borrow_mut::<deno_permissions::PermissionsContainer>()
      .check_ffi_partial_no_path()
      .map_err(deno_error::JsErrorBox::from_err)?;
    state.try_borrow::<Arc<dyn DesktopMainThread>>().cloned()
  };
  if function.is_null() {
    return Err(deno_error::JsErrorBox::type_error(
      "the function pointer is null",
    ));
  }
  let Some(main_thread) = main_thread else {
    return Err(deno_error::JsErrorBox::not_supported());
  };
  // SAFETY: the caller holds --allow-ffi (checked above), which vouches for
  // the pointer as for Deno.UnsafeFnPointer#call.
  let pending =
    unsafe { main_thread.call(function as usize, context as usize) };
  Ok(async move {
    pending
      .await
      .map(|value| value.to_string())
      .map_err(deno_error::JsErrorBox::generic)
  })
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

/// DESKTOP_JS hands over the webidl brand and `setEventTargetData` once, at
/// startup, before any app code runs. The op sits on `core.ops`
/// (NOT_IMPORTED_OPS), so later calls are refused: app code can't swap in
/// its own function for every window, tray and notification made after.
#[op2(fast)]
pub fn op_desktop_init(
  state: &mut OpState,
  scope: &mut v8::PinScope<'_, '_>,
  webidl_brand: v8::Local<v8::Value>,
  set_event_target_data: v8::Local<v8::Value>,
) -> Result<(), JsErrorBox> {
  if state.has::<EventTargetSetup>() {
    return Err(JsErrorBox::generic("the desktop runtime is already set up"));
  }
  let Ok(set_event_target_data) =
    v8::Local::<v8::Function>::try_from(set_event_target_data)
  else {
    return Err(JsErrorBox::type_error(
      "setEventTargetData is not a function",
    ));
  };
  state.put(EventTargetSetup {
    brand: v8::Global::new(scope, webidl_brand),
    set_event_target_data: v8::Global::new(scope, set_event_target_data),
  });
  Ok(())
}

#[op2(fast)]
fn op_desktop_alert(
  state: &mut OpState,
  #[string] title: &str,
  #[string] message: &str,
) -> Result<(), deno_error::JsErrorBox> {
  reject_nul("alert() title", title)?;
  reject_nul("alert() message", message)?;
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    let _dialog = SyncDialog::begin(api.as_ref());
    api
      .alert(title, message)
      .map_err(|_| dialog_unsupported_error("alert()"))?;
  }
  Ok(())
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
  // An error message may hold anything; the dialog must still show.
  let (title, message) = (replace_nul(title), replace_nul(message));
  let dialog = deno_core::unsync::spawn_blocking(move || {
    let _guard = ErrorDialogGuard;
    // Nothing shown (no dialog provider): the message is on stderr already.
    let _ = api.alert(&title, &message);
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

/// How long an HTTPS error report may take (connect, TLS, request and
/// response) before it is given up. The report is posted on its own thread
/// and joined, so the panic hook's report is out before the process exits;
/// without a bound, an endpoint that accepts and never answers (or a
/// black-holed network) hung the exiting app, or the error path, forever.
const ERROR_REPORT_TIMEOUT: std::time::Duration =
  std::time::Duration::from_secs(5);

fn post_error_report(client: deno_fetch::Client, url: String, body: String) {
  post_error_report_within(client, url, body, ERROR_REPORT_TIMEOUT);
}

fn post_error_report_within(
  client: deno_fetch::Client,
  url: String,
  body: String,
  timeout: std::time::Duration,
) {
  let _ =
    std::thread::spawn(move || {
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
      if tokio::time::timeout(timeout, client.send(req)).await.is_err() {
        log::warn!(
          "desktop: error report not delivered within {timeout:?}; dropping it"
        );
      }
    });
      // Don't wait on a resolver thread still stuck in the dropped request.
      runtime.shutdown_timeout(std::time::Duration::from_millis(100));
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

/// Sends the report off the JavaScript thread (an HTTPS report may take up
/// to [`ERROR_REPORT_TIMEOUT`], and the JavaScript thread used to wait for
/// it, its timers and servers stalled); the error handler awaits the promise
/// before it exits.
#[op2]
async fn op_desktop_send_error_report(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] body: String,
) {
  let Some(url) = prepare_error_report(&mut state.borrow_mut()) else {
    return;
  };
  let _ = deno_core::unsync::spawn_blocking(move || {
    send_error_report(url, &body);
  })
  .await;
}

/// The configured report destination, with the report client set up (see
/// [`op_desktop_send_error_report`]); `None` when nothing is configured.
fn prepare_error_report(state: &mut OpState) -> Option<&'static str> {
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
    return None;
  };
  // Make sure the panic-hook path has a client too. The OpState client is
  // the one configured with the user's TLS roots/permissions, so we share
  // it across both code paths instead of creating an ad-hoc client.
  if ERROR_REPORT_CLIENT.get().is_none()
    && let Ok(client) = deno_fetch::get_or_create_client_from_state(state)
  {
    set_error_report_client(client);
  }
  Some(url)
}

#[op2(fast)]
fn op_desktop_confirm(
  state: &mut OpState,
  #[string] message: &str,
) -> Result<bool, deno_error::JsErrorBox> {
  // Sync op: web `confirm()` returns a boolean, not a Promise. The
  // backend's `confirm` blocks the calling thread inside the platform's
  // modal run loop (NSAlert runModal / MessageBoxW / gtk_dialog_run /
  // rfd) which itself pumps OS events, so other windows stay responsive
  // while the dialog is up.
  reject_nul("confirm() message", message)?;
  Ok(match state.try_borrow::<Arc<dyn DesktopApi>>() {
    Some(api) => {
      let _dialog = SyncDialog::begin(api.as_ref());
      api
        .confirm("", message)
        .map_err(|_| dialog_unsupported_error("confirm()"))?
    }
    None => false,
  })
}

#[op2]
#[string]
fn op_desktop_prompt(
  state: &mut OpState,
  #[string] message: &str,
  #[string] default_value: Option<String>,
) -> Result<Option<String>, deno_error::JsErrorBox> {
  // See `op_desktop_confirm` for the sync-blocking rationale.
  reject_nul("prompt() message", message)?;
  let default_value = default_value.unwrap_or_default();
  reject_nul("prompt() default value", &default_value)?;
  Ok(match state.try_borrow::<Arc<dyn DesktopApi>>() {
    Some(api) => {
      let _dialog = SyncDialog::begin(api.as_ref());
      // A cancel is null; nothing shown at all is NotSupported.
      api
        .prompt("", message, &default_value)
        .map_err(|_| dialog_unsupported_error("prompt()"))?
    }
    None => None,
  })
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
    check_desktop_integration(&s)?;
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
  reject_nul("the clipboard text", &text)?;
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

fn desktop_api(
  state: &std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<Arc<dyn DesktopApi>> {
  state.borrow().try_borrow::<Arc<dyn DesktopApi>>().cloned()
}

/// The permission the desktop integrations that reach past the app's own
/// windows need: unscoped `--allow-sys` (or `-A`). They are reading the
/// clipboard (and watching it change), global shortcuts (key combinations
/// taken from every other app), launch at login, taking over a URL scheme
/// another app owns (`registerScheme({ force: true })`), and posting OS
/// notifications. A permission-less dependency of the app reached all of
/// them. Deno has no permission kind of their own, and the stock `deno
/// desktop` CLI that packages an app refuses `--allow-sys` names it doesn't
/// know, so they share the whole `sys` grant; a partial
/// `--allow-sys=<names>` is not enough. Checked before anything else, so
/// the refusal (`NotCapable`) is the same in and outside a desktop app.
fn check_desktop_integration(
  state: &OpState,
) -> Result<(), deno_error::JsErrorBox> {
  state
    .borrow::<deno_permissions::PermissionsContainer>()
    .check_sys_all()
    .map_err(deno_error::JsErrorBox::from_err)
}

/// Runs a blocking clipboard call on the blocking pool with the clipboard
/// timeout, for the same reasons as `op_desktop_read_clipboard_text`.
async fn clipboard_blocking<T: Send + 'static>(
  op: &'static str,
  f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, deno_error::JsErrorBox> {
  let task = deno_core::unsync::spawn_blocking(f);
  match tokio::time::timeout(CLIPBOARD_TIMEOUT, task).await {
    Ok(Ok(v)) => Ok(v),
    Ok(Err(_join)) => Err(clipboard_failed(op)),
    Err(_elapsed) => Err(clipboard_unavailable(op)),
  }
}

fn clipboard_not_supported(what: &str) -> deno_error::JsErrorBox {
  deno_error::JsErrorBox::new(
    "NotSupported",
    format!("the clipboard does not support {what} on this platform"),
  )
}

/// `Deno.desktop.clipboard.capabilities()`.
#[op2]
#[serde]
fn op_desktop_clipboard_capabilities(
  state: &mut OpState,
) -> ClipboardCapabilitiesInfo {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.clipboard_capabilities())
    .unwrap_or_default()
}

/// `Deno.desktop.clipboard.readHTML()`: `None` when the clipboard holds no
/// HTML.
#[op2]
#[string]
async fn op_desktop_read_clipboard_html(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<Option<String>, deno_error::JsErrorBox> {
  check_desktop_integration(&state.borrow())?;
  let Some(api) = desktop_api(&state) else {
    return Ok(None);
  };
  clipboard_blocking("read", move || api.read_clipboard_html()).await
}

/// `Deno.desktop.clipboard.writeHTML()`.
#[op2]
async fn op_desktop_write_clipboard_html(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] html: String,
  #[string] text: Option<String>,
) -> Result<(), deno_error::JsErrorBox> {
  reject_nul("the clipboard HTML", &html)?;
  reject_nul_opt("the clipboard text", text.as_deref())?;
  let Some(api) = desktop_api(&state) else {
    return Err(clipboard_not_supported("HTML"));
  };
  if !api.clipboard_capabilities().html {
    return Err(clipboard_not_supported("HTML"));
  }
  let ok = clipboard_blocking("write", move || {
    api.write_clipboard_html(&html, text.as_deref())
  })
  .await?;
  if ok {
    Ok(())
  } else {
    Err(clipboard_failed("write"))
  }
}

/// `Deno.desktop.clipboard.readImage()`: PNG bytes, or `None` when the
/// clipboard holds no image.
#[op2]
#[serde]
async fn op_desktop_read_clipboard_image(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<Option<deno_core::ToJsBuffer>, deno_error::JsErrorBox> {
  check_desktop_integration(&state.borrow())?;
  let Some(api) = desktop_api(&state) else {
    return Ok(None);
  };
  let png =
    clipboard_blocking("read", move || api.read_clipboard_image()).await?;
  Ok(png.map(deno_core::ToJsBuffer::from))
}

/// True when `bytes` starts with the PNG signature.
pub fn looks_like_png(bytes: &[u8]) -> bool {
  bytes.len() > 8
    && bytes[..8] == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']
}

/// `Deno.desktop.clipboard.writeImage(png)`.
#[op2]
async fn op_desktop_write_clipboard_image(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[buffer(copy)] png: Vec<u8>,
) -> Result<(), deno_error::JsErrorBox> {
  if !looks_like_png(&png) {
    return Err(deno_error::JsErrorBox::type_error(
      "writeImage takes PNG bytes",
    ));
  }
  let Some(api) = desktop_api(&state) else {
    return Err(clipboard_not_supported("images"));
  };
  if !api.clipboard_capabilities().image {
    return Err(clipboard_not_supported("images"));
  }
  let ok = clipboard_blocking("write", move || api.write_clipboard_image(&png))
    .await?;
  if ok {
    Ok(())
  } else {
    Err(deno_error::JsErrorBox::generic(
      "clipboard write failed (not a decodable PNG, or the clipboard refused it)",
    ))
  }
}

/// `Deno.desktop.clipboard.availableFormats()`.
#[op2]
#[serde]
async fn op_desktop_read_clipboard_formats(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<Vec<String>, deno_error::JsErrorBox> {
  check_desktop_integration(&state.borrow())?;
  let Some(api) = desktop_api(&state) else {
    return Ok(Vec::new());
  };
  let formats =
    clipboard_blocking("read", move || api.read_clipboard_formats()).await?;
  Ok(formats.unwrap_or_default())
}

/// Starts / stops the clipboard "change" events (the JS side turns them on
/// with the first listener and off with the last). Turning them on needs
/// `--allow-sys`, as reading the clipboard does.
#[op2(fast)]
fn op_desktop_clipboard_watch(
  state: &mut OpState,
  on: bool,
) -> Result<(), deno_error::JsErrorBox> {
  if on {
    check_desktop_integration(state)?;
  }
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.set_clipboard_watch(on);
  }
  Ok(())
}

/// `BrowserWindow.prototype.startDrag()`: `"dropped"`, `"cancelled"` or
/// `"failed"`.
///
/// Dragging a file out hands it (its contents) to another app, so every
/// path needs read permission (`--allow-read`), checked before the drag
/// starts: a missing permission rejects with `NotCapable`, as reading the
/// file would.
#[op2(stack_trace)]
#[string]
async fn op_desktop_start_drag(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[smi] window_id: u32,
  #[serde] paths: Vec<String>,
  #[buffer(copy)] icon: Vec<u8>,
) -> Result<String, deno_error::JsErrorBox> {
  let Some(api) = desktop_api(&state) else {
    return Ok(DragOutcome::Failed.as_str().to_string());
  };
  // A path with a NUL can't name a file (nor cross laufey's C ABI).
  if paths.is_empty()
    || paths.len() > MAX_DRAG_PATHS
    || paths.iter().any(|p| p.contains('\0'))
  {
    return Ok(DragOutcome::Failed.as_str().to_string());
  }
  check_drag_read_permission(&state.borrow(), &paths)?;
  let icon = if icon.is_empty() { None } else { Some(icon) };
  Ok(
    api
      .start_file_drag(window_id, paths, icon)
      .await
      .as_str()
      .to_string(),
  )
}

/// Read permission for each path a drag-out carries.
fn check_drag_read_permission(
  state: &OpState,
  paths: &[String],
) -> Result<(), deno_error::JsErrorBox> {
  let permissions = state.borrow::<deno_permissions::PermissionsContainer>();
  for path in paths {
    permissions
      .check_open(
        Cow::Borrowed(Path::new(path)),
        deno_permissions::OpenAccessKind::Read,
        Some("BrowserWindow.startDrag()"),
      )
      .map_err(deno_error::JsErrorBox::from_err)?;
  }
  Ok(())
}

/// `Deno.desktop.dialog.*`: shows the dialog and returns the id the JS side
/// waits on (`op_desktop_file_dialog_wait`) and cancels with
/// (`op_desktop_file_dialog_cancel`). Never blocks.
#[op2]
#[smi]
fn op_desktop_file_dialog_open(
  state: &mut OpState,
  #[serde] request: FileDialogRequest,
) -> Result<u32, deno_error::JsErrorBox> {
  reject_nul_in_file_dialog(&request)?;
  let (dialog_id, outcome) = match state.try_borrow::<Arc<dyn DesktopApi>>() {
    Some(api) => api.show_file_dialog(request),
    None => (
      0,
      Box::pin(async { FileDialogOutcome::Failed })
        as DesktopFuture<FileDialogOutcome>,
    ),
  };
  if !state.has::<FileDialogTable>() {
    state.put(FileDialogTable::default());
  }
  Ok(
    state
      .borrow_mut::<FileDialogTable>()
      .insert(dialog_id, outcome),
  )
}

/// The outcome of a dialog `op_desktop_file_dialog_open` showed.
#[op2]
#[serde]
async fn op_desktop_file_dialog_wait(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[smi] rid: u32,
) -> FileDialogResultInfo {
  let outcome = state
    .borrow_mut()
    .try_borrow_mut::<FileDialogTable>()
    .and_then(|t| t.take_outcome(rid));
  let result = match outcome {
    Some(f) => f.await,
    None => FileDialogOutcome::Failed,
  };
  if let Some(t) = state.borrow_mut().try_borrow_mut::<FileDialogTable>() {
    t.remove(rid);
  }
  result.into()
}

/// `Deno.desktop.shortcuts.capabilities()` and the launch-at-login /
/// DevTools availability (laufey API 40).
#[op2]
#[serde]
fn op_desktop_system_capabilities(
  state: &mut OpState,
) -> SystemCapabilitiesInfo {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.system_capabilities())
    .unwrap_or_default()
}

/// `Deno.desktop.platformFeatures()` (laufey API 45): the backend's probe of
/// this session, or `null` outside a desktop app (or from a backend that
/// can't say). Async: the probe runs on a blocking-pool thread, never on the
/// JavaScript thread (its first call on Linux may wait seconds for
/// xdg-desktop-portal to start). `desktopHint` (XDG_CURRENT_DESKTOP) is
/// reported only with env access to it (`--allow-env`); without, it is null,
/// the reasons don't quote it, and nothing KWallet-specific is reported
/// ([`redact_desktop_hint`]).
#[op2]
#[serde]
async fn op_desktop_platform_features(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<serde_json::Value> {
  let (api, env_allowed) = {
    let state = state.borrow();
    (
      state.try_borrow::<Arc<dyn DesktopApi>>().cloned(),
      desktop_hint_allowed(&state),
    )
  };
  let api = api?;
  let json = deno_core::unsync::spawn_blocking(move || api.platform_features())
    .await
    .ok()??;
  let mut features: serde_json::Value = serde_json::from_str(&json).ok()?;
  if !env_allowed {
    redact_desktop_hint(&mut features);
  }
  Some(features)
}

/// A `Deno.desktop.secureStore` call (laufey API 47).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRequest {
  /// "get", "set" or "delete".
  pub op: String,
  pub service: String,
  pub account: String,
  /// "set": the secret (text).
  #[serde(default)]
  pub value: Option<String>,
  /// "set": what a keyring manager shows (the service when absent).
  #[serde(default)]
  pub label: Option<String>,
  /// The bound on an unlock prompt nobody answers (the backend's default,
  /// 20 s, when absent).
  #[serde(default)]
  pub timeout_ms: Option<u32>,
}

/// What a secure-store call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOutcome {
  /// get: the value, or `None` (not found); set / delete: `None`.
  Ok(Option<String>),
  /// The store can't answer (no provider, a locked keyring no one unlocked,
  /// no session bus; on macOS a locked keychain, access refused, another
  /// program's item in the way): why, and what to do.
  Unavailable(String),
  /// Bad arguments.
  Invalid(String),
  /// No secure store in this backend.
  Unsupported,
}

/// The wire shape of a [`SecretOutcome`].
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretResultInfo {
  /// "ok", "unavailable", "invalid" or "unsupported".
  pub status: &'static str,
  pub value: Option<String>,
  pub reason: Option<String>,
}

impl From<SecretOutcome> for SecretResultInfo {
  fn from(o: SecretOutcome) -> Self {
    let (status, value, reason) = match o {
      SecretOutcome::Ok(v) => ("ok", v, None),
      SecretOutcome::Unavailable(r) => ("unavailable", None, Some(r)),
      SecretOutcome::Invalid(r) => ("invalid", None, Some(r)),
      SecretOutcome::Unsupported => ("unsupported", None, None),
    };
    SecretResultInfo {
      status,
      value,
      reason,
    }
  }
}

/// `Deno.desktop.secureStore.supported` (laufey API 47).
#[op2(fast)]
fn op_desktop_secret_supported(state: &OpState) -> bool {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .is_some_and(|api| api.secret_store_supported())
}

/// `Deno.desktop.secureStore.get / set / delete` (laufey API 47): the OS's
/// secret store, which every app of the user shares, so it needs unscoped
/// `--allow-sys` like the other integrations that reach past the app's own
/// windows ([`check_desktop_integration`]). The call blocks for as long as
/// an unlock prompt may stay up (bounded by `timeoutMs`), so it runs on the
/// blocking pool.
#[op2]
#[serde]
async fn op_desktop_secret_request(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[serde] request: SecretRequest,
) -> Result<SecretResultInfo, deno_error::JsErrorBox> {
  let api = {
    let s = state.borrow();
    check_desktop_integration(&s)?;
    s.try_borrow::<Arc<dyn DesktopApi>>().cloned()
  };
  if !matches!(request.op.as_str(), "get" | "set" | "delete") {
    return Ok(
      SecretOutcome::Invalid(format!("unknown op {}", request.op)).into(),
    );
  }
  let Some(api) = api else {
    return Ok(SecretOutcome::Unsupported.into());
  };
  let outcome =
    deno_core::unsync::spawn_blocking(move || api.secret_request(&request))
      .await
      .unwrap_or_else(|_| {
        SecretOutcome::Unavailable("the secure store failed".to_string())
      });
  Ok(outcome.into())
}

/// `Deno.desktop.titleBarPreferences()` (laufey API 47): how the user set
/// up title bars, for an app that draws its own; `null` outside a desktop app
/// (or from a backend that can't say). Async: on Linux the first call may
/// wait for xdg-desktop-portal to start, so it runs on a blocking-pool
/// thread.
#[op2]
#[serde]
async fn op_desktop_title_bar_preferences(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Option<serde_json::Value> {
  let api = state
    .borrow()
    .try_borrow::<Arc<dyn DesktopApi>>()
    .cloned()?;
  let json =
    deno_core::unsync::spawn_blocking(move || api.title_bar_preferences())
      .await
      .ok()??;
  serde_json::from_str(&json).ok()
}

/// Whether the page may see XDG_CURRENT_DESKTOP (env access to it).
fn desktop_hint_allowed(state: &OpState) -> bool {
  state
    .try_borrow::<deno_permissions::PermissionsContainer>()
    .is_some_and(|p| {
      p.query_env(Some("XDG_CURRENT_DESKTOP"))
        == deno_permissions::PermissionState::Granted
    })
}

/// A reason without the desktop's name: laufey names it only in a
/// ` (XDG_CURRENT_DESKTOP=…)` part that is always the reason's last (with
/// any wording that names the desktop inside it), so the reason is cut
/// there. Never by matching the closing parenthesis: the desktop's name can
/// contain one.
fn strip_desktop_hint(reason: &str) -> String {
  const MARK: &str = " (XDG_CURRENT_DESKTOP=";
  match reason.find(MARK) {
    Some(start) => reason[..start].to_string(),
    None => reason.to_string(),
  }
}

/// `cookieEncryptionWait` without a desktop-specific cause: what a KWallet
/// reason becomes when the page may not see the desktop.
const NEUTRAL_COOKIE_ENCRYPTION_WAIT: &str =
  "the system keyring can't hand out the key in this session";

/// `desktopHint` null, and no reason quoting it. `kwallet` is null too and a
/// KWallet `cookieEncryptionWait` reason is neutral: KWallet is reported only
/// where Chromium's own rule calls the desktop KDE, so either would name it.
fn redact_desktop_hint(features: &mut serde_json::Value) {
  let Some(obj) = features.as_object_mut() else {
    return;
  };
  for key in ["desktopHint", "kwallet"] {
    if obj.contains_key(key) {
      obj.insert(key.into(), serde_json::Value::Null);
    }
  }
  for key in ["trayReason", "notificationReason"] {
    if let Some(serde_json::Value::String(reason)) = obj.get_mut(key) {
      *reason = strip_desktop_hint(reason);
    }
  }
  if let Some(serde_json::Value::String(reason)) =
    obj.get_mut("cookieEncryptionWait")
  {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("kwallet") {
      *reason = NEUTRAL_COOKIE_ENCRYPTION_WAIT.to_string();
    } else {
      *reason = strip_desktop_hint(reason);
    }
  }
}

/// Why `new Deno.Tray()` got no icon, from the backend's tray reason
/// (Linux: no tray host, or no appindicator library) when there is one.
fn tray_unavailable_message(reason: Option<&str>, env_allowed: bool) -> String {
  match reason {
    Some(reason) if env_allowed => {
      format!("Tray icons are not available here: {reason}")
    }
    Some(reason) => format!(
      "Tray icons are not available here: {}",
      strip_desktop_hint(reason)
    ),
    None => "Tray icons are not available here".to_string(),
  }
}

/// Longest accelerator string accepted (laufey's parser takes 128 bytes).
const MAX_ACCELERATOR_LEN: usize = 128;

/// `Deno.desktop.shortcuts.register()`. Never blocks: on Wayland the answer
/// waits for the user to approve the shortcut in the desktop's dialog.
#[op2]
#[serde]
async fn op_desktop_register_shortcut(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  #[string] accelerator: String,
) -> Result<ShortcutRegisterInfo, deno_error::JsErrorBox> {
  check_desktop_integration(&state.borrow())?;
  if accelerator.is_empty()
    || accelerator.len() > MAX_ACCELERATOR_LEN
    || accelerator.contains('\0')
  {
    return Ok(ShortcutRegisterInfo::err("invalid"));
  }
  Ok(match desktop_api(&state) {
    Some(api) => api.register_shortcut(&accelerator).await,
    None => ShortcutRegisterInfo::err("not_supported"),
  })
}

/// `Deno.desktop.shortcuts.unregister()`.
#[op2(fast)]
fn op_desktop_unregister_shortcut(
  state: &mut OpState,
  #[string] accelerator: &str,
) -> bool {
  if accelerator.contains('\0') {
    return false;
  }
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.unregister_shortcut(accelerator))
    .unwrap_or(false)
}

/// `Deno.desktop.shortcuts.unregisterAll()`.
#[op2(fast)]
fn op_desktop_unregister_all_shortcuts(state: &mut OpState) {
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.unregister_all_shortcuts();
  }
}

/// `Deno.desktop.shortcuts.list()`.
#[op2]
#[serde]
fn op_desktop_list_shortcuts(state: &mut OpState) -> Vec<String> {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.list_shortcuts())
    .unwrap_or_default()
}

/// `Deno.desktop.shortcuts.canonicalize()`: `null` when it doesn't parse.
#[op2]
#[string]
fn op_desktop_canonical_accelerator(
  state: &mut OpState,
  #[string] accelerator: &str,
) -> Option<String> {
  if accelerator.len() > MAX_ACCELERATOR_LEN || accelerator.contains('\0') {
    return None;
  }
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .and_then(|api| api.canonical_accelerator(accelerator))
}

/// `Deno.desktop.launchAtLogin.get()`.
#[op2]
#[string]
async fn op_desktop_get_launch_at_login(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Result<String, deno_error::JsErrorBox> {
  let Some(api) = desktop_api(&state) else {
    return Ok("not-supported".to_string());
  };
  // SMAppService and the registry are quick, but they are system calls the
  // event loop shouldn't wait on.
  deno_core::unsync::spawn_blocking(move || api.launch_at_login())
    .await
    .map(|s| s.to_string())
    .map_err(|_| {
      deno_error::JsErrorBox::generic("reading launch at login failed")
    })
}

/// `Deno.desktop.launchAtLogin.set()`: the state afterwards.
#[op2]
#[string]
async fn op_desktop_set_launch_at_login(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
  enabled: bool,
) -> Result<String, deno_error::JsErrorBox> {
  check_desktop_integration(&state.borrow())?;
  let Some(api) = desktop_api(&state) else {
    return Ok("not-supported".to_string());
  };
  match deno_core::unsync::spawn_blocking(move || {
    api.set_launch_at_login(enabled)
  })
  .await
  {
    Ok(Ok(state)) => Ok(state.to_string()),
    Ok(Err(message)) => Err(deno_error::JsErrorBox::generic(message)),
    Err(_) => Err(deno_error::JsErrorBox::generic(
      "changing launch at login failed",
    )),
  }
}

/// `Deno.desktop.devtools.enabled` (window 0: the launch setting) and
/// `BrowserWindow.isDevtoolsEnabled()`.
#[op2(fast)]
fn op_desktop_devtools_enabled(
  state: &mut OpState,
  #[smi] window_id: u32,
) -> bool {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.devtools_enabled(window_id))
    .unwrap_or(false)
}

/// Close a dialog `op_desktop_file_dialog_open` showed, as cancelled (an
/// AbortSignal). False when it is no longer open.
#[op2(fast)]
fn op_desktop_file_dialog_cancel(state: &mut OpState, #[smi] rid: u32) -> bool {
  let Some(dialog_id) = state
    .try_borrow::<FileDialogTable>()
    .and_then(|t| t.dialog_id(rid))
  else {
    return false;
  };
  if dialog_id == 0 {
    return false;
  }
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.cancel_file_dialog(dialog_id))
    .unwrap_or(false)
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
  provisional: bool,
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
  let cb: Box<dyn FnOnce(PermissionState) + Send> = Box::new(move |state| {
    let _ = tx.send(state);
  });
  // On a blocking-pool thread, never the JavaScript thread (the answer
  // comes through `cb`; the task's handle is dropped): a backend may answer
  // inline after starting a notification server over D-Bus (Winit on
  // Linux), which can take seconds.
  drop(deno_core::unsync::spawn_blocking(move || {
    if provisional {
      api.request_provisional_notification_permission(cb);
    } else {
      api.request_notification_permission(cb);
    }
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
  // Off the JavaScript thread, as for the request above.
  drop(deno_core::unsync::spawn_blocking(move || {
    api.query_notification_permission(Box::new(move |state| {
      let _ = tx.send(state);
    }));
  }));
  permission_state_to_web_string(
    rx.await.unwrap_or(PermissionState::Unsupported),
  )
  .to_string()
}

/// `Deno.desktop.menuCapabilities()` (laufey API 41).
#[op2]
#[serde]
fn op_desktop_menu_capabilities(state: &mut OpState) -> MenuCapabilitiesInfo {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.menu_capabilities())
    .unwrap_or_default()
}

/// `Deno.desktop.notifications.capabilities()` (laufey API 41).
#[op2]
#[serde]
fn op_desktop_notification_capabilities(
  state: &mut OpState,
) -> NotificationCapabilitiesInfo {
  state
    .try_borrow::<Arc<dyn DesktopApi>>()
    .map(|api| api.notification_capabilities())
    .unwrap_or_default()
}

/// Checks a tag and data against laufey's limits.
fn check_notification_ids(
  tag: Option<&str>,
  data: Option<&str>,
) -> Result<(), deno_error::JsErrorBox> {
  if tag.is_some_and(|t| t.is_empty() || t.len() > MAX_NOTIFICATION_TAG_BYTES) {
    return Err(deno_error::JsErrorBox::type_error(format!(
      "a notification tag must be 1 to {MAX_NOTIFICATION_TAG_BYTES} bytes"
    )));
  }
  if data.is_some_and(|d| d.len() > MAX_NOTIFICATION_DATA_BYTES) {
    return Err(deno_error::JsErrorBox::type_error(format!(
      "notification data must serialize to at most \
       {MAX_NOTIFICATION_DATA_BYTES} bytes of JSON"
    )));
  }
  Ok(())
}

/// `Deno.desktop.notifications.schedule(options)` (laufey API 41): true
/// when the backend took it.
#[op2]
fn op_desktop_schedule_notification(
  state: &mut OpState,
  #[serde] options: NotificationScheduleOptions,
  #[buffer] icon: Option<&[u8]>,
) -> Result<bool, deno_error::JsErrorBox> {
  check_desktop_integration(state)?;
  check_notification_ids(Some(&options.tag), options.data.as_deref())?;
  if options.title.is_empty() {
    return Err(deno_error::JsErrorBox::type_error(
      "a notification needs a title",
    ));
  }
  if !options.at.is_finite() || options.at < 0.0 || options.at > 8.64e15 {
    return Err(deno_error::JsErrorBox::type_error(
      "the notification time is not a valid date",
    ));
  }
  let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() else {
    return Ok(false);
  };
  let request = NotificationRequest {
    title: options.title,
    body: options.body,
    icon: icon.map(|i| i.to_vec()),
    tag: Some(options.tag),
    silent: options.silent,
    require_interaction: options.require_interaction,
    actions: options.actions,
    data: options.data,
    // A time in the past shows it now.
    schedule_at_ms: Some((options.at as i64).max(1)),
  };
  reject_nul_in_notification(&request)?;
  Ok(api.schedule_notification(&request))
}

/// `Deno.desktop.notifications.getScheduled()` (laufey API 41).
#[op2]
#[serde]
async fn op_desktop_list_scheduled_notifications(
  state: std::rc::Rc<std::cell::RefCell<OpState>>,
) -> Vec<ScheduledNotificationInfo> {
  match desktop_api(&state) {
    Some(api) => api.list_scheduled_notifications().await,
    None => Vec::new(),
  }
}

/// `Deno.desktop.notifications.cancel(tag)` (laufey API 41).
#[op2(fast)]
fn op_desktop_cancel_notification(state: &mut OpState, #[string] tag: &str) {
  if tag.is_empty()
    || tag.len() > MAX_NOTIFICATION_TAG_BYTES
    || tag.contains('\0')
  {
    return;
  }
  if let Some(api) = state.try_borrow::<Arc<dyn DesktopApi>>() {
    api.cancel_notification(tag);
  }
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
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (api, brand, set_event_target_data) =
      class_prerequisites(state, "Dock")?;

    let dock = Dock { api };
    let dock = deno_core::cppgc::make_cppgc_object(scope, dock);
    init_event_target(scope, dock, &brand, &set_event_target_data);
    let dock = dock.cast::<v8::Value>();

    Ok(v8::Global::new(scope, dock))
  }

  // `null` / `undefined` clear the badge, as the d.ts says (a required
  // string turned `null` into the text "null").
  fn set_badge(
    &self,
    #[string] text: Option<String>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_opt("the badge text", text.as_deref())?;
    self.api.set_dock_badge(dock_badge_text(text.as_deref()));
    Ok(())
  }

  #[fast]
  fn bounce(&self, critical: bool) {
    self.api.bounce_dock(critical);
  }

  fn set_menu(
    &self,
    #[serde] menu: Option<Vec<MenuItem>>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_in_menu(menu.as_deref().unwrap_or_default())?;
    self.api.set_dock_menu(menu);
    Ok(())
  }

  #[fast]
  fn set_visible(&self, visible: bool) {
    self.api.set_dock_visible(visible);
  }
}

/// What `Dock.setBadge(text)` hands `DesktopApi::set_dock_badge`: the text,
/// or "" (which clears) for `null` / `undefined`.
fn dock_badge_text(text: Option<&str>) -> &str {
  text.unwrap_or("")
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
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (api, brand, set_event_target_data) =
      class_prerequisites(state, "Tray")?;

    let tray_id = api.create_tray();
    if tray_id == 0 {
      // No icon could be shown (Linux with no tray host, as on stock GNOME,
      // or no appindicator library): refuse instead of a dead Tray.
      // The tray part of the probe only: never the portal, which can take
      // seconds on this (the JavaScript) thread.
      return Err(JsErrorBox::new(
        "NotSupported",
        tray_unavailable_message(
          api.tray_unavailable_reason().as_deref(),
          desktop_hint_allowed(state),
        ),
      ));
    }
    let tray = Tray { api, tray_id };
    let tray = deno_core::cppgc::make_cppgc_object(scope, tray);
    init_event_target(scope, tray, &brand, &set_event_target_data);
    let tray = tray.cast::<v8::Value>();

    Ok(v8::Global::new(scope, tray))
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

  fn set_tooltip(
    &self,
    #[string] text: Option<String>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_opt("the tooltip", text.as_deref())?;
    self.api.set_tray_tooltip(self.tray_id, text.as_deref());
    Ok(())
  }

  fn set_menu(
    &self,
    #[serde] menu: Option<Vec<MenuItem>>,
  ) -> Result<(), deno_error::JsErrorBox> {
    reject_nul_in_menu(menu.as_deref().unwrap_or_default())?;
    self.api.set_tray_menu(self.tray_id, menu);
    Ok(())
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

/// The options `new Notification()` normalizes in JS (laufey API 41): the
/// action buttons, and `data` as JSON text for the OS to hand back.
#[derive(Debug, Default, serde::Deserialize)]
struct NotificationExtra {
  #[serde(default)]
  actions: Vec<NotificationActionInfo>,
  data: Option<String>,
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
    #[serde] extra: Option<NotificationExtra>,
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (api, brand, set_event_target_data) =
      class_prerequisites(state, "Notification")?;
    check_desktop_integration(state)?;

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

    let extra = extra.unwrap_or_default();
    let request = NotificationRequest {
      title: title.clone(),
      body: options.body.clone(),
      icon: icon_bytes.map(|b| b.to_vec()),
      tag: options.tag.clone().filter(|t| !t.is_empty()),
      silent: options.silent,
      require_interaction: options.require_interaction,
      actions: extra.actions,
      data: extra.data,
      schedule_at_ms: None,
    };
    reject_nul_in_notification(&request)?;
    // Out-of-range tags / data show nothing (an "error" event, as for any
    // notification the backend refuses).
    let notification_id = if check_notification_ids(
      request.tag.as_deref(),
      request.data.as_deref(),
    )
    .is_err()
    {
      0
    } else {
      api.show_notification(&request)
    };

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
    init_event_target(scope, notification, &brand, &set_event_target_data);
    let notification = notification.cast::<v8::Value>();

    Ok(v8::Global::new(scope, notification))
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
    op_desktop_auth_session_capabilities,
    op_desktop_auth_session_start,
    op_desktop_auth_session_cancel,
    op_desktop_run_on_main_thread,
    op_desktop_resolve_bind_call,
    op_desktop_reject_bind_call,
    op_desktop_alert,
    op_desktop_alert_async,
    op_desktop_confirm,
    op_desktop_prompt,
    op_desktop_read_clipboard_text,
    op_desktop_write_clipboard_text,
    op_desktop_clipboard_capabilities,
    op_desktop_read_clipboard_html,
    op_desktop_write_clipboard_html,
    op_desktop_read_clipboard_image,
    op_desktop_write_clipboard_image,
    op_desktop_read_clipboard_formats,
    op_desktop_clipboard_watch,
    op_desktop_start_drag,
    op_desktop_file_dialog_open,
    op_desktop_file_dialog_wait,
    op_desktop_file_dialog_cancel,
    op_desktop_system_capabilities,
    op_desktop_platform_features,
    op_desktop_title_bar_preferences,
    op_desktop_secret_supported,
    op_desktop_secret_request,
    op_desktop_register_shortcut,
    op_desktop_unregister_shortcut,
    op_desktop_unregister_all_shortcuts,
    op_desktop_list_shortcuts,
    op_desktop_canonical_accelerator,
    op_desktop_get_launch_at_login,
    op_desktop_set_launch_at_login,
    op_desktop_devtools_enabled,
    op_desktop_menu_capabilities,
    op_desktop_notification_capabilities,
    op_desktop_schedule_notification,
    op_desktop_list_scheduled_notifications,
    op_desktop_cancel_notification,
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

  #[test]
  fn tray_unavailable_message_carries_the_reason() {
    use super::tray_unavailable_message;
    // Linux with no tray host: the backend's tray reason is the error's.
    let reason = "no tray host (StatusNotifierWatcher) on this session; some \
                  desktops need an extension (XDG_CURRENT_DESKTOP=GNOME; \
                  GNOME shows tray icons only with the AppIndicator \
                  extension enabled)";
    assert_eq!(
      tray_unavailable_message(Some(reason), true),
      format!("Tray icons are not available here: {reason}")
    );
    // Without env access no desktop is named: neutral wording.
    let message = tray_unavailable_message(Some(reason), false);
    assert_eq!(
      message,
      "Tray icons are not available here: no tray host \
       (StatusNotifierWatcher) on this session; some desktops need an \
       extension"
    );
    assert!(!message.contains("GNOME"));
    // No reason (an older backend): still a clear refusal.
    assert_eq!(
      tray_unavailable_message(None, true),
      "Tray icons are not available here"
    );
  }

  #[test]
  fn desktop_hint_is_redacted_without_env_access() {
    use super::redact_desktop_hint;
    use super::strip_desktop_hint;
    let mut features = json!({
      "os": "linux",
      "desktopHint": "sway",
      "trayReason": null,
      "notificationReason": "no notification server: nothing owns \
        org.freedesktop.Notifications on the session bus \
        (XDG_CURRENT_DESKTOP=sway)",
    });
    redact_desktop_hint(&mut features);
    assert_eq!(features["desktopHint"], serde_json::Value::Null);
    assert_eq!(features["trayReason"], serde_json::Value::Null);
    assert!(!features.to_string().contains("sway"));
    assert_eq!(strip_desktop_hint("a (XDG_CURRENT_DESKTOP=KDE)"), "a");
    assert_eq!(strip_desktop_hint("no hint"), "no hint");
    assert_eq!(strip_desktop_hint("cut (XDG_CURRENT_DESKTOP=x"), "cut");
    // A `)` inside the desktop's name hides nothing after it.
    assert_eq!(
      strip_desktop_hint("a (XDG_CURRENT_DESKTOP=K)leak:GNOME; GNOME advice)"),
      "a"
    );
    let mut features = json!({
      "desktopHint": "x)y",
      "trayReason": "no tray host (StatusNotifierWatcher) on this session; \
        some desktops need an extension (XDG_CURRENT_DESKTOP=x)y)",
    });
    redact_desktop_hint(&mut features);
    assert!(!features.to_string().contains("y)"));
  }

  #[test]
  fn kwallet_is_redacted_without_env_access() {
    use super::NEUTRAL_COOKIE_ENCRYPTION_WAIT;
    use super::redact_desktop_hint;
    let mut features = json!({
      "desktopHint": "KDE",
      "kwallet": "closed",
      "cookieEncryption": "os",
      "cookieEncryptionWait": "the cookie store uses KWallet here, and its \
        wallet is closed: a request for its key is never answered",
    });
    redact_desktop_hint(&mut features);
    assert_eq!(features["kwallet"], serde_json::Value::Null);
    assert_eq!(
      features["cookieEncryptionWait"],
      NEUTRAL_COOKIE_ENCRYPTION_WAIT
    );
    assert_eq!(features["cookieEncryption"], "os");
    // No value names KWallet or KDE (the `kwallet` key itself stays).
    for value in features.as_object().unwrap().values() {
      let text = value.to_string().to_ascii_lowercase();
      assert!(!text.contains("kwallet") && !text.contains("kde"), "{text}");
    }
    // kwalletd's own wording is neutral too.
    let mut features = json!({
      "kwallet": "not-running",
      "cookieEncryptionWait": "the cookie store uses KWallet here, and \
        kwalletd is not running: a request for its key may never be answered",
    });
    redact_desktop_hint(&mut features);
    assert_eq!(
      features["cookieEncryptionWait"],
      NEUTRAL_COOKIE_ENCRYPTION_WAIT
    );
    // A Secret Service reason names no desktop: kept; no wait stays null.
    let secret = "the Secret Service's default keyring is locked and no one \
      can answer its unlock prompt in this unknown session";
    let mut features = json!({
      "kwallet": null,
      "cookieEncryptionWait": secret,
    });
    redact_desktop_hint(&mut features);
    assert_eq!(features["kwallet"], serde_json::Value::Null);
    assert_eq!(features["cookieEncryptionWait"], secret);
    let mut features = json!({ "cookieEncryptionWait": null });
    redact_desktop_hint(&mut features);
    assert_eq!(features["cookieEncryptionWait"], serde_json::Value::Null);
    assert!(features.get("kwallet").is_none());
  }

  #[test]
  fn an_unshown_dialog_is_not_supported() {
    use deno_error::JsErrorClass;
    let err = super::dialog_unsupported_error("prompt()");
    assert_eq!(err.get_class(), "NotSupported");
    assert!(err.get_message().contains("prompt() can't be shown here"));
  }

  use super::AUTH_SESSION_NOT_SUPPORTED_MESSAGE;
  use super::AuthSessionCapabilitiesInfo;
  use super::AuthSessionOutcome;
  use super::BrowserWindow;
  use super::DesktopEvent;
  use super::DesktopValue;
  use super::MenuItem;
  use super::PASSKEY_NOT_SUPPORTED_ENVELOPE;
  use super::PasskeyCapabilitiesInfo;
  use super::PendingBindResponses;
  use super::PermissionState;
  use super::Tray;
  use super::dock_badge_text;
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
  #[allow(clippy::disallowed_methods, reason = "test fixtures on disk")]
  fn drag_out_needs_read_permission_for_every_path() {
    use std::sync::Arc;

    use deno_permissions::Permissions;
    use deno_permissions::PermissionsContainer;
    use deno_permissions::PermissionsOptions;
    use deno_permissions::RuntimePermissionDescriptorParser;

    let tmp = tempfile::tempdir().unwrap();
    let allowed = tmp.path().join("allowed");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&allowed).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(allowed.join("a.txt"), "a").unwrap();
    std::fs::write(other.join("b.txt"), "b").unwrap();
    let parser =
      RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
    let perms = Permissions::from_options(
      &parser,
      &PermissionsOptions {
        allow_read: Some(vec![allowed.to_string_lossy().into_owned()]),
        ..Default::default()
      },
    )
    .unwrap();
    let mut state = deno_core::OpState::new(None);
    state.put(PermissionsContainer::new(Arc::new(parser), perms));
    let path = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    assert!(
      super::check_drag_read_permission(&state, &[path(allowed.join("a.txt"))])
        .is_ok()
    );
    // One unreadable path refuses the whole drag.
    assert!(
      super::check_drag_read_permission(
        &state,
        &[path(allowed.join("a.txt")), path(other.join("b.txt"))]
      )
      .is_err()
    );
  }

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
    assert_eq!(
      serde_json::to_value(DesktopEvent::PlatformFeaturesChanged).unwrap(),
      json!({ "kind": "platformFeaturesChanged" })
    );
    assert_eq!(
      serde_json::to_value(DesktopEvent::TitleBarPreferencesChanged).unwrap(),
      json!({ "kind": "titleBarPreferencesChanged" })
    );
    // laufey API 47: the secure store's answers.
    let wire =
      |o| serde_json::to_value(super::SecretResultInfo::from(o)).unwrap();
    assert_eq!(
      wire(super::SecretOutcome::Ok(Some("s".into()))),
      json!({ "status": "ok", "value": "s", "reason": null })
    );
    assert_eq!(
      wire(super::SecretOutcome::Ok(None)),
      json!({ "status": "ok", "value": null, "reason": null })
    );
    assert_eq!(
      wire(super::SecretOutcome::Unavailable("locked".into())),
      json!({ "status": "unavailable", "value": null, "reason": "locked" })
    );
    assert_eq!(
      wire(super::SecretOutcome::Unsupported),
      json!({ "status": "unsupported", "value": null, "reason": null })
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
    use super::CloseTimeout;
    use super::PendingCloses;
    let t0 = std::time::Instant::now();
    let after = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    let p = PendingCloses::default();
    // Not canceled: close.
    p.begin_at(1, t0);
    assert_eq!(p.reply(1, false), CloseDecision::Close);
    // Answered already: the timeout does nothing.
    assert_eq!(p.reply(1, false), CloseDecision::Ignore);
    // Canceled: keep, and the timeout must not close it later.
    let token = p.begin_at(2, t0);
    assert_eq!(p.reply(2, true), CloseDecision::Keep);
    assert_eq!(p.check_timeout_at(2, token, after(5)), CloseTimeout::Ignore);
    // Never answered: the timeout closes it, not before.
    let token = p.begin_at(3, t0);
    assert_eq!(
      p.check_timeout_at(3, token, after(2)),
      CloseTimeout::Wait(std::time::Duration::from_secs(3))
    );
    assert_eq!(p.check_timeout_at(3, token, after(5)), CloseTimeout::Close);
    assert_eq!(p.reply(3, false), CloseDecision::Ignore);
    // A stale timer from an earlier request leaves a newer one pending.
    let old = p.begin_at(4, t0);
    assert_eq!(p.reply(4, true), CloseDecision::Keep);
    let new = p.begin_at(4, t0);
    assert_eq!(p.check_timeout_at(4, old, after(5)), CloseTimeout::Ignore);
    assert_eq!(p.check_timeout_at(4, new, after(5)), CloseTimeout::Close);
    // close() settles a pending request.
    let token = p.begin_at(5, t0);
    p.forget(5);
    assert_eq!(p.check_timeout_at(5, token, after(5)), CloseTimeout::Ignore);
    assert_eq!(p.reply(5, false), CloseDecision::Ignore);
    assert_eq!(
      super::CLOSE_REPLY_TIMEOUT,
      std::time::Duration::from_secs(5)
    );
  }

  #[test]
  fn pending_closes_do_not_count_sync_dialog_time() {
    use super::CloseDecision;
    use super::CloseTimeout;
    use super::PendingCloses;
    let t0 = std::time::Instant::now();
    let after = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    let p = PendingCloses::default();
    // A close listener that asks `confirm("Discard changes?")` 1 s in, and
    // the user takes 30 s to answer: the window must not close under the
    // open dialog (it used to close at 5 s).
    let token = p.begin_at(1, t0);
    p.dialog_began_at(after(1));
    assert!(matches!(
      p.check_timeout_at(1, token, after(5)),
      CloseTimeout::Wait(_)
    ));
    assert!(matches!(
      p.check_timeout_at(1, token, after(30)),
      CloseTimeout::Wait(_)
    ));
    p.dialog_ended_at(after(31));
    // 1 s before the dialog counted; 4 s are left after it.
    assert_eq!(
      p.check_timeout_at(1, token, after(31)),
      CloseTimeout::Wait(std::time::Duration::from_secs(4))
    );
    // The listener answers right after the dialog: kept.
    assert_eq!(p.reply(1, true), CloseDecision::Keep);
    // A listener that never answers still closes, 5 s of counted time after
    // its request (here: 2 s, a 10 s dialog, 3 s).
    let token = p.begin_at(2, after(40));
    p.dialog_began_at(after(42));
    p.dialog_ended_at(after(52));
    assert!(matches!(
      p.check_timeout_at(2, token, after(54)),
      CloseTimeout::Wait(_)
    ));
    assert_eq!(p.check_timeout_at(2, token, after(55)), CloseTimeout::Close);
    // Nested dialogs pause until the outermost ends.
    let token = p.begin_at(3, after(60));
    p.dialog_began_at(after(60));
    p.dialog_began_at(after(61));
    p.dialog_ended_at(after(62));
    assert!(matches!(
      p.check_timeout_at(3, token, after(70)),
      CloseTimeout::Wait(_)
    ));
    p.dialog_ended_at(after(70));
    assert_eq!(p.check_timeout_at(3, token, after(75)), CloseTimeout::Close);
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
        origin: String::new(),
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
  ) -> (super::DesktopLaunchInbox, super::DesktopEventReceiver) {
    let (tx, rx) = super::create_desktop_event_channel();
    (
      super::DesktopLaunchInbox::new(tx.0, launch_urls, launch_files),
      rx,
    )
  }

  fn drain(rx: &mut super::DesktopEventReceiver) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(ev) = rx.0.try_recv() {
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
        notifications: vec![],
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
  fn notification_responses_go_through_the_launch_inbox() {
    // The click that launched the app: held, then in the snapshot.
    let (inbox, mut rx) = inbox_with_channel(vec![], vec![]);
    inbox.notification_response(
      "launch-tag".into(),
      Some("open".into()),
      Some("{\"n\":1}".into()),
      true,
    );
    assert!(drain(&mut rx).is_empty());
    let snapshot = inbox.take_launch_targets();
    assert_eq!(
      snapshot.notifications,
      vec![super::NotificationResponseInfo {
        tag: "launch-tag".into(),
        action: Some("open".into()),
        data: Some("{\"n\":1}".into()),
        launch: true,
      }]
    );
    // Not before a listener: held, then handed to the first one.
    inbox.notification_response("later".into(), None, None, false);
    assert!(drain(&mut rx).is_empty());
    let pending = inbox.subscribe("notificationresponse");
    assert_eq!(pending.len(), 1);
    assert_eq!(
      serde_json::to_value(&pending[0]).unwrap(),
      json!({
        "kind": "notificationResponse",
        "tag": "later",
        "action": null,
        "data": null,
        "launch": false,
      })
    );
    // Once JS listens, straight into the channel.
    inbox.notification_response("warm".into(), None, None, false);
    let sent = drain(&mut rx);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("warm"), "{sent:?}");
    // A response that arrives before the snapshot, with a listener absent,
    // is part of the snapshot, not an event.
    let (inbox, mut rx) = inbox_with_channel(vec![], vec![]);
    inbox.notification_response("early".into(), None, None, true);
    assert_eq!(inbox.take_launch_targets().notifications.len(), 1);
    assert!(inbox.subscribe("notificationresponse").is_empty());
    assert!(drain(&mut rx).is_empty());
  }

  #[test]
  fn oversized_notification_responses_are_dropped() {
    let (inbox, mut rx) = inbox_with_channel(vec![], vec![]);
    let _ = inbox.subscribe("notificationresponse");
    inbox.notification_response("t".repeat(257), None, None, false);
    inbox.notification_response(
      "t".into(),
      None,
      Some("d".repeat(4097)),
      false,
    );
    inbox.notification_response(
      "t".into(),
      Some("a".repeat(1025)),
      None,
      false,
    );
    assert!(drain(&mut rx).is_empty());
    // At the limits: delivered.
    inbox.notification_response(
      "t".repeat(256),
      Some("a".repeat(1024)),
      Some("d".repeat(4096)),
      false,
    );
    assert_eq!(drain(&mut rx).len(), 1);
  }

  #[test]
  fn menu_notification_wire_format() {
    assert_eq!(
      serde_json::to_value(DesktopEvent::ContextMenuClose { window_id: 3 })
        .unwrap(),
      json!({ "kind": "contextMenuClose", "windowId": 3 })
    );
    assert_eq!(
      serde_json::to_value(DesktopEvent::NotificationAction {
        notification_id: 7,
        action: "reply".into(),
      })
      .unwrap(),
      json!({ "kind": "notificationAction", "notificationId": 7, "action": "reply" })
    );
    assert_eq!(
      serde_json::to_value(super::MenuCapabilitiesInfo {
        accelerators: true,
        context_closed: true,
        ..Default::default()
      })
      .unwrap(),
      json!({
        "appMenu": false,
        "accelerators": true,
        "contextMenu": false,
        "contextClosed": true,
        "icons": false,
        "tooltips": false,
      })
    );
    assert_eq!(
      serde_json::to_value(super::NotificationCapabilitiesInfo {
        schedule: true,
        cold_start: true,
        ..Default::default()
      })
      .unwrap(),
      json!({
        "show": false,
        "schedule": true,
        "schedulePersists": false,
        "actions": false,
        "clicks": false,
        "coldStart": true,
      })
    );
    assert_eq!(
      serde_json::to_value(super::ScheduledNotificationInfo {
        tag: "t".into(),
        title: "T".into(),
        body: "".into(),
        at: 1700000000123,
        data: None,
        actions: vec![super::NotificationActionInfo {
          action: "a".into(),
          title: "A".into(),
        }],
      })
      .unwrap(),
      json!({
        "tag": "t",
        "title": "T",
        "body": "",
        "at": 1700000000123i64,
        "data": null,
        "actions": [{ "action": "a", "title": "A" }],
      })
    );
    let opts: super::NotificationScheduleOptions =
      serde_json::from_value(json!({
        "title": "T",
        "tag": "x",
        "at": 1700000000123.0,
        "actions": [{ "action": "a", "title": "A" }],
        "data": "{}",
        "requireInteraction": true,
      }))
      .unwrap();
    assert_eq!(opts.tag, "x");
    assert_eq!(opts.at, 1700000000123.0);
    assert_eq!(opts.actions.len(), 1);
    assert_eq!(opts.require_interaction, Some(true));
    assert!(super::check_notification_ids(Some("ok"), Some("{}")).is_ok());
    assert!(super::check_notification_ids(Some(""), None).is_err());
    assert!(
      super::check_notification_ids(
        None,
        Some(&"x".repeat(super::MAX_NOTIFICATION_DATA_BYTES + 1))
      )
      .is_err()
    );
    assert!(
      super::check_notification_ids(
        Some(&"t".repeat(super::MAX_NOTIFICATION_TAG_BYTES + 1)),
        None
      )
      .is_err()
    );
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
      origin: "myapp://app".into(),
    };
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(v["args"][0]["name"], "ada");
    assert_eq!(v["args"][0]["n"], 42);
    assert_eq!(v["callId"], 7);
    assert_eq!(v["windowId"], 1);
    assert_eq!(v["origin"], "myapp://app");
  }

  #[test]
  fn bindings_answer_only_the_documents_they_trust() {
    use super::BindOrigins;
    use super::bind_call_allowed;
    let trusted = vec![
      "myapp://app".to_string(),
      "http://localhost:5173".to_string(),
    ];
    // The app's own origins.
    for origin in ["myapp://app", "http://localhost:5173"] {
      assert!(bind_call_allowed(origin, &trusted, &BindOrigins::App));
    }
    // A page the main frame navigated to, an opaque document, another
    // host on the app's scheme, a different port, no origin at all.
    for origin in [
      "https://evil.example",
      "null",
      "myapp://other",
      "http://localhost:5174",
      "MYAPP://APP",
      "",
    ] {
      assert!(
        !bind_call_allowed(origin, &trusted, &BindOrigins::App),
        "{origin:?}"
      );
    }
    // Opted in.
    let list = BindOrigins::List(vec!["https://idp.example".to_string()]);
    assert!(bind_call_allowed("https://idp.example", &trusted, &list));
    assert!(!bind_call_allowed("https://evil.example", &trusted, &list));
    assert!(!bind_call_allowed("", &trusted, &list));
    assert!(bind_call_allowed(
      "https://evil.example",
      &trusted,
      &BindOrigins::Any
    ));
    assert!(bind_call_allowed("null", &[], &BindOrigins::Any));
    assert!(bind_call_allowed("", &[], &BindOrigins::Any));
  }

  #[test]
  fn bind_origins_are_serialized_like_the_browser() {
    use super::BindOrigins;
    use super::serialize_bind_origin;
    for (input, want) in [
      ("https://Example.COM", Some("https://example.com")),
      ("https://example.com:443", Some("https://example.com")),
      ("http://127.0.0.1:5173", Some("http://127.0.0.1:5173")),
      ("myapp://app", Some("myapp://app")),
      ("null", Some("null")),
      ("https://example.com/", None),
      ("https://example.com/path", None),
      ("https://example.com?x", None),
      ("https://u:p@example.com", None),
      ("not a url", None),
      ("", None),
    ] {
      assert_eq!(serialize_bind_origin(input).as_deref(), want, "{input:?}");
    }
    assert_eq!(BindOrigins::from_spec("").unwrap(), BindOrigins::App);
    assert_eq!(BindOrigins::from_spec("*").unwrap(), BindOrigins::Any);
    assert_eq!(
      BindOrigins::from_spec("HTTPS://IDP.example:443\nnull").unwrap(),
      BindOrigins::List(vec!["https://idp.example".into(), "null".into()])
    );
    for bad in ["x", "https://x/", "https://x\n", "*\nhttps://x"] {
      assert!(BindOrigins::from_spec(bad).is_err(), "{bad:?}");
    }
    let too_many = vec!["https://x"; 65].join("\n");
    assert!(BindOrigins::from_spec(&too_many).is_err());
  }

  #[test]
  fn desktop_values_become_own_data_properties() {
    use deno_core::ToV8;
    use deno_core::v8;
    let mut runtime = deno_core::JsRuntime::new(Default::default());
    runtime
      .execute_script(
        "setup",
        "globalThis.hits = 0;\n\
         for (const proto of [Object.prototype, Array.prototype]) {\n\
           Object.defineProperty(proto, proto === Object.prototype ? 'x' : '0', {\n\
             set() { globalThis.hits++; }, configurable: true,\n\
           });\n\
         }",
      )
      .unwrap();
    let value = DesktopValue::Dict(vec![
      (
        "__proto__".into(),
        DesktopValue::Dict(vec![("polluted".into(), DesktopValue::Bool(true))]),
      ),
      ("x".into(), DesktopValue::Int(1)),
      (
        "list".into(),
        DesktopValue::List(vec![DesktopValue::Int(7)]),
      ),
    ]);
    {
      deno_core::scope!(scope, &mut runtime);
      let v = value.to_v8(scope).unwrap();
      let global = scope.get_current_context().global(scope);
      let key = v8::String::new(scope, "value").unwrap();
      global.set(scope, key.into(), v);
    }
    let out = runtime
      .execute_script(
        "check",
        "JSON.stringify([hits, Object.getPrototypeOf(value) === Object.prototype, \
         Object.hasOwn(value, '__proto__'), value.__proto__ === Object.prototype, \
         value.polluted, Object.hasOwn(value, 'x'), value.x, \
         Object.hasOwn(value.list, '0'), value.list[0], value.list.length])",
      )
      .unwrap();
    deno_core::scope!(scope, &mut runtime);
    let out = v8::Local::new(scope, out).to_rust_string_lossy(scope);
    // No setter ran, the prototype is untouched and `__proto__` is an own
    // data property like any other key.
    assert_eq!(out, "[0,true,true,false,null,true,1,true,7,1]");
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

  // --- Drag and drop, file dialogs, rich clipboard (laufey API 39) ---
  //
  // DESKTOP_JS consumes these shapes: the event fields, the capability keys
  // and the dialog request / result.

  #[test]
  fn file_drop_event_wire_format() {
    let v = serde_json::to_value(DesktopEvent::FileDrop {
      window_id: 3,
      phase: "drop".into(),
      x: 12.5,
      y: 40.0,
      paths: Some(vec!["/a b.txt".into()]),
      count: 1,
    })
    .unwrap();
    assert_eq!(
      v,
      json!({
        "kind": "fileDrop",
        "windowId": 3,
        "phase": "drop",
        "x": 12.5,
        "y": 40.0,
        "paths": ["/a b.txt"],
        "count": 1,
      })
    );
    let leave = serde_json::to_value(DesktopEvent::FileDrop {
      window_id: 3,
      phase: "leave".into(),
      x: 0.0,
      y: 0.0,
      paths: None,
      count: 0,
    })
    .unwrap();
    assert_eq!(leave["paths"], json!(null));
    assert_eq!(
      serde_json::to_value(DesktopEvent::ClipboardChange).unwrap(),
      json!({ "kind": "clipboardChange" })
    );
  }

  #[test]
  fn system_wire_format() {
    assert_eq!(
      serde_json::to_value(DesktopEvent::Shortcut {
        accelerator: "Ctrl+Shift+K".into(),
      })
      .unwrap(),
      json!({ "kind": "shortcut", "accelerator": "Ctrl+Shift+K" })
    );
    let caps = serde_json::to_value(super::SystemCapabilitiesInfo {
      global_shortcuts: true,
      devtools: true,
      ..Default::default()
    })
    .unwrap();
    assert_eq!(
      caps,
      json!({
        "globalShortcuts": true,
        "shortcutsUserBinds": false,
        "launchAtLogin": false,
        "devtools": true,
      })
    );
    assert_eq!(
      serde_json::to_value(super::ShortcutRegisterInfo::ok("Alt+F4".into()))
        .unwrap(),
      json!({ "status": "ok", "accelerator": "Alt+F4" })
    );
    assert_eq!(
      serde_json::to_value(super::ShortcutRegisterInfo::err("conflict"))
        .unwrap(),
      json!({ "status": "conflict", "accelerator": null })
    );
    // The launch-at-login states the JS side and the d.ts know.
    assert_eq!(
      super::LOGIN_ITEM_STATES,
      ["enabled", "disabled", "requires-approval", "not-supported"]
    );
  }

  #[test]
  fn io_capabilities_wire_format() {
    let caps = serde_json::to_value(super::WindowCapabilitiesInfo {
      file_drop: true,
      file_dialog_files_and_directories: true,
      ..Default::default()
    })
    .unwrap();
    assert_eq!(caps["fileDrop"], json!(true));
    assert_eq!(caps["fileDropEnterPaths"], json!(false));
    assert_eq!(caps["fileDragOut"], json!(false));
    assert_eq!(caps["fileDialogs"], json!(false));
    assert_eq!(caps["fileDialogFilesAndDirectories"], json!(true));
    assert_eq!(caps["fileDialogModal"], json!(false));
    let clip = serde_json::to_value(super::ClipboardCapabilitiesInfo {
      text: true,
      change_events: true,
      ..Default::default()
    })
    .unwrap();
    assert_eq!(
      clip,
      json!({
        "text": true,
        "html": false,
        "image": false,
        "formats": false,
        "changeEvents": true,
      })
    );
  }

  #[test]
  fn file_dialog_request_from_js() {
    let r: super::FileDialogRequest = serde_json::from_value(json!({
      "save": false,
      "windowId": 2,
      "title": "Import",
      "defaultPath": null,
      "buttonLabel": null,
      "filters": [{ "name": "Images", "extensions": ["png", "jpg"] }],
      "files": true,
      "directories": false,
      "multiple": true,
      "showHidden": false,
    }))
    .unwrap();
    assert_eq!(r.window_id, 2);
    assert_eq!(r.title.as_deref(), Some("Import"));
    assert_eq!(r.default_path, None);
    assert!(r.files && r.multiple && !r.directories && !r.save);
    assert_eq!(r.filters[0].extensions, vec!["png", "jpg"]);
    // Missing members default.
    let d: super::FileDialogRequest =
      serde_json::from_value(json!({ "save": true })).unwrap();
    assert!(d.save && d.filters.is_empty() && d.window_id == 0);
  }

  #[test]
  fn file_dialog_results() {
    use super::FileDialogOutcome;
    use super::FileDialogResultInfo;
    let accepted: FileDialogResultInfo =
      FileDialogOutcome::Accepted(vec!["/x".into()]).into();
    assert_eq!(
      serde_json::to_value(&accepted).unwrap(),
      json!({ "status": "accepted", "paths": ["/x"] })
    );
    // Accepted with nothing selected reads as cancelled.
    let empty: FileDialogResultInfo =
      FileDialogOutcome::Accepted(vec![]).into();
    assert_eq!(empty.status, "cancelled");
    let busy: FileDialogResultInfo = FileDialogOutcome::Busy.into();
    assert_eq!(busy.status, "busy");
    let failed: FileDialogResultInfo = FileDialogOutcome::Failed.into();
    assert_eq!((failed.status, failed.paths.len()), ("failed", 0));
    assert_eq!(super::DragOutcome::Dropped.as_str(), "dropped");
    assert_eq!(super::DragOutcome::Cancelled.as_str(), "cancelled");
    assert_eq!(super::DragOutcome::Failed.as_str(), "failed");
  }

  #[test]
  fn file_dialog_table_hands_out_each_outcome_once() {
    use super::FileDialogOutcome;
    let mut t = super::FileDialogTable::default();
    let a = t.insert(7, Box::pin(async { FileDialogOutcome::Cancelled }));
    let b = t.insert(0, Box::pin(async { FileDialogOutcome::Busy }));
    assert_ne!(a, b);
    assert_ne!(a, 0);
    assert_eq!(t.dialog_id(a), Some(7));
    assert_eq!(t.dialog_id(b), Some(0));
    assert!(t.take_outcome(a).is_some());
    assert!(t.take_outcome(a).is_none());
    // Still known (for cancel) until removed.
    assert_eq!(t.dialog_id(a), Some(7));
    t.remove(a);
    assert_eq!(t.dialog_id(a), None);
    assert_eq!(t.len(), 1);
  }

  #[test]
  fn png_signature() {
    let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', 0];
    assert!(super::looks_like_png(&png));
    assert!(!super::looks_like_png(&png[..8]));
    assert!(!super::looks_like_png(b"GIF89a-not-a-png"));
  }

  #[test]
  fn auth_session_wire_format() {
    // DESKTOP_JS reads `ok`, `url`, `code`, `message` and the capability
    // names; absent members are left out.
    assert_eq!(
      serde_json::to_value(AuthSessionOutcome::success(
        "myapp://cb?code=1".into()
      ))
      .unwrap(),
      json!({ "ok": true, "url": "myapp://cb?code=1" })
    );
    assert_eq!(
      serde_json::to_value(AuthSessionOutcome::error("cancelled", "closed"))
        .unwrap(),
      json!({ "ok": false, "code": "cancelled", "message": "closed" })
    );
    assert_eq!(
      serde_json::to_value(AuthSessionCapabilitiesInfo {
        supported: true,
        ephemeral: true,
        https_callback: false,
      })
      .unwrap(),
      json!({ "supported": true, "ephemeral": true, "httpsCallback": false })
    );
    assert_eq!(
      serde_json::to_value(AuthSessionCapabilitiesInfo::default()).unwrap(),
      json!({ "supported": false, "ephemeral": false, "httpsCallback": false })
    );
    // The fallback answer points at the system browser, as laufey's does.
    assert!(AUTH_SESSION_NOT_SUPPORTED_MESSAGE.contains("RFC 8252"));
  }

  /// A stand-in for laufey's one-slot auth session: `cancel()` resolves the
  /// running session's future with `cancelled` and frees the slot.
  #[derive(Default)]
  struct OneSlotAuthSession {
    running: std::sync::Mutex<
      Option<tokio::sync::oneshot::Sender<AuthSessionOutcome>>,
    >,
  }

  impl super::DesktopAuthSession for OneSlotAuthSession {
    fn capabilities(&self) -> AuthSessionCapabilitiesInfo {
      AuthSessionCapabilitiesInfo::default()
    }

    fn start(
      &self,
      _window_id: u32,
      _url: String,
      _callback: String,
      _ephemeral: bool,
    ) -> std::pin::Pin<
      Box<dyn std::future::Future<Output = AuthSessionOutcome> + Send>,
    > {
      let (tx, rx) = tokio::sync::oneshot::channel();
      let mut slot = self.running.lock().unwrap();
      if slot.is_some() {
        return Box::pin(async { AuthSessionOutcome::error("busy", "busy") });
      }
      *slot = Some(tx);
      Box::pin(async move {
        rx.await
          .unwrap_or_else(|_| AuthSessionOutcome::error("failed", "x"))
      })
    }

    fn cancel(&self) -> bool {
      match self.running.lock().unwrap().take() {
        Some(tx) => {
          let _ = tx.send(AuthSessionOutcome::error("cancelled", "cancelled"));
          true
        }
        None => false,
      }
    }
  }

  #[tokio::test]
  async fn auth_session_cancel_ends_the_running_session_once() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use deno_core::OpState;

    // No auth session in the runtime (not a desktop app): nothing to cancel.
    let state = Rc::new(RefCell::new(OpState::new(None)));
    assert!(!super::auth_session_cancel(&state));

    let auth = Arc::new(OneSlotAuthSession::default());
    state
      .borrow_mut()
      .put::<Arc<dyn super::DesktopAuthSession>>(auth.clone());
    // Nothing running: false, a no-op.
    assert!(!super::auth_session_cancel(&state));

    let pending = super::DesktopAuthSession::start(
      auth.as_ref(),
      0,
      "https://idp.example/authorize".into(),
      "myapp".into(),
      true,
    );
    assert!(super::auth_session_cancel(&state));
    // Exactly once: the slot is free again.
    assert!(!super::auth_session_cancel(&state));
    assert_eq!(
      pending.await,
      AuthSessionOutcome::error("cancelled", "cancelled")
    );
    // The next session is not busy, and can be cancelled in its turn.
    let next = super::DesktopAuthSession::start(
      auth.as_ref(),
      0,
      "https://idp.example/authorize".into(),
      "myapp".into(),
      true,
    );
    assert!(super::auth_session_cancel(&state));
    assert_eq!(next.await.code.as_deref(), Some("cancelled"));
  }

  /// Every desktop op and class in NOT_IMPORTED_OPS (they survive
  /// `removeImportedOps()`, so any code reaches them through `core.ops`) has
  /// a case in tests/specs/run/desktop_ops_inert, which proves it inert in a
  /// plain `deno run`. A new op must get a case there.
  #[test]
  fn desktop_ops_inert_spec_covers_not_imported_ops() {
    const MAIN_JS: &str = include_str!("../js/99_main.js");
    const SPEC: &str =
      include_str!("../../tests/specs/run/desktop_ops_inert/main.js");
    let start = MAIN_JS.find("const NOT_IMPORTED_OPS = [").unwrap();
    let end = start + MAIN_JS[start..].find("];").unwrap();
    let names: Vec<&str> = MAIN_JS[start..end]
      .split('"')
      .skip(1)
      .step_by(2)
      .filter(|n| {
        n.starts_with("op_desktop_")
          || ["BrowserWindow", "Dock", "Tray", "Notification"].contains(n)
      })
      .collect();
    assert!(names.len() > 60, "{names:?}");
    let missing: Vec<&&str> = names
      .iter()
      .filter(|n| !SPEC.contains(&format!("\n  {n}: ")))
      .collect();
    assert!(
      missing.is_empty(),
      "add a case to tests/specs/run/desktop_ops_inert/main.js for: {missing:?}"
    );
  }

  #[test]
  fn dock_badge_null_clears() {
    // Dock.setBadge(null) / setBadge() clear the badge ("" is the clear
    // value of DesktopApi::set_dock_badge), never show "null".
    assert_eq!(dock_badge_text(None), "");
    assert_eq!(dock_badge_text(Some("")), "");
    assert_eq!(dock_badge_text(Some("3")), "3");
    // The JS method is not a fast call: those can't take an optional
    // string, so null would be coerced to "null".
    let method = super::Dock::DECL
      .methods
      .iter()
      .find(|m| m.name == "setBadge" || m.name == "set_badge")
      .expect("Dock.setBadge");
    assert!(std::panic::catch_unwind(|| method.fast_fn()).is_err());
  }

  #[test]
  fn browser_window_adopts_the_bootstrap_window_only_when_it_fits() {
    use super::BrowserWindowOptions;
    use super::InitialWindowAttributes;
    use super::adopts_initial_window;

    let plain = InitialWindowAttributes::default();
    // No options, or options without creation-time attributes: adopt.
    assert!(adopts_initial_window(&plain, None));
    assert!(adopts_initial_window(
      &plain,
      Some(&BrowserWindowOptions {
        title: Some("x".into()),
        width: Some(300),
        ..Default::default()
      })
    ));
    // Asking for what the bootstrap window already has: adopt.
    assert!(adopts_initial_window(
      &plain,
      Some(&BrowserWindowOptions {
        frameless: Some(false),
        transparent: Some(false),
        ..Default::default()
      })
    ));
    // A tray panel (frameless, no activation) can't be the framed
    // bootstrap window: a new window is created instead.
    let panel = BrowserWindowOptions {
      frameless: Some(true),
      no_activate: Some(true),
      ..Default::default()
    };
    assert!(!adopts_initial_window(&plain, Some(&panel)));
    for options in [
      BrowserWindowOptions {
        transparent: Some(true),
        ..Default::default()
      },
      BrowserWindowOptions {
        transparent_titlebar: Some(true),
        ..Default::default()
      },
    ] {
      assert!(!adopts_initial_window(&plain, Some(&options)));
    }
    // A frameless `initialWindow` is adopted by a frameless request, and
    // not by one asking for a frame.
    let frameless = InitialWindowAttributes {
      frameless: true,
      no_activate: true,
      ..Default::default()
    };
    assert!(adopts_initial_window(&frameless, Some(&panel)));
    assert!(!adopts_initial_window(
      &frameless,
      Some(&BrowserWindowOptions {
        frameless: Some(false),
        ..Default::default()
      })
    ));
  }

  #[test]
  fn strings_with_nul_are_refused_before_the_backend() {
    use deno_error::JsErrorClass;

    use super::DesktopValue;
    use super::FileDialogRequest;
    use super::FileFilterInfo;
    use super::MenuItem;
    use super::NotificationActionInfo;
    use super::NotificationRequest;
    use super::reject_nul;
    use super::reject_nul_in_file_dialog;
    use super::reject_nul_in_menu;
    use super::reject_nul_in_notification;
    use super::reject_nul_in_value;
    use super::replace_nul;

    let err = reject_nul("the window title", "a\0b").unwrap_err();
    assert_eq!(err.get_class(), "TypeError");
    assert!(err.get_message().contains("the window title"));
    assert!(reject_nul("the window title", "ab").is_ok());

    let item = |label: &str| MenuItem::Item {
      label: label.into(),
      id: Some("id".into()),
      accelerator: None,
      enabled: true,
      checked: false,
      icon: None,
      tooltip: None,
    };
    assert!(reject_nul_in_menu(&[item("ok"), MenuItem::Separator]).is_ok());
    // Deep in a submenu too.
    let nested = MenuItem::Submenu {
      label: "File".into(),
      items: vec![item("ok"), item("bad\0")],
    };
    assert!(reject_nul_in_menu(&[nested]).is_err());
    assert!(
      reject_nul_in_menu(&[MenuItem::Role {
        role: "quit\0".into()
      }])
      .is_err()
    );

    let ok = DesktopValue::Dict(vec![(
      "k".into(),
      DesktopValue::List(vec![DesktopValue::String("v".into())]),
    )]);
    assert!(reject_nul_in_value(&ok).is_ok());
    let bad_value = DesktopValue::List(vec![DesktopValue::String("\0".into())]);
    assert!(reject_nul_in_value(&bad_value).is_err());
    let bad_key = DesktopValue::Dict(vec![("k\0".into(), DesktopValue::Null)]);
    assert!(reject_nul_in_value(&bad_key).is_err());
    // Binary data may hold any byte.
    assert!(reject_nul_in_value(&DesktopValue::Binary(vec![0, 0])).is_ok());

    let notification = NotificationRequest {
      title: "t".into(),
      actions: vec![NotificationActionInfo {
        action: "a".into(),
        title: "A\0".into(),
      }],
      ..Default::default()
    };
    assert!(reject_nul_in_notification(&notification).is_err());

    let dialog = FileDialogRequest {
      save: false,
      window_id: 0,
      title: None,
      default_path: None,
      button_label: None,
      filters: vec![FileFilterInfo {
        name: "Images".into(),
        extensions: vec!["png\0".into()],
      }],
      files: true,
      directories: false,
      multiple: false,
      show_hidden: false,
    };
    assert!(reject_nul_in_file_dialog(&dialog).is_err());

    assert_eq!(replace_nul("a\0b".into()), "a\u{FFFD}b");
  }

  #[test]
  fn a_window_with_a_webgpu_surface_is_never_destroyed() {
    use super::NativeClose;
    use super::native_close_action;
    assert_eq!(
      native_close_action(false, true, false),
      NativeClose::Destroy
    );
    assert_eq!(
      native_close_action(false, false, true),
      NativeClose::Destroy
    );
    // A surface window is hidden and kept; the app still quits when it was
    // the last window and the app quits on the last window closing.
    assert_eq!(
      native_close_action(true, true, false),
      NativeClose::HideAndKeep { quit: true }
    );
    assert_eq!(
      native_close_action(true, true, true),
      NativeClose::HideAndKeep { quit: false }
    );
    assert_eq!(
      native_close_action(true, false, false),
      NativeClose::HideAndKeep { quit: false }
    );
  }

  #[test]
  fn desktop_event_queue_never_loses_discrete_events() {
    use super::DesktopEvent;
    use super::create_desktop_event_channel;
    let (tx, rx) = create_desktop_event_channel();
    let motion = |window_id: u32, x: f64| DesktopEvent::MouseMove {
      window_id,
      client_x: x,
      client_y: 0.0,
      shift: false,
      control: false,
      alt: false,
      meta: false,
    };
    let wheel = |dy: f64| DesktopEvent::Wheel {
      window_id: 1,
      delta_x: 0.0,
      delta_y: dy,
      delta_mode: 0,
      client_x: 0.0,
      client_y: 0.0,
      shift: false,
      control: false,
      alt: false,
      meta: false,
    };
    // A flood of motion for one window takes one slot: the latest.
    for i in 0..10_000 {
      tx.0.try_send(motion(1, i as f64)).unwrap();
    }
    // Wheel deltas add up.
    tx.0.try_send(wheel(1.0)).unwrap();
    tx.0.try_send(wheel(2.5)).unwrap();
    let mut got = vec![];
    while let Some(ev) = rx.0.try_recv() {
      got.push(ev);
    }
    assert_eq!(got.len(), 2);
    assert!(matches!(
      got[0],
      DesktopEvent::MouseMove { client_x, .. } if client_x == 9999.0
    ));
    assert!(matches!(
      got[1],
      DesktopEvent::Wheel { delta_y, .. } if delta_y == 3.5
    ));

    // Fill the queue with motion that can't coalesce (alternating windows).
    for i in 0..super::DESKTOP_EVENT_CHANNEL_CAPACITY {
      tx.0.try_send(motion((i % 2) as u32, 0.0)).unwrap();
    }
    // Full: more motion is dropped, a bound-function call refused...
    assert!(tx.0.try_send(motion(5, 0.0)).is_err());
    assert!(
      tx.0
        .try_send(DesktopEvent::BindCall {
          window_id: 1,
          name: "f".into(),
          args: vec![],
          call_id: 1,
          origin: String::new(),
        })
        .is_err()
    );
    // ...but a context menu closing, a close request and a page load are
    // still delivered, after the motion, in order.
    tx.0
      .try_send(DesktopEvent::ContextMenuClose { window_id: 1 })
      .unwrap();
    tx.0
      .try_send(DesktopEvent::CloseRequested { window_id: 1 })
      .unwrap();
    tx.0
      .try_send(DesktopEvent::PageLoad { window_id: 1 })
      .unwrap();
    let mut tail = vec![];
    while let Some(ev) = rx.0.try_recv() {
      tail.push(ev);
    }
    assert_eq!(tail.len(), super::DESKTOP_EVENT_CHANNEL_CAPACITY + 3);
    assert!(matches!(
      tail[tail.len() - 3..],
      [
        DesktopEvent::ContextMenuClose { .. },
        DesktopEvent::CloseRequested { .. },
        DesktopEvent::PageLoad { .. }
      ]
    ));

    // Once the runtime's receiver is gone, sends fail and a weak sender
    // (a binding's handler) no longer reaches the queue.
    let weak = tx.0.downgrade();
    drop(rx);
    assert!(
      tx.0
        .try_send(DesktopEvent::PageLoad { window_id: 1 })
        .is_err()
    );
    drop(tx);
    assert!(weak.upgrade().is_none());
  }

  #[tokio::test]
  async fn desktop_event_queue_wakes_the_receiver() {
    use super::DesktopEvent;
    let (tx, rx) = super::create_desktop_event_channel();
    let sender = tx.0.clone();
    let recv = tokio::spawn(async move { rx.0.recv().await });
    tokio::task::yield_now().await;
    sender
      .try_send(DesktopEvent::PageLoad { window_id: 7 })
      .unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), recv)
      .await
      .expect("woken")
      .unwrap();
    assert!(matches!(got, Some(DesktopEvent::PageLoad { window_id: 7 })));
  }

  #[test]
  fn error_report_post_gives_up_on_a_silent_endpoint() {
    // An HTTPS endpoint that accepts the connection and never answers (not
    // even the TLS handshake). The post used to wait on it forever.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let held = std::thread::spawn(move || {
      let conn = listener.accept();
      std::thread::sleep(std::time::Duration::from_secs(10));
      drop(conn);
    });
    // As the runtime binaries do at startup (cli/rt/lib.rs).
    let _ =
      deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = deno_fetch::create_http_client(
      "deno-test",
      deno_fetch::CreateHttpClientOptions::default(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    super::post_error_report_within(
      client,
      format!("https://{addr}/report"),
      "{}".into(),
      std::time::Duration::from_millis(500),
    );
    assert!(
      started.elapsed() < std::time::Duration::from_secs(5),
      "took {:?}",
      started.elapsed()
    );
    drop(held);
  }
}
