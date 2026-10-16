# `deno desktop --inspect-renderer`: design note (spike, `denext.21`)

## Goal

`denext desktop dev` should let a developer, a test runner or an agent attach to
the **window's renderer** (the page, not the Deno runtime) over the Chrome
DevTools Protocol: read the DOM, evaluate script, take screenshots, drive input,
profile. Today the only renderer debugger is the DevTools window the app opens
itself, and `--inspect` only reaches the renderer through the unified
multiplexer (`cli/tools/desktop_devtools.rs`), which needs the Deno inspector
too and works only on CEF.

## What exists upstream

- `deno desktop --inspect-renderer[=<addr>]` is parsed
  (`libs/cli_parser/src/defs.rs`, default `127.0.0.1:0`) but only used **with
  `--inspect`**: it picks the CEF renderer's internal port behind the mux. Alone
  it did nothing, and a bare `--inspect-renderer` (port 0) handed the host a
  port it ignores.
- The CEF host already takes `LAUFEY_REMOTE_DEBUGGING_PORT` and sets CEF's
  `remote_debugging_port` (all three OSes), only while DevTools are on
  (`LAUFEY_INSPECTABLE`, API 40). Chromium then serves `/json/list` and a
  WebSocket per page on loopback.

## The spike (this branch)

`cli/tools/desktop_renderer_inspect.rs`, wired into `run_desktop_hmr`:

1. `--inspect-renderer` alone now runs the app as a development build (the same
   path as `--hmr` / `--inspect`: the dylib under `deno_dir`, the prebuilt host,
   `Metadata::desktop_dev`).
2. Port 0 is replaced by a free port (also behind the mux).
3. `plan(backend, os, addr)` maps the request to the backend's own mechanism,
   given to the host process as environment:

   | backend            | protocol                     | environment                                                              |
   | ------------------ | ---------------------------- | ------------------------------------------------------------------------ |
   | CEF (every OS)     | CDP, `http://127.0.0.1:<p>`  | `LAUFEY_REMOTE_DEBUGGING_PORT=<p>`                                       |
   | WebView2 (Windows) | CDP, `http://127.0.0.1:<p>`  | `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` += `--remote-debugging-port=<p>` |
   | WebKitGTK (Linux)  | WebKit's inspector (not CDP) | `WEBKIT_INSPECTOR_HTTP_SERVER=<addr>`                                    |
   | WKWebView (macOS)  | none remote                  | none: Safari > Develop (the view is `inspectable`)                       |

4. `announce` prints the endpoint and, for CDP, polls `/json/list` in the
   background until the window's page appears, then prints its
   `webSocketDebuggerUrl` and a DevTools frontend URL.

No laufey change is needed: the CEF host's port setting and the WebView2
environment override already exist, and both are refused by a packaged app with
DevTools off (`WebView2EnvironmentOverridesToClear`, the CEF switch gating).

### Verified

- Unit tests (`desktop_renderer_inspect::tests`): the per-backend plans, the
  `/json/list` page parsing, the HTTP status check.
- CEF on Linux (an x86_64 Ubuntu box under Xvfb, the debug CLI and libdenort of
  this branch, laufey `df1d8c8`):
  `deno desktop --backend cef
  --inspect-renderer=127.0.0.1:9339 main.ts`
  printed the endpoint; a Deno CDP client fetched `/json/list`, opened the
  page's WebSocket (`ws://127.0.0.1:9339/devtools/page/<id>`, page
  `app://localhost/`) and ran `Runtime.evaluate`, which returned the page's
  title, origin and a textarea's value.
- Not run: WebView2 (no reachable Windows machine; the mechanism is Microsoft's
  documented `--remote-debugging-port` browser argument through the documented
  environment override), WebKitGTK's HTTP inspector, macOS.

## `denext desktop dev` integration (next step, denext side)

`denext desktop dev` does not go through `deno desktop --hmr`: it builds the app
into a temporary directory and starts the packaged executable itself
(`src/cli/commands/desktop.ts`). It should take the same flag,
`denext desktop dev --inspect-renderer[=<addr>]`, and put the same environment
on that launch (the table above; keep it in sync with `plan()`), then print the
page's WebSocket as `announce` does. The MCP tools (`denext_component_tree`,
`denext_profile`) could then attach to a desktop window the way they attach to a
browser tab.

## Security

A remote debugging port gives any local process full control of the page and its
origin's storage and cookies. Keep it to development builds: CDP listens on
loopback only (Chromium's choice, and `plan()` forces 127.0.0.1); the flag
exists only on the dev path; a packaged app with DevTools off clears the
settings (laufey API 40). Chromium's `--remote-allow-origins` is left unset, so
a WebSocket from a web page (which carries an `Origin`) is refused; CDP clients
outside a browser send none.

## Open questions

- WebKitGTK: `WEBKIT_INSPECTOR_HTTP_SERVER` serves WebKit's own frontend to a
  browser, not CDP. A WebKit-to-CDP bridge (as in Playwright's WebKit) is out of
  scope; is the HTTP inspector enough for denext's tools?
- WKWebView: there is no remote port. Safari's Web Inspector (or
  `ios_webkit_debug_proxy`-style tooling for macOS) is the only route; denext
  could point users at `--backend cef` when they need CDP on macOS.
- Several windows: each is a page target in `/json/list`; `announce` prints
  every page found on the first poll that finds one. A window opened later is
  listed by `/json/list` but not announced.
