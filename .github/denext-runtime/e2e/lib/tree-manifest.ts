// Copyright 2018-2026 the Deno authors. MIT license.
// A Windows app's tree manifest (runtime/ops/desktop_update/treecheck.rs,
// DENEXT.md items 202 and 212): `{ "v": 1, "appId", "version", "files" }`
// over every regular file of the app but its executable, written into that
// executable as the RT_RCDATA resource DENEXT_TREE. A test-only copy of what
// denext's packaging writes (src/build/desktop-tree-manifest.ts, Brainwires/
// denext 2c0aa80da, with lib/pe-resources.ts beside this file), so the
// harness needs nothing but the stock deno.

import {
  type PeResourceTree,
  readPeResources,
  writePeResources,
} from "./pe-resources.ts";

export const TREE_RESOURCE = "DENEXT_TREE";
const RT_RCDATA = 10;
const LANG_EN_US = 1033;

export interface TreeFile {
  path: string;
  size: number;
  sha256: string;
}

export interface TreeManifest {
  v: 1;
  appId: string;
  version: string;
  files: TreeFile[];
}

async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(d, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** The manifest of app directory `dir` (executable `exe` left out), sorted. */
export async function treeManifest(
  dir: string,
  exe: string,
  appId: string,
  version: string,
): Promise<TreeManifest> {
  const files: TreeFile[] = [];
  const walk = async (d: string, rel: string) => {
    for await (const e of Deno.readDir(d)) {
      const p = `${d}/${e.name}`;
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isSymlink) throw new Error(`${p} is a symlink`);
      if (e.isDirectory) {
        await walk(p, r);
        continue;
      }
      if (!e.isFile || r === exe) continue;
      const bytes = await Deno.readFile(p);
      files.push({
        path: r,
        size: bytes.length,
        sha256: await sha256Hex(bytes),
      });
    }
  };
  await walk(dir, "");
  files.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  return { v: 1, appId, version, files };
}

/** `pe` with `manifest` as its DENEXT_TREE resource (replacing any). */
export function embedTreeManifest(
  pe: Uint8Array,
  manifest: TreeManifest,
): Uint8Array {
  const tree: PeResourceTree = readPeResources(pe);
  const rcdata = tree.get(RT_RCDATA) ?? new Map();
  for (const name of [...rcdata.keys()]) {
    if (typeof name === "string" && name.toUpperCase() === TREE_RESOURCE) {
      rcdata.delete(name);
    }
  }
  const data = new TextEncoder().encode(JSON.stringify(manifest));
  rcdata.set(TREE_RESOURCE, new Map([[LANG_EN_US, { data, codepage: 0 }]]));
  tree.set(RT_RCDATA, rcdata);
  return writePeResources(pe, tree);
}

/** The DENEXT_TREE manifest `pe` carries, or null. */
export function readTreeManifest(pe: Uint8Array): TreeManifest | null {
  const rcdata = readPeResources(pe).get(RT_RCDATA);
  for (const [name, langs] of rcdata ?? []) {
    if (typeof name === "string" && name.toUpperCase() === TREE_RESOURCE) {
      const [first] = langs.values();
      return JSON.parse(new TextDecoder().decode(first.data));
    }
  }
  return null;
}

/**
 * Write the manifest of app directory `dir` for `appId` / `version` into its
 * executable `exe` (as denext's packaging does before it signs), or the
 * manifest given.
 */
export async function stampTreeManifest(
  dir: string,
  exe: string,
  appId: string,
  version: string,
  manifest?: TreeManifest,
): Promise<TreeManifest> {
  const m = manifest ?? await treeManifest(dir, exe, appId, version);
  const p = `${dir}/${exe}`;
  await Deno.writeFile(p, embedTreeManifest(await Deno.readFile(p), m));
  return m;
}
