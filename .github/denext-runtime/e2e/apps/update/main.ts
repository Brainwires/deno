// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: full-app self-update (Deno.desktop.updater). The same code ships as
// 1.0.0, 2.0.0 and 3.0.0 (only deno.json "version" differs). Each launch
// reads <dir>/update/probe.json for what to do, records its status, and
// exits; the runner (areas/update.ts) drives the sequence and checks the
// install on disk. Launches started by the update helper (the relaunch
// after a swap, the rollback) have no runner, so everything goes to files.

// deno-lint-ignore-file no-explicit-any

import { desktop, e2eDir, html, page } from "../_shared/e2e.ts";

const dir = `${e2eDir()}/update`;
Deno.mkdirSync(dir, { recursive: true });
const updater = desktop?.updater;
const log = (name: string, value: unknown) =>
  Deno.writeTextFileSync(`${dir}/${name}.json`, JSON.stringify(value, null, 2));
const append = (line: string) =>
  Deno.writeTextFileSync(
    `${dir}/events.log`,
    `${new Date().toISOString()} ${Deno.pid} ${line}\n`,
    { append: true },
  );

Deno.serve(() => html(page("e2e update")));

let probe: any = {};
try {
  probe = JSON.parse(Deno.readTextFileSync(`${dir}/probe.json`));
} catch { /* none */ }
const status = updater
  ? updater.status()
  : { configured: false, reason: "no Deno.desktop.updater" };
const v = status.version;
log(`status-${v}-${Deno.pid}`, {
  ...status,
  argv: Deno.args,
  execPath: Deno.execPath(),
});
append(
  `start version=${v} trial=${status.trial} updatedFrom=${status.updatedFrom} rolledBackFrom=${status.rolledBackFrom} phase=${status.phase}`,
);
const opts = {
  caCerts: probe.caCert ? [Deno.readTextFileSync(probe.caCert)] : [],
};

async function outcome(f: () => Promise<unknown>) {
  try {
    return { ok: await f() };
  } catch (e) {
    return {
      code: (e as any).code ?? null,
      name: (e as any).name,
      message: String((e as any).message),
    };
  }
}

async function main() {
  if (status.trial) {
    if (probe.crashTrialVersion === v) {
      append(`crashing trial ${v} before confirm`);
      Deno.exit(3);
    }
    const confirmed = updater.confirm();
    append(`confirmed=${confirmed}`);
    log(`confirmed-${v}`, { confirmed, status: updater.status() });
    Deno.exit(0);
  }
  if (probe.mode === "adversarial" && probe.cases) {
    const out: Record<string, any> = {};
    for (
      const [name, c] of Object.entries(
        probe.cases as Record<
          string,
          { url: string; download?: boolean; optOut?: boolean }
        >,
      )
    ) {
      out[name] = await outcome(async () => {
        const r = await updater.check(c.url, opts);
        if (c.download && r.available) {
          await updater.download(opts);
          return await updater.stage(
            c.optOut === false ? {} : { allowUnsignedDev: true },
          );
        }
        return r;
      });
      append(`case ${name}: ${JSON.stringify(out[name]).slice(0, 200)}`);
    }
    log(`adversarial-${v}`, out);
    Deno.exit(0);
  }
  if (probe.mode === "update" && probe.updateFrom === v) {
    const check = await outcome(() => updater.check(probe.manifest, opts));
    append(`check ${JSON.stringify(check).slice(0, 300)}`);
    log(`check-${v}`, check);
    if (!(check as any).ok?.available) Deno.exit(0);
    let last = 0;
    let events = 0;
    updater.addEventListener("progress", (e: CustomEvent) => {
      events++;
      last = e.detail.transferred;
    });
    const dl = await outcome(() => updater.download(opts));
    const stage = await outcome(() =>
      updater.stage({ allowUnsignedDev: true })
    );
    append(
      `downloaded ${JSON.stringify(dl)} progress=${last} staged ${
        JSON.stringify(stage).slice(0, 300)
      }`,
    );
    log(`staged-${v}`, {
      dl,
      stage,
      progress: last,
      progressEvents: events,
      status: updater.status(),
    });
    if (!(stage as any).ok) Deno.exit(1);
    const r = updater.applyAndRelaunch({ force: true });
    append(`applyAndRelaunch ${JSON.stringify(r)}`);
    setTimeout(() => Deno.exit(0), 3000);
    return;
  }
  if (probe.mode === "status") {
    log(`only-status-${v}`, status);
  }
  Deno.exit(0);
}
main().catch((e) => {
  append(`main threw ${e}`);
  Deno.exit(1);
});
