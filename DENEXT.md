# Brainwires/deno: denext's Deno Desktop runtime fork

This is a fork of [denoland/deno](https://github.com/denoland/deno) that
exists for one reason: to ship a patched **Deno Desktop runtime** (the
`libdenort` library `deno desktop` embeds an app into, plus
[laufey](https://github.com/littledivy/laufey)'s backend hosts) for
[denext](https://github.com/Brainwires/denext) 3.1 without waiting for the
patches to land upstream.

It is not a general-purpose Deno distribution. The `deno` CLI is **not**
rebuilt: denext runs the stock `deno desktop` CLI (2.9.7) and points it at
the prebuilt runtime from this fork's releases:

```sh
DENORT_DESKTOP_BIN=<unpacked archive>/libdenort.dylib   # .so on Linux, denort.dll on Windows
LAUFEY_DEV_DIR=<unpacked archive>/laufey
deno desktop --backend webview|cef ...
```

## Exit criterion

This fork goes away when **stock Deno ships these features**: once a Deno
release carries the in-process memory transport, the configured app origin,
and the per-app identifier handed to every laufey backend (and laufey
releases the matching backend changes), denext uses the stock runtime and
this fork is archived.

## Branches and tags

| Ref | What it is |
| --- | --- |
| `denext/v<deno version>` (e.g. `denext/v2.9.7`) | The branch the runtime is built from: upstream tag `v<deno version>` + the feature commits + this file and the release workflow. The default branch of this fork. |
| `feat/desktop-app-origin`, `feat/desktop-app-id` | The feature branches the `denext/*` branch is built from (`feat/desktop-app-id` contains `feat/desktop-app-origin`). Kept for review and for rebasing onto a newer Deno. |
| `denext-runtime-v<deno version>-denext.<n>` (e.g. `denext-runtime-v2.9.7-denext.1`) | A published runtime. Pushing such a tag on the `denext/*` branch runs the workflow and creates the GitHub Release. `<n>` counts runtime releases on one Deno version. |

`main` and the other upstream branches are untouched mirrors from the fork
point. Nothing here is proposed back from this fork; the upstream PRs below
are where the changes are discussed.

## What is patched (on top of `v2.9.7`)

1. **`feat(net,http)`: in-process memory transport for `Deno.serve`** —
   `DENO_SERVE_ADDRESS=memory:<name>`. Ported from
   [denoland/deno#35675](https://github.com/denoland/deno/pull/35675).
2. **`feat(desktop)`: serve the app at a stable, configured origin** —
   `desktop.app.origin` (`<scheme>://<host>`), a custom-scheme handler that
   bridges webview requests into `Deno.serve` over the memory transport, a
   WebSocket relay that checks the `Origin` header.
3. **`feat(desktop)`: read the app origin from an embedded
   `.deno-desktop/app.json`** — so a stock CLI that rejects
   `desktop.app.origin` in `deno.json` can still configure it:
   `{ "origin": "<scheme>://<host>", "identifier": "<reverse-DNS id>" }`,
   embedded with `"compile": { "include": [".deno-desktop/app.json"] }`.
4. **`fix(desktop)`: set the Linux app_id so Wayland shows the configured
   icon** — a port of [denoland/deno#35662](https://github.com/denoland/deno/pull/35662)
   by Leo Kettmeir ([@crowlKats](https://github.com/crowlKats)), authored by
   him; squashed and re-applied onto `v2.9.7`.
5. **`feat(desktop)`: hand every laufey backend the app identifier** —
   `LAUFEY_APP_ID`, so each app's web storage (localStorage, IndexedDB,
   cookies) lives in its own directory and persists across launches.
6. **`feat(desktop)`: deep links, opened files and second instances in
   `Deno.desktop`** — `openurl` / `openfile` / `secondinstance` events plus
   `Deno.desktop.launchUrls` / `launchFiles`, `desktop.app.singleInstance`, and
   a `laufey-launch.json` in every package. Built on laufey's `on_open_url`
   ([littledivy/laufey#77](https://github.com/littledivy/laufey/pull/77) by
   [@diegoholiveira](https://github.com/diegoholiveira)) and `on_second_instance`.
7. **`fix(http)`: an absolute-form request target can't claim `http+memory:`
   over TCP** (400), and **`feat(os)`: an env overlay** so the runtime never
   calls `setenv` while the host's UI thread runs.
8. **`feat(desktop)`: register deep-link schemes with the OS and report their
   owner** — `Deno.desktop.getSchemeOwner` / `registerScheme({ force })`, a
   first-launch registration that never takes a scheme another app owns
   (Windows HKCU, macOS LaunchServices, Linux XDG), and the Windows `.msi`
   writing the same keys at install.
9. **`feat(desktop)`: native passkeys** — `Deno.desktop.passkeys`
   (`capabilities` / `create` / `get`) on macOS AuthenticationServices and
   Windows `webauthn.dll`, with the request/response envelope of
   `@clerk/electron-passkeys`.
10. **`feat(desktop)`: window state, size limits, screens, chrome, cancelable
    close and quit** — `BrowserWindow` maximize / minimize / fullscreen with
    events, min / max size, `getBounds` / `setBounds` / `getNormalBounds`,
    title bar styles, traffic-light position, Mica / Acrylic and vibrancy;
    `Deno.desktop.screens()`, `"displaychanged"`, `windowCapabilities()`,
    `quit()` (cancelable, Electron's `app.quit()`) and
    `quitOnLastWindowClosed`; a cancelable `close` event (5 s answer
    timeout); the tray-only and off-screen placement fixes. On top of
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
    napi-rs / neon `GetProcAddress(GetModuleHandle(NULL))`), which in a
    desktop app is laufey's host and exports nothing, so the first Node-API
    call killed the process (`0xC06D007F`). The runtime now gives the
    executable an in-memory export table whose Node-API entries jump to the
    DLL's functions (`cli/rt_desktop/napi_host_exports.rs`). macOS and Linux
    already resolved them (the dylib's exports promoted to `RTLD_GLOBAL`,
    with denoland/deno#36718's Linux flag fix). The runtime smoke test loads
    a probe addon (`.github/denext-runtime/napi-probe`) on every target.

12. **`feat(desktop)`: drag and drop, native file dialogs and the rich
    clipboard** — `BrowserWindow` `dragenter` / `dragover` / `dragleave` /
    `drop` with native paths and `startDrag({ files, icon })`;
    `Deno.desktop.dialog.showOpenDialog` / `showSaveDialog` (the OS's own
    dialogs, AbortSignal-cancellable, never blocking the runtime);
    `Deno.desktop.clipboard` (text, HTML, PNG images, `availableFormats()`,
    a `"change"` event); new `windowCapabilities()` keys (laufey API 39).

13. **`feat(desktop)`: global shortcuts, launch at login and DevTools
    control** — `Deno.desktop.shortcuts` (`register(accelerator,
    callback?)` resolving with the canonical accelerator, `unregister`,
    `unregisterAll`, `isRegistered`, `list`, `canonicalize`, a `"shortcut"`
    event; errors carry a `code`: `invalid`, `conflict`,
    `already_registered`, `not_supported`, `denied`);
    `Deno.desktop.launchAtLogin` (`get()` / `set(enabled)`:
    `enabled` / `disabled` / `requires-approval` / `not-supported`);
    `Deno.desktop.devtools` (`enabled`, `open` / `close` / `toggle` /
    `isOpen`) and `BrowserWindow.closeDevtools()` / `toggleDevtools()` /
    `isDevtoolsOpen()` / `isDevtoolsEnabled()`; `openDevtools()` does
    nothing when the app launched with DevTools off (`LAUFEY_INSPECTABLE=0`
    / `"inspectable": false`), in dev mode too (laufey API 40).

14. **`feat(desktop)`: menu accelerators and close events, scheduled and
    actionable notifications** — menu `accelerator`s work on every backend
    (`Deno.desktop.menuCapabilities()` says what each supports);
    `showContextMenu` returns a promise of the chosen id (`null` when
    dismissed) and fires `contextmenuclose`. `Notification` takes
    `actions` and `data`, fires a separate `action` event (actions are no
    longer folded into `click`) and reports `Notification.maxActions`;
    `Deno.desktop.notifications` adds `schedule({ at, ... })`,
    `getScheduled()`, `cancel(tag)`, `capabilities()` and
    `requestPermission({ provisional })`. A click on a notification of an
    earlier run, or the one that launched the app, is a
    `notificationresponse` event, and `Deno.desktop.launchNotificationResponses`
    holds the ones that arrived before the app listened (laufey API 41).

15. **`feat(desktop)`: OS auth sessions and native code on the UI thread** —
    `Deno.desktop.authSession` (`capabilities()`, `start({ url,
    callbackScheme | callbackUrl, ephemeral, window })` resolving with the
    callback URL) runs `ASWebAuthenticationSession` on macOS, with a real
    `cancelled` when the user closes the sheet; Windows and Linux have no OS
    equivalent and reject with code `not_supported` (RFC 8252: the system
    browser). `Deno.desktop.runOnMainThread(fn, context)` calls a native
    `void* (*)(void*)` (an `UnsafeFnPointer`, `UnsafeCallback` or pointer) on
    the UI thread and resolves with its return value as a bigint; it needs
    `--allow-ffi` and rejects instead of hanging once the app is quitting.
    Both are main-scope only (laufey API 42, which also stops WebKitGTK's
    custom-scheme writes from blocking the event loop).

The runtime-side parts (1-3, 5-15) are what the prebuilt `libdenort` carries.
The CLI-side parts (for example writing `LAUFEY_CUSTOM_SCHEMES` /
`LAUFEY_APP_ID` into a packaged app's launchers) are in the branch too, but a
stock CLI does not run them; denext's own launcher provides that environment.

The laufey backend hosts are built from
[Brainwires/laufey](https://github.com/Brainwires/laufey) branch
`denext/integration` (registered schemes, app data dir, WebKitGTK scheme
request bodies, launch config, open-url / single instance, passkeys, the
window API with littledivy/laufey#80 and #81, drag and drop / file dialogs /
rich clipboard, global shortcuts / launch at login / DevTools control, menu
accelerators / notifications, UI-thread tasks / auth sessions /
non-blocking WebKitGTK scheme bodies, Windows bindgen fix), at the commit pinned by
`LAUFEY_SHA` in the workflow (or the `laufey_ref` input).

laufey's `init_api` rejects any C ABI version mismatch between the runtime
and the host, so the `laufey` crate libdenort links and the hosts are built
from the **same** laufey revision: the workflow patches whatever source
`Cargo.lock` names for `laufey` (crates.io, or a git rev of
Brainwires/laufey) with a path to that revision's `capi/`, records the
`LAUFEY_API_VERSION` it compiled, and the package job fails if it differs
from the hosts' `capi/include/laufey.h`.

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
directory holding the backend binary into the packaged app, so those
directories hold only the backend's runtime files.

Each release also has `SHA256SUMS` and `manifest.json`
(target -> backend -> url, sha256, size; plus the deno SHA, laufey SHA and
CEF version). Every archive has a build provenance attestation:

```sh
gh attestation verify deno-desktop-runtime-<...>.tar.gz -R Brainwires/deno
```

Binaries are not code-signed or notarized: an app is signed with its
author's identity when it is packaged. The macOS `libdenort.dylib` is
ad-hoc signed after stripping, because arm64 macOS will not load unsigned
code.

## Reproducing a build

The workflow is `.github/workflows/denext_runtime.yml` (helpers in
`.github/denext-runtime/`). Run it from the Actions tab (`workflow_dispatch`,
optional `targets` filter, `laufey_ref` override, and `reuse_*_run_id` to
reuse a previous run's libdenort or laufey build), or push a
`denext-runtime-v*` tag to publish. Per target it:

1. builds `libdenort` with `cargo build --release --locked -p denort_desktop`
   (the release profile: fat LTO, `codegen-units = 1`), on the runner of that
   target (Intel macOS on `macos-15-intel`, Linux on `ubuntu-22.04` /
   `ubuntu-22.04-arm` so the glibc baseline matches laufey's WebKitGTK 4.1
   requirement of Ubuntu 22.04+);
2. builds laufey's webview and CEF hosts with laufey's `make webview` /
   `make cef` (the CEF minimal distribution is downloaded by the Makefile);
3. assembles the archives, checks architectures and dynamic dependencies
   (`file` / `lipo` / `otool -L` / `ldd`), packages a small `Deno.serve` app
   with the stock `deno desktop` 2.9.7 for both backends, and launches it
   (Linux under Xvfb): the page, served from `t3code://app`, POSTs back to
   `Deno.serve` and the app exits. A launch failure fails the job;
4. attests the archives and, on a tag, publishes the release.

Locally, the equivalent is:

```sh
cargo build --release -p denort_desktop                   # this branch
git clone https://github.com/Brainwires/laufey && cd laufey
git checkout <LAUFEY_SHA> && make webview && make cef
DENORT_DESKTOP_BIN=$PWD/../deno/target/release/libdenort.dylib \
LAUFEY_DEV_DIR=$PWD deno desktop --backend webview main.ts
```

Upstream Deno's own workflows are removed on the `denext/*` branch and
disabled in this fork's Actions settings, so pushes and tags here only run
the denext runtime workflow.
