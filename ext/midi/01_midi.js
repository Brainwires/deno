// Copyright 2018-2026 the Deno authors. MIT license.

// Web MIDI (https://webaudio.github.io/web-midi-api/), MIDI 1.0 (denext,
// DENEXT.md). `navigator.requestMIDIAccess()` checks `--allow-midi`
// (`--allow-midi=sysex` for system exclusive access) and resolves to a
// `MIDIAccess` whose `inputs` / `outputs` maps hold this access's
// `MIDIInput` / `MIDIOutput` objects. The OS side is in `lib.rs`; this module
// keeps the port objects, their `state` / `connection`, the events, and
// validates what `MIDIOutput.send()` is given. The classes keep their state
// in private fields, so a getter called on another object throws a
// `TypeError` (the brand check).
//
// Hot-plug: each `MIDIAccess` polls the OS port list every 500 ms on an
// unref'd timer (so it never keeps the process alive). A port that goes away
// stays in its map as `"disconnected"`; one that was open becomes
// `"pending"` and is reopened when the port comes back. An open input keeps
// the process alive (it waits for messages, as a listener waits for
// connections) until `close()`; an open output doesn't.

(function () {
const { core, internals, primordials } = __bootstrap;
const {
  op_midi_input_recv,
  op_midi_open_input,
  op_midi_open_output,
  op_midi_output_clear,
  op_midi_output_send,
  op_midi_ports,
  op_midi_request_access,
} = core.ops;
const {
  ArrayPrototypePush,
  FunctionPrototypeCall,
  MapPrototypeEntries,
  MapPrototypeForEach,
  MapPrototypeGet,
  MapPrototypeGetSize,
  MapPrototypeHas,
  MapPrototypeKeys,
  MapPrototypeSet,
  MapPrototypeValues,
  NumberPrototypeToString,
  ObjectDefineProperty,
  ObjectPrototypeIsPrototypeOf,
  PromiseReject,
  PromiseResolve,
  SafeArrayIterator,
  SafeMap,
  SafeSet,
  SetPrototypeAdd,
  SetPrototypeHas,
  StringPrototypeToUpperCase,
  Symbol,
  SymbolFor,
  SymbolIterator,
  TypeError,
  TypedArrayPrototypeGetLength,
  TypedArrayPrototypeGetSymbolToStringTag,
  TypedArrayPrototypeSlice,
  Uint8Array,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { createFilteredInspectProxy } = core.loadExtScript(
  "ext:deno_web/01_console.js",
);
const { Event, EventTarget, defineEventHandler, setIsTrusted } = core
  .loadExtScript("ext:deno_web/02_event.js");
const { setInterval, unrefTimer } = core.loadExtScript(
  "ext:deno_web/02_timers.js",
);

const illegalConstructorKey = Symbol("illegalConstructorKey");
const POLL_INTERVAL_MS = 500;

// ---- message validation (MIDI 1.0) ---------------------------------------

// The length of a message with this status byte; 0 for one that starts no
// fixed-length message (0xF0, 0xF7) or is undefined (0xF4, 0xF5, 0xF9, 0xFD).
function messageLength(status) {
  if (status < 0x80) return 0;
  if (status < 0xC0) return 3;
  if (status < 0xE0) return 2;
  if (status < 0xF0) return 3;
  switch (status) {
    case 0xF1:
    case 0xF3:
      return 2;
    case 0xF2:
      return 3;
    case 0xF6:
    case 0xF8:
    case 0xFA:
    case 0xFB:
    case 0xFC:
    case 0xFE:
    case 0xFF:
      return 1;
    default:
      return 0;
  }
}

function invalid(prefix, reason) {
  return new TypeError(`${prefix}: ${reason}`);
}

function hex(byte) {
  return `0x${byte < 16 ? "0" : ""}${
    StringPrototypeToUpperCase(NumberPrototypeToString(byte, 16))
  }`;
}

// Checks that `data` is a sequence of complete MIDI 1.0 messages, each with
// its status byte (no running status). Real-time messages may appear
// anywhere, inside a system exclusive message too, as MIDI allows. A system
// exclusive message without `sysex` access is an InvalidAccessError.
function validateMessages(data, sysexEnabled, prefix) {
  const length = TypedArrayPrototypeGetLength(data);
  if (length === 0) {
    throw invalid(prefix, "the data contains no MIDI message");
  }
  let i = 0;
  while (i < length) {
    const status = data[i];
    if (status >= 0xF8) {
      if (messageLength(status) === 0) {
        throw invalid(prefix, `${hex(status)} is an undefined status byte`);
      }
      i++;
      continue;
    }
    if (status < 0x80) {
      throw invalid(
        prefix,
        `expected a status byte at index ${i}, found the data byte ${
          hex(status)
        } (running status is not allowed)`,
      );
    }
    if (status === 0xF0) {
      if (!sysexEnabled) {
        throw new DOMException(
          `${prefix}: System exclusive messages need a MIDIAccess requested with { sysex: true }`,
          "InvalidAccessError",
        );
      }
      i++;
      for (;;) {
        if (i >= length) {
          throw invalid(
            prefix,
            "the system exclusive message has no end (0xF7)",
          );
        }
        const byte = data[i];
        if (byte === 0xF7) {
          i++;
          break;
        }
        if (byte >= 0xF8 && messageLength(byte) === 1) {
          i++;
          continue;
        }
        if (byte >= 0x80) {
          throw invalid(
            prefix,
            `the status byte ${
              hex(byte)
            } at index ${i} interrupts a system exclusive message`,
          );
        }
        i++;
      }
      continue;
    }
    const len = messageLength(status);
    if (len === 0) {
      throw invalid(
        prefix,
        status === 0xF7
          ? `0xF7 at index ${i} ends no system exclusive message`
          : `${hex(status)} is an undefined status byte`,
      );
    }
    if (i + len > length) {
      throw invalid(
        prefix,
        `the message starting at index ${i} (${
          hex(status)
        }) needs ${len} bytes`,
      );
    }
    for (let j = 1; j < len; j++) {
      if (data[i + j] >= 0x80) {
        throw invalid(
          prefix,
          `expected a data byte at index ${i + j}, found ${hex(data[i + j])}`,
        );
      }
    }
    i += len;
  }
}

const toOctets = webidl.createSequenceConverter(webidl.converters.octet);

function toBytes(data, prefix) {
  if (TypedArrayPrototypeGetSymbolToStringTag(data) === "Uint8Array") {
    return TypedArrayPrototypeSlice(data);
  }
  return new Uint8Array(toOctets(data, prefix, "Argument 1"));
}

// ---- events ----------------------------------------------------------------

webidl.converters.MIDIMessageEventInit = webidl.createDictionaryConverter(
  "MIDIMessageEventInit",
  [
    {
      key: "bubbles",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    {
      key: "cancelable",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    {
      key: "composed",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    { key: "data", converter: webidl.converters.Uint8Array },
  ],
);

const _timeStamp = Symbol("[[timeStamp]]");

class MIDIMessageEvent extends Event {
  #data;
  [_timeStamp] = undefined;

  constructor(type, eventInitDict = undefined) {
    const prefix = "Failed to construct 'MIDIMessageEvent'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    type = webidl.converters.DOMString(type, prefix, "Argument 1");
    const init = webidl.converters.MIDIMessageEventInit(
      eventInitDict,
      prefix,
      "Argument 2",
    );
    super(type, init);
    this.#data = init.data ?? null;
  }

  get data() {
    return this.#data;
  }

  get timeStamp() {
    return this[_timeStamp] ?? super.timeStamp;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      createFilteredInspectProxy({
        object: this,
        evaluate: ObjectPrototypeIsPrototypeOf(MIDIMessageEventPrototype, this),
        keys: ["type", "data", "timeStamp"],
      }),
      inspectOptions,
    );
  }
}
webidl.configureInterface(MIDIMessageEvent);
const MIDIMessageEventPrototype = MIDIMessageEvent.prototype;

webidl.converters.MIDIConnectionEventInit = webidl.createDictionaryConverter(
  "MIDIConnectionEventInit",
  [
    {
      key: "bubbles",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    {
      key: "cancelable",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    {
      key: "composed",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
    { key: "port", converter: (v) => v },
  ],
);

class MIDIConnectionEvent extends Event {
  #port;

  constructor(type, eventInitDict = undefined) {
    const prefix = "Failed to construct 'MIDIConnectionEvent'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    type = webidl.converters.DOMString(type, prefix, "Argument 1");
    const init = webidl.converters.MIDIConnectionEventInit(
      eventInitDict,
      prefix,
      "Argument 2",
    );
    if (
      init.port !== undefined && init.port !== null &&
      !ObjectPrototypeIsPrototypeOf(MIDIPortPrototype, init.port)
    ) {
      throw new TypeError(
        `${prefix}: 'port' of MIDIConnectionEventInit is not of type 'MIDIPort'`,
      );
    }
    super(type, init);
    this.#port = init.port ?? null;
  }

  get port() {
    return this.#port;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      createFilteredInspectProxy({
        object: this,
        evaluate: ObjectPrototypeIsPrototypeOf(
          MIDIConnectionEventPrototype,
          this,
        ),
        keys: ["type", "port"],
      }),
      inspectOptions,
    );
  }
}
webidl.configureInterface(MIDIConnectionEvent);
const MIDIConnectionEventPrototype = MIDIConnectionEvent.prototype;

// ---- ports -----------------------------------------------------------------

const _init = Symbol("[[init]]");
const _setState = Symbol("[[setState]]");
const _open = Symbol("[[open]]");
const _release = Symbol("[[release]]");
const _rid = Symbol("[[rid]]");
const _read = Symbol("[[read]]");
const _update = Symbol("[[update]]");

class MIDIPort extends EventTarget {
  #access;
  #info;
  #state = "connected";
  #connection = "closed";
  #rid = null;

  constructor(key = null) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
  }

  [_init](access, info) {
    this.#access = access;
    this.#info = info;
  }

  get id() {
    return this.#info.id;
  }

  get manufacturer() {
    return this.#info.manufacturer ?? null;
  }

  get name() {
    return this.#info.name ?? null;
  }

  get type() {
    return this.#info.type;
  }

  get version() {
    return this.#info.version ?? null;
  }

  get state() {
    return this.#state;
  }

  get connection() {
    return this.#connection;
  }

  open() {
    try {
      this[_open]();
      return PromiseResolve(this);
    } catch (error) {
      return PromiseReject(error);
    }
  }

  close() {
    try {
      if (this.#connection !== "closed") {
        this[_release]();
        this.#changed(this.#state, "closed");
      }
      return PromiseResolve(this);
    } catch (error) {
      return PromiseReject(error);
    }
  }

  get [_rid]() {
    return this.#rid;
  }

  // Opens the OS port (synchronously). A disconnected port becomes
  // "pending" and opens when it comes back.
  [_open]() {
    if (this.#connection === "open" || this.#connection === "pending") {
      return;
    }
    if (this.#state === "disconnected") {
      this.#changed("disconnected", "pending");
      return;
    }
    this.#rid = this.#info.type === "input"
      ? op_midi_open_input(this.#info.id, this.#access.sysexEnabled)
      : op_midi_open_output(this.#info.id);
    this.#changed("connected", "open");
    if (this.#info.type === "input") {
      this[_read](this.#rid);
    }
  }

  [_release]() {
    if (this.#rid !== null) {
      core.tryClose(this.#rid);
      this.#rid = null;
    }
  }

  // Hot-plug: the port appeared or went away.
  [_setState](state) {
    if (state === this.#state) return;
    if (state === "disconnected") {
      const wasOpen = this.#connection === "open";
      this[_release]();
      this.#changed("disconnected", wasOpen ? "pending" : this.#connection);
      return;
    }
    if (this.#connection === "pending") {
      this.#state = "connected";
      this.#connection = "closed";
      try {
        this[_open]();
        return;
      } catch {
        // it can't be opened again (taken by another program): closed
      }
    }
    this.#changed("connected", this.#connection);
  }

  #changed(state, connection) {
    if (state === this.#state && connection === this.#connection) return;
    this.#state = state;
    this.#connection = connection;
    dispatchStateChange(this, this);
    dispatchStateChange(this.#access, this);
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      createFilteredInspectProxy({
        object: this,
        evaluate: ObjectPrototypeIsPrototypeOf(MIDIPortPrototype, this),
        keys: [
          "id",
          "manufacturer",
          "name",
          "type",
          "version",
          "state",
          "connection",
        ],
      }),
      inspectOptions,
    );
  }
}
defineEventHandler(MIDIPort.prototype, "statechange");
webidl.configureInterface(MIDIPort);
const MIDIPortPrototype = MIDIPort.prototype;

function dispatchStateChange(target, port) {
  const event = new MIDIConnectionEvent("statechange", { port });
  setIsTrusted(event, true);
  target.dispatchEvent(event);
}

class MIDIInput extends MIDIPort {
  constructor(key = null) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super(key);
  }

  // Delivers the input's messages until it is closed.
  async [_read](rid) {
    for (;;) {
      let batch;
      try {
        batch = await op_midi_input_recv(rid);
      } catch {
        return;
      }
      if (batch === null) return;
      for (const message of new SafeArrayIterator(batch)) {
        if (this[_rid] !== rid) return;
        const event = new MIDIMessageEvent("midimessage", {
          data: message.data,
        });
        event[_timeStamp] = message.timestamp;
        setIsTrusted(event, true);
        this.dispatchEvent(event);
      }
    }
  }
}
// Setting `onmidimessage` opens the port (Web MIDI, "implicitly open").
defineEventHandler(MIDIInput.prototype, "midimessage", (port) => {
  try {
    port[_open]();
  } catch {
    // as an explicit open() would reject; the handler stays set
  }
});
webidl.configureInterface(MIDIInput);

class MIDIOutput extends MIDIPort {
  #access;

  constructor(key = null) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super(key);
  }

  [_init](access, info) {
    super[_init](access, info);
    this.#access = access;
  }

  send(data, timestamp = 0) {
    const prefix = "Failed to execute 'send' on 'MIDIOutput'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    const bytes = toBytes(data, prefix);
    timestamp = webidl.converters.DOMHighResTimeStamp(
      timestamp,
      prefix,
      "Argument 2",
    );
    if (this.state === "disconnected") {
      throw new DOMException(
        `${prefix}: The port is disconnected`,
        "InvalidStateError",
      );
    }
    validateMessages(bytes, this.#access.sysexEnabled, prefix);
    if (this.connection !== "open") {
      // implicitly open
      this[_open]();
    }
    if (this[_rid] === null) {
      throw new DOMException(
        `${prefix}: The port is not open`,
        "InvalidStateError",
      );
    }
    op_midi_output_send(this[_rid], bytes, timestamp);
  }

  clear() {
    if (this[_rid] !== null) {
      op_midi_output_clear(this[_rid]);
    }
  }
}
webidl.configureInterface(MIDIOutput);

// ---- the maps --------------------------------------------------------------

const _map = Symbol("[[map]]");

function makeMapClass(name) {
  class PortMap {
    constructor(key = null) {
      if (key !== illegalConstructorKey) {
        webidl.illegalConstructor();
      }
      this[_map] = new SafeMap();
    }

    get size() {
      return MapPrototypeGetSize(this[_map]);
    }

    get(key) {
      return MapPrototypeGet(this[_map], `${key}`);
    }

    has(key) {
      return MapPrototypeHas(this[_map], `${key}`);
    }

    keys() {
      return MapPrototypeKeys(this[_map]);
    }

    values() {
      return MapPrototypeValues(this[_map]);
    }

    entries() {
      return MapPrototypeEntries(this[_map]);
    }

    forEach(callback, thisArg = undefined) {
      MapPrototypeForEach(
        this[_map],
        (value, key) =>
          FunctionPrototypeCall(callback, thisArg, value, key, this),
      );
    }

    [SymbolIterator]() {
      return MapPrototypeEntries(this[_map]);
    }

    [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
      return `${name} ${inspect(this[_map], inspectOptions)}`;
    }
  }
  ObjectDefineProperty(PortMap, "name", { __proto__: null, value: name });
  webidl.configureInterface(PortMap);
  return PortMap;
}

const MIDIInputMap = makeMapClass("MIDIInputMap");
const MIDIOutputMap = makeMapClass("MIDIOutputMap");

// ---- MIDIAccess ------------------------------------------------------------

class MIDIAccess extends EventTarget {
  #inputs;
  #outputs;
  #sysex;
  #software;

  constructor(key = null, sysex = false, software = false) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this.#inputs = new MIDIInputMap(illegalConstructorKey);
    this.#outputs = new MIDIOutputMap(illegalConstructorKey);
    this.#sysex = sysex;
    this.#software = software;
    this[_update](false);
    const timer = setInterval(() => {
      try {
        this[_update](true);
      } catch {
        // the next poll tries again
      }
    }, POLL_INTERVAL_MS);
    unrefTimer(timer);
  }

  get inputs() {
    return this.#inputs;
  }

  get outputs() {
    return this.#outputs;
  }

  get sysexEnabled() {
    return this.#sysex;
  }

  // Brings the maps up to date with the OS port list; with `notify`, a new
  // port fires "statechange" here.
  [_update](notify) {
    const seen = new SafeSet();
    const ports = op_midi_ports();
    for (const info of new SafeArrayIterator(ports)) {
      if (info.software && !this.#software && info.type === "output") {
        continue;
      }
      const map = info.type === "input" ? this.#inputs : this.#outputs;
      const key = info.id;
      SetPrototypeAdd(seen, `${info.type}:${key}`);
      const existing = MapPrototypeGet(map[_map], key);
      if (existing) {
        existing[_setState]("connected");
        continue;
      }
      const port = info.type === "input"
        ? new MIDIInput(illegalConstructorKey)
        : new MIDIOutput(illegalConstructorKey);
      port[_init](this, info);
      MapPrototypeSet(map[_map], key, port);
      if (notify) {
        dispatchStateChange(this, port);
      }
    }
    const gone = [];
    for (const map of new SafeArrayIterator([this.#inputs, this.#outputs])) {
      MapPrototypeForEach(map[_map], (port, key) => {
        if (!SetPrototypeHas(seen, `${port.type}:${key}`)) {
          ArrayPrototypePush(gone, port);
        }
      });
    }
    for (const port of new SafeArrayIterator(gone)) {
      port[_setState]("disconnected");
    }
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      createFilteredInspectProxy({
        object: this,
        evaluate: ObjectPrototypeIsPrototypeOf(MIDIAccessPrototype, this),
        keys: ["inputs", "outputs", "sysexEnabled"],
      }),
      inspectOptions,
    );
  }
}
defineEventHandler(MIDIAccess.prototype, "statechange");
webidl.configureInterface(MIDIAccess);
const MIDIAccessPrototype = MIDIAccess.prototype;

webidl.converters.MIDIOptions = webidl.createDictionaryConverter(
  "MIDIOptions",
  [
    { key: "sysex", converter: webidl.converters.boolean, defaultValue: false },
    {
      key: "software",
      converter: webidl.converters.boolean,
      defaultValue: false,
    },
  ],
);

// `navigator.requestMIDIAccess(options)`.
function requestMIDIAccess(options = undefined) {
  try {
    const prefix = "Failed to execute 'requestMIDIAccess' on 'Navigator'";
    const { sysex, software } = webidl.converters.MIDIOptions(
      options,
      prefix,
      "Argument 1",
    );
    try {
      op_midi_request_access(sysex);
    } catch (error) {
      if (error?.name === "NotCapable") {
        // Web MIDI's refusal; the message names the flag that grants it
        throw new DOMException(error.message, "NotAllowedError");
      }
      throw error;
    }
    return PromiseResolve(
      new MIDIAccess(illegalConstructorKey, sysex, software),
    );
  } catch (error) {
    return PromiseReject(error);
  }
}

// for tests/unit/midi_test.ts, which can't reach a port everywhere
internals.midiValidateMessages = validateMessages;

return {
  MIDIAccess,
  MIDIConnectionEvent,
  MIDIInput,
  MIDIInputMap,
  MIDIMessageEvent,
  MIDIOutput,
  MIDIOutputMap,
  MIDIPort,
  requestMIDIAccess,
};
})();
