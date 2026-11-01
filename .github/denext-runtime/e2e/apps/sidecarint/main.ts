// Copyright 2018-2026 the Deno authors. MIT license.
// e2e: a program the app carries, under a run grant that records its hash
// (DENEXT.md item 215). The runner packages it with `--allow-write` unscoped,
// so what keeps the app from the program is the runtime's denial and check,
// not a missing grant. Each launch does what the params' `phase` says; the
// runner swaps the file between launches.

import { describeError, html, page, Report, shared } from "../_shared/e2e.ts";

const r = new Report("sidecarint");
Deno.serve((req) => shared(req) ?? html(page("e2e sidecar integrity")));

const program: string = r.params.program;
const run = async () => {
  try {
    const o = await new Deno.Command(program, {
      stdout: "piped",
      stderr: "null",
    }).output();
    return o.success ? "ran" : `exit ${o.code}`;
  } catch (e) {
    return describeError(e);
  }
};
const attempt = async (f: () => unknown) => {
  try {
    await f();
    return "allowed";
  } catch (e) {
    return describeError(e);
  }
};

if (r.params.phase === "first") {
  r.data.first = await run();
  r.check(
    "the program is written from the app and runs",
    r.data.first === "ran",
    r.data.first,
  );
  const state =
    (await Deno.permissions.query({ name: "write", path: program })).state;
  r.check("the app's write grant is denied there", state === "denied", state);
  const written = await attempt(() => Deno.writeTextFile(program, "not it"));
  r.check(
    "the app cannot write the program",
    /NotCapable/.test(written),
    written,
  );
  const removed = await attempt(() => Deno.remove(program));
  r.check(
    "the app cannot remove the program",
    /NotCapable/.test(removed),
    removed,
  );
  const added = await attempt(() => Deno.writeTextFile(`${program}.sib`, "x"));
  r.check("the app cannot add to its folder", /NotCapable/.test(added), added);
  const beside = await attempt(() =>
    new Deno.Command(`${program}.sib`).outputSync()
  );
  r.check(
    "nothing else runs under the grant",
    /NotCapable/.test(beside),
    beside,
  );
} else {
  r.data.swapped = await run();
  r.check(
    "a program swapped outside the app is refused at spawn",
    /NotCapable/.test(r.data.swapped) && /Refusing to run/.test(r.data.swapped),
    r.data.swapped,
  );
  r.data.healed = await run();
  r.check(
    "the next spawn runs what was packaged",
    r.data.healed === "ran",
    r.data.healed,
  );
}
r.finish();
