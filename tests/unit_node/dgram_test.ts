// Copyright 2018-2026 the Deno authors. MIT license.

import { assert, assertEquals } from "@std/assert";
import { execCode } from "../unit/test_util.ts";
import { createSocket, type Socket } from "node:dgram";

const listenPort = 4503;
const listenPort2 = 4504;

Deno.test("[node/dgram] udp ref and unref", {
  permissions: { read: true, run: true, net: true },
}, async () => {
  const { promise, resolve } = Promise.withResolvers<void>();

  const udpSocket = createSocket("udp4");
  udpSocket.bind(listenPort);

  udpSocket.unref();
  udpSocket.ref();

  let data;
  udpSocket.on("message", (buffer, _rinfo) => {
    data = Uint8Array.from(buffer);
    udpSocket.close();
  });
  udpSocket.on("close", () => {
    resolve();
  });

  const conn = await Deno.listenDatagram({
    port: listenPort2,
    transport: "udp",
  });
  await conn.send(new Uint8Array([0, 1, 2, 3]), {
    transport: "udp",
    port: listenPort,
    hostname: "127.0.0.1",
  });

  await promise;
  conn.close();
  assertEquals(data, new Uint8Array([0, 1, 2, 3]));
});

Deno.test("[node/dgram] udp unref", {
  permissions: { read: true, run: true, net: true },
}, async () => {
  const [statusCode, _output] = await execCode(`
      import { createSocket } from "node:dgram";
      const udpSocket = createSocket('udp4');
      udpSocket.bind(${listenPort2});
      // This should let the program to exit without waiting for the
      // udp socket to close.
      udpSocket.unref();
      udpSocket.on('message', (buffer, rinfo) => {
      });
    `);
  assertEquals(statusCode, 0);
});

Deno.test("[node/dgram] addMembership works", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const socket = createSocket("udp4");
  socket.bind(0, () => {
    try {
      socket.addMembership("224.0.0.114");
    } finally {
      socket.close();
    }
  });
  socket.on("close", () => resolve());
  await promise;
});

Deno.test("[node/dgram] addMembership accepts scoped IPv6 interface", async () => {
  // Regression test for https://github.com/denoland/deno/issues/34838.
  // A scoped IPv6 interface whose zone id names no existing interface must
  // resolve to the default interface (index 0) — exactly like passing no
  // interface at all — instead of being rejected with EINVAL. Node.js (via
  // libuv's `uv_ip6_addr`) behaves the same.
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  const socket = createSocket({ type: "udp6", ipv6Only: true });
  let bound = false;
  socket.on("error", (err) => {
    // IPv6 may be unavailable before binding; treat that as a skip.
    socket.close();
    if (bound) {
      reject(err);
    } else {
      resolve();
    }
  });
  socket.bind(0, () => {
    bound = true;
    try {
      // First join without an interface to determine whether IPv6 multicast is
      // available at all in this environment (some CI sandboxes have no
      // multicast-capable default interface). If it fails, skip the test.
      socket.addMembership("ff02::fb");
    } catch {
      socket.close(() => resolve());
      return;
    }
    try {
      // The scoped interface resolves to the same default interface as the
      // baseline join above, so it must be accepted too. On Unix a numeric
      // zone is resolved via `if_nametoindex` (-> 0 for non-names), which is
      // the reporter's exact "::%12" case; on Windows a numeric zone is a
      // literal index (`atoi`), so use a non-numeric unknown name there.
      const scopedIface = Deno.build.os === "windows"
        ? "::%nonexistent0"
        : "::%9999999";
      // Use a different multicast group so this is a fresh join rather than a
      // duplicate of the baseline join above.
      socket.addMembership("ff02::1:3", scopedIface);
      socket.close(() => resolve());
    } catch (err) {
      socket.close();
      reject(err);
    }
  });
  await promise;
});

Deno.test("[node/dgram] createSocket, reuseAddr option", async () => {
  const { promise, resolve } = Promise.withResolvers<string>();
  const socket0 = createSocket({ type: "udp4", reuseAddr: true });
  let socket1: Socket | undefined;
  socket0.bind(0, "0.0.0.0", () => {
    const port = socket0.address().port;
    socket1 = createSocket({ type: "udp4", reuseAddr: true });
    socket1.bind(port, "0.0.0.0", () => {
      const socket = createSocket({ type: "udp4" });
      socket.send("hello", port, "localhost", () => {
        socket.close();
      });
    });
    socket1.on("message", (msg) => {
      resolve(msg.toString());
    });
  });
  socket0.on("message", (msg) => {
    resolve(msg.toString());
  });
  assertEquals(await promise, "hello");
  socket0.close();
  socket1?.close();
});

Deno.test("[node/dgram] addMembership, setBroadcast, setMulticastTTL after bind", async () => {
  const { promise, resolve, reject } = Promise.withResolvers<void>();

  const socket = createSocket({ type: "udp4", reuseAddr: true });

  socket.on("error", (err) => {
    reject(err);
  });

  socket.bind(0, "0.0.0.0", () => {
    try {
      socket.addMembership("239.255.255.250");
      socket.setBroadcast(true);
      socket.setMulticastTTL(4);
      socket.dropMembership("239.255.255.250");
      resolve();
    } catch (err) {
      reject(err);
    } finally {
      socket.close();
    }
  });

  await promise;
});

Deno.test("[node/dgram] setTTL sets unicast TTL without error", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const socket = createSocket("udp4");
  socket.bind(0, () => {
    socket.setTTL(128);
    socket.close(() => resolve());
  });
  await promise;
});

Deno.test("[node/dgram] setTTL throws on invalid TTL", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const socket = createSocket("udp4");
  socket.bind(0, () => {
    try {
      socket.setTTL(0);
      assert(false, "should have thrown");
    } catch (e) {
      assert(e instanceof Error);
    }
    try {
      socket.setTTL(256);
      assert(false, "should have thrown");
    } catch (e) {
      assert(e instanceof Error);
    }
    socket.close(() => resolve());
  });
  await promise;
});

Deno.test("[node/dgram] setMulticastInterface sets interface without error", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const socket = createSocket("udp4");
  socket.bind(0, () => {
    socket.setMulticastInterface("0.0.0.0");
    socket.close(() => resolve());
  });
  await promise;
});

Deno.test("[node/dgram] addSourceSpecificMembership and dropSourceSpecificMembership", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const socket = createSocket("udp4");
  socket.bind(0, () => {
    socket.addSourceSpecificMembership("127.0.0.1", "232.1.1.1");
    socket.dropSourceSpecificMembership("127.0.0.1", "232.1.1.1");
    socket.close(() => resolve());
  });
  await promise;
});

Deno.test("[node/dgram] send checks destination permission", {
  permissions: { read: true, write: true, run: true, net: true },
}, async () => {
  // Verify that a subprocess with restricted --allow-net cannot send to
  // destinations outside the allowed set.
  const tempFile = Deno.makeTempFileSync({ suffix: ".ts" });
  Deno.writeTextFileSync(
    tempFile,
    `import dgram from "node:dgram";
const socket = dgram.createSocket("udp4");
socket.bind(0, "0.0.0.0", () => {
  socket.send("test", 9999, "127.0.0.1", (err) => {
    if (err) {
      console.log("SEND_BLOCKED:" + err.message);
    } else {
      console.log("SEND_ALLOWED");
    }
    socket.close();
  });
});
socket.on("error", (err) => {
  console.log("ERROR:" + err.message);
  socket.close();
});
`,
  );
  try {
    const { stdout, stderr } = await new Deno.Command(Deno.execPath(), {
      args: [
        "run",
        "--no-prompt",
        "--allow-net=0.0.0.0:0",
        "--allow-read=" + tempFile,
        tempFile,
      ],
      stdout: "piped",
      stderr: "piped",
    }).output();
    const output = new TextDecoder().decode(stdout) +
      new TextDecoder().decode(stderr);
    assert(
      !output.includes("SEND_ALLOWED"),
      `Send should have been blocked, but got: ${output}`,
    );
  } finally {
    Deno.removeSync(tempFile);
  }
});

Deno.test("[node/dgram] addMembership checks group permission", {
  permissions: { write: true, run: true, net: true },
}, async () => {
  const reservation = Deno.listenDatagram({
    hostname: "0.0.0.0",
    port: 0,
    transport: "udp",
  });
  assert(reservation.addr.transport === "udp");
  const port = reservation.addr.port;
  reservation.close();

  const tempFile = Deno.makeTempFileSync({ suffix: ".ts" });
  Deno.writeTextFileSync(
    tempFile,
    `import dgram from "node:dgram";
const socket = dgram.createSocket({ type: "udp4", reuseAddr: true });
socket.bind(${port}, "0.0.0.0", () => {
  try {
    socket.addMembership("224.0.0.114");
    console.log("JOINED");
    socket.dropMembership("224.0.0.114");
  } catch (error) {
    console.log("ADD_ERROR:" + (error.code ?? error.name));
  } finally {
    socket.close();
  }
});
`,
  );

  const run = async (allowNet: string) => {
    const { success, stdout, stderr } = await new Deno.Command(
      Deno.execPath(),
      {
        args: ["run", "--no-prompt", allowNet, tempFile],
        stdout: "piped",
        stderr: "piped",
      },
    ).output();
    return {
      success,
      output: new TextDecoder().decode(stdout) +
        new TextDecoder().decode(stderr),
    };
  };

  try {
    const denied = await run(`--allow-net=0.0.0.0:${port}`);
    assert(denied.success, denied.output);
    assert(
      denied.output.includes("ADD_ERROR:") &&
        !denied.output.includes("JOINED"),
      `Membership should have been blocked, but got: ${denied.output}`,
    );

    const allowed = await run(
      `--allow-net=0.0.0.0:${port},224.0.0.114:${port}`,
    );
    assert(allowed.success, allowed.output);
    assert(
      allowed.output.includes("JOINED"),
      `Membership should have been allowed, but got: ${allowed.output}`,
    );
  } finally {
    Deno.removeSync(tempFile);
  }
});

Deno.test("[node/dgram] large recvBufferSize and sendBufferSize do not throw", async () => {
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  const socket = createSocket({
    type: "udp4",
    recvBufferSize: 4194304,
    sendBufferSize: 4194304,
  });
  socket.on("error", (err) => {
    reject(err);
  });
  socket.bind(0, () => {
    socket.close(() => resolve());
  });
  await promise;
});

// denext: a datagram sent to the unspecified address goes to loopback, as on
// Linux (macOS sends it to the primary interface's address, which a host
// firewall may drop); a cluster worker's shared socket is announced to the
// primary bound to `0.0.0.0`.
Deno.test("[node/dgram] a send to 0.0.0.0 reaches a local socket", {
  permissions: { net: true },
}, async () => {
  const receiver = createSocket("udp4");
  const sender = createSocket("udp4");
  const { promise, resolve, reject } = Promise.withResolvers<string>();
  receiver.on("message", (msg) => resolve(msg.toString()));
  receiver.bind(0, "0.0.0.0", () => {
    sender.send("hi", receiver.address().port, "0.0.0.0", (err) => {
      if (err) reject(err);
    });
  });
  try {
    assertEquals(await promise, "hi");
  } finally {
    sender.close();
    receiver.close();
  }
});

// denext: `reusePort` is Linux's load-balancing SO_REUSEPORT; elsewhere the
// bind fails with ENOTSUP, as in Node (libuv).
Deno.test("[node/dgram] reusePort", {
  permissions: { net: true },
}, async () => {
  const options = { type: "udp4", reusePort: true } as const;
  const first = createSocket(options);
  const bound = Promise.withResolvers<NodeJS.ErrnoException | null>();
  first.once("error", (err) => bound.resolve(err));
  first.bind(0, "127.0.0.1", () => bound.resolve(null));
  const err = await bound.promise;
  if (Deno.build.os !== "linux") {
    assertEquals(err?.code, "ENOTSUP");
    first.close();
    return;
  }
  assertEquals(err, null);
  const second = createSocket(options);
  const { promise, resolve, reject } = Promise.withResolvers<void>();
  second.once("error", reject);
  second.bind(first.address().port, "127.0.0.1", () => resolve());
  try {
    await promise;
    assertEquals(second.address().port, first.address().port);
  } finally {
    second.close();
    first.close();
  }
});

// denext: libuv reads at most 32 datagrams for one readiness event and then
// returns to the loop. Every queued datagram used to be received from the
// microtask of the one before, so a socket that always had one queued kept
// the loop from its timers, immediates and other handles.
Deno.test("[node/dgram] queued datagrams are received in bursts", {
  permissions: { net: true },
}, async () => {
  const total = 100;
  const receiver = createSocket("udp4");
  await new Promise<void>((resolve) => receiver.bind(0, "127.0.0.1", resolve));
  const port = receiver.address().port;
  const sender = Deno.listenDatagram({
    transport: "udp",
    hostname: "127.0.0.1",
    port: 0,
  });
  try {
    let received = 0;
    let receivedBeforeImmediate = -1;
    const all = Promise.withResolvers<void>();
    receiver.on("message", () => {
      received++;
      if (received === 1) {
        // The check phase of the iteration that delivers this datagram.
        setImmediate(() => {
          receivedBeforeImmediate = received;
        });
      }
      if (received === total) all.resolve();
    });

    // Each datagram leaves during its `send()` call; all of them are queued
    // in the receiver's socket by the time this turn ends.
    const sends = [];
    const addr = { transport: "udp", hostname: "127.0.0.1", port } as const;
    for (let i = 0; i < total; i++) {
      sends.push(sender.send(new Uint8Array([i]), addr));
    }
    const end = performance.now() + 50;
    while (performance.now() < end);

    await Promise.all(sends);
    await all.promise;
    assertEquals(received, total);
    assert(
      receivedBeforeImmediate >= 1 && receivedBeforeImmediate <= 32,
      `${receivedBeforeImmediate} datagrams were received before the loop's check phase`,
    );
  } finally {
    sender.close();
    await new Promise<void>((resolve) => receiver.close(resolve));
  }
});
