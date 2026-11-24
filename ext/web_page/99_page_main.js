// Copyright 2018-2026 the Deno authors. MIT license.

// The bootstrap of a page context: installs the web platform on the global
// object, turns it into a Window, hands the host its hooks, and then removes
// every way from page JavaScript to the ops (`Deno`, `__bootstrap`).
//
// Everything is installed eagerly, as plain data properties. Nothing is
// loaded lazily once a page script has run, so no extension script is ever
// evaluated in a realm a page has already tampered with, and a page sees no
// accessor where a browser has a value.

import { core, internals, primordials } from "ext:core/mod.js";
import {
  op_web_page_bootstrap_info,
  op_web_page_console,
  op_web_page_register_hooks,
} from "ext:core/ops";
import * as dom from "ext:deno_web_page/00_dom.js";
import * as uiEvents from "ext:deno_web_page/01_events.js";
import * as html from "ext:deno_web_page/02_html.js";
import * as windowObjects from "ext:deno_web_page/03_window.js";
const {
  FunctionPrototypeCall,
  JSONStringify,
  ObjectDefineProperties,
  ObjectFreeze,
  ObjectKeys,
  ObjectSetPrototypeOf,
  ReflectHas,
  String,
  Symbol,
  TypeError,
} = primordials;

const info = op_web_page_bootstrap_info();

// `26_fetch.js` reads the tracing hooks of `deno_telemetry`, and loads that
// extension when they are missing. A page context has no telemetry: these
// are the hooks with tracing off, which is all fetch asks of them.
internals.__telemetry = {
  __proto__: null,
  TRACING_ENABLED: false,
  PROPAGATORS: ObjectFreeze([]),
  DID_NOT_ENTER: Symbol("DID_NOT_ENTER"),
  builtinTracer() {
    return null;
  },
  ContextManager: {
    __proto__: null,
    active() {
      return null;
    },
  },
  enterSpan() {
    return null;
  },
  exitSpan() {},
};
internals.__telemetryUtil = {
  __proto__: null,
  updateSpanFromClientResponse() {},
  updateSpanFromError() {},
  updateSpanFromRequest() {},
};
// `00_crypto.js` brands CryptoKeys with a symbol `ext/node` normally
// provides.
internals.kKeyObject ??= Symbol("kKeyObject");

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException, QuotaExceededError } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const event = core.loadExtScript("ext:deno_web/02_event.js");
const consoleModule = core.loadExtScript("ext:deno_web/01_console.js");
const timers = core.loadExtScript("ext:deno_web/02_timers.js");
const url = core.loadExtScript("ext:deno_web/00_url.js");
const urlPattern = core.loadExtScript("ext:deno_web/01_urlpattern.js");
const abortSignal = core.loadExtScript("ext:deno_web/03_abort_signal.js");
const globalInterfaces = core.loadExtScript(
  "ext:deno_web/04_global_interfaces.js",
);
const base64 = core.loadExtScript("ext:deno_web/05_base64.js");
const streams = core.loadExtScript("ext:deno_web/06_streams.js");
const encoding = core.loadExtScript("ext:deno_web/08_text_encoding.js");
const file = core.loadExtScript("ext:deno_web/09_file.js");
const fileReader = core.loadExtScript("ext:deno_web/10_filereader.js");
const messagePort = core.loadExtScript("ext:deno_web/13_message_port.js");
const compression = core.loadExtScript("ext:deno_web/14_compression.js");
const performance = core.loadExtScript("ext:deno_web/15_performance.js");
const imageData = core.loadExtScript("ext:deno_web/16_image_data.js");
const geometry = core.loadExtScript("ext:deno_web/17_geometry.js");
const broadcastChannel = core.loadExtScript(
  "ext:deno_web/01_broadcast_channel.js",
);
const headers = core.loadExtScript("ext:deno_fetch/20_headers.js");
const formData = core.loadExtScript("ext:deno_fetch/21_formdata.js");
const request = core.loadExtScript("ext:deno_fetch/23_request.js");
const response = core.loadExtScript("ext:deno_fetch/23_response.js");
const fetch = core.loadExtScript("ext:deno_fetch/26_fetch.js");
const eventSource = core.loadExtScript("ext:deno_fetch/27_eventsource.js");
const webStorage = core.loadExtScript("ext:deno_webstorage/01_webstorage.js");
const crypto = core.loadExtScript("ext:deno_crypto/00_crypto.js");

// The errors ops throw by class name. `dom_ops.rs` names the first eight;
// the rest are the ones the web extensions use.
const DOM_EXCEPTIONS = [
  "HierarchyRequestError",
  "NotFoundError",
  "InvalidCharacterError",
  "NamespaceError",
  "SyntaxError",
  "NotSupportedError",
  "InvalidNodeTypeError",
  "InvalidStateError",
  "OperationError",
  "NetworkError",
  "AbortError",
  "DataError",
  "IndexSizeError",
  "TypeMismatchError",
  "InvalidAccessError",
  "InvalidModificationError",
];
for (let i = 0; i < DOM_EXCEPTIONS.length; i++) {
  const name = DOM_EXCEPTIONS[i];
  core.registerErrorBuilder(
    "DOMException" + name,
    (message) => new DOMException(message, name),
  );
}
core.registerErrorBuilder(
  "DOMExceptionQuotaExceededError",
  (message) => new QuotaExceededError(message),
);
// The I/O error classes `deno_runtime` maps to `Deno.errors.*`. A page has
// no such classes: a failed request is a TypeError, as in a browser.
const IO_ERRORS = [
  "NotFound",
  "ConnectionRefused",
  "ConnectionReset",
  "ConnectionAborted",
  "NotConnected",
  "AddrInUse",
  "AddrNotAvailable",
  "BrokenPipe",
  "PermissionDenied",
  "AlreadyExists",
  "InvalidData",
  "TimedOut",
  "WouldBlock",
  "WriteZero",
  "UnexpectedEof",
  "Http",
  "Busy",
  "NotSupported",
  "FilesystemLoop",
  "IsADirectory",
  "NetworkUnreachable",
  "NotADirectory",
];
for (let i = 0; i < IO_ERRORS.length; i++) {
  core.registerErrorBuilder(IO_ERRORS[i], (message) => new TypeError(message));
}

// --- the console: to the host, never to the process's stdout ----------------

// No colors: the sink is a list of messages, not a terminal.
consoleModule.setNoColorFns(() => true, () => true);
const console = new consoleModule.Console((message, level) =>
  op_web_page_console(message, level)
);

// --- the global object ---------------------------------------------------------

const windowParts = windowObjects.createWindowObjects(info);
dom.setWindow(globalThis, info.url);
const document = dom.documentNode(dom.DOC.DOCUMENT);
const pageUrl = new url.URL(info.url);

function interfaceDescriptors(interfaces) {
  const descriptors = { __proto__: null };
  const names = ObjectKeys(interfaces);
  for (let i = 0; i < names.length; i++) {
    descriptors[names[i]] = core.propNonEnumerable(interfaces[names[i]]);
  }
  return descriptors;
}

// `self` and its aliases: getters that answer with the global object and,
// like any Web IDL attribute, refuse another receiver.
function globalGetter() {
  return core.propGetterOnly(function () {
    if (this !== undefined && this !== null && this !== globalThis) {
      throw new TypeError("Illegal invocation");
    }
    return globalThis;
  });
}

const storageDescriptor = (getStorage) => ({
  __proto__: null,
  configurable: true,
  enumerable: true,
  get() {
    return getStorage();
  },
  set() {},
});

ObjectDefineProperties(globalThis, {
  __proto__: null,

  // Interfaces of this extension.
  ...interfaceDescriptors({
    __proto__: null,
    Node: dom.Node,
    Element: dom.Element,
    CharacterData: dom.CharacterData,
    Text: dom.Text,
    Comment: dom.Comment,
    DocumentType: dom.DocumentType,
    DocumentFragment: dom.DocumentFragment,
    Document: dom.Document,
    HTMLDocument: dom.Document,
    NodeList: dom.NodeList,
    HTMLCollection: dom.HTMLCollection,
    NamedNodeMap: dom.NamedNodeMap,
    Attr: dom.Attr,
    DOMTokenList: dom.DOMTokenList,
    DOMStringMap: dom.DOMStringMap,
    UIEvent: uiEvents.UIEvent,
    MouseEvent: uiEvents.MouseEvent,
    PointerEvent: uiEvents.PointerEvent,
    KeyboardEvent: uiEvents.KeyboardEvent,
    InputEvent: uiEvents.InputEvent,
    FocusEvent: uiEvents.FocusEvent,
    SubmitEvent: uiEvents.SubmitEvent,
    Location: windowObjects.Location,
    History: windowObjects.History,
    Navigator: windowObjects.Navigator,
  }),
  ...interfaceDescriptors(html.htmlInterfaces),

  // Interfaces of the web extensions.
  ...interfaceDescriptors({
    __proto__: null,
    AbortController: abortSignal.AbortController,
    AbortSignal: abortSignal.AbortSignal,
    Blob: file.Blob,
    BroadcastChannel: broadcastChannel.BroadcastChannel,
    ByteLengthQueuingStrategy: streams.ByteLengthQueuingStrategy,
    CloseEvent: event.CloseEvent,
    CompressionStream: compression.CompressionStream,
    CountQueuingStrategy: streams.CountQueuingStrategy,
    Crypto: crypto.Crypto,
    CryptoKey: crypto.CryptoKey,
    CustomEvent: event.CustomEvent,
    DecompressionStream: compression.DecompressionStream,
    DOMException,
    DOMMatrix: geometry.DOMMatrix,
    DOMMatrixReadOnly: geometry.DOMMatrixReadOnly,
    DOMPoint: geometry.DOMPoint,
    DOMPointReadOnly: geometry.DOMPointReadOnly,
    DOMQuad: geometry.DOMQuad,
    DOMRect: geometry.DOMRect,
    DOMRectReadOnly: geometry.DOMRectReadOnly,
    ErrorEvent: event.ErrorEvent,
    Event: event.Event,
    EventSource: eventSource.EventSource,
    EventTarget: event.EventTarget,
    File: file.File,
    FileReader: fileReader.FileReader,
    FormData: formData.FormData,
    Headers: headers.Headers,
    ImageData: imageData.ImageData,
    MessageChannel: messagePort.MessageChannel,
    MessageEvent: event.MessageEvent,
    MessagePort: messagePort.MessagePort,
    Performance: performance.Performance,
    PerformanceEntry: performance.PerformanceEntry,
    PerformanceMark: performance.PerformanceMark,
    PerformanceMeasure: performance.PerformanceMeasure,
    PerformanceObserver: performance.PerformanceObserver,
    PerformanceObserverEntryList: performance.PerformanceObserverEntryList,
    ProgressEvent: event.ProgressEvent,
    PromiseRejectionEvent: event.PromiseRejectionEvent,
    QuotaExceededError,
    ReadableByteStreamController: streams.ReadableByteStreamController,
    ReadableStream: streams.ReadableStream,
    ReadableStreamBYOBReader: streams.ReadableStreamBYOBReader,
    ReadableStreamBYOBRequest: streams.ReadableStreamBYOBRequest,
    ReadableStreamDefaultController: streams.ReadableStreamDefaultController,
    ReadableStreamDefaultReader: streams.ReadableStreamDefaultReader,
    Request: request.Request,
    Response: response.Response,
    Storage: webStorage.Storage,
    SubtleCrypto: crypto.SubtleCrypto,
    TextDecoder: encoding.TextDecoder,
    TextDecoderStream: encoding.TextDecoderStream,
    TextEncoder: encoding.TextEncoder,
    TextEncoderStream: encoding.TextEncoderStream,
    TransformStream: streams.TransformStream,
    TransformStreamDefaultController: streams.TransformStreamDefaultController,
    URL: url.URL,
    URLPattern: urlPattern.URLPattern,
    URLSearchParams: url.URLSearchParams,
    Window: globalInterfaces.Window,
    WritableStream: streams.WritableStream,
    WritableStreamDefaultController: streams.WritableStreamDefaultController,
    WritableStreamDefaultWriter: streams.WritableStreamDefaultWriter,
  }),
  console: core.propNonEnumerable(console),

  // Window.
  window: globalGetter(),
  self: globalGetter(),
  frames: globalGetter(),
  parent: globalGetter(),
  top: globalGetter(),
  document: core.propGetterOnly(() => document),
  location: {
    __proto__: null,
    configurable: false,
    enumerable: true,
    get() {
      return windowParts.location;
    },
    set(value) {
      windowParts.location.href = value;
    },
  },
  history: core.propGetterOnly(() => windowParts.history),
  navigator: core.propGetterOnly(() => windowParts.navigator),
  name: core.propWritable(""),
  closed: core.propGetterOnly(() => false),
  length: core.propGetterOnly(() => 0),
  frameElement: core.propGetterOnly(() => null),
  opener: core.propWritable(null),
  isSecureContext: core.propGetterOnly(() => pageUrl.protocol === "https:"),
  origin: core.propGetterOnly(() => pageUrl.origin),
  alert: core.propWritable(windowObjects.alert),
  confirm: core.propWritable(windowObjects.confirm),
  prompt: core.propWritable(windowObjects.prompt),
  scroll: core.propWritable(windowObjects.scrollTo),
  scrollTo: core.propWritable(windowObjects.scrollTo),
  scrollBy: core.propWritable(windowObjects.scrollTo),
  requestAnimationFrame: core.propWritable(windowObjects.requestAnimationFrame),
  cancelAnimationFrame: core.propWritable(windowObjects.cancelAnimationFrame),

  // WindowOrWorkerGlobalScope.
  atob: core.propWritable(base64.atob),
  btoa: core.propWritable(base64.btoa),
  clearInterval: core.propWritable(timers.clearInterval),
  clearTimeout: core.propWritable(timers.clearTimeout),
  crypto: core.propGetterOnly(() => crypto.crypto),
  fetch: core.propWritable(fetch.fetch),
  performance: core.propWritable(performance.performance),
  reportError: core.propWritable(event.reportError),
  setInterval: core.propWritable(timers.setInterval),
  setTimeout: core.propWritable(timers.setTimeout),
  structuredClone: core.propWritable(messagePort.structuredClone),
  localStorage: storageDescriptor(webStorage.localStorage),
  sessionStorage: storageDescriptor(webStorage.sessionStorage),
  [webidl.brand]: core.propNonEnumerable(webidl.brand),
});

ObjectSetPrototypeOf(globalThis, globalInterfaces.Window.prototype);
event.setEventTargetData(globalThis);
event.saveGlobalThisReference(globalThis);
performance.setTimeOrigin();

// The event handlers of Window.
const WINDOW_EVENT_HANDLERS = [
  "beforeunload",
  "blur",
  "change",
  "click",
  "focus",
  "hashchange",
  "input",
  "keydown",
  "keyup",
  "load",
  "message",
  "mousedown",
  "mousemove",
  "mouseup",
  "offline",
  "online",
  "pagehide",
  "pageshow",
  "popstate",
  "rejectionhandled",
  "resize",
  "scroll",
  "storage",
  "submit",
  "unhandledrejection",
  "unload",
];
for (let i = 0; i < WINDOW_EVENT_HANDLERS.length; i++) {
  event.defineEventHandler(globalThis, WINDOW_EVENT_HANDLERS[i]);
}
event.defineEventHandler(globalThis, "error", undefined, true);

// --- errors nothing caught ---------------------------------------------------

// `deno_web` reports an uncaught exception by dispatching `error` at the
// global object and, when no listener cancels it, ending execution. For a
// page that is a line in the console, not the end of the page: this
// listener is added before any page script can add one, so it runs first,
// and cancels.
function describe(value) {
  try {
    if (typeof value === "object" && value !== null) {
      const stack = value.stack;
      if (typeof stack === "string" && stack !== "") {
        return stack;
      }
    }
    return String(value);
  } catch {
    return "[an error that cannot be described]";
  }
}

const addEventListener = event.EventTarget.prototype.addEventListener;
FunctionPrototypeCall(addEventListener, globalThis, "error", (ev) => {
  ev.preventDefault();
  op_web_page_console("Uncaught " + describe(ev.error ?? ev.message), 3);
});

core.setReportExceptionCallback(event.reportException);

// An unhandled rejection fires `unhandledrejection` and, unless a listener
// cancels it, is written to the console. It never ends the page.
core.setUnhandledPromiseRejectionHandler((promise, reason) => {
  const rejection = new event.PromiseRejectionEvent("unhandledrejection", {
    cancelable: true,
    promise,
    reason,
  });
  globalThis.dispatchEvent(rejection);
  if (!rejection.defaultPrevented) {
    op_web_page_console("Uncaught (in promise) " + describe(reason), 3);
  }
  return true;
});
core.setHandledPromiseRejectionHandler((promise, reason) => {
  globalThis.dispatchEvent(
    new event.PromiseRejectionEvent("rejectionhandled", { promise, reason }),
  );
});

// --- the host's hooks ------------------------------------------------------------

/** The JSON text of a script's completion value, for `evaluate_json`. */
function serialize(value) {
  if (value === undefined) {
    return "null";
  }
  try {
    if (dom.isNode(value)) {
      return JSONStringify(
        dom.isElement(value) ? value.outerHTML : dom.textOf(value),
      );
    }
    const text = JSONStringify(value);
    if (typeof text === "string") {
      return text;
    }
  } catch {
    // Cyclic, a BigInt, a throwing `toJSON`: fall through to the string form.
  }
  try {
    return JSONStringify(String(value));
  } catch {
    return '"[a value that cannot be serialized]"';
  }
}

const windowDispatch = event.EventTarget.prototype.dispatchEvent;

function fireAtDocument(type, bubbles) {
  const ev = new event.Event(type, { bubbles });
  event.setIsTrusted(ev, true);
  dom.dispatchNodeEvent(document, ev);
}

/** Stage 0: the document was parsed. Stage 1: the page was loaded. */
function lifecycle(stage) {
  if (stage === 0) {
    dom.setReadyState("interactive");
    fireAtDocument("readystatechange", false);
    fireAtDocument("DOMContentLoaded", true);
  } else {
    dom.setReadyState("complete");
    fireAtDocument("readystatechange", false);
    const ev = new event.Event("load");
    event.setIsTrusted(ev, true);
    FunctionPrototypeCall(windowDispatch, globalThis, ev);
  }
  return true;
}

op_web_page_register_hooks({
  __proto__: null,
  click: html.hostClick,
  setValue: html.hostSetValue,
  formState: html.hostFormState,
  lifecycle,
  serialize,
});

// --- no way back to the ops ---------------------------------------------------------

// Page JavaScript runs after this module. `Deno.core.ops` and `__bootstrap`
// are the only global paths to the ops; with both gone, what a page can call
// is what this file installed. (`import("ext:...")` is refused by deno_core
// once the extensions have loaded.)
delete globalThis.Deno;
delete globalThis.__bootstrap;
if (ReflectHas(globalThis, "Deno") || ReflectHas(globalThis, "__bootstrap")) {
  throw new TypeError(
    "deno_web_page: the internal globals could not be removed",
  );
}
