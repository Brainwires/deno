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

- Check that Python 3.11 is available (required by the WPT test server)
- Update the WPT manifest (`./tests/wpt/runner/manifest.json`)
- Configure `/etc/hosts` with entries required by the WPT test server

You can specify the following flags:

- `--rebuild` — Rebuild the manifest from scratch instead of incrementally
  updating. This can take up to 3 minutes.
- `--auto-config` — Automatically configure `/etc/hosts` without prompting.

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
- `--dom-shim` — Give `.html` tests a small, layout-free `document` (see
  [The `--dom-shim` document](#the---dom-shim-document))

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

When the `run` command finishes, it shows a git diff between the current
expectation files and what the actual results would produce. This makes it easy
to see regressions and improvements.

## Test files the runner takes

Besides the `.any.js` / `.window.js` / `.worker.js` tests (whose HTML wrapper
the WPT server generates), a suite's own `testharness.js` `.html` files run too:
the runner fetches the page from the WPT server and runs its `<script src>` and
inline `<script>` blocks in document order, as one program, with
`globalThis.window` set and the runner's `testharnessreport.js` in place of the
browser one. There is no DOM: a test that needs `document`, an `<iframe>`, a
`<video>` / `<canvas>` element or a browsing context fails, and its expectation
says so (`false`, or its subtests under `expectedFailures`).

### The `--dom-shim` document

Many `.html` tests touch the page only for bookkeeping: a helper reads
`document.currentScript`, appends a scratch element to `document.body`, or
checks whether an element interface exists before taking another path. Without a
DOM each of those is a `ReferenceError` before the test reaches the API it is
about. `--dom-shim` installs `./tests/wpt/runner/dom_shim.js` right after
`testharness.js` has loaded (so testharness keeps its shell environment), and
gives those tests a node tree to talk to and nothing more:

- `document` with `documentElement`, `head`, `body`, `title` (the page's
  `<title>`), `currentScript` (the running `<script>`, `null` between scripts),
  `readyState` (`"complete"`), `createElement`, `createTextNode`,
  `getElementById` and `getElementsByTagName`;
- `Node` / `Element` with `appendChild`, `insertBefore`, `removeChild`,
  `append`, `remove`, `contains`, attributes and `textContent`;
- `HTMLCanvasElement`, which exists but draws nothing: `getContext()` returns
  `null` and there is no `captureStream()`, so a helper that feature-detects
  canvas capture falls back to `getUserMedia()` (the fake devices);
- every tag outside a small table (`html`, `head`, `body`, `title`, `div`,
  `span`, `p`, `pre`, `script`, `canvas`) is an `HTMLUnknownElement`: there is
  no `<video>`, `<audio>`, `<iframe>` or `<img>`.

The page's own markup is not copied into it (`getElementById` finds only what a
script created), and the shim never dispatches an event, lays anything out or
fakes an API under test: a test either reaches the real implementation or still
fails. The option is off by default, and the expectation files are written
without it.

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
both are `ReferenceError`s in a runtime without a DOM. `--dom-shim` gets both,
and the helpers' scratch elements, past that point.

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
