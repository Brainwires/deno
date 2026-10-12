import { als } from "./als.mjs";

// Top-level code of a module first loaded by `import()` runs in the
// importer's async context, as in Node.
export const atTop = als.getStore();
