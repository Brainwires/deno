const d = Deno.desktop;
// cancel() answers a boolean, not a promise.
d.authSession.cancel().then(() => {});
// ephemeral is a boolean.
d.authSession.start({ url: "https://x", callbackScheme: "x", ephemeral: 1 });
// Not an updater error code.
const code: Deno.desktop.AppUpdateErrorCode = "nope";
console.log(code);
