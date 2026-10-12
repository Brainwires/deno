// A TCP proxy that delays every chunk by `delayMs` in each direction (so one
// round trip costs 2 * delayMs) and counts the client's turns: a chunk from
// the client after it has been handed data from the server since its last
// chunk starts a new turn. With the delay, everything a client pipelines is
// one turn, and each request it has to wait for an answer to before sending
// is another. Used by pg_round_trips.ts (the spec and the benchmark).
//
//   deno run --allow-net delay_proxy.ts <listen port> <target host:port> [delayMs]

export interface DelayProxy {
  /** The port it listens on (127.0.0.1). */
  port: number;
  /** Client turns since the last `reset()`, over every connection. */
  readonly turns: number;
  /** Bytes from clients since the last `reset()`. */
  readonly bytesUp: number;
  reset(): void;
  close(): Promise<void>;
}

interface Counters {
  turns: number;
  bytesUp: number;
}

function sleepUntil(t: number): Promise<void> {
  const wait = t - performance.now();
  return wait > 0
    ? new Promise((resolve) => setTimeout(resolve, wait))
    : Promise.resolve();
}

async function forward(
  src: Deno.Conn,
  dst: Deno.Conn,
  delayMs: number,
  onChunk: (chunk: Uint8Array) => void,
  onDelivered: () => void,
): Promise<void> {
  let tail = Promise.resolve();
  const buf = new Uint8Array(64 * 1024);
  try {
    while (true) {
      const n = await src.read(buf);
      if (n === null) break;
      const chunk = buf.slice(0, n);
      onChunk(chunk);
      const due = performance.now() + delayMs;
      tail = tail.then(async () => {
        await sleepUntil(due);
        let off = 0;
        while (off < chunk.length) off += await dst.write(chunk.subarray(off));
        onDelivered();
      });
    }
  } catch {
    // The other side went away.
  }
  await tail.catch(() => {});
  try {
    await dst.closeWrite();
  } catch {
    // Already closed.
  }
}

export function startDelayProxy(
  targetHost: string,
  targetPort: number,
  delayMs: number,
  listenPort = 0,
): DelayProxy {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: listenPort });
  const counters: Counters = { turns: 0, bytesUp: 0 };
  const conns = new Set<Deno.Conn>();
  const tasks = new Set<Promise<void>>();

  const serve = async (client: Deno.TcpConn) => {
    let server: Deno.TcpConn;
    try {
      server = await Deno.connect({ hostname: targetHost, port: targetPort });
    } catch {
      client.close();
      return;
    }
    // Without it, Nagle's algorithm holds a small chunk back until the
    // previous one is acknowledged, which a delayed ACK stretches to 40 ms.
    for (const c of [client, server] as Deno.TcpConn[]) c.setNoDelay(true);
    conns.add(client);
    conns.add(server);
    // Whether the client has been handed server data since its last chunk.
    let answered = true;
    await Promise.all([
      forward(client, server, delayMs, (chunk) => {
        counters.bytesUp += chunk.length;
        if (answered) counters.turns++;
        answered = false;
      }, () => {}),
      forward(server, client, delayMs, () => {}, () => {
        answered = true;
      }),
    ]);
    for (const c of [client, server]) {
      conns.delete(c);
      try {
        c.close();
      } catch {
        // Already closed.
      }
    }
  };

  const accepting = (async () => {
    try {
      for await (const client of listener) {
        const task = serve(client);
        tasks.add(task);
        task.finally(() => tasks.delete(task));
      }
    } catch {
      // Closed.
    }
  })();

  return {
    port: (listener.addr as Deno.NetAddr).port,
    get turns() {
      return counters.turns;
    },
    get bytesUp() {
      return counters.bytesUp;
    },
    reset() {
      counters.turns = 0;
      counters.bytesUp = 0;
    },
    async close() {
      listener.close();
      for (const c of conns) {
        try {
          c.close();
        } catch {
          // Already closed.
        }
      }
      await accepting;
      await Promise.allSettled([...tasks]);
    },
  };
}

if (import.meta.main) {
  const [listen, target, delay = "10"] = Deno.args;
  const [host, port] = target.split(":");
  const proxy = startDelayProxy(
    host,
    Number(port),
    Number(delay),
    Number(listen),
  );
  console.log(`127.0.0.1:${proxy.port} -> ${target}, ${delay} ms each way`);
}
