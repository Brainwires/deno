// Every `Deno.desktop` op stays reachable in a plain `deno run`: the ops in
// NOT_IMPORTED_OPS (runtime/js/99_main.js) survive `removeImportedOps()` so
// the desktop runtime's post-bootstrap JS can call them, which means any code
// can call them through `Deno[Deno.internal].core.ops`. Outside a desktop app
// each must be inert: no dialog, no window, no clipboard, no OS registration,
// no file, no network, no panic and no promise that keeps the process alive;
// it answers "not supported", an empty value or a refusal. This runs with no
// permissions.
//
// Guard: every desktop op and class the runtime exposes must have a case
// below, and every case must still name an exposed op. A new op in
// NOT_IMPORTED_OPS without a case here fails this test (the Rust test
// `desktop_ops_inert_spec_covers_not_imported_ops` checks the same against
// 99_main.js's list).
const core = Deno[Deno.internal].core;
const ops = core.ops;

const isDesktop = (name) =>
  name.startsWith("op_desktop_") ||
  ["BrowserWindow", "Dock", "Tray", "Notification"].includes(name);

function show(value) {
  if (value === undefined) return "undefined";
  if (value instanceof Uint8Array) return `Uint8Array(${value.length})`;
  return JSON.stringify(value);
}

function describe(e) {
  const message = String(e?.message ?? e).split("\n")[0];
  return `${e?.name ?? typeof e}: ${message}`;
}

async function outcome(fn) {
  try {
    return `-> ${show(await fn())}`;
  } catch (e) {
    return `throws ${describe(e)}`;
  }
}

const png = new Uint8Array([
  0x89,
  0x50,
  0x4e,
  0x47,
  0x0d,
  0x0a,
  0x1a,
  0x0a,
  0,
  0,
  0,
  0,
]);

// Ordered as in NOT_IMPORTED_OPS. Each case calls the op the way its
// `Deno.desktop` wrapper would, with arguments that would act in an app.
const cases = {
  op_desktop_apply_patch: () =>
    ops.op_desktop_apply_patch(new Uint8Array(4), "00".repeat(32)),
  op_desktop_confirm_update: () => ops.op_desktop_confirm_update(),
  op_desktop_verify_ed25519: () =>
    ops.op_desktop_verify_ed25519("AAAA", "AAAA", new Uint8Array(1)),
  // First call wins; DESKTOP_JS makes it before any app code runs.
  op_desktop_init: async () => {
    const first = await outcome(() =>
      ops.op_desktop_init(Symbol("brand"), () => {})
    );
    const again = await outcome(() =>
      ops.op_desktop_init(Symbol("brand"), () => {})
    );
    return `${first}; again ${again}`;
  },
  op_desktop_recv_event: () => ops.op_desktop_recv_event(),
  op_desktop_take_launch_targets: () => ops.op_desktop_take_launch_targets(),
  op_desktop_subscribe_launch_events: () =>
    ops.op_desktop_subscribe_launch_events("openurl"),
  op_desktop_get_scheme_owner: () => ops.op_desktop_get_scheme_owner("myapp"),
  op_desktop_register_scheme: () =>
    ops.op_desktop_register_scheme("myapp", true),
  op_desktop_passkey_capabilities: () => ops.op_desktop_passkey_capabilities(),
  op_desktop_passkey_request: () =>
    ops.op_desktop_passkey_request(true, 0, "{}"),
  op_desktop_auth_session_capabilities: () =>
    ops.op_desktop_auth_session_capabilities(),
  op_desktop_auth_session_start: () =>
    ops.op_desktop_auth_session_start(
      0,
      "https://example.com/authorize",
      "myapp",
      true,
    ),
  op_desktop_auth_session_cancel: () => ops.op_desktop_auth_session_cancel(),
  // Needs --allow-ffi before it looks at anything else (ffi.js has the
  // rest).
  op_desktop_run_on_main_thread: () =>
    ops.op_desktop_run_on_main_thread(null, null),
  op_desktop_resolve_bind_call: () => ops.op_desktop_resolve_bind_call(1, null),
  op_desktop_reject_bind_call: () => ops.op_desktop_reject_bind_call(1, "x"),
  op_desktop_alert: () => ops.op_desktop_alert("title", "message"),
  op_desktop_alert_async: () =>
    ops.op_desktop_alert_async("Application Error", "message"),
  op_desktop_confirm: () => ops.op_desktop_confirm("message"),
  op_desktop_prompt: () => ops.op_desktop_prompt("message", "default"),
  op_desktop_read_clipboard_text: () => ops.op_desktop_read_clipboard_text(),
  op_desktop_write_clipboard_text: () =>
    ops.op_desktop_write_clipboard_text("text"),
  op_desktop_clipboard_capabilities: () =>
    ops.op_desktop_clipboard_capabilities(),
  op_desktop_read_clipboard_html: () => ops.op_desktop_read_clipboard_html(),
  op_desktop_write_clipboard_html: () =>
    ops.op_desktop_write_clipboard_html("<b>x</b>", "x"),
  op_desktop_read_clipboard_image: () => ops.op_desktop_read_clipboard_image(),
  op_desktop_write_clipboard_image: () =>
    ops.op_desktop_write_clipboard_image(png),
  op_desktop_read_clipboard_formats: () =>
    ops.op_desktop_read_clipboard_formats(),
  op_desktop_clipboard_watch: () => ops.op_desktop_clipboard_watch(true),
  op_desktop_start_drag: () =>
    ops.op_desktop_start_drag(0, ["/etc/hosts"], new Uint8Array()),
  // Open, then wait on and cancel what it returned.
  op_desktop_file_dialog_open: async () => {
    const rid = ops.op_desktop_file_dialog_open({
      title: "t",
      files: true,
      multiple: true,
    });
    const cancelled = ops.op_desktop_file_dialog_cancel(rid);
    const result = await ops.op_desktop_file_dialog_wait(rid);
    return `rid ${rid}, cancel ${cancelled}, wait ${show(result)}`;
  },
  op_desktop_file_dialog_wait: () => ops.op_desktop_file_dialog_wait(999),
  op_desktop_file_dialog_cancel: () => ops.op_desktop_file_dialog_cancel(999),
  op_desktop_system_capabilities: () => ops.op_desktop_system_capabilities(),
  op_desktop_platform_features: () => ops.op_desktop_platform_features(),
  op_desktop_title_bar_preferences: () =>
    ops.op_desktop_title_bar_preferences(),
  op_desktop_register_shortcut: () =>
    ops.op_desktop_register_shortcut("CmdOrCtrl+Shift+K"),
  op_desktop_unregister_shortcut: () =>
    ops.op_desktop_unregister_shortcut("CmdOrCtrl+Shift+K"),
  op_desktop_unregister_all_shortcuts: () =>
    ops.op_desktop_unregister_all_shortcuts(),
  op_desktop_list_shortcuts: () => ops.op_desktop_list_shortcuts(),
  op_desktop_canonical_accelerator: () =>
    ops.op_desktop_canonical_accelerator("CmdOrCtrl+Shift+K"),
  op_desktop_get_launch_at_login: () => ops.op_desktop_get_launch_at_login(),
  op_desktop_set_launch_at_login: () =>
    ops.op_desktop_set_launch_at_login(true),
  op_desktop_devtools_enabled: () => ops.op_desktop_devtools_enabled(0),
  op_desktop_menu_capabilities: () => ops.op_desktop_menu_capabilities(),
  op_desktop_notification_capabilities: () =>
    ops.op_desktop_notification_capabilities(),
  op_desktop_schedule_notification: () =>
    ops.op_desktop_schedule_notification({
      tag: "t",
      title: "title",
      body: "body",
      at: Date.now() + 60_000,
      actions: [],
    }, null),
  op_desktop_list_scheduled_notifications: () =>
    ops.op_desktop_list_scheduled_notifications(),
  op_desktop_cancel_notification: () => ops.op_desktop_cancel_notification("t"),
  // The destination is operator config, never the caller's (see
  // desktop_error_report_no_escape).
  op_desktop_send_error_report: () =>
    ops.op_desktop_send_error_report("report"),
  op_desktop_request_notification_permission: () =>
    ops.op_desktop_request_notification_permission(false),
  op_desktop_query_notification_permission: () =>
    ops.op_desktop_query_notification_permission(),
  op_desktop_screens: () => ops.op_desktop_screens(),
  op_desktop_window_capabilities: () => ops.op_desktop_window_capabilities(),
  op_desktop_quit: () => ops.op_desktop_quit(),
  op_desktop_set_quit_on_last_window_closed: () =>
    ops.op_desktop_set_quit_on_last_window_closed(false),
  op_desktop_close_reply: () => ops.op_desktop_close_reply(1, true),
  // The updater ops (desktop_update_ops_inert has the detail).
  op_desktop_app_update_info: () => ops.op_desktop_app_update_info().configured,
  op_desktop_app_update_check: () =>
    ops.op_desktop_app_update_check(new Uint8Array(2), false),
  op_desktop_app_update_begin: () => ops.op_desktop_app_update_begin(),
  op_desktop_app_update_write: () =>
    ops.op_desktop_app_update_write(new Uint8Array(1)),
  op_desktop_app_update_finish: () => ops.op_desktop_app_update_finish(),
  op_desktop_app_update_abort: () => ops.op_desktop_app_update_abort(),
  op_desktop_app_update_stage: () => ops.op_desktop_app_update_stage(false),
  op_desktop_app_update_apply: () => ops.op_desktop_app_update_apply(),
  op_desktop_app_update_confirm: () => ops.op_desktop_app_update_confirm(),
  // The native classes refuse to construct (they used to panic).
  BrowserWindow: () => new ops.BrowserWindow({ width: 10, height: 10 }),
  Dock: () => new ops.Dock(),
  Tray: () => new ops.Tray(),
  Notification: () => new ops.Notification("title", { body: "body" }),
};

const exposed = Object.keys(ops).filter(isDesktop);
const uncovered = exposed.filter((name) => !(name in cases));
const stale = Object.keys(cases).filter((name) => !exposed.includes(name));
for (const name of uncovered) console.log(`UNCOVERED ${name}`);
for (const name of stale) console.log(`STALE ${name}`);

for (const [name, fn] of Object.entries(cases)) {
  if (stale.includes(name)) continue;
  console.log(`${name} ${await outcome(fn)}`);
}
console.log(`${exposed.length} exposed, ${uncovered.length} uncovered`);
if (uncovered.length || stale.length) Deno.exit(1);
