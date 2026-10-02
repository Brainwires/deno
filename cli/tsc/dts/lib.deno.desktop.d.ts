// Copyright 2018-2026 the Deno authors. MIT license.

/// <reference no-default-lib="true" />
/// <reference lib="deno.ns" />
/// <reference lib="deno.window" />
/// <reference lib="deno.shared_globals" />
/// <reference lib="deno.webstorage" />
/// <reference lib="esnext" />
/// <reference lib="deno.cache" />
/// <reference lib="es2022.intl" />

declare interface UIEventInit extends EventInit {
  detail?: number;
  view?: null;
}

declare class UIEvent extends Event {
  constructor(type: string, init?: UIEventInit);
  readonly detail: number;
  readonly view: null;
}

declare interface FocusEventInit extends UIEventInit {
  relatedTarget?: EventTarget | null;
}

declare class FocusEvent extends UIEvent {
  constructor(type: string, init?: FocusEventInit);
  readonly relatedTarget: EventTarget | null;
}

declare interface KeyboardEventInit extends UIEventInit {
  key?: string;
  code?: string;
  location?: number;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
  repeat?: boolean;
  isComposing?: boolean;
}

declare class KeyboardEvent extends UIEvent {
  constructor(type: string, init?: KeyboardEventInit);
  readonly key: string;
  readonly code: string;
  readonly location: number;
  readonly ctrlKey: boolean;
  readonly shiftKey: boolean;
  readonly altKey: boolean;
  readonly metaKey: boolean;
  readonly repeat: boolean;
  readonly isComposing: boolean;
  getModifierState(key: string): boolean;
}

declare interface MouseEventInit extends UIEventInit {
  button?: number;
  buttons?: number;
  clientX?: number;
  clientY?: number;
  screenX?: number;
  screenY?: number;
  ctrlKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
  metaKey?: boolean;
}

declare class MouseEvent extends UIEvent {
  constructor(type: string, init?: MouseEventInit);
  readonly button: number;
  readonly buttons: number;
  readonly clientX: number;
  readonly clientY: number;
  readonly screenX: number;
  readonly screenY: number;
  readonly ctrlKey: boolean;
  readonly shiftKey: boolean;
  readonly altKey: boolean;
  readonly metaKey: boolean;
  getModifierState(key: string): boolean;
}

declare interface WheelEventInit extends MouseEventInit {
  deltaX?: number;
  deltaY?: number;
  deltaZ?: number;
  deltaMode?: number;
}

declare class WheelEvent extends MouseEvent {
  constructor(type: string, init?: WheelEventInit);
  readonly deltaX: number;
  readonly deltaY: number;
  readonly deltaZ: number;
  readonly deltaMode: number;
}

declare type NotificationPermission = "default" | "denied" | "granted";
declare type NotificationDirection = "auto" | "ltr" | "rtl";

/** An action button on a notification (the Web Notifications shape). */
declare interface NotificationAction {
  /** Identifies the button: the `action` of the `"action"` event. */
  action: string;
  /** The button's label. */
  title: string;
}

declare interface NotificationOptions {
  body?: string;
  /** Kept on the object; also stored with the OS notification as JSON (at
   * most 4 KiB), so a click after a restart
   * ({@linkcode Deno.desktop.NotificationResponseDetail}) gets it back. A
   * value JSON can't represent stays on the object only. */
  data?: any;
  /** Action buttons. A click on one fires the notification's `"action"`
   * event (not `"click"`). Shown on macOS, Windows (up to five) and Linux
   * servers that support actions; see
   * {@linkcode Deno.desktop.NotificationCapabilities.actions}. */
  actions?: NotificationAction[];
  dir?: NotificationDirection;
  icon?: string;
  lang?: string;
  badge?: string;
  requireInteraction?: boolean;
  silent?: boolean | null;
  tag?: string;
}

declare interface NotificationPermissionCallback {
  (permission: NotificationPermission): void;
}

/** The event a click on a notification's action button fires. */
declare interface NotificationActionEvent extends Event {
  /** The `action` of the clicked {@linkcode NotificationAction}. */
  readonly action: string;
}

declare interface NotificationEventMap {
  /** The body was clicked. */
  click: Event;
  /** An action button was clicked. */
  action: NotificationActionEvent;
  close: Event;
  error: Event;
  show: Event;
}

declare interface Notification extends EventTarget {
  readonly title: string;
  readonly body: string;
  readonly data: any;
  readonly dir: NotificationDirection;
  readonly icon: string;
  readonly lang: string;
  readonly badge: string;
  readonly tag: string;
  readonly silent: boolean | null;
  readonly requireInteraction: boolean;

  onclick: ((this: Notification, ev: Event) => any) | null;
  onaction: ((this: Notification, ev: NotificationActionEvent) => any) | null;
  onclose: ((this: Notification, ev: Event) => any) | null;
  onerror: ((this: Notification, ev: Event) => any) | null;
  onshow: ((this: Notification, ev: Event) => any) | null;

  close(): void;

  addEventListener<K extends keyof NotificationEventMap>(
    type: K,
    listener: (this: Notification, ev: NotificationEventMap[K]) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof NotificationEventMap>(
    type: K,
    listener: (this: Notification, ev: NotificationEventMap[K]) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

/** Web Notifications API.
 *
 * Construct a notification to display it. Only available in apps
 * compiled with `deno desktop`.
 *
 * Notification permission is checked against the OS (e.g. macOS User
 * Notifications). {@linkcode Notification.permission} reports the
 * cached result of the most recent query/request, and
 * {@linkcode Notification.requestPermission} triggers a system prompt
 * if the user has not yet decided.
 *
 * The Web Notifications API specifies `icon` as a URL string. The
 * desktop runtime can only resolve `data:` URLs synchronously; other
 * URL schemes are accepted (the value round-trips through the
 * {@linkcode Notification.icon} property) but the OS notification is
 * shown without an icon.
 */
declare var Notification: {
  prototype: Notification;
  new (title: string, options?: NotificationOptions): Notification;
  readonly permission: NotificationPermission;
  /** 5 where action buttons are shown, else 0. */
  readonly maxActions: number;
  requestPermission(
    deprecatedCallback?: NotificationPermissionCallback,
  ): Promise<NotificationPermission>;
};

/** Permissions API state value. Mirrors the Web Permissions API. */
declare type PermissionState = "granted" | "denied" | "prompt";

declare interface PermissionStatusEventMap {
  change: Event;
}

declare interface PermissionStatus extends EventTarget {
  readonly name: string;
  readonly state: PermissionState;
  onchange: ((this: PermissionStatus, ev: Event) => any) | null;

  addEventListener<K extends keyof PermissionStatusEventMap>(
    type: K,
    listener: (
      this: PermissionStatus,
      ev: PermissionStatusEventMap[K],
    ) => any,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | AddEventListenerOptions,
  ): void;
  removeEventListener<K extends keyof PermissionStatusEventMap>(
    type: K,
    listener: (
      this: PermissionStatus,
      ev: PermissionStatusEventMap[K],
    ) => any,
    options?: boolean | EventListenerOptions,
  ): void;
  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: boolean | EventListenerOptions,
  ): void;
}

declare var PermissionStatus: {
  prototype: PermissionStatus;
};

declare interface PermissionDescriptor {
  name: string;
}

declare interface Permissions {
  query(descriptor: PermissionDescriptor): Promise<PermissionStatus>;
}

/** Read from and write plain text to the system clipboard. A subset of the
 * web [Clipboard API](https://developer.mozilla.org/en-US/docs/Web/API/Clipboard);
 * only the text methods are backed by `deno desktop`. */
declare interface Clipboard extends EventTarget {
  /** Resolve with the clipboard's text content, or an empty string when the
   * clipboard is empty or holds no text.
   *
   * Rejects if the clipboard doesn't respond — on Linux the read is serviced
   * by whichever application owns the selection, so an unresponsive one
   * fails rather than resolving to an empty string it can't be told apart
   * from. */
  readText(): Promise<string>;
  /** Replace the clipboard's content with `data`. An empty string clears the
   * clipboard.
   *
   * Resolving means the write completed; it rejects if the clipboard doesn't
   * respond. */
  writeText(data: string): Promise<void>;
}

/** `Clipboard` has no constructor: the only instance is
 * {@linkcode Navigator.clipboard}. */
declare var Clipboard: {
  prototype: Clipboard;
};

/** Extends the {@linkcode Navigator} provided by `deno.window` with the
 * Permissions and Clipboard API surface available to `deno desktop` apps. */
declare interface Navigator {
  readonly permissions: Permissions;
  readonly clipboard: Clipboard;
}

declare namespace Deno {
  export {}; // stop default export type behavior

  /** The application version read from `deno.json` at compile time, or
   * `null` if no version was configured. Only available in apps compiled
   * with `deno desktop`. */
  export const desktopVersion: string | null;

  export interface AutoUpdateOptions {
    /** Base URL of the release server hosting `latest.json` and patch
     * files. Defaults to `desktop.release.baseUrl` from `deno.json` when
     * configured; required otherwise.
     *
     * Must be an `https:` URL — non-HTTPS URLs are refused. */
    url?: string;
    /** Poll interval in milliseconds. If omitted, only a single check is
     * performed ~1s after the call; pass an interval to keep checking for
     * the lifetime of the process. */
    interval?: number;
    /** Base64-encoded 32-byte Ed25519 public key used to verify the
     * release manifest.
     *
     * When set, `latest.json` must carry a top-level `signature` (base64
     * Ed25519 signature) over a `signed` field holding the manifest JSON
     * as a string. The signature is verified before any patch is fetched,
     * and only the contents of the verified `signed` payload are trusted.
     * A manifest that is unsigned or fails verification is rejected.
     *
     * Strongly recommended for production: without it, update integrity
     * rests solely on TLS and the per-patch SHA-256 in the manifest. */
    publicKey?: string;
    /** Called once an update has been downloaded, verified, and staged for
     * the next launch. Receives the version string being staged. */
    onUpdateReady?: (version: string) => void;
    /** Called if the previous launch's update failed to start and was
     * automatically rolled back to the prior version. Receives a
     * human-readable reason. */
    onRollback?: (reason: string) => void;
  }

  /** Start checking a release server for over-the-air updates.
   *
   * Updates are delivered as binary diffs against the app's native
   * library, so only the bytes that changed between versions are
   * downloaded. On each check, the manifest at `<url>/latest.json` is
   * fetched and its `version` compared against {@linkcode
   * Deno.desktopVersion}:
   *
   * - If `publicKey` is set, the manifest signature is verified first and
   *   an unsigned or invalid manifest is rejected.
   * - If the manifest advertises a newer version and lists a patch from
   *   the currently running version (under `patches[<currentVersion>]`,
   *   as `{ name, sha256 }`), that patch is downloaded, checked against
   *   its declared SHA-256, applied, and staged for the next launch.
   * - `onUpdateReady` is then invoked. The new version takes effect the
   *   next time the app starts.
   *
   * The staged update is swapped in atomically on the next launch, with
   * the previous version kept as a backup. If the updated app fails to
   * start, it is automatically rolled back to the backup and `onRollback`
   * is invoked shortly after the next `autoUpdate` call.
   *
   * The release server URL may be passed directly, supplied via
   * {@linkcode AutoUpdateOptions.url}, or configured once in `deno.json`
   * under `desktop.release.baseUrl` (in which case it can be omitted).
   * A single check runs ~1s after the call; pass `interval` to keep
   * polling.
   *
   * Because updates rewrite the app's own library file in place, they only
   * apply where the running process can write to its installed files. This
   * works for self-contained, user-writable installs (a `.app` bundle or a
   * loose binary in the user's home directory, a writable AppImage next to
   * its data). It does **not** work for read-only or system-owned installs
   * — an AppImage mounted read-only, or an app installed under `/usr` from
   * an `rpm`/`deb` package owned by `root`. In those cases the write fails,
   * the failure is logged, and the update is skipped; distribute updates
   * through the system package manager instead.
   *
   * Only available in apps compiled with `deno desktop`.
   *
   * ```ts
   * Deno.autoUpdate({
   *   url: "https://releases.example.com/myapp",
   *   interval: 60 * 60 * 1000, // hourly
   *   publicKey: "b64EncodedEd25519PublicKey==",
   *   onUpdateReady(version) {
   *     console.log(`v${version} staged; restart to apply`);
   *   },
   *   onRollback(reason) {
   *     console.warn(`update rolled back: ${reason}`);
   *   },
   * });
   * ```
   */
  export function autoUpdate(url: string): void;
  export function autoUpdate(options?: AutoUpdateOptions): void;

  export interface OpenDevtoolsOptions {
    /** Inspect the CEF renderer isolate. @default {true} */
    renderer?: boolean;
    /** Inspect the Deno runtime isolate. @default {true} */
    deno?: boolean;
  }

  export interface BrowserWindowOptions {
    title?: string;
    /** @default {800} */
    width?: number;
    /** @default {600} */
    height?: number;
    x?: number;
    y?: number;
    /** @default {true} */
    resizable?: boolean;
    /** @default {false} */
    alwaysOnTop?: boolean;
    /** Overall window opacity as a uniform factor in the range `0`–`1`, where
     * `1` is fully opaque (the default) and `0` is fully transparent. Fades the
     * entire window — web content and native chrome alike — like CSS `opacity`.
     * This is distinct from {@linkcode transparent}, which makes the background
     * transparent while honoring the page's own per-pixel alpha. Out-of-range
     * values are clamped. Can also be changed at runtime with
     * {@linkcode BrowserWindow.setOpacity}.
     *
     * @default {1} */
    opacity?: number;
    /** Remove the title bar and standard window chrome (border, traffic
     * light / caption buttons). Set at creation time only.
     *
     * @default {false} */
    frameless?: boolean;
    /** Create the window as a floating, non-activating utility "panel": it
     * floats above normal windows and does not activate the app or steal key
     * focus from the foreground app when shown. Combined with
     * {@linkcode frameless} and {@linkcode Tray.getBounds}, this is the
     * configuration used for tray / menu-bar popovers. Set at creation time
     * only.
     *
     * @default {false} */
    noActivate?: boolean;
    transparentTitlebar?: boolean;
    /** Give the window a transparent background so the web content's own alpha
     * composites against whatever is behind the window. Any region the page
     * leaves transparent (e.g. a `transparent` root background) shows the
     * desktop through it. Often combined with {@linkcode frameless}. Distinct
     * from {@linkcode opacity}, which uniformly fades the whole window. Set at
     * creation time only.
     *
     * Supported on macOS and Linux with the system WebView; ignored on Windows
     * and with the CEF backend, which paint an opaque window background.
     *
     * @default {false} */
    transparent?: boolean;
    /** Minimum size, applied with {@linkcode BrowserWindow.setMinimumSize}.
     * `0` (the default) is no limit. */
    minWidth?: number;
    minHeight?: number;
    /** Maximum size, applied with {@linkcode BrowserWindow.setMaximumSize}.
     * `0` (the default) is no limit. */
    maxWidth?: number;
    maxHeight?: number;
    /** Open in fullscreen ({@linkcode BrowserWindow.setFullScreen}).
     *
     * @default {false} */
    fullscreen?: boolean;
    /** See {@linkcode BrowserWindow.setTitleBarStyle}. */
    titleBarStyle?: TitleBarStyle;
    /** See {@linkcode BrowserWindow.setWindowButtonPosition}. */
    trafficLightPosition?: WindowButtonPosition;
    /** See {@linkcode BrowserWindow.setVibrancy}. */
    vibrancy?: VibrancyMaterial;
    /** See {@linkcode BrowserWindow.setBackgroundMaterial}. */
    backgroundMaterial?: BackgroundMaterial;
  }

  /** A rectangle in screen space: the {@linkcode BrowserWindow.getPosition}
   * coordinates of this backend (points / DIP on macOS, Linux and CEF;
   * physical pixels with the Windows WebView2 backend). */
  export interface Rectangle {
    x: number;
    y: number;
    width: number;
    height: number;
  }

  /** A display, from {@linkcode Deno.desktop.screens}. */
  export interface Screen {
    /** Identifies the display while it stays connected (not across a
     * reconnect or a restart). */
    id: number;
    /** The whole display. */
    bounds: Rectangle;
    /** The display minus the menu bar, Dock, taskbar and panels. */
    workArea: Rectangle;
    /** Physical pixels per CSS pixel on this display (what
     * {@linkcode BrowserWindow.devicePixelRatio} reports for a window on it). */
    scaleFactor: number;
    /** The primary display (macOS: the one with the menu bar). */
    isPrimary: boolean;
  }

  /** `"hidden"`: a transparent title bar, the page drawn under it, the
   * system buttons on top. `"hiddenInset"`: the same with the macOS traffic
   * lights inset. */
  export type TitleBarStyle = "default" | "hidden" | "hiddenInset";

  /** Where the macOS traffic lights go: the close button's top-left corner,
   * in points from the window's top-left. */
  export interface WindowButtonPosition {
    x: number;
    y: number;
  }

  /** Windows 11 backdrops: `"mica"`, `"acrylic"`, `"tabbed"` (Mica Alt). */
  export type BackgroundMaterial = "none" | "mica" | "acrylic" | "tabbed";

  /** macOS vibrancy materials (NSVisualEffectMaterial). */
  export type VibrancyMaterial =
    | "titlebar"
    | "selection"
    | "menu"
    | "popover"
    | "sidebar"
    | "header"
    | "sheet"
    | "window"
    | "hud"
    | "fullscreen-ui"
    | "tooltip"
    | "content"
    | "under-window"
    | "under-page";

  interface BrowserWindowObject {
    [key: string]: BrowserWindowValue;
  }

  type BrowserWindowValue =
    | null
    | boolean
    | number
    | string
    | BrowserWindowObject
    | BrowserWindowValue[]
    | Uint8Array;

  /** The leaf types that survive the trip across the webview boundary. */
  type BrowserWindowLeaf =
    | null
    | undefined
    | void
    | boolean
    | number
    | string
    | Uint8Array;

  /** Maps `T` to itself if every value it can hold survives the trip across
   * the webview boundary, and to `never` at the first member that does not.
   * `T` is serializable when `[T] extends [BrowserWindowSerializable<T>]`.
   *
   * This is a structural walk rather than a plain union because TypeScript
   * never gives an `interface` an implicit index signature: a union arm of
   * `{ [key: string]: unknown }` would reject every user-declared interface,
   * including `Deno.FileInfo`. Recursing through properties instead accepts
   * interfaces, and rejects the values that silently serialize to `{}` —
   * `Date`, `Map`, `Set`, class instances with methods, and functions.
   *
   * `unknown extends T` holds only for `unknown` and `any`. Both are let
   * through: nothing can be proven about them, and rejecting them would
   * break `Record<string, unknown>` payloads.
   *
   * `undefined` (and optional) properties are dropped during serialization,
   * and a handler that returns nothing resolves as `null` in the webview. */
  type BrowserWindowSerializable<T> = unknown extends T ? T
    : T extends BrowserWindowLeaf ? T
    : T extends (...args: any[]) => any ? never
    : T extends readonly (infer U)[] ? readonly BrowserWindowSerializable<U>[]
    : T extends object ? { [K in keyof T]: BrowserWindowSerializable<T[K]> }
    : never;

  /** The default bindings type: any set of async handlers.
   *
   * Handler parameters are `any[]` because the webview may call a binding
   * with arbitrary arguments — nothing checks them at runtime. Supply an
   * explicit type argument to {@linkcode BrowserWindow} to have the
   * arguments of {@linkcode BrowserWindow.bind} handlers checked and
   * inferred. Handler return values are checked either way. */
  export type WindowBindings = Record<
    string,
    (this: BrowserWindow, ...args: any[]) => Promise<unknown>
  >;

  /** Intersected with a handler's own type to reject non-serializable return
   * values at the call site of {@linkcode BrowserWindow.bind}.
   *
   * The check lives here, in parameter position, rather than as an
   * `R extends BrowserWindowSerializable<R>` type-parameter constraint,
   * because a type parameter constrained by a conditional type over itself
   * is circular (TS2313). Intersecting the string makes the mismatch print
   * the reason. */
  type BrowserWindowSerializableCheck<F> = F extends
    (...args: any[]) => Promise<infer P>
    ? [P] extends [BrowserWindowSerializable<P>] ? unknown
    : "binding handler must resolve with a serializable value"
    : unknown;

  /** Constrains T to a record of async binding functions taking and
   * resolving with serializable values.
   *
   * Each handler is validated and then passed through unchanged, so that
   * {@linkcode BrowserWindow.bind} keeps the caller's own parameter types.
   * Replacing `T[K]` with a common supertype here would instead force every
   * handler's parameters to be checked contravariantly against that
   * supertype, rejecting any handler that narrows them. */
  type ValidBindings<T> = {
    [K in keyof T]: T[K] extends (...args: infer A) => Promise<infer P>
      ? [A, P] extends
        [BrowserWindowSerializable<A>, BrowserWindowSerializable<P>] ? T[K]
      : never
      : never;
  };

  export type MenuItem =
    | {
      item: {
        label: string;
        id?: string;
        /** A keyboard shortcut in the {@linkcode Deno.desktop.shortcuts}
         * syntax (`"CommandOrControl+Shift+K"`). In the application menu it
         * fires the item while the window has the focus, on every OS; in a
         * context menu it is only shown. One that doesn't parse is ignored.
         */
        accelerator?: string;
        enabled: boolean;
        /** Show a checkmark next to the item. Supported on all
         * platforms. Defaults to `false`. */
        checked?: boolean;
        /** PNG-encoded image bytes shown next to the label, like
         * {@linkcode Tray.setIcon}. Shown on macOS, Windows and Linux
         * (WebKitGTK); not in the CEF backend's application menu on Windows
         * and Linux, nor its Linux context menus (see
         * {@linkcode Deno.desktop.menuCapabilities}). On macOS a monochrome
         * black+alpha PNG is rendered as a template image, tinting to white
         * when the item is highlighted. */
        icon?: Uint8Array;
        /** Tooltip shown when hovering over the item: macOS and Linux
         * (WebKitGTK). */
        tooltip?: string;
      };
    }
    | {
      submenu: {
        label: string;
        items: MenuItem[];
      };
    }
    | "separator"
    | {
      role: {
        role: string;
      };
    };

  interface BrowserWindowResizeDetail {
    width: number;
    height: number;
  }

  interface BrowserWindowMoveDetail {
    x: number;
    y: number;
  }

  interface MenuClickDetail {
    id: string;
  }

  /** Detail of a {@linkcode BrowserWindow} `dragenter`, `dragover`,
   * `dragleave` or `drop` event: files dragged over, or dropped on, the
   * window.
   *
   * @category Desktop
   * @experimental */
  export interface BrowserWindowFileDropDetail {
    /** The files' absolute native paths. Always set for `drop`; `null` for
     * `dragleave`, and for `dragenter` / `dragover` where the engine reveals
     * the paths only on the drop (WebView2 on Windows; see
     * `Deno.desktop.windowCapabilities().fileDropEnterPaths`). */
    readonly paths: readonly string[] | null;
    /** How many files are dragged (0 for `dragleave`). */
    readonly count: number;
    /** The pointer in the window's content area, in CSS pixels (the
     * `clientX` / `clientY` space of the page). The Winit backend has no drag
     * position: there it is the last pointer position seen in the window. */
    readonly x: number;
    readonly y: number;
  }

  /** What {@linkcode BrowserWindow.startDrag} drags.
   *
   * @category Desktop
   * @experimental */
  export interface BrowserWindowDragItem {
    /** Absolute paths of existing files or directories (1 to 4096). */
    files: string[];
    /** PNG bytes shown under the pointer; by default the OS's file icon. */
    icon?: Uint8Array;
  }

  interface BrowserWindowEventMap {
    keydown: KeyboardEvent;
    keyup: KeyboardEvent;
    mousedown: MouseEvent;
    mouseup: MouseEvent;
    click: MouseEvent;
    dblclick: MouseEvent;
    mousemove: MouseEvent;
    mouseenter: MouseEvent;
    mouseleave: MouseEvent;
    wheel: WheelEvent;
    focus: FocusEvent;
    blur: FocusEvent;

    // non-standard events
    resize: CustomEvent<BrowserWindowResizeDetail>;
    move: CustomEvent<BrowserWindowMoveDetail>;
    /** Fires after a navigation finishes loading. */
    load: Event;
    /** The user asked to close the window (its close button, Alt+F4, the
     * window manager) or {@linkcode Deno.desktop.quit} is closing it.
     * Cancelable: `preventDefault()` keeps the window open; close it later
     * with {@linkcode BrowserWindow.close}, which closes it without another
     * `close` event. Answer synchronously: if the runtime does not get an
     * answer within 5 seconds (its event loop is blocked or gone), the window
     * closes anyway. Not fired by {@linkcode BrowserWindow.close}. */
    close: Event;
    /** The window was maximized (zoomed on macOS). */
    maximize: Event;
    unmaximize: Event;
    minimize: Event;
    /** The window came back from minimized. */
    restore: Event;
    enterfullscreen: Event;
    leavefullscreen: Event;
    /** Files are dragged into the window. The page keeps getting its own
     * DOM `dragenter` (with `File` objects, never paths); this event is
     * where the native paths are. */
    dragenter: CustomEvent<BrowserWindowFileDropDetail>;
    /** The files moved over the window (Winit: never fired). */
    dragover: CustomEvent<BrowserWindowFileDropDetail>;
    /** The drag left the window or was cancelled. */
    dragleave: CustomEvent<BrowserWindowFileDropDetail>;
    /** Files were dropped on the window: `detail.paths` are their absolute
     * native paths. The window accepts every file drag (the cursor shows a
     * copy) so the drop reaches the app even where the page doesn't handle
     * it; what the page does with its own DOM `drop` is unchanged. On Linux
     * (WebKitGTK) a page that refuses file drops (`dropEffect = "none"`)
     * also hides them from this event. */
    drop: CustomEvent<BrowserWindowFileDropDetail>;
    menuclick: CustomEvent<MenuClickDetail>;
    contextmenuclick: CustomEvent<MenuClickDetail>;
    /** The context menu {@linkcode BrowserWindow.showContextMenu} opened
     * closed: `detail.id` is the chosen item's id, or `null` when it was
     * dismissed. Fires after its `"contextmenuclick"`. */
    contextmenuclose: CustomEvent<{ id: string | null }>;
  }

  type BrowserWindowEventHandlers = {
    [K in keyof BrowserWindowEventMap as `on${K}`]:
      | ((this: BrowserWindow, ev: BrowserWindowEventMap[K]) => any)
      | null;
  };

  export interface BrowserWindow<T extends ValidBindings<T> = WindowBindings>
    extends BrowserWindowEventHandlers {}

  export class BrowserWindow<
    T extends ValidBindings<T> = WindowBindings,
  > extends EventTarget {
    constructor(options?: BrowserWindowOptions);

    readonly windowId: number;

    /** Expose `fn` to the webview under `name`.
     *
     * The resolved value must be serializable; see
     * {@linkcode BrowserWindowSerializable}. This is checked even when `T` is
     * left to its default, so returning e.g. a `Date` is a type error rather
     * than an empty object at runtime. */
    bind<N extends keyof T, F extends T[N]>(
      name: N,
      fn: F & BrowserWindowSerializableCheck<F>,
    ): void;
    unbind<N extends keyof T>(name: N): void;
    /** @throws {BrowserWindowValue} */
    executeJs(script: string): Promise<BrowserWindowValue>;

    setTitle(title: string): void;

    getSize(): [number, number];
    setSize(width: number, height: number): void;
    /** Viewport width in CSS pixels, like
     * [`window.innerWidth`](https://developer.mozilla.org/docs/Web/API/Window/innerWidth).
     * Same as the first element of {@linkcode BrowserWindow.getSize}. */
    readonly innerWidth: number;
    /** Viewport height in CSS pixels, like
     * [`window.innerHeight`](https://developer.mozilla.org/docs/Web/API/Window/innerHeight).
     * Same as the second element of {@linkcode BrowserWindow.getSize}. */
    readonly innerHeight: number;
    /** Chrome-inclusive width in CSS pixels, like
     * [`window.outerWidth`](https://developer.mozilla.org/docs/Web/API/Window/outerWidth).
     * A frameless window matches {@linkcode BrowserWindow.innerWidth}. */
    readonly outerWidth: number;
    /** Chrome-inclusive height in CSS pixels, like
     * [`window.outerHeight`](https://developer.mozilla.org/docs/Web/API/Window/outerHeight).
     * A frameless window matches {@linkcode BrowserWindow.innerHeight}. */
    readonly outerHeight: number;

    /** Physical pixels per CSS pixel for this window, like
     * [`window.devicePixelRatio`](https://developer.mozilla.org/docs/Web/API/Window/devicePixelRatio).
     * Updates when the window moves to a display with a different scale.
     * Observe that with {@linkcode BrowserWindow.matchMedia}, not
     * {@linkcode BrowserWindowEventMap.resize}. */
    readonly devicePixelRatio: number;

    /** [`window.matchMedia`](https://developer.mozilla.org/docs/Web/API/Window/matchMedia)
     * for this window. `change` fires when a move or resize crosses the
     * query, including a move onto a display with a different
     * {@linkcode BrowserWindow.devicePixelRatio}. Understands `width`,
     * `height`, `aspect-ratio`, `orientation`, `resolution`, and
     * `-webkit-device-pixel-ratio` / `device-pixel-ratio`, including
     * Media Queries Level 4 range syntax. */
    matchMedia(query: string): MediaQueryList;

    getPosition(): [number, number];
    setPosition(x: number, y: number): void;
    /** Frame origin in screen CSS pixels, like
     * [`window.screenX`](https://developer.mozilla.org/docs/Web/API/Window/screenX).
     * Same as the first element of {@linkcode BrowserWindow.getPosition}.
     * Not {@linkcode MouseEvent.screenX} — that is
     * {@linkcode BrowserWindow.getInnerPosition}`()[0] + clientX`. */
    readonly screenX: number;
    /** Frame origin in screen CSS pixels, like
     * [`window.screenY`](https://developer.mozilla.org/docs/Web/API/Window/screenY).
     * Same as the second element of {@linkcode BrowserWindow.getPosition}.
     * Not {@linkcode MouseEvent.screenY} — that is
     * {@linkcode BrowserWindow.getInnerPosition}`()[1] + clientY`. */
    readonly screenY: number;
    /** Alias of {@linkcode BrowserWindow.screenX}, like
     * [`window.screenLeft`](https://developer.mozilla.org/docs/Web/API/Window/screenLeft). */
    readonly screenLeft: number;
    /** Alias of {@linkcode BrowserWindow.screenY}, like
     * [`window.screenTop`](https://developer.mozilla.org/docs/Web/API/Window/screenTop). */
    readonly screenTop: number;
    /** Top-left of the content view in screen CSS pixels. Differs from
     * {@linkcode BrowserWindow.getPosition} by the title-bar height, so
     * `getInnerPosition()[1] + clientY` is `MouseEvent.screenY`. */
    getInnerPosition(): [number, number];

    isResizable(): boolean;
    setResizable(resizable: boolean): void;

    isAlwaysOnTop(): boolean;
    setAlwaysOnTop(alwaysOnTop: boolean): void;

    /** Get the window's overall opacity, a uniform factor in the range `0`–`1`
     * where `1` is fully opaque. */
    getOpacity(): number;
    /** Set the window's overall opacity, a uniform factor in the range `0`–`1`
     * where `1` is fully opaque (the default) and `0` is fully transparent.
     * Fades the entire window — web content and native chrome alike — like CSS
     * `opacity`. Out-of-range values are clamped. No-op on backends without
     * opacity support. */
    setOpacity(opacity: number): void;

    isClosed(): boolean;
    /** Close the window, without a `close` event (the way to finish a close
     * that a `close` listener canceled). */
    close(): void;

    /** Maximize the window (zoom on macOS). Like the other state methods it
     * may complete asynchronously while the OS animates the change; the
     * `maximize` event fires once it took effect. No-op where
     * `Deno.desktop.windowCapabilities().state` is false. */
    maximize(): void;
    unmaximize(): void;
    isMaximized(): boolean;
    minimize(): void;
    /** Un-minimize: back to the state the window had before it was
     * minimized. */
    restore(): void;
    isMinimized(): boolean;
    setFullScreen(flag: boolean): void;
    isFullScreen(): boolean;

    /** Limit how small the window can get, in {@linkcode setSize} units
     * (`0` = no limit on that axis). The OS enforces it while the user
     * resizes, {@linkcode setSize} clamps to it, and a smaller window grows
     * to it. */
    setMinimumSize(width: number, height: number): void;
    getMinimumSize(): [number, number];
    /** Limit how large the window can get (`0` = no limit). */
    setMaximumSize(width: number, height: number): void;
    getMaximumSize(): [number, number];

    /** The outer frame: {@linkcode getPosition} and
     * {@linkcode outerWidth} / {@linkcode outerHeight}. */
    getBounds(): Rectangle;
    /** Move and / or resize the outer frame; missing fields keep their
     * value. A rectangle that would leave the window unreachable (on a
     * display that is gone, or less than a 64x32 corner on any work area)
     * is moved to the primary display, shrunk to fit and centered. The same
     * rule applies to {@linkcode setPosition} and the constructor's `x` /
     * `y`.
     *
     * To reopen a window where the user left it, save
     * {@linkcode getNormalBounds} (and {@linkcode isMaximized} /
     * {@linkcode isFullScreen}) when it closes, then on the next launch call
     * `setBounds(saved)` and `maximize()` / `setFullScreen(true)` as needed. */
    setBounds(bounds: Partial<Rectangle>): void;
    /** The page area: {@linkcode getInnerPosition} and {@linkcode getSize}. */
    getContentBounds(): Rectangle;
    /** Like {@linkcode getBounds}, for the bounds the window returns to when
     * it leaves the maximized, minimized or fullscreen state (the current
     * bounds for a normal window). This is what to persist. */
    getNormalBounds(): Rectangle;
    /** The display the window is on (the one it overlaps most), or `null`. */
    getScreen(): Screen | null;

    /** Drag files out of the window to another app or the desktop, as a
     * copy (Electron's `webContents.startDrag`). The OS runs the drag from
     * the pointer, so call it while the left mouse button is held: from the
     * page's `dragstart` (call `preventDefault()` there and ask the app to
     * start this one) or from a `mousedown` followed by a move.
     *
     * Resolves `"dropped"` when a target took the files, `"cancelled"` when
     * the user pressed Escape or dropped where nothing took them, and
     * `"failed"` when the drag never started: no left button held, another
     * drag running, a path that is not an existing absolute path, or no
     * drag-out on this backend
     * (`Deno.desktop.windowCapabilities().fileDragOut`: none on Winit, and
     * CEF on Linux needs an X11 display). Rejects with a `TypeError` for a
     * wrong argument.
     *
     * - macOS: `beginDraggingSessionWithItems` (an `NSURL` per file).
     * - Windows: `DoDragDrop` with a `CF_HDROP` data object.
     * - Linux: a GTK drag source offering `text/uri-list`.
     *
     * @experimental */
    startDrag(
      item: BrowserWindowDragItem,
    ): Promise<"dropped" | "cancelled" | "failed">;

    /** Change the title bar style. Returns `false` (and changes nothing)
     * where the backend can't: only macOS has title bar styles. */
    setTitleBarStyle(style: TitleBarStyle): boolean;
    /** Move the macOS traffic lights (Electron's `setWindowButtonPosition` /
     * `trafficLightPosition`); `null` puts them back. Kept across resizes
     * and fullscreen. Returns `false` where unsupported. */
    setWindowButtonPosition(position: WindowButtonPosition | null): boolean;
    /** Put a Windows 11 backdrop (Mica, Acrylic, tabbed Mica) behind the
     * page; it shows where the page's background is transparent. Returns
     * `false` (and changes nothing) where unsupported: other OSes, Windows
     * before 11 (Acrylic and tabbed need 22H2), and the CEF backend. */
    setBackgroundMaterial(material: BackgroundMaterial): boolean;
    /** Put macOS vibrancy (an NSVisualEffectView) behind the page; it shows
     * where the page's background is transparent. `null` removes it. Returns
     * `false` where unsupported: other OSes and the CEF backend. */
    setVibrancy(material: VibrancyMaterial | null): boolean;

    isVisible(): boolean;
    show(): void;
    hide(): void;
    focus(): void;
    navigate(url: string): void;
    /** Open a DevTools window.
     *
     * By default both targets are shown. Pass an options object to
     * select which targets to inspect. At least one must be `true`.
     */
    openDevtools(options?: OpenDevtoolsOptions): void;
    /** Close this window's DevTools (opened with {@linkcode openDevtools} or
     * by the user). */
    closeDevtools(): void;
    /** Open this window's DevTools if they are closed, else close them. */
    toggleDevtools(options?: OpenDevtoolsOptions): void;
    /** Whether this window's DevTools are open.
     *
     * On Windows (WebView2), DevTools the user opened with F12 or the
     * context menu count only while the app has a single window; with
     * several, only those opened through {@linkcode openDevtools}. */
    isDevtoolsOpen(): boolean;
    /** Whether this window's engine lets DevTools open, read back from the
     * engine: false when the app launched with DevTools turned off (see
     * {@linkcode Deno.desktop.devtools}). */
    isDevtoolsEnabled(): boolean;
    reload(): void;

    setApplicationMenu(menu: MenuItem[]): void;
    /** Pop up a context menu inside this window.
     *
     * `x` and `y` are window-relative CSS pixels with a top-left origin — the
     * same space as a DOM mouse event's `clientX` / `clientY`, so those can be
     * forwarded unchanged:
     *
     * ```ts
     * win.addEventListener("mousedown", (e) => {
     *   if (e.button === 2) win.showContextMenu(e.clientX, e.clientY, menu);
     * });
     * ```
     *
     * The platform may shift the menu to keep it on screen when it does not
     * fit below or to the right of that point.
     *
     * Resolves once the menu closed, with the chosen item's id, or `null`
     * when it was dismissed (Escape, a click outside); a
     * `"contextmenuclose"` event fires then too. It never blocks the app.
     * Where the backend can't report the close
     * ({@linkcode Deno.desktop.MenuCapabilities.contextClosed} false) it
     * resolves with `null` at once.
     *
     * Items' `accelerator`s are shown but not bound in a context menu. In
     * the application menu ({@linkcode BrowserWindow.setApplicationMenu})
     * they fire their items from the keyboard while the window has the focus,
     * on every OS: the syntax of {@linkcode Deno.desktop.shortcuts}
     * (`"CommandOrControl+Shift+K"`), an accelerator that doesn't parse is
     * ignored.
     */
    showContextMenu(
      x: number,
      y: number,
      menu: MenuItem[],
    ): Promise<string | null>;

    getNativeWindow(): Deno.UnsafeWindowSurface;

    addEventListener<K extends keyof BrowserWindowEventMap>(
      type: K,
      listener: (this: BrowserWindow, ev: BrowserWindowEventMap[K]) => any,
      options?: boolean | AddEventListenerOptions,
    ): void;
    addEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener<K extends keyof BrowserWindowEventMap>(
      type: K,
      listener: (this: BrowserWindow, ev: BrowserWindowEventMap[K]) => any,
      options?: boolean | EventListenerOptions,
    ): void;
    removeEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | EventListenerOptions,
    ): void;
  }

  interface DockReopenDetail {
    hasVisibleWindows: boolean;
  }

  interface DockEventMap {
    menuclick: CustomEvent<MenuClickDetail>;
    reopen: CustomEvent<DockReopenDetail>;
  }

  type DockEventHandlers = {
    [K in keyof DockEventMap as `on${K}`]:
      | ((this: Dock, ev: DockEventMap[K]) => any)
      | null;
  };

  export interface Dock extends DockEventHandlers {}

  /** App-level dock / taskbar handle.
   *
   * A `"reopen"` event fires on macOS when the user clicks the dock icon;
   * the default behavior of showing the last hidden window is swallowed,
   * so listeners decide what (if anything) to do.
   */
  export class Dock extends EventTarget {
    constructor();

    /** Set a short text badge on the app's dock icon (macOS) or taskbar
     * icon (Windows), or prefix the focused window's title on Linux.
     * Pass `null` or an empty string to clear the badge. */
    setBadge(text: string | null): void;

    /** Bounce the dock icon (macOS), flash the focused window's taskbar
     * button (Windows), or set the urgency hint on the focused window
     * (Linux).
     *
     * When `critical` is `false` (the default) this triggers a single
     * bounce; when `true` it bounces continuously until the app is
     * focused. */
    bounce(critical?: boolean): void;

    /** Set a custom right-click menu on the app's dock icon. Pass
     * `null` to remove any menu previously set.
     *
     * macOS only. Click events are delivered as `"menuclick"` events on
     * {@linkcode Deno.dock}. No-op on Windows and Linux. */
    setMenu(menu: MenuItem[] | null): void;

    /** Show or hide the app's dock icon.
     *
     * macOS only — controls the app's activation policy. No-op on
     * Windows and Linux. */
    setVisible(visible: boolean): void;

    addEventListener<K extends keyof DockEventMap>(
      type: K,
      listener: (this: Dock, ev: DockEventMap[K]) => any,
      options?: boolean | AddEventListenerOptions,
    ): void;
    addEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener<K extends keyof DockEventMap>(
      type: K,
      listener: (this: Dock, ev: DockEventMap[K]) => any,
      options?: boolean | EventListenerOptions,
    ): void;
    removeEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | EventListenerOptions,
    ): void;
  }

  /** App-level dock / taskbar singleton. */
  export const dock: Dock;

  /** The tray icon's bounding rectangle in screen coordinates, in the same
   * top-left-origin space as {@linkcode BrowserWindow.setPosition}. Use it to
   * anchor a popover window under the icon. */
  export interface TrayBounds {
    x: number;
    y: number;
    width: number;
    height: number;
  }

  export interface TrayPanelOptions {
    /** URL to load in the panel window. */
    url?: string;
    /** Panel width in pixels. @default {360} */
    width?: number;
    /** Panel height in pixels. @default {480} */
    height?: number;
    /** Hide the panel when it loses focus (click-outside to dismiss).
     * @default {true} */
    hideOnBlur?: boolean;
    /** Override where the panel is placed. Receives the tray icon's bounds
     * and the panel size, and returns the top-left screen position. The
     * default centers the panel horizontally under the icon — correct for a
     * top menu bar; provide this to place it elsewhere (e.g. above a
     * bottom-edge taskbar). */
    position?: (
      trayBounds: TrayBounds,
      panelSize: { width: number; height: number },
    ) => { x: number; y: number };
  }

  /** Handle to a tray-attached popover window created by
   * {@linkcode Tray.attachPanel}. */
  export interface TrayPanel {
    /** The underlying panel window — use it to `bind()`, `executeJs()`,
     * open devtools, etc. */
    readonly window: BrowserWindow;
    /** Whether the panel is currently shown. */
    readonly visible: boolean;
    /** Show the panel, positioned under the tray icon. */
    show(): void;
    /** Hide the panel. */
    hide(): void;
    /** Toggle the panel's visibility. */
    toggle(): void;
    /** Detach the panel: remove the tray/blur listeners and close the
     * window. */
    destroy(): void;
  }

  interface TrayEventMap {
    click: MouseEvent;
    dblclick: MouseEvent;
    menuclick: CustomEvent<MenuClickDetail>;
  }

  type TrayEventHandlers = {
    [K in keyof TrayEventMap as `on${K}`]:
      | ((this: Tray, ev: TrayEventMap[K]) => any)
      | null;
  };

  export interface Tray extends TrayEventHandlers {}

  /** A persistent icon in the OS status area (macOS menu bar extras,
   * Windows system tray, Linux AppIndicator).
   *
   * The icon is removed from the OS when {@linkcode Tray.destroy} is
   * called. Multiple trays may be created.
   */
  export class Tray extends EventTarget implements Disposable {
    constructor();

    readonly trayId: number;

    /** Set the tray icon image from PNG-encoded bytes. */
    setIcon(pngBytes: Uint8Array): void;

    /** Set the tray icon used in OS dark mode. Pass `null` to clear
     * it. */
    setIconDark(pngBytes: Uint8Array | null): void;

    /** Set the tooltip shown on hover. Pass `null` or an empty string
     * to clear the tooltip. */
    setTooltip(text: string | null): void;

    /** Set the right-click context menu. Click events are delivered as
     * `"menuclick"` events on the tray. Pass `null` to remove any
     * menu previously set. */
    setMenu(menu: MenuItem[] | null): void;

    /** The tray icon's bounding rectangle in screen coordinates, or `null`
     * if the icon has no on-screen position yet or the platform can't report
     * it. Typically called from a `"click"` handler to position a popover
     * {@linkcode BrowserWindow} (created with `frameless` + `noActivate`)
     * under the icon. */
    getBounds(): TrayBounds | null;

    /** Attach a frameless, non-activating popover window to this tray icon
     * (the classic menu-bar-app pattern). The returned panel toggles on tray
     * click, is positioned under the icon via {@linkcode Tray.getBounds}, and
     * hides when it loses focus.
     *
     * Convenience built on the primitives; for full control create a
     * `frameless` + `noActivate` {@linkcode BrowserWindow} yourself.
     *
     * ```ts
     * const tray = new Deno.Tray();
     * tray.setIcon(iconBytes);
     * const panel = tray.attachPanel({ url: "https://localhost:8000/panel" });
     * panel.window.bind("doThing", async () => { ... });
     * ```
     *
     * Pass a string as shorthand for `{ url }`. On Linux the icon position
     * can't be queried, so the panel shows at its last position rather than
     * anchored to the icon. */
    attachPanel(options: TrayPanelOptions | string): TrayPanel;

    /** Remove the tray icon from the OS status area. The instance must
     * not be used after this call. */
    destroy(): void;

    [Symbol.dispose](): void;

    addEventListener<K extends keyof TrayEventMap>(
      type: K,
      listener: (this: Tray, ev: TrayEventMap[K]) => any,
      options?: boolean | AddEventListenerOptions,
    ): void;
    addEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ): void;
    removeEventListener<K extends keyof TrayEventMap>(
      type: K,
      listener: (this: Tray, ev: TrayEventMap[K]) => any,
      options?: boolean | EventListenerOptions,
    ): void;
    removeEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | EventListenerOptions,
    ): void;
  }

  /** App-level events of a `deno desktop` app: deep links and files the
   * OS hands the app, and launches forwarded by a second instance.
   *
   * A deep link is a URL with one of the schemes the app registers
   * (`desktop.app.deepLinks` in `deno.json`, e.g. `["acme"]` for
   * `acme://…`). How one reaches the app depends on the OS:
   *
   * - macOS routes links (and files opened with the app) to the running app:
   *   `"openurl"` / `"openfile"`, including the link that launched it.
   * - Windows and Linux start a new process with the link or file in its
   *   arguments. At a cold start it is in {@linkcode Deno.desktop.launchUrls}
   *   / {@linkcode Deno.desktop.launchFiles}. While the app runs, with
   *   `desktop.app.singleInstance` on, the new process forwards its
   *   arguments to the running one (`"secondinstance"`) and exits; without
   *   it a second instance starts.
   *
   * Events that arrive before the app listens are held until the first
   * listener for that type is added (or its `on…` handler set), then
   * delivered to it. So add listeners at startup, and read `launchUrls`
   * there too:
   *
   * ```ts
   * for (const url of Deno.desktop.launchUrls) route(url);
   * Deno.desktop.addEventListener("openurl", (e) => route(e.detail.url));
   * Deno.desktop.addEventListener("secondinstance", (e) => {
   *   for (const url of e.detail.urls) route(url);
   * });
   * ```
   *
   * **Everything delivered here is untrusted input.** Any program running as
   * the same user can start the app with arbitrary arguments, open any URL
   * with it, or forward a launch to it. Check the URL's scheme and validate
   * the rest before acting on it, and don't treat a path as one the user
   * chose.
   *
   * @category Desktop
   */
  export namespace desktop {
    /** Detail of an `"openurl"` event. */
    export interface OpenUrlDetail {
      /** The URL as the OS delivered it (not validated). */
      url: string;
    }

    /** Detail of an `"openfile"` event. */
    export interface OpenFileDetail {
      /** Absolute filesystem path of the opened file. */
      path: string;
    }

    /** Detail of a `"secondinstance"` event. */
    export interface SecondInstanceDetail {
      /** The second launch's arguments, after the executable name. */
      args: string[];
      /** The second launch's working directory. */
      cwd: string;
      /** The deep links in `args`: absolute URLs with a registered scheme. */
      urls: string[];
      /** The existing paths in `args` (relative ones resolved against
       * `cwd`, and `file:` URLs), as absolute paths. */
      files: string[];
    }

    /** What this backend can do on this OS
     * ({@linkcode Deno.desktop.windowCapabilities}). A method for a feature
     * reported `false` changes nothing (and returns `false` where it returns
     * a boolean): Linux has no title bar styles or backdrops, CEF no
     * backdrops, Wayland can't place windows. */
    export interface WindowCapabilities {
      /** maximize / minimize / fullscreen. */
      state: boolean;
      /** The `maximize` … `leavefullscreen` events. */
      stateEvents: boolean;
      /** setMinimumSize / setMaximumSize. */
      sizeConstraints: boolean;
      screens: boolean;
      /** The `"displaychanged"` event. */
      displayEvents: boolean;
      titleBarHidden: boolean;
      titleBarHiddenInset: boolean;
      windowButtonPosition: boolean;
      mica: boolean;
      acrylic: boolean;
      tabbed: boolean;
      vibrancy: boolean;
      /** getNormalBounds tracks the bounds to return to. */
      normalBounds: boolean;
      /** {@linkcode quitOnLastWindowClosed} works. */
      keepAlive: boolean;
      /** setPosition / setBounds can move the window (not on Wayland). */
      setPosition: boolean;
      /** The `dragenter` / `dragover` / `dragleave` / `drop` window events
       * fire. */
      fileDrop: boolean;
      /** `dragenter` / `dragover` already carry the paths (not on WebView2,
       * which reveals them only on the drop). */
      fileDropEnterPaths: boolean;
      /** {@linkcode BrowserWindow.startDrag} works. */
      fileDragOut: boolean;
      /** {@linkcode Deno.desktop.dialog} works. */
      fileDialogs: boolean;
      /** One open dialog can pick files and directories (macOS). */
      fileDialogFilesAndDirectories: boolean;
      /** A dialog given a window is modal to it (a sheet on macOS); not on
       * CEF for Linux. */
      fileDialogModal: boolean;
    }

    /** A file-type filter of a file dialog: a label and extensions without
     * the dot (`"*"` matches any file). */
    export interface FileFilter {
      name: string;
      extensions: string[];
    }

    /** Options shared by {@linkcode Deno.desktop.dialog} functions. */
    export interface FileDialogOptions {
      /** The window the dialog is modal to (a sheet on macOS). Default: an
       * app-level dialog. */
      // deno-lint-ignore no-explicit-any
      window?: BrowserWindow<any> | number;
      /** The dialog's title (on macOS shown as the panel's message line). */
      title?: string;
      /** A directory to start in, or a file path: an open dialog starts in
       * its directory, a save dialog also proposes its name. */
      defaultPath?: string;
      /** The accept button's label ("Import"). */
      buttonLabel?: string;
      /** File-type filters. Windows and Linux show them as a type menu;
       * macOS allows the union of every filter's extensions. */
      filters?: FileFilter[];
      /** Closes the dialog (the promise rejects with the signal's reason). */
      signal?: AbortSignal;
    }

    /** Options of {@linkcode Deno.desktop.dialog.showOpenDialog}. */
    export interface OpenDialogOptions extends FileDialogOptions {
      /** `openFile` (the default), `openDirectory` (the folder dialog; with
       * `openFile` too, either, on macOS only), `multiSelections`,
       * `showHiddenFiles`. */
      properties?: Array<
        "openFile" | "openDirectory" | "multiSelections" | "showHiddenFiles"
      >;
    }

    /** Options of {@linkcode Deno.desktop.dialog.showSaveDialog}. */
    export interface SaveDialogOptions extends FileDialogOptions {
      properties?: Array<"showHiddenFiles">;
    }

    /**
     * The OS's own file dialogs: `NSOpenPanel` / `NSSavePanel` on macOS,
     * `IFileOpenDialog` / `IFileSaveDialog` on Windows,
     * `GtkFileChooserNative` on Linux (through the xdg-desktop-portal when
     * GTK uses it: inside Flatpak / Snap, or with `GTK_USE_PORTAL=1`). The
     * same dialog on every backend of an OS, CEF included. The dialog runs
     * on the UI thread; the runtime never blocks on it.
     *
     * Both take an optional window first, as Electron's `dialog` does:
     * `showOpenDialog(win, options)` is `showOpenDialog({ ...options,
     * window: win })`. One file dialog is open at a time: another call
     * meanwhile rejects with `Deno.errors.Busy`. They reject with a
     * `TypeError` for wrong options and with an `Error` when the OS can't
     * show the dialog (or the backend has none: Winit;
     * {@linkcode windowCapabilities}`().fileDialogs`).
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const dialog: {
      /** Absolute paths of the chosen files / directories, or `null` when
       * the user cancelled. */
      showOpenDialog(options?: OpenDialogOptions): Promise<string[] | null>;
      showOpenDialog(
        // deno-lint-ignore no-explicit-any
        window: BrowserWindow<any>,
        options?: OpenDialogOptions,
      ): Promise<string[] | null>;
      /** The absolute path to save to, or `null` when the user cancelled.
       * The OS asks before replacing an existing file. */
      showSaveDialog(options?: SaveDialogOptions): Promise<string | null>;
      showSaveDialog(
        // deno-lint-ignore no-explicit-any
        window: BrowserWindow<any>,
        options?: SaveDialogOptions,
      ): Promise<string | null>;
    };

    /** What {@linkcode Deno.desktop.clipboard} supports on this backend /
     * OS. */
    export interface ClipboardCapabilities {
      text: boolean;
      html: boolean;
      image: boolean;
      formats: boolean;
      /** The `"change"` event fires. */
      changeEvents: boolean;
    }

    /** The system clipboard: text, HTML and PNG images. See
     * {@linkcode Deno.desktop.clipboard}. */
    export interface DesktopClipboard extends EventTarget {
      capabilities(): ClipboardCapabilities;
      /** The clipboard's text, or `""` when it holds none. */
      readText(): Promise<string>;
      /** Replace the clipboard with `text` (`""` clears it). */
      writeText(text: string): Promise<void>;
      /** The clipboard's HTML (a fragment or a document, as the source app
       * wrote it), or `""` when it holds none. */
      readHTML(): Promise<string>;
      /** Replace the clipboard with `html`, plus `text` as the plain-text
       * alternative other apps paste. Rejects with `Deno.errors.NotSupported`
       * where the clipboard has no HTML. */
      writeHTML(html: string, text?: string): Promise<void>;
      /** The clipboard's image as PNG bytes (converted from TIFF, a DIB or
       * any other image format the clipboard holds), or `null`. */
      readImage(): Promise<Uint8Array | null>;
      /** Replace the clipboard with a PNG image (also offered as TIFF on
       * macOS, `CF_DIBV5` on Windows, every gdk-pixbuf format on Linux).
       * Rejects with a `TypeError` for bytes that aren't a PNG. */
      writeImage(png: Uint8Array): Promise<void>;
      /** The kinds of content on the clipboard as MIME types:
       * `"text/plain"`, `"text/html"`, `"image/png"` (any image),
       * `"text/uri-list"` (files), `"text/rtf"`. Empty for an empty
       * clipboard. */
      availableFormats(): Promise<string[]>;
      /** Fired when the clipboard changes, this app's own writes included.
       * The OS watcher runs only while a listener is set: macOS has no
       * notification, so the pasteboard's change count is polled twice a
       * second; Windows uses `AddClipboardFormatListener`; Linux GTK's
       * `owner-change` (X11 needs the XFixes extension; on Wayland GTK only
       * hears of changes while one of the app's windows has focus). */
      onchange: ((this: DesktopClipboard, ev: Event) => any) | null;
      addEventListener(
        type: "change",
        listener: (this: DesktopClipboard, ev: Event) => any,
        options?: boolean | AddEventListenerOptions,
      ): void;
      addEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | AddEventListenerOptions,
      ): void;
      removeEventListener(
        type: "change",
        listener: (this: DesktopClipboard, ev: Event) => any,
        options?: boolean | EventListenerOptions,
      ): void;
      removeEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | EventListenerOptions,
      ): void;
    }

    /**
     * The system clipboard with rich formats (`navigator.clipboard` stays the
     * web's text-only API).
     *
     * Every read is capped at 64 MiB (a bigger content reads as absent) and
     * rejects if the app owning the clipboard doesn't answer within a few
     * seconds. Formats per OS: macOS `NSPasteboard` (string, HTML, PNG and
     * TIFF), Windows `CF_UNICODETEXT`, `HTML Format` (the `CF_HTML` header is
     * stripped on read), the registered `PNG` format and `CF_DIBV5`, Linux
     * the GTK `CLIPBOARD` selection (`text/html`, `image/png` and every
     * gdk-pixbuf format). On the Winit backend only text is available.
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const clipboard: DesktopClipboard;

    /** What {@linkcode Deno.desktop.shortcuts} can do here. */
    export interface ShortcutCapabilities {
      /** Registering can bind system-wide shortcuts in this session. */
      globalShortcuts: boolean;
      /** The user approves each shortcut and may pick another trigger
       * (the XDG GlobalShortcuts portal on Wayland). */
      userBinds: boolean;
    }

    /** The detail of a `"shortcut"` event: the canonical accelerator the
     * registration resolved with. */
    export interface ShortcutEventDetail {
      accelerator: string;
    }

    /** System-wide keyboard shortcuts. See
     * {@linkcode Deno.desktop.shortcuts}. */
    export interface DesktopShortcuts extends EventTarget {
      capabilities(): ShortcutCapabilities;
      /** Bind `accelerator` system-wide. Resolves with its canonical form
       * (`"CommandOrControl+Shift+K"` → `"Ctrl+Shift+K"`, or
       * `"Shift+Super+K"` on macOS), which `callback` and the `"shortcut"`
       * event receive on each press.
       *
       * Rejects (the error's `code` names the case) with a `TypeError` for an
       * accelerator that doesn't parse or a printable key without a modifier
       * other than Shift (`"invalid"`), `Deno.errors.AlreadyExists` when
       * another application holds the combination (`"conflict"`) or this app
       * registered it already (`"already_registered"`),
       * `Deno.errors.NotSupported` where there are no global shortcuts
       * (`"not_supported"`), `Deno.errors.PermissionDenied` when the user
       * declined it (`"denied"`, the Wayland portal), or an `Error`
       * (`"failed"`). */
      register(
        accelerator: string,
        callback?: (accelerator: string) => void,
      ): Promise<string>;
      /** Release a shortcut this app registered (any spelling). False if it
       * wasn't registered. */
      unregister(accelerator: string): boolean;
      /** Release every shortcut this app registered. */
      unregisterAll(): void;
      /** Whether this app holds `accelerator` (any spelling). */
      isRegistered(accelerator: string): boolean;
      /** The canonical accelerators this app holds, in registration
       * order. */
      list(): string[];
      /** The canonical form of an accelerator, or `null` when it doesn't
       * parse. */
      canonicalize(accelerator: string): string | null;
      /** A registered shortcut was pressed. */
      onshortcut:
        | ((
          this: DesktopShortcuts,
          ev: CustomEvent<ShortcutEventDetail>,
        ) => any)
        | null;
      addEventListener(
        type: "shortcut",
        listener: (
          this: DesktopShortcuts,
          ev: CustomEvent<ShortcutEventDetail>,
        ) => any,
        options?: boolean | AddEventListenerOptions,
      ): void;
      addEventListener(
        type: string,
        listener: EventListenerOrEventListenerObject,
        options?: boolean | AddEventListenerOptions,
      ): void;
      removeEventListener(
        type: "shortcut",
        listener: (
          this: DesktopShortcuts,
          ev: CustomEvent<ShortcutEventDetail>,
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
     * Global shortcuts: key combinations that reach the app whichever app
     * has the keyboard focus.
     *
     * ```ts
     * await Deno.desktop.shortcuts.register("CommandOrControl+Shift+Space",
     *   () => win.show());
     * ```
     *
     * Accelerators use the menu syntax: modifiers (`CommandOrControl`,
     * `Control`, `Alt`/`Option`, `Shift`, `Super`/`Meta`, and `Command` on
     * macOS) then one key (`A`-`Z`, `0`-`9`, punctuation, `F1`-`F24`,
     * `Space`, `Enter`, `Escape`, arrows, `PageUp`, `Num0`-`Num9`, media
     * keys, ...). Per OS: macOS Carbon hot keys (no Accessibility
     * permission needed), Windows `RegisterHotKey`, X11 key grabs, and on
     * Wayland the XDG GlobalShortcuts portal, where the desktop asks the
     * user to approve each shortcut; without that portal (or on the Winit
     * backend) registration rejects with `Deno.errors.NotSupported`.
     * Shortcuts are released when the app exits.
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const shortcuts: DesktopShortcuts;

    /** Whether the app starts at login: `"requires-approval"` means it is
     * registered but the user has to allow it in the system settings
     * (macOS Login Items, or a Windows startup entry the user turned
     * off). */
    export type LaunchAtLoginState =
      | "enabled"
      | "disabled"
      | "requires-approval"
      | "not-supported";

    /**
     * Start the app when the user logs in: macOS 13+ `SMAppService` (the app
     * bundle becomes a login item), Windows an `HKCU\...\Run` value, Linux
     * an XDG autostart entry (`~/.config/autostart/<app id>.desktop`). The
     * entry is named after the app's identifier. `set` resolves with the
     * state afterwards and rejects with the OS's message when it failed.
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const launchAtLogin: {
      get(): Promise<LaunchAtLoginState>;
      set(enabled: boolean): Promise<LaunchAtLoginState>;
    };

    /**
     * DevTools control. `enabled` is false when the app was launched with
     * DevTools turned off (`LAUFEY_INSPECTABLE=0`, or `"inspectable": false`
     * in the packaged app's `laufey-launch.json`): then neither these calls
     * nor the user (F12, the context menu, Safari's Develop menu, remote
     * debugging) can open them. The per-window functions are the
     * `BrowserWindow` methods.
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const devtools: {
      readonly enabled: boolean;
      // deno-lint-ignore no-explicit-any
      open(window: BrowserWindow<any>, options?: OpenDevtoolsOptions): void;
      // deno-lint-ignore no-explicit-any
      close(window: BrowserWindow<any>): void;
      toggle(
        // deno-lint-ignore no-explicit-any
        window: BrowserWindow<any>,
        options?: OpenDevtoolsOptions,
      ): void;
      // deno-lint-ignore no-explicit-any
      isOpen(window: BrowserWindow<any>): boolean;
    };

    export interface DesktopEventMap {
      /** Displays were added, removed, rearranged or rescaled, or a work
       * area changed; read {@linkcode screens} again. */
      displaychanged: Event;
      /** Cancelable: {@linkcode quit} was called. */
      beforequit: Event;
      /** A URL routed to the running app (macOS). */
      openurl: CustomEvent<OpenUrlDetail>;
      /** A file opened with the running app (macOS). */
      openfile: CustomEvent<OpenFileDetail>;
      /** The app was launched again while running, with
       * `desktop.app.singleInstance` on. */
      secondinstance: CustomEvent<SecondInstanceDetail>;
      /** A click on one of the app's notifications that no live
       * `Notification` owns (see {@linkcode NotificationResponseDetail}). */
      notificationresponse: CustomEvent<NotificationResponseDetail>;
    }

    /** The deep links the app was launched with: the arguments of this
     * process that are absolute URLs with a registered scheme, plus (macOS)
     * the links delivered before the first `"openurl"` listener was added.
     * Taken on first read; a link delivered after that is an `"openurl"`
     * event instead, so each link is seen once. */
    export const launchUrls: readonly string[];

    /** The files the app was launched with: the arguments of this process
     * that name an existing path (or are `file:` URLs), as absolute paths,
     * plus (macOS) the files delivered before the first `"openfile"`
     * listener was added. Taken on first read, like `launchUrls`. */
    export const launchFiles: readonly string[];

    /** Clicks on the app's notifications that arrived before the first
     * `"notificationresponse"` listener was added: on macOS and Windows the
     * click that launched the app (`launch: true`). Taken on first read, like
     * `launchUrls`. */
    export const launchNotificationResponses:
      readonly NotificationResponseDetail[];

    /** A click on one of the app's notifications that no live
     * `Notification` object owns: one an earlier run of the app posted, a
     * {@linkcode Deno.desktop.notifications.schedule}d one, or the click that
     * launched the app. Delivered as `Deno.desktop`'s
     * `"notificationresponse"` event, or in
     * {@linkcode Deno.desktop.launchNotificationResponses}. */
    export interface NotificationResponseDetail {
      /** The notification's tag. */
      tag: string;
      /** The clicked action button's `action`, or `null` for the body. */
      action: string | null;
      /** The notification's `data` (parsed back from JSON), if it had any. */
      data: unknown;
      /** It arrived before the app listened: the click that launched it (or
       * one made while it was starting). */
      launch: boolean;
    }

    /** What notifications can do here (`Deno.desktop.notifications`). */
    export interface NotificationCapabilities {
      show: boolean;
      /** `schedule()` delivers at its time, at least while the app runs. */
      schedule: boolean;
      /** The OS delivers a scheduled notification while the app isn't
       * running (macOS, Windows). On Linux the runtime's own timer delivers
       * it while the app runs; the schedule is kept with the app's data and
       * re-armed at the next launch, which delivers one whose time passed
       * meanwhile at once. */
      schedulePersists: boolean;
      /** Action buttons are shown. */
      actions: boolean;
      clicks: boolean;
      /** A click while the app isn't running launches it and arrives in
       * `launchNotificationResponses` (macOS, Windows; not Linux, whose
       * notification servers send clicks to the process that posted). */
      coldStart: boolean;
    }

    /** Options of {@linkcode Deno.desktop.notifications.schedule}. */
    export interface ScheduledNotificationOptions {
      title: string;
      body?: string;
      /** When to deliver it; a time in the past shows it now. */
      at: Date | number;
      /** Identifies it to `cancel()` and in responses; a random UUID when
       * omitted. A later notification with the same tag replaces it. */
      tag?: string;
      actions?: NotificationAction[];
      /** Stored with the notification as JSON (at most 4 KiB). */
      data?: unknown;
      /** A `data:` URL (PNG). */
      icon?: string;
      silent?: boolean;
      requireInteraction?: boolean;
    }

    /** A scheduled notification not delivered yet. */
    export interface ScheduledNotification {
      tag: string;
      title: string;
      body: string;
      at: Date;
      data: unknown;
      actions: NotificationAction[];
    }

    /** Scheduled notifications and notification capabilities.
     *
     * Mechanisms: macOS `UNUserNotificationCenter` (a calendar or
     * time-interval trigger), Windows toasts (`ScheduledToastNotification`;
     * the app registers its AppUserModelID and a COM activator per user, under
     * `HKCU\Software\Classes`, the first time it posts), Linux
     * `org.freedesktop.Notifications` with the runtime's own timer. */
    export const notifications: {
      capabilities(): NotificationCapabilities;
      /** Schedule a notification; resolves with its tag. Rejects with
       * `Deno.errors.NotSupported` where it can't be scheduled. Its clicks
       * arrive as `"notificationresponse"` events. */
      schedule(options: ScheduledNotificationOptions): Promise<string>;
      /** The pending scheduled notifications, soonest first. */
      getScheduled(): Promise<ScheduledNotification[]>;
      /** Cancel the scheduled notification `tag`, and remove a delivered one
       * with that tag from the notification center. */
      cancel(tag: string): void;
      /** `Notification.requestPermission()`, plus `{ provisional: true }`:
       * quiet authorization, which macOS grants without a prompt (the
       * notifications go to the Notification Center without a banner until
       * the user keeps them). Windows and Linux have no prompt: their status
       * is the user's setting / whether a notification server runs. */
      requestPermission(
        options?: { provisional?: boolean },
      ): Promise<"granted" | "denied" | "prompt" | "unsupported">;
    };

    /** What menus can do here. */
    export interface MenuCapabilities {
      appMenu: boolean;
      /** Application-menu accelerators fire their items from the keyboard. */
      accelerators: boolean;
      contextMenu: boolean;
      /** `showContextMenu()` resolves when the menu closes. */
      contextClosed: boolean;
      /** Item icons are drawn. */
      icons: boolean;
      /** Item tooltips are shown. */
      tooltips: boolean;
    }

    /** The menu capabilities of this backend and OS. */
    export function menuCapabilities(): MenuCapabilities;

    /** Who handles a deep-link scheme, from this app's point of view:
     *
     * - `"self"`: this app (this executable or bundle).
     * - `"other"`: another app. A link with the scheme, including an OAuth
     *   callback, goes to that app, not this one.
     * - `"none"`: no app; opening a link with the scheme fails.
     */
    export type SchemeOwner = "self" | "other" | "none";

    /** The result of {@linkcode Deno.desktop.getSchemeOwner}. */
    export interface SchemeOwnerInfo {
      owner: SchemeOwner;
      /** What identifies the current handler, for display only: an
       * executable path (Windows), a bundle id (macOS) or a `.desktop` file
       * id (Linux). It comes from the OS's handler database, which any
       * program of the same user can write: don't act on it. */
      handler?: string;
    }

    /** The result of {@linkcode Deno.desktop.registerScheme}. */
    export interface RegisterSchemeResult {
      /** Whether this app handles the scheme after the call. */
      registered: boolean;
      /** The handler after the call. */
      owner: SchemeOwner;
      /** As in {@linkcode SchemeOwnerInfo.handler}. */
      handler?: string;
      /** Why the app does not handle the scheme, when it doesn't (another
       * app handles it, a Windows "UserChoice" overrides the registration,
       * `xdg-mime` is not installed, the app is not running from a bundle,
       * …). */
      reason?: string;
    }

    /** Options of {@linkcode Deno.desktop.registerScheme}. */
    export interface RegisterSchemeOptions {
      /** Take the scheme over from another app. Only `true` forces.
       *
       * **Only on an explicit user action** (e.g. "Make this app the handler
       * for `acme:` links" in the app's settings): the other app loses the
       * scheme. On macOS this changes the system's default handler for the
       * scheme, which the OS may confirm with the user. A Windows
       * "UserChoice" (the user picked a handler in Settings) cannot be
       * overridden by any app; the result then reports `registered: false`.
       */
      force?: boolean;
    }

    /** Who handles one of the app's deep-link schemes (declared in
     * `desktop.app.deepLinks`), so the app can tell whether a link with the
     * scheme will reach it. Rejects with a `TypeError` for any other scheme.
     *
     * Check it before starting a sign-in whose callback uses the scheme: if
     * another app handles it (`"other"`), the OS would hand that app the
     * callback (RFC 8252 §8.6), so use a loopback redirect instead, or ask
     * the user whether to make this app the handler
     * ({@linkcode Deno.desktop.registerScheme} with `force`).
     *
     * The answer is a snapshot. Any program running as the same user can
     * register itself for the scheme at any time and the OS offers no
     * protection against that, so `"self"` is not proof that a callback
     * reaches this app: keep PKCE (and `state`) on every flow.
     *
     * - Windows: the `UserChoice` for the scheme if the user made one, else
     *   `HKCU\Software\Classes\<scheme>`, else
     *   `HKLM\Software\Classes\<scheme>`; this app's is the one whose
     *   command runs this executable.
     * - macOS: the LaunchServices default handler, by bundle id.
     * - Linux: the `x-scheme-handler/<scheme>` default of the freedesktop
     *   `mimeapps.list` files, then `mimeinfo.cache`; this app's is its own
     *   `<app id>.desktop` entry, or one that runs this executable.
     */
    export function getSchemeOwner(scheme: string): Promise<SchemeOwnerInfo>;

    /** Register this app as the handler of one of its deep-link schemes
     * (declared in `desktop.app.deepLinks`); rejects with a `TypeError` for
     * any other scheme.
     *
     * The runtime already does this at startup, in the background, for every
     * declared scheme that no app handles, and refreshes this app's own
     * registration when it is out of date (e.g. the app moved). It never
     * takes a scheme another app handles. Call this to retry, or, with
     * `force`, to take the scheme over from another app on an explicit user
     * action.
     *
     * Without `force` the app is registered only when no app handles the
     * scheme or this app already does; otherwise the result is
     * `registered: false` with the other app as the owner. The result always
     * reports the handler the OS sees after the call.
     *
     * - Windows: writes `HKCU\Software\Classes\<scheme>` (`URL Protocol`,
     *   `DefaultIcon`, `shell\open\command` = `"<this exe>" "%1"`). A
     *   per-user registration shadows a machine-wide one.
     * - macOS: registers the app bundle with LaunchServices; with `force`,
     *   also makes it the scheme's default handler. Requires running from
     *   the app bundle.
     * - Linux: installs `~/.local/share/applications/<app id>.desktop`
     *   (hidden from menus) and runs `xdg-mime default` for the scheme;
     *   `registered` is `false` with a `reason` when `xdg-mime` is missing.
     *   Needs the app id (`desktop.app.identifier`, or the one the packager
     *   derived into the launch configuration), which names the entry.
     *
     * Not available in a development run (`deno desktop --hmr`, a dev
     * server): the result is `registered: false`. The executable path
     * registered is always the running app's own, never from input.
     *
     * Registering needs no privileges beyond the user's own: every write is
     * per-user (Windows `HKCU`, the user's LaunchServices database, the
     * user's XDG directories).
     */
    export function registerScheme(
      scheme: string,
      options?: RegisterSchemeOptions,
    ): Promise<RegisterSchemeResult>;

    /** What {@linkcode Deno.desktop.passkeys.capabilities} reports. */
    export interface PasskeyCapabilities {
      /** Touch ID / iCloud Keychain (macOS), Windows Hello (Windows). */
      platformAuthenticator: boolean;
      /** Roaming security keys (USB / NFC / BLE). */
      securityKeys: boolean;
    }

    /** Options of {@linkcode Deno.desktop.passkeys.create} / `get`. */
    export interface PasskeyRequestOptions {
      /** The window the OS sheet / dialog is anchored to. Defaults to the
       * focused window (on macOS the key, main or first visible window; on
       * Windows the foreground window when it is the app's, else its first
       * visible window). With no visible window (e.g. before the initial
       * window has loaded) the request resolves with `unknown`, "no window to
       * anchor the passkey request to". */
      // deno-lint-ignore no-explicit-any
      window?: BrowserWindow<any> | number;
    }

    /**
     * Native passkeys: WebAuthn ceremonies through the OS platform
     * authenticator, for an app whose page origin (a custom scheme, a
     * loopback address) can't satisfy the RP ID the browser engine checks.
     *
     * Strings in, strings out, in the wire format of
     * `@clerk/electron-passkeys`, so a preload can expose this unchanged to
     * `@clerk/electron/passkeys`: `optionsJson` is
     * `PublicKeyCredentialCreationOptions` / `RequestOptions` as JSON with
     * unpadded base64url binary members, and every request resolves with a
     * JSON envelope, `{"ok":true,"credential":{...}}` or
     * `{"ok":false,"error":{"code","message"}}`, `code` one of `cancelled`,
     * `invalid_rp`, `not_supported`, `timeout`, `unknown`. It rejects only
     * on a wrong argument type.
     *
     * **Security.** Treat `optionsJson` as untrusted input and the result as
     * a credential for the RP: call this only from the app's own trusted
     * code, never on behalf of arbitrary web content. This path skips the
     * browser's origin check; on Windows nothing ties the RP ID to the app,
     * so check it against the RP IDs the app expects.
     *
     * One ceremony runs at a time; a request made meanwhile resolves with
     * `unknown` ("a passkey request is already in progress").
     *
     * **Per OS.**
     * - macOS 12+: the app's code signature must carry
     *   `com.apple.developer.associated-domains` = `webcredentials:<rp-id>`
     *   (with a provisioning profile), and
     *   `https://<rp-id>/.well-known/apple-app-site-association` must list
     *   `<TeamID>.<bundle-id>`; otherwise every request is `invalid_rp`.
     *   The OS builds clientDataJSON (origin `https://<rp-id>`).
     * - Windows 10 1903+: `webauthn.dll`; any RP ID; clientDataJSON is built
     *   with origin `https://<rp-id>`. A request without `timeout` ends
     *   after 60 s.
     * - Linux: not supported (`capabilities()` reports none and requests
     *   resolve with `not_supported`).
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const passkeys: {
      /** What a request can use right now. */
      capabilities(): Promise<PasskeyCapabilities>;
      /** A registration ceremony (`navigator.credentials.create`). */
      create(
        optionsJson: string,
        options?: PasskeyRequestOptions,
      ): Promise<string>;
      /** An authentication ceremony (`navigator.credentials.get`). */
      get(
        optionsJson: string,
        options?: PasskeyRequestOptions,
      ): Promise<string>;
    };

    /** What {@linkcode Deno.desktop.authSession.capabilities} reports. */
    export interface AuthSessionCapabilities {
      /** An OS auth session exists: macOS 10.15+ (`ASWebAuthenticationSession`).
       * `false` on Windows and Linux. */
      supported: boolean;
      /** `ephemeral: true` is honored (macOS). */
      ephemeral: boolean;
      /** `callbackUrl` (an https callback) works: macOS 14.4+. */
      httpsCallback: boolean;
    }

    /** Options of {@linkcode Deno.desktop.authSession.start}. Exactly one of
     * `callbackScheme` and `callbackUrl`. */
    export interface AuthSessionStartOptions {
      /** The sign-in page (http or https; percent-encode anything outside
       * printable ASCII), e.g. the provider's authorization URL with its
       * PKCE challenge and `state`. */
      url: string;
      /** The custom scheme the sign-in ends at (`"myapp"`, no `:`): the
       * first navigation to `myapp:…` completes the session. Not `http`,
       * `https`, `file`, `about`, `data`, `javascript`, `blob`, `ws`, `wss`. */
      callbackScheme?: string;
      /** An https callback (`"https://example.com/auth/done"`, no port,
       * query or fragment): a navigation to that host and path completes the
       * session. macOS 14.4+, and the app needs the host as an associated
       * domain. */
      callbackUrl?: string;
      /** A private browser session: no cookies shared with the browser, and
       * no "“App” Wants to Use “example.com” to Sign In" prompt. Default
       * `false`. */
      ephemeral?: boolean;
      /** The window the sheet is anchored to. Defaults to the key window (or
       * the app's first visible window); with no window the OS shows the
       * sheet on a window of its own. */
      // deno-lint-ignore no-explicit-any
      window?: BrowserWindow<any> | number;
    }

    /** The `code` of an {@linkcode AuthSessionError}. */
    export type AuthSessionErrorCode =
      | "cancelled"
      | "not_supported"
      | "invalid"
      | "busy"
      | "failed";

    /** How {@linkcode Deno.desktop.authSession.start} rejects. */
    export interface AuthSessionError extends Error {
      name: "AuthSessionError";
      /**
       * - `cancelled`: the user closed the sheet or declined the prompt, the
       *   anchor window closed, or the app is quitting.
       * - `not_supported`: no OS auth session (Windows, Linux), or an https
       *   callback before macOS 14.4.
       * - `invalid`: a bad `url`, callback or window.
       * - `busy`: another session is in progress (one at a time per app).
       * - `failed`: the OS refused or failed (`message` says why).
       */
      code: AuthSessionErrorCode;
    }

    /**
     * OS auth sessions: a sign-in in the user's browser that ends at a
     * callback URL (RFC 8252, "OAuth 2.0 for Native Apps").
     *
     * - **macOS** (10.15+): `ASWebAuthenticationSession`, a sheet on the
     *   app's window backed by Safari (or the default browser when it
     *   supports it). Closing the sheet, or `cancel()`, rejects with
     *   `cancelled`.
     * - **Windows, Linux**: the OS has no equivalent; `capabilities()`
     *   reports none and `start()` rejects with `not_supported`. RFC 8252
     *   says to open the system browser and receive the redirect through a
     *   loopback listener or a claimed URL scheme (see
     *   {@linkcode Deno.desktop.registerScheme} and the `openurl` event);
     *   the browser gives no signal when its tab is closed, so keep a
     *   timeout and an in-app cancel button there.
     *
     * `start()` resolves with the full callback URL. Checking `state` and
     * redeeming the code with PKCE are the caller's job. Nothing is logged.
     *
     * ```ts
     * const caps = await Deno.desktop.authSession.capabilities();
     * if (caps.supported) {
     *   const { url } = await Deno.desktop.authSession.start({
     *     url: authorizeUrl,
     *     callbackScheme: "myapp",
     *     ephemeral: true,
     *   });
     *   const code = new URL(url).searchParams.get("code");
     * }
     * ```
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export const authSession: {
      /** What this platform supports. */
      capabilities(): AuthSessionCapabilities;
      /** Run a sign-in; resolves with the callback URL, rejects with an
       * {@linkcode AuthSessionError}. */
      start(options: AuthSessionStartOptions): Promise<{ url: string }>;
      /** End the running session because the app gave up on it (the page
       * cancelled, a timeout): its sheet closes and its `start()` rejects
       * with an {@linkcode AuthSessionError} of code `cancelled`, exactly
       * once; the next session can start. Returns `false`, and does
       * nothing, when no session is running, which is always the case on
       * Windows and Linux. */
      cancel(): boolean;
    };

    /**
     * Call a native function on the app's UI thread (the thread AppKit,
     * Win32 windows and GTK objects belong to: the process main thread with
     * the WebView backends, CEF's UI thread), and resolve with its return
     * value.
     *
     * `fn` is a C function `void* fn(void* context)`: a
     * `Deno.UnsafeFnPointer`, a `Deno.UnsafeCallback`, or a pointer value
     * (e.g. from `Deno.dlopen(...).symbols` through
     * `Deno.UnsafePointer.of`, or an extension's own function). It is called
     * with `context` (default `null`); its pointer-sized return value is
     * resolved as an unsigned bigint (meaningless for a `void` function).
     * The call is queued behind the UI work already posted and never runs
     * on the calling thread.
     *
     * **Full trust**: this is FFI, so it needs `--allow-ffi`, and a wrong
     * pointer or signature crashes the app. A `Deno.UnsafeCallback` does not
     * run JavaScript on the UI thread: the UI thread waits while the callback
     * runs on the JavaScript thread, so it must not wait for the UI thread
     * itself.
     *
     * Rejects without calling `fn` once the app is quitting (the UI thread's
     * event loop has ended), so a call never hangs.
     *
     * Not available in workers.
     *
     * @category Desktop
     * @experimental
     */
    export function runOnMainThread(
      // deno-lint-ignore no-explicit-any
      fn: UnsafeFnPointer<any> | UnsafeCallback<any> | PointerObject,
      context?: PointerValue,
    ): Promise<bigint>;

    /** Why a {@linkcode Deno.desktop.updater} step refused. */
    export type AppUpdateErrorCode =
      | "not_configured"
      | "invalid_manifest"
      | "signature"
      | "wrong_app"
      | "downgrade"
      | "rejected"
      | "no_platform"
      | "insecure_url"
      | "size_exceeded"
      | "integrity"
      | "unsafe_archive"
      | "bundle_mismatch"
      | "os_signature"
      | "install_not_writable"
      | "unsupported_layout"
      | "not_staged"
      | "busy"
      | "io";

    /** Options for fetching the manifest or the archive. */
    export interface AppUpdateFetchOptions {
      /** Extra trusted CA certificates (PEM), e.g. a test server's. */
      caCerts?: string[];
      /** DEV ONLY: accept `http://` to a loopback host. */
      allowInsecureLoopback?: boolean;
      /** Manifest request timeout (`check()` only). Default 30 000 ms. */
      timeoutMs?: number;
    }

    /** What {@linkcode Deno.desktop.updater.check} found. */
    export interface AppUpdateCheck {
      /** A newer, verified version for this platform. */
      available: boolean;
      /** The manifest's version. */
      version: string;
      /** The running version. */
      currentVersion: string;
      /** The running version is below the manifest's `minVersion`. */
      required: boolean;
      releaseNotes: string | null;
      publishedAt: string | null;
      /** The archive size in bytes. */
      size: number | null;
      /** This build's platform key, `<target>-<backend>`. */
      platform: string | null;
    }

    /** {@linkcode Deno.desktop.updater.status}. */
    export interface AppUpdateStatus {
      /** Whether updates can run (a key, identifier and version are baked in
       * and the install is replaceable); `reason` says why not. */
      configured: boolean;
      reason: string | null;
      version: string | null;
      appId: string | null;
      platform: string | null;
      install: string | null;
      kind: "macBundle" | "appDir" | "appImage" | null;
      phase: "idle" | "staged" | "swapping" | "swapped" | "rollingBack" | null;
      /** A swapped-in version awaiting {@linkcode Deno.desktop.updater.confirm}. */
      pendingVersion: string | null;
      stagedVersion: string | null;
      /** The last version rolled back after failing to start. */
      rejected: string | null;
      lastError: string | null;
      /** Launched by the updater after an update from this version. */
      updatedFrom: string | null;
      /** Launched after this version was rolled back. */
      rolledBackFrom: string | null;
      /** This is the first, unconfirmed launch of a new version. */
      trial: boolean;
    }

    /**
     * Full-app self-update: replaces the whole signed app (bundle, app
     * directory or AppImage), never patching it in place.
     *
     * Every manifest must be signed (ECDSA P-256) by the key baked into the
     * app at package time (deno.json `desktop.update.publicKey`); there is no
     * unsigned path. A manifest for another app, a version that is not newer
     * than the running one, an http URL, a download that is larger than
     * declared or hashes differently, an archive with unsafe entries, or a
     * staged app the OS signature check refuses (macOS: same Team ID +
     * Gatekeeper; Windows: same Authenticode signer) is refused with an
     * error whose `code` is an {@linkcode AppUpdateErrorCode}.
     *
     * ```ts
     * const u = Deno.desktop.updater;
     * if (u.status().trial) u.confirm(); // after the app proved healthy
     * const found = await u.check("https://updates.example.com/app.json");
     * if (found.available) {
     *   await u.download({ onProgress: (p) => console.log(p) });
     *   await u.stage();
     *   u.applyAndRelaunch();
     * }
     * ```
     *
     * An update not confirmed by its next launch is rolled back, and that
     * version is not offered again. Fires `"progress"` events (`detail:
     * { transferred, total }`) while downloading.
     *
     * @category Desktop
     * @experimental
     */
    export const updater: EventTarget & {
      /** Fetch and verify the signed manifest. */
      check(
        manifestUrl: string | URL,
        options?: AppUpdateFetchOptions,
      ): Promise<AppUpdateCheck>;
      /** Stream the verified archive next to the install (size-capped,
       * SHA-256 checked). */
      download(
        options?: AppUpdateFetchOptions & {
          onProgress?: (p: { transferred: number; total: number }) => void;
          signal?: AbortSignal;
        },
      ): Promise<{ version: string; size: number }>;
      /** Extract safely and check the staged app's OS code signature.
       * `allowUnsignedDev` (DEV ONLY) accepts an unsigned / ad-hoc running
       * app; it never weakens a signed one. */
      stage(options?: { allowUnsignedDev?: boolean }): Promise<{
        version: string;
        signature: { mode: string; identity: string | null };
      }>;
      /** Start the swap helper and quit (it relaunches the new version).
       * `quitting: false` when a `beforequit` / `close` listener refused;
       * `force: true` exits anyway. */
      applyAndRelaunch(options?: { force?: boolean }): { quitting: boolean };
      /** Confirm the running version after an update (deletes the previous
       * app). `false` when nothing was pending. */
      confirm(): boolean;
      status(): AppUpdateStatus;
    };

    /** The connected displays, primary first. */
    export function screens(): Screen[];
    /** The primary display, or `null` when the backend can't list them. */
    export function getPrimaryScreen(): Screen | null;
    /** What window features this backend supports on this OS. */
    export function windowCapabilities(): WindowCapabilities;

    /** Quit the app, like Electron's `app.quit()`: fires a cancelable
     * `"beforequit"` here, then a cancelable `close` on every open window;
     * if any listener calls `preventDefault()`, nothing happens and this
     * returns `false`. Otherwise the app shuts down as when its last window
     * closes (the remaining windows close without another event) and this
     * returns `true`.
     *
     * Quitting from the macOS app menu's Quit item (Cmd+Q, the `quit` role)
     * does not go through this and can't be canceled. */
    export function quit(): boolean;

    /** Whether the app quits when its last window closes (default `true`).
     * Creating a {@linkcode Deno.Tray} sets it to `false` unless the app set
     * it itself, so a tray / menu-bar app keeps running with no window;
     * destroying the tray does not quit the app (call {@linkcode quit}).
     * On macOS an app hidden from the Dock (`Deno.dock.setVisible(false)`)
     * also keeps running. A tray-only app that should never show its first
     * window sets `desktop.initialWindow.showOnFirstLoad: false` in
     * deno.json (or `"initialWindow"` in `.deno-desktop/app.json`); the
     * window is also left alone once the app calls `show()`, `hide()` or
     * `close()` on it before it first loads. */
    export let quitOnLastWindowClosed: boolean;

    export let ondisplaychanged: ((ev: Event) => any) | null;
    export let onbeforequit: ((ev: Event) => any) | null;
    export let onopenurl:
      | ((ev: CustomEvent<OpenUrlDetail>) => any)
      | null;
    export let onopenfile:
      | ((ev: CustomEvent<OpenFileDetail>) => any)
      | null;
    export let onsecondinstance:
      | ((ev: CustomEvent<SecondInstanceDetail>) => any)
      | null;
    export let onnotificationresponse:
      | ((ev: CustomEvent<NotificationResponseDetail>) => any)
      | null;

    export function addEventListener<K extends keyof DesktopEventMap>(
      type: K,
      listener: (ev: DesktopEventMap[K]) => any,
      options?: boolean | AddEventListenerOptions,
    ): void;
    export function addEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ): void;
    export function removeEventListener<K extends keyof DesktopEventMap>(
      type: K,
      listener: (ev: DesktopEventMap[K]) => any,
      options?: boolean | EventListenerOptions,
    ): void;
    export function removeEventListener(
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | EventListenerOptions,
    ): void;
    export function dispatchEvent(event: Event): boolean;
  }
}
