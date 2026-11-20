// Copyright 2018-2026 the Deno authors. MIT license.

// The `--no-hosts` forwarding proxy.
//
// The WPT server is reached by the names `web-platform.test`,
// `not-web-platform.test` and their subdomains (`www1.web-platform.test`, ...),
// which normally come from `/etc/hosts` (`wpt.ts setup`, which needs root).
// With `--no-hosts` the runner starts this proxy on a loopback port instead and
// hands it to the test processes as `HTTP_PROXY` / `HTTPS_PROXY`: it sends
// those names to 127.0.0.1 and every other name to wherever it resolves.
//
// It speaks just enough HTTP/1.1 for that: `CONNECT host:port` (how a client
// reaches an `https:` origin through a proxy) becomes a raw TCP tunnel, and an
// absolute-form request (`GET http://host:port/path`) is rewritten to origin
// form, sent on a fresh connection with `Connection: close`, and its response
// relayed as is. It never looks inside TLS and never changes a body.

/** Whether `hostname` is one of the WPT server's names. */
export function isWptHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/\.$/, "");
  return host === "web-platform.test" ||
    host.endsWith(".web-platform.test") ||
    host === "not-web-platform.test" ||
    host.endsWith(".not-web-platform.test");
}

/** Where a connection to `hostname` really goes. */
function mapHost(hostname: string): string {
  return isWptHost(hostname) ? "127.0.0.1" : hostname;
}

export interface WptProxy {
  /** `http://127.0.0.1:<port>`, for `HTTP_PROXY` / `HTTPS_PROXY`. */
  url: string;
  /** An HTTP client for the runner's own requests to the WPT server. */
  client: Deno.HttpClient;
  close(): void;
}

const HEAD_LIMIT = 64 * 1024;

/** Starts the proxy on a free loopback port. */
export function startWptProxy(): WptProxy {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const { port } = listener.addr as Deno.NetAddr;
  const url = `http://127.0.0.1:${port}`;
  const conns = new Set<Deno.Conn>();

  (async () => {
    try {
      for await (const conn of listener) {
        conns.add(conn);
        handle(conn, conns)
          .catch(() => {})
          .finally(() => {
            conns.delete(conn);
            closeQuietly(conn);
          });
      }
    } catch {
      // The listener was closed.
    }
  })();

  const client = Deno.createHttpClient({ proxy: { url } });
  return {
    url,
    client,
    close() {
      try {
        listener.close();
      } catch {
        // Already closed.
      }
      for (const conn of conns) closeQuietly(conn);
      conns.clear();
      client.close();
    },
  };
}

function closeQuietly(conn: Deno.Conn) {
  try {
    conn.close();
  } catch {
    // Already closed.
  }
}

/** Bytes as a string, one char per byte (HTTP heads are ISO-8859-1). */
function bytesToLatin1(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 4096) {
    out += String.fromCharCode(...bytes.subarray(i, i + 4096));
  }
  return out;
}

function latin1ToBytes(text: string): Uint8Array {
  const out = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) out[i] = text.charCodeAt(i) & 0xff;
  return out;
}

async function writeAll(conn: Deno.Conn, data: Uint8Array) {
  let written = 0;
  while (written < data.length) {
    written += await conn.write(data.subarray(written));
  }
}

/** The index of the `\r\n\r\n` that ends a message head, or -1. */
function headEnd(bytes: Uint8Array, from: number): number {
  for (let i = Math.max(0, from - 3); i + 3 < bytes.length; i++) {
    if (
      bytes[i] === 13 && bytes[i + 1] === 10 && bytes[i + 2] === 13 &&
      bytes[i + 3] === 10
    ) {
      return i;
    }
  }
  return -1;
}

/**
 * Reads a message head (through its blank line). Returns the head and the
 * bytes read past it, or `null` if the peer closed or the head is too long.
 */
async function readHead(
  conn: Deno.Conn,
): Promise<{ head: string; rest: Uint8Array } | null> {
  let buffer = new Uint8Array(0);
  const chunk = new Uint8Array(16 * 1024);
  while (buffer.length <= HEAD_LIMIT) {
    const n = await conn.read(chunk);
    if (n === null) return null;
    const next = new Uint8Array(buffer.length + n);
    next.set(buffer);
    next.set(chunk.subarray(0, n), buffer.length);
    const searchFrom = buffer.length;
    buffer = next;
    const end = headEnd(buffer, searchFrom);
    if (end !== -1) {
      return {
        head: bytesToLatin1(buffer.subarray(0, end + 4)),
        rest: buffer.slice(end + 4),
      };
    }
  }
  return null;
}

/** `host:port` (or `[v6]:port`) into its parts. */
function splitHostPort(
  target: string,
  defaultPort: number,
): { hostname: string; port: number } {
  if (target.startsWith("[")) {
    const close = target.indexOf("]");
    const hostname = target.slice(1, close);
    const port = target.slice(close + 2);
    return { hostname, port: port ? Number(port) : defaultPort };
  }
  const colon = target.lastIndexOf(":");
  if (colon === -1) return { hostname: target, port: defaultPort };
  return {
    hostname: target.slice(0, colon),
    port: Number(target.slice(colon + 1)),
  };
}

const HOP_BY_HOP =
  /^(connection|proxy-connection|keep-alive|proxy-authorization|proxy-authenticate)\s*:/i;

/** A head's header lines without the hop-by-hop ones, plus `Connection: close`. */
function withConnectionClose(firstLine: string, head: string): string {
  const lines = head.split("\r\n").slice(1).filter((line) =>
    line !== "" && !HOP_BY_HOP.test(line)
  );
  lines.push("Connection: close");
  return `${firstLine}\r\n${lines.join("\r\n")}\r\n\r\n`;
}

async function relay(from: Deno.Conn, to: Deno.Conn) {
  try {
    await from.readable.pipeTo(to.writable, { preventClose: true });
  } catch {
    // A side closed or reset; the other direction ends too.
  }
  try {
    await to.closeWrite();
  } catch {
    // Already closed.
  }
}

async function respond(conn: Deno.Conn, status: string) {
  await writeAll(
    conn,
    latin1ToBytes(
      `HTTP/1.1 ${status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`,
    ),
  );
}

async function handle(conn: Deno.Conn, conns: Set<Deno.Conn>) {
  const request = await readHead(conn);
  if (request === null) return;
  const firstLine = request.head.slice(0, request.head.indexOf("\r\n"));
  const [method, target, version] = firstLine.split(" ");
  if (!method || !target || !version) {
    await respond(conn, "400 Bad Request");
    return;
  }

  if (method.toUpperCase() === "CONNECT") {
    const { hostname, port } = splitHostPort(target, 443);
    let upstream: Deno.Conn;
    try {
      upstream = await Deno.connect({ hostname: mapHost(hostname), port });
    } catch {
      await respond(conn, "502 Bad Gateway");
      return;
    }
    conns.add(upstream);
    try {
      await writeAll(
        conn,
        latin1ToBytes("HTTP/1.1 200 Connection Established\r\n\r\n"),
      );
      if (request.rest.length > 0) await writeAll(upstream, request.rest);
      await Promise.all([relay(conn, upstream), relay(upstream, conn)]);
    } finally {
      conns.delete(upstream);
      closeQuietly(upstream);
    }
    return;
  }

  let url: URL;
  try {
    url = new URL(target);
  } catch {
    await respond(conn, "400 Bad Request");
    return;
  }
  if (url.protocol !== "http:") {
    await respond(conn, "400 Bad Request");
    return;
  }
  let upstream: Deno.Conn;
  try {
    upstream = await Deno.connect({
      hostname: mapHost(url.hostname.replace(/^\[|\]$/g, "")),
      port: url.port ? Number(url.port) : 80,
    });
  } catch {
    await respond(conn, "502 Bad Gateway");
    return;
  }
  conns.add(upstream);
  try {
    const requestHead = withConnectionClose(
      `${method} ${url.pathname}${url.search} ${version}`,
      request.head,
    );
    await writeAll(upstream, latin1ToBytes(requestHead));
    if (request.rest.length > 0) await writeAll(upstream, request.rest);
    // The request body (if any) keeps flowing while the response comes back.
    const upload = relay(conn, upstream);
    const response = await readHead(upstream);
    if (response !== null) {
      // `Connection: close` on the response too, so the client opens a new
      // connection (to the proxy) for its next request.
      const statusLine = response.head.slice(0, response.head.indexOf("\r\n"));
      await writeAll(
        conn,
        latin1ToBytes(withConnectionClose(statusLine, response.head)),
      );
      if (response.rest.length > 0) await writeAll(conn, response.rest);
      await relay(upstream, conn);
    }
    // The response is done; closing the client connection (the caller does)
    // ends whatever is still being read from it.
    void upload;
  } finally {
    conns.delete(upstream);
    closeQuietly(upstream);
  }
}
