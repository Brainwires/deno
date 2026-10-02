// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: drag and drop, native file dialogs and the rich clipboard.
//
// The clipboard round-trips every format through the OS clipboard. A real
// file dialog is opened and closed by an AbortSignal (no person needed), with
// the busy and argument checks around it. Drags need a pointer the runner
// cannot drive (an OS drag session), so the drag checks are the refusal
// paths plus the event surface; the drops themselves are n/a.

// deno-lint-ignore-file no-explicit-any

import {
  desktop,
  errorOf,
  html,
  page,
  PNG,
  Report,
  shared,
  sleep,
  titledWindow,
  within,
} from "../_shared/e2e.ts";

const r = new Report("dnd");
const isA = (e: unknown, name: string) =>
  e instanceof Error && (e.name === name || e.constructor.name === name);

Deno.serve((req) => shared(req) ?? html(page("e2e dnd")));
const win = titledWindow("E2E DnD");
await sleep(2500);

await r.step("surface", () => {
  const caps = desktop.windowCapabilities();
  r.set("capabilities", caps);
  r.set("clipboardCapabilities", desktop.clipboard.capabilities());
  r.check(
    "dialog, clipboard, startDrag and the drop handlers exist",
    typeof desktop.dialog?.showOpenDialog === "function" &&
      typeof desktop.dialog?.showSaveDialog === "function" &&
      desktop.clipboard instanceof EventTarget &&
      typeof win.startDrag === "function" &&
      ["ondragenter", "ondragover", "ondragleave", "ondrop"].every((k) =>
        k in win
      ),
  );
});

await r.step("clipboard", async () => {
  const cb = desktop.clipboard;
  const caps = cb.capabilities();
  r.check("clipboard capabilities: text everywhere", caps.text === true, caps);
  const text = `e2e-${Deno.pid} é中 \u{1F600}`;
  await cb.writeText(text);
  r.check("text round-trips", (await cb.readText()) === text);
  r.check(
    "navigator.clipboard reads the same text",
    (await (navigator as any).clipboard.readText()) === text,
  );
  r.check(
    "availableFormats() has text/plain",
    (await cb.availableFormats()).includes("text/plain"),
  );
  if (caps.html) {
    await cb.writeHTML(`<b>e2e</b> ${Deno.pid}`, `plain ${Deno.pid}`);
    const got = await cb.readHTML();
    r.check("HTML round-trips", got.includes(`<b>e2e</b> ${Deno.pid}`), got);
    r.check(
      "the HTML's plain alternative reads as text",
      (await cb.readText()) === `plain ${Deno.pid}`,
    );
    const f = await cb.availableFormats();
    r.check(
      "availableFormats() after HTML",
      f.includes("text/html") && f.includes("text/plain"),
      f,
    );
  } else {
    r.check(
      "no HTML here: writeHTML rejects NotSupported",
      isA(await errorOf(cb.writeHTML("<b>x</b>")), "NotSupported"),
    );
  }
  if (caps.image) {
    r.check(
      "writeImage of non-PNG bytes is a TypeError",
      (await errorOf(
        cb.writeImage(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9])),
      )) instanceof TypeError,
    );
    await cb.writeImage(PNG);
    const img = await cb.readImage();
    // A clipboard may re-encode the image: compare the decoded size via the
    // PNG header (IHDR width / height), not the bytes.
    const dims = (b: Uint8Array) =>
      b.length > 24
        ? new DataView(b.buffer, b.byteOffset).getUint32(16) + "x" +
          new DataView(b.buffer, b.byteOffset).getUint32(20)
        : null;
    r.check(
      "a PNG round-trips",
      img instanceof Uint8Array && img[0] === 0x89 && img[1] === 0x50 &&
        dims(img) === "1x1",
      img && { len: img.length, dims: dims(img) },
    );
    r.check(
      "availableFormats() after an image has image/png",
      (await cb.availableFormats()).includes("image/png"),
    );
  }
  await cb.writeText("");
  r.check(
    "writeText('') clears: readText() is empty",
    (await cb.readText()) === "",
  );
  if (caps.changeEvents) {
    let changes = 0;
    cb.onchange = () => changes++;
    await sleep(600);
    await cb.writeText(`change-${Deno.pid}`);
    for (let i = 0; i < 60 && changes === 0; i++) await sleep(50);
    r.check("a change event fires for a write", changes > 0);
    cb.onchange = null;
    await sleep(1000);
    const after = changes;
    await cb.writeText(`quiet-${Deno.pid}`);
    await sleep(1500);
    r.check("no change events once onchange is null", changes === after);
  } else {
    r.na(
      "clipboard change events",
      "the backend reports changeEvents: false here",
    );
  }
});

await r.step("dialogs", async () => {
  const caps = desktop.windowCapabilities();
  r.check(
    "bad properties reject with TypeError",
    (await errorOf(
      desktop.dialog.showOpenDialog({ properties: ["nope"] }),
    )) instanceof TypeError,
  );
  r.check(
    "a bad filter rejects with TypeError",
    (await errorOf(
      desktop.dialog.showOpenDialog({ filters: [{ name: 1 }] }),
    )) instanceof TypeError,
  );
  r.check(
    "an already-aborted signal rejects AbortError without a dialog",
    isA(
      await errorOf(
        desktop.dialog.showOpenDialog({ signal: AbortSignal.abort() }),
      ),
      "AbortError",
    ),
  );
  if (!caps.fileDialogs) {
    r.check(
      "no dialogs here: showOpenDialog rejects",
      (await errorOf(desktop.dialog.showOpenDialog({}))) !== null,
    );
    return;
  }
  const ac = new AbortController();
  const t0 = Date.now();
  const p = desktop.dialog.showOpenDialog(win, {
    title: "e2e",
    properties: ["openFile", "multiSelections"],
    filters: [{ name: "Text", extensions: ["txt"] }],
    signal: ac.signal,
  });
  await sleep(2000);
  r.check(
    "a second dialog while one is open is Deno.errors.Busy",
    isA(await errorOf(desktop.dialog.showSaveDialog({})), "Busy"),
  );
  ac.abort(new Error("e2e abort"));
  const got1 = await within(errorOf(p), 15000);
  const e = "value" in got1 ? got1.value : null;
  r.check(
    "aborting closes the dialog and rejects with the signal's reason",
    e?.message === "e2e abort",
    e && String(e),
  );
  r.check(
    "the runtime ran while the dialog was open",
    Date.now() - t0 < 20000,
    Date.now() - t0,
  );
  // Every kind of dialog closes on abort and frees the slot: open / save,
  // modal to a window / app-level.
  for (const kind of ["save", "open"] as const) {
    for (const modal of [false, true]) {
      const label = `${kind} dialog${modal ? " (modal)" : " (app-level)"}`;
      r.mark(label);
      const ac2 = new AbortController();
      const opts = { defaultPath: "e2e.txt", signal: ac2.signal };
      const p2 = kind === "save"
        ? (modal
          ? desktop.dialog.showSaveDialog(win, opts)
          : desktop.dialog.showSaveDialog(opts))
        : (modal
          ? desktop.dialog.showOpenDialog(win, opts)
          : desktop.dialog.showOpenDialog(opts));
      await sleep(1500);
      ac2.abort();
      const got = await within(errorOf(p2), 15000);
      r.check(
        `a ${label} opens and closes on abort (AbortError)`,
        "value" in got && isA(got.value, "AbortError"),
        "value" in got ? String(got.value) : "still open 15 s after abort()",
      );
      if (!("value" in got)) break;
    }
  }
});

await r.step("drag and drop", async () => {
  r.check(
    "startDrag({}) is a TypeError",
    (await errorOf(win.startDrag({}))) instanceof TypeError,
  );
  r.check(
    "startDrag({ files: [] }) is a TypeError",
    (await errorOf(win.startDrag({ files: [] }))) instanceof TypeError,
  );
  r.check(
    "a non-PNG icon is a TypeError",
    (await errorOf(
      win.startDrag({ files: [Deno.execPath()], icon: "nope" }),
    )) instanceof TypeError,
  );
  r.check(
    "a relative path fails",
    (await win.startDrag({ files: ["relative.txt"] })) === "failed",
  );
  r.check(
    "no held mouse button: the drag fails",
    (await win.startDrag({ files: [Deno.execPath()] })) === "failed",
  );
  for (const t of ["ondragenter", "ondragover", "ondragleave", "ondrop"]) {
    win[t] = () => {};
  }
  r.check(
    "the drop handlers accept listeners",
    ["ondragenter", "ondragover", "ondragleave", "ondrop"].every((k) =>
      typeof win[k] === "function"
    ),
  );
  r.na(
    "a real file drop and a completed drag-out",
    "they need an OS drag session driven by a pointer, which no hosted runner can synthesize",
  );
});

r.finish();
