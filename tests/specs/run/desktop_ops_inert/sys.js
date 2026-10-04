// The desktop integrations that reach past the app's own windows (reading
// and watching the clipboard, global shortcuts, launch at login, taking a
// URL scheme from its owner, notifications) check unscoped --allow-sys
// first: NotCapable without it, also with only some sys names; with it they
// are as inert as every other op outside a desktop app.
const ops = Deno[Deno.internal].core.ops;
const granted = Deno.permissions.querySync({ name: "sys" }).state ===
  "granted";

function show(value) {
  return value === undefined ? "undefined" : JSON.stringify(value);
}

const cases = {
  op_desktop_read_clipboard_text: () => ops.op_desktop_read_clipboard_text(),
  op_desktop_read_clipboard_html: () => ops.op_desktop_read_clipboard_html(),
  op_desktop_read_clipboard_image: () => ops.op_desktop_read_clipboard_image(),
  op_desktop_read_clipboard_formats: () =>
    ops.op_desktop_read_clipboard_formats(),
  op_desktop_clipboard_watch: () => ops.op_desktop_clipboard_watch(true),
  "op_desktop_clipboard_watch(false)": () =>
    ops.op_desktop_clipboard_watch(false),
  op_desktop_register_shortcut: () =>
    ops.op_desktop_register_shortcut("CmdOrCtrl+Shift+K"),
  op_desktop_set_launch_at_login: () =>
    ops.op_desktop_set_launch_at_login(true),
  op_desktop_register_scheme: () =>
    ops.op_desktop_register_scheme("myapp", true),
  "op_desktop_register_scheme(no force)": () =>
    ops.op_desktop_register_scheme("myapp", false),
  op_desktop_schedule_notification: () =>
    ops.op_desktop_schedule_notification({
      tag: "t",
      title: "title",
      at: Date.now() + 60_000,
    }),
};

console.log(`sys ${granted ? "granted" : "not granted"}`);
for (const [name, fn] of Object.entries(cases)) {
  try {
    console.log(`${name} -> ${show(await fn())}`);
  } catch (e) {
    console.log(`${name} throws ${e.name}: ${e.message.split("\n")[0]}`);
  }
}
