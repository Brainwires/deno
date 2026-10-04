// Copyright 2018-2026 the Deno authors. MIT license.
// Publishing a full-app update the way `Deno.desktop.updater` reads it
// (runtime/ops/desktop_update): the packaged app as a `.tar.gz` with one
// top-level entry, and the signed manifest envelope
// `{ "signed": "<payload JSON>", "signature": "<base64 r||s>" }`, ECDSA
// P-256 / SHA-256 over `"denext-app-update-v1\n" + signed`. A test-only copy
// of what denext's `denext desktop publish-update` writes, so the harness
// needs nothing but the stock deno.

export const SIGNATURE_DOMAIN = "denext-app-update-v1\n";

export interface PlatformEntry {
  url: string;
  sha256: string;
  size: number;
  kind: "bundle";
}

export interface Payload {
  schema: 1;
  app: string;
  version: string;
  minVersion?: string;
  platforms: Record<string, PlatformEntry>;
  releaseNotes?: string;
  publishedAt: string;
  /** RFC 3339; the runtime refuses the manifest after it (`expired`). */
  expiresAt?: string;
  /** Only grows; a lower one than the install accepted is `replayed`. */
  sequence?: number;
}

const enc = new TextEncoder();

export function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

/** A throwaway signing key pair: the private CryptoKey and the base64 SPKI
 * public key the app bakes in. */
export async function keyPair(): Promise<
  { key: CryptoKey; publicKey: string }
> {
  const pair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  const spki = new Uint8Array(
    await crypto.subtle.exportKey("spki", pair.publicKey),
  );
  return { key: pair.privateKey, publicKey: toBase64(spki) };
}

/** Sign `payload` (any JSON value, valid or not: the adversarial cases
 * sign broken payloads too). */
export async function sign(payload: unknown, key: CryptoKey): Promise<string> {
  const signed = JSON.stringify(payload);
  const sig = await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    key,
    enc.encode(SIGNATURE_DOMAIN + signed),
  );
  return JSON.stringify({ signed, signature: toBase64(new Uint8Array(sig)) });
}

export async function sha256(bytes: Uint8Array): Promise<string> {
  return Array.from(
    new Uint8Array(
      await crypto.subtle.digest("SHA-256", bytes as BufferSource),
    ),
  )
    .map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function gzip(raw: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(
    await new Response(
      new Blob([raw as BlobPart]).stream().pipeThrough(
        new CompressionStream("gzip"),
      ),
    ).arrayBuffer(),
  );
}

// --- tar ---------------------------------------------------------------

function octal(n: number, width: number): string {
  return n.toString(8).padStart(width - 1, "0") + "\0";
}

function paxRecord(k: string, v: string): string {
  const body = ` ${k}=${v}\n`;
  let len = enc.encode(body).length + 1;
  while (String(len).length + enc.encode(body).length !== len) len++;
  return `${len}${body}`;
}

/** One header block (`type` "0" file, "2" symlink, "5" dir, "x" pax). */
export function tarHeader(
  name: string,
  type: string,
  size: number,
  mode = 0o755,
  link = "",
): Uint8Array {
  const h = new Uint8Array(512);
  const put = (s: string, at: number, w: number) =>
    h.set(enc.encode(s).subarray(0, w), at);
  put(name, 0, 100);
  put(octal(mode & 0o7777, 8), 100, 8);
  put(octal(0, 8), 108, 8);
  put(octal(0, 8), 116, 8);
  put(octal(size, 12), 124, 12);
  put(octal(0, 12), 136, 12);
  put("        ", 148, 8);
  put(type, 156, 1);
  put(link, 157, 100);
  put("ustar\0", 257, 6);
  put("00", 263, 2);
  let sum = 0;
  for (const b of h) sum += b;
  put(sum.toString(8).padStart(6, "0") + "\0 ", 148, 8);
  return h;
}

function entry(
  name: string,
  type: string,
  size: number,
  mode: number,
  link = "",
): Uint8Array[] {
  const recs = (enc.encode(name).length > 100 ? paxRecord("path", name) : "") +
    (enc.encode(link).length > 100 ? paxRecord("linkpath", link) : "");
  const out: Uint8Array[] = [];
  if (recs) {
    const d = enc.encode(recs);
    out.push(
      tarHeader("././@PaxHeader", "x", d.length, 0o644),
      d,
      new Uint8Array((512 - d.length % 512) % 512),
    );
  }
  out.push(tarHeader(name, type, size, mode, link));
  return out;
}

/** A tar of hand-made entries [name, type, body, link?] (hostile archives). */
export function tar(
  entries: [string, string, Uint8Array, string?][],
): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const [n, t, body, link] of entries) {
    parts.push(
      ...entry(n, t, body.length, 0o755, link ?? ""),
      body,
      new Uint8Array((512 - body.length % 512) % 512),
    );
  }
  parts.push(new Uint8Array(1024));
  return concat(parts);
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((a, p) => a + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function join(...p: string[]) {
  return p.join("/");
}

function modeOf(info: Deno.FileInfo, p: string): number {
  if (info.mode !== null) return info.mode & 0o777;
  if (info.isDirectory) return 0o755;
  return /\.(exe|dll)$/i.test(p) ? 0o755 : 0o644;
}

async function* tarStream(
  artifact: string,
  top: string,
): AsyncGenerator<Uint8Array> {
  const walk = async function* (
    dir: string,
    rel: string,
  ): AsyncGenerator<[string, Deno.FileInfo, string]> {
    const names = (await Array.fromAsync(Deno.readDir(dir))).map((e) => e.name)
      .sort();
    for (const n of names) {
      const full = join(dir, n);
      const info = await Deno.lstat(full);
      const r = rel ? `${rel}/${n}` : n;
      yield [r, info, full];
      if (info.isDirectory) yield* walk(full, r);
    }
  };
  const rootInfo = await Deno.lstat(artifact);
  yield* entry(`${top}/`, "5", 0, modeOf(rootInfo, artifact));
  for await (const [rel, info, full] of walk(artifact, "")) {
    const name = `${top}/${rel}`;
    if (info.isDirectory) yield* entry(`${name}/`, "5", 0, modeOf(info, full));
    else if (info.isSymlink) {
      yield* entry(name, "2", 0, 0o777, await Deno.readLink(full));
    } else if (info.isFile) {
      yield* entry(name, "0", info.size, modeOf(info, full));
      const bytes = await Deno.readFile(full);
      yield bytes;
      yield new Uint8Array((512 - bytes.length % 512) % 512);
    }
  }
  yield new Uint8Array(1024);
}

/** Pack `artifact` (a directory: an app directory or a macOS bundle) into
 * `outFile`; its SHA-256 and size. */
export async function packArtifact(
  artifact: string,
  top: string,
  outFile: string,
): Promise<{ sha256: string; size: number }> {
  const gz = ReadableStream.from(tarStream(artifact, top)).pipeThrough(
    new CompressionStream("gzip") as unknown as TransformStream<
      Uint8Array,
      Uint8Array
    >,
  );
  const chunks: Uint8Array[] = [];
  for await (const c of gz) chunks.push(c);
  const bytes = concat(chunks);
  await Deno.writeFile(outFile, bytes);
  return { sha256: await sha256(bytes), size: bytes.length };
}
