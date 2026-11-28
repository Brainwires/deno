// Copyright 2018-2026 the Deno authors. MIT license.

// The bootstrap of a page context: installs the web platform on the global
// object, turns it into a Window, hands the host its hooks, and then removes
// every way from page JavaScript to the ops (`Deno`, `__bootstrap`).
//
// Everything is installed eagerly, as plain data properties. Nothing is
// loaded lazily once a page script has run (the modules the extensions
// would load on first use are loaded here), so no extension script is ever
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
import "ext:deno_web_page/04_hardening.js";
const {
  FunctionPrototypeCall,
  JSONStringify,
  ObjectDefineProperties,
  ObjectDefineProperty,
  ObjectFreeze,
  ObjectKeys,
  ObjectSetPrototypeOf,
  Proxy,
  ReflectConstruct,
  ReflectHas,
  String,
  Symbol,
  TypeError,
} = primordials;

const info = op_web_page_bootstrap_info();

// The ops the extension's middleware took the implementation of
// (`op_policy.rs`): each one throws `op is disabled` whatever it is called
// with. Checked here, for every page, because nothing can call an op once
// this module has run.
for (let i = 0; i < info.disabledOps.length; i++) {
  const name = info.disabledOps[i];
  let disabled = false;
  try {
    core.ops[name]();
  } catch (error) {
    disabled = error?.message === "op is disabled";
  }
  if (!disabled) {
    throw new TypeError(`deno_web_page: ${name} is not disabled`);
  }
}

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
// The same-origin policy of the page's requests is `deno_fetch`'s
// web-origin mode, which it enters when the op state has the page's origin
// (`lib.rs` puts it there). Checked here, for every page: a fetch that
// answered as Deno's does would hand a page every response it can reach.
if (fetch.webOriginMode() !== true) {
  throw new TypeError("deno_web_page: fetch is not in web-origin mode");
}
const eventSource = core.loadExtScript("ext:deno_fetch/27_eventsource.js");
const webStorage = core.loadExtScript("ext:deno_webstorage/01_webstorage.js");
const crypto = core.loadExtScript("ext:deno_crypto/00_crypto.js");
const webSocket = core.createLazyLoader(
  "ext:deno_websocket/01_websocket.js",
)();
const webSocketStream = core.createLazyLoader(
  "ext:deno_websocket/02_websocketstream.js",
)();
const image = core.createLazyLoader("ext:deno_image/01_image.js")();
const canvas = core.createLazyLoader("ext:deno_canvas/01_canvas.js")();
// WebRTC Identity runs an identity provider's script in a worker, and a
// page runtime has no workers: with this set, `06_identity.js` answers
// `NotSupportedError` instead of loading `deno_runtime`'s worker module.
internals.webrtcIdentityUnavailable = true;
const webrtc = core.loadExtScript("ext:deno_webrtc/01_webrtc.js");
// `01_webrtc.js` loads these two on first use (a `transform` set on a sender
// or a receiver, a transferred data channel). Loaded here, they are in
// deno_core's cache: no extension script is evaluated once a page script
// has run.
const encodedTransform = core.loadExtScript(
  "ext:deno_webrtc/03_encoded_transform.js",
);
const webrtcTransfer = core.loadExtScript("ext:deno_webrtc/05_transfer.js");

// What `structuredClone()` and `postMessage()` need to know about the WebRTC
// objects, as `deno_runtime` registers it (98_global_scope_shared.js).
core.registerCloneableResource(
  "RTCEncodedVideoFrame",
  (data) => encodedTransform.deserializeFrame(data),
);
core.registerCloneableResource(
  "RTCEncodedAudioFrame",
  (data) => encodedTransform.deserializeFrame(data),
);
core.registerCloneableResource(
  "RTCCertificate",
  (data) => webrtc.deserializeCertificate(data),
);
core.registerTransferableResource(
  "RTCDataChannel",
  (channel) => webrtcTransfer.transferSteps(channel),
  (rid) => webrtcTransfer.transferReceivingSteps(rid),
);

// --- WebSocket: the constructors a browser has ------------------------------------
//
// `deno_websocket`'s constructors take more than a browser's: request
// headers, a `Deno.HttpClient`, and an undici dispatcher whose options (found
// under a `Symbol.for()` key any script can make) turn certificate
// verification off or add trusted roots. A page gets the standard
// arguments: these wrappers pass nothing else on. They are proxies, so the
// interface object keeps its name, its constants and its prototype, and
// `new.target` reaches the real constructor for a subclass.

function pageConstructor(Interface, standardArguments) {
  const wrapper = new Proxy(Interface, {
    __proto__: null,
    construct(target, args, newTarget) {
      return ReflectConstruct(target, standardArguments(args), newTarget);
    },
  });
  ObjectDefineProperty(Interface.prototype, "constructor", {
    __proto__: null,
    value: wrapper,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  return wrapper;
}

// `new WebSocket(url, protocols)`: a string or a sequence of strings. An
// object that is not iterable is a TypeError here, never a dictionary.
const WebSocket = pageConstructor(webSocket.WebSocket, (args) => {
  if (args.length < 2 || args[1] === undefined) {
    return args.length < 1 ? [] : [args[0]];
  }
  const prefix = "Failed to construct 'WebSocket'";
  let protocols = args[1];
  if (
    (typeof protocols === "object" && protocols !== null) ||
    typeof protocols === "function"
  ) {
    protocols = webidl.converters["sequence<DOMString>"](
      protocols,
      prefix,
      "Argument 2",
    );
  } else {
    protocols = webidl.converters.DOMString(protocols, prefix, "Argument 2");
  }
  return [args[0], protocols];
});

// `new WebSocketStream(url, { protocols, signal })`.
const WebSocketStream = pageConstructor(
  webSocketStream.WebSocketStream,
  (args) => {
    if (args.length < 2) {
      return args;
    }
    const options = args[1];
    if (
      options === null ||
      (typeof options !== "object" && typeof options !== "function")
    ) {
      // `undefined` and `null` are the empty dictionary; anything else is
      // the constructor's TypeError.
      return [args[0], options];
    }
    return [args[0], {
      __proto__: null,
      protocols: options.protocols,
      signal: options.signal,
    }];
  },
);

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
    ImageBitmap: image.ImageBitmap,
    ImageBitmapRenderingContext: canvas.ImageBitmapRenderingContext,
    ImageData: imageData.ImageData,
    MediaStream: webrtc.MediaStream,
    MediaStreamTrack: webrtc.MediaStreamTrack,
    MediaStreamTrackEvent: webrtc.MediaStreamTrackEvent,
    MessageChannel: messagePort.MessageChannel,
    MessageEvent: event.MessageEvent,
    MessagePort: messagePort.MessagePort,
    OffscreenCanvas: canvas.OffscreenCanvas,
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
    RTCCertificate: webrtc.RTCCertificate,
    RTCDataChannel: webrtc.RTCDataChannel,
    RTCDataChannelEvent: webrtc.RTCDataChannelEvent,
    RTCDtlsTransport: webrtc.RTCDtlsTransport,
    RTCDTMFSender: webrtc.RTCDTMFSender,
    RTCDTMFToneChangeEvent: webrtc.RTCDTMFToneChangeEvent,
    RTCEncodedAudioFrame: encodedTransform.RTCEncodedAudioFrame,
    RTCEncodedVideoFrame: encodedTransform.RTCEncodedVideoFrame,
    RTCError: webrtc.RTCError,
    RTCErrorEvent: webrtc.RTCErrorEvent,
    RTCIceCandidate: webrtc.RTCIceCandidate,
    RTCIceTransport: webrtc.RTCIceTransport,
    RTCPeerConnection: webrtc.RTCPeerConnection,
    RTCPeerConnectionIceErrorEvent: webrtc.RTCPeerConnectionIceErrorEvent,
    RTCPeerConnectionIceEvent: webrtc.RTCPeerConnectionIceEvent,
    RTCRtpReceiver: webrtc.RTCRtpReceiver,
    RTCRtpSender: webrtc.RTCRtpSender,
    RTCRtpTransceiver: webrtc.RTCRtpTransceiver,
    RTCSctpTransport: webrtc.RTCSctpTransport,
    RTCSessionDescription: webrtc.RTCSessionDescription,
    RTCStatsReport: webrtc.RTCStatsReport,
    RTCTrackEvent: webrtc.RTCTrackEvent,
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
    WebSocket,
    WebSocketError: webSocketStream.WebSocketError,
    WebSocketStream,
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
  createImageBitmap: core.propWritable(image.createImageBitmap),
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
  event.dispatchTrusted(globalThis, rejection);
  if (!rejection.defaultPrevented) {
    op_web_page_console("Uncaught (in promise) " + describe(reason), 3);
  }
  return true;
});
core.setHandledPromiseRejectionHandler((promise, reason) => {
  event.dispatchTrusted(
    globalThis,
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
    event.dispatchTrusted(globalThis, ev);
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
