# Brainwires/deno: denext's Deno

This is a fork of [denoland/deno](https://github.com/denoland/deno) that we
maintain as our own Deno (decided 2026-10-11): it follows Deno releases on our
own schedule and is not waiting to be retired in favor of stock Deno. It carries
two kinds of changes, for [denext](https://github.com/Brainwires/denext) and its
users:

- **The Deno Desktop runtime**: the `libdenort` library `deno desktop` embeds an
  app into, plus [laufey](https://github.com/littledivy/laufey)'s backend hosts
  (items 1-22 below, plus the `deno_core` fix in item 25). It is published as
  prebuilt archives that also work with the stock `deno desktop` CLI (2.9.7):

  ```sh
  DENORT_DESKTOP_BIN=<unpacked archive>/libdenort.dylib   # .so on Linux, denort.dll on Windows
  LAUFEY_DEV_DIR=<unpacked archive>/laufey
  deno desktop --backend webview|cef ...
  ```

- **Features of the `deno` CLI and runtime itself**, such as Vitest / Jest
  compatibility for `deno test` (item 23) and the built-in SQL client
  (`Deno.SQL` / `Deno.sql`, item 24). These need this fork's own `deno` binary,
  published with each runtime release since `denext.15` (`deno-cli-*`, see
  [Release archives](#release-archives) and item 26) or built from a release
  line (see [Reproducing a build](#reproducing-a-build)); the stock CLI does not
  have them.

## Upstream

Changes that would help everyone can still be offered to denoland/deno, but
nothing here depends on that: the fork keeps every change whether or not
upstream takes it, and is rebased onto a newer Deno when we choose to move.

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
    the file, instead of being dropped. Loading a `vitest.config.*` / Jest
    config, automocking and `__mocks__` lookups still read files, so need
    `--allow-read`.

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
    `EmitCache::get_emit_code_any_source_hash`), accepted only when its inline
    source map's sources content is the module's current source (or it maps
    nothing, as for a module of types) and its length without the source map is
    the length of the script V8 ran (its top-level function's range). A
    JavaScript user module, which the transform emits too, is matched the same
    way. Coverage ranges starting after the module's last mapping (the export
    setters the transform appends) are dropped like transformer helpers,
    `deno-vitest:` modules (the vendored runtime, the mock modules) are left out
    of reports, and a mocked module's coverage merges the files where it is
    real, `importOriginal` / `vi.importActual` included.

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
    also carries `deno-cli-<version>-<target>.zip` for the same five targets,
    built with `cargo build --release --locked --bin deno`, so denext can
    download the CLI features (items 23, 24, and the CLI-side parts of the rest)
    pinned by version and SHA-256 like the runtime, with nothing else to
    install. The binary is still `deno`. A release build bakes `DENEXT_VERSION`
    (the release version, e.g. `2.9.7-denext.15`) in at compile time: it becomes
    `Deno.version.deno`, `deno --version` / `deno -V`
    (`deno 2.9.7-denext.15 (stable, release, <target>)`) and the user agent
    (`Deno/2.9.7-denext.15`), still a semver on the stable channel, so a program
    (denext) can tell this CLI from a stock one. The `denort` / `libdenort`
    downloads of `deno compile` / `deno desktop` and `deno upgrade` keep the
    plain Deno version (`DENO_VERSION`, `2.9.7`). A value that does not extend
    the build's own Deno version with `-` or `+` is ignored; without it every
    version is upstream's (the `libdenort` build sets none). Each archive is
    smoke-tested on its own runner (the version lines and `Deno.version.deno`,
    `typeof Deno.SQL === "function"`, a Vitest suite passing under `deno test`)
    and attested. Test: `denext_version_extends_the_deno_version` in
    `cli/lib/version.rs`.

The runtime-side parts (1-3, 5-22, 24, 25) are what the prebuilt `libdenort`
carries. The CLI-side parts (for example writing `LAUFEY_CUSTOM_SCHEMES` /
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
bridge, a strict bridge JSON parser; and every host runs the runtime's own
helper and worker launches (`run …`, `NODE_CHANNEL_FD`, `NEXT_PRIVATE_WORKER`)
headless before its single-instance check), at the commit pinned by `LAUFEY_SHA`
in the workflow (or the `laufey_ref` input). The same commit is the `laufey` git
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

Since `denext.15` each release also has this fork's `deno` CLI (item 26), one
zip per target (every OS): `deno-cli-<deno version>-denext.<n>-<target>.zip`, no
wrapping directory:

```text
deno | deno.exe     the CLI; `Deno.version.deno` is `<deno version>-denext.<n>`
BUILD_INFO.json     deno version + SHA, run URL
licenses/           the deno license
```

Each release also has `SHA256SUMS` (every runtime and CLI archive) and
`manifest.json` (target -> backend -> url, sha256, size; the CLI archives under
`cli.targets` -> target -> binary, url, sha256, size; plus the deno SHA, laufey
SHA and CEF version). Every archive has a build provenance attestation:

```sh
gh attestation verify deno-desktop-runtime-<...>.tar.gz -R Brainwires/deno
gh attestation verify deno-cli-<...>.zip -R Brainwires/deno
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
4. attests the archives and, on a pushed tag, publishes the release;
5. in a parallel `cli` job, builds this fork's `deno` CLI with
   `cargo build --release --locked --bin deno` and `DENEXT_VERSION` set to the
   release version (`<deno version>-denext.0-ci.<run>` for untagged runs), packs
   it with `assemble_cli.sh`, smoke-tests the unpacked archive with
   `cli_smoke.sh` and attests it. The release needs every runtime archive and
   every CLI archive.

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
  read-only files) and Postgres client failures against stand-in servers
  (`sslmode=require` refused by a server without TLS, connect timeout, refused
  port, net and env permissions). The `pg_*` tests that need a real server
  (types both ways, SQLSTATE errors, transactions, the pool's limits and
  timeouts, TLS modes) run when `DENO_SQL_TEST_POSTGRES_URL` (and, for TLS,
  `DENO_SQL_TEST_POSTGRES_CA`) is set, and are skipped otherwise.
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
