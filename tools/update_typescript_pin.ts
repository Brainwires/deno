#!/usr/bin/env -S deno run --allow-net=registry.npmjs.org --allow-read --allow-write
// Copyright 2018-2026 the Deno authors. MIT license.

// deno-lint-ignore-file no-console

/**
 * Bump a native TypeScript (tsgo) pin in cli/tsc/typescript_pins.json (denext).
 *
 *   tools/update_typescript_pin.ts <role> [<version> | next | latest]
 *
 * `<role>` is a key of the pins file (`lsp`: the `deno lsp` tsgo backend).
 * With `next` (the default for `lsp`) or `latest` the npm dist-tag is
 * resolved to an exact version first; a pin is always an exact version.
 * Every platform package's tarball URL and `dist.integrity` (sha512) are read
 * from the registry and written to the pins file, which the Rust code, the
 * release archive (`.github/denext-runtime/fetch_typescript.py`) and the test
 * harness all read, so this is the only edit a bump needs. Then run:
 *
 *   cargo test -p integration_tests --test integration -- lsp_tsgo
 *
 * (the harness re-fetches the newly pinned binary into target/ on first use).
 */

const PLATFORMS = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-arm64",
  "win32-x64",
] as const;
const REGISTRY = "https://registry.npmjs.org";
const PINS_PATH = new URL("../cli/tsc/typescript_pins.json", import.meta.url);

interface PlatformPin {
  tarball: string;
  integrity: string;
}
interface Pin {
  version: string;
  platforms: Record<string, PlatformPin>;
}

async function getJson(url: string): Promise<Record<string, unknown>> {
  const res = await fetch(url, { headers: { accept: "application/json" } });
  if (!res.ok) {
    throw new Error(`GET ${url}: ${res.status} ${res.statusText}`);
  }
  return await res.json();
}

async function resolveVersion(spec: string): Promise<string> {
  if (/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(spec)) {
    return spec;
  }
  const tags = await getJson(`${REGISTRY}/-/package/typescript/dist-tags`);
  const version = tags[spec];
  if (typeof version !== "string") {
    throw new Error(`typescript has no dist-tag "${spec}"`);
  }
  return version;
}

async function main() {
  const [role, spec] = Deno.args;
  if (!role) {
    console.error(
      "usage: tools/update_typescript_pin.ts <role> [<version>|next|latest]",
    );
    Deno.exit(2);
  }
  const pins = JSON.parse(await Deno.readTextFile(PINS_PATH)) as Record<
    string,
    unknown
  >;
  const previous = pins[role] as Pin | undefined;
  const version = await resolveVersion(
    spec ?? (role === "lsp" ? "next" : "latest"),
  );
  if (role === "lsp" && !/^7\.\d+\.\d+-dev\.\d{8}\.\d+$/.test(version)) {
    console.warn(
      `note: ${version} is not a dated 7.x nightly; the lsp pin is meant to track one until 7.1 is stable`,
    );
  }
  const platforms: Record<string, PlatformPin> = {};
  for (const platform of PLATFORMS) {
    const name = `@typescript/typescript-${platform}`;
    const meta = await getJson(`${REGISTRY}/${name}/${version}`);
    const dist = meta.dist as { tarball?: string; integrity?: string };
    if (meta.version !== version || !dist?.tarball || !dist?.integrity) {
      throw new Error(`${name}@${version}: incomplete registry metadata`);
    }
    if (!dist.integrity.startsWith("sha512-")) {
      throw new Error(`${name}@${version}: integrity is not sha512`);
    }
    platforms[platform] = {
      tarball: dist.tarball,
      integrity: dist.integrity,
    };
  }
  pins[role] = { version, platforms } satisfies Pin;
  await Deno.writeTextFile(PINS_PATH, JSON.stringify(pins, null, 2) + "\n");
  console.log(
    `${role}: ${
      previous?.version ?? "(new)"
    } -> ${version} (${PLATFORMS.length} platforms)`,
  );
  console.log(
    "next: cargo test -p integration_tests --test integration -- lsp_tsgo",
  );
}

await main();
