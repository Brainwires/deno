// Copyright 2018-2026 the Deno authors. MIT license.
// The results directory of the e2e run that packaged this app. The runner
// (../../lib/runner.ts, packageApp) overwrites this file in each copy it
// packages, so every launch of the app, the ones the OS starts included,
// writes where that run reads and nowhere another run on the same machine
// does. null here, in the source tree: see e2eDir() in e2e.ts.
export const RUN_DIR: string | null = null;
