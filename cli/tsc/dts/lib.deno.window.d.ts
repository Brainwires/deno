// Copyright 2018-2026 the Deno authors. MIT license.

/// <reference no-default-lib="true" />
/// <reference lib="deno.ns" />
/// <reference lib="deno.shared_globals" />
/// <reference lib="deno.webstorage" />
/// <reference lib="esnext" />
/// <reference lib="deno.cache" />

/**
 * Defines the mapping between event names and their corresponding event types
 * for the `Window` interface in Deno.
 *
 * This interface provides type safety for event handlers by associating event names
 * with their proper event types.
 *
 * @category Platform
 */
interface WindowEventMap {
  "error": ErrorEvent;
  "unhandledrejection": PromiseRejectionEvent;
  "rejectionhandled": PromiseRejectionEvent;
}

/**
 * Represents the global window object in the Deno runtime environment.
 *
 * While Deno doesn't have a browser window, this interface mimics browser window
 * functionality for compatibility with web APIs. It provides access to global
 * properties and methods such as timers, storage, and event handling.
 *
 * @example
 * ```ts
 * // Accessing global objects
 * const localStorage = window.localStorage;
 *
 * // Event handling
 * window.addEventListener("unhandledrejection", (event) => {
 *   console.log("Unhandled promise rejection:", event.reason);d
 * });
 * ```
 *
 * @category Platform
 */
interface Window extends EventTarget {
  readonly window: Window & typeof globalThis;
  readonly self: Window & typeof globalThis;
  onerror: ((this: Window, ev: ErrorEvent) => any) | null;
  onload: ((this: Window, ev: Event) => any) | null;
  onbeforeunload: ((this: Window, ev: Event) => any) | null;
  onunload: ((this: Window, ev: Event) => any) | null;
  onunhandledrejection:
    | ((this: Window, ev: PromiseRejectionEvent) => any)
    | null;
  onrejectionhandled:
    | ((this: Window, ev: PromiseRejectionEvent) => any)
    | null;
  close: () => void;
  readonly closed: boolean;
  alert: (message?: string) => void;
  confirm: (message?: string) => boolean;
  prompt: (message?: string, defaultValue?: string) => string | null;
  Deno: typeof Deno;
  Navigator: typeof Navigator;
  navigator: Navigator;
  Location: typeof Location;
  location: Location;
  localStorage: Storage;
  sessionStorage: Storage;
  caches: CacheStorage;
  name: string;

  addEventListener<K extends keyof WindowEventMap>(
    type: K,
    listener: (
      this: Window,
      ev: WindowEventMap[K],
    ) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof WindowEventMap>(
    type: K,
    listener: (
      this: Window,
      ev: WindowEventMap[K],
    ) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/**
 * Constructor for `Window` objects.
 *
 * Note: This constructor cannot be used to create new `Window` instances in Deno.
 * The global `window` object is pre-defined in the runtime environment.
 *
 * @category Platform
 */
declare var Window: {
  readonly prototype: Window;
  new (): never;
};

/**
 * The window variable was removed in Deno 2. This declaration should be
 * removed at some point, but we're leaving it in out of caution.
 * @ignore
 * @category Platform
 */
declare var window: Window & typeof globalThis;

/**
 * Reference to the global object itself.
 * Equivalent to the global `window` object in browser environments.
 *
 * @category Platform
 */
declare var self: Window & typeof globalThis;

/**
 * Indicates whether the current window (context) is closed.
 * In Deno, this property is primarily for API compatibility with browsers.
 *
 * @category Platform
 */
declare var closed: boolean;

/**
 * Exits the current Deno process.
 *
 * This function terminates the process by signaling the runtime to exit.
 * Similar to exit(0) in posix. Its behavior is similar to the `window.close()`
 * method in the browser, but specific to the Deno runtime.
 *
 * Note: Use this function cautiously, as it will stop the execution of the
 * entire Deno program immediately.
 *
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/close
 *
 * @example
 * ```ts
 * console.log("About to close the Deno process.");
 * close(); // The process will terminate here.
 * console.log("This will not be logged."); // This line will never execute.
 * ```
 *
 * @category Platform
 */
declare function close(): void;

/**
 * Error event handler for the window.
 * Triggered when an uncaught error occurs in the global scope.
 *
 * @example
 * ```ts
 * onerror = (event) => {
 *   console.log(`Error occurred: ${event.message}`);
 *   return true; // Prevents the default error handling
 * };
 * ```
 *
 * @category Events
 */
declare var onerror: ((this: Window, ev: ErrorEvent) => any) | null;

/**
 * Load event handler for the window.
 * In Deno, this is primarily for API compatibility with browsers.
 *
 * @category Events
 */
declare var onload: ((this: Window, ev: Event) => any) | null;

/**
 * Before unload event handler for the window.
 * In Deno, this is primarily for API compatibility with browsers.
 *
 * @category Events
 */
declare var onbeforeunload: ((this: Window, ev: Event) => any) | null;

/**
 * Unload event handler for the window.
 * In Deno, this is primarily for API compatibility with browsers.
 *
 * @category Events
 */
declare var onunload: ((this: Window, ev: Event) => any) | null;

/**
 * Event handler for unhandled promise rejections.
 * Triggered when a `Promise` is rejected and no rejection handler is attached to it.
 *
 * @example
 * ```ts
 * onunhandledrejection = (event) => {
 *   console.log("Unhandled rejection:", event.reason);
 *   event.preventDefault(); // Prevents the default handling
 * };
 *
 * // This will trigger the event handler
 * Promise.reject(new Error("Example error"));
 * ```
 *
 * @category Events
 */
declare var onunhandledrejection:
  | ((this: Window, ev: PromiseRejectionEvent) => any)
  | null;

/**
 * Deno's `localStorage` API provides a way to store key-value pairs in a
 * web-like environment, similar to the Web Storage API found in browsers.
 * It allows developers to persist data across sessions in a Deno application.
 * This API is particularly useful for applications that require a simple
 * and effective way to store data locally.
 *
 * - Key-Value Storage: Stores data as key-value pairs.
 * - Persistent: Data is retained even after the application is closed.
 * - Synchronous API: Operations are performed synchronously.
 *
 * `localStorage` is similar to {@linkcode sessionStorage}, and shares the same
 * API methods, visible in the {@linkcode Storage} type.
 *
 * When using the `--location` flag, the origin for the location is used to
 * uniquely store the data. That means a location of http://example.com/a.ts
 * and http://example.com/b.ts and http://example.com:80/ would all share the
 * same storage, but https://example.com/ would be different.
 *
 * For more information, see the reference guide for
 * [Web Storage](https://docs.deno.com/runtime/reference/web_platform_apis/#web-storage)
 * and using
 * [the `--location` flag](https://docs.deno.com/runtime/reference/web_platform_apis/#location-flag).
 *
 * @example
 * ```ts
 * // Set a value in localStorage
 * localStorage.setItem("key", "value");
 *
 * // Get a value from localStorage
 * const value = localStorage.getItem("key");
 * console.log(value); // Output: "value"
 *
 * // Remove a value from localStorage
 * localStorage.removeItem("key");
 *
 * // Clear all values from localStorage
 * localStorage.clear();
 * ```
 *
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/localStorage
 * @category Storage */
declare var localStorage: Storage;

/**
 * Deno's `sessionStorage` API operates similarly to the {@linkcode localStorage} API,
 * but it is intended for storing data temporarily for the duration of a session.
 * Data stored in sessionStorage is cleared when the application session or
 * process ends. This makes it suitable for temporary data that you do not need
 * to persist across user sessions.
 *
 * - Key-Value Storage: Stores data as key-value pairs.
 * - Session-Based: Data is only available for the duration of the page session.
 * - Synchronous API: Operations are performed synchronously.
 *
 * `sessionStorage` is similar to {@linkcode localStorage}, and shares the same API
 * methods, visible in the {@linkcode Storage} type.
 *
 * For more information, see the reference guide for
 * [Web Storage](https://docs.deno.com/runtime/reference/web_platform_apis/#web-storage)
 *
 * @example
 * ```ts
 * // Set a value in sessionStorage
 * sessionStorage.setItem("key", "value");
 *
 * // Get a value from sessionStorage
 * const value = sessionStorage.getItem("key");
 * console.log(value); // Output: "value"
 *
 * // Remove a value from sessionStorage
 * sessionStorage.removeItem("key");
 *
 * // Clear all the values from sessionStorage
 * sessionStorage.clear();
 * ```
 *
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/sessionStorage
 * @category Storage
 */
declare var sessionStorage: Storage;
/** @category Cache */
/**
 * Provides access to the Cache API. Returns a CacheStorage object, which enables storing, retrieving, and managing request/response pairs in a cache.
 *
 * @example
 * ```ts
 * // Open (or create) a cache
 * const cache = await caches.open('v1');
 *
 * // Store a response
 * await cache.put('/api/data', new Response('Hello World'));
 *
 * // Retrieve from cache with fallback
 * const response = await caches.match('/api/data') || await fetch('/api/data');
 *
 * // Delete specific cache
 * await caches.delete('v1');
 *
 * // List all cache names
 * const cacheNames = await caches.keys();
 *
 * // Cache-first strategy
 * async function fetchWithCache(request) {
 *   const cached = await caches.match(request);
 *   if (cached) return cached;
 *
 *   const response = await fetch(request);
 *   const cache = await caches.open('v1');
 *   await cache.put(request, response.clone());
 *   return response;
 * }
 * ```
 *
 * @see  https://developer.mozilla.org/en-US/docs/Web/API/Window/caches
 */
declare var caches: CacheStorage;

/**
 * Provides information about the Deno runtime environment and the system
 * on which it's running. Similar to the browser `Navigator` object but
 * adapted for the Deno context.
 *
 * @example
 * ```ts
 * // Check available CPU cores
 * console.log(`Available CPU cores: ${navigator.hardwareConcurrency}`);
 *
 * // Check user agent
 * console.log(`User agent: ${navigator.userAgent}`);
 *
 * // Check language settings
 * console.log(`Language: ${navigator.language}`);
 * ```
 *
 * @category Platform
 */
interface Navigator {
  readonly gpu: GPU;
  readonly hardwareConcurrency: number;
  readonly userAgent: string;
  readonly language: string;
  readonly languages: string[];
  readonly platform: string;
  readonly userAgentData: NavigatorUAData;
  /**
   * Web MIDI: access to the system's MIDI 1.0 ports. Needs `--allow-midi`,
   * and `--allow-midi=sysex` for `{ sysex: true }`; rejects with
   * `NotAllowedError` without it, and with `NotSupportedError` where the OS
   * has no MIDI service (on Linux: no ALSA library or sequencer).
   *
   * @category MIDI
   */
  requestMIDIAccess(options?: MIDIOptions): Promise<MIDIAccess>;
}

/**
 * Options for {@linkcode Navigator.requestMIDIAccess}.
 *
 * @category MIDI
 */
interface MIDIOptions {
  /** Ask for system exclusive messages too. Needs `--allow-midi=sysex`
   * (a bare `--allow-midi` grants MIDI devices only). */
  sysex?: boolean;
  /** Include software synthesizers among the outputs. */
  software?: boolean;
}

/**
 * The ports of a {@linkcode MIDIAccess}, by id: a read-only map.
 *
 * @category MIDI
 */
interface MIDIInputMap extends ReadonlyMap<string, MIDIInput> {}

/** @category MIDI */
declare var MIDIInputMap: typeof globalThis extends
  { document: any; MIDIInputMap: infer T } ? T : {
  readonly prototype: MIDIInputMap;
  new (): never;
};

/**
 * The output ports of a {@linkcode MIDIAccess}, by id: a read-only map.
 *
 * @category MIDI
 */
interface MIDIOutputMap extends ReadonlyMap<string, MIDIOutput> {}

/** @category MIDI */
declare var MIDIOutputMap: typeof globalThis extends
  { document: any; MIDIOutputMap: infer T } ? T : {
  readonly prototype: MIDIOutputMap;
  new (): never;
};

/** @category MIDI */
interface MIDIAccessEventMap {
  "statechange": MIDIConnectionEvent;
}

/**
 * Access to the system's MIDI ports, from
 * {@linkcode Navigator.requestMIDIAccess}. MIDI 1.0 (the Web MIDI API has no
 * MIDI 2.0).
 *
 * @category MIDI
 */
interface MIDIAccess extends EventTarget {
  /** The input ports. A port that goes away stays here as `"disconnected"`. */
  readonly inputs: MIDIInputMap;
  /** The output ports. */
  readonly outputs: MIDIOutputMap;
  /** Whether system exclusive messages may be sent and are received. */
  readonly sysexEnabled: boolean;
  /** Fires when a port is added, goes away or comes back, or is opened or
   * closed. */
  onstatechange: ((this: MIDIAccess, ev: MIDIConnectionEvent) => any) | null;
  addEventListener<K extends keyof MIDIAccessEventMap>(
    type: K,
    listener: (this: MIDIAccess, ev: MIDIAccessEventMap[K]) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof MIDIAccessEventMap>(
    type: K,
    listener: (this: MIDIAccess, ev: MIDIAccessEventMap[K]) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/** @category MIDI */
declare var MIDIAccess: typeof globalThis extends
  { document: any; MIDIAccess: infer T } ? T : {
  readonly prototype: MIDIAccess;
  new (): never;
};

/** @category MIDI */
type MIDIPortType = "input" | "output";
/** @category MIDI */
type MIDIPortDeviceState = "connected" | "disconnected";
/** @category MIDI */
type MIDIPortConnectionState = "open" | "closed" | "pending";

/** @category MIDI */
interface MIDIPortEventMap {
  "statechange": MIDIConnectionEvent;
}

/**
 * A MIDI input or output port.
 *
 * @category MIDI
 */
interface MIDIPort extends EventTarget {
  /** The port's id, stable across sessions as far as the OS allows. */
  readonly id: string;
  readonly manufacturer: string | null;
  readonly name: string | null;
  readonly type: MIDIPortType;
  readonly version: string | null;
  /** Whether the device is present. */
  readonly state: MIDIPortDeviceState;
  /** Whether the port is open; `"pending"` while an open port's device is
   * away (it reopens when the device comes back). */
  readonly connection: MIDIPortConnectionState;
  onstatechange: ((this: MIDIPort, ev: MIDIConnectionEvent) => any) | null;
  /** Opens the port. Rejects with `InvalidAccessError` when the OS refuses
   * (another program holds it). */
  open(): Promise<MIDIPort>;
  close(): Promise<MIDIPort>;
  addEventListener<K extends keyof MIDIPortEventMap>(
    type: K,
    listener: (this: MIDIPort, ev: MIDIPortEventMap[K]) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof MIDIPortEventMap>(
    type: K,
    listener: (this: MIDIPort, ev: MIDIPortEventMap[K]) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/** @category MIDI */
declare var MIDIPort: typeof globalThis extends
  { document: any; MIDIPort: infer T } ? T : {
  readonly prototype: MIDIPort;
  new (): never;
};

/** @category MIDI */
interface MIDIInputEventMap extends MIDIPortEventMap {
  "midimessage": MIDIMessageEvent;
}

/**
 * A MIDI input port. Setting `onmidimessage` opens it; an open input keeps
 * the process alive until it is closed.
 *
 * @category MIDI
 */
interface MIDIInput extends MIDIPort {
  /** One complete message per event: running status is expanded, and a
   * real-time message arrives as its own event. */
  onmidimessage: ((this: MIDIInput, ev: MIDIMessageEvent) => any) | null;
  addEventListener<K extends keyof MIDIInputEventMap>(
    type: K,
    listener: (this: MIDIInput, ev: MIDIInputEventMap[K]) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof MIDIInputEventMap>(
    type: K,
    listener: (this: MIDIInput, ev: MIDIInputEventMap[K]) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/** @category MIDI */
declare var MIDIInput: typeof globalThis extends
  { document: any; MIDIInput: infer T } ? T : {
  readonly prototype: MIDIInput;
  new (): never;
};

/**
 * A MIDI output port.
 *
 * @category MIDI
 */
interface MIDIOutput extends MIDIPort {
  /** Sends complete MIDI messages (each with its status byte), at
   * `timestamp` on `performance.now()`'s clock, or at once when it is 0 or
   * past. Throws `TypeError` for an invalid message, `InvalidAccessError` for
   * a system exclusive message without `sysexEnabled`, and
   * `InvalidStateError` when the port is disconnected. Opens a closed port. */
  send(data: number[] | Uint8Array, timestamp?: number): void;
  /** Drops the messages queued for later and not yet sent. */
  clear(): void;
}

/** @category MIDI */
declare var MIDIOutput: typeof globalThis extends
  { document: any; MIDIOutput: infer T } ? T : {
  readonly prototype: MIDIOutput;
  new (): never;
};

/** @category MIDI */
interface MIDIMessageEventInit extends EventInit {
  data?: Uint8Array;
}

/**
 * A message an input received. `timeStamp` is its arrival time on
 * `performance.now()`'s clock.
 *
 * @category MIDI
 */
interface MIDIMessageEvent extends Event {
  readonly data: Uint8Array | null;
}

/** @category MIDI */
declare var MIDIMessageEvent: typeof globalThis extends
  { document: any; MIDIMessageEvent: infer T } ? T : {
  readonly prototype: MIDIMessageEvent;
  new (type: string, eventInitDict?: MIDIMessageEventInit): MIDIMessageEvent;
};

/** @category MIDI */
interface MIDIConnectionEventInit extends EventInit {
  port?: MIDIPort;
}

/**
 * A port's `state` or `connection` changed.
 *
 * @category MIDI
 */
interface MIDIConnectionEvent extends Event {
  readonly port: MIDIPort | null;
}

/** @category MIDI */
declare var MIDIConnectionEvent: typeof globalThis extends
  { document: any; MIDIConnectionEvent: infer T } ? T : {
  readonly prototype: MIDIConnectionEvent;
  new (
    type: string,
    eventInitDict?: MIDIConnectionEventInit,
  ): MIDIConnectionEvent;
};

/**
 * Constructor for `Navigator` objects.
 *
 * Note: This constructor cannot be used to create new `Navigator` instances in Deno.
 * The global `navigator` object is pre-defined in the runtime environment.
 *
 * @category Platform
 */
declare var Navigator: typeof globalThis extends
  { document: any; Navigator: infer T } ? T : {
  readonly prototype: Navigator;
  new (): never;
};

/**
 * Provides access to the Deno runtime's `Navigator` interface, which contains
 * information about the environment in which the script is running.
 *
 * @example
 * ```ts
 * // Log information about the runtime environment
 * console.log(`Hardware concurrency: ${navigator.hardwareConcurrency}`);
 * console.log(`User agent: ${navigator.userAgent}`);
 * ```
 *
 * @category Platform
 */
declare var navigator: typeof globalThis extends
  { document: any; navigator: infer T } ? T : Navigator;

/**
 * Shows the given message and waits for the enter key pressed.
 *
 * If the stdin is not interactive, it does nothing.
 *
 * In a desktop app (`deno desktop`) it shows a native dialog instead.
 * @throws {Deno.errors.NotSupported} in a desktop app whose backend has no
 * way to show a dialog here (Linux without kdialog, zenity or a GTK display):
 * nothing was shown, which is told apart from the user dismissing it.
 *
 * @example
 * ```ts
 * // Displays the message "Acknowledge me! [Enter]" and waits for the enter key to be pressed before continuing.
 * alert("Acknowledge me!");
 * ```
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/alert
 * @category Platform
 *
 * @param message
 */
declare function alert(message?: string): void;

/**
 * Shows the given message and waits for the answer. Returns the user's answer as boolean.
 *
 * Only `y` and `Y` are considered as true.
 *
 * If the stdin is not interactive, it returns false.
 *
 * In a desktop app (`deno desktop`) it shows a native dialog instead.
 * @throws {Deno.errors.NotSupported} in a desktop app whose backend has no
 * way to show a dialog here (Linux without kdialog, zenity or a GTK display):
 * nothing was shown, which is told apart from a cancel (`false`).
 *
 * @example
 * ```ts
 * const shouldProceed = confirm("Do you want to proceed?");
 *
 * // If the user presses 'y' or 'Y', the result will be true
 * // If the user presses 'n' or 'N', the result will be false
 * console.log("Should proceed?", shouldProceed);
 * ```
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/confirm
 * @category Platform
 *
 * @param message
 */
declare function confirm(message?: string): boolean;

/**
 * Shows the given message and waits for the user's input. Returns the user's input as string.
 *
 * If the default value is given and the user inputs the empty string, then it returns the given
 * default value.
 *
 * If the default value is not given and the user inputs the empty string, it returns the empty
 * string.
 *
 * If the stdin is not interactive, it returns null.
 *
 * In a desktop app (`deno desktop`) it shows a native dialog instead.
 * @throws {Deno.errors.NotSupported} in a desktop app whose backend has no
 * way to show a dialog here (Linux without kdialog, zenity or a GTK display):
 * nothing was shown, which is told apart from a cancel (`null`).
 *
 * @example
 * ```ts
 * const pet = prompt("Cats or dogs?", "It's fine to love both!");
 *
 * // Displays the user's input or the default value of "It's fine to love both!"
 * console.log("Best pet:", pet);
 * ```
 * @see https://developer.mozilla.org/en-US/docs/Web/API/Window/prompt
 *
 * @category Platform
 *
 * @param message
 * @param defaultValue
 */
declare function prompt(message?: string, defaultValue?: string): string | null;

/** Registers an event listener in the global scope, which will be called
 * synchronously whenever the event `type` is dispatched.
 *
 * ```ts
 * addEventListener('unload', () => { console.log('All finished!'); });
 * ...
 * dispatchEvent(new Event('unload'));
 * ```
 *
 * @category Events
 */
declare function addEventListener<
  K extends keyof WindowEventMap,
>(
  type: K,
  listener: (this: Window, ev: WindowEventMap[K]) => any,
  options?: boolean | AddEventListenerOptions,
): void;
/** Registers an event listener for an arbitrary event `type` on the global
 * scope.
 *
 * @category Events
 */
declare function addEventListener(
  type: string,
  listener: EventListenerOrEventListenerObject,
  options?: boolean | AddEventListenerOptions,
): void;

/** Remove a previously registered event listener from the global scope
 *
 * ```ts
 * const listener = () => { console.log('hello'); };
 * addEventListener('load', listener);
 * removeEventListener('load', listener);
 * ```
 *
 * @category Events
 */
declare function removeEventListener<
  K extends keyof WindowEventMap,
>(
  type: K,
  listener: (this: Window, ev: WindowEventMap[K]) => any,
  options?: boolean | EventListenerOptions,
): void;
/** Removes a previously registered event listener for an arbitrary event
 * `type` from the global scope.
 *
 * @category Events
 */
declare function removeEventListener(
  type: string,
  listener: EventListenerOrEventListenerObject,
  options?: boolean | EventListenerOptions,
): void;

// TODO(nayeemrmn): Move this to `extensions/web` where its implementation is.
// The types there must first be split into window, worker and global types.
/** The location (URL) of the object it is linked to. Changes done on it are
 * reflected on the object it relates to. Accessible via
 * `globalThis.location`.
 *
 * @category Platform
 */
interface Location {
  /** Returns a DOMStringList object listing the origins of the ancestor
   * browsing contexts, from the parent browsing context to the top-level
   * browsing context.
   *
   * Always empty in Deno. */
  readonly ancestorOrigins: DOMStringList;
  /** Returns the Location object's URL's fragment (includes leading "#" if
   * non-empty).
   *
   * Cannot be set in Deno. */
  hash: string;
  /** Returns the Location object's URL's host and port (if different from the
   * default port for the scheme).
   *
   * Cannot be set in Deno. */
  host: string;
  /** Returns the Location object's URL's host.
   *
   * Cannot be set in Deno. */
  hostname: string;
  /** Returns the Location object's URL.
   *
   * Cannot be set in Deno. */
  href: string;
  toString(): string;
  /** Returns the Location object's URL's origin. */
  readonly origin: string;
  /** Returns the Location object's URL's path.
   *
   * Cannot be set in Deno. */
  pathname: string;
  /** Returns the Location object's URL's port.
   *
   * Cannot be set in Deno. */
  port: string;
  /** Returns the Location object's URL's scheme.
   *
   * Cannot be set in Deno. */
  protocol: string;
  /** Returns the Location object's URL's query (includes leading "?" if
   * non-empty).
   *
   * Cannot be set in Deno. */
  search: string;
  /** Navigates to the given URL.
   *
   * Cannot be set in Deno. */
  assign(url: string): void;
  /** Reloads the current page.
   *
   * Disabled in Deno. */
  reload(): void;
  /** @deprecated */
  reload(forcedReload: boolean): void;
  /** Removes the current page from the session history and navigates to the
   * given URL.
   *
   * Disabled in Deno. */
  replace(url: string): void;
}

// TODO(nayeemrmn): Move this to `extensions/web` where its implementation is.
// The types there must first be split into window, worker and global types.
/** The location (URL) of the object it is linked to. Changes done on it are
 * reflected on the object it relates to. Accessible via
 * `globalThis.location`.
 *
 * @category Platform
 */
declare var Location: {
  readonly prototype: Location;
  new (): never;
};

// TODO(nayeemrmn): Move this to `extensions/web` where its implementation is.
// The types there must first be split into window, worker and global types.
/** The {@linkcode Location} object describing the absolute URL of the main
 * module, available when the program is started with the `--location` flag.
 * Accessing it without `--location` throws.
 *
 * @category Platform */
declare var location: Location;

/** Gets or sets the name of the global scope's browsing context.
 *
 * Provided for web compatibility; Deno has no browsing context, so this is an
 * empty string by default.
 *
 * @category Platform */
declare var name: string;

// EXPERIMENT (feat/stock-globals-experiment): phantom global markers so stock
// @types/node defers its web-platform globals to Deno's, mirroring how it
// defers to lib.dom. @types/node guards each web global with
// `typeof globalThis extends { <marker>; <Name>: infer T } ? T : <fallback>`,
// where the markers are `onmessage` (most), `onabort` (Storage) and
// `ReportingObserver` (Compression/DecompressionStream). Declaring them here
// makes those probes resolve to Deno's own globals. Each marker itself uses the
// document-deferral form so it still merges cleanly when real lib.dom is loaded.
// (Worker context already declares `onmessage`; these are for the main scope.)

/** Marker used so that `@types/node` defers its web-platform globals to Deno's.
 * The main scope has no message event handler, so this is `null`.
 *
 * @category Platform */
declare var onmessage: typeof globalThis extends
  { document: any; onmessage: infer T } ? T
  : ((this: void, ev: MessageEvent) => unknown) | null;

/** Marker used so that `@types/node` defers its `Storage` global to Deno's.
 * The main scope has no abort event handler, so this is `null`.
 *
 * @category Platform */
declare var onabort: typeof globalThis extends
  { document: any; onabort: infer T } ? T
  : ((this: void, ev: Event) => unknown) | null;

/** Marker used so that `@types/node` defers its `CompressionStream` and
 * `DecompressionStream` globals to Deno's.
 *
 * @category Platform */
declare var ReportingObserver: typeof globalThis extends
  { document: any; ReportingObserver: infer T } ? T
  : { readonly prototype: unknown; new (...args: any[]): unknown };
