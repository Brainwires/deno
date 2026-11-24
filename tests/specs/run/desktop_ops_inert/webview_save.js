// `WebView.controlDownload(id, "save", path)` writes where the app says, so
// it needs write permission for that path: outside the --allow-write grant
// it throws NotCapable before anything else is looked at; inside it the op
// goes on (and, with no desktop backend, answers "unsupported").
const ops = Deno[Deno.internal].core.ops;
const sep = Deno.build.os === "windows" ? "\\" : "/";
const dir = import.meta.dirname;
for (
  const [label, path] of [
    ["inside the grant", `${dir}${sep}granted${sep}file.bin`],
    ["outside the grant", `${dir}${sep}elsewhere${sep}file.bin`],
  ]
) {
  try {
    const r = ops.op_desktop_webview_download_control(1, 1, "save", path);
    console.log(`save ${label} -> ${r.ok ? "ok" : r.code}`);
  } catch (e) {
    console.log(`save ${label} throws ${e.name}`);
  }
}
