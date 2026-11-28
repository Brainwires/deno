# deno_web_page

A runtime for **hostile web-page JavaScript**: the page context of
[Thalora](https://github.com/Brainwires/thalora-web-browser), the headless
browser, and later of a confined page process for denext apps.

A `PageRuntime` is a plain `deno_core::JsRuntime` made of the web extensions
(fetch, streams, crypto, storage, `WebSocket`, WebRTC, `OffscreenCanvas`) and
nothing else, plus this crate's own extension, which binds Thalora's
engine-neutral DOM crate (`thalora-dom`: an html5ever tree with DOM-standard
mutation and `selectors` matching, no JS-engine types) to V8.

It is **not a Deno API**. There is no `deno_runtime` worker, no `Deno`
namespace, no Node. Nothing in `deno` or `denort` depends on this crate, so
their builds are unchanged; Thalora links it by git revision. The design note is
`docs/A3_V8_DESIGN.md` in the Thalora repository (this is its delivery step 2).

## Use

```rust
use std::time::Duration;
use deno_web_page::{PageOptions, PageRuntime};

// Inside a current-thread tokio runtime, on the thread that owns the page.
let mut options = PageOptions::new(url, html);
options.user_agent = "…".into();
options.net_allow = vec!["example.com".into()]; // or deno_web_page::NET_ALLOW_ANY
// options.deny_addresses: loopback, private, link-local, metadata by default
// options.webrtc_force_relay: true by default (no local address leaves)
let mut page = PageRuntime::new(options)?; // parses the markup, boots the context

page.execute_script("https://example.com/app.js", source)?; // classic script
page.fire_dom_content_loaded()?;
page.fire_load()?;
page.run_until_idle(Duration::from_secs(2)).await?; // timers, fetches, promises

let title = page.evaluate_json("document.title".into())?; // serde_json::Value
let user = page
  .evaluate_json_async(
    "await fetch('/api/me').then((r) => r.json())".into(),
    Duration::from_secs(5),
  )
  .await?;
let button = page.with_document(|doc| doc.get_element_by_id("go")).unwrap();
let outcome = page.click(button)?; // .navigation: a link or form for the host
page.set_value(input, "typed text")?; // value + input and change events
```

| Call                                                | What it does                                                                                                                                                                                    |
| --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PageRuntime::new(options)`                         | Parses `html` into a `thalora_dom::Document`, boots the context. No page script has run.                                                                                                        |
| `execute_script(name, source)`                      | Runs a classic script, then its microtasks. An uncaught exception is `PageError::Js` (message and stack).                                                                                       |
| `evaluate_json(source)`                             | The completion value as JSON: `undefined` is `null`; what JSON cannot express is its string form; a node is its markup. Promises are not awaited.                                               |
| `evaluate_json_async(source, budget)`               | The same, awaiting a promise while pumping the event loop, with `await` allowed at the top level (below). `PageError::Timeout` after `budget`, `PageError::Stalled` when nothing can settle it. |
| `run_until_idle(budget)`                            | Pumps the event loop until nothing is pending (`Idle::Quiescent`) or the budget is spent (`Idle::BudgetExhausted`). Errors in tasks do not stop it.                                             |
| `with_document(f)`                                  | The live document, read from Rust.                                                                                                                                                              |
| `click(node)`                                       | Pointer and mouse events, focus, a trusted `click`, then the default action. A checkbox or radio button toggles; a submit button or a link is reported in `ClickOutcome` (never followed).      |
| `set_value(node, value)`                            | Sets an `<input>`, `<textarea>` or `<select>` as a user would, fires `input` and `change`.                                                                                                      |
| `form_state(node)`                                  | The live value and checkedness of a form control (they are not content attributes).                                                                                                             |
| `fire_dom_content_loaded()`, `fire_load()`          | `readyState` and the lifecycle events, when the host says so.                                                                                                                                   |
| `terminate_handle()`                                | A `v8::IsolateHandle` for a watchdog thread; the interrupted call fails with `PageError::Terminated` and the page stays usable.                                                                 |
| `take_console_messages()`, `take_uncaught_errors()` | What the page logged, and the errors nothing caught.                                                                                                                                            |
| `take_navigation_requests()`                        | Navigations scripts asked for outside a click (`location.href = …`, `form.submit()`).                                                                                                           |
| `op_names()`                                        | The op table, for the allowlist test.                                                                                                                                                           |

A `PageRuntime` never navigates and never loads a script by itself: fetching
scripts, CSP, SRI and navigation are the host's.

### `evaluate_json_async`

A script without the word `await` runs as a classic script, exactly as
`evaluate_json` runs it, and a promise it completes with is awaited. A script
with the word runs as the body of an async function:

- as `return (script)` when it is one expression (a trailing `;` is dropped):
  `await fetch(url).then((r) => r.text())`;
- otherwise as the function's statements: the value is what `return` gives
  (`const r = await fetch(url); return r.status;`), `undefined` without one, and
  `var` and function declarations are local to the call.

A script that has the word but is a valid classic script and not a single
expression runs as the classic script: `await` in a string or a comment, but
also `await (x)` at the start of a statement, which a classic script reads as a
call of a function named `await` (a `ReferenceError`). Write `return`, or drop
the parentheses.

The promise's rejection and the script's exception are `PageError::Js`. An
uncaught error in another task while waiting does not end the wait.

## Security model

Page JavaScript is hostile. What contains it:

|   | Boundary                | Status             | How                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| - | ----------------------- | ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| a | No privileged ops       | by construction    | Only `deno_webidl`, `deno_web`, `deno_fetch`, `deno_webstorage`, `deno_crypto`, `deno_websocket`, `deno_webrtc`, `deno_image`, `deno_canvas_offscreen` and this crate are composed. No file system, environment, FFI, subprocess, listening socket, KV, GPU or OS op is registered, and the registered ops a page has no use for are disabled (below). `tests.rs` holds the op table against `op_allowlist.rs`.                                                                                                                                                       |
| b | Network                 | enforced           | A real `PermissionsContainer` built from parsed descriptors: net for `net_allow` only, everything else denied, no prompt. On top of it, one deny list (`deny_addresses`; default `loopback`, `private`, `link-local`, `metadata`) for every way out: the page's only HTTP client checks it on the resolved address of every connection and redirect, `WebSocket` connects through a connector that does the same, and ICE drops remote candidates in it, refuses STUN and TURN servers that resolve into it and sends no datagram to it. `file:` fetches are refused. |
| c | Storage                 | origin-scoped      | `localStorage` is a directory per origin under the host's `storage_dir` (`std::fs`, never a virtual file system); an opaque origin has none. `sessionStorage` is in memory, per page.                                                                                                                                                                                                                                                                                                                                                                                 |
| d | Isolation between pages | per-page isolate   | One isolate per `PageRuntime`. `BroadcastChannel` and blob URLs are per page too.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| e | Memory and CPU per page | built              | `heap_limit_bytes` sets V8's heap limit; the near-heap-limit callback ends the page (`PageError::OutOfMemory`), not the process. `terminate_handle()` lets a watchdog end a long task.                                                                                                                                                                                                                                                                                                                                                                                |
| f | V8 sandbox              | **off**            | The fork's V8 is built without `v8_enable_sandbox`. A V8 memory-corruption bug in page JavaScript is a compromise of the process, so the process is the boundary: standalone Thalora runs confined (`deno_sandbox`), and inside a denext app pages run in a separate confined page process. In-process page isolates next to an app's backend are not supported.                                                                                                                                                                                                      |
| g | Other origins           | same-origin policy | The page's requests follow the Fetch Standard for the origin of `PageOptions::url`: CORS and its preflight, request modes, forbidden request headers, `Origin` and `Referer`, filtered responses (below). No cookies yet.                                                                                                                                                                                                                                                                                                                                             |

How page JavaScript is kept from the ops (a):

- Every op is imported by the extension's own modules. The bootstrap
  (`99_page_main.js`) deletes `globalThis.Deno` and `globalThis.__bootstrap`
  before it returns, and fails the runtime if they are still there.
- `import("ext:…")` from a page is refused by deno_core once the extensions have
  loaded, and there is no module loader for anything else.
- Every web global is installed eagerly, as a data property. No extension script
  is evaluated after a page script has run.
- An extension registers all of its ops or none, and deno_core's built-in table
  is in every `JsRuntime`. The ones with no place in a page are taken back by
  the extension's op middleware (`op_policy.rs`), which sees every op of the
  runtime: they keep their names in the table and throw `op is disabled`. The
  bootstrap calls each one and fails the runtime if it does not throw. They are:
  - of deno_core: `op_panic` (ends the process), `op_eval_context`,
    `op_import_sync`, `op_import_sync_with_source` (compile and run code under a
    name the caller chooses), `op_resources`, `op_is_terminal` (describe the
    host's resource table). `op_print` is not disabled but replaced: it writes
    to the page's console, never to the process's stdout or stderr. The rest of
    the built-in table stays, because the extensions' scripts call it (the
    resource ops carry `fetch` bodies and `WebSocket`s; the resource table of a
    page holds what its own requests and connections put there: no stdio, no
    file);
  - of `deno_webrtc`: what is behind `Deno.webrtc` (`createNetwork()`, which
    binds sockets on ports the program chooses, and its `Network`;
    `EncodedMediaTrack`, `readRtp()`, `readFrames()`, `setLogLevel()`), the fake
    capture devices, and `op_webrtc_data_globals`.
  - Not disabled: `op_webrtc_certificate_to_pem` and `_from_pem`. They are
    `Deno.webrtc.exportCertificate()` and `importCertificate()`, which a page
    does not have, but also how an `RTCCertificate` is structured-cloned, which
    the standard has.
- The page's `console` writes to a bounded buffer the host drains, never to the
  process's stdout or stderr (Thalora speaks MCP on stdio).

## The same-origin policy of a page's requests

A page has an origin, the one of `PageOptions::url` (an opaque origin, sent as
`null` and same origin with nothing, for a `data:`, `about:` or `file:` URL),
and its requests follow the [Fetch Standard](https://fetch.spec.whatwg.org/) for
it. `PageRuntime` puts a `deno_fetch::EmbedderWebOrigin` in the op state, which
turns on `deno_fetch`'s web-origin mode (`ext/fetch/cors.rs`); the bootstrap
fails the runtime if `fetch` is not in it. `net_allow` and the deny list are
checked as before, on every request the policy lets out, the preflight included.

- **Requests.** `mode` is `cors` (the default), `same-origin` or `no-cors`;
  `credentials` is `same-origin` by default. `CONNECT`, `TRACE` and `TRACK` are
  a `TypeError`, a URL with credentials is one in `new Request()`, and so is a
  `no-cors` request with a method other than `GET`, `HEAD` or `POST`. The
  headers a browser sets itself cannot be set: `Host`, `Cookie`, `Origin`,
  `Referer`, `Content-Length`, `Sec-*`, `Proxy-*` and the rest of the standard's
  forbidden request headers are dropped by the `Headers` of a `Request`, and
  again when the request is made. A `no-cors` request keeps its safelisted
  headers only.
- **`Origin` and `Referer`.** Every cross-origin request and every same-origin
  request that is not a `GET` or `HEAD` carries `Origin`. `Referer` follows
  `referrer` and `referrerPolicy`, by default `strict-origin-when-cross-origin`:
  the document URL to the page's own origin, its origin to another one, nothing
  from `https:` to `http:`.
- **Reading a response.** A same-origin response is `basic` (everything but
  `Set-Cookie`). A cross-origin one is read only when it passes the CORS check:
  `Access-Control-Allow-Origin` is the page's origin, or `*` for a request
  without credentials, and with credentials (`credentials: "include"`)
  `Access-Control-Allow-Credentials: true` too. Its type is `cors` and a page
  sees the CORS-safelisted response headers and those
  `Access-Control-Expose-Headers` names. A `no-cors` request to another origin
  is sent and its response is `opaque`: status 0, no headers, no body, no URL.
- **Preflight.** A cross-origin `cors` request with a method or a header that is
  not safelisted is preceded by an `OPTIONS` request
  (`Access-Control-Request-Method`, `-Headers`, no credentials, no body). The
  request is only made when the answer passes the CORS check, has an ok status
  and allows the method and every header name (`*` covers them for a request
  without credentials, never `Authorization`). Answers are cached per origin and
  URL for `Access-Control-Max-Age` seconds, 5 by default and at most 7200.
- **Redirects.** Each hop is checked like a first request. `same-origin` mode
  stops at a hop to another origin; a `cors` request that was redirected from
  another origin to yet another one (or back) has the origin `null` from there
  on; credentials in the URL of a cross-origin redirect are a network error;
  `redirect: "manual"` gives an `opaqueredirect` response and follows nothing.
- **`EventSource`** is a `cors` request (`withCredentials` makes it one with
  credentials) and takes no request headers. **`WebSocket`** and
  `WebSocketStream` send the page's `Origin` with the handshake; there is no
  CORS for a WebSocket, the server decides by that header.
- **What a page learns when the policy refuses.** `TypeError: Failed to fetch`,
  for every reason alike. The reason is written to the page's console
  (`take_console_messages()`, level error), which a page cannot read. A request
  the policy let out and the network refused (`net_allow`, the deny list, a
  connection error) fails as before, with its `cause`.

### Credentials: none yet

There is no cookie store and no HTTP authentication cache, so a request never
carries cookies or an `Authorization` the page did not set, whatever its
credentials mode, and `Set-Cookie` is dropped. The credentials mode is still
followed where the standard makes it decide something else: the CORS check (`*`,
`Access-Control-Allow-Credentials`), the wildcards of a preflight answer and of
`Access-Control-Expose-Headers`, the preflight cache.

TODO: Thalora's cookie store. `cors.rs` has the two places it plugs into,
`attach_credentials()` (the `Cookie` header of a request made with credentials,
on every hop, never on a preflight) and `store_response_credentials()` (the
`Set-Cookie` headers of its response, before they are filtered away). The store
is the embedder's and comes in with the `EmbedderWebOrigin`; `document.cookie`
reads the same one.

## What a page gets from the web extensions, and what it does not

- **`WebSocket`, `WebSocketStream`** (`CloseEvent`, `MessageEvent`,
  `WebSocketError`), with the constructors a browser has: a URL and subprotocols
  (and `signal` for a stream). `deno_websocket`'s own take request headers, a
  `Deno.HttpClient` and an undici dispatcher whose options turn certificate
  verification off; the bootstrap wraps the two constructors so that none of
  that is passed on. A host outside `net_allow` is a `NotCapable` error thrown
  by the constructor (a browser reports it as an `error` event).
- **WebRTC**: `RTCPeerConnection`, `RTCSessionDescription`, `RTCIceCandidate`,
  `RTCDataChannel`, `RTCCertificate`, the transports, senders, receivers and
  transceivers, their events, `RTCStatsReport`, `RTCEncodedAudioFrame` /
  `RTCEncodedVideoFrame`, `MediaStream`, `MediaStreamTrack`.
  - `webrtc_force_relay` (default `true`) makes every connection gather as with
    `iceTransportPolicy: "relay"`: no host or server-reflexive candidate is
    signalled, so the machine's addresses, private ones included, reach neither
    the page nor its peer, and a connection needs a TURN server. With `false` a
    page gets the candidates a browser gives it, when the permissions let it
    gather any.
  - A peer is an address, and the permissions are checked on it: a connection
    carries data only where `net_allow` covers the peer's (or the TURN server's)
    address. With a list of host names a connection is negotiated and never
    connects.
  - There is no capture device (`navigator.mediaDevices`), so a page sends data
    channels and receives media; nothing decodes or plays it.
  - WebRTC Identity (`setIdentityProvider()`, `getIdentityAssertion()`,
    `peerIdentity`, a remote `a=identity`) runs an identity provider's script in
    a worker: here it fails with a `NotSupportedError`, and no provider is
    fetched.
  - Not exposed: `Deno.webrtc`, `RTCRtpScriptTransform` (needs a worker),
    `RTCIdentityAssertion`, the SFrame interfaces.
- **`OffscreenCanvas`** with the `"bitmaprenderer"` context,
  `ImageBitmapRenderingContext`, `ImageBitmap`, `createImageBitmap()`,
  `ImageData`. `getContext("webgpu")` is `null`, there is no `navigator.gpu`,
  and no 2D context yet. Their pixels are native memory, outside
  `heap_limit_bytes`.
- **Not composed: `deno_cache`, `deno_cron`, `deno_kv`, `deno_webgpu`, workers,
  `deno_telemetry`**: not web page APIs, or they bring ops a page must not have.
  (`26_fetch.js` wants `deno_telemetry`'s tracing hooks; the bootstrap gives it
  inert ones.) So a page has no `Worker`, `caches` or `navigator.gpu`.

## Node handles

The tree lives in Rust. A JavaScript wrapper holds only its node's id, the two
halves of `NodeId::to_bits()`: the arena **index** and the slot's
**generation**, as two `u32`s in private fields. Every DOM op takes them as two
numbers (no BigInt), and Rust checks them: a stale or made-up id is an error,
never another node.

Ops that answer with a node write `[index, generation, nodeType, flavor]` into
one shared `Uint32Array` (flavor: not an element, a non-HTML element, an HTML
element); list ops return the same four numbers per node. The wrapper class
comes from the node type and, for HTML elements, the local name.

Traversal and read ops are fast calls. The ops a page can make fail
(pre-insertion validity, invalid names, selector syntax) are `#[op2(nofast)]`:
an error leaving a fast call is built without running JavaScript, so a
`DOMException` (which only has a JavaScript builder) would arrive as a plain
`Error`.

### Wrappers are never freed (this cut)

There is one wrapper per live node, kept in a map by arena index, and nothing
removes it: a node a page creates stays allocated in the document, with its
wrapper, until the `PageRuntime` is dropped. `Document::free_subtree` is not
called yet. A page that creates nodes in a loop grows until `heap_limit_bytes`
ends it; the arena itself is not under that limit. Collecting detached,
unreferenced subtrees (weak wrappers plus `free_subtree`) is the next step.

## The DOM, and what it does not do yet

Hand-written, for the host's tools (evaluate, click, type, wait, read) and the
first things page scripts touch. A WebIDL generator replaces the interface
shells later; descriptors are right where that is cheap (members on prototypes,
enumerable, `Symbol.toStringTag`, illegal constructors).

- `Node`, `Element`, `CharacterData`, `Text`, `Comment`, `DocumentType`,
  `DocumentFragment`, `Document`; `HTMLElement` and the form, link, script,
  template and common structural elements; `DOMTokenList`, `dataset`,
  `NamedNodeMap`, a `style` that is a view of the `style` attribute.
- Events: capture, target and bubble along the tree up to the window, over
  `deno_web`'s `Event` and listener lists; `on*` properties and `on*` content
  attributes; `UIEvent`, `MouseEvent`, `PointerEvent`, `KeyboardEvent`,
  `InputEvent`, `FocusEvent`, `SubmitEvent`; click activation (checkbox, radio,
  submit, reset, link, label), form submission and reset, focus.
- `window`, `document`, `location`, `history`, `navigator`, timers,
  `requestAnimationFrame`, `fetch` and the rest of the web extensions' globals.

Not yet:

- **Collections are snapshots.** `childNodes`, `children`,
  `getElementsByTagName`, `form.elements` and the others are not live and not
  identity-stable.
- No layout: no sizes or positions, no `getComputedStyle`, no
  `getBoundingClientRect`, no `matchMedia`, no viewport.
- No `MutationObserver` (the DOM crate records mutations; nothing reads them),
  ranges, selection, shadow DOM, custom elements, `normalize()`.
- No `document.cookie` (and `fetch` sends no cookies); `javascript:` URLs do not
  run; constraint validation always passes; no file inputs.
- Event handler content attributes run without the HTML scope chain (`this` and
  `event` only). Form association by the `form` attribute works; `<base href>`
  is ignored.
- Fingerprint surface is not Chrome's: `deno_web`'s `Event` and `EventTarget`
  carry own symbol properties and a few non-standard members, unknown HTML
  elements are plain `HTMLElement`s, `navigator` is minimal.
- **`isTrusted` can be forged.** `deno_web` keeps an event's trust in an own
  symbol property of the event (`Object.getOwnPropertySymbols(event)` finds it),
  and `02_event.js` offers no other place for it: a page script can make its own
  event read `isTrusted === true` to another script of the page. No decision of
  the host rests on `isTrusted`: what a click does is the same for a script's
  click and the host's `click()`, and a label only passes the flag of its click
  on to the click it sends its control. Closing it takes a change to
  `deno_web`'s `Event` (trust in a slot scripts cannot reach).
- No cookies and no HTTP authentication: no request carries a credential the
  page did not write itself (see the same-origin policy above for what the
  credentials mode still decides). `document.cookie` is not there.
- Of the same-origin policy, not yet: mixed content blocking (an `https:` page
  can fetch `http:`), CSP, `Cross-Origin-Resource-Policy`,
  `X-Content-Type-Options: nosniff`, subresource integrity, the `Sec-Fetch-*`
  request headers, the request `cache` modes and `keepalive`. A page's own
  navigation (`history.pushState()`) does not change the URL its referrer is
  taken from.
- Dialogs have no user: `alert()` returns, `confirm()` is `false`, `prompt()` is
  `null`.

## Tests

`tests.rs` (Rust unit tests, each on a current-thread tokio runtime): DOM round
trip and Rust-side visibility, the op allowlist and the disabled ops, no `Deno`
/ `require` / `process` and no `ext:` imports, fetch and WebSocket confinement
(`ERR_ADDRESS_DENIED`, the permission error, `file:`, the browser's WebSocket
constructor), the same-origin policy against three loopback servers (CORS with
and without credentials, exposed headers, the preflight and its cache, `no-cors`
and `same-origin`, redirects across origins, forbidden headers, `Origin` on
requests and WebSocket handshakes, a page with an opaque origin), an
`RTCPeerConnection` offer and WebRTC Identity's `NotSupportedError`,
`OffscreenCanvas` without a GPU and `createImageBitmap()`, timers and the
watchdog, click default actions and form submission, `DOMException`s, event
propagation and lifecycle, uncaught errors, JSON evaluation with and without
`await`, origin-keyed storage, the heap limit.
