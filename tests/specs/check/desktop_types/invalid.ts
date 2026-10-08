const d = Deno.desktop;
// cancel() answers a boolean, not a promise.
d.authSession.cancel().then(() => {});
// ephemeral is a boolean.
d.authSession.start({ url: "https://x", callbackScheme: "x", ephemeral: 1 });
// Not an updater error code.
const code: Deno.desktop.AppUpdateErrorCode = "nope";
console.log(code);
// runOnMainThread runs native code, never a JavaScript callback.
declare const callback: Deno.UnsafeCallback<
  { parameters: ["pointer"]; result: "pointer" }
>;
d.runOnMainThread(callback);
// Not a sandbox mode or a file chooser.
const sandbox: Deno.desktop.PlatformFeatures["sandbox"] = "seccomp";
const chooser: Deno.desktop.PlatformFeatures["fileChooser"] = "zenity";
console.log(sandbox, chooser);
