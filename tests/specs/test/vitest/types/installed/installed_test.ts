// An installed vitest's own types take precedence over the built-in ones,
// which have no such export.
import { installedVitestTypes } from "vitest";

const installed: true = installedVitestTypes;
console.log(installed);
