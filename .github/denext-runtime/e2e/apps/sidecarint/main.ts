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
const parent = (p: string) =>
  p.slice(0, Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")));
const run = async (cmd = program) => {
  try {
    const o = await new Deno.Command(cmd, {
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
  // The folders above the program's are the app's to write, but moving one
  // would take the program's folder with it.
  const hashDir = parent(program);
  for (
    const [name, dir] of [["parent", parent(hashDir)], [
      "sidecars",
      parent(parent(hashDir)),
    ]]
  ) {
    const moved = await attempt(() => Deno.rename(dir, `${dir}.moved`));
    r.check(
      `the app cannot move the program's ${name} folder`,
      /NotCapable/.test(moved),
      moved,
    );
  }
  const linked = await attempt(() =>
    Deno.link(program, `${parent(parent(hashDir))}.link`)
  );
  r.check(
    "the app cannot link to the program",
    /NotCapable/.test(linked),
    linked,
  );
  if (Deno.build.os !== "linux") {
    // The grant takes another case of the path there; so does the check.
    const cased = `${hashDir}${program.slice(hashDir.length).toUpperCase()}`;
    r.data.cased = await run(cased);
    r.check(
      "another case of the path runs what was packaged",
      r.data.cased === "ran",
      r.data.cased,
    );
  }
} else {
  const swapped = r.data.swapped = await run();
  r.check(
    "a program swapped outside the app is refused at spawn",
    /NotCapable/.test(swapped) && /Refusing to run/.test(swapped),
    swapped,
  );
  r.data.healed = await run();
  r.check(
    "the next spawn runs what was packaged",
    r.data.healed === "ran",
    r.data.healed,
  );
}
r.finish();
