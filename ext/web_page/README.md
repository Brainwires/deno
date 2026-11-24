# deno_web_page

A runtime for **hostile web-page JavaScript**: the page context of
[Thalora](https://github.com/Brainwires/thalora-web-browser), the headless
browser, and later of a confined page process for denext apps.

A `PageRuntime` is a plain `deno_core::JsRuntime` made of the web extensions and
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
let mut page = PageRuntime::new(options)?; // parses the markup, boots the context

page.execute_script("https://example.com/app.js", source)?; // classic script
page.fire_dom_content_loaded()?;
page.fire_load()?;
page.run_until_idle(Duration::from_secs(2)).await?; // timers, fetches, promises

let title = page.evaluate_json("document.title".into())?; // serde_json::Value
let button = page.with_document(|doc| doc.get_element_by_id("go")).unwrap();
let outcome = page.click(button)?; // .navigation: a link or form for the host
page.set_value(input, "typed text")?; // value + input and change events
```

| Call                                                | What it does                                                                                                                                                                               |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `PageRuntime::new(options)`                         | Parses `html` into a `thalora_dom::Document`, boots the context. No page script has run.                                                                                                   |
| `execute_script(name, source)`                      | Runs a classic script, then its microtasks. An uncaught exception is `PageError::Js` (message and stack).                                                                                  |
| `evaluate_json(source)`                             | The completion value as JSON: `undefined` is `null`; what JSON cannot express is its string form; a node is its markup. Promises are not awaited.                                          |
| `run_until_idle(budget)`                            | Pumps the event loop until nothing is pending (`Idle::Quiescent`) or the budget is spent (`Idle::BudgetExhausted`). Errors in tasks do not stop it.                                        |
| `with_document(f)`                                  | The live document, read from Rust.                                                                                                                                                         |
| `click(node)`                                       | Pointer and mouse events, focus, a trusted `click`, then the default action. A checkbox or radio button toggles; a submit button or a link is reported in `ClickOutcome` (never followed). |
| `set_value(node, value)`                            | Sets an `<input>`, `<textarea>` or `<select>` as a user would, fires `input` and `change`.                                                                                                 |
| `form_state(node)`                                  | The live value and checkedness of a form control (they are not content attributes).                                                                                                        |
| `fire_dom_content_loaded()`, `fire_load()`          | `readyState` and the lifecycle events, when the host says so.                                                                                                                              |
| `terminate_handle()`                                | A `v8::IsolateHandle` for a watchdog thread; the interrupted call fails with `PageError::Terminated` and the page stays usable.                                                            |
| `take_console_messages()`, `take_uncaught_errors()` | What the page logged, and the errors nothing caught.                                                                                                                                       |
| `take_navigation_requests()`                        | Navigations scripts asked for outside a click (`location.href = …`, `form.submit()`).                                                                                                      |
| `op_names()`                                        | The op table, for the allowlist test.                                                                                                                                                      |

A `PageRuntime` never navigates and never loads a script by itself: fetching
scripts, CSP, SRI and navigation are the host's.

## Security model

Page JavaScript is hostile. What contains it:

|   | Boundary                | Status           | How                                                                                                                                                                                                                                                                                                                                                              |
| - | ----------------------- | ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| a | No privileged ops       | by construction  | Only `deno_webidl`, `deno_web`, `deno_fetch`, `deno_webstorage`, `deno_crypto` and this crate are composed. No file system, environment, FFI, subprocess, listening socket, KV or OS op is registered. `tests.rs` holds the op table against `op_allowlist.rs`.                                                                                                  |
| b | Network                 | enforced         | A real `PermissionsContainer` built from parsed descriptors: net for `net_allow` only, everything else denied, no prompt. On top of it, the page's only HTTP client is built with `denyAddresses` (default `loopback`, `private`, `link-local`, `metadata`), checked on the resolved address of every connection and redirect. `file:` fetches are refused.      |
| c | Storage                 | origin-scoped    | `localStorage` is a directory per origin under the host's `storage_dir` (`std::fs`, never a virtual file system); an opaque origin has none. `sessionStorage` is in memory, per page.                                                                                                                                                                            |
| d | Isolation between pages | per-page isolate | One isolate per `PageRuntime`. `BroadcastChannel` and blob URLs are per page too.                                                                                                                                                                                                                                                                                |
| e | Memory and CPU per page | built            | `heap_limit_bytes` sets V8's heap limit; the near-heap-limit callback ends the page (`PageError::OutOfMemory`), not the process. `terminate_handle()` lets a watchdog end a long task.                                                                                                                                                                           |
| f | V8 sandbox              | **off**          | The fork's V8 is built without `v8_enable_sandbox`. A V8 memory-corruption bug in page JavaScript is a compromise of the process, so the process is the boundary: standalone Thalora runs confined (`deno_sandbox`), and inside a denext app pages run in a separate confined page process. In-process page isolates next to an app's backend are not supported. |

How page JavaScript is kept from the ops (a):

- Every op is imported by the extension's own modules. The bootstrap
  (`99_page_main.js`) deletes `globalThis.Deno` and `globalThis.__bootstrap`
  before it returns, and fails the runtime if they are still there.
- `import("ext:…")` from a page is refused by deno_core once the extensions have
  loaded, and there is no module loader for anything else.
- Every web global is installed eagerly, as a data property. No extension script
  is evaluated after a page script has run.
- deno_core's built-in table is registered in every `JsRuntime` and contains ops
  no page must reach (`op_print`, `op_panic`, `op_eval_context`, the resource
  ops). They are as unreachable as the rest; nothing else protects them.
- The page's `console` writes to a bounded buffer the host drains, never to the
  process's stdout or stderr (Thalora speaks MCP on stdio).

## What is left out, and why

- **`deno_websocket`, `deno_webrtc`**: their connections do not go through the
  fetch connector, so neither `denyAddresses` nor its redirect and DNS-rebinding
  checks apply: a page could reach loopback and internal services. `deno_webrtc`
  also requires `deno_net`, which registers listening-socket ops. They come back
  when they honour the deny list.
- **`deno_canvas`**: requires `deno_webgpu`.
- **`deno_cache`, `deno_cron`, `deno_kv`, workers, `deno_telemetry`**: not web
  page APIs, or they bring ops a page must not have. (`26_fetch.js` wants
  `deno_telemetry`'s tracing hooks; the bootstrap gives it inert ones.)
- So a page has no `WebSocket`, `RTCPeerConnection`, `Worker`, `caches`,
  `OffscreenCanvas` or `navigator.gpu`.

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
- Dialogs have no user: `alert()` returns, `confirm()` is `false`, `prompt()` is
  `null`.

## Tests

`tests.rs` (Rust unit tests, each on a current-thread tokio runtime): DOM round
trip and Rust-side visibility, the op allowlist, no `Deno` / `require` /
`process` and no `ext:` imports, fetch confinement (`ERR_ADDRESS_DENIED`, the
permission error, `file:`), timers and the watchdog, click default actions and
form submission, `DOMException`s, event propagation and lifecycle, uncaught
errors, JSON evaluation, origin-keyed storage, the heap limit.
