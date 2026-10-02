// The Deno.desktop.updater ops survive `removeImportedOps()` (they are in
// NOT_IMPORTED_OPS so the desktop runtime's post-bootstrap JS can reach
// them), so a plain `deno run` can call them through
// `Deno[Deno.internal].core.ops`. Outside a packaged desktop app they must
// refuse without touching the install of the running `deno` executable:
// no path, no update state, no file deleted (this runs with no permissions).
const ops = Deno[Deno.internal].core.ops;

const info = ops.op_desktop_app_update_info();
console.log(
  "info:",
  info.configured,
  info.install,
  info.kind,
  info.phase,
  info.appId,
);

function code(fn) {
  try {
    fn();
    return "did not throw";
  } catch (e) {
    return e.message.split(":")[0];
  }
}
console.log(
  "check:",
  code(() => ops.op_desktop_app_update_check(new Uint8Array(2), false)),
);
console.log("begin:", code(() => ops.op_desktop_app_update_begin()));
console.log("apply:", code(() => ops.op_desktop_app_update_apply()));
try {
  await ops.op_desktop_app_update_stage(false);
  console.log("stage: did not throw");
} catch (e) {
  console.log("stage:", e.message.split(":")[0]);
}
console.log("confirm:", ops.op_desktop_app_update_confirm());
ops.op_desktop_app_update_abort();
console.log(
  "write:",
  code(() => ops.op_desktop_app_update_write(new Uint8Array(1))),
);
console.log("finish:", code(() => ops.op_desktop_app_update_finish()));
