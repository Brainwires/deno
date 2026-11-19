// Copyright 2018-2026 the Deno authors. MIT license.
// @ts-check

// `<video>` and `<audio>` for the WPT runner's document environment, loaded
// right after dom_shim.js. A media element here is a sink for a
// `MediaStream` (`srcObject`) and nothing else: it plays no URL, decodes
// nothing and draws nothing. Everything it reports comes from the frames the
// runtime really received on the stream's remote tracks, read with the
// public `Deno.webrtc.readFrames(track)` API, or from the clock, as the
// specs define it for a MediaStream (mediacapture-main section 6, "MediaStreams
// in Media Elements"; HTML 4.8.11):
//
// | Member                           | What feeds it                                  |
// | -------------------------------- | ---------------------------------------------- |
// | `readyState`, `loadedmetadata`,  | the first received frame (for a `<video>`      |
// | `loadeddata`, `canplay`,         | whose stream has a video track: the first      |
// | `canplaythrough`, `duration`     | video frame whose bitstream named its size)    |
// | `videoWidth`, `videoHeight`,     | that frame's coded size (`frame.width` /       |
// | `resize`                         | `height`), and each later change of it         |
// | `play()` promise, `playing`      | resolves / fires once that frame arrived       |
// | autoplay (`autoplay` attribute)  | starts playing once that frame arrived         |
// | `currentTime`, `timeupdate`      | 0 until playing; then real time while the      |
// |                                  | element is potentially playing (spec: the      |
// |                                  | MediaStream timeline "increments linearly in   |
// |                                  | real time"), `timeupdate` every 200 ms of it   |
// | `requestVideoFrameCallback`      | each received video frame (its size, RTP       |
// |                                  | timestamp, delivery time)                      |
// | `getVideoPlaybackQuality()`      | the count of received video frames             |
//
// A local track (a capture device's, an EncodedMediaTrack) delivers nothing:
// the runtime renders no local media, so such an element never gets data.
// There are no pixels: `captureStream()` is absent and a canvas can't draw
// a video (dom_shim.js's `getContext()` is `null`); seeking is ignored.

(() => {
  "use strict";
  const define = Symbol.for("deno.wpt.defineElement");
  if (!("document" in globalThis) || "HTMLMediaElement" in globalThis) return;
  const HTMLElementBase = /** @type {any} */ (globalThis).HTMLElement;
  if (typeof HTMLElementBase !== "function") return;

  /** @returns {((track: MediaStreamTrack) => ReadableStream<any>) | null} */
  const readFramesApi = () => {
    const webrtc = /** @type {any} */ (globalThis).Deno?.webrtc;
    return typeof webrtc?.readFrames === "function"
      ? webrtc.readFrames.bind(webrtc)
      : null;
  };

  const HAVE_NOTHING = 0;
  const HAVE_METADATA = 1;
  const HAVE_ENOUGH_DATA = 4;
  const NETWORK_EMPTY = 0;
  const NETWORK_IDLE = 1;
  const TIMEUPDATE_MS = 200;

  /** "Queue a media element task": a later task of the event loop. */
  const queueTask = (/** @type {() => void} */ fn) => setTimeout(fn, 0);

  /** @param {number} id */
  const unref = (id) => {
    const deno = /** @type {any} */ (globalThis).Deno;
    if (typeof deno?.unrefTimer === "function") deno.unrefTimer(id);
  };

  /**
   * An `on<event>` IDL attribute.
   * @param {object} proto @param {string} name
   */
  function defineHandler(proto, name) {
    const key = Symbol(name);
    Object.defineProperty(proto, `on${name}`, {
      get() {
        return this[key]?.handler ?? null;
      },
      set(value) {
        const old = this[key];
        if (old) this.removeEventListener(name, old.listener);
        if (typeof value !== "function") {
          this[key] = undefined;
          return;
        }
        const entry = {
          handler: value,
          /** @param {Event} event */
          listener: (event) => entry.handler.call(this, event),
        };
        this[key] = entry;
        this.addEventListener(name, entry.listener);
      },
      configurable: true,
      enumerable: true,
    });
  }

  /**
   * A boolean IDL attribute that reflects a content attribute.
   * @param {object} proto @param {string} prop @param {string} attr
   */
  function reflectBoolean(proto, prop, attr) {
    Object.defineProperty(proto, prop, {
      get() {
        return this.hasAttribute(attr);
      },
      set(value) {
        if (value) this.setAttribute(attr, "");
        else this.removeAttribute(attr);
      },
      configurable: true,
      enumerable: true,
    });
  }

  class HTMLMediaElement extends HTMLElementBase {
    static NETWORK_EMPTY = 0;
    static NETWORK_IDLE = 1;
    static NETWORK_LOADING = 2;
    static NETWORK_NO_SOURCE = 3;
    static HAVE_NOTHING = 0;
    static HAVE_METADATA = 1;
    static HAVE_CURRENT_DATA = 2;
    static HAVE_FUTURE_DATA = 3;
    static HAVE_ENOUGH_DATA = 4;

    /** @type {any} */
    #srcObject = null;
    #readyState = HAVE_NOTHING;
    #networkState = NETWORK_EMPTY;
    #paused = true;
    #muted = false;
    #volume = 1;
    /** Seconds of the timeline before the current playing stretch. */
    #timeBase = 0;
    /** `performance.now()` when the current playing stretch began. */
    /** @type {number | null} */
    #playingSince = null;
    /** @type {number | undefined} */
    #timer = undefined;
    /** @type {{ resolve: () => void, reject: (e: unknown) => void }[]} */
    #pendingPlay = [];
    /** One reader per track of the stream. */
    /** @type {Map<MediaStreamTrack, ReadableStreamDefaultReader<any>>} */
    #readers = new Map();
    /** @type {((e: Event) => void) | null} */
    #onAddTrack = null;
    /** @type {((e: Event) => void) | null} */
    #onRemoveTrack = null;
    /** Bumped by every load: frames of an older load are ignored. */
    #generation = 0;
    videoSize = { width: 0, height: 0 };
    /** @type {Map<number, Function>} */
    frameCallbacks = new Map();
    nextFrameCallback = 1;
    framesReceived = 0;

    /** @param {string} localName */
    constructor(localName) {
      super(localName);
    }

    get srcObject() {
      return this.#srcObject;
    }
    set srcObject(value) {
      this.#srcObject = value ?? null;
      this.load();
    }
    get src() {
      return "";
    }
    get currentSrc() {
      return "";
    }
    get error() {
      return null;
    }
    get networkState() {
      return this.#networkState;
    }
    get readyState() {
      return this.#readyState;
    }
    get seeking() {
      return false;
    }
    get paused() {
      return this.#paused;
    }
    get ended() {
      // A MediaStream element ends when its stream becomes inactive.
      const stream = this.#srcObject;
      return this.#readyState > HAVE_NOTHING && isStream(stream) &&
        !stream.active;
    }
    get duration() {
      // mediacapture-main 6: +Infinity for a MediaStream, once known.
      return this.#readyState >= HAVE_METADATA ? Infinity : NaN;
    }
    get currentTime() {
      const running = this.#playingSince === null
        ? 0
        : (performance.now() - this.#playingSince) / 1000;
      return this.#timeBase + running;
    }
    set currentTime(_value) {
      // A MediaStream can't be seeked (mediacapture-main 6).
    }
    get playbackRate() {
      return 1;
    }
    set playbackRate(_value) {}
    get defaultPlaybackRate() {
      return 1;
    }
    set defaultPlaybackRate(_value) {}
    get preload() {
      return "none";
    }
    set preload(_value) {}
    get loop() {
      return false;
    }
    set loop(_value) {}
    get muted() {
      return this.#muted;
    }
    set muted(value) {
      const muted = !!value;
      if (muted === this.#muted) return;
      this.#muted = muted;
      this.#fire("volumechange");
    }
    get volume() {
      return this.#volume;
    }
    set volume(value) {
      const volume = Number(value);
      if (!(volume >= 0 && volume <= 1)) {
        throw new DOMException(
          "The volume is outside the range [0, 1]",
          "IndexSizeError",
        );
      }
      if (volume === this.#volume) return;
      this.#volume = volume;
      this.#fire("volumechange");
    }

    /** HTML's media element load algorithm, for a MediaStream. */
    load() {
      this.#generation++;
      this.#stopReading();
      for (const { reject } of this.#pendingPlay.splice(0)) {
        reject(new DOMException("The load was aborted", "AbortError"));
      }
      if (this.#networkState !== NETWORK_EMPTY) {
        this.#fire("emptied");
      }
      this.#readyState = HAVE_NOTHING;
      this.#networkState = NETWORK_EMPTY;
      this.#timeBase = 0;
      this.#playingSince = null;
      this.#updateClock();
      this.videoSize = { width: 0, height: 0 };
      this.framesReceived = 0;
      // HTML: the load sets `paused`; autoplay clears it once data came.
      this.#paused = true;
      const stream = this.#srcObject;
      if (stream === null) return;
      this.#networkState = NETWORK_IDLE;
      this.#fire("loadstart");
      if (!isStream(stream)) return;
      const generation = this.#generation;
      this.#onAddTrack = (event) => {
        const track = /** @type {any} */ (event).track;
        if (generation === this.#generation && track) this.#read(track);
      };
      this.#onRemoveTrack = (event) => {
        const track = /** @type {any} */ (event).track;
        const reader = this.#readers.get(track);
        if (reader) {
          this.#readers.delete(track);
          reader.cancel().catch(() => {});
        }
      };
      stream.addEventListener("addtrack", this.#onAddTrack);
      stream.addEventListener("removetrack", this.#onRemoveTrack);
      for (const track of stream.getTracks()) this.#read(track);
    }

    play() {
      return new Promise((resolve, reject) => {
        if (this.#paused) {
          this.#paused = false;
          this.#fire("play");
          if (this.#readyState >= HAVE_ENOUGH_DATA) this.#fire("playing");
        }
        if (this.#readyState >= HAVE_ENOUGH_DATA) {
          queueTask(() => resolve(undefined));
        } else {
          this.#pendingPlay.push({ resolve: () => resolve(undefined), reject });
        }
        this.#updateClock();
      });
    }

    pause() {
      if (this.#paused) return;
      this.#paused = true;
      this.#updateClock();
      this.#fire("timeupdate");
      this.#fire("pause");
      for (const { reject } of this.#pendingPlay.splice(0)) {
        reject(new DOMException("The play() request was paused", "AbortError"));
      }
    }

    /** @param {string} _type */
    canPlayType(_type) {
      return "";
    }

    /** @param {string} type */
    #fire(type) {
      queueTask(() => this.dispatchEvent(new Event(type)));
    }

    /** @param {MediaStreamTrack} track */
    #read(track) {
      if (this.#readers.has(track)) return;
      const readFrames = readFramesApi();
      if (readFrames === null) return;
      let stream;
      try {
        stream = readFrames(track);
      } catch {
        return; // A local track: the runtime renders no local media.
      }
      const reader = stream.getReader();
      this.#readers.set(track, reader);
      const generation = this.#generation;
      (async () => {
        try {
          while (true) {
            const { value, done } = await reader.read();
            if (done || generation !== this.#generation) break;
            this.#frame(track, value);
          }
        } catch {
          // Cancelled.
        } finally {
          if (this.#readers.get(track) === reader) this.#readers.delete(track);
        }
      })();
    }

    #stopReading() {
      const stream = this.#srcObject;
      if (isStream(stream)) {
        if (this.#onAddTrack) {
          stream.removeEventListener("addtrack", this.#onAddTrack);
        }
        if (this.#onRemoveTrack) {
          stream.removeEventListener("removetrack", this.#onRemoveTrack);
        }
      }
      this.#onAddTrack = null;
      this.#onRemoveTrack = null;
      for (const reader of this.#readers.values()) {
        reader.cancel().catch(() => {});
      }
      this.#readers.clear();
    }

    /**
     * A frame the runtime received on one of the stream's tracks.
     * @param {MediaStreamTrack} track
     * @param {any} frame
     */
    #frame(track, frame) {
      const video = track.kind === "video";
      if (video) this.framesReceived++;
      if (video && frame.width !== null && frame.height !== null) {
        const size = this.videoSize;
        if (size.width !== frame.width || size.height !== frame.height) {
          this.videoSize = { width: frame.width, height: frame.height };
          // HTML: a change of the intrinsic size once there is metadata.
          if (this.#readyState !== HAVE_NOTHING) this.#fire("resize");
        }
      }
      if (this.#readyState === HAVE_NOTHING) {
        const stream = this.#srcObject;
        const waitsForVideo = this instanceof HTMLVideoElement &&
          stream.getVideoTracks().length > 0;
        if (waitsForVideo && (!video || this.videoSize.width === 0)) return;
        this.#haveData();
      }
      if (video && this.#readyState >= HAVE_METADATA) {
        this.presentFrame(frame);
      }
    }

    /** The first frame: metadata, then enough data (HTML 4.8.11.7). */
    #haveData() {
      this.#readyState = HAVE_METADATA;
      this.#fire("durationchange");
      this.#fire("loadedmetadata");
      this.#readyState = HAVE_ENOUGH_DATA;
      this.#fire("loadeddata");
      this.#fire("canplay");
      if (this.#paused && this.autoplay) {
        this.#paused = false;
        this.#fire("play");
      }
      if (!this.#paused) {
        this.#fire("playing");
        for (const { resolve } of this.#pendingPlay.splice(0)) {
          queueTask(resolve);
        }
      }
      this.#fire("canplaythrough");
      this.#updateClock();
    }

    /** @param {any} _frame */
    presentFrame(_frame) {}

    /** Starts or stops the timeline: it runs while potentially playing. */
    #updateClock() {
      const playing = !this.#paused && this.#readyState >= HAVE_ENOUGH_DATA;
      if (playing && this.#playingSince === null) {
        this.#playingSince = performance.now();
        this.#timer = setInterval(
          () => this.dispatchEvent(new Event("timeupdate")),
          TIMEUPDATE_MS,
        );
        unref(this.#timer);
      } else if (!playing && this.#playingSince !== null) {
        this.#timeBase += (performance.now() - this.#playingSince) / 1000;
        this.#playingSince = null;
      }
      if (!playing && this.#timer !== undefined) {
        clearInterval(this.#timer);
        this.#timer = undefined;
      }
    }
  }
  for (
    const [name, value] of Object.entries({
      NETWORK_EMPTY: 0,
      NETWORK_IDLE: 1,
      NETWORK_LOADING: 2,
      NETWORK_NO_SOURCE: 3,
      HAVE_NOTHING: 0,
      HAVE_METADATA: 1,
      HAVE_CURRENT_DATA: 2,
      HAVE_FUTURE_DATA: 3,
      HAVE_ENOUGH_DATA: 4,
    })
  ) {
    Object.defineProperty(HTMLMediaElement.prototype, name, {
      value,
      enumerable: true,
    });
  }
  reflectBoolean(HTMLMediaElement.prototype, "autoplay", "autoplay");
  reflectBoolean(HTMLMediaElement.prototype, "controls", "controls");
  reflectBoolean(HTMLMediaElement.prototype, "defaultMuted", "muted");
  for (
    const name of [
      "abort",
      "canplay",
      "canplaythrough",
      "durationchange",
      "emptied",
      "ended",
      "error",
      "loadeddata",
      "loadedmetadata",
      "loadstart",
      "pause",
      "play",
      "playing",
      "progress",
      "ratechange",
      "resize",
      "seeked",
      "seeking",
      "stalled",
      "suspend",
      "timeupdate",
      "volumechange",
      "waiting",
    ]
  ) {
    defineHandler(HTMLMediaElement.prototype, name);
  }

  class HTMLVideoElement extends HTMLMediaElement {
    get videoWidth() {
      return this.readyState === HAVE_NOTHING ? 0 : this.videoSize.width;
    }
    get videoHeight() {
      return this.readyState === HAVE_NOTHING ? 0 : this.videoSize.height;
    }
    get width() {
      return Number(this.getAttribute("width") ?? 0) >>> 0;
    }
    set width(value) {
      this.setAttribute("width", Number(value) >>> 0);
    }
    get height() {
      return Number(this.getAttribute("height") ?? 0) >>> 0;
    }
    set height(value) {
      this.setAttribute("height", Number(value) >>> 0);
    }
    get poster() {
      return this.getAttribute("poster") ?? "";
    }
    set poster(value) {
      this.setAttribute("poster", value);
    }

    /**
     * Calls `callback(now, metadata)` for the next video frame the runtime
     * receives (the WICG video-rvfc draft's metadata, from that frame).
     * @param {Function} callback
     */
    requestVideoFrameCallback(callback) {
      if (typeof callback !== "function") {
        throw new TypeError("The callback is not a function");
      }
      const handle = this.nextFrameCallback++;
      this.frameCallbacks.set(handle, callback);
      return handle;
    }
    /** @param {number} handle */
    cancelVideoFrameCallback(handle) {
      this.frameCallbacks.delete(Number(handle));
    }

    getVideoPlaybackQuality() {
      return {
        creationTime: performance.now(),
        totalVideoFrames: this.framesReceived,
        droppedVideoFrames: 0,
        corruptedVideoFrames: 0,
      };
    }

    /** @override @param {any} frame */
    presentFrame(frame) {
      if (this.frameCallbacks.size === 0) return;
      const callbacks = [...this.frameCallbacks.values()];
      this.frameCallbacks.clear();
      const metadata = {
        presentationTime: frame.timestamp,
        expectedDisplayTime: frame.timestamp,
        width: this.videoSize.width,
        height: this.videoSize.height,
        mediaTime: this.currentTime,
        presentedFrames: this.framesReceived,
        receiveTime: frame.timestamp,
        rtpTimestamp: frame.rtpTimestamp,
      };
      queueTask(() => {
        const now = performance.now();
        for (const callback of callbacks) {
          try {
            callback(now, metadata);
          } catch (error) {
            reportError(error);
          }
        }
      });
    }
  }
  reflectBoolean(HTMLVideoElement.prototype, "playsInline", "playsinline");

  class HTMLAudioElement extends HTMLMediaElement {}

  /** @param {unknown} value @returns {value is MediaStream} */
  function isStream(value) {
    return typeof MediaStream === "function" && value instanceof MediaStream;
  }

  Object.defineProperty(globalThis, "HTMLMediaElement", {
    value: HTMLMediaElement,
    writable: true,
    configurable: true,
    enumerable: false,
  });
  const doc = /** @type {any} */ (globalThis).document;
  const interfaces = { video: HTMLVideoElement, audio: HTMLAudioElement };
  for (const [name, ctor] of Object.entries(interfaces)) {
    if (typeof doc[define] === "function") {
      doc[define](name, ctor);
    } else {
      Object.defineProperty(globalThis, ctor.name, {
        value: ctor,
        writable: true,
        configurable: true,
        enumerable: false,
      });
    }
  }
})();
