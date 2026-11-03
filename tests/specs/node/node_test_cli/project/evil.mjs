// Must never load: a test name pattern that tries to add `--import` to the
// child's NODE_OPTIONS (see pattern_cannot_add_flags.mjs).
import { writeFileSync } from "node:fs";
writeFileSync(process.env.INJECT_MARK, "");
