// Copyright 2018-2026 the Deno authors. MIT license.

// Preloaded (config.jsonc `extraDenoArgs`) by Node's Argon2 tests only. They
// skip themselves unless `process.versions.openssl` reports at least 3.2,
// the OpenSSL that gained Argon2. Deno's `node:crypto` has Argon2 without
// OpenSSL and keeps reporting 3.0.7 everywhere else
// (ext/node/polyfills/_process/process.ts says why), so only these tests
// see a newer version.
import process from "node:process";

process.versions.openssl = "3.2.0";
