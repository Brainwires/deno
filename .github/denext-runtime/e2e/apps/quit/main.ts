// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: the OS's quits reach `beforequit` (laufey API 52). The runner sends this
// process quit Apple Events: a plain one is refused and the app stays up; a
// logout is held, and refused with cancelQuit(); a last one is let through.

import { desktop, html, page, Report } from "../_shared/e2e.ts";

const r = new Report("quit");
const seen: string[] = [];
desktop.addEventListener("beforequit", (ev: Event) => {
  const reason = (ev as Event & { reason: string }).reason;
  seen.push(reason);
  r.set("reasons", seen);
  if (seen.length === 1) {
    ev.preventDefault();
  } else if (seen.length === 2) {
    // Held, as a page across the bridge would hold it, then refused.
    ev.preventDefault();
    setTimeout(() => r.set("cancelQuit", desktop.cancelQuit()), 200);
  } else if (seen.length === 3) {
    // Held and never answered: laufey's deadline lets the logout go, and a
    // page trying to refuse it just before can't stop that or come later.
    ev.preventDefault();
  }
});
r.set("ready", true);
r.done();

Deno.serve(() => html(page("e2e quit", "", "")));
