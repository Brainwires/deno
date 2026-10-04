# Brainwires/deno: denext's Deno Desktop runtime fork

This is a fork of [denoland/deno](https://github.com/denoland/deno) that exists
for one reason: to ship a patched **Deno Desktop runtime** (the `libdenort`
library `deno desktop` embeds an app into, plus
[laufey](https://github.com/littledivy/laufey)'s backend hosts) for
[denext](https://github.com/Brainwires/denext) 3.1 without waiting for the
patches to land upstream.

It is not a general-purpose Deno distribution. The `deno` CLI is **not**
rebuilt: denext runs the stock `deno desktop` CLI (2.9.7) and points it at the
prebuilt runtime from this fork's releases:

```sh
DENORT_DESKTOP_BIN=<unpacked archive>/libdenort.dylib   # .so on Linux, denort.dll on Windows
LAUFEY_DEV_DIR=<unpacked archive>/laufey
deno desktop --backend webview|cef ...
```

## Exit criterion

This fork goes away when **stock Deno ships these features**: once a Deno
release carries the in-process memory transport, the configured app origin, and
the per-app identifier handed to every laufey backend (and laufey releases the
matching backend changes), denext uses the stock runtime and this fork is
archived.

## Branches and tags

| Ref                                                                                 | What it is                                                                                                                                                                                               |
| ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `rel/denext<n>` (e.g. `rel/denext7`)                                                | The release line runtime `denext.<n>` is built from: the previous line + the feature and fix branches merged for it (`--no-ff`). Releases since `denext.6` are cut here.                                 |
| `denext/v<deno version>` (e.g. `denext/v2.9.7`)                                     | The original line: upstream tag `v<deno version>` + the first feature commits + this file and the release workflow. The default branch of this fork.                                                     |
| `feat/desktop-*`, `fix/*`, `ci/*`, `test/*`                                         | The feature and fix branches the release lines are built from. Kept for review and for rebasing onto a newer Deno.                                                                                       |
| `denext-runtime-v<deno version>-denext.<n>` (e.g. `denext-runtime-v2.9.7-denext.1`) | A published runtime. PUSHING such a tag runs the workflow and creates the GitHub Release (a manual dispatch on a tag ref builds but never publishes). `<n>` counts runtime releases on one Deno version. |

`main` and the other upstream branches are untouched mirrors from the fork
point. Nothing here is proposed back from this fork; the upstream PRs below are
where the changes are discussed.

## What is patched (on top of `v2.9.7`)

1. **`feat(net,http)`: in-process memory transport for `Deno.serve`** —
   `DENO_SERVE_ADDRESS=memory:<name>`. Ported from
   [denoland/deno#35675](https://github.com/denoland/deno/pull/35675).
2. **`feat(desktop)`: serve the app at a stable, configured origin** —
   `desktop.app.origin` (`<scheme>://<host>`), a custom-scheme handler that
   bridges webview requests into `Deno.serve` over the memory transport, a
   WebSocket relay that checks the `Origin` header.
3. **`feat(desktop)`: read the app origin from an embedded
   `.deno-desktop/app.json`** — so a stock CLI that rejects `desktop.app.origin`
   in `deno.json` can still configure it:
   `{ "origin": "<scheme>://<host>", "identifier": "<reverse-DNS id>" }`,
   embedded with `"compile": { "include": [".deno-desktop/app.json"] }`.
4. **`fix(desktop)`: set the Linux app_id so Wayland shows the configured icon**
   — a port of
   [denoland/deno#35662](https://github.com/denoland/deno/pull/35662) by Leo
   Kettmeir ([@crowlKats](https://github.com/crowlKats)), authored by him;
   squashed and re-applied onto `v2.9.7`.
5. **`feat(desktop)`: hand every laufey backend the app identifier** —
   `LAUFEY_APP_ID`, so each app's web storage (localStorage, IndexedDB, cookies)
   lives in its own directory and persists across launches.
6. **`feat(desktop)`: deep links, opened files and second instances in
   `Deno.desktop`** — `openurl` / `openfile` / `secondinstance` events plus
   `Deno.desktop.launchUrls` / `launchFiles`, `desktop.app.singleInstance`, and
   a `laufey-launch.json` in every package. Built on laufey's `on_open_url`
   ([littledivy/laufey#77](https://github.com/littledivy/laufey/pull/77) by
   [@diegoholiveira](https://github.com/diegoholiveira)) and
   `on_second_instance`.
7. **`fix(http)`: an absolute-form request target can't claim `http+memory:`
   over TCP** (400), and **`feat(os)`: an env overlay** so the runtime never
   calls `setenv` while the host's UI thread runs.
8. **`feat(desktop)`: register deep-link schemes with the OS and report their
   owner** — `Deno.desktop.getSchemeOwner` / `registerScheme({ force })`, a
   first-launch registration that never takes a scheme another app owns (Windows
   HKCU, macOS LaunchServices, Linux XDG), and the Windows `.msi` writing the
   same keys at install.
9. **`feat(desktop)`: native passkeys** — `Deno.desktop.passkeys`
   (`capabilities` / `create` / `get`) on macOS AuthenticationServices and
   Windows `webauthn.dll`, with the request/response envelope of
   `@clerk/electron-passkeys`.
10. **`feat(desktop)`: window state, size limits, screens, chrome, cancelable
    close and quit** — `BrowserWindow` maximize / minimize / fullscreen with
    events, min / max size, `getBounds` / `setBounds` / `getNormalBounds`, title
    bar styles, traffic-light position, Mica / Acrylic and vibrancy;
    `Deno.desktop.screens()`, `"displaychanged"`, `windowCapabilities()`,
    `quit()` (cancelable, Electron's `app.quit()`) and `quitOnLastWindowClosed`;
    a cancelable `close` event (5 s answer timeout); the tray-only and
    off-screen placement fixes. On top of
    [denoland/deno#36761](https://github.com/denoland/deno/pull/36761)
    (`devicePixelRatio`, inner / outer size, `screenX` / `screenY`, by Kenta
    Moriuchi, [@petamoriken](https://github.com/petamoriken)) and
    [denoland/deno#36789](https://github.com/denoland/deno/pull/36789)
    (`desktop.initialWindow`, by Sasivarnan R,
    [@sasivarnan](https://github.com/sasivarnan)), cherry-picked with their
    authorship; `initialWindow` is also read from the embedded
    `.deno-desktop/app.json`.
11. **`fix(desktop)`: Node-API addons load on Windows** — addons look the
    Node-API functions up in the host executable (node-gyp's delay-load hook,
    napi-rs / neon `GetProcAddress(GetModuleHandle(NULL))`), which in a desktop
    app is laufey's host and exports nothing, so the first Node-API call killed
    the process (`0xC06D007F`). The runtime now gives the executable an
    in-memory export table whose Node-API entries jump to the DLL's functions
    (`cli/rt_desktop/napi_host_exports.rs`). macOS and Linux already resolved
    them (the dylib's exports promoted to `RTLD_GLOBAL`, with
    denoland/deno#36718's Linux flag fix). The runtime smoke test loads a probe
    addon (`.github/denext-runtime/napi-probe`) on every target.

12. **`feat(desktop)`: drag and drop, native file dialogs and the rich
    clipboard** — `BrowserWindow` `dragenter` / `dragover` / `dragleave` /
    `drop` with native paths and `startDrag({ files, icon })`;
    `Deno.desktop.dialog.showOpenDialog` / `showSaveDialog` (the OS's own
    dialogs, AbortSignal-cancellable, never blocking the runtime);
    `Deno.desktop.clipboard` (text, HTML, PNG images, `availableFormats()`, a
    `"change"` event); new `windowCapabilities()` keys (laufey API 39).

13. **`feat(desktop)`: global shortcuts, launch at login and DevTools control**
    — `Deno.desktop.shortcuts` (`register(accelerator,
    callback?)` resolving
    with the canonical accelerator, `unregister`, `unregisterAll`,
    `isRegistered`, `list`, `canonicalize`, a `"shortcut"` event; errors carry a
    `code`: `invalid`, `conflict`, `already_registered`, `not_supported`,
    `denied`); `Deno.desktop.launchAtLogin` (`get()` / `set(enabled)`: `enabled`
    / `disabled` / `requires-approval` / `not-supported`);
    `Deno.desktop.devtools` (`enabled`, `open` / `close` / `toggle` / `isOpen`)
    and `BrowserWindow.closeDevtools()` / `toggleDevtools()` /
    `isDevtoolsOpen()` / `isDevtoolsEnabled()`; `openDevtools()` does nothing
    when the app launched with DevTools off (`LAUFEY_INSPECTABLE=0` /
    `"inspectable": false`), in dev mode too (laufey API 40).

14. **`feat(desktop)`: menu accelerators and close events, scheduled and
    actionable notifications** — menu `accelerator`s work on every backend
    (`Deno.desktop.menuCapabilities()` says what each supports);
    `showContextMenu` returns a promise of the chosen id (`null` when dismissed)
    and fires `contextmenuclose`. `Notification` takes `actions` and `data`,
    fires a separate `action` event (actions are no longer folded into `click`)
    and reports `Notification.maxActions`; `Deno.desktop.notifications` adds
    `schedule({ at, ... })`, `getScheduled()`, `cancel(tag)`, `capabilities()`
    and `requestPermission({ provisional })`. A click on a notification of an
    earlier run, or the one that launched the app, is a `notificationresponse`
    event, and `Deno.desktop.launchNotificationResponses` holds the ones that
    arrived before the app listened (laufey API 41).

15. **`feat(desktop)`: OS auth sessions and native code on the UI thread** —
    `Deno.desktop.authSession` (`capabilities()`,
    `start({ url,
    callbackScheme | callbackUrl, ephemeral, window })`
    resolving with the callback URL) runs `ASWebAuthenticationSession` on macOS,
    with a real `cancelled` when the user closes the sheet; Windows and Linux
    have no OS equivalent and reject with code `not_supported` (RFC 8252: the
    system browser). `Deno.desktop.runOnMainThread(fn, context)` calls a native
    `void* (*)(void*)` (an `UnsafeFnPointer`, `UnsafeCallback` or pointer) on
    the UI thread and resolves with its return value as a bigint; it needs
    `--allow-ffi` and rejects instead of hanging once the app is quitting. Both
    are main-scope only (laufey API 42, which also stops WebKitGTK's
    custom-scheme writes from blocking the event loop).
    `Deno.desktop.authSession.cancel()` (laufey API 43) ends the running session
    when the app gives up on it: the sheet closes and `start()` rejects with
    `cancelled`, exactly once; it returns `false` when no session is running
    (always on Windows and Linux).

16. **`fix(desktop)`: `Notification.close()` no longer deadlocks** — it held the
    runtime's notification map lock while the backend waited for the thread
    whose close event takes that lock (found by the e2e suite).
17. **`fix(desktop)`: the update helper waits for the install's processes on
    Windows** — a CEF subprocess outliving its browser process kept the install
    directory from being renamed, failing the rollback of an unconfirmed trial
    (found by the e2e suite).
18. **`feat(desktop)`: full-app self-update (`Deno.desktop.updater`)**
    (`1511cdaa20`) — a signed manifest (ECDSA P-256 over
    `"denext-app-update-v1\n" + signed`), a size-capped and SHA-256-checked
    download, a safe extractor, the OS code-signature check against the running
    app (same Developer ID Team ID + Gatekeeper on macOS, same Authenticode
    signer on Windows), an atomic swap by a helper process, and a rollback of a
    version not confirmed by its next launch. With its fixes: a rolled-back
    install is finished deleting at the next start (`5e009fb794`), the macOS
    helper finds the bundle's runtime (`3c6217526f`), and the ops act only
    inside a packaged app (`5132a43948`).
19. **`fix(desktop)`: the JIT entitlement for webview bundles** (`346cb47225`)
    and **`Dock.setBadge(null)` clears the badge** (`16f8a904a4`, it showed
    "null").
20. **`fix(desktop)`: the 3.1.0 fork-code audit** (`fix/fork-audit`):
    - `node:http` / `node:https` / `node:http2` serve under the memory transport
      (they failed with "unknown override kind: 5", so a framework on
      `node:http` never served); the socket's `remoteAddress` is
      `memory:<name>`;
    - a string with an embedded NUL is a `TypeError` at every op that hands it
      to laufey (the laufey crate panicked, and a panic exits the app);
    - `op_desktop_alert_async` survives bootstrap, so the uncaught-error dialog
      shows again;
    - the close request's 5 s answer timeout does not count time the JavaScript
      thread spends in a synchronous `alert` / `confirm` / `prompt` (a close
      listener's `confirm()` no longer closes the window under the user);
    - a window with a WebGPU surface is hidden, never destroyed, on every close
      path (the user's, a timed-out request, DevTools), not only `close()`;
    - the first `BrowserWindow` adopts the bootstrap window only when its
      creation-time options (`frameless`, `noActivate`, `transparent`,
      `transparentTitlebar`) agree; a tray panel is a real panel again;
    - the event queue coalesces pointer motion, wheel, resize and move, drops
      only motion and wheel when full, and never loses a discrete event (a lost
      `contextMenuClose` used to wedge `showContextMenu()`); a binding's handler
      no longer keeps the queue alive;
    - `BrowserWindow` / `Dock` / `Tray` / `Notification` throw `NotSupported` in
      workers (they panicked) and are kept out of worker scope;
    - `DENO_SERVE_ADDRESS=memory:` is not inherited by child processes (env
      overlay variables can be process-local:
      `deno_os::set_env_overlay_var_not_inherited`);
    - an HTTPS error report gives up after 5 s instead of hanging the exit;
    - the scheme bridge drops a request the webview cancelled (laufey's
      `on_cancel`, `SchemeExchange::is_cancelled`), so the app's
      `request.signal` aborts and a long poll or an endless stream ends; it
      honours short writes (it dropped the rest of the chunk) and waits instead
      of buffering a slow reader's stream, drops a forwarded `content-length` /
      `expect`, keeps non-ASCII header values, and rewrites `http+memory://`
      URLs in `Location` / `Content-Location` / `Refresh` onto the app origin;
    - the updater: a `check()` during a download no longer relabels it, the
      state file reads across versions (no `deny_unknown_fields`), a reused
      trial PID is not taken for the trial (process start time), an executable
      without its execute bit is refused at `stage()`, and large deletions and
      the download's fsync run off the JavaScript thread;
    - the config schema describes `desktop.initialWindow`, `errorReporting`,
      `macos` and icon sets;
    - the fork's `deno desktop` refuses to download the laufey release hosts (an
      older C ABI the runtime can't load) and says to set `LAUFEY_DEV_DIR`;
    - CI: only a tag push publishes, the lockfile check compares every package
      but laufey and the build is `--locked`, and the archive check fails on an
      unresolved macOS dependency and checks Windows imports (`pe_deps.py`).

21. **`fix(desktop)`: the 3.1.1 audit** (`fix/audit-3-1-1`, runtime `denext.9`,
    laufey API 44):
    - a forked worker runs only for the app's own runtime: both fork shapes
      (`<App> run x.js`, and a compiled binary's `<App> x.js` with
      `DENO_INTERNAL_CHILD_ENTRYPOINT`) need a per-launch
      `DENO_DESKTOP_WORKER_TOKEN` naming the real parent (which runs the same
      executable) and an inherited IPC channel; a packaged app forks only
      modules under its embedded file system, and a worker exits when its module
      is done; any other such launch exits at once;
    - `node:child_process` no longer copies the process-local
      `DENO_SERVE_ADDRESS=memory:` into a child's environment;
    - the scheme bridge: a non-ASCII `Location` no longer panics; a request from
      a document of another origin (as far as the engine discloses `Origin` /
      `Sec-Fetch-Site`) carries the `x-deno-desktop-cross-origin` header; a body
      the backend failed to deliver is a 400; laufey's write backpressure (API
      44: 0 means retry the same bytes) stops on a cancel;
    - the WebSocket relay requires a per-launch token in the request target
      besides the exact `Origin`: `DENO_DESKTOP_WS_URL` is the relay origin plus
      `/.deno-desktop-relay/<64 hex>`, and child processes don't inherit it;
    - bindings answer only the app's own documents (the app origin, a
      development run's dev server) unless `bind(name, fn, { origins })` opts
      another origin in; `{ withCaller: true }` passes the caller's
      `{ origin, windowId }`;
    - the updater: every PE file pinned to the running app's signer (issuer +
      subject), the staged app's own version checked against the manifest,
      required manifest `expiresAt` / `sequence` (a lower sequence than the
      highest accepted is `replayed`), a set of rejected versions, crash-safe
      swap and rollback (atomic exchange, a checked undo), an OS file lock for
      the helper, symlink-safe owned state / log / lock files, extracted modes
      masked to `0o755` and symlinks kept inside the app;
    - reading / watching the clipboard, global shortcuts, `launchAtLogin.set`,
      `registerScheme({ force: true })` and OS notifications need unscoped
      `--allow-sys`;
    - launch arguments: after the scheme registration's `--` only one
      declared-scheme link counts, and a network path is never checked; macOS
      `openURLs` drops undeclared schemes; `runOnMainThread` refuses a
      `Deno.UnsafeCallback`; page values become own data properties; `close()`
      of a WebGPU window closes it; a NUL title is refused before the window
      exists; the error report leaves the JavaScript thread; a closed window's
      JavaScript state and old notifications are forgotten.

The runtime-side parts (1-3, 5-21) are what the prebuilt `libdenort` carries.
The CLI-side parts (for example writing `LAUFEY_CUSTOM_SCHEMES` /
`LAUFEY_APP_ID` into a packaged app's launchers) are in the branch too, but a
stock CLI does not run them; denext's own launcher provides that environment.

The laufey backend hosts are built from
[Brainwires/laufey](https://github.com/Brainwires/laufey) branch
`denext/integration` (registered schemes, app data dir, WebKitGTK scheme request
bodies, launch config, open-url / single instance, passkeys, the window API with
littledivy/laufey#80 and #81, drag and drop / file dialogs / rich clipboard,
global shortcuts / launch at login / DevTools control, menu accelerators /
notifications, UI-thread tasks / auth sessions / non-blocking WebKitGTK scheme
bodies, Windows bindgen fix, and API 43: auth session cancel, Local Network
Access for the registered custom-scheme origins on CEF, Windows menu and
file-dialog fixes, CEF file drops; then the 3.1.0 audits: scheme request
cancellation (`on_cancel` on every backend), no aborts on a NUL in a string, the
Linux window-destroy use-after-free, sync UI hops that can't hang exit; and API
44, the 3.1.1 audit: each JS call's document origin, the launch file's
`bridgeOrigins`, scheme response write backpressure, a packaged app loading only
its own runtime and pinned launch keys, WebKitGTK sub-frames kept off the
bridge, a strict bridge JSON parser), at the commit pinned by `LAUFEY_SHA` in
the workflow (or the `laufey_ref` input). The same commit is the `laufey` git
dependency of `cli/rt_desktop/Cargo.toml` (crate 0.8.0, API 44).

laufey's `init_api` rejects any C ABI version mismatch between the runtime and
the host, so the `laufey` crate libdenort links and the hosts are built from the
**same** laufey revision: the workflow patches whatever source `Cargo.lock`
names for `laufey` (crates.io, or a git rev of Brainwires/laufey) with a path to
that revision's `capi/`, records the `LAUFEY_API_VERSION` it compiled, and the
package job fails if it differs from the hosts' `capi/include/laufey.h`.

## Release archives

One archive per target and backend:
`deno-desktop-runtime-<deno version>-denext.<n>-<target>-<backend>.tar.gz`
(`.zip` on Windows). Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`. Backends: `webview`, `cef`.

```text
libdenort.dylib | libdenort.so | denort.dll    -> DENORT_DESKTOP_BIN
laufey/                                         -> LAUFEY_DEV_DIR
  webview/build/laufey_webview.app              (macOS, webview)
  webview/build/laufey_webview[.exe]            (Linux / Windows, webview)
  cef/build/Release/laufey.app                  (macOS, cef)
  cef/build/Release/laufey[.exe], libcef.*, ... (Linux / Windows, cef)
BUILD_INFO.json                                 deno + laufey SHAs, CEF version, run URL
licenses/                                       deno, laufey (and CEF) licenses
```

The `laufey/` paths are the build-tree paths the stock CLI searches under
`LAUFEY_DEV_DIR` (`cli/tools/desktop.rs`, `locate_dev_backend_binary` /
`locate_dev_app_bundle`). On Linux and Windows the CLI copies the whole
directory holding the backend binary into the packaged app, so those directories
hold only the backend's runtime files.

Each release also has `SHA256SUMS` and `manifest.json` (target -> backend ->
url, sha256, size; plus the deno SHA, laufey SHA and CEF version). Every archive
has a build provenance attestation:

```sh
gh attestation verify deno-desktop-runtime-<...>.tar.gz -R Brainwires/deno
```

Binaries are not code-signed or notarized: an app is signed with its author's
identity when it is packaged. The macOS `libdenort.dylib` is ad-hoc signed after
stripping, because arm64 macOS will not load unsigned code.

## Reproducing a build

The workflow is `.github/workflows/denext_runtime.yml` (helpers in
`.github/denext-runtime/`). Run it from the Actions tab (`workflow_dispatch`,
optional `targets` filter, `laufey_ref` override, and `reuse_*_run_id` to reuse
a previous run's libdenort or laufey build), or push a `denext-runtime-v*` tag
to publish. It also runs on every push to a `rel/**` branch and nightly on the
default branch. A tag publishes only when `denext tests` (`denext_tests.yml`)
passed on exactly the tagged commit; the setup job checks and fails otherwise.
Per target it:

1. builds `libdenort` with `cargo build --release --locked -p denort_desktop`
   after patching `laufey` (`cargo metadata` rewrites laufey's lockfile entry;
   `lock_drift.py` then fails the job if any other package's name, version,
   source, checksum or dependencies moved), with the release profile (fat LTO,
   `codegen-units = 1`), on the runner of that target (Intel macOS on
   `macos-15-intel`, Linux on `ubuntu-22.04` / `ubuntu-22.04-arm` so the glibc
   baseline matches laufey's WebKitGTK 4.1 requirement of Ubuntu 22.04+);
2. builds laufey's webview and CEF hosts with laufey's `make webview` /
   `make cef` (the CEF minimal distribution is downloaded by the Makefile);
3. assembles the archives, checks architectures and dynamic dependencies (`file`
   / `lipo` / `otool -L` / `ldd`, and the import table of every Windows `.exe` /
   `.dll` with `pe_deps.py`; any dependency that resolves nowhere fails it),
   packages a small `Deno.serve` app with the stock `deno desktop` 2.9.7 for
   both backends, and launches it (Linux under Xvfb): the page, served from
   `t3code://app`, POSTs back to `Deno.serve` and the app exits. A launch
   failure fails the job;
4. attests the archives and, on a pushed tag, publishes the release.

Locally, the equivalent is:

```sh
cargo build --release -p denort_desktop                   # this branch
git clone https://github.com/Brainwires/laufey && cd laufey
git checkout <LAUFEY_SHA> && make webview && make cef
DENORT_DESKTOP_BIN=$PWD/../deno/target/release/libdenort.dylib \
LAUFEY_DEV_DIR=$PWD deno desktop --backend webview main.ts
```

Upstream Deno's own workflows are removed on the `denext/*` and `rel/*` branches
and disabled in this fork's Actions settings. Two workflows run instead:

- `denext_runtime.yml` (above): the release build, the launch smoke and the e2e
  suite (see Test coverage). Every push to a `rel/**` branch, nightly on the
  default branch, manual dispatch, or a pushed `denext-runtime-v*` tag (which
  publishes only when `denext_tests.yml` passed on the tagged commit).
- `denext_tests.yml` (with the per-OS `denext_tests_os.yml`): upstream Deno's
  own test bar over the fork, the way `v2.9.7`'s `ci.generated.yml` ran it, on
  macOS, Windows and Linux: `tools/lint.js` (workspace clippy with upstream's
  deny flags, dlint, copyright and hygiene checks; the fork's copy knows about
  this file and the removed generated workflows), `tools/format.js --check`,
  `jsdoc_checker.js`, `cargo fmt --check`, `cargo test --locked --lib` over
  upstream's crate list plus the fork's crates, a debug build, and the unit and
  (sharded) spec suites against it; on Linux also upstream's `integration`
  (sharded) and `unit_node` suites; and the build and lib tests on macOS x64 and
  Linux arm64. It runs on pushes to `denext/**`, `rel/**`, `ci/**`, `feat/**`
  and `fix/**`, on pull requests to `denext/**` and `rel/**`, and by manual
  dispatch.

## Test coverage

Specs this fork adds for what its runtime exposes outside a desktop app:

- `tests/specs/run/desktop_ops_inert`: every `Deno.desktop` op and native class
  in `NOT_IMPORTED_OPS` (reachable from any code through
  `Deno[Deno.internal].core.ops`) called in a plain `deno run` with no
  permissions: each answers "not supported", an empty value or a refusal, opens
  nothing, writes nothing, and the classes throw `NotSupported`;
  `runOnMainThread`'s op needs `--allow-ffi` first. The spec fails on an exposed
  op it has no case for, and a Rust test fails when `NOT_IMPORTED_OPS` names a
  desktop op the spec lacks.
- `tests/specs/serve/memory_address`: `DENO_SERVE_ADDRESS=memory:<name>` serves
  on the in-process memory transport (no socket, no net permission), and a TCP
  server refuses a request target that claims `http+memory://` (400).
- `tests/specs/check/desktop_types`: the fork's `Deno.desktop` types check with
  `--desktop`, and misuses are type errors.
- `tests/specs/run/desktop_ops_inert` (`sys.js`): the integrations that need
  `--allow-sys` refuse without it (and with only some sys names) and are inert
  with it.

Upstream Deno has no end-to-end harness for `deno desktop` apps (its own tests
stop at unit tests and `tests/specs` of the CLI), so this fork carries its own,
the equivalent of laufey's `native_e2e`: `.github/denext-runtime/e2e/`. The
runtime workflow runs it after the launch smoke on all five targets with both
backends (Linux under Xvfb with the xfwm4 window manager, a private D-Bus
session and a stand-in notification server, `linux/`).

Layout:

| Path                  | What                                                                                                                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `e2e.sh`              | Unpacks the archive under test, sets up the Linux session, runs `run.ts` with the stock `deno` (2.9.7).                                                                                                       |
| `run.ts`              | Runs every area (`E2E_AREAS` / the workflow's `e2e_areas` input narrows it), writes `e2e-<target>-<backend>.json` and the step summary, exits 1 on any failure.                                               |
| `lib/runner.ts`       | Packages an app with the stock `deno desktop` against `DENORT_DESKTOP_BIN` / `LAUFEY_DEV_DIR`, writes `.deno-desktop/app.json` and `laufey-launch.json` as denext's packager does, launches, collects, kills. |
| `apps/<area>/main.ts` | One small app per area; `apps/_shared` has the reporting (`e2e.ts`) and real input (`input.ts`: XTEST / `keybd_event` / CoreGraphics key presses, the window manager's close).                                |
| `areas/<area>.ts`     | The runner side of each area: launches the OS would make, and checks only the outside can make (state across launches, the registry / XDG / LaunchServices, processes that must not start).                   |

Each check is `pass`, `fail` or `n/a`; an `n/a` carries the reason and is listed
in the job summary, never skipped silently. Areas:

- **origin** — page origin and secure context, `request.url` / `remoteAddr` of
  the memory transport, incremental streaming, `Origin` on a cross-origin fetch,
  the page's WebSocket through the relay, a response the page aborts cancelled
  in the app, a stream the page stops reading held back; the relay refusing
  foreign / missing / duplicate / upper-cased origins and a missing or wrong
  relay token (403) and plain HTTP (400) and admitting the exact origin with the
  token (101); the cross-origin marker on an opaque frame's POST; bindings
  refusing a page at another origin unless opted in; a TCP `Deno.serve` refusing
  `http+memory:` request targets (400); the env overlay reaching a child process
  (also through `node:child_process`), without the in-process
  `DENO_SERVE_ADDRESS` or the relay URL; a forked module the app ships answering
  over IPC, a script outside the app refused, and `<App> run x.js` from outside
  exiting without running it; and a second app whose server is `node:http`
  (`originnode`): the page, a POST round trip and the memory socket's
  `remoteAddress`.
- **appid** — localStorage and IndexedDB persisting across launches of one
  identifier and not visible to another identifier at the same origin.
- **deeplink** — cold argv links and files, second-instance forwarding (no
  second runtime), startup registration (HKCU, XDG in a throwaway
  `XDG_DATA_HOME`/`XDG_CONFIG_HOME`, LaunchServices), OS-routed links cold and
  warm (`Start-Process`, `xdg-open`, `open`), `open -a <app> <file>` on macOS,
  another owner left alone until `force`; on Windows the stock CLI's `.msi`
  installed with `msiexec`, registering from Program Files and receiving links
  cold and warm, then uninstalled.
- **window** — the window API, state events, limits, placement, chrome vs
  `windowCapabilities()`, a user close canceled and a close the runtime never
  answers, `quit()`, the tray rule, `initialWindow` from `app.json`; a frameless
  panel created first is a new window and the next plain one adopts the initial
  window; a string with a NUL is a `TypeError` (`setTitle`, `navigate`,
  `clipboard.writeText`); workers have no native classes.
- **dnd** — the clipboard in every format and its change event, file dialogs
  opened and aborted (open / save, modal / app-level), busy and argument
  refusals, `startDrag` refusals.
- **passkeys** — argument refusals, every refusal envelope, one ceremony at a
  time, the slot freed after an OS refusal, no passkeys in workers.
- **sld** — global shortcuts with a real key press, launch at login against the
  OS's record (autostart entry, `HKCU\...\Run`), DevTools on (env override) and
  off (`"inspectable": false`).
- **menus** — application-menu accelerators from the keyboard, context menus
  dismissed and chosen from the keyboard; notifications shown, clicked, acted on
  and dismissed (Linux: the stand-in server; Windows: the app's toast activator,
  `windows/toast-click.ps1`), scheduled / listed / cancelled, a scheduled one's
  click as `notificationresponse`, and on Windows the cold-start click (COM
  starts the app).
- **asmt** — `runOnMainThread` proven on the UI thread (the thread that owns the
  windows, by the OS's own thread id) and not the JavaScript thread, context /
  return values, `UnsafeFnPointer` / `UnsafeCallback`, 50 calls at once,
  argument refusals, `--allow-ffi`; `authSession`: argument refusals, on macOS a
  real ephemeral `ASWebAuthenticationSession` against a loopback identity
  provider ending at the callback, `busy`, and the anchor window closing
  (`cancelled`), the app's `cancel()` (`cancelled` once, then `false`, and the
  next session completes); `cancel()` with nothing running is `false` on every
  OS; `not_supported` on Windows and Linux; neither in workers.
- **update** — full-app self-update with throwaway keys and a throwaway TLS CA:
  hostile manifests and archives refused with their codes and the install
  untouched (including a replayed `sequence`, a missing `sequence` or
  `expiresAt`, an expired `expiresAt`, and another version's archive under a
  manifest's version), the accepted sequence recorded, an unwritable install,
  1.0.0 -> 2.0.0 relaunch and confirm, a 3.0.0 trial that never confirms rolled
  back and refused (and kept in the rejected set).
- **Node-API** stays in `launch.sh`.

What a hosted runner cannot do, reported `n/a` with the reason:

| Check                                               | Why                                                                                                                    |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| A real file drop / completed drag-out               | needs an OS drag session driven by a pointer                                                                           |
| A completed passkey ceremony                        | needs a person (Touch ID / Windows Hello) and, on macOS, an associated-domains entitlement in a Developer ID signature |
| A staged update accepted by signer match            | needs a real Developer ID / Authenticode identity                                                                      |
| Notification clicks and dismissals on macOS         | only a person clicking a banner produces one for an unsigned app                                                       |
| A notification click on Linux after the app exited  | freedesktop servers send the click to the posting process only (`coldStart: false`)                                    |
| A dismissed toast on Windows, a live toast's click  | the hosted session retires toasts within ~2 s; the click is then checked as a `notificationresponse`                   |
| Key presses on macOS without an Accessibility grant | macOS refuses synthesized events (checked with `AXIsProcessTrusted`)                                                   |
| A real OS auth session on Windows / Linux           | they have no OS auth session (`not_supported` is checked)                                                              |
| `.msi` on macOS / Linux                             | the stock CLI builds `.msi` only for Windows                                                                           |
| An unwritable install on Windows                    | the runner is an administrator                                                                                         |

The suite's first full run found four failures in laufey's hosts (not in this
fork's runtime), all fixed in laufey `denext/integration` at API 43 and now
checked on every leg:

- origin, CEF on every OS: the page's WebSocket through the relay and a
  cross-origin fetch to loopback were held by Chromium's Local Network Access
  checks (a custom-scheme page is a "public" origin). The CEF hosts grant
  local-network access to the app's own registered custom-scheme origins only;
  the checks stay on for everything else.
- menus, CEF on Windows: the runtime's event loop stopped while
  `TrackPopupMenu`'s modal loop ran on CEF's UI thread (task starvation).
- menus, WebView2: keyboard selection in a context menu (the owner window is
  made foreground first, KB135788).
- dnd, Windows: an aborted file dialog did not always close, so its promise
  never settled.
