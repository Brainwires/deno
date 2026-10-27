// Copyright 2018-2026 the Deno authors. MIT license.
// The e2e n/a baseline (P7): which checks are expected to be n/a on each
// <target>/<backend>/<variant> (../expected-na.json). An n/a outside it is a
// regression (a probe that started reporting "unsupported" turns its
// checks into silent passes otherwise), so run.ts fails on it.

/** expected-na.json: key `<target>/<backend>/<variant>` → area → check names. */
export type NaBaseline = Record<string, Record<string, string[]> | string>;

export interface NaCheck {
  area: string;
  name: string;
  reason?: string;
}

export interface NaComparison {
  /** The baseline key compared against. */
  key: string;
  /** n/a checks the baseline does not list: failures. */
  unexpected: NaCheck[];
  /** Baseline entries of the areas that ran that were not n/a this time. */
  noLongerNa: NaCheck[];
}

export function baselineKey(
  target: string,
  backend: string,
  variant: string,
): string {
  return `${target}/${backend}/${variant}`;
}

/**
 * Compares this run's n/a checks with the baseline. Only the areas that ran
 * are compared (E2E_AREAS may select a few); a key the baseline lacks
 * expects no n/a at all.
 */
export function compareNa(
  baseline: NaBaseline,
  key: string,
  areasRun: readonly string[],
  na: readonly NaCheck[],
): NaComparison {
  const entry = baseline[key];
  const expected = typeof entry === "object" && entry !== null ? entry : {};
  const unexpected = na.filter((c) =>
    !(expected[c.area] ?? []).includes(c.name)
  );
  const seen = new Set(na.map((c) => `${c.area}\0${c.name}`));
  const noLongerNa: NaCheck[] = [];
  for (const area of areasRun) {
    for (const name of expected[area] ?? []) {
      if (!seen.has(`${area}\0${name}`)) noLongerNa.push({ area, name });
    }
  }
  return { key, unexpected, noLongerNa };
}
