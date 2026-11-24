// Copyright 2018-2026 the Deno authors. MIT license.

// The window-side objects of a page context: Location, History, Navigator
// and the handful of Window methods scripts call before anything else.
//
// A page context never navigates. Setting `location`, following a link or
// submitting a form hands a request to the host (`op_web_page_navigate`),
// which decides what happens next.

import { core, primordials } from "ext:core/mod.js";
import { op_web_page_navigate } from "ext:core/ops";
import {
  getDocumentUrl,
  kConstruct,
  setDocumentUrl,
  toStr,
} from "ext:deno_web_page/00_dom.js";
const {
  ObjectFreeze,
  TypeError,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const { URL } = core.loadExtScript("ext:deno_web/00_url.js");
const { setLocationHref } = core.loadExtScript("ext:deno_web/12_location.js");
const { structuredClone } = core.loadExtScript(
  "ext:deno_web/02_structured_clone.js",
);
const timers = core.loadExtScript("ext:deno_web/02_timers.js");
const { performance } = core.loadExtScript("ext:deno_web/15_performance.js");

function illegalInvocation() {
  return new TypeError("Illegal invocation");
}

function currentUrl() {
  return new URL(getDocumentUrl());
}

function resolve(url) {
  try {
    return new URL(toStr(url), getDocumentUrl()).href;
  } catch {
    throw new DOMException(
      `The URL '${toStr(url)}' is invalid.`,
      "SyntaxError",
    );
  }
}

function requestLocation(url, replace) {
  op_web_page_navigate({
    __proto__: null,
    kind: "location",
    url: resolve(url),
    replace,
  });
}

class Location {
  #location() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#location in value)) {
      throw illegalInvocation();
    }
  }

  constructor(key = undefined) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
  }

  get href() {
    Location.#check(this);
    return getDocumentUrl();
  }

  set href(value) {
    Location.#check(this);
    requestLocation(value, false);
  }

  get origin() {
    Location.#check(this);
    return currentUrl().origin;
  }

  get protocol() {
    Location.#check(this);
    return currentUrl().protocol;
  }

  get host() {
    Location.#check(this);
    return currentUrl().host;
  }

  get hostname() {
    Location.#check(this);
    return currentUrl().hostname;
  }

  get port() {
    Location.#check(this);
    return currentUrl().port;
  }

  get pathname() {
    Location.#check(this);
    return currentUrl().pathname;
  }

  get search() {
    Location.#check(this);
    return currentUrl().search;
  }

  get hash() {
    Location.#check(this);
    return currentUrl().hash;
  }

  assign(url) {
    Location.#check(this);
    requestLocation(url, false);
  }

  replace(url) {
    Location.#check(this);
    requestLocation(url, true);
  }

  reload() {
    Location.#check(this);
    op_web_page_navigate({ __proto__: null, kind: "reload" });
  }

  toString() {
    Location.#check(this);
    return getDocumentUrl();
  }
}
webidl.configureInterface(Location);

class History {
  #state = null;

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#state in value)) {
      throw illegalInvocation();
    }
  }

  constructor(key = undefined) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
  }

  get length() {
    History.#check(this);
    return 1;
  }

  get state() {
    History.#check(this);
    return this.#state;
  }

  get scrollRestoration() {
    History.#check(this);
    return "auto";
  }

  // The session history is the host's. These are no navigation.
  go(_delta = 0) {
    History.#check(this);
  }

  back() {
    History.#check(this);
  }

  forward() {
    History.#check(this);
  }

  #update(state, url) {
    const cloned = structuredClone(state);
    if (url !== undefined && url !== null) {
      const next = new URL(resolve(url));
      if (next.origin !== currentUrl().origin) {
        throw new DOMException(
          "A history state object with a URL of another origin cannot be created.",
          "SecurityError",
        );
      }
      setDocumentUrl(next.href);
      setLocationHref(next.href);
    }
    this.#state = cloned;
  }

  pushState(state, _unused, url = undefined) {
    History.#check(this);
    this.#update(state, url);
  }

  replaceState(state, _unused, url = undefined) {
    History.#check(this);
    this.#update(state, url);
  }
}
webidl.configureInterface(History);

let userAgent = "";

class Navigator {
  #navigator() {}

  static #check(value) {
    if (
      typeof value !== "object" || value === null || !(#navigator in value)
    ) {
      throw illegalInvocation();
    }
  }

  constructor(key = undefined) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
  }

  get userAgent() {
    Navigator.#check(this);
    return userAgent;
  }

  get language() {
    Navigator.#check(this);
    return "en-US";
  }

  get languages() {
    Navigator.#check(this);
    return LANGUAGES;
  }

  get onLine() {
    Navigator.#check(this);
    return true;
  }

  // `document.cookie` is not wired in this cut.
  get cookieEnabled() {
    Navigator.#check(this);
    return false;
  }
}
webidl.configureInterface(Navigator);

const LANGUAGES = ObjectFreeze(["en-US", "en"]);

/** The window's `location`, `history` and `navigator`. */
function createWindowObjects(info) {
  userAgent = info.userAgent;
  setLocationHref(info.url);
  return {
    __proto__: null,
    location: new Location(kConstruct),
    history: new History(kConstruct),
    navigator: new Navigator(kConstruct),
  };
}

// There is no rendering, so a frame is a timer: 60 per second.
function requestAnimationFrame(callback) {
  if (typeof callback !== "function") {
    throw new TypeError(
      "Failed to execute 'requestAnimationFrame' on 'Window': parameter 1 is not of type 'Function'.",
    );
  }
  return timers.setTimeout(() => callback(performance.now()), 16);
}

function cancelAnimationFrame(handle) {
  timers.clearTimeout(handle);
}

// Dialogs have no user to answer them: `alert` returns, `confirm` is
// dismissed and `prompt` is canceled.
function alert(_message = undefined) {}

function confirm(_message = undefined) {
  return false;
}

function prompt(_message = undefined, _default = undefined) {
  return null;
}

// Nothing scrolls.
function scrollTo() {}

export {
  alert,
  cancelAnimationFrame,
  confirm,
  createWindowObjects,
  History,
  Location,
  Navigator,
  prompt,
  requestAnimationFrame,
  scrollTo,
};
