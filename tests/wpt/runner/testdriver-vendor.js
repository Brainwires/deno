// Copyright 2018-2026 the Deno authors. MIT license.
// @ts-check

// The runner's `resources/testdriver-vendor.js`: WPT's designated hook for a
// test runner to implement `test_driver` (served in place of the suite's
// empty file, as the runner does for `testharnessreport.js`).
//
// A browser runner answers these calls over WebDriver. This runner has no
// WebDriver; it implements only what it can answer truthfully:
//
// - `set_permission` for `camera` and `microphone`. The runner starts every
//   test with `DENO_FAKE_MEDIA_DEVICES=1`, under which the runtime grants
//   `getUserMedia()` (as Chrome's `--use-fake-ui-for-media-stream` does), so
//   `"granted"` is already the runtime's state and resolves. Any other state
//   (`"denied"`, `"prompt"`) is set through the runtime's hook,
//   `Deno.webrtc.setMediaPermission(name, state)`, when the runtime has one;
//   without it the call rejects, so a test that needs a denied permission
//   keeps failing.
// - everything else keeps `testdriver.js`'s own "... is not implemented by
//   testdriver-vendor.js" rejection. `in_automation` makes the calls that
//   would otherwise wait for a human (`click`, `send_keys`, and `bless`,
//   which clicks) reject at once instead.

(() => {
  "use strict";
  const internal = Reflect.get(globalThis, "test_driver_internal");
  // `testdriver.js` reads `document` while it loads, so without the runner's
  // document environment it has failed before this file runs.
  if (internal === undefined || internal === null) return;

  internal.in_automation = true;

  const MEDIA_PERMISSIONS = ["camera", "microphone"];
  const notImplemented = () =>
    new Error("set_permission() is not implemented by testdriver-vendor.js");

  /**
   * @param {{ descriptor: { name: string }, state: string }} params
   * @param {unknown} context
   */
  internal.set_permission = async function (params, context = null) {
    const name = params?.descriptor?.name;
    const state = params?.state;
    // Another browsing context, or a permission other than capture: nothing
    // here can answer it.
    if (context !== null || !MEDIA_PERMISSIONS.includes(name)) {
      throw notImplemented();
    }
    const webrtc = Reflect.get(globalThis.Deno ?? {}, "webrtc");
    const setMediaPermission = webrtc?.setMediaPermission;
    if (typeof setMediaPermission === "function") {
      await setMediaPermission.call(webrtc, name, state);
      return;
    }
    if (state === "granted") return;
    throw notImplemented();
  };
})();
