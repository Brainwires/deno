// Copyright 2018-2026 the Deno authors. MIT license.

//! Desktop window management for `deno compile --desktop`.
//!
//! The ops are defined in `deno_runtime::ops::desktop` and included in the
//! V8 snapshot. This module re-exports the key types and provides the JS
//! initialization code.

use std::sync::Arc;

use deno_core::OpState;
// Re-export from runtime so denort_desktop can use them.
pub use deno_runtime::ops::desktop::AutoUpdateState;
pub use deno_runtime::ops::desktop::DesktopApi;
pub use deno_runtime::ops::desktop::MenuItem;

/// JS code that exposes desktop APIs via `Deno.BrowserWindow` and `Deno.desktop`.
pub const DESKTOP_JS: &str = r#"
(() => {
  const internals = Deno[Deno.internal];
  const {
    BrowserWindow,
    Dock,
    Tray,
    Notification: NotificationNative,
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
    op_desktop_request_notification_permission,
    op_desktop_query_notification_permission,
    op_desktop_screens,
    op_desktop_window_capabilities,
    op_desktop_quit,
    op_desktop_set_quit_on_last_window_closed,
    op_desktop_close_reply,
  } = internals.core.ops;
  const BrowserWindowPrototype = BrowserWindow.prototype;
  Object.setPrototypeOf(BrowserWindowPrototype, EventTarget.prototype);
  const privateDesktopBind = Symbol.for("Deno_privateDesktopBind");
  const privateDesktopUnbind = Symbol.for("Deno_privateDesktopUnbind");

  class UIEvent extends Event {
    #detail = 0;
    #view = null;

    get detail() { return this.#detail; }
    get view() { return this.#view; }

    constructor(type, init = {}) {
      super(type, init);
      this.#detail = init.detail ?? 0;
      this.#view = init.view ?? null;
    }
  }

  class FocusEvent extends UIEvent {
    #relatedTarget = null;

    get relatedTarget() { return this.#relatedTarget; }

    constructor(type, init = {}) {
      super(type, init);
      this.#relatedTarget = init.relatedTarget ?? null;
    }
  }

  class KeyboardEvent extends UIEvent {
    #key = "";
    #code = "";
    #location = 0;
    #ctrlKey = false;
    #shiftKey = false;
    #altKey = false;
    #metaKey = false;
    #repeat = false;
    #isComposing = false;

    get key() { return this.#key; }
    get code() { return this.#code; }
    get location() { return this.#location; }
    get ctrlKey() { return this.#ctrlKey; }
    get shiftKey() { return this.#shiftKey; }
    get altKey() { return this.#altKey; }
    get metaKey() { return this.#metaKey; }
    get repeat() { return this.#repeat; }
    get isComposing() { return this.#isComposing; }

    constructor(type, init = {}) {
      super(type, init);
      this.#key = init.key ?? "";
      this.#code = init.code ?? "";
      this.#location = init.location ?? 0;
      this.#ctrlKey = init.ctrlKey ?? false;
      this.#shiftKey = init.shiftKey ?? false;
      this.#altKey = init.altKey ?? false;
      this.#metaKey = init.metaKey ?? false;
      this.#repeat = init.repeat ?? false;
      this.#isComposing = init.isComposing ?? false;
    }

    getModifierState(key) {
      switch (key) {
        case "Alt": return this.#altKey;
        case "Control": return this.#ctrlKey;
        case "Meta": return this.#metaKey;
        case "Shift": return this.#shiftKey;
        default: return false;
      }
    }
  }

  class MouseEvent extends UIEvent {
    #button = 0;
    #buttons = 0;
    #clientX = 0;
    #clientY = 0;
    #screenX = 0;
    #screenY = 0;
    #ctrlKey = false;
    #shiftKey = false;
    #altKey = false;
    #metaKey = false;

    get button() { return this.#button; }
    get buttons() { return this.#buttons; }
    get clientX() { return this.#clientX; }
    get clientY() { return this.#clientY; }
    get screenX() { return this.#screenX; }
    get screenY() { return this.#screenY; }
    get ctrlKey() { return this.#ctrlKey; }
    get shiftKey() { return this.#shiftKey; }
    get altKey() { return this.#altKey; }
    get metaKey() { return this.#metaKey; }

    constructor(type, init = {}) {
      super(type, init);
      this.#button = init.button ?? 0;
      this.#buttons = init.buttons ?? 0;
      this.#clientX = init.clientX ?? 0;
      this.#clientY = init.clientY ?? 0;
      this.#screenX = init.screenX ?? this.#clientX;
      this.#screenY = init.screenY ?? this.#clientY;
      this.#ctrlKey = init.ctrlKey ?? false;
      this.#shiftKey = init.shiftKey ?? false;
      this.#altKey = init.altKey ?? false;
      this.#metaKey = init.metaKey ?? false;
    }

    getModifierState(key) {
      switch (key) {
        case "Alt": return this.#altKey;
        case "Control": return this.#ctrlKey;
        case "Meta": return this.#metaKey;
        case "Shift": return this.#shiftKey;
        default: return false;
      }
    }
  }

  class WheelEvent extends MouseEvent {
    #deltaX = 0;
    #deltaY = 0;
    #deltaZ = 0;
    #deltaMode = 0;

    get deltaX() { return this.#deltaX; }
    get deltaY() { return this.#deltaY; }
    get deltaZ() { return this.#deltaZ; }
    get deltaMode() { return this.#deltaMode; }

    constructor(type, init = {}) {
      super(type, init);
      this.#deltaX = init.deltaX ?? 0;
      this.#deltaY = init.deltaY ?? 0;
      this.#deltaZ = init.deltaZ ?? 0;
      this.#deltaMode = init.deltaMode ?? 0;
    }
  }

  // width / height / aspect-ratio / orientation / resolution /
  // device-pixel-ratio. Colon form, boolean features, MQ4 ranges.
  function mqSplitTop(s, sep) {
    const out = [];
    let buf = "";
    let depth = 0;
    for (let i = 0; i < s.length; i++) {
      const ch = s[i];
      if (ch === "(") depth++;
      else if (ch === ")") depth = Math.max(0, depth - 1);
      if (ch === sep && depth === 0) {
        out.push(buf);
        buf = "";
      } else {
        buf += ch;
      }
    }
    out.push(buf);
    return out;
  }

  function mqSplitAndOr(s) {
    const parts = [];
    let buf = "";
    let depth = 0;
    let pendingOp = "";
    const lower = s.toLowerCase();
    const flush = (nextOp) => {
      if (buf.trim()) {
        parts.push({ op: pendingOp, text: buf });
        pendingOp = nextOp;
        buf = "";
      }
    };
    for (let i = 0; i < s.length; i++) {
      const ch = s[i];
      if (ch === "(") depth++;
      else if (ch === ")") depth = Math.max(0, depth - 1);
      if (depth === 0) {
        if (lower.startsWith(" and ", i) || (i === 0 && lower.startsWith("and ", i))) {
          flush("and");
          i += (lower.startsWith(" and ", i) ? 5 : 4) - 1;
          continue;
        }
        if (lower.startsWith(" or ", i) || (i === 0 && lower.startsWith("or ", i))) {
          flush("or");
          i += (lower.startsWith(" or ", i) ? 4 : 3) - 1;
          continue;
        }
      }
      buf += ch;
    }
    flush("");
    return parts;
  }

  function mqUnwrap(s) {
    s = s.trim();
    while (s.startsWith("(") && s.endsWith(")")) {
      let depth = 0;
      let wraps = true;
      for (let i = 0; i < s.length; i++) {
        if (s[i] === "(") depth++;
        else if (s[i] === ")") {
          depth--;
          if (depth === 0 && i !== s.length - 1) {
            wraps = false;
            break;
          }
        }
      }
      if (!wraps) break;
      s = s.slice(1, -1).trim();
    }
    return s;
  }

  function mqParseNumber(s) {
    const m = String(s).trim().match(/^([+-]?(?:\d+\.?\d*|\.\d+))(.*)$/);
    if (!m) return null;
    const n = parseFloat(m[1]);
    if (!Number.isFinite(n)) return null;
    return { n, unit: m[2].trim().toLowerCase() };
  }

  function mqParsePx(s) {
    const v = mqParseNumber(s);
    if (!v) return null;
    if (v.unit === "" || v.unit === "px") return v.n;
    return null;
  }

  function mqParseDppx(s) {
    const v = mqParseNumber(s);
    if (!v) return null;
    if (v.unit === "" || v.unit === "dppx" || v.unit === "x") return v.n;
    if (v.unit === "dpi") return v.n / 96;
    if (v.unit === "dpcm") return v.n / (96 / 2.54);
    return null;
  }

  function mqParseRatio(s) {
    const t = String(s).trim();
    const frac = t.match(/^([+-]?(?:\d+\.?\d*|\.\d+))\s*\/\s*([+-]?(?:\d+\.?\d*|\.\d+))$/);
    if (frac) {
      const a = parseFloat(frac[1]);
      const b = parseFloat(frac[2]);
      if (!Number.isFinite(a) || !Number.isFinite(b) || b === 0) return null;
      return a / b;
    }
    const n = parseFloat(t);
    return Number.isFinite(n) ? n : null;
  }

  function mqCanonical(name) {
    name = name.toLowerCase();
    if (name === "-webkit-device-pixel-ratio") return "device-pixel-ratio";
    return name;
  }

  function mqIsRangeName(name) {
    name = mqCanonical(name);
    return name === "width" || name === "height" || name === "aspect-ratio" ||
      name === "resolution" || name === "device-pixel-ratio";
  }

  function mqActual(ctx, name) {
    name = mqCanonical(name);
    if (name === "width") return ctx.width;
    if (name === "height") return ctx.height;
    if (name === "aspect-ratio") {
      return ctx.height > 0 ? ctx.width / ctx.height : 0;
    }
    if (name === "resolution" || name === "device-pixel-ratio") return ctx.dpr;
    if (name === "orientation") {
      return ctx.width >= ctx.height ? "landscape" : "portrait";
    }
    return null;
  }

  function mqParseValue(name, raw) {
    name = mqCanonical(name);
    if (name === "width" || name === "height") return mqParsePx(raw);
    if (name === "aspect-ratio") return mqParseRatio(raw);
    if (name === "resolution") return mqParseDppx(raw);
    if (name === "device-pixel-ratio") {
      const v = mqParseNumber(raw);
      return v && (v.unit === "") ? v.n : null;
    }
    return null;
  }

  function mqCmp(actual, op, expected) {
    if (actual == null || expected == null || Number.isNaN(expected)) return false;
    switch (op) {
      case "<": return actual < expected;
      case "<=": return actual <= expected;
      case ">": return actual > expected;
      case ">=": return actual >= expected;
      case "=": return Math.abs(actual - expected) < 1e-6;
      default: return false;
    }
  }

  function mqFlip(op) {
    if (op === "<") return ">";
    if (op === "<=") return ">=";
    if (op === ">") return "<";
    if (op === ">=") return "<=";
    return op;
  }

  function mqSameDir(op1, op2) {
    const lt = op1 === "<" || op1 === "<=";
    const lt2 = op2 === "<" || op2 === "<=";
    const gt = op1 === ">" || op1 === ">=";
    const gt2 = op2 === ">" || op2 === ">=";
    return (lt && lt2) || (gt && gt2);
  }

  function mqEvalColon(ctx, name, value) {
    let min = false;
    let max = false;
    name = name.toLowerCase();
    if (name.startsWith("min-")) {
      min = true;
      name = name.slice(4);
    } else if (name.startsWith("max-")) {
      max = true;
      name = name.slice(4);
    } else if (name === "-webkit-min-device-pixel-ratio") {
      min = true;
      name = "device-pixel-ratio";
    } else if (name === "-webkit-max-device-pixel-ratio") {
      max = true;
      name = "device-pixel-ratio";
    }
    name = mqCanonical(name);
    if (name === "orientation") {
      if (min || max) return false;
      return mqActual(ctx, name) === String(value).trim().toLowerCase();
    }
    if (!mqIsRangeName(name)) return false;
    const expected = mqParseValue(name, value);
    const actual = mqActual(ctx, name);
    if (min) return mqCmp(actual, ">=", expected);
    if (max) return mqCmp(actual, "<=", expected);
    return mqCmp(actual, "=", expected);
  }

  function mqEvalBoolean(ctx, name) {
    name = mqCanonical(name);
    if (name === "orientation") return true;
    if (!mqIsRangeName(name)) return false;
    const actual = mqActual(ctx, name);
    return typeof actual === "number" && actual > 0;
  }

  function mqEvalPlainFeature(ctx, inner) {
    inner = inner.trim();
    const range3 = /^(.+?)\s*(<=|>=|<|>|=)\s*([-\w]+)\s*(<=|>=|<|>|=)\s*(.+)$/
      .exec(inner);
    if (range3 && mqIsRangeName(range3[3])) {
      const op1 = range3[2];
      const op2 = range3[4];
      if (op1 === "=" || op2 === "=" || !mqSameDir(op1, op2)) return false;
      const name = range3[3];
      const actual = mqActual(ctx, name);
      return mqCmp(actual, mqFlip(op1), mqParseValue(name, range3[1])) &&
        mqCmp(actual, op2, mqParseValue(name, range3[5]));
    }
    const leftName = /^([-\w]+)\s*(<=|>=|<|>|=)\s*(.+)$/.exec(inner);
    if (leftName && mqIsRangeName(leftName[1])) {
      const name = leftName[1];
      return mqCmp(
        mqActual(ctx, name),
        leftName[2],
        mqParseValue(name, leftName[3]),
      );
    }
    const rightName = /^(.+?)\s*(<=|>=|<|>|=)\s*([-\w]+)$/.exec(inner);
    if (rightName && mqIsRangeName(rightName[3])) {
      const name = rightName[3];
      return mqCmp(
        mqActual(ctx, name),
        mqFlip(rightName[2]),
        mqParseValue(name, rightName[1]),
      );
    }
    const colon = /^([-\w]+)\s*:\s*(.+)$/.exec(inner);
    if (colon) return mqEvalColon(ctx, colon[1], colon[2]);
    const bool = /^([-\w]+)$/.exec(inner);
    if (bool) return mqEvalBoolean(ctx, bool[1]);
    return false;
  }

  function mqEvalCondition(ctx, raw) {
    const parts = mqSplitAndOr(raw.trim());
    if (parts.length === 0) return false;
    let sawAnd = false;
    let sawOr = false;
    for (let i = 1; i < parts.length; i++) {
      if (parts[i].op === "and") sawAnd = true;
      if (parts[i].op === "or") sawOr = true;
    }
    if (sawAnd && sawOr) return false;
    const evalPart = (text) => {
      let t = text.trim();
      const not = /^not\s+/i.exec(t);
      let negated = false;
      if (not) {
        negated = true;
        t = t.slice(not[0].length);
      }
      const inner = mqUnwrap(t);
      let ok;
      if (inner !== t.trim() || /^\(.*\)$/.test(t.trim())) {
        const unwrapped = mqUnwrap(t);
        if (
          /\band\b|\bor\b|^not\s+/i.test(unwrapped) &&
          !/^[-\w]+\s*(<=|>=|<|>|=|:)/.test(unwrapped)
        ) {
          ok = mqEvalCondition(ctx, unwrapped);
        } else {
          ok = mqEvalPlainFeature(ctx, unwrapped);
        }
      } else {
        ok = mqEvalPlainFeature(ctx, inner);
      }
      return negated ? !ok : ok;
    };
    if (sawOr) return parts.some((p) => evalPart(p.text));
    return parts.every((p) => evalPart(p.text));
  }

  function evalMediaQuery(ctx, query) {
    let q = query.trim();
    if (!q) return false;
    let negated = false;
    const prefix = /^(only|not)\s+/i.exec(q);
    if (prefix) {
      if (prefix[1].toLowerCase() === "not") negated = true;
      q = q.slice(prefix[0].length);
    }
    let result;
    if (q.startsWith("(") || /^not\s*\(/i.test(q)) {
      result = mqEvalCondition(ctx, q);
    } else {
      const m = /^([a-zA-Z][\w-]*)(?:\s+and\s+([\s\S]+))?$/i.exec(q);
      if (!m) return false;
      const type = m[1].toLowerCase();
      const typeOk = type === "all" || type === "screen";
      result = typeOk && (m[2] ? mqEvalCondition(ctx, m[2]) : true);
    }
    return negated ? !result : result;
  }

  function evalMediaQueryList(ctx, media) {
    const s = String(media);
    if (s.trim() === "") return true;
    return mqSplitTop(s, ",").some((q) => evalMediaQuery(ctx, q));
  }

  function mediaContext(win) {
    let width = 0;
    let height = 0;
    let dpr = 1;
    try {
      width = win.innerWidth;
      height = win.innerHeight;
      dpr = win.devicePixelRatio;
    } catch (_) {
      // Native getters throw on a closed window.
    }
    if (!(dpr > 0)) dpr = 1;
    return { width, height, dpr };
  }

  const mediaLists = new WeakMap();

  class MediaQueryListEvent extends Event {
    #media = "";
    #matches = false;
    get media() { return this.#media; }
    get matches() { return this.#matches; }
    constructor(type, init = {}) {
      super(type, init);
      this.#media = init.media ?? "";
      this.#matches = !!init.matches;
    }
  }

  class MediaQueryList extends EventTarget {
    #win;
    #query;
    #matches;
    #listening = false;

    constructor(win, query) {
      super();
      if (win == null || typeof win !== "object") {
        throw new TypeError("Illegal constructor");
      }
      this.#win = win;
      this.#query = String(query);
      this.#matches = evalMediaQueryList(mediaContext(win), this.#query);
    }

    get matches() {
      return evalMediaQueryList(mediaContext(this.#win), this.#query);
    }
    get media() { return this.#query; }

    #track() {
      if (this.#listening) return;
      this.#listening = true;
      let set = mediaLists.get(this.#win);
      if (!set) {
        set = new Set();
        mediaLists.set(this.#win, set);
      }
      set.add(this);
    }

    addEventListener(type, cb, opts) {
      super.addEventListener(type, cb, opts);
      if (type === "change" && cb != null) this.#track();
    }

    addListener(cb) {
      if (cb == null) return;
      this.addEventListener("change", cb);
    }
    removeListener(cb) {
      if (cb == null) return;
      this.removeEventListener("change", cb);
    }

    static reeval(win) {
      const set = mediaLists.get(win);
      if (!set) return;
      for (const list of set) list.#reeval();
    }

    #reeval() {
      const next = evalMediaQueryList(mediaContext(this.#win), this.#query);
      if (next === this.#matches) return;
      this.#matches = next;
      this.dispatchEvent(new MediaQueryListEvent("change", {
        media: this.#query,
        matches: next,
      }));
    }
  }
  internals.defineEventHandler(MediaQueryList.prototype, "change");

  op_desktop_init(
    internals.webidlBrand,
    internals.setEventTargetData,
  );

  // Window registry: windowId -> BrowserWindow instance.
  const windows = new Map();
  const nativeConstructor = BrowserWindow;
  const OrigBW = function(...args) {
    const instance = new nativeConstructor(...args);
    const windowId = instance.windowId;
    windows.set(windowId, instance);
    applyWindowOptions(instance, args[0]);
    return instance;
  };
  // Options the native constructor doesn't apply (size limits, fullscreen,
  // chrome), applied right after the window exists.
  function applyWindowOptions(win, options) {
    if (options == null || typeof options !== "object") return;
    const { minWidth, minHeight, maxWidth, maxHeight } = options;
    if (minWidth != null || minHeight != null) {
      win.setMinimumSize(minWidth ?? 0, minHeight ?? 0);
    }
    if (maxWidth != null || maxHeight != null) {
      win.setMaximumSize(maxWidth ?? 0, maxHeight ?? 0);
    }
    if (options.titleBarStyle != null) {
      win.setTitleBarStyle(options.titleBarStyle);
    }
    if (options.trafficLightPosition != null) {
      win.setWindowButtonPosition(options.trafficLightPosition);
    }
    if (options.vibrancy != null) win.setVibrancy(options.vibrancy);
    if (options.backgroundMaterial != null) {
      win.setBackgroundMaterial(options.backgroundMaterial);
    }
    if (options.fullscreen) win.setFullScreen(true);
  }
  Object.setPrototypeOf(OrigBW, nativeConstructor);
  Object.setPrototypeOf(OrigBW.prototype, nativeConstructor.prototype);
  Deno.BrowserWindow = OrigBW;

  internals.defineEventHandler(BrowserWindowPrototype, "keydown");
  internals.defineEventHandler(BrowserWindowPrototype, "keyup");
  internals.defineEventHandler(BrowserWindowPrototype, "mousedown");
  internals.defineEventHandler(BrowserWindowPrototype, "mouseup");
  internals.defineEventHandler(BrowserWindowPrototype, "click");
  internals.defineEventHandler(BrowserWindowPrototype, "dblclick");
  internals.defineEventHandler(BrowserWindowPrototype, "mousemove");
  internals.defineEventHandler(BrowserWindowPrototype, "wheel");
  internals.defineEventHandler(BrowserWindowPrototype, "mouseenter");
  internals.defineEventHandler(BrowserWindowPrototype, "mouseleave");
  internals.defineEventHandler(BrowserWindowPrototype, "focus");
  internals.defineEventHandler(BrowserWindowPrototype, "blur");
  internals.defineEventHandler(BrowserWindowPrototype, "resize");
  internals.defineEventHandler(BrowserWindowPrototype, "move");
  internals.defineEventHandler(BrowserWindowPrototype, "load");
  internals.defineEventHandler(BrowserWindowPrototype, "close");
  internals.defineEventHandler(BrowserWindowPrototype, "menuclick");
  internals.defineEventHandler(BrowserWindowPrototype, "contextmenuclick");
  internals.defineEventHandler(BrowserWindowPrototype, "contextmenuclose");
  internals.defineEventHandler(BrowserWindowPrototype, "maximize");
  internals.defineEventHandler(BrowserWindowPrototype, "unmaximize");
  internals.defineEventHandler(BrowserWindowPrototype, "minimize");
  internals.defineEventHandler(BrowserWindowPrototype, "restore");
  internals.defineEventHandler(BrowserWindowPrototype, "enterfullscreen");
  internals.defineEventHandler(BrowserWindowPrototype, "leavefullscreen");
  // Files dragged over / dropped on the window (laufey API 39).
  internals.defineEventHandler(BrowserWindowPrototype, "dragenter");
  internals.defineEventHandler(BrowserWindowPrototype, "dragover");
  internals.defineEventHandler(BrowserWindowPrototype, "dragleave");
  internals.defineEventHandler(BrowserWindowPrototype, "drop");

  // Window chrome (laufey API 38). Each returns whether this backend / OS
  // applied it (see Deno.desktop.windowCapabilities()).
  const privateTitleBarStyle = Symbol.for("Deno_privateDesktopTitleBarStyle");
  const privateWindowButtonPosition = Symbol.for(
    "Deno_privateDesktopWindowButtonPosition",
  );
  const privateBackdrop = Symbol.for("Deno_privateDesktopBackdrop");
  const privateScreenId = Symbol.for("Deno_privateDesktopScreenId");
  const TITLE_BAR_STYLES = { default: 0, hidden: 1, hiddenInset: 2 };
  const BACKGROUND_MATERIALS = { none: 0, mica: 1, acrylic: 2, tabbed: 3 };
  // Electron's vibrancy names -> NSVisualEffectMaterial.
  const VIBRANCY_MATERIALS = {
    "titlebar": 3,
    "selection": 4,
    "menu": 5,
    "popover": 6,
    "sidebar": 7,
    "header": 10,
    "sheet": 11,
    "window": 12,
    "hud": 13,
    "fullscreen-ui": 15,
    "tooltip": 17,
    "content": 18,
    "under-window": 21,
    "under-page": 22,
  };
  const BACKDROP_VIBRANCY = 4;
  BrowserWindowPrototype.setTitleBarStyle = function(style) {
    const raw = TITLE_BAR_STYLES[String(style)];
    if (raw === undefined) {
      throw new TypeError(`Unknown title bar style: ${style}`);
    }
    return this[privateTitleBarStyle](raw);
  };
  BrowserWindowPrototype.setWindowButtonPosition = function(position) {
    if (position == null) return this[privateWindowButtonPosition](true, 0, 0);
    const x = Math.trunc(Number(position.x));
    const y = Math.trunc(Number(position.y));
    if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0) {
      throw new TypeError("position must be { x, y } with x, y >= 0, or null");
    }
    return this[privateWindowButtonPosition](false, x, y);
  };
  BrowserWindowPrototype.setBackgroundMaterial = function(material) {
    const raw = BACKGROUND_MATERIALS[String(material)];
    if (raw === undefined) {
      throw new TypeError(`Unknown background material: ${material}`);
    }
    return this[privateBackdrop](raw, 0);
  };
  BrowserWindowPrototype.setVibrancy = function(material) {
    if (material == null) return this[privateBackdrop](0, 0);
    const raw = VIBRANCY_MATERIALS[String(material)];
    if (raw === undefined) {
      throw new TypeError(`Unknown vibrancy material: ${material}`);
    }
    return this[privateBackdrop](BACKDROP_VIBRANCY, raw);
  };
  // Drag files out of the window to another app or the desktop (laufey API
  // 39), as a copy. Call it while the left mouse button is held (from the
  // page's dragstart, after preventDefault()). Resolves "dropped",
  // "cancelled" or "failed"; rejects only on a wrong argument.
  const MAX_DRAG_FILES = 4096;
  BrowserWindowPrototype.startDrag = async function startDrag(item) {
    if (item == null || typeof item !== "object") {
      throw new TypeError("startDrag takes { files: string[], icon? }");
    }
    let files = item.files;
    if (files === undefined && typeof item.file === "string") {
      files = [item.file];
    }
    if (
      !Array.isArray(files) || files.length === 0 ||
      files.length > MAX_DRAG_FILES ||
      !files.every((f) => typeof f === "string" && f.length > 0)
    ) {
      throw new TypeError(
        `item.files must be 1 to ${MAX_DRAG_FILES} absolute paths`,
      );
    }
    let icon = item.icon;
    if (icon === undefined || icon === null) {
      icon = new Uint8Array(0);
    } else if (!(icon instanceof Uint8Array)) {
      throw new TypeError("item.icon must be PNG bytes (a Uint8Array)");
    }
    return await op_desktop_start_drag(this.windowId, [...files], icon);
  };
  // showContextMenu (laufey API 41): resolves once the menu closed, with the
  // chosen item's id (null when it was dismissed); a "contextmenuclose"
  // event (detail { id }) fires then too. One context menu is open at a
  // time, so the window's close events resolve its calls in order.
  const nativeShowContextMenu = BrowserWindowPrototype.showContextMenu;
  const contextMenuWaiters = new WeakMap(); // window -> [{ resolve }]
  const contextMenuChoice = new WeakMap(); // window -> id of the last click
  BrowserWindowPrototype.showContextMenu = function showContextMenu(
    x,
    y,
    items,
  ) {
    nativeShowContextMenu.call(this, x, y, items);
    if (!op_desktop_menu_capabilities().contextClosed) {
      return Promise.resolve(null);
    }
    return new Promise((resolve) => {
      let list = contextMenuWaiters.get(this);
      if (!list) {
        list = [];
        contextMenuWaiters.set(this, list);
      }
      list.push(resolve);
    });
  };
  function contextMenuClosed(target) {
    const id = contextMenuChoice.get(target) ?? null;
    contextMenuChoice.delete(target);
    target.dispatchEvent(new CustomEvent("contextmenuclose", {
      detail: { id },
    }));
    const resolve = contextMenuWaiters.get(target)?.shift();
    if (resolve) resolve(id);
  }

  BrowserWindowPrototype.getScreen = function() {
    const id = this[privateScreenId]();
    if (!id) return null;
    return op_desktop_screens().find((s) => s.id === id) ?? null;
  };
  // A state change from the backend -> the Electron-style events.
  function dispatchWindowStateEvents(target, state, previous) {
    const fire = (type) => target.dispatchEvent(new Event(type));
    if (!previous.minimized && state.minimized) fire("minimize");
    if (previous.minimized && !state.minimized) fire("restore");
    if (!previous.maximized && state.maximized) fire("maximize");
    if (previous.maximized && !state.maximized) fire("unmaximize");
    if (!previous.fullscreen && state.fullscreen) fire("enterfullscreen");
    if (previous.fullscreen && !state.fullscreen) fire("leavefullscreen");
  }

  BrowserWindowPrototype.matchMedia = function(query) {
    return new MediaQueryList(this, query);
  };

  // Per-window pressed-button mask (DOM `MouseEvent.buttons`).
  const windowButtons = new Map();

  function buttonBit(button) {
    switch (button) {
      case 0: return 1;
      case 1: return 4;
      case 2: return 2;
      case 3: return 8;
      case 4: return 16;
      default: return 0;
    }
  }

  function screenXY(target, clientX, clientY) {
    try {
      const pos = typeof target.getInnerPosition === "function"
        ? target.getInnerPosition()
        : target.getPosition();
      if (Array.isArray(pos) && pos.length >= 2) {
        return [pos[0] + clientX, pos[1] + clientY];
      }
    } catch (_) {
      // Native getter missing or closed.
    }
    return [clientX, clientY];
  }

  function mouseInit(target, ev, extra) {
    extra = extra || {};
    const [screenX, screenY] = screenXY(target, ev.clientX, ev.clientY);
    return {
      button: ev.button ?? 0,
      buttons: extra.buttons ?? 0,
      clientX: ev.clientX,
      clientY: ev.clientY,
      screenX,
      screenY,
      ctrlKey: ev.control,
      shiftKey: ev.shift,
      altKey: ev.alt,
      metaKey: ev.meta,
      detail: extra.detail ?? 0,
    };
  }

  // Per-window bind callback registry: windowId -> Map<name, fn>
  const windowBindCallbacks = new Map();

  // Binding-call correlation: when --inspect is active, both the Deno
  // and renderer consoles emit matching console.debug messages so the
  // developer can trace a binding call across isolates.
  // bindingTrace is only useful under --inspect (where DENO_DESKTOP_MUX_WS
  // is set by the parent). `Deno.env.get` THROWS `NotCapable` if the
  // runtime wasn't compiled with --allow-env, which aborts DESKTOP_JS
  // execution before the event-loop IIFE below has a chance to register.
  // That's the "nothing works — no mouse, no keyboard, no alerts"
  // failure mode: events fire on the wef side and pile up in the mpsc
  // channel, but the JS side never reads them because this throw kills
  // the script. Catch the env-permission error and disable tracing.
  let bindingTrace = false;
  try {
    bindingTrace = typeof Deno.env?.get === "function"
      && Deno.env.get("DENO_DESKTOP_MUX_WS") != null;
  } catch (_) {
    // No env access — fine, we just don't trace binding calls.
  }

  // `options.origins`: the documents besides the app's own that may call the
  // binding ("*" for any, or a list of origins); `options.withCaller`: the
  // handler gets `{ origin, windowId }` of the calling document first.
  BrowserWindowPrototype.bind = function(name, fn, options = undefined) {
    const windowId = this.windowId;
    const origins = options?.origins;
    if (
      origins !== undefined && origins !== "*" &&
      !(Array.isArray(origins) && origins.length > 0 &&
        origins.every((o) => typeof o === "string" && !o.includes("\n")))
    ) {
      throw new TypeError('origins must be "*" or an array of origins');
    }
    // The native method takes "" (app only), "*" or one origin per line.
    const originsSpec = origins === undefined
      ? ""
      : origins === "*"
      ? "*"
      : origins.join("\n");
    BrowserWindowPrototype[privateDesktopBind].call(this, name, originsSpec);
    if (!windowBindCallbacks.has(windowId)) {
      windowBindCallbacks.set(windowId, new Map());
    }
    windowBindCallbacks.get(windowId).set(name, {
      fn: fn.bind(this),
      withCaller: options?.withCaller === true,
    });

    // Inject a renderer-side wrapper that emits console.debug around
    // every binding call. The wrapper waits for the native binding to
    // appear (CEF registers it asynchronously via IPC) and then
    // replaces it with a logging shim.
    if (bindingTrace) {
      const escapedName = JSON.stringify(name);
      // Cap retries at ~2s (200 × 10ms). CEF registers bindings via async
      // IPC; missing them after that long means navigation tore down the
      // page or the binding will never appear, and an unbounded
      // setTimeout loop would otherwise leak forever per such call.
      this.executeJs(`(function() {
        var n = ${escapedName};
        var seq = 0;
        var attempts = 0;
        function tryWrap() {
          if (typeof window.bindings === "undefined" || typeof window.bindings[n] !== "function") {
            if (++attempts >= 200) return;
            setTimeout(tryWrap, 10);
            return;
          }
          var orig = window.bindings[n];
          if (orig.__bindTrace) return;
          window.bindings[n] = async function() {
            var id = ++seq;
            var args = Array.prototype.slice.call(arguments);
            console.debug("[binding:call]", n, ":" + id, args);
            try {
              var result = await orig.apply(this, arguments);
              console.debug("[binding:return]", n, ":" + id, result);
              return result;
            } catch (e) {
              console.debug("[binding:error]", n, ":" + id, e);
              throw e;
            }
          };
          window.bindings[n].__bindTrace = true;
        }
        tryWrap();
      })();`);
    }
  };

  BrowserWindowPrototype.unbind = function(name) {
    const windowId = this.windowId;
    const callbacks = windowBindCallbacks.get(windowId);
    if (callbacks) callbacks.delete(name);
    BrowserWindowPrototype[privateDesktopUnbind].call(this, name);
  };

  function alert(message = "Alert") {
    op_desktop_alert("", String(message));
  }

  function confirm(message = "Confirm") {
    return op_desktop_confirm(String(message));
  }

  function prompt(message = "Prompt", defaultValue) {
    return op_desktop_prompt(String(message), defaultValue != null ? String(defaultValue) : null);
  }


  Object.defineProperties(globalThis, {
    alert: internals.core.propWritable(alert),
    confirm: internals.core.propWritable(confirm),
    prompt: internals.core.propWritable(prompt),
    UIEvent: internals.core.propNonEnumerable(UIEvent),
    FocusEvent: internals.core.propNonEnumerable(FocusEvent),
    KeyboardEvent: internals.core.propNonEnumerable(KeyboardEvent),
    MouseEvent: internals.core.propNonEnumerable(MouseEvent),
    WheelEvent: internals.core.propNonEnumerable(WheelEvent),
    MediaQueryList: internals.core.propNonEnumerable(MediaQueryList),
    MediaQueryListEvent: internals.core.propNonEnumerable(MediaQueryListEvent),
  });

  const DockPrototype = Dock.prototype;
  Object.setPrototypeOf(DockPrototype, EventTarget.prototype);

  const docks = new Set();
  const nativeDockConstructor = Dock;
  const OrigDock = function(...args) {
    const instance = new nativeDockConstructor(...args);
    docks.add(instance);
    return instance;
  };
  Object.setPrototypeOf(OrigDock, nativeDockConstructor);
  Object.setPrototypeOf(OrigDock.prototype, nativeDockConstructor.prototype);
  Deno.Dock = OrigDock;

  internals.defineEventHandler(DockPrototype, "menuclick");
  internals.defineEventHandler(DockPrototype, "reopen");

  const dock = new OrigDock();
  Object.defineProperty(Deno, "dock", internals.core.propReadOnly(dock));

  // Deno.desktop: app-level launch events. Deep links and files the OS hands
  // the running app ("openurl", "openfile"), launches forwarded by a second
  // instance ("secondinstance"), and the links and files the app was
  // launched with (launchUrls / launchFiles). The runtime buffers each kind
  // until the first listener for it is added (see DesktopLaunchInbox), so a
  // link that arrives while the app is still starting is not lost.
  const desktop = new EventTarget();
  const LAUNCH_EVENT_TYPES = [
    "openurl",
    "openfile",
    "secondinstance",
    "notificationresponse",
  ];
  // A notification response's data: the JSON text new Notification() /
  // schedule() stored, parsed back (undefined if there was none).
  function notificationResponseDetail(ev) {
    let data;
    if (typeof ev.data === "string") {
      try {
        data = JSON.parse(ev.data);
      } catch {
        data = ev.data;
      }
    }
    return {
      tag: ev.tag,
      action: ev.action ?? null,
      data,
      launch: ev.launch,
    };
  }
  const subscribedLaunchEvents = new Set();
  function dispatchLaunchEvent(ev) {
    switch (ev.kind) {
      case "openUrl":
        desktop.dispatchEvent(new CustomEvent("openurl", {
          detail: { url: ev.url },
        }));
        break;
      case "openFile":
        desktop.dispatchEvent(new CustomEvent("openfile", {
          detail: { path: ev.path },
        }));
        break;
      case "notificationResponse":
        desktop.dispatchEvent(new CustomEvent("notificationresponse", {
          detail: notificationResponseDetail(ev),
        }));
        break;
      case "secondInstance":
        desktop.dispatchEvent(new CustomEvent("secondinstance", {
          detail: {
            args: ev.args,
            cwd: ev.cwd,
            urls: ev.urls,
            files: ev.files,
          },
        }));
        break;
    }
  }
  function subscribeLaunchEvents(type) {
    if (
      !LAUNCH_EVENT_TYPES.includes(type) || subscribedLaunchEvents.has(type)
    ) {
      return;
    }
    subscribedLaunchEvents.add(type);
    const pending = op_desktop_subscribe_launch_events(type);
    if (pending.length > 0) {
      // After the current task, so every listener added alongside this one
      // (and an `on…` handler set next to it) sees the buffered events.
      queueMicrotask(() => {
        for (const ev of pending) dispatchLaunchEvent(ev);
      });
    }
  }
  const eventTargetAddEventListener = EventTarget.prototype.addEventListener;
  Object.defineProperty(desktop, "addEventListener", {
    value: function addEventListener(type, listener, options) {
      eventTargetAddEventListener.call(this, type, listener, options);
      if (listener != null) subscribeLaunchEvents(String(type));
    },
    writable: true,
    configurable: true,
    enumerable: false,
  });
  for (const type of LAUNCH_EVENT_TYPES) {
    internals.defineEventHandler(desktop, type);
  }
  let launchTargets = null;
  function getLaunchTargets() {
    if (launchTargets === null) {
      const { urls, files, notifications } = op_desktop_take_launch_targets();
      launchTargets = {
        urls: Object.freeze(urls),
        files: Object.freeze(files),
        notifications: Object.freeze(
          notifications.map((ev) =>
            Object.freeze(notificationResponseDetail(ev))
          ),
        ),
      };
    }
    return launchTargets;
  }
  Object.defineProperties(desktop, {
    launchUrls: {
      get() { return getLaunchTargets().urls; },
      configurable: true,
      enumerable: true,
    },
    launchFiles: {
      get() { return getLaunchTargets().files; },
      configurable: true,
      enumerable: true,
    },
    // Clicks on the app's notifications that arrived before it listened for
    // "notificationresponse": the click that launched it (laufey API 41).
    launchNotificationResponses: {
      get() { return getLaunchTargets().notifications; },
      configurable: true,
      enumerable: true,
    },
  });
  // Who handles the app's deep-link schemes, and registering the app as
  // their handler (the runtime also registers unowned ones at startup).
  // Only schemes declared in desktop.app.deepLinks are accepted.
  Object.defineProperties(desktop, {
    getSchemeOwner: {
      value: function getSchemeOwner(scheme) {
        return op_desktop_get_scheme_owner(String(scheme));
      },
      writable: true,
      configurable: true,
      enumerable: false,
    },
    registerScheme: {
      value: function registerScheme(scheme, options = undefined) {
        // Only a literal `true` takes a scheme over from another app.
        const force = options != null && options.force === true;
        return op_desktop_register_scheme(String(scheme), force);
      },
      writable: true,
      configurable: true,
      enumerable: false,
    },
  });
  // Native passkeys (WebAuthn through the OS platform authenticator), in the
  // @clerk/electron-passkeys wire format: JSON options in, a JSON envelope
  // out. A thin pass-through, so a preload can expose it unchanged as
  // window.__clerk_internal_electron_passkeys.
  function passkeyWindowId(options) {
    const target = options == null ? undefined : options.window;
    if (target === undefined || target === null) return 0; // focused window
    if (typeof target === "number") {
      if (!Number.isInteger(target) || target < 0 || target > 0x7fffffff) {
        throw new TypeError(
          "options.window must be a window id (an integer, 0 to 2^31 - 1)",
        );
      }
      return target;
    }
    if (Object.prototype.isPrototypeOf.call(BrowserWindowPrototype, target)) {
      return target.windowId;
    }
    throw new TypeError("options.window must be a BrowserWindow or a window id");
  }
  async function passkeyRequest(create, optionsJson, options) {
    if (typeof optionsJson !== "string") {
      throw new TypeError("optionsJson must be a string (JSON)");
    }
    return await op_desktop_passkey_request(
      create,
      passkeyWindowId(options),
      optionsJson,
    );
  }
  const passkeys = Object.freeze({
    capabilities: function capabilities() {
      return op_desktop_passkey_capabilities();
    },
    create: function create(optionsJson, options = undefined) {
      return passkeyRequest(true, optionsJson, options);
    },
    get: function get(optionsJson, options = undefined) {
      return passkeyRequest(false, optionsJson, options);
    },
  });
  Object.defineProperty(desktop, "passkeys", {
    value: passkeys,
    writable: false,
    configurable: true,
    enumerable: true,
  });

  // OS auth sessions (laufey API 42): ASWebAuthenticationSession on macOS,
  // a sign-in that ends at a callback URL with a real "cancelled" when the
  // user closes the sheet. Windows and Linux have none (RFC 8252: the system
  // browser), so start() rejects with code "not_supported" there and the
  // caller falls back. Rejections are AuthSessionErrors with a `code`.
  function authSessionError(code, message) {
    const error = new Error(message);
    error.name = "AuthSessionError";
    error.code = code;
    return error;
  }
  const authSession = Object.freeze({
    capabilities: function capabilities() {
      return op_desktop_auth_session_capabilities();
    },
    start: async function start(options) {
      if (options === null || typeof options !== "object") {
        throw new TypeError("options must be an object");
      }
      if (typeof options.url !== "string") {
        throw new TypeError("options.url must be a string");
      }
      const scheme = options.callbackScheme;
      const callbackUrl = options.callbackUrl;
      if ((scheme === undefined) === (callbackUrl === undefined)) {
        throw new TypeError(
          "pass exactly one of options.callbackScheme and options.callbackUrl",
        );
      }
      const callback = scheme !== undefined ? scheme : callbackUrl;
      if (typeof callback !== "string") {
        throw new TypeError(
          scheme !== undefined
            ? "options.callbackScheme must be a string"
            : "options.callbackUrl must be a string",
        );
      }
      if (
        callbackUrl !== undefined &&
        !callbackUrl.toLowerCase().startsWith("https://")
      ) {
        throw new TypeError("options.callbackUrl must be an https URL");
      }
      if (scheme !== undefined && scheme.includes(":")) {
        throw new TypeError(
          "options.callbackScheme is a scheme name (\"myapp\"), not a URL",
        );
      }
      const ephemeral = options.ephemeral === undefined
        ? false
        : options.ephemeral;
      if (typeof ephemeral !== "boolean") {
        throw new TypeError("options.ephemeral must be a boolean");
      }
      const outcome = await op_desktop_auth_session_start(
        passkeyWindowId(options),
        options.url,
        callback,
        ephemeral,
      );
      if (outcome.ok) return { url: outcome.url };
      throw authSessionError(outcome.code, outcome.message);
    },
    // The app gives up on the running session (the page cancelled, a
    // timeout): its sheet closes and its start() rejects with code
    // "cancelled", once. False when no session is running.
    cancel: function cancel() {
      return op_desktop_auth_session_cancel();
    },
  });
  Object.defineProperty(desktop, "authSession", {
    value: authSession,
    writable: false,
    configurable: true,
    enumerable: true,
  });

  // Run native code on the app's UI thread (laufey API 42): AppKit, Win32
  // and GTK objects belong to it. `fn` is a C function `void* (*)(void*)`:
  // a Deno.UnsafeFnPointer or a pointer value. Full trust, so it needs
  // --allow-ffi. Resolves with the return value as a bigint; rejects once
  // the app is quitting (the function was not called).
  //
  // A Deno.UnsafeCallback is refused: it runs JavaScript, so the UI thread
  // would block until the JavaScript thread ran it, while anything the
  // JavaScript thread does that waits for the UI thread (most window calls)
  // deadlocks the app, and a callback closed before the UI thread got to it
  // aborts the process.
  function nativeFunctionPointer(fn) {
    if (fn !== null && typeof fn === "object") {
      if (fn instanceof Deno.UnsafeCallback) {
        throw new TypeError(
          "runOnMainThread runs native code: a Deno.UnsafeCallback (JavaScript) would make the UI thread wait for the JavaScript thread",
        );
      }
      if (fn instanceof Deno.UnsafeFnPointer) return fn.pointer;
    }
    return fn;
  }
  Object.defineProperty(desktop, "runOnMainThread", {
    value: async function runOnMainThread(fn, context = null) {
      const pointer = nativeFunctionPointer(fn);
      if (pointer === null || pointer === undefined) {
        throw new TypeError(
          "fn must be a Deno.UnsafeFnPointer or a non-null pointer",
        );
      }
      if (context !== null && typeof context !== "object") {
        throw new TypeError("context must be a pointer value or null");
      }
      return BigInt(await op_desktop_run_on_main_thread(pointer, context));
    },
    writable: true,
    configurable: true,
    enumerable: false,
  });

  // Native file dialogs (laufey API 39): the OS's own open / save / folder
  // dialogs, shown on the UI thread without blocking the runtime. Electron's
  // option names; absolute paths out, null when the user cancels; an
  // AbortSignal closes the dialog.
  function dialogArgs(windowOrOptions, maybeOptions) {
    let target = undefined;
    let options = windowOrOptions;
    if (
      windowOrOptions != null &&
      Object.prototype.isPrototypeOf.call(BrowserWindowPrototype, windowOrOptions)
    ) {
      target = windowOrOptions;
      options = maybeOptions;
    }
    if (options === undefined || options === null) options = {};
    if (typeof options !== "object") {
      throw new TypeError("dialog options must be an object");
    }
    let windowId = 0; // an app-level dialog
    const w = target !== undefined ? target : options.window;
    if (w !== undefined && w !== null) {
      if (typeof w === "number") {
        if (!Number.isInteger(w) || w < 0 || w > 0x7fffffff) {
          throw new TypeError(
            "options.window must be a window id (an integer, 0 to 2^31 - 1)",
          );
        }
        windowId = w;
      } else if (Object.prototype.isPrototypeOf.call(BrowserWindowPrototype, w)) {
        windowId = w.windowId;
      } else {
        throw new TypeError(
          "options.window must be a BrowserWindow or a window id",
        );
      }
    }
    const str = (key) => {
      const v = options[key];
      if (v === undefined || v === null) return null;
      if (typeof v !== "string") {
        throw new TypeError(`options.${key} must be a string`);
      }
      return v;
    };
    let filters = [];
    if (options.filters !== undefined && options.filters !== null) {
      if (!Array.isArray(options.filters)) {
        throw new TypeError(
          "options.filters must be an array of { name, extensions }",
        );
      }
      filters = options.filters.map((f, i) => {
        if (
          f == null || typeof f.name !== "string" ||
          !Array.isArray(f.extensions) ||
          !f.extensions.every((e) => typeof e === "string")
        ) {
          throw new TypeError(
            `options.filters[${i}] must be { name: string, extensions: string[] }`,
          );
        }
        return { name: f.name, extensions: [...f.extensions] };
      });
    }
    const signal = options.signal;
    if (signal !== undefined && signal !== null && !(signal instanceof AbortSignal)) {
      throw new TypeError("options.signal must be an AbortSignal");
    }
    return {
      windowId,
      title: str("title"),
      defaultPath: str("defaultPath"),
      buttonLabel: str("buttonLabel"),
      filters,
      properties: options.properties,
      signal: signal ?? null,
    };
  }
  function dialogProperties(properties, allowed) {
    if (properties === undefined || properties === null) return new Set();
    if (!Array.isArray(properties)) {
      throw new TypeError("options.properties must be an array");
    }
    for (const p of properties) {
      if (!allowed.includes(p)) {
        throw new TypeError(`Unknown dialog property: ${p}`);
      }
    }
    return new Set(properties);
  }
  async function runFileDialog(request, signal) {
    if (signal) signal.throwIfAborted();
    const rid = op_desktop_file_dialog_open(request);
    const onAbort = () => op_desktop_file_dialog_cancel(rid);
    if (signal) signal.addEventListener("abort", onAbort, { once: true });
    try {
      const result = await op_desktop_file_dialog_wait(rid);
      if (signal && signal.aborted) throw signal.reason;
      switch (result.status) {
        case "accepted": return result.paths;
        case "cancelled": return null;
        case "busy":
          throw new Deno.errors.Busy("Another file dialog is open");
        default:
          throw new Error("The file dialog could not be shown");
      }
    } finally {
      if (signal) signal.removeEventListener("abort", onAbort);
    }
  }
  const OPEN_DIALOG_PROPERTIES = [
    "openFile",
    "openDirectory",
    "multiSelections",
    "showHiddenFiles",
  ];
  const SAVE_DIALOG_PROPERTIES = ["showHiddenFiles"];
  const dialog = Object.freeze({
    showOpenDialog: async function showOpenDialog(
      windowOrOptions = undefined,
      maybeOptions = undefined,
    ) {
      const a = dialogArgs(windowOrOptions, maybeOptions);
      const props = dialogProperties(a.properties, OPEN_DIALOG_PROPERTIES);
      return await runFileDialog({
        save: false,
        windowId: a.windowId,
        title: a.title,
        defaultPath: a.defaultPath,
        buttonLabel: a.buttonLabel,
        filters: a.filters,
        files: props.has("openFile"),
        directories: props.has("openDirectory"),
        multiple: props.has("multiSelections"),
        showHidden: props.has("showHiddenFiles"),
      }, a.signal);
    },
    showSaveDialog: async function showSaveDialog(
      windowOrOptions = undefined,
      maybeOptions = undefined,
    ) {
      const a = dialogArgs(windowOrOptions, maybeOptions);
      const props = dialogProperties(a.properties, SAVE_DIALOG_PROPERTIES);
      const paths = await runFileDialog({
        save: true,
        windowId: a.windowId,
        title: a.title,
        defaultPath: a.defaultPath,
        buttonLabel: a.buttonLabel,
        filters: a.filters,
        files: false,
        directories: false,
        multiple: false,
        showHidden: props.has("showHiddenFiles"),
      }, a.signal);
      return paths === null ? null : paths[0];
    },
  });
  Object.defineProperty(desktop, "dialog", {
    value: dialog,
    writable: false,
    configurable: true,
    enumerable: true,
  });

  // The native clipboard (laufey API 39): text, HTML, PNG images, the
  // formats present, and a "change" event. The OS watcher (on macOS a
  // twice-a-second change-count poll) runs only while a "change" listener
  // (or onchange) is set.
  const clipboardKey = Symbol("DesktopClipboard");
  const privateClipboardChanged = Symbol("Deno_privateClipboardChanged");
  class DesktopClipboard extends EventTarget {
    #listeners = []; // { listener, capture, once }
    #watching = false;
    #onchange = null;
    #onchangeWrapper = null;

    constructor(key) {
      if (key !== clipboardKey) throw new TypeError("Illegal constructor");
      super();
    }

    #sync() {
      const on = this.#listeners.length > 0;
      if (on === this.#watching) return;
      // Turning the watcher on needs --allow-sys (it throws NotCapable
      // without it); only a watcher that started counts as on.
      op_desktop_clipboard_watch(on);
      this.#watching = on;
    }

    #forget(listener, capture) {
      this.#listeners = this.#listeners.filter((l) =>
        !(l.listener === listener && l.capture === capture)
      );
    }

    addEventListener(type, listener, options = undefined) {
      super.addEventListener(type, listener, options);
      if (type !== "change" || listener == null) return;
      const capture = typeof options === "boolean"
        ? options
        : !!(options && options.capture);
      const once = typeof options === "object" && options !== null &&
        !!options.once;
      const signal = typeof options === "object" && options !== null
        ? options.signal
        : undefined;
      if (signal && signal.aborted) return;
      if (
        this.#listeners.some((l) =>
          l.listener === listener && l.capture === capture
        )
      ) return;
      this.#listeners.push({ listener, capture, once });
      if (signal) {
        signal.addEventListener("abort", () => {
          this.#forget(listener, capture);
          this.#sync();
        }, { once: true });
      }
      this.#sync();
    }

    removeEventListener(type, listener, options = undefined) {
      super.removeEventListener(type, listener, options);
      if (type !== "change") return;
      const capture = typeof options === "boolean"
        ? options
        : !!(options && options.capture);
      this.#forget(listener, capture);
      this.#sync();
    }

    get onchange() {
      return this.#onchange;
    }

    set onchange(fn) {
      if (this.#onchangeWrapper) {
        this.removeEventListener("change", this.#onchangeWrapper);
        this.#onchangeWrapper = null;
      }
      this.#onchange = typeof fn === "function" ? fn : null;
      if (this.#onchange) {
        const handler = this.#onchange;
        this.#onchangeWrapper = (ev) => handler.call(this, ev);
        this.addEventListener("change", this.#onchangeWrapper);
      }
    }

    [privateClipboardChanged]() {
      this.dispatchEvent(new Event("change"));
      // `once` listeners are gone now.
      if (this.#listeners.some((l) => l.once)) {
        this.#listeners = this.#listeners.filter((l) => !l.once);
        this.#sync();
      }
    }

    capabilities() {
      return op_desktop_clipboard_capabilities();
    }

    async readText() {
      return (await op_desktop_read_clipboard_text()) ?? "";
    }

    async writeText(text) {
      await op_desktop_write_clipboard_text(String(text));
    }

    async readHTML() {
      return (await op_desktop_read_clipboard_html()) ?? "";
    }

    async writeHTML(html, text = undefined) {
      await op_desktop_write_clipboard_html(
        String(html),
        text === undefined || text === null ? null : String(text),
      );
    }

    async readImage() {
      return (await op_desktop_read_clipboard_image()) ?? null;
    }

    async writeImage(png) {
      if (!(png instanceof Uint8Array)) {
        throw new TypeError("writeImage takes PNG bytes (a Uint8Array)");
      }
      await op_desktop_write_clipboard_image(png);
    }

    async availableFormats() {
      return await op_desktop_read_clipboard_formats();
    }
  }
  const desktopClipboard = new DesktopClipboard(clipboardKey);
  Object.defineProperty(desktop, "clipboard", {
    value: desktopClipboard,
    writable: false,
    configurable: true,
    enumerable: true,
  });
  // Global shortcuts (laufey API 40): system-wide shortcuts that reach the
  // app whichever app has the focus. register() resolves with the canonical
  // accelerator ("Ctrl+Shift+K") once the OS bound it (on Wayland, once the
  // user approved it in the desktop's dialog); each press calls the
  // registration's callback with it and fires a "shortcut" event (detail
  // { accelerator }).
  const shortcutsKey = Symbol("DesktopShortcuts");
  const privateShortcutPressed = Symbol("Deno_privateShortcutPressed");
  function shortcutError(status, accelerator) {
    let err;
    switch (status) {
      case "invalid":
        err = new TypeError(`Invalid accelerator: ${accelerator}`);
        break;
      case "conflict":
        err = new Deno.errors.AlreadyExists(
          `${accelerator} is already taken by another application`,
        );
        break;
      case "already_registered":
        err = new Deno.errors.AlreadyExists(
          `${accelerator} is already registered`,
        );
        break;
      case "not_supported":
        err = new Deno.errors.NotSupported(
          "Global shortcuts are not supported here",
        );
        break;
      case "denied":
        err = new Deno.errors.PermissionDenied(
          `The user declined ${accelerator}`,
        );
        break;
      default:
        err = new Error(`${accelerator} could not be registered`);
    }
    err.code = status;
    return err;
  }
  class DesktopShortcuts extends EventTarget {
    #callbacks = new Map(); // canonical accelerator -> callback | null

    constructor(key) {
      if (key !== shortcutsKey) throw new TypeError("Illegal constructor");
      super();
    }

    capabilities() {
      const caps = op_desktop_system_capabilities();
      return {
        globalShortcuts: caps.globalShortcuts,
        userBinds: caps.shortcutsUserBinds,
      };
    }

    canonicalize(accelerator) {
      return op_desktop_canonical_accelerator(String(accelerator)) ?? null;
    }

    async register(accelerator, callback = undefined) {
      accelerator = String(accelerator);
      if (
        callback !== undefined && callback !== null &&
        typeof callback !== "function"
      ) {
        throw new TypeError("callback must be a function");
      }
      const result = await op_desktop_register_shortcut(accelerator);
      if (result.status !== "ok") {
        throw shortcutError(result.status, accelerator);
      }
      this.#callbacks.set(result.accelerator, callback ?? null);
      return result.accelerator;
    }

    unregister(accelerator) {
      accelerator = String(accelerator);
      const canonical = this.canonicalize(accelerator);
      const removed = op_desktop_unregister_shortcut(accelerator);
      if (canonical !== null) this.#callbacks.delete(canonical);
      return removed;
    }

    unregisterAll() {
      op_desktop_unregister_all_shortcuts();
      this.#callbacks.clear();
    }

    isRegistered(accelerator) {
      const canonical = this.canonicalize(accelerator);
      return canonical !== null &&
        op_desktop_list_shortcuts().includes(canonical);
    }

    list() {
      return op_desktop_list_shortcuts();
    }

    [privateShortcutPressed](accelerator) {
      const callback = this.#callbacks.get(accelerator);
      if (callback) {
        try {
          callback(accelerator);
        } catch (err) {
          reportError(err);
        }
      }
      this.dispatchEvent(new CustomEvent("shortcut", {
        detail: { accelerator },
      }));
    }
  }
  const desktopShortcuts = new DesktopShortcuts(shortcutsKey);
  internals.defineEventHandler(desktopShortcuts, "shortcut");

  // Launch at login (laufey API 40): "enabled", "disabled",
  // "requires-approval" (registered, but the user has to allow it in the
  // system settings) or "not-supported".
  const launchAtLogin = Object.freeze({
    get: async function get() {
      return await op_desktop_get_launch_at_login();
    },
    set: async function set(enabled) {
      if (typeof enabled !== "boolean") {
        throw new TypeError("launchAtLogin.set takes a boolean");
      }
      return await op_desktop_set_launch_at_login(enabled);
    },
  });

  // DevTools (laufey API 40). `enabled` is false when the app was launched
  // with DevTools turned off (LAUFEY_INSPECTABLE=0 / "inspectable": false in
  // laufey-launch.json); then nothing opens them.
  BrowserWindowPrototype.toggleDevtools = function(options = undefined) {
    if (this.isDevtoolsOpen()) {
      this.closeDevtools();
    } else {
      this.openDevtools(options);
    }
  };
  function devtoolsTarget(win) {
    if (!(win instanceof BrowserWindow)) {
      throw new TypeError("Expected a BrowserWindow");
    }
    return win;
  }
  const devtools = Object.freeze({
    get enabled() {
      return op_desktop_devtools_enabled(0);
    },
    open(win, options = undefined) {
      devtoolsTarget(win).openDevtools(options);
    },
    close(win) {
      devtoolsTarget(win).closeDevtools();
    },
    toggle(win, options = undefined) {
      devtoolsTarget(win).toggleDevtools(options);
    },
    isOpen(win) {
      return devtoolsTarget(win).isDevtoolsOpen();
    },
  });
  Object.defineProperties(desktop, {
    shortcuts: {
      value: desktopShortcuts,
      writable: false,
      configurable: true,
      enumerable: true,
    },
    launchAtLogin: {
      value: launchAtLogin,
      writable: false,
      configurable: true,
      enumerable: true,
    },
    devtools: {
      value: devtools,
      writable: false,
      configurable: true,
      enumerable: true,
    },
  });

  // Screens, capabilities and the app's lifetime (laufey API 38).
  internals.defineEventHandler(desktop, "displaychanged");
  internals.defineEventHandler(desktop, "platformfeatureschanged");
  internals.defineEventHandler(desktop, "titlebarpreferenceschanged");
  internals.defineEventHandler(desktop, "beforequit");
  let quitOnLastWindowClosed = true;
  let quitOnLastWindowClosedSet = false;
  function setQuitOnLastWindowClosed(value) {
    quitOnLastWindowClosed = !!value;
    op_desktop_set_quit_on_last_window_closed(quitOnLastWindowClosed);
  }
  Object.defineProperties(desktop, {
    screens: {
      value: function screens() {
        return op_desktop_screens();
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
    getPrimaryScreen: {
      value: function getPrimaryScreen() {
        const all = op_desktop_screens();
        return all.find((s) => s.isPrimary) ?? all[0] ?? null;
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
    // What this session provides (laufey API 45): probed, never guessed
    // from the desktop's name, off the JavaScript thread. A promise of a
    // fresh object per call; null outside a desktop app.
    platformFeatures: {
      value: function platformFeatures() {
        return op_desktop_platform_features();
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
    // How the user set up title bars (laufey API 47), for an app that draws
    // its own: the buttons on each side, the double-click action, the
    // colour scheme. A promise of a fresh object per call; null outside a
    // desktop app.
    titleBarPreferences: {
      value: function titleBarPreferences() {
        return op_desktop_title_bar_preferences();
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
    windowCapabilities: {
      value: function windowCapabilities() {
        return op_desktop_window_capabilities();
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
    quitOnLastWindowClosed: {
      get() { return quitOnLastWindowClosed; },
      set(value) {
        quitOnLastWindowClosedSet = true;
        setQuitOnLastWindowClosed(value);
      },
      configurable: true,
      enumerable: true,
    },
    // Electron's app.quit(): a cancelable "beforequit" on Deno.desktop, then
    // a cancelable "close" on every open window; any preventDefault() aborts
    // (returns false). Otherwise the app shuts down as when its last window
    // closes, and the remaining windows close without another event.
    quit: {
      value: function quit() {
        const beforeQuit = new Event("beforequit", { cancelable: true });
        desktop.dispatchEvent(beforeQuit);
        if (beforeQuit.defaultPrevented) return false;
        for (const win of windows.values()) {
          if (win.isClosed()) continue;
          const closeEvent = new Event("close", { cancelable: true });
          win.dispatchEvent(closeEvent);
          if (closeEvent.defaultPrevented) return false;
        }
        op_desktop_quit();
        return true;
      },
      writable: true,
      configurable: true,
      enumerable: true,
    },
  });
  Object.defineProperty(Deno, "desktop", internals.core.propReadOnly(desktop));

  const TrayPrototype = Tray.prototype;
  Object.setPrototypeOf(TrayPrototype, EventTarget.prototype);
  const privateDesktopTrayDestroy = Symbol.for(
    "Deno_privateDesktopTrayDestroy",
  );

  const trays = new Map();
  const nativeTrayConstructor = Tray;
  const OrigTray = function(...args) {
    const instance = new nativeTrayConstructor(...args);
    trays.set(instance.trayId, instance);
    // A tray app keeps running with no window, unless it decided otherwise
    // (Deno.desktop.quitOnLastWindowClosed set explicitly).
    if (!quitOnLastWindowClosedSet && quitOnLastWindowClosed) {
      setQuitOnLastWindowClosed(false);
    }
    return instance;
  };
  Object.setPrototypeOf(OrigTray, nativeTrayConstructor);
  Object.setPrototypeOf(OrigTray.prototype, nativeTrayConstructor.prototype);
  Deno.Tray = OrigTray;

  TrayPrototype.destroy = function() {
    trays.delete(this.trayId);
    TrayPrototype[privateDesktopTrayDestroy].call(this);
  };
  TrayPrototype[Symbol.dispose] = function() {
    this.destroy();
  };

  internals.defineEventHandler(TrayPrototype, "click");
  internals.defineEventHandler(TrayPrototype, "dblclick");
  internals.defineEventHandler(TrayPrototype, "menuclick");

  // High-level convenience: wire a frameless, non-activating popover window
  // to this tray icon (the classic menu-bar-app pattern). Built entirely on
  // the primitives — `new BrowserWindow({ frameless, noActivate })`,
  // `tray.getBounds()`, the tray "click" event and the window "blur" event.
  TrayPrototype.attachPanel = function(options) {
    if (typeof options === "string") options = { url: options };
    options = options ?? {};
    const width = options.width ?? 360;
    const height = options.height ?? 480;
    const hideOnBlur = options.hideOnBlur ?? true;
    const positionFn = options.position;
    const tray = this;

    const window = new Deno.BrowserWindow({
      width,
      height,
      frameless: true,
      noActivate: true,
      resizable: false,
    });
    window.hide();
    if (options.url != null) window.navigate(options.url);

    let visible = false;
    // Guards the click -> blur -> click toggle race: a tray click on a
    // focused panel blurs it (hiding via the blur handler) *before* the
    // tray "click" fires, which would otherwise immediately re-show it.
    let suppressNextShow = false;

    const place = () => {
      const bounds = tray.getBounds();
      // No bounds (e.g. Linux, where the tray protocol has no geometry):
      // leave the window at its current position.
      if (!bounds) return;
      const pos = positionFn
        ? positionFn(bounds, { width, height })
        : {
          x: Math.round(bounds.x + bounds.width / 2 - width / 2),
          y: Math.round(bounds.y + bounds.height),
        };
      window.setPosition(pos.x, pos.y);
    };

    const show = () => {
      place();
      window.show();
      // Take key focus so the panel is interactive and so losing focus
      // (clicking elsewhere) dismisses it via the blur handler.
      window.focus();
      visible = true;
    };
    const hide = () => {
      window.hide();
      visible = false;
    };
    const toggle = () => {
      if (visible) hide();
      else show();
    };

    const onTrayClick = () => {
      if (suppressNextShow) {
        suppressNextShow = false;
        return;
      }
      toggle();
    };
    tray.addEventListener("click", onTrayClick);

    let onBlur = null;
    if (hideOnBlur) {
      onBlur = () => {
        if (!visible) return;
        hide();
        // If this blur was caused by clicking the tray icon, the tray
        // "click" is about to fire — tell it to stay hidden.
        suppressNextShow = true;
        setTimeout(() => {
          suppressNextShow = false;
        }, 250);
      };
      window.addEventListener("blur", onBlur);
    }

    return {
      window,
      get visible() {
        return visible;
      },
      show,
      hide,
      toggle,
      destroy() {
        tray.removeEventListener("click", onTrayClick);
        if (onBlur) window.removeEventListener("blur", onBlur);
        window.close();
      },
    };
  };

  // --- Web Notifications API ---
  //
  // Backend wants raw PNG bytes for the icon while the Web Notifications
  // API specifies icon as a URL string. We synchronously decode `data:`
  // URLs (the only form a sync constructor can resolve without I/O) and
  // ignore other schemes — the URL is still stored verbatim on the
  // instance so `notification.icon` round-trips per spec.
  function decodeDataUrlSync(url) {
    if (typeof url !== "string" || !url.startsWith("data:")) return null;
    const comma = url.indexOf(",");
    if (comma === -1) return null;
    const meta = url.slice(5, comma);
    const isBase64 = meta.endsWith(";base64");
    const payload = url.slice(comma + 1);
    try {
      if (isBase64) {
        const bin = atob(payload);
        const out = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
        return out;
      }
      return new TextEncoder().encode(decodeURIComponent(payload));
    } catch {
      return null;
    }
  }

  const NotificationPrototype = NotificationNative.prototype;
  Object.setPrototypeOf(NotificationPrototype, EventTarget.prototype);

  const notifications = new Map();
  const MAX_LIVE_NOTIFICATIONS = 1024;
  // An action button was clicked (laufey API 41): `action` is the button's
  // `action` (the Web Notifications NotificationEvent.action).
  class NotificationActionEvent extends Event {
    #action = "";
    get action() { return this.#action; }
    constructor(type, init = {}) {
      super(type, init);
      this.#action = String(init.action ?? "");
    }
  }
  // The most action buttons a notification shows (Windows' limit; laufey
  // passes more on macOS and Linux).
  const MAX_NOTIFICATION_ACTIONS = 5;
  function normalizeNotificationActions(actions) {
    if (actions == null) return [];
    const out = [];
    for (const a of actions) {
      if (a == null) continue;
      const action = String(a.action ?? "");
      const title = String(a.title ?? "");
      if (action === "" || title === "") {
        throw new TypeError(
          "a notification action needs an `action` and a `title`",
        );
      }
      out.push({ action, title });
    }
    return out;
  }
  // `data` travels to the OS as JSON text; one that can't be serialized
  // stays on the object only.
  function notificationDataText(data) {
    if (data === undefined || data === null) return undefined;
    try {
      const text = JSON.stringify(data);
      return typeof text === "string" ? text : undefined;
    } catch {
      return undefined;
    }
  }
  // The Web Notifications API constructor is `new Notification(title, options?)`
  // and shows the notification immediately. The native constructor takes
  // a third arg for pre-decoded icon bytes so the icon URL → bytes step
  // happens on the JS side (sync data: URL decoding only).
  const Notification = function Notification(title, options) {
    if (arguments.length < 1) {
      throw new TypeError(
        "Failed to construct 'Notification': 1 argument required, but only 0 present.",
      );
    }
    const t = String(title);
    const opts = options ?? {};
    const iconBytes = decodeDataUrlSync(opts.icon);
    const extra = {
      actions: normalizeNotificationActions(opts.actions),
      data: notificationDataText(opts.data),
    };
    const instance = new NotificationNative(
      t,
      opts,
      iconBytes ?? undefined,
      extra,
    );
    if (instance.notificationId !== 0) {
      notifications.set(instance.notificationId, instance);
      // A notification left in the notification center never sends its
      // close event: keep the newest ones (as the runtime does), not all.
      while (notifications.size > MAX_LIVE_NOTIFICATIONS) {
        notifications.delete(notifications.keys().next().value);
      }
    } else {
      // Backend didn't show it (no support / failure). The native side
      // already emitted a NotificationError event; nothing to track here.
    }
    return instance;
  };
  Object.setPrototypeOf(Notification, NotificationNative);
  Object.setPrototypeOf(Notification.prototype, NotificationPrototype);
  Notification.prototype.constructor = Notification;

  // Cache of the last status the OS reported. `Notification.permission`
  // is a *synchronous* getter per spec, but the underlying laufey call is
  // async (it's serviced on the UI thread). The cache starts at
  // "default" and updates as `requestPermission()` / permissions.query()
  // resolve. We deliberately do NOT do a startup query — that op
  // dispatches into the laufey backend's UI thread, which may not be
  // pumping when DESKTOP_JS first runs; a hung promise there shouldn't
  // be possible to interfere with the event loop below, but the cost
  // of being defensive is also zero (apps generally read `.permission`
  // only after a user-driven request anyway).
  //
  // Web spec maps laufey's "prompt" status (no decision yet) to "default"
  // for `Notification`, and "prompt" for `navigator.permissions`. The
  // laufey "unsupported" status — emitted when the backend or platform
  // has no permission model — surfaces to JS as a thrown error from
  // requestPermission (most honest) and as "denied" from
  // permissions.query (spec doesn't have an "unsupported" state).
  let cachedNotificationPermission = "default";

  function laufeyToNotificationPermission(s) {
    // "prompt" → "default" per the Notifications spec; "unsupported"
    // is handled by the caller (throws on requestPermission).
    switch (s) {
      case "granted": return "granted";
      case "denied": return "denied";
      case "prompt": return "default";
      default: return "default";
    }
  }

  // Wrap every new descriptor mutation in a try/catch. Anything that
  // throws here would otherwise abort DESKTOP_JS execution and prevent
  // the event-loop IIFE at the bottom of this script from registering,
  // which manifests as "nothing works" (no mouse, no keyboard, no
  // alerts). The catch is logged via console.error so a regression is
  // visible but doesn't take the whole desktop runtime down with it.
  try {
    Object.defineProperties(Notification, {
      permission: {
        get() { return cachedNotificationPermission; },
        enumerable: true,
        configurable: true,
      },
      maxActions: {
        get() {
          return op_desktop_notification_capabilities().actions
            ? MAX_NOTIFICATION_ACTIONS
            : 0;
        },
        enumerable: true,
        configurable: true,
      },
      requestPermission: internals.core.propWritable(function requestPermission(
        cb,
      ) {
        // The Web Notifications spec gates `requestPermission` on a
        // transient user activation. The desktop runtime can't observe
        // renderer activations cleanly (the OS-level UN dialog lives
        // outside Chromium's activation tracking), so we don't enforce.
        const promise = (async () => {
          const s = await op_desktop_request_notification_permission(false);
          if (s === "unsupported") {
            // Honest signaling: this OS / backend has no notification
            // permission model. Throw rather than silently returning a
            // misleading "denied" or "granted".
            throw new TypeError(
              "Notification.requestPermission: not supported by this platform/backend",
            );
          }
          const perm = laufeyToNotificationPermission(s);
          cachedNotificationPermission = perm;
          return perm;
        })();
        if (typeof cb === "function") {
          // Deprecated callback form. Per spec, the callback is invoked
          // with the resolved permission *and* the promise still resolves.
          promise.then(
            (perm) => { try { cb(perm); } catch (_) {} },
            () => { try { cb("denied"); } catch (_) {} },
          );
        }
        return promise;
      }),
    });
  } catch (e) {
    console.error("[deno desktop] failed to install Notification permission API:", e);
  }

  internals.defineEventHandler(NotificationPrototype, "show");
  internals.defineEventHandler(NotificationPrototype, "click");
  internals.defineEventHandler(NotificationPrototype, "action");
  internals.defineEventHandler(NotificationPrototype, "close");
  internals.defineEventHandler(NotificationPrototype, "error");

  Object.defineProperty(globalThis, "Notification", {
    value: Notification,
    writable: true,
    enumerable: false,
    configurable: true,
  });

  // Deno.desktop.notifications (laufey API 41): scheduled notifications, the
  // pending list, cancel, capabilities and quiet authorization. A scheduled
  // notification is identified by its tag; its clicks (and any click on a
  // notification no live Notification object owns) arrive as Deno.desktop's
  // "notificationresponse" event, or in launchNotificationResponses for the
  // click that launched the app.
  const notificationsApi = {
    capabilities() {
      return op_desktop_notification_capabilities();
    },
    async schedule(options) {
      if (options == null || typeof options !== "object") {
        throw new TypeError("schedule() needs an options object");
      }
      const at = options.at instanceof Date ? options.at.getTime()
        : Number(options.at);
      if (!Number.isFinite(at)) {
        throw new TypeError("schedule(): `at` must be a Date or a time in ms");
      }
      const tag = options.tag != null ? String(options.tag)
        : crypto.randomUUID();
      const iconBytes = decodeDataUrlSync(options.icon);
      const accepted = op_desktop_schedule_notification({
        title: String(options.title ?? ""),
        body: options.body != null ? String(options.body) : undefined,
        tag,
        at,
        actions: normalizeNotificationActions(options.actions),
        data: notificationDataText(options.data),
        silent: options.silent != null ? Boolean(options.silent) : undefined,
        requireInteraction: options.requireInteraction != null
          ? Boolean(options.requireInteraction)
          : undefined,
      }, iconBytes ?? undefined);
      if (!accepted) {
        throw new Deno.errors.NotSupported(
          "Scheduled notifications are not available here",
        );
      }
      return tag;
    },
    async getScheduled() {
      const list = await op_desktop_list_scheduled_notifications();
      return list.map((n) => {
        let data;
        if (typeof n.data === "string") {
          try {
            data = JSON.parse(n.data);
          } catch {
            data = n.data;
          }
        }
        return {
          tag: n.tag,
          title: n.title,
          body: n.body,
          at: new Date(n.at),
          data,
          actions: n.actions,
        };
      });
    },
    cancel(tag) {
      op_desktop_cancel_notification(String(tag));
    },
    async requestPermission(options = undefined) {
      const s = await op_desktop_request_notification_permission(
        Boolean(options?.provisional),
      );
      if (s === "granted") cachedNotificationPermission = "granted";
      else if (s === "denied") cachedNotificationPermission = "denied";
      return s;
    },
  };
  Object.defineProperty(desktop, "notifications", {
    value: Object.freeze(notificationsApi),
    configurable: true,
    enumerable: true,
  });
  Object.defineProperty(desktop, "menuCapabilities", {
    value: function menuCapabilities() {
      return op_desktop_menu_capabilities();
    },
    writable: true,
    configurable: true,
    enumerable: false,
  });

  // --- navigator.permissions.query (minimal) ---
  //
  // Spec surface: `navigator.permissions.query({name})` returns a
  // Promise<PermissionStatus> where `PermissionStatus` extends EventTarget
  // and exposes a readonly `state` plus an `onchange` slot. The desktop
  // runtime today only routes `notifications` through laufey; other names
  // resolve to "denied" (Chrome's behavior for unknown / unsupported
  // names — closer to honest than "prompt" for things we can't fulfill).
  //
  // Note: we don't fire `change` events. laufey has no change-notification
  // channel for permissions, and the cached decision only flips when the
  // user goes through System Settings (rare, manual, not worth polling).
  class PermissionStatus extends EventTarget {
    #name;
    #state;
    #onchange = null;
    constructor(name, state) {
      super();
      this.#name = name;
      this.#state = state;
    }
    get name() { return this.#name; }
    get state() { return this.#state; }
    get status() { return this.#state; } // legacy alias kept by some libs
    get onchange() { return this.#onchange; }
    set onchange(v) { this.#onchange = typeof v === "function" ? v : null; }
  }

  function laufeyToPermissionsApiState(s) {
    // Spec maps "prompt" through verbatim; "unsupported" has no spec
    // analog so we return "denied" — query() shouldn't throw, but we
    // shouldn't lie and say "granted" either.
    switch (s) {
      case "granted": return "granted";
      case "denied": return "denied";
      case "prompt": return "prompt";
      default: return "denied";
    }
  }

  const permissionsImpl = {
    async query(descriptor) {
      if (descriptor == null || typeof descriptor !== "object") {
        throw new TypeError(
          "Failed to execute 'query' on 'Permissions': descriptor required",
        );
      }
      const name = String(descriptor.name);
      if (name === "notifications") {
        // No side effects per spec — never call request_*.
        const s = await op_desktop_query_notification_permission();
        // Keep Notification.permission's cache in sync: a permissions.query
        // result is authoritative and lets the synchronous getter report
        // a current value without us needing a second roundtrip.
        if (s !== "unsupported") {
          cachedNotificationPermission = laufeyToNotificationPermission(s);
        }
        return new PermissionStatus(name, laufeyToPermissionsApiState(s));
      }
      // Unknown / unrouted name. Chrome returns "denied" for descriptors
      // it doesn't recognize; mimic that rather than throwing.
      return new PermissionStatus(name, "denied");
    },
  };

  // Plug into globalThis.navigator. The base Deno runtime defines
  // `navigator` without a `permissions` slot — add ours, but don't
  // clobber the object if it's missing entirely (defensive against
  // future-Deno changes). Wrapped: a failure here must not abort the
  // event-loop IIFE below, because that's what drives all input.
  try {
    if (typeof navigator === "object" && navigator != null) {
      Object.defineProperty(navigator, "permissions", {
        value: permissionsImpl,
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
    Object.defineProperty(globalThis, "PermissionStatus", {
      value: PermissionStatus,
      writable: true,
      enumerable: false,
      configurable: true,
    });
  } catch (e) {
    console.error("[deno desktop] failed to install navigator.permissions:", e);
  }

  // --- navigator.clipboard (text only) ---
  //
  // Spec surface: `navigator.clipboard` is a `Clipboard` (extends EventTarget)
  // exposing async `readText()` / `writeText()`. The ops behind them are
  // genuinely async: laufey's clipboard calls block their calling thread, and
  // on X11/Wayland a read is serviced by whichever app owns the selection, so
  // an unresponsive owner would otherwise freeze the whole runtime behind a
  // Promise that looks like it couldn't. They reject rather than resolve if
  // that owner never answers — `""` is indistinguishable from an empty
  // clipboard, and a resolved `writeText()` has to mean the write happened.
  //
  // The richer `read()` / `write()` (`ClipboardItem` / arbitrary MIME types)
  // aren't backed by laufey, so they're omitted rather than stubbed. Per spec
  // the read/write are gated on the `clipboard-read` / `clipboard-write`
  // permissions, but laufey has no clipboard permission model, so access
  // isn't gated here (mirroring how the desktop Notification surface
  // degrades).
  const webidl = internals.webidl;
  class Clipboard extends EventTarget {
    constructor() {
      super();
      webidl.illegalConstructor();
    }

    async readText() {
      webidl.assertBranded(this, ClipboardPrototype);
      return (await op_desktop_read_clipboard_text()) ?? "";
    }

    async writeText(data) {
      webidl.assertBranded(this, ClipboardPrototype);
      const prefix = "Failed to execute 'writeText' on 'Clipboard'";
      webidl.requiredArguments(arguments.length, 1, prefix);
      data = webidl.converters["DOMString"](data, prefix, "Argument 1");
      await op_desktop_write_clipboard_text(data);
    }
  }
  webidl.configureInterface(Clipboard);
  const ClipboardPrototype = Clipboard.prototype;

  try {
    const clipboard = webidl.createBranded(Clipboard);
    // createBranded skips the constructor, so initialize the EventTarget
    // internal slots explicitly (see ext/web/02_event.js setEventTargetData).
    internals.setEventTargetData(clipboard);

    if (typeof navigator === "object" && navigator != null) {
      // Install as a prototype getter (as in browsers) rather than an own
      // data property, asserting the receiver is a real Navigator.
      const NavigatorPrototype = Object.getPrototypeOf(navigator);
      Object.defineProperty(NavigatorPrototype, "clipboard", {
        get() {
          webidl.assertBranded(this, NavigatorPrototype);
          return clipboard;
        },
        enumerable: true,
        configurable: true,
      });
    }
    Object.defineProperty(globalThis, "Clipboard", {
      value: Clipboard,
      writable: true,
      enumerable: false,
      configurable: true,
    });
  } catch (e) {
    console.error("[deno desktop] failed to install navigator.clipboard:", e);
  }

  // Start polling loops immediately. Use core.unrefOpPromise so these
  // pending ops don't block event loop completion (e.g. the pre-module
  // tick used by HMR, or module evaluation with top-level await).
  const { unrefOpPromise } = internals.core;

  const FILE_DROP_EVENTS = {
    __proto__: null,
    enter: "dragenter",
    over: "dragover",
    leave: "dragleave",
    drop: "drop",
  };

  // Single polling loop for all native desktop events.
  (async () => {
    while (true) {
      try {
        const p = op_desktop_recv_event();
        unrefOpPromise(p);
        const ev = await p;
        if (ev == null) break;
        switch (ev.kind) {
          case "appMenuClick": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new CustomEvent("menuclick", { detail: { id: ev.id } }));
            break;
          }
          case "contextMenuClick": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            contextMenuChoice.set(target, ev.id);
            target.dispatchEvent(new CustomEvent("contextmenuclick", { detail: { id: ev.id } }));
            break;
          }
          case "contextMenuClose": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            contextMenuClosed(target);
            break;
          }
          case "keyboardEvent": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new KeyboardEvent(ev.type, {
              key: ev.key,
              code: ev.code,
              shiftKey: ev.shift,
              ctrlKey: ev.control,
              altKey: ev.alt,
              metaKey: ev.meta,
              repeat: ev.repeat,
            }));
            break;
          }
          case "bindCall": {
            const callbacks = windowBindCallbacks.get(ev.windowId);
            const binding = callbacks?.get(ev.name);
            const fn_ = binding?.fn;
            if (!fn_) {
              op_desktop_reject_bind_call(ev.callId, "No callback bound for: " + ev.name);
              break;
            }
            // Run async so it doesn't block the event loop
            (async () => {
              try {
                const args = Array.isArray(ev.args) ? ev.args : [];
                if (bindingTrace) {
                  console.debug("[binding:call]", ev.name, ":" + ev.callId, args);
                }
                // The runtime already refused documents the binding doesn't
                // trust; `withCaller` tells the handler which one called.
                const result = binding.withCaller
                  ? await fn_({ origin: ev.origin, windowId: ev.windowId }, ...args)
                  : await fn_(...args);
                if (bindingTrace) {
                  console.debug("[binding:return]", ev.name, ":" + ev.callId, result);
                }
                op_desktop_resolve_bind_call(ev.callId, result ?? null);
              } catch (e) {
                if (bindingTrace) {
                  console.debug("[binding:error]", ev.name, ":" + ev.callId, String(e));
                }
                op_desktop_reject_bind_call(ev.callId, String(e));
              }
            })();
            break;
          }
          case "mouseClick": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            const bit = buttonBit(ev.button);
            let buttons = windowButtons.get(ev.windowId) ?? 0;
            if (ev.state === "pressed") {
              buttons |= bit;
              windowButtons.set(ev.windowId, buttons);
              target.dispatchEvent(new MouseEvent(
                "mousedown",
                mouseInit(target, ev, { buttons, detail: ev.clickCount }),
              ));
            } else {
              buttons &= ~bit;
              windowButtons.set(ev.windowId, buttons);
              const init = mouseInit(target, ev, {
                buttons,
                detail: ev.clickCount,
              });
              target.dispatchEvent(new MouseEvent("mouseup", init));
              if (ev.button === 0) {
                target.dispatchEvent(new MouseEvent("click", init));
                if (ev.clickCount >= 2) {
                  target.dispatchEvent(new MouseEvent("dblclick", init));
                }
              }
            }
            break;
          }
          case "mouseMove": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new MouseEvent(
              "mousemove",
              mouseInit(target, ev, {
                buttons: windowButtons.get(ev.windowId) ?? 0,
              }),
            ));
            break;
          }
          case "wheel": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new WheelEvent("wheel", {
              ...mouseInit(target, ev, {
                buttons: windowButtons.get(ev.windowId) ?? 0,
              }),
              deltaX: ev.deltaX,
              deltaY: ev.deltaY,
              deltaMode: ev.deltaMode,
            }));
            break;
          }
          case "cursorEnterLeave": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new MouseEvent(
              ev.entered ? "mouseenter" : "mouseleave",
              mouseInit(target, ev, {
                buttons: windowButtons.get(ev.windowId) ?? 0,
              }),
            ));
            break;
          }
          case "focusChanged": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new FocusEvent(ev.focused ? "focus" : "blur"));
            break;
          }
          case "windowResize": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            MediaQueryList.reeval(target);
            target.dispatchEvent(new CustomEvent("resize", {
              detail: { width: ev.width, height: ev.height },
            }));
            break;
          }
          case "windowMove": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            MediaQueryList.reeval(target);
            target.dispatchEvent(new CustomEvent("move", {
              detail: { x: ev.x, y: ev.y },
            }));
            break;
          }
          case "pageLoad": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            target.dispatchEvent(new Event("load"));
            break;
          }
          case "closeRequested": {
            // Cancelable: preventDefault() keeps the window open (the app
            // later calls close(), which closes without another event). The
            // runtime closes it if this answer never comes (5 s).
            const target = windows.get(ev.windowId);
            let prevented = false;
            try {
              if (target) {
                const closeEvent = new Event("close", { cancelable: true });
                target.dispatchEvent(closeEvent);
                prevented = closeEvent.defaultPrevented;
              }
            } finally {
              op_desktop_close_reply(ev.windowId, prevented);
            }
            break;
          }
          case "windowState": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            dispatchWindowStateEvents(target, ev.state, ev.previous);
            break;
          }
          case "displayChanged": {
            desktop.dispatchEvent(new Event("displaychanged"));
            break;
          }
          case "titleBarPreferencesChanged": {
            // laufey API 47: read titleBarPreferences() again.
            desktop.dispatchEvent(new Event("titlebarpreferenceschanged"));
            break;
          }
          case "platformFeaturesChanged": {
            // laufey API 45: a tray host appeared or went away; read
            // platformFeatures() again (and create the tray once trayHost
            // is true).
            desktop.dispatchEvent(new Event("platformfeatureschanged"));
            break;
          }
          case "fileDrop": {
            const target = windows.get(ev.windowId);
            if (!target) break;
            const type = FILE_DROP_EVENTS[ev.phase];
            if (!type) break;
            target.dispatchEvent(new CustomEvent(type, {
              detail: Object.freeze({
                paths: ev.paths == null ? null : Object.freeze([...ev.paths]),
                count: ev.count,
                x: ev.x,
                y: ev.y,
              }),
            }));
            break;
          }
          case "clipboardChange": {
            desktopClipboard[privateClipboardChanged]();
            break;
          }
          case "shortcut": {
            desktopShortcuts[privateShortcutPressed](ev.accelerator);
            break;
          }
          case "runtimeError": {
            dispatchEvent(new ErrorEvent("error", {
              message: ev.message,
              error: new Error(ev.message),
            }));
            break;
          }
          case "dockMenuClick": {
            for (const d of docks) {
              d.dispatchEvent(new CustomEvent("menuclick", {
                detail: { id: ev.id },
              }));
            }
            break;
          }
          case "dockReopen": {
            for (const d of docks) {
              d.dispatchEvent(new CustomEvent("reopen", {
                detail: { hasVisibleWindows: ev.hasVisibleWindows },
              }));
            }
            break;
          }
          case "openUrl":
          case "openFile":
          case "secondInstance":
          case "notificationResponse": {
            dispatchLaunchEvent(ev);
            break;
          }
          case "trayClick": {
            const target = trays.get(ev.trayId);
            if (!target) break;
            target.dispatchEvent(new MouseEvent("click"));
            break;
          }
          case "trayDoubleClick": {
            const target = trays.get(ev.trayId);
            if (!target) break;
            target.dispatchEvent(new MouseEvent("dblclick"));
            break;
          }
          case "trayMenuClick": {
            const target = trays.get(ev.trayId);
            if (!target) break;
            target.dispatchEvent(new CustomEvent("menuclick", {
              detail: { id: ev.id },
            }));
            break;
          }
          case "notificationShow": {
            const target = notifications.get(ev.notificationId);
            if (!target) break;
            target.dispatchEvent(new Event("show"));
            break;
          }
          case "notificationClick": {
            const target = notifications.get(ev.notificationId);
            if (!target) break;
            target.dispatchEvent(new Event("click"));
            break;
          }
          case "notificationAction": {
            const target = notifications.get(ev.notificationId);
            if (!target) break;
            target.dispatchEvent(new NotificationActionEvent("action", {
              action: ev.action,
            }));
            break;
          }
          case "windowClosed": {
            // Gone for good: forget what was kept per window (the
            // registry held every BrowserWindow ever created).
            windows.delete(ev.windowId);
            windowBindCallbacks.delete(ev.windowId);
            windowButtons.delete(ev.windowId);
            break;
          }
          case "notificationClose": {
            const target = notifications.get(ev.notificationId);
            notifications.delete(ev.notificationId);
            if (!target) break;
            target.dispatchEvent(new Event("close"));
            break;
          }
          case "notificationError": {
            // notificationId === 0 means the backend rejected the show
            // before any instance was registered. Per spec, errors only
            // fire on the instance — best-effort dispatch to whichever
            // instance is registered under that id (or none for id 0).
            const target = notifications.get(ev.notificationId);
            if (!target) break;
            target.dispatchEvent(new Event("error"));
            break;
          }
        }
      } catch (e) {
        console.error("Desktop event loop error:", e?.stack ?? e);
      }
    }
  })();
})();
"#;

/// JS code that initializes auto-update APIs. Executed separately so
/// version and rollback state can be baked in as literals.
pub fn desktop_auto_update_js(
  version: Option<&str>,
  rolled_back: bool,
  release_base_url: Option<&str>,
) -> String {
  format!(
    r#"(() => {{
  const {{
    op_desktop_apply_patch,
    op_desktop_verify_ed25519,
    op_desktop_confirm_update,
  }} = Deno[Deno.internal].core.ops;
  const {{ propReadOnly, propWritable }} = Deno[Deno.internal].core;

  const _version = {version};
  const _rolledBack = {rolled_back};
  const _releaseBaseUrl = {release_base_url};

  const ROLLBACK_REASON = "Update failed to start, rolled back.";

  if (!_rolledBack) {{
    op_desktop_confirm_update();
  }}

  let autoUpdateTimer = null;

  function isHttpsUrl(u) {{
    try {{
      const parsed = new URL(u);
      return parsed.protocol === "https:";
    }} catch {{
      return false;
    }}
  }}

  function autoUpdate(urlOrOpts) {{
    const opts = typeof urlOrOpts === "string"
      ? {{ url: urlOrOpts }}
      : (urlOrOpts ?? {{}});
    const {{
      url = _releaseBaseUrl,
      interval,
      onUpdateReady,
      onRollback,
      publicKey,
    }} = opts;

    if (_rolledBack && typeof onRollback === "function") {{
      queueMicrotask(() => {{
        try {{ onRollback(ROLLBACK_REASON); }} catch (e) {{
          console.error("Deno.autoUpdate onRollback threw:", e);
        }}
      }});
    }}

    if (!_version) {{
      console.warn("Deno.autoUpdate: no version in deno.json, skipping");
      return;
    }}
    if (typeof url !== "string" || url.length === 0) {{
      console.warn("Deno.autoUpdate: missing 'url' option, skipping");
      return;
    }}
    if (!isHttpsUrl(url)) {{
      console.error(
        "Deno.autoUpdate: refusing non-https url (got %s); ignoring.", url,
      );
      return;
    }}

    const base = url.replace(/\/$/, "");
    const te = new TextEncoder();

    const check = async () => {{
      try {{
        const resp = await fetch(base + "/latest.json", {{
          cache: "no-store",
          redirect: "error",
        }});
        if (!resp.ok) return;
        const manifestText = await resp.text();
        let manifest;
        try {{
          manifest = JSON.parse(manifestText);
        }} catch {{
          console.warn("Deno.autoUpdate: latest.json is not valid JSON");
          return;
        }}
        if (manifest.version === _version) return;

        if (publicKey) {{
          const sig = manifest.signature;
          if (typeof sig !== "string" || !sig) {{
            console.error(
              "Deno.autoUpdate: publicKey configured but manifest has no signature",
            );
            return;
          }}
          // Signature is computed over the manifest with the `signature` field
          // removed, serialized canonically. To avoid depending on a JCS
          // implementation, signers must put the signature on a top-level
          // `signature` field and include the rest of the manifest verbatim
          // under a `signed` field (string). We then verify over that string.
          const signed = manifest.signed;
          if (typeof signed !== "string") {{
            console.error(
              "Deno.autoUpdate: signed manifest must include a `signed` string field",
            );
            return;
          }}
          if (!op_desktop_verify_ed25519(publicKey, sig, te.encode(signed))) {{
            console.error("Deno.autoUpdate: manifest signature verification failed");
            return;
          }}
          // Re-parse the signed payload — only its contents are trusted.
          try {{
            manifest = JSON.parse(signed);
          }} catch {{
            console.error("Deno.autoUpdate: signed payload is not valid JSON");
            return;
          }}
          if (manifest.version === _version) return;
        }}

        const patchEntry = manifest.patches?.[_version];
        if (!patchEntry) {{
          console.warn("Deno.autoUpdate: no patch available for",
            _version, "->", manifest.version);
          return;
        }}
        // Accept either a string (legacy/unsafe) or {{ name, sha256 }}. The
        // SHA-256 is required — Rust will reject the patch otherwise.
        const patchName = typeof patchEntry === "string"
          ? patchEntry
          : patchEntry?.name;
        const patchSha256 = typeof patchEntry === "object"
          ? patchEntry?.sha256
          : undefined;
        if (!patchName) {{
          console.error("Deno.autoUpdate: malformed patch entry");
          return;
        }}
        if (typeof patchSha256 !== "string" || patchSha256.length !== 64) {{
          console.error(
            "Deno.autoUpdate: manifest patch entry must include sha256",
          );
          return;
        }}
        const patchResp = await fetch(base + "/" + patchName, {{
          cache: "no-store",
          redirect: "error",
        }});
        if (!patchResp.ok) return;
        const patchBytes = new Uint8Array(await patchResp.arrayBuffer());
        op_desktop_apply_patch(patchBytes, patchSha256);
        if (typeof onUpdateReady === "function") {{
          try {{ onUpdateReady(manifest.version); }} catch (e) {{
            console.error("Deno.autoUpdate onUpdateReady threw:", e);
          }}
        }}
        if (autoUpdateTimer) {{
          clearInterval(autoUpdateTimer);
          autoUpdateTimer = null;
        }}
      }} catch (e) {{
        console.warn("Deno.autoUpdate: check failed:", e.message);
      }}
    }};

    setTimeout(check, 1000);
    if (interval) {{
      autoUpdateTimer = setInterval(check, interval);
    }}
  }}

  Object.defineProperties(Deno, {{
    desktopVersion: propReadOnly(_version),
    autoUpdate: propWritable(autoUpdate),
  }});
}})();
"#,
    version = serde_json::to_string(&version).unwrap(),
    rolled_back = if rolled_back { "true" } else { "false" },
    release_base_url = serde_json::to_string(&release_base_url).unwrap(),
  )
}

/// JS code that initializes error reporting. Installs `"error"` and
/// `"unhandledrejection"` listeners that show a native alert and
/// optionally POST error reports to a configured URL.
pub fn desktop_error_reporting_js(
  url: Option<&str>,
  version: Option<&str>,
) -> String {
  format!(
    r#"(() => {{
  const {{ op_desktop_alert_async, op_desktop_send_error_report }} = Deno[Deno.internal].core.ops;
  const _errorReportingUrl = {url};
  const _appVersion = {version};
  // Set once the first error has taken over the exit. The dialog it shows
  // is what keeps the process alive, so later errors only log and report —
  // which also keeps at most one error dialog on screen rather than
  // stacking one per rejection.
  let _exiting = false;

  function handleError(ev, err, message, stack) {{
    // Always reach stderr first: the dialog below is best-effort, and in
    // headless/hidden-window runs it's the only place the error surfaces
    // at all (#36393).
    //
    // Log the error object itself when there is one. `preventDefault()`
    // below suppresses Deno's own uncaught-error output, and passing the
    // object keeps that path's formatting (console.error inspects an Error
    // into its formatted stack); flattening to `String(message)` plus a raw
    // stack string would hand a developer watching a terminal strictly less
    // than they get today.
    if (err !== null && err !== undefined) {{
      console.error("Uncaught (desktop):", err);
    }} else {{
      console.error("Uncaught (desktop):", String(message));
      if (stack) console.error(String(stack));
    }}

    // The report goes out off the JavaScript thread; exiting waits for it
    // (it is bounded: 5 s for HTTPS).
    let reported = Promise.resolve();
    if (_errorReportingUrl) {{
      const body = JSON.stringify({{
        version: 1,
        message: String(message),
        stack: stack ?? null,
        appVersion: _appVersion,
        timestamp: new Date().toISOString(),
        platform: Deno.build.os,
        arch: Deno.build.arch,
      }});
      // The destination is not passed from JS — the op reads the
      // operator-configured `error_reporting_url` from native state so an
      // untrusted caller can't retarget it. `_errorReportingUrl` here only
      // gates whether there's anything to report.
      reported = op_desktop_send_error_report(body).catch(() => {{}});
    }}

    // Take over the default handling. Letting this listener return without
    // preventing it tears the runtime down immediately, which would cut the
    // dialog off before it appeared — the old blocking `alert` was what held
    // the process open. Instead the dialog goes up on its own thread, the
    // event loop keeps running (timers, servers and the SIGTERM handler stay
    // live, which is the #36393 fix), and we exit once it's dismissed.
    ev.preventDefault();
    if (_exiting) return;
    _exiting = true;

    let shown;
    try {{
      shown = op_desktop_alert_async("Application Error", String(message));
    }} catch (_) {{
      reported.then(() => Deno.exit(1));
      return;
    }}
    // Exit on rejection too: a dialog we can't show must not strand the app.
    Promise.allSettled([shown, reported]).then(() => Deno.exit(1));
  }}

  addEventListener("error", (ev) => {{
    if (ev.defaultPrevented) return;
    const err = ev.error;
    handleError(
      ev,
      err,
      err?.message ?? ev.message ?? "Unknown error",
      err?.stack ?? null,
    );
  }});

  addEventListener("unhandledrejection", (ev) => {{
    if (ev.defaultPrevented) return;
    const err = ev.reason;
    handleError(
      ev,
      err,
      err?.message ?? String(err ?? "Unhandled promise rejection"),
      err?.stack ?? null,
    );
  }});
}})();
"#,
    url = serde_json::to_string(&url).unwrap(),
    version = serde_json::to_string(&version).unwrap(),
  )
}

pub use deno_runtime::ops::desktop::DesktopEvent;
pub use deno_runtime::ops::desktop::DesktopEventReceiver;
pub use deno_runtime::ops::desktop::DesktopEventSender;
pub use deno_runtime::ops::desktop::DesktopEventTx;
pub use deno_runtime::ops::desktop::DesktopLaunchInbox;
pub use deno_runtime::ops::desktop::InitialWindowId;
pub use deno_runtime::ops::desktop::PendingBindCall;
pub use deno_runtime::ops::desktop::PendingBindResponses;
pub use deno_runtime::ops::desktop::create_desktop_event_channel;
pub use deno_runtime::ops::desktop::register_bind_call;

/// Place the DesktopApi and optional AutoUpdateState into OpState.
/// The ops are already registered in the snapshot; this just provides
/// the runtime implementation.
pub fn init_desktop_state(
  state: &mut OpState,
  api: Box<dyn DesktopApi>,
  auto_update: Option<AutoUpdateState>,
) {
  let api: Arc<dyn DesktopApi> = Arc::from(api);
  state.put::<Arc<dyn DesktopApi>>(api);
  if let Some(au) = auto_update {
    state.put::<AutoUpdateState>(au);
  }
}

pub use deno_lib::util::net::allocate_random_port;

#[cfg(test)]
mod tests {
  use super::DESKTOP_JS;
  use super::desktop_auto_update_js;
  use super::desktop_error_reporting_js;

  // --- DESKTOP_JS structural invariants ---
  //
  // DESKTOP_JS is an 800+ line string baked into the binary. We can't
  // cheaply exec it in a v8 isolate from here, but the asserts below
  // pin the regressions that motivated this whole fix: the "Deno.env
  // throws NotCapable and aborts the IIFE" bug from May 2026.

  #[test]
  fn desktop_js_window_api_is_wired() {
    // The close event is cancelable and always answered, even when a
    // listener throws (`finally`), so the runtime never waits out the 5 s
    // timeout for a responsive app.
    assert!(DESKTOP_JS.contains(r#"new Event("close", { cancelable: true })"#));
    assert!(DESKTOP_JS.contains(
      "} finally {\n              op_desktop_close_reply(ev.windowId, prevented);"
    ));
    // State changes become the Electron-style events.
    for ev in [
      "\"minimize\"",
      "\"restore\"",
      "\"maximize\"",
      "\"unmaximize\"",
      "\"enterfullscreen\"",
      "\"leavefullscreen\"",
    ] {
      assert!(
        DESKTOP_JS.contains(&format!(
          "defineEventHandler(BrowserWindowPrototype, {ev})"
        )),
        "{ev}"
      );
    }
    assert!(DESKTOP_JS.contains("case \"windowState\":"));
    assert!(DESKTOP_JS.contains("case \"displayChanged\":"));
    assert!(DESKTOP_JS.contains("new Event(\"displaychanged\")"));
    assert!(DESKTOP_JS.contains("case \"platformFeaturesChanged\":"));
    assert!(DESKTOP_JS.contains("new Event(\"platformfeatureschanged\")"));
    assert!(DESKTOP_JS.contains("case \"titleBarPreferencesChanged\":"));
    assert!(DESKTOP_JS.contains("new Event(\"titlebarpreferenceschanged\")"));
    assert!(DESKTOP_JS.contains(
      "internals.defineEventHandler(desktop, \"platformfeatureschanged\")"
    ));
    // quit(): beforequit, then every window's close, any cancel aborts.
    assert!(
      DESKTOP_JS.contains(r#"new Event("beforequit", { cancelable: true })"#)
    );
    assert!(
      DESKTOP_JS.contains("if (closeEvent.defaultPrevented) return false;")
    );
    assert!(DESKTOP_JS.contains("op_desktop_quit();"));
    // The tray-only rule: a Tray keeps the app alive unless the app chose.
    assert!(
      DESKTOP_JS.contains(
        "if (!quitOnLastWindowClosedSet && quitOnLastWindowClosed) {"
      )
    );
    for api in [
      "screens:",
      "getPrimaryScreen:",
      "windowCapabilities:",
      "quitOnLastWindowClosed:",
      "quit:",
      "BrowserWindowPrototype.setTitleBarStyle",
      "BrowserWindowPrototype.setWindowButtonPosition",
      "BrowserWindowPrototype.setBackgroundMaterial",
      "BrowserWindowPrototype.setVibrancy",
      "BrowserWindowPrototype.getScreen",
      "applyWindowOptions(instance, args[0]);",
    ] {
      assert!(DESKTOP_JS.contains(api), "{api}");
    }
  }

  #[test]
  fn desktop_js_wraps_binding_trace_env_read_in_try_catch() {
    // The original bug: `Deno.env.get("DENO_DESKTOP_MUX_WS")` threw
    // NotCapable when --allow-env wasn't granted, aborting the rest of
    // DESKTOP_JS. The fix is to wrap that read in try/catch and let it
    // soft-fail. A regression that removed the try/catch would
    // reintroduce the "blank window where nothing works" failure mode.
    let near = locate_around(DESKTOP_JS, "DENO_DESKTOP_MUX_WS");
    assert!(
      near.contains("try {") || near.contains("try{"),
      "DENO_DESKTOP_MUX_WS read must be wrapped in try/catch; got:\n{near}"
    );
  }

  #[test]
  fn desktop_js_installs_alert_confirm_prompt_overrides() {
    // The renderer reaches `op_desktop_alert/confirm/prompt` through
    // these globalThis overrides; the assignment lines must survive.
    // Each must reference its op so a stub/no-op replacement would
    // fail the check.
    assert!(DESKTOP_JS.contains("op_desktop_alert"));
    assert!(DESKTOP_JS.contains("op_desktop_confirm"));
    assert!(DESKTOP_JS.contains("op_desktop_prompt"));
    assert!(DESKTOP_JS.contains("globalThis"));
  }

  #[test]
  fn desktop_js_installs_recv_event_loop() {
    // The event-loop IIFE at the bottom polls `op_desktop_recv_event`
    // and dispatches to BrowserWindow listeners. Without this code path
    // mouse / keyboard / focus / resize events would never reach JS,
    // exactly the symptom from the original bug report.
    assert!(
      DESKTOP_JS.contains("op_desktop_recv_event"),
      "DESKTOP_JS must call op_desktop_recv_event"
    );
  }

  #[test]
  fn desktop_js_installs_notification_permission_getter() {
    // Notification.permission is a synchronous spec-mandated getter.
    // A regression that turned the property definition into a plain
    // value would break feature-detection in user code.
    assert!(DESKTOP_JS.contains("Notification"));
    assert!(DESKTOP_JS.contains("permission"));
    assert!(DESKTOP_JS.contains("requestPermission"));
  }

  #[test]
  fn desktop_js_parses() {
    // Compiles the whole script without running it (its body needs the
    // runtime's internals): a syntax error anywhere would otherwise only
    // show at app start.
    // V8 posts delayed tasks; JsRuntime needs a tokio runtime for them.
    let tokio = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .unwrap();
    let _guard = tokio.enter();
    let mut runtime = deno_core::JsRuntime::new(Default::default());
    let source = format!(
      "new Function({});",
      serde_json::to_string(DESKTOP_JS).unwrap()
    );
    runtime
      .execute_script("desktop_js_parses", source)
      .expect("DESKTOP_JS has a syntax error");
  }

  #[test]
  fn desktop_js_forgets_closed_windows_and_old_notifications() {
    let closed = DESKTOP_JS
      .split("case \"windowClosed\": {")
      .nth(1)
      .expect("a windowClosed case");
    let closed = &closed[..closed.find("break;").unwrap()];
    for map in ["windows", "windowBindCallbacks", "windowButtons"] {
      assert!(
        closed.contains(&format!("{map}.delete(ev.windowId);")),
        "{map}"
      );
    }
    assert!(DESKTOP_JS.contains("const MAX_LIVE_NOTIFICATIONS = 1024;"));
    assert!(
      DESKTOP_JS
        .contains("while (notifications.size > MAX_LIVE_NOTIFICATIONS)")
    );
  }

  #[test]
  fn desktop_js_menus_and_notifications_are_wired() {
    // showContextMenu resolves on the close event with the chosen id.
    assert!(DESKTOP_JS.contains("case \"contextMenuClose\":"));
    assert!(DESKTOP_JS.contains("contextMenuChoice.set(target, ev.id);"));
    assert!(DESKTOP_JS.contains("new CustomEvent(\"contextmenuclose\", {"));
    assert!(DESKTOP_JS.contains(
      "defineEventHandler(BrowserWindowPrototype, \"contextmenuclose\")"
    ));
    // Actions are their own event, not folded into click.
    assert!(DESKTOP_JS.contains("case \"notificationAction\":"));
    assert!(DESKTOP_JS.contains("new NotificationActionEvent(\"action\", {"));
    assert!(
      DESKTOP_JS
        .contains("defineEventHandler(NotificationPrototype, \"action\")")
    );
    // Responses ride the launch inbox and the launch snapshot.
    assert!(DESKTOP_JS.contains("\"notificationresponse\","));
    assert!(DESKTOP_JS.contains("case \"notificationResponse\":"));
    // ... both when buffered and from the event loop.
    assert_eq!(
      DESKTOP_JS.matches("case \"notificationResponse\":").count(),
      2
    );
    assert!(DESKTOP_JS.contains("launchNotificationResponses:"));
    for api in [
      "async schedule(options)",
      "async getScheduled()",
      "cancel(tag)",
      "async requestPermission(options = undefined)",
      "Object.defineProperty(desktop, \"notifications\"",
      "Object.defineProperty(desktop, \"menuCapabilities\"",
    ] {
      assert!(DESKTOP_JS.contains(api), "{api}");
    }
  }

  #[test]
  fn desktop_js_installs_launch_events() {
    // `Deno.desktop` is the app-level EventTarget for deep links, opened
    // files and second-instance launches.
    assert!(
      DESKTOP_JS.contains(
        "Object.defineProperty(Deno, \"desktop\", internals.core.propReadOnly(desktop))"
      )
    );
    for (kind, ty) in [
      ("openUrl", "openurl"),
      ("openFile", "openfile"),
      ("secondInstance", "secondinstance"),
    ] {
      // The event loop routes the wire kind, and the dispatcher turns it into
      // the DOM event type the d.ts documents.
      assert!(DESKTOP_JS.contains(&format!("case \"{kind}\":")), "{kind}");
      assert!(
        DESKTOP_JS.contains(&format!("new CustomEvent(\"{ty}\"")),
        "{ty}"
      );
    }
    // Adding a listener is what drains the runtime's buffer.
    assert!(DESKTOP_JS.contains("op_desktop_subscribe_launch_events(type)"));
    assert!(DESKTOP_JS.contains("internals.defineEventHandler(desktop, type)"));
    // The launch snapshot is taken once, lazily.
    assert!(DESKTOP_JS.contains("op_desktop_take_launch_targets()"));
    assert!(DESKTOP_JS.contains("launchUrls:"));
    assert!(DESKTOP_JS.contains("launchFiles:"));
  }

  #[test]
  fn desktop_js_installs_dialogs() {
    assert!(DESKTOP_JS.contains(r#"Object.defineProperty(desktop, "dialog""#));
    assert!(
      DESKTOP_JS.contains("showOpenDialog: async function showOpenDialog(")
    );
    assert!(
      DESKTOP_JS.contains("showSaveDialog: async function showSaveDialog(")
    );
    // Open, wait and cancel go through the id the open op hands out, and an
    // AbortSignal cancels.
    assert!(
      DESKTOP_JS.contains("const rid = op_desktop_file_dialog_open(request);")
    );
    assert!(DESKTOP_JS.contains("await op_desktop_file_dialog_wait(rid)"));
    assert!(DESKTOP_JS.contains("op_desktop_file_dialog_cancel(rid)"));
    assert!(DESKTOP_JS.contains("signal.addEventListener(\"abort\", onAbort"));
    // Cancelled resolves null; busy is Deno.errors.Busy.
    assert!(DESKTOP_JS.contains("case \"cancelled\": return null;"));
    assert!(DESKTOP_JS.contains("new Deno.errors.Busy("));
    // A save dialog resolves one path.
    assert!(DESKTOP_JS.contains("return paths === null ? null : paths[0];"));
    // Electron's properties, nothing else.
    assert!(DESKTOP_JS.contains("Unknown dialog property"));
  }

  #[test]
  fn desktop_js_installs_platform_features() {
    // laufey API 45: Deno.desktop.platformFeatures().
    for needle in [
      "platformFeatures: {",
      "value: function platformFeatures() {",
      "return op_desktop_platform_features();",
    ] {
      assert!(DESKTOP_JS.contains(needle), "missing: {needle}");
    }
  }

  #[test]
  fn desktop_js_installs_title_bar_preferences() {
    // laufey API 47: Deno.desktop.titleBarPreferences() and its event.
    for needle in [
      "titleBarPreferences: {",
      "value: function titleBarPreferences() {",
      "return op_desktop_title_bar_preferences();",
      "internals.defineEventHandler(desktop, \"titlebarpreferenceschanged\");",
    ] {
      assert!(DESKTOP_JS.contains(needle), "missing: {needle}");
    }
  }

  #[test]
  fn desktop_js_installs_shortcuts_login_devtools() {
    // laufey API 40: Deno.desktop.shortcuts / launchAtLogin / devtools and
    // the BrowserWindow DevTools toggle.
    for needle in [
      "const result = await op_desktop_register_shortcut(accelerator);",
      "throw shortcutError(result.status, accelerator);",
      "op_desktop_unregister_shortcut(accelerator)",
      "op_desktop_unregister_all_shortcuts();",
      "op_desktop_list_shortcuts()",
      "op_desktop_canonical_accelerator(String(accelerator))",
      "op_desktop_system_capabilities()",
      "internals.defineEventHandler(desktopShortcuts, \"shortcut\");",
      "await op_desktop_get_launch_at_login()",
      "await op_desktop_set_launch_at_login(enabled)",
      "op_desktop_devtools_enabled(0)",
      "BrowserWindowPrototype.toggleDevtools = function(options = undefined)",
      "shortcuts: {",
      "launchAtLogin: {",
      "devtools: {",
    ] {
      assert!(DESKTOP_JS.contains(needle), "missing: {needle}");
    }
    // Presses reach the shortcuts object through the event loop.
    assert!(DESKTOP_JS.contains("case \"shortcut\":"));
    assert!(
      DESKTOP_JS
        .contains("desktopShortcuts[privateShortcutPressed](ev.accelerator);")
    );
    // Every status the runtime reports has an error with that code.
    for status in [
      "\"invalid\"",
      "\"conflict\"",
      "\"already_registered\"",
      "\"not_supported\"",
      "\"denied\"",
    ] {
      assert!(
        DESKTOP_JS.contains(&format!("case {status}:")),
        "no error for {status}"
      );
    }
  }

  #[test]
  fn desktop_js_installs_rich_clipboard() {
    assert!(
      DESKTOP_JS.contains(r#"Object.defineProperty(desktop, "clipboard""#)
    );
    assert!(DESKTOP_JS.contains("class DesktopClipboard extends EventTarget"));
    for op in [
      "op_desktop_read_clipboard_html()",
      "op_desktop_write_clipboard_html(",
      "op_desktop_read_clipboard_image()",
      "op_desktop_write_clipboard_image(png)",
      "op_desktop_read_clipboard_formats()",
      "op_desktop_clipboard_capabilities()",
    ] {
      assert!(DESKTOP_JS.contains(op), "{op}");
    }
    // The OS watcher follows the listeners (macOS polls only while on).
    assert!(DESKTOP_JS.contains("op_desktop_clipboard_watch(on);"));
    assert!(DESKTOP_JS.contains("case \"clipboardChange\":"));
    assert!(
      DESKTOP_JS.contains("desktopClipboard[privateClipboardChanged]();")
    );
  }

  #[test]
  fn desktop_js_installs_file_drag_and_drop() {
    for ev in ["dragenter", "dragover", "dragleave", "drop"] {
      assert!(
        DESKTOP_JS.contains(&format!(
          "internals.defineEventHandler(BrowserWindowPrototype, \"{ev}\")"
        )),
        "{ev}"
      );
    }
    assert!(DESKTOP_JS.contains("case \"fileDrop\":"));
    assert!(DESKTOP_JS.contains("const type = FILE_DROP_EVENTS[ev.phase];"));
    assert!(DESKTOP_JS.contains(
      "BrowserWindowPrototype.startDrag = async function startDrag(item)"
    ));
    assert!(DESKTOP_JS.contains(
      "return await op_desktop_start_drag(this.windowId, [...files], icon);"
    ));
  }

  #[test]
  fn desktop_js_installs_passkeys() {
    assert!(
      DESKTOP_JS.contains(r#"Object.defineProperty(desktop, "passkeys""#)
    );
    assert!(DESKTOP_JS.contains("op_desktop_passkey_capabilities()"));
    // Strings in, the envelope out: the JSON is passed through untouched.
    assert!(DESKTOP_JS.contains("typeof optionsJson !== \"string\""));
    assert!(DESKTOP_JS.contains(
      "op_desktop_passkey_request(\n      create,\n      passkeyWindowId(options),\n      optionsJson,"
    ));
    // The window defaults to 0 (focused); a BrowserWindow gives its id.
    assert!(DESKTOP_JS.contains("return 0; // focused window"));
    assert!(DESKTOP_JS.contains("return target.windowId;"));
  }

  #[test]
  fn desktop_js_installs_auth_session_and_main_thread() {
    assert!(
      DESKTOP_JS.contains(r#"Object.defineProperty(desktop, "authSession""#)
    );
    assert!(DESKTOP_JS.contains("op_desktop_auth_session_capabilities()"));
    // Exactly one callback; an error outcome becomes an AuthSessionError
    // with its code; the window option is shared with passkeys.
    assert!(
      DESKTOP_JS
        .contains("(scheme === undefined) === (callbackUrl === undefined)")
    );
    assert!(DESKTOP_JS.contains("error.name = \"AuthSessionError\";"));
    assert!(
      DESKTOP_JS
        .contains("throw authSessionError(outcome.code, outcome.message);")
    );
    assert!(DESKTOP_JS.contains(
      "op_desktop_auth_session_start(\n        passkeyWindowId(options),"
    ));
    // cancel() ends the running session (laufey API 43), false when none.
    assert!(DESKTOP_JS.contains("return op_desktop_auth_session_cancel();"));
    // runOnMainThread takes FFI pointers, resolves with a bigint.
    assert!(
      DESKTOP_JS
        .contains(r#"Object.defineProperty(desktop, "runOnMainThread""#)
    );
    assert!(DESKTOP_JS.contains("fn instanceof Deno.UnsafeFnPointer"));
    assert!(DESKTOP_JS.contains(
      "return BigInt(await op_desktop_run_on_main_thread(pointer, context));"
    ));
    // A JavaScript callback would make the UI thread wait for the
    // JavaScript thread: refused before the op.
    assert!(DESKTOP_JS.contains("if (fn instanceof Deno.UnsafeCallback) {"));
  }

  #[test]
  fn desktop_js_installs_scheme_registration() {
    assert!(DESKTOP_JS.contains("getSchemeOwner: {"));
    assert!(DESKTOP_JS.contains("registerScheme: {"));
    assert!(DESKTOP_JS.contains("op_desktop_get_scheme_owner(String(scheme))"));
    // `force` must be exactly `true`: a truthy non-boolean doesn't take a
    // scheme over.
    assert!(DESKTOP_JS.contains("options.force === true"));
    assert!(
      DESKTOP_JS.contains("op_desktop_register_scheme(String(scheme), force)")
    );
  }

  #[test]
  fn desktop_js_installs_navigator_permissions() {
    assert!(DESKTOP_JS.contains("navigator"));
    assert!(DESKTOP_JS.contains("permissions"));
    assert!(DESKTOP_JS.contains("PermissionStatus"));
  }

  #[test]
  fn desktop_js_installs_navigator_clipboard() {
    // Assert on code, not prose: every one of "navigator", "clipboard",
    // "readText" and "writeText" also appears in the comment block above the
    // implementation, so substring checks on those words alone would still
    // pass with the whole class deleted.
    assert!(
      DESKTOP_JS.contains("class Clipboard extends EventTarget"),
      "Clipboard must be a real EventTarget subclass"
    );
    assert!(
      DESKTOP_JS.contains("async readText()"),
      "readText must be defined on the class"
    );
    assert!(
      DESKTOP_JS.contains("async writeText(data)"),
      "writeText must be defined on the class"
    );
    // Installed as a prototype getter on Navigator, as in browsers, rather
    // than an own data property on the instance.
    assert!(
      DESKTOP_JS.contains("defineProperty(NavigatorPrototype, \"clipboard\""),
      "clipboard must be installed on Navigator.prototype"
    );
    // The constructor is not reachable, and the receiver is checked.
    assert!(
      DESKTOP_JS.contains("webidl.illegalConstructor()"),
      "Clipboard must not be constructible"
    );
    assert!(
      DESKTOP_JS.contains("webidl.assertBranded(this, ClipboardPrototype)"),
      "the text methods must assert a branded receiver"
    );
  }

  #[test]
  fn desktop_js_clipboard_awaits_the_ops() {
    // The ops are async so a slow clipboard owner can't freeze the runtime
    // (see op_desktop_read_clipboard_text). That only holds if the JS side
    // actually awaits them — dropping the `await` would return a pending
    // promise as the text and silently break `readText()`.
    assert!(
      DESKTOP_JS.contains("await op_desktop_read_clipboard_text()"),
      "readText must await the op"
    );
    assert!(
      DESKTOP_JS.contains("await op_desktop_write_clipboard_text(data)"),
      "writeText must await the op"
    );
  }

  #[test]
  fn desktop_js_installs_browser_window_constructor() {
    assert!(DESKTOP_JS.contains("Deno.BrowserWindow"));
    // The original BrowserWindow is wrapped so per-window state is
    // recorded.
    assert!(DESKTOP_JS.contains("windows.set"));
    assert!(DESKTOP_JS.contains(
      "internals.defineEventHandler(BrowserWindowPrototype, \"load\")"
    ));
    assert!(DESKTOP_JS.contains("case \"pageLoad\""));
    assert!(DESKTOP_JS.contains("dispatchEvent(new Event(\"load\"))"));
  }

  #[test]
  fn desktop_js_installs_match_media() {
    assert!(DESKTOP_JS.contains("BrowserWindowPrototype.matchMedia"));
    assert!(DESKTOP_JS.contains("class MediaQueryList "));
    assert!(DESKTOP_JS.contains("evalMediaQueryList"));
    assert!(DESKTOP_JS.contains("MediaQueryList.reeval(target)"));
    assert!(DESKTOP_JS.contains("case \"windowResize\""));
    assert!(DESKTOP_JS.contains("case \"windowMove\""));
  }

  #[test]
  fn desktop_js_interposes_on_native_registry_methods() {
    assert!(DESKTOP_JS.contains(
      "BrowserWindowPrototype.bind = function(name, fn, options = undefined)"
    ));
    assert!(
      DESKTOP_JS.contains("BrowserWindowPrototype.unbind = function(name)")
    );
    assert!(DESKTOP_JS.contains("TrayPrototype.destroy = function()"));
    assert!(DESKTOP_JS.contains(
      "const privateDesktopBind = Symbol.for(\"Deno_privateDesktopBind\")"
    ));
    assert!(DESKTOP_JS.contains(
      "const privateDesktopUnbind = Symbol.for(\"Deno_privateDesktopUnbind\")"
    ));
    assert!(DESKTOP_JS.contains(
      "BrowserWindowPrototype[privateDesktopBind].call(this, name, originsSpec);"
    ));
    // The handler learns the calling document only when it asked to.
    assert!(DESKTOP_JS.contains(
      "? await fn_({ origin: ev.origin, windowId: ev.windowId }, ...args)"
    ));
    assert!(DESKTOP_JS.contains(
      "BrowserWindowPrototype[privateDesktopUnbind].call(this, name)"
    ));
    assert!(DESKTOP_JS.contains("Deno_privateDesktopTrayDestroy"));
    assert!(
      DESKTOP_JS
        .contains("TrayPrototype[privateDesktopTrayDestroy].call(this)")
    );
  }

  // --- desktop_auto_update_js ---

  #[test]
  fn auto_update_js_inlines_version_as_json_literal() {
    let js = desktop_auto_update_js(Some("1.2.3"), false, None);
    // The version must be a JSON-quoted string, not a bare identifier:
    // we feed it through serde_json::to_string. A regression that
    // dropped the quoting would produce invalid JS for any non-trivial
    // version (e.g. `1.2.3-alpha`).
    assert!(
      js.contains(r#""1.2.3""#),
      "version must be quoted; got: {js}"
    );
    assert!(js.contains("const _version ="));
    assert!(js.contains("const _rolledBack = false"));
  }

  #[test]
  fn auto_update_js_serializes_none_as_null_literal() {
    let js = desktop_auto_update_js(None, true, None);
    assert!(js.contains("const _version = null"));
    assert!(js.contains("const _rolledBack = true"));
    assert!(js.contains("const _releaseBaseUrl = null"));
  }

  #[test]
  fn auto_update_js_inlines_release_base_url() {
    // The configured `desktop.release.baseUrl` is baked in as the default
    // `url` for `Deno.autoUpdate`, so a no-arg call uses it.
    let js = desktop_auto_update_js(
      Some("1.0.0"),
      false,
      Some("https://releases.example/app"),
    );
    assert!(
      js.contains(r#"const _releaseBaseUrl = "https://releases.example/app""#),
      "release base url must be quoted; got: {js}"
    );
    assert!(js.contains("url = _releaseBaseUrl"));
  }

  #[test]
  fn auto_update_js_blocks_non_https_manifest_url() {
    // Anti-downgrade defence: the auto-update path must refuse to
    // fetch its manifest over http://, gopher://, file://, etc. A
    // change that loosened this check is a security regression.
    let js = desktop_auto_update_js(Some("1.0.0"), false, None);
    assert!(js.contains("isHttpsUrl"));
    assert!(js.contains("https:"));
  }

  // --- desktop_error_reporting_js ---

  #[test]
  fn error_reporting_js_quotes_url_and_version() {
    let js =
      desktop_error_reporting_js(Some("https://err.example/r"), Some("0.1.0"));
    assert!(js.contains(r#""https://err.example/r""#));
    assert!(js.contains(r#""0.1.0""#));
    // The URL must be referenced by the script body — otherwise the
    // emitted code would silently never POST.
    assert!(js.contains("_errorReportingUrl"));
  }

  #[test]
  fn error_reporting_js_handles_none_url() {
    let js = desktop_error_reporting_js(None, None);
    // null both — the handler short-circuits the POST but still shows
    // the alert.
    assert!(js.contains("const _errorReportingUrl = null"));
    assert!(js.contains("const _appVersion = null"));
  }

  #[test]
  fn error_reporting_js_never_blocks_the_js_thread() {
    // The error dialog must go through the async op: the blocking
    // `op_desktop_alert` parks the JS thread until the dialog is dismissed,
    // which wedged the entire runtime (timers, servers, signal handlers)
    // whenever nobody could click it — hidden window, headless child
    // (#36393). Errors must also always reach stderr, because in those runs
    // the dialog is invisible and stderr is the only surface left.
    let js = desktop_error_reporting_js(None, None);
    assert!(js.contains("op_desktop_alert_async"));
    assert!(!js.contains("op_desktop_alert(\"Application Error\""));
    assert!(js.contains("console.error"));
  }

  #[test]
  fn error_reporting_js_holds_the_process_open_for_the_dialog() {
    // The dialog is only reached by awaiting the op, so the handler has to
    // stop the runtime tearing down the moment it returns — otherwise the
    // dialog (and the exit that follows it) races process teardown and the
    // user sees nothing. `preventDefault()` hands us the default handling;
    // the exit below is then ours to perform.
    let js = desktop_error_reporting_js(None, None);
    assert!(
      js.contains("ev.preventDefault()"),
      "handler must prevent the runtime from terminating before the dialog"
    );
    assert!(
      js.contains("Deno.exit(1)"),
      "having prevented the default, the handler owns the exit"
    );
    // Both listeners must pass the event through, or `preventDefault` above
    // is unreachable for one of them. Counting `addEventListener` and
    // `handleError(` separately rather than matching an indented literal:
    // the previous form pinned the exact whitespace inside the template and
    // would have broken on a reformat that changed nothing real.
    assert_eq!(
      js.matches("addEventListener(").count(),
      2,
      "both the `error` and `unhandledrejection` listeners must be installed"
    );
    assert_eq!(
      js.matches("handleError(").count(),
      3,
      "one definition plus one call from each listener"
    );
    assert!(
      js.contains("function handleError(ev, err, message, stack)"),
      "the handler must receive the event so it can prevent the default"
    );
  }

  #[test]
  fn every_op_the_desktop_scripts_call_survives_bootstrap() {
    // The desktop scripts reach their ops through
    // `Deno[Deno.internal].core.ops`, and bootstrap (`removeImportedOps` in
    // runtime/js/99_main.js) deletes every op that is not listed in
    // NOT_IMPORTED_OPS. An op missing from that list is `undefined` by the
    // time the script runs: `op_desktop_alert_async` was, so the
    // uncaught-error handler threw inside itself and the error dialog never
    // showed.
    const MAIN_JS: &str = include_str!("../../runtime/js/99_main.js");
    let start = MAIN_JS
      .find("const NOT_IMPORTED_OPS = [")
      .expect("NOT_IMPORTED_OPS in 99_main.js");
    let end = start + MAIN_JS[start..].find("];").expect("end of the list");
    let preserved = &MAIN_JS[start..end];
    let scripts = [
      DESKTOP_JS.to_string(),
      desktop_error_reporting_js(Some("https://err.example/r"), Some("1.0.0")),
      desktop_auto_update_js(Some("1.0.0"), false, Some("https://up.example/")),
    ];
    let mut checked = 0;
    for script in &scripts {
      let mut rest = script.as_str();
      while let Some(i) = rest.find("op_desktop_") {
        let tail = &rest[i..];
        let len = tail
          .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
          .unwrap_or(tail.len());
        let op = &tail[..len];
        assert!(
          preserved.contains(&format!("\"{op}\"")),
          "{op} is called by a desktop script but removed at bootstrap: \
           add it to NOT_IMPORTED_OPS in runtime/js/99_main.js"
        );
        checked += 1;
        rest = &tail[len..];
      }
    }
    assert!(checked > 10, "found only {checked} op references");
  }

  #[test]
  fn error_reporting_js_listens_for_unhandledrejection() {
    // Both `error` and `unhandledrejection` events must be hooked
    // — missing either would let half of all user-code failures fall
    // out the bottom of the runtime without notification.
    let js = desktop_error_reporting_js(None, None);
    assert!(js.contains("\"error\""));
    assert!(js.contains("\"unhandledrejection\""));
  }

  // --- helpers ---

  /// Return a window of DESKTOP_JS around the first occurrence of `needle`,
  /// covering ~10 lines on each side. Useful for asserting "the region
  /// near this token contains a try/catch" without coupling the test
  /// to a precise line range.
  fn locate_around(hay: &str, needle: &str) -> String {
    // The first occurrence of DENO_DESKTOP_MUX_WS in DESKTOP_JS is
    // inside a comment block; the *code* occurrence is the second. We
    // want the window around the code path, so search after the first.
    let first = hay.find(needle).unwrap_or_else(|| {
      panic!("needle {needle:?} not found in DESKTOP_JS");
    });
    let idx = hay[first + needle.len()..]
      .find(needle)
      .map(|i| first + needle.len() + i)
      .unwrap_or(first);
    let start = idx.saturating_sub(500);
    let end = (idx + needle.len() + 500).min(hay.len());
    hay[start..end].to_string()
  }
}
