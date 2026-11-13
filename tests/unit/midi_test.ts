// Copyright 2018-2026 the Deno authors. MIT license.

// Web MIDI (denext, ext/midi). The harness sets DENO_MIDI_VIRTUAL_LOOPBACK
// for this file (tests/unit/mod.rs), so where the OS has virtual ports
// (CoreMIDI on macOS, the ALSA sequencer on Linux) a "Deno Loopback" output
// sends back to the "Deno Loopback" input and the round trip runs on the
// real OS service. Windows (WinMM) has no virtual ports, and a Linux machine
// without the sequencer (a container, no snd-seq module) has no MIDI at all:
// there the loopback tests log why and return.

import {
  assert,
  assertEquals,
  assertInstanceOf,
  assertRejects,
  assertThrows,
} from "./test_util.ts";

const LOOPBACK = "Deno Loopback";

// deno-lint-ignore no-explicit-any
const internals = (Deno as any)[(Deno as any).internal];

function validate(data: number[], sysex = true) {
  // loads the module (the globals are lazy)
  void globalThis.MIDIOutput;
  internals.midiValidateMessages(new Uint8Array(data), sysex, "send");
}

Deno.test(function midiGlobals() {
  for (
    const name of [
      "MIDIAccess",
      "MIDIInputMap",
      "MIDIOutputMap",
      "MIDIPort",
      "MIDIInput",
      "MIDIOutput",
      "MIDIMessageEvent",
      "MIDIConnectionEvent",
    ]
  ) {
    assertEquals(
      typeof (globalThis as Record<string, unknown>)[name],
      "function",
    );
  }
  assertEquals(typeof navigator.requestMIDIAccess, "function");
  assertThrows(
    () => new (MIDIAccess as unknown as new () => unknown)(),
    TypeError,
  );
  assertThrows(
    () => new (MIDIPort as unknown as new () => unknown)(),
    TypeError,
  );
  assertThrows(
    () => new (MIDIInput as unknown as new () => unknown)(),
    TypeError,
  );
  assertThrows(
    () => new (MIDIOutput as unknown as new () => unknown)(),
    TypeError,
  );
  assert(Object.getPrototypeOf(MIDIInput.prototype) === MIDIPort.prototype);
  assert(Object.getPrototypeOf(MIDIOutput.prototype) === MIDIPort.prototype);
});

Deno.test(function midiMessageEventConstructor() {
  const data = new Uint8Array([0x90, 60, 100]);
  const event = new MIDIMessageEvent("midimessage", { data });
  assertInstanceOf(event, Event);
  assertEquals(event.type, "midimessage");
  assertEquals(event.data, data);
  assertEquals(new MIDIMessageEvent("midimessage").data, null);
  assertThrows(
    () =>
      new MIDIMessageEvent("midimessage", {
        data: [1, 2] as unknown as Uint8Array,
      }),
    TypeError,
  );
});

Deno.test(function midiConnectionEventConstructor() {
  const event = new MIDIConnectionEvent("statechange");
  assertEquals(event.port, null);
  assertThrows(
    () =>
      new MIDIConnectionEvent("statechange", {
        port: {} as unknown as MIDIPort,
      }),
    TypeError,
  );
});

Deno.test(function midiValidatesMessages() {
  // complete messages, any number of them
  validate([0x90, 60, 100]);
  validate([0x80, 60, 0, 0xC0, 5, 0xE0, 0, 0x40]);
  validate([0xF1, 0x10, 0xF2, 1, 2, 0xF3, 3, 0xF6]);
  validate([0xF8, 0xFA, 0xFB, 0xFC, 0xFE, 0xFF]);
  validate([0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7]);
  // real-time inside a system exclusive message is allowed
  validate([0xF0, 0x01, 0xF8, 0x02, 0xF7]);

  const invalid = (data: number[]) =>
    assertThrows(() => validate(data), TypeError);
  invalid([]);
  // running status
  invalid([0x90, 60, 100, 62, 100]);
  // a data byte first
  invalid([0x40]);
  // incomplete
  invalid([0x90, 60]);
  invalid([0xC0]);
  // a status byte where a data byte belongs
  invalid([0x90, 0x90, 100]);
  // undefined status bytes
  invalid([0xF4]);
  invalid([0xF5]);
  invalid([0xF9]);
  invalid([0xFD]);
  // system exclusive framing
  invalid([0xF7]);
  invalid([0xF0, 1, 2]);
  invalid([0xF0, 1, 0x90, 60, 100, 0xF7]);

  // system exclusive without sysex access
  assertThrows(
    () => validate([0xF0, 1, 0xF7], false),
    DOMException,
    "sysex",
  );
  validate([0x90, 60, 100], false);
});

Deno.test(async function midiPermissionQuery() {
  // the harness runs with -A, which grants both kinds
  assertEquals(
    (await Deno.permissions.query({ name: "midi" })).state,
    "granted",
  );
  assertEquals(
    (await Deno.permissions.query({ name: "midi", sysex: true })).state,
    "granted",
  );
  assert(Deno.permissions.names.includes("midi"));
});

Deno.test(
  { permissions: { midi: false } },
  async function midiRequestWithoutPermission() {
    await assertRejects(
      () => navigator.requestMIDIAccess(),
      DOMException,
      "--allow-midi",
    );
    const error = await navigator.requestMIDIAccess().catch((e) => e);
    assertEquals(error.name, "NotAllowedError");
  },
);

Deno.test(
  { permissions: { midi: ["devices"] } },
  async function midiSysexNeedsItsOwnGrant() {
    assertEquals(
      (await Deno.permissions.query({ name: "midi", sysex: true })).state,
      "prompt",
    );
    const error = await navigator.requestMIDIAccess({ sysex: true }).catch((
      e,
    ) => e);
    assertEquals(error.name, "NotAllowedError");
    assert(error.message.includes("--allow-midi=sysex"), error.message);
  },
);

// The loopback pair, or null (with the reason logged) where the OS has none.
async function loopback(
  options: MIDIOptions = { sysex: true },
): Promise<
  { access: MIDIAccess; input: MIDIInput; output: MIDIOutput } | null
> {
  let access: MIDIAccess;
  try {
    access = await navigator.requestMIDIAccess(options);
  } catch (error) {
    if ((error as Error).name === "NotSupportedError") {
      console.log(`no MIDI on this machine: ${(error as Error).message}`);
      return null;
    }
    throw error;
  }
  // CoreMIDI may list a new virtual endpoint a moment later; the access
  // polls every 500 ms
  const deadline = Date.now() + 3000;
  for (;;) {
    const input = [...access.inputs.values()].find((p) => p.name === LOOPBACK);
    const output = [...access.outputs.values()].find((p) =>
      p.name === LOOPBACK
    );
    if (input && output) return { access, input, output };
    if (Date.now() > deadline) {
      console.log(`no virtual MIDI ports on ${Deno.build.os}`);
      return null;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

function collect(input: MIDIInput) {
  const received: { data: number[]; timeStamp: number }[] = [];
  const waiters: (() => void)[] = [];
  input.onmidimessage = (event) => {
    received.push({ data: [...event.data!], timeStamp: event.timeStamp });
    waiters.splice(0).forEach((w) => w());
  };
  return {
    received,
    async until(count: number, ms = 5000) {
      const deadline = Date.now() + ms;
      while (received.length < count) {
        if (Date.now() > deadline) {
          throw new Error(
            `received ${received.length} of ${count} messages: ${
              JSON.stringify(received)
            }`,
          );
        }
        await new Promise<void>((resolve) => {
          waiters.push(resolve);
          setTimeout(resolve, 50);
        });
      }
    },
  };
}

// a MIDIAccess keeps its (unref'd) hot-plug poll for the process
Deno.test(
  { sanitizeOps: false, sanitizeResources: false },
  async function midiLoopbackRoundTrip() {
    const pair = await loopback();
    if (!pair) return;
    const { access, input, output } = pair;
    assert(access.sysexEnabled);
    assertEquals(input.type, "input");
    assertEquals(output.type, "output");
    assertEquals(input.state, "connected");
    assertEquals(input.connection, "closed");
    assert(access.inputs.get(input.id) === input);
    assert(access.outputs.has(output.id));

    const states: string[] = [];
    access.onstatechange = (e) =>
      states.push(`${e.port!.type}:${e.port!.connection}`);

    // setting onmidimessage opens the input
    const sink = collect(input);
    assertEquals(input.connection, "open");

    // send() opens the output
    output.send([0x90, 60, 100]);
    assertEquals(output.connection, "open");
    output.send(new Uint8Array([0xB0, 7, 127, 0xC0, 5]));
    const sysex = [0xF0, 0x7D];
    for (let i = 0; i < 1000; i++) sysex.push(i % 128);
    sysex.push(0xF7);
    output.send(sysex);
    output.send([0xF8]);
    await sink.until(5);
    assertEquals(sink.received.map((m) => m.data), [
      [0x90, 60, 100],
      [0xB0, 7, 127],
      [0xC0, 5],
      sysex,
      [0xF8],
    ]);
    for (const { timeStamp } of sink.received) {
      assert(timeStamp > 0 && timeStamp <= performance.now(), `${timeStamp}`);
    }
    assert(states.includes("input:open"), JSON.stringify(states));
    assert(states.includes("output:open"), JSON.stringify(states));

    // invalid data throws and sends nothing
    assertThrows(() => output.send([0x90, 60]), TypeError);
    assertThrows(() => output.send([60, 100]), TypeError);

    await input.close();
    await output.close();
    assertEquals(input.connection, "closed");
    assertEquals(output.connection, "closed");
  },
);

// a MIDIAccess keeps its (unref'd) hot-plug poll for the process
Deno.test(
  { sanitizeOps: false, sanitizeResources: false },
  async function midiLoopbackScheduledSend() {
    const pair = await loopback();
    if (!pair) return;
    const { input, output } = pair;
    const sink = collect(input);
    await output.open();
    const sentAt = performance.now();
    // later messages first: delivery follows the timestamps
    output.send([0x90, 62, 100], sentAt + 300);
    output.send([0x90, 61, 100], sentAt + 150);
    output.send([0x90, 60, 100]);
    // dropped by clear() below
    output.send([0x90, 63, 100], sentAt + 5000);
    await sink.until(3);
    output.clear();
    assertEquals(sink.received.map((m) => m.data[1]), [60, 61, 62]);
    // allow for clock granularity
    assert(
      sink.received[2].timeStamp >= sentAt + 280,
      `${sink.received[2].timeStamp - sentAt}`,
    );
    await new Promise((resolve) => setTimeout(resolve, 300));
    assertEquals(sink.received.length, 3);
    await input.close();
    await output.close();
  },
);

// a MIDIAccess keeps its (unref'd) hot-plug poll for the process
Deno.test(
  { sanitizeOps: false, sanitizeResources: false },
  async function midiLoopbackWithoutSysex() {
    const pair = await loopback({ sysex: false });
    if (!pair) return;
    const { access, input, output } = pair;
    assert(!access.sysexEnabled);
    assertThrows(
      () => output.send([0xF0, 1, 0xF7]),
      DOMException,
      "sysex",
    );
    // the input of a non-sysex access drops system exclusive messages; send
    // one from a sysex access to check
    const sysexPair = await loopback({ sysex: true });
    assert(sysexPair);
    const sink = collect(input);
    sysexPair.output.send([0xF0, 1, 2, 0xF7, 0x90, 64, 1]);
    await sink.until(1);
    await new Promise((resolve) => setTimeout(resolve, 100));
    assertEquals(sink.received.map((m) => m.data), [[0x90, 64, 1]]);
    await input.close();
    await sysexPair.output.close();
  },
);
