// Copyright 2018-2026 the Deno authors. MIT license.

// `navigator.mediaDevices` (Media Capture and Streams): `MediaDevices`,
// `MediaDeviceInfo`, `InputDeviceInfo` and `OverconstrainedError`.
//
// The runtime has no camera or microphone of its own, so there are no
// devices: `enumerateDevices()` is empty and `getUserMedia()` rejects with a
// NotFoundError. Under `DENO_FAKE_MEDIA_DEVICES=1` (Chrome's
// `--use-fake-device-for-media-stream`) there are two fake ones, a camera
// that shows a moving test card (640x480 at 30 fps) and a microphone that
// plays a tone (48 kHz, mono or stereo). Their tracks are real
// `MediaStreamTrack`s: added to a peer connection, they send that media in the
// codec it negotiated (pre-encoded clips, `fake_media.rs`).

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, primordials } = __bootstrap;
const {
  op_webrtc_fake_media_devices,
  op_webrtc_sender_write_fake,
} = core.ops;
const {
  ArrayIsArray,
  ArrayPrototypeIncludes,
  ArrayPrototypeMap,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  MathFloor,
  MathMax,
  MathMin,
  NumberIsFinite,
  ObjectFreeze,
  ObjectKeys,
  PromiseReject,
  PromiseResolve,
  SafeSet,
  SafeWeakMap,
  SetPrototypeDelete,
  SetPrototypeForEach,
  String,
  Symbol,
  SymbolFor,
  TypeError,
  WeakMapPrototypeGet,
  WeakMapPrototypeSet,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { defineEventHandler, EventTarget } = core.loadExtScript(
  "ext:deno_web/02_event.js",
);
const { clearInterval, setInterval, unrefTimer } = core.loadExtScript(
  "ext:deno_web/02_timers.js",
);
const { performance } = core.loadExtScript("ext:deno_web/15_performance.js");
const webrtc = core.loadExtScript("ext:deno_webrtc/01_webrtc.js");
const { MediaStream, MediaStreamTrack } = webrtc;
const {
  illegalConstructorKey,
  _rid,
  _source,
  _pc,
  _state,
  _trackState,
  _transform,
  eachFedSender,
} = webrtc.mediaInternals;
let encodedTransformMod;
const loadEncodedTransform = () =>
  encodedTransformMod ??
    (encodedTransformMod = core.loadExtScript(
      "ext:deno_webrtc/03_encoded_transform.js",
    ));

// --- OverconstrainedError ----------------------------------------------------

class OverconstrainedError extends DOMException {
  #constraint;

  constructor(constraint, message = "") {
    const prefix = "Failed to construct 'OverconstrainedError'";
    webidl.requiredArguments(arguments.length, 1, prefix);
    constraint = webidl.converters.DOMString(constraint, prefix, "Argument 1");
    message = webidl.converters.DOMString(message, prefix, "Argument 2");
    super(message, "OverconstrainedError");
    this.#constraint = constraint;
  }

  get constraint() {
    if (!(#constraint in this)) {
      throw new TypeError("Illegal invocation");
    }
    return this.#constraint;
  }
}
webidl.configureInterface(OverconstrainedError);

// --- MediaDeviceInfo / InputDeviceInfo ---------------------------------------

const _info = Symbol("[[info]]");

class MediaDeviceInfo {
  constructor(key = undefined, info = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    this[_info] = info;
    this[webidl.brand] = webidl.brand;
  }

  get deviceId() {
    webidl.assertBranded(this, MediaDeviceInfoPrototype);
    return this[_info].deviceId;
  }
  get kind() {
    webidl.assertBranded(this, MediaDeviceInfoPrototype);
    return this[_info].kind;
  }
  get label() {
    webidl.assertBranded(this, MediaDeviceInfoPrototype);
    return this[_info].label;
  }
  get groupId() {
    webidl.assertBranded(this, MediaDeviceInfoPrototype);
    return this[_info].groupId;
  }
  toJSON() {
    webidl.assertBranded(this, MediaDeviceInfoPrototype);
    const { deviceId, kind, label, groupId } = this[_info];
    return { deviceId, kind, label, groupId };
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    // A prototype or an `Object.create()` object has no internal state.
    if (this[webidl.brand] !== webidl.brand) {
      return `${this.constructor.name} ${inspect({}, inspectOptions)}`;
    }
    const { deviceId, kind, label, groupId } = this[_info];
    return `${this.constructor.name} ${
      inspect({ deviceId, kind, label, groupId }, inspectOptions)
    }`;
  }
}
webidl.configureInterface(MediaDeviceInfo);
const MediaDeviceInfoPrototype = MediaDeviceInfo.prototype;

class InputDeviceInfo extends MediaDeviceInfo {
  getCapabilities() {
    webidl.assertBranded(this, InputDeviceInfoPrototype);
    return capabilitiesOf(this[_info].device);
  }
}
webidl.configureInterface(InputDeviceInfo);
const InputDeviceInfoPrototype = InputDeviceInfo.prototype;

// --- the fake devices --------------------------------------------------------

// Each constrainable property of a device: a numeric range, a set of
// booleans or a set of strings, and the value it takes by default.
const GROUP_ID = "deno-fake-media-devices";
const FAKE_DEVICES = ObjectFreeze([
  {
    kind: "audioinput",
    trackKind: "audio",
    deviceId: "fake-microphone",
    label: "Fake Microphone",
    properties: {
      __proto__: null,
      sampleRate: { range: [48000, 48000], default: 48000 },
      sampleSize: { range: [16, 16], default: 16 },
      channelCount: { range: [1, 2], default: 1, integer: true },
      latency: { range: [0.01, 0.01], default: 0.01 },
      echoCancellation: { values: [true, false], default: true },
      autoGainControl: { values: [true, false], default: true },
      noiseSuppression: { values: [true, false], default: true },
    },
  },
  {
    kind: "videoinput",
    trackKind: "video",
    deviceId: "fake-camera",
    label: "Fake Camera",
    properties: {
      __proto__: null,
      width: { range: [640, 640], default: 640, integer: true },
      height: { range: [480, 480], default: 480, integer: true },
      aspectRatio: { range: [4 / 3, 4 / 3], default: 4 / 3 },
      frameRate: { range: [30, 30], default: 30 },
      facingMode: { values: [], default: undefined },
      resizeMode: { values: ["none"], default: "none" },
    },
  },
]);

const KINDS = ["audio", "video"];

const SUPPORTED_CONSTRAINTS = [
  "width",
  "height",
  "aspectRatio",
  "frameRate",
  "facingMode",
  "resizeMode",
  "sampleRate",
  "sampleSize",
  "echoCancellation",
  "autoGainControl",
  "noiseSuppression",
  "latency",
  "channelCount",
  "deviceId",
  "groupId",
];

function devices() {
  return op_webrtc_fake_media_devices() ? FAKE_DEVICES : [];
}

function capabilitiesOf(device) {
  const out = { deviceId: device.deviceId, groupId: GROUP_ID };
  const keys = ObjectKeys(device.properties);
  for (let i = 0; i < keys.length; ++i) {
    const p = device.properties[keys[i]];
    if (p.range !== undefined) {
      out[keys[i]] = { min: p.range[0], max: p.range[1] };
    } else {
      out[keys[i]] = ArrayPrototypeSlice(p.values);
    }
  }
  return out;
}

// --- constraints ------------------------------------------------------------
//
// A track's settings: for each property, an exact / min / max is required (a
// device that can't meet it fails with OverconstrainedError naming it), and a
// bare value or `ideal` is what the device comes as close to as it can. The
// `advanced` sets are taken in order, each only if the device meets all of it.

function isObject(v) {
  return v !== null && typeof v === "object";
}

// A constraint as { exact, min, max, ideal }; a bare value is ideal, or exact
// inside an `advanced` set.
function normalize(value, advanced) {
  if (isObject(value) && !ArrayIsArray(value)) {
    return {
      exact: value.exact,
      min: value.min,
      max: value.max,
      ideal: value.ideal,
    };
  }
  return advanced
    ? { exact: value, min: undefined, max: undefined, ideal: undefined }
    : { exact: undefined, min: undefined, max: undefined, ideal: value };
}

function asList(v) {
  if (v === undefined) return undefined;
  return ArrayIsArray(v) ? ArrayPrototypeMap(v, String) : [String(v)];
}

// What a device can take for one property under one constraint: the
// remaining numeric range, or the allowed values; null when it can't.
function narrow(device, name, c, current) {
  if (name === "deviceId" || name === "groupId") {
    const id = name === "deviceId" ? device.deviceId : GROUP_ID;
    const exact = asList(c.exact);
    if (exact !== undefined && !ArrayPrototypeIncludes(exact, id)) return null;
    return current;
  }
  const p = device.properties[name];
  if (p === undefined) return current; // Not a property of this device.
  if (p.range !== undefined) {
    let lo = current[0];
    let hi = current[1];
    const num = (v) => (typeof v === "number" ? v : undefined);
    const exact = num(c.exact);
    const min = num(c.min);
    const max = num(c.max);
    if (exact !== undefined) {
      lo = MathMax(lo, exact);
      hi = MathMin(hi, exact);
    }
    if (min !== undefined) lo = MathMax(lo, min);
    if (max !== undefined) hi = MathMin(hi, max);
    // Small slack for values such as an aspect ratio of 4 / 3.
    return lo <= hi + 1e-9 ? [lo, MathMax(lo, hi)] : null;
  }
  const exact = p.values.length > 0 && typeof p.values[0] === "boolean"
    ? (typeof c.exact === "boolean" ? [c.exact] : undefined)
    : asList(c.exact);
  if (exact === undefined) return current;
  const left = [];
  for (let i = 0; i < current.length; ++i) {
    if (ArrayPrototypeIncludes(exact, current[i])) {
      ArrayPrototypePush(left, current[i]);
    }
  }
  return left.length > 0 ? left : null;
}

function initialRanges(device) {
  const ranges = { __proto__: null };
  const keys = ObjectKeys(device.properties);
  for (let i = 0; i < keys.length; ++i) {
    const p = device.properties[keys[i]];
    ranges[keys[i]] = ArrayPrototypeSlice(
      p.range !== undefined ? p.range : p.values,
    );
  }
  ranges.deviceId = true;
  ranges.groupId = true;
  return ranges;
}

// Applies one constraint set to `ranges`: the name of the first property it
// can't meet, or null (`ranges` is updated only when all of it is met).
function applySet(device, ranges, set, advanced) {
  const next = { __proto__: null, ...ranges };
  const names = ObjectKeys(set);
  for (let i = 0; i < names.length; ++i) {
    const name = names[i];
    if (name === "advanced" || set[name] === undefined) continue;
    if (!ArrayPrototypeIncludes(SUPPORTED_CONSTRAINTS, name)) continue;
    const c = normalize(set[name], advanced);
    const r = narrow(device, name, c, next[name]);
    if (r === null) return name;
    if (r !== undefined) next[name] = r;
  }
  const keys = ObjectKeys(next);
  for (let i = 0; i < keys.length; ++i) ranges[keys[i]] = next[keys[i]];
  return null;
}

// The settings `constraints` select on `device`: { settings } or { failed }.
function selectSettings(device, constraints) {
  const ranges = initialRanges(device);
  const set = isObject(constraints) ? constraints : {};
  const failed = applySet(device, ranges, set, false);
  if (failed !== null) return { failed };
  const advanced = ArrayIsArray(set.advanced) ? set.advanced : [];
  for (let i = 0; i < advanced.length; ++i) {
    if (isObject(advanced[i])) applySet(device, ranges, advanced[i], true);
  }
  const settings = { deviceId: device.deviceId, groupId: GROUP_ID };
  const keys = ObjectKeys(device.properties);
  for (let i = 0; i < keys.length; ++i) {
    const name = keys[i];
    const p = device.properties[name];
    const r = ranges[name];
    const ideal = set[name] === undefined
      ? undefined
      : normalize(set[name], false).ideal;
    if (p.range !== undefined) {
      let v = typeof ideal === "number" && NumberIsFinite(ideal)
        ? ideal
        : p.default;
      v = MathMin(MathMax(v, r[0]), r[1]);
      if (p.integer) v = MathMax(r[0], MathMin(r[1], v | 0));
      settings[name] = v;
    } else if (r.length > 0) {
      const wanted = typeof ideal === "boolean" ? [ideal] : asList(ideal);
      let v = r[0];
      if (wanted !== undefined) {
        for (let j = 0; j < wanted.length; ++j) {
          if (ArrayPrototypeIncludes(r, wanted[j])) {
            v = wanted[j];
            break;
          }
        }
      } else if (ArrayPrototypeIncludes(r, p.default)) {
        v = p.default;
      }
      settings[name] = v;
    }
  }
  return { settings };
}

// Web IDL's MediaTrackConstraints: a dictionary (a copy, for getConstraints).
function convertConstraints(value, prefix) {
  if (value === undefined || value === null) return {};
  if (typeof value !== "object" && typeof value !== "function") {
    throw new TypeError(`${prefix}: constraints must be a dictionary`);
  }
  const out = {};
  for (let i = 0; i < SUPPORTED_CONSTRAINTS.length; ++i) {
    const name = SUPPORTED_CONSTRAINTS[i];
    const v = value[name];
    if (v !== undefined) out[name] = v;
  }
  if (value.advanced !== undefined) {
    if (!ArrayIsArray(value.advanced)) {
      throw new TypeError(`${prefix}: 'advanced' must be a sequence`);
    }
    out.advanced = ArrayPrototypeMap(
      value.advanced,
      (set) => convertConstraints(set, prefix),
    );
  }
  return out;
}

// --- capture sources ---------------------------------------------------------

// Each fake device track's settings and constraints.
const trackSettings = new SafeWeakMap();

function trackInfo(track) {
  return WeakMapPrototypeGet(trackSettings, track);
}

// The most frames one tick writes to catch up with the capture clock (half a
// second of media); a longer stall drops the rest, as a camera drops frames.
const MAX_CATCH_UP_MS = 500;

// A capture source: what a getUserMedia() call's track (and its clones)
// share. While some peer connection sends one of its tracks, it writes a
// frame of its clip to each such sender every frame interval; with none it
// stops, so an unused track costs nothing. The frames follow the capture
// clock, not the timer: a tick that comes late (a busy or slow host
// stretches an interval) writes every frame that fell due since the last
// one, so the media's RTP clock keeps pace with real time.
function createSource(device) {
  const source = {
    tracks: new SafeSet(),
    senders: new SafeSet(),
    ctor: MediaStreamTrack,
    device: null,
  };
  let timer = null;
  let frameMs = 0;
  // the capture clock: when the source started, and the frames written since
  let started = 0;
  let written = 0;
  const stop = () => {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
  const tick = () => {
    const due = MathFloor((performance.now() - started) / frameMs) - written;
    if (due <= 0) return;
    const cap = MathMax(1, MathFloor(MAX_CATCH_UP_MS / frameMs));
    // a stall longer than the cap: those frames are dropped
    written += due;
    for (let i = 0; i < MathMin(due, cap); ++i) {
      if (!writeFrame()) return;
    }
  };
  // One frame to each fed sender; false once the source has stopped.
  const writeFrame = () => {
    // Senders of closed connections are gone for good.
    SetPrototypeForEach(source.senders, (sender) => {
      if (sender[_pc][_state].closed) {
        SetPrototypeDelete(source.senders, sender);
      }
    });
    if (source.senders.size === 0 || source.tracks.size === 0) {
      stop();
      return false;
    }
    eachFedSender(source, (pc, key, sender) => {
      const track = firstLiveTrack(source);
      const stereo = track !== null &&
        trackInfo(track)?.settings.channelCount === 2;
      try {
        // A sender with an encoded transform hands its frames to the
        // transform's worker first.
        if (sender[_transform]) {
          return loadEncodedTransform().feedFake(sender, pc, key, stereo);
        }
        return op_webrtc_sender_write_fake(pc[_rid], key, stereo);
      } catch {
        // The connection is going away; the next tick skips it.
        return false;
      }
    });
    return true;
  };
  source.device = {
    capabilities: () => capabilitiesOf(device),
    settings: (track) => ({ ...trackInfo(track).settings }),
    constraints: (track) => structuredCopy(trackInfo(track).constraints),
    applyConstraints: (track, constraints) => {
      let converted;
      try {
        converted = convertConstraints(
          constraints,
          "Failed to execute 'applyConstraints' on 'MediaStreamTrack'",
        );
      } catch (error) {
        return PromiseReject(error);
      }
      const { settings, failed } = selectSettings(device, converted);
      if (failed !== undefined) {
        return PromiseReject(
          new OverconstrainedError(
            failed,
            "Constraints could not be satisfied",
          ),
        );
      }
      WeakMapPrototypeSet(trackSettings, track, {
        settings,
        constraints: converted,
      });
      return PromiseResolve();
    },
    cloned: (from, to) => {
      const info = trackInfo(from);
      WeakMapPrototypeSet(trackSettings, to, {
        settings: { ...info.settings },
        constraints: structuredCopy(info.constraints),
      });
    },
    ended: stop,
    attached: () => {
      if (timer !== null) return;
      frameMs = device.trackKind === "audio" ? 20 : 1000 / 30;
      started = performance.now();
      written = 0;
      timer = setInterval(tick, frameMs);
      unrefTimer(timer);
    },
  };
  return source;
}

function firstLiveTrack(source) {
  let found = null;
  SetPrototypeForEach(source.tracks, (t) => {
    if (found === null && t[_trackState].readyState === "live") found = t;
  });
  return found;
}

function structuredCopy(constraints) {
  const out = { ...constraints };
  if (ArrayIsArray(constraints.advanced)) {
    out.advanced = ArrayPrototypeMap(constraints.advanced, (s) => ({ ...s }));
  }
  return out;
}

// --- MediaDevices -----------------------------------------------------------

class MediaDevices extends EventTarget {
  constructor(key = undefined) {
    if (key !== illegalConstructorKey) {
      webidl.illegalConstructor();
    }
    super();
    this[webidl.brand] = webidl.brand;
  }

  enumerateDevices() {
    try {
      webidl.assertBranded(this, MediaDevicesPrototype);
    } catch (error) {
      return PromiseReject(error);
    }
    const list = devices();
    return PromiseResolve(ArrayPrototypeMap(
      list,
      (device) =>
        new InputDeviceInfo(illegalConstructorKey, {
          deviceId: device.deviceId,
          kind: device.kind,
          label: device.label,
          groupId: GROUP_ID,
          device,
        }),
    ));
  }

  getSupportedConstraints() {
    webidl.assertBranded(this, MediaDevicesPrototype);
    const out = {};
    for (let i = 0; i < SUPPORTED_CONSTRAINTS.length; ++i) {
      out[SUPPORTED_CONSTRAINTS[i]] = true;
    }
    return out;
  }

  getUserMedia(constraints = undefined) {
    const prefix = "Failed to execute 'getUserMedia' on 'MediaDevices'";
    let requested;
    try {
      webidl.assertBranded(this, MediaDevicesPrototype);
      if (constraints === null || constraints === undefined) constraints = {};
      if (
        typeof constraints !== "object" && typeof constraints !== "function"
      ) {
        throw new TypeError(`${prefix}: constraints must be a dictionary`);
      }
      // MediaStreamConstraints: (boolean or MediaTrackConstraints) each.
      requested = [];
      for (let k = 0; k < KINDS.length; ++k) {
        const kind = KINDS[k];
        const v = constraints[kind];
        if (v === undefined || v === false) continue;
        if (isObject(v) || typeof v === "function") {
          ArrayPrototypePush(requested, {
            kind,
            constraints: convertConstraints(v, prefix),
          });
        } else if (webidl.converters.boolean(v)) {
          ArrayPrototypePush(requested, { kind, constraints: {} });
        }
      }
      if (requested.length === 0) {
        throw new TypeError(
          `${prefix}: At least one of audio and video must be requested`,
        );
      }
    } catch (error) {
      return PromiseReject(error);
    }
    const tracks = [];
    for (let i = 0; i < requested.length; ++i) {
      const { kind, constraints: c } = requested[i];
      const list = devices();
      let device = null;
      let failed = null;
      for (let j = 0; j < list.length; ++j) {
        if (list[j].trackKind !== kind) continue;
        const r = selectSettings(list[j], c);
        if (r.failed !== undefined) {
          failed ??= r.failed;
          continue;
        }
        device = { device: list[j], settings: r.settings };
        break;
      }
      if (device === null) {
        return PromiseReject(
          failed !== null
            ? new OverconstrainedError(
              failed,
              "Constraints could not be satisfied",
            )
            : new DOMException("Requested device not found", "NotFoundError"),
        );
      }
      const source = createSource(device.device);
      const track = new MediaStreamTrack(illegalConstructorKey, {
        kind,
        label: device.device.label,
        source,
      });
      WeakMapPrototypeSet(trackSettings, track, {
        settings: device.settings,
        constraints: c,
      });
      ArrayPrototypePush(tracks, track);
    }
    return PromiseResolve(new MediaStream(tracks));
  }

  [SymbolFor("Deno.privateCustomInspect")](_inspect, _inspectOptions) {
    return "MediaDevices {}";
  }
}
defineEventHandler(MediaDevices.prototype, "devicechange");
webidl.configureInterface(MediaDevices);
const MediaDevicesPrototype = MediaDevices.prototype;

let mediaDevices;
function getMediaDevices() {
  return mediaDevices ??= new MediaDevices(illegalConstructorKey);
}

return {
  InputDeviceInfo,
  MediaDeviceInfo,
  MediaDevices,
  OverconstrainedError,
  getMediaDevices,
};
})();
