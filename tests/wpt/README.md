# Web Platform Tests (WPT)

Deno uses a custom test runner for Web Platform Tests. It can be found at
`./tests/wpt/wpt.ts`, relative to the root of this codebase.

## Setup

Before attempting to run WPT tests for the first time, run the setup command.
You must also run this command every time the `./tests/wpt/suite` submodule is
updated:

```shell
./tests/wpt/wpt.ts setup
```

This will:

- Check that Python 3.11 or newer is available (required by the WPT test server)
- Update the WPT manifest (`./tests/wpt/runner/manifest.json`)
- Configure `/etc/hosts` with entries required by the WPT test server (skipped
  with `--no-hosts`)

You can specify the following flags:

- `--rebuild` — Rebuild the manifest from scratch instead of incrementally
  updating. This can take up to 3 minutes.
- `--auto-config` — Automatically configure `/etc/hosts` without prompting.
- `--no-hosts` — Leave `/etc/hosts` alone; run with `--no-hosts` too (see
  [Running without `/etc/hosts`](#running-without-etchosts)).

## Running tests

To run all web platform tests, use the `--all` flag:

```shell
./tests/wpt/wpt.ts run --all
```

To run a specific subset, specify filters after `--`:

```shell
# Run all tests in a suite
./tests/wpt/wpt.ts run -- fetch

# Run tests in a subdirectory
./tests/wpt/wpt.ts run -- streams/piping/general

# Run a single test file
./tests/wpt/wpt.ts run -- /WebCryptoAPI/getRandomValues.any.html

# Run multiple filters
./tests/wpt/wpt.ts run -- hr-time fetch/api/basic
```

Running `wpt.ts run` with neither `--all` nor filters will print usage help.

Filters can start with `/` (absolute path match) or without (prefix match
without the leading `/`).

Tests are run in parallel across CPU cores, partitioned by top-level directory.

### Flags

- `--all` — Run all tests (required if no filters are specified)
- `--release` — Use `./target/release/deno` instead of `./target/debug/deno`
- `--binary=<path>` — Use a specific Deno binary (skips `cargo build`)
- `--quiet` — Only print failing test cases
- `--json=<file>` — Write test results as JSON
- `--wptreport=<file>` — Write results in the
  [wptreport](https://github.com/nicedoc/wpt-report) format
- `--inspect-brk` — Attach the V8 inspector to each test
- `--no-ignore` — Run tests marked with `{"ignore": true}` in expectations
- `--exit-zero` — Exit with code 0 even if there are failures
- `--no-document` — Run window-scoped tests without the runner's document
  environment (tier T0, see
  [The document environment](#the-document-environment)); it is on by default
  for the WebRTC and Media Capture suites
- `--document` — Give every window-scoped test the document environment
  (`--dom-shim` is its old name)
- `--no-hosts` — Reach the WPT host names through a proxy the runner starts,
  instead of `/etc/hosts` entries

A test file that runs out of time is reported as `TIMEOUT` in `--wptreport` (not
`CRASH`), and testharness.js's own harness and subtest statuses (`TIMEOUT`,
`NOTRUN`, `PRECONDITION_FAILED`) keep their names there.

### Running without `/etc/hosts`

The WPT server is reached by the names `web-platform.test`,
`not-web-platform.test` and their subdomains. `--no-hosts` (on `setup`, `run`
and `update`) needs no `/etc/hosts` entries and no root: the server binds to
127.0.0.1 (`runner/config.no-hosts.json`), and the runner starts a small
forwarding proxy on a loopback port (`runner/proxy.ts`: `CONNECT` tunnels and
absolute-form HTTP requests) that sends those names to 127.0.0.1. The test
processes get it as `HTTP_PROXY` / `HTTPS_PROXY`, which `fetch()` and the module
loader (workers) honour.

```shell
./tests/wpt/wpt.ts setup --no-hosts
./tests/wpt/wpt.ts run --no-hosts -- webrtc
```

## Updating expectations

The `update` command runs tests and overwrites the expectation files to match
current results:

```shell
# Update all expectations
./tests/wpt/wpt.ts update --all

# Update expectations for specific suites
./tests/wpt/wpt.ts update -- hr-time fetch
```

Running `wpt.ts run` immediately after `wpt.ts update` should always pass.

The `update` command accepts the same flags as `run` (`--release`, `--binary`,
`--quiet`, `--json`, `--no-ignore`, `--inspect-brk`).

## Expectation file format

The expectations directory (`./tests/wpt/runner/expectations/`) contains one
JSON file per test suite (e.g., `fetch.json`, `dom.json`, `WebCryptoAPI.json`).
Each file is a nested JSON object that mirrors the WPT directory structure,
following the directory tree down to individual test files.

Leaf values describe what is expected for each test file:

| Value                                      | Meaning                                                               |
| ------------------------------------------ | --------------------------------------------------------------------- |
| `true`                                     | All subtests are expected to pass                                     |
| `false`                                    | The entire test file is expected to fail (crash, harness error, etc.) |
| `{"expectedFailures": ["name1", "name2"]}` | These specific subtests are expected to fail; all others should pass  |
| `{"ignore": true}`                         | Skip this test entirely (override with `--no-ignore`)                 |

Example:

```jsonc
{
  "fetch": {
    "api": {
      "basic": {
        "accept-header.any.html": true, // all subtests pass
        "stream-response.any.html": false, // entire file fails
        "request-headers.any.html": { // these 2 subtests fail
          "expectedFailures": [
            "Fetch with PUT with body",
            "Fetch with POST with body"
          ]
        },
        "mode-no-cors.sub.any.html": { // skipped
          "ignore": true
        }
      }
    }
  }
}
```

A suite can be tracked without gating (`NON_GATING_SUITES` in `runner/utils.ts`;
today `mediacapture-streams`): its failures are listed after the run but never
fail it, and a test file its expectation file doesn't list yet runs as if
expected to pass, so `update` adds it.

When the `run` command finishes, it shows a git diff between the current
expectation files and what the actual results would produce. This makes it easy
to see regressions and improvements.

## Test files the runner takes

Besides the `.any.js` / `.window.js` / `.worker.js` tests (whose HTML wrapper
the WPT server generates), a suite's own `testharness.js` `.html` files run too:
the runner fetches the page from the WPT server and runs its `<script src>` and
inline `<script>` blocks in document order, as one program, with
`globalThis.window` set, the runner's `testharnessreport.js` in place of the
browser one and the runner's `testdriver-vendor.js` in place of the suite's (see
below). Without the document environment there is no DOM: a test that needs
`document` fails, and its expectation says so.

### The document environment

A WPT `.html` test is defined as a document, and loading that document is the
test runner's job for every product (wptrunner does it for browsers through
WebDriver). `document`, `document.body`, `createElement` and `test_driver` are
test plumbing, not APIs of the specs the tests are about. So the runner supplies
them; Deno does not grow a `document` to pass them.

Results are published in tiers:

| Tier | What the test gets                                                                                                           |
| ---- | ---------------------------------------------------------------------------------------------------------------------------- |
| T0   | The bare runtime, no document (`--no-document`)                                                                              |
| T1   | T0 + the runner's document environment + its `testdriver-vendor.js`: the default for the WebRTC and Media Capture suites     |
| T1m  | The part of T1 that passes only through the runner's media elements (`<video>` / `<audio>` driven by frames really received) |
| T2   | Needs real encoding and decoding (canvas capture, pixel checks): not provided                                                |

The environment is given to window-scoped tests (a suite's own `.html` files and
`.window.js` tests; `.any.js` and `.worker.js` tests are written for any global
or a worker, so they never get it). By default only the suites in
`DOCUMENT_SUITES` (`runner/utils.ts`: the `webrtc*` directories and
`mediacapture-streams`) get it: a suite about the DOM or HTML itself would
otherwise be testing the environment. `--document` gives it to every
window-scoped test, `--no-document` to none.

Honesty rules for the environment:

1. The upstream test files are never edited, skipped or special-cased by name.
2. The environment implements no interface of the specs under test (webrtc-pc,
   webrtc-stats, webrtc-encoded-transform, webrtc-extensions, webrtc-identity,
   webrtc-priority, webrtc-svc, webrtc-ice, mediacapture-main). Those live in
   the runtime.
3. An environment object reports only what the runtime did. A `<video>` fires
   `loadedmetadata` only after a real frame arrived on its track, and
   `videoWidth` is the size coded in that frame. Nothing is answered from a
   constant.
4. A feature the environment lacks stays a failure.
   `HTMLCanvasElement.getContext()` returns `null` and there is no
   `captureStream()`.
5. Reports list T0, T1 and T1m, and this feature list.

What the environment has (`runner/dom_shim.js`, installed right after
`testharness.js` has loaded so testharness keeps its shell environment, then
`runner/media_env.js` when present):

- `document` with `documentElement`, `head`, `body`, `title` (the page's
  `<title>`), `currentScript` (the running `<script>` element, `null` between
  scripts), `readyState` (`"complete"`), `createElement`, `createTextNode`,
  `getElementById` and `getElementsByTagName`;
- `Node` / `Element` with `appendChild`, `insertBefore`, `removeChild`,
  `append`, `remove`, `contains`, attributes and `textContent` (`innerHTML` is a
  plain property: nothing parses it);
- the test file's own markup: the elements, attributes and text of the page are
  instantiated as an HTML parser would have, the nodes before each `<script>`
  before that script runs and the rest after the last one, so
  `getElementById("log")` finds the page's `<div id="log">`;
- named access on the window (HTML 7.2.2.3): a connected element's `id` is a
  global naming that element (`video1` for `<video id="video1">`), unless the
  global object already has that property;
- `HTMLCanvasElement`, which exists but draws nothing: `getContext()` returns
  `null` and there is no `captureStream()`, so a helper that feature-detects
  canvas capture falls back to `getUserMedia()` (the fake devices);
- the element interfaces of `media_env.js` (`<video>`, `<audio>` over the
  runtime's received frames) when that file is present; every other tag outside
  a small table (`html`, `head`, `body`, `title`, `div`, `span`, `p`, `pre`,
  `script`, `canvas`) is an `HTMLUnknownElement`: there is no `<iframe>` or
  `<img>`;
- `test_driver.set_permission()` for `camera` and `microphone`
  (`runner/testdriver-vendor.js`): `"granted"` resolves, since the runtime
  grants `getUserMedia()` under the runner's `DENO_FAKE_MEDIA_DEVICES=1`; any
  other state goes to the runtime's `Deno.webrtc.setMediaPermission()` when it
  has one and rejects otherwise. Every other `test_driver` call rejects with
  testdriver.js's "... is not implemented by testdriver-vendor.js", at once
  (`in_automation`), including `click` and `bless`.

The environment never dispatches an event (no `load`, no `DOMContentLoaded`),
lays anything out or fakes an API under test: a test either reaches the real
implementation or still fails. Not available: iframes and other browsing
contexts, navigation, service workers (their files are skipped by path),
`crossOriginIsolated`, canvas drawing, `AudioContext`.

### WebRTC (`webrtc`, `webrtc-extensions`, `webrtc-stats`)

These open real peer connections over loopback. In a container or a machine
whose other interfaces shouldn't be used, run them with
`DENO_WEBRTC_LOOPBACK_ONLY=1` (host candidates on `127.0.0.1` only):

```shell
DENO_WEBRTC_LOOPBACK_ONLY=1 ./tests/wpt/wpt.ts run -- webrtc webrtc-extensions webrtc-stats
```

The runner sets `DENO_FAKE_MEDIA_DEVICES=1` for every test (as wptrunner runs
browsers with `--use-fake-device-for-media-stream`), so `getUserMedia()` gives
the fake camera and microphone, whose tracks send real media. Most of the files
that still fail on media do so in the suite's own helpers: `getNoiseStream()`'s
`canCreate()` reads `HTMLCanvasElement.prototype` before it falls back to
`getUserMedia()`, and `resources/testdriver.js` reads `document` when it loads;
both are `ReferenceError`s in a runtime without a DOM. The document environment
gets both, and the helpers' scratch elements, past that point.

## FAQ

### Upgrading the WPT submodule

```shell
cd tests/wpt/suite
git fetch origin
git checkout origin/epochs/daily
cd ../../../
git add ./tests/wpt/suite
```

All contributors will need to rerun `./tests/wpt/wpt.ts setup` after this.

Since upgrading WPT usually requires updating the expectations to cover upstream
changes, it's best to do that as a separate PR rather than as part of a PR that
implements a fix or feature.
