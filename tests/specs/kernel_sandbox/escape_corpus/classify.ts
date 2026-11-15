// Copyright 2018-2026 the Deno authors. MIT license.
// Who refused a probe? probe.c sees only an errno: EPERM / EACCES is "held".
// But the host may refuse the same call with no Deno sandbox in force: a
// container's seccomp profile (io_uring_setup, keyctl), a prohibit route
// (169.254.169.254), an outer Seatbelt profile, Yama. corpus.ts therefore runs
// every probe once UNCONFINED first (the baseline: the bare probe executable,
// started by the unconfined orchestrator, under the same host policy) and
// classifies each confined result against it: a refusal counts as "held" only
// when the baseline was not refused, i.e. only our sandbox can explain it.

export type State = "open" | "held" | "host" | "n/a" | "crash";

export interface Result {
  state: State | string;
  detail: string;
}

/** The baseline refused it too (or a probe's own host check said so). */
function refusedByHost(baseline: Result | undefined): boolean {
  return baseline !== undefined &&
    (baseline.state === "held" || baseline.state === "host");
}

export function classify(confined: Result, baseline?: Result): Result {
  if (confined.state === "held" && refusedByHost(baseline)) {
    return {
      state: "host",
      detail:
        `${confined.detail} (refused without any Deno sandbox too: host policy)`,
    };
  }
  return confined;
}

/** "<state> <detail>" as probe.c prints it. */
export function parse(line: string): Result {
  const [state, ...rest] = line.trim().split(" ");
  return { state, detail: rest.join(" ") };
}
