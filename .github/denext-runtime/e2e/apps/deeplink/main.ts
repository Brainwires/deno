// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: deep links, opened files, second instances and scheme registration.
//
// Every launch records what it was launched with (Deno.desktop.launchUrls /
// launchFiles, Deno.args), every openurl / openfile / secondinstance event,
// and who owns its deep-link scheme (at start, after the runtime's startup
// registration, after an explicit registerScheme(), and after a forced one
// when the params ask for it). It keeps running until the runner kills it,
// rewriting its result on every event; `done` is set once the scheme checks
// ran, so the runner knows the launch is fully up.

import {
  describeError,
  desktop,
  errorOf,
  html,
  page,
  Report,
  sleep,
} from "../_shared/e2e.ts";

const r = new Report("deeplink");
const scheme: string = r.params.scheme;
const events: unknown[] = [];

r.set("launch", {
  args: Deno.args,
  cwd: Deno.cwd(),
  execPath: Deno.execPath(),
  launchUrls: desktop?.launchUrls ?? null,
  launchFiles: desktop?.launchFiles ?? null,
  identifier: r.params.identifier ?? null,
});
r.set("events", events);

const record = (type: string) => (e: CustomEvent) => {
  events.push({ type, at: new Date().toISOString(), detail: e.detail });
  r.set("events", events);
};
desktop.addEventListener("openurl", record("openurl"));
desktop.addEventListener("openfile", record("openfile"));
// The on… handler property subscribes the same way addEventListener does.
desktop.onsecondinstance = record("secondinstance");

Deno.serve(() => html(page(`e2e deeplink ${Deno.pid}`)));

const owner: Record<string, unknown> = {};
const capture = async (f: () => Promise<unknown>) => {
  try {
    return await f();
  } catch (e) {
    return { error: describeError(e) };
  }
};
owner.atStart = await capture(() => desktop.getSchemeOwner(scheme));
const undeclared = await errorOf(desktop.getSchemeOwner("e2enotdeclared"));
r.check(
  "getSchemeOwner of an undeclared scheme is a TypeError",
  undeclared instanceof TypeError,
  undeclared && describeError(undeclared),
);
const undeclaredReg = await errorOf(desktop.registerScheme("e2enotdeclared"));
r.check(
  "registerScheme of an undeclared scheme is a TypeError",
  undeclaredReg instanceof TypeError,
  undeclaredReg && describeError(undeclaredReg),
);
r.set("owner", owner);
// The startup registration runs in the background.
await sleep(Number(r.params.settleMs ?? 4000));
owner.later = await capture(() => desktop.getSchemeOwner(scheme));
owner.explicit = await capture(() => desktop.registerScheme(scheme));
if (r.params.force) {
  owner.forced = await capture(() =>
    desktop.registerScheme(scheme, { force: true })
  );
  owner.afterForce = await capture(() => desktop.getSchemeOwner(scheme));
}
r.set("owner", owner);
r.done();
