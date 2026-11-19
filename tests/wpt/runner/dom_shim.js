// Copyright 2018-2026 the Deno authors. MIT license.
// @ts-check

// The WPT runner's document environment (tier T1 in README.md): a small,
// layout-free `document` for window-scoped tests (on by default for the
// WebRTC and Media Capture suites, `--document` for all, `--no-document` for
// none).
//
// Loading a test's document is the test runner's job (wptrunner does it for a
// browser through WebDriver); it is test plumbing, not an API under test.
// Many `.html` tests only touch the page for bookkeeping: a helper reads
// `document.currentScript`, appends a scratch element to `document.body`, or
// checks whether an element interface exists before choosing another path. In
// a runtime with no DOM each of those is a `ReferenceError` before the test
// reaches the API it is about. This shim gives them a node tree to talk to and
// nothing more:
//
// - `document` with `documentElement`, `head`, `body`, `title`,
//   `currentScript`, `createElement`, `createTextNode`, `getElementById` and
//   `getElementsByTagName`;
// - `Node` / `Element` with `appendChild`, `insertBefore`, `removeChild`,
//   `append`, `remove`, `contains`, attributes and `textContent`;
// - the element interfaces for the tags it creates. `HTMLCanvasElement` exists
//   but draws nothing: `getContext()` returns `null` (the answer for a context
//   type a user agent does not support) and it has no `captureStream()`, so a
//   helper that feature-detects canvas capture falls back to `getUserMedia()`.
//   Every tag outside the small table below (`iframe`, `img`, ...) is an
//   `HTMLUnknownElement`: this runtime has no such element. A runner
//   environment file loaded after this one (media_env.js: `<video>`,
//   `<audio>` over the runtime's received frames) adds element interfaces
//   with the `deno.wpt.defineElement` hook, or by defining the global
//   interface (`HTMLVideoElement`, ...) as a subclass of `HTMLElement`;
// - the test file's own markup (`<div id="log">`, `<video id="video1">`):
//   the runner hands over the parsed page (`deno.wpt.loadMarkup`) and the
//   shim instantiates it as an HTML parser would have, the nodes before each
//   `<script>` before that script runs (`deno.wpt.setCurrentScript`), the
//   rest after the last one (`deno.wpt.finishMarkup`). That script element is
//   `document.currentScript` while it runs;
// - named access on the window (HTML 7.2.2.3): a connected element's `id`
//   names a global that returns the element, unless the global object already
//   has that property.
//
// There is no layout, style, HTML parsing of strings (`innerHTML` is a plain
// property), rendering, media or browsing context, and no event is ever
// dispatched by the shim. It must never make a test pass without exercising
// the API under test; a gap stays a failure.
//
// The runner evaluates this file after `testharness.js` has loaded, so
// testharness keeps its shell environment (it picks the window environment
// when `document` exists at load time).

(() => {
  "use strict";
  if ("document" in globalThis) return;

  const HTML_NS = "http://www.w3.org/1999/xhtml";
  let constructing = false;
  /** @type {(node: Node) => readonly Node[]} */
  let childrenOf;
  /**
   * Called with a node just inserted into a connected tree, and with an
   * element whose `id` changed while connected (named access, see
   * `exposeNamed`).
   * @type {(node: Node) => void}
   */
  let onConnected = () => {};

  /**
   * The elements under `root`, in tree order, that `match` accepts.
   * @param {Node} root @param {(element: Element) => boolean} match
   * @returns {Element[]}
   */
  function descendants(root, match) {
    /** @type {Element[]} */
    const found = [];
    for (const child of childrenOf(root)) {
      if (child instanceof Element && match(child)) found.push(child);
      found.push(...descendants(child, match));
    }
    return found;
  }

  /** @param {string} message @param {string} name */
  const domError = (message, name) => new DOMException(message, name);

  class Node extends EventTarget {
    /** @type {Node | null} */
    #parent = null;
    /** @type {Node[]} */
    #children = [];

    static {
      childrenOf = (node) => node.#children;
    }

    constructor() {
      super();
      if (!constructing) throw new TypeError("Illegal constructor");
    }

    get parentNode() {
      return this.#parent;
    }
    /** @returns {Element | null} */
    get parentElement() {
      return this.#parent instanceof Element ? this.#parent : null;
    }
    get childNodes() {
      return [...this.#children];
    }
    /** @returns {Node | null} */
    get firstChild() {
      return this.#children[0] ?? null;
    }
    /** @returns {Node | null} */
    get lastChild() {
      return this.#children.at(-1) ?? null;
    }
    get isConnected() {
      if (this instanceof Document) return true;
      let root = this.#parent;
      while (root && root.#parent) root = root.#parent;
      return root instanceof Document;
    }

    /** @returns {string} */
    get textContent() {
      return this.#children.map((child) => child.textContent ?? "").join("");
    }
    set textContent(value) {
      for (const child of this.#children) child.#parent = null;
      this.#children = [];
      const text = String(value ?? "");
      if (text) this.appendChild(createNode(Text, text));
    }

    /** @param {Node | null} other */
    contains(other) {
      for (let node = other; node; node = node.#parent) {
        if (node === this) return true;
      }
      return false;
    }

    /** @template {Node} T @param {T} node @param {Node | null} child */
    insertBefore(node, child) {
      if (!(node instanceof Node)) {
        throw new TypeError("Argument 1 is not a Node");
      }
      if (node.contains(this) || node instanceof Document) {
        throw domError(
          "The new child would create a cycle",
          "HierarchyRequestError",
        );
      }
      if (child !== null && child.#parent !== this) {
        throw domError(
          "The reference child is not a child of this node",
          "NotFoundError",
        );
      }
      node.remove();
      const index = child === null
        ? this.#children.length
        : this.#children.indexOf(child);
      this.#children.splice(index, 0, node);
      node.#parent = this;
      if (node.isConnected) onConnected(node);
      return node;
    }

    /** @template {Node} T @param {T} node */
    appendChild(node) {
      return this.insertBefore(node, null);
    }

    /** @template {Node} T @param {T} node */
    removeChild(node) {
      if (!(node instanceof Node) || node.#parent !== this) {
        throw domError(
          "The node to be removed is not a child of this node",
          "NotFoundError",
        );
      }
      this.#children.splice(this.#children.indexOf(node), 1);
      node.#parent = null;
      return node;
    }

    /** @param {...(Node | string)} nodes */
    append(...nodes) {
      for (const node of nodes) {
        this.appendChild(
          node instanceof Node ? node : createNode(Text, String(node)),
        );
      }
    }

    remove() {
      this.#parent?.removeChild(this);
    }

    /** @param {string} name */
    getElementsByTagName(name) {
      const lower = String(name).toLowerCase();
      return descendants(this, (el) => name === "*" || el.localName === lower);
    }
  }

  class Text extends Node {
    /** @param {string} data */
    constructor(data) {
      super();
      this.data = data;
    }
    get nodeValue() {
      return this.data;
    }
    /** @override @returns {string} */
    get textContent() {
      return this.data;
    }
    /** @override @param {string} value */
    set textContent(value) {
      this.data = String(value ?? "");
    }
  }

  class Element extends Node {
    /** @type {Map<string, string>} */
    #attributes = new Map();
    /** @type {Record<string, string>} */
    style = {};

    /** @param {string} localName */
    constructor(localName) {
      super();
      this.localName = localName;
      this.namespaceURI = HTML_NS;
    }
    get tagName() {
      return this.localName.toUpperCase();
    }
    get children() {
      return this.childNodes.filter((node) => node instanceof Element);
    }
    get id() {
      return this.getAttribute("id") ?? "";
    }
    set id(value) {
      this.setAttribute("id", value);
    }
    get className() {
      return this.getAttribute("class") ?? "";
    }
    set className(value) {
      this.setAttribute("class", value);
    }
    /** @param {string} name */
    getAttribute(name) {
      return this.#attributes.get(String(name).toLowerCase()) ?? null;
    }
    /** @param {string} name @param {unknown} value */
    setAttribute(name, value) {
      const lower = String(name).toLowerCase();
      this.#attributes.set(lower, String(value));
      if (lower === "id" && this.isConnected) onConnected(this);
    }
    getAttributeNames() {
      return [...this.#attributes.keys()];
    }
    /** @param {string} name */
    hasAttribute(name) {
      return this.#attributes.has(String(name).toLowerCase());
    }
    /** @param {string} name */
    removeAttribute(name) {
      this.#attributes.delete(String(name).toLowerCase());
    }
  }

  class HTMLElement extends Element {}
  class HTMLUnknownElement extends HTMLElement {}
  class HTMLHtmlElement extends HTMLElement {}
  class HTMLHeadElement extends HTMLElement {}
  class HTMLBodyElement extends HTMLElement {}
  class HTMLTitleElement extends HTMLElement {
    get text() {
      return this.textContent;
    }
    set text(value) {
      this.textContent = value;
    }
  }
  class HTMLDivElement extends HTMLElement {}
  class HTMLSpanElement extends HTMLElement {}
  class HTMLParagraphElement extends HTMLElement {}
  class HTMLPreElement extends HTMLElement {}
  class HTMLScriptElement extends HTMLElement {
    get src() {
      const src = this.getAttribute("src");
      return src === null ? "" : new URL(src, location.href).href;
    }
    set src(value) {
      this.setAttribute("src", value);
    }
  }
  /** A canvas with no rendering context and no `captureStream()`. */
  class HTMLCanvasElement extends HTMLElement {
    get width() {
      return Number(this.getAttribute("width") ?? 300);
    }
    set width(value) {
      this.setAttribute("width", value >>> 0);
    }
    get height() {
      return Number(this.getAttribute("height") ?? 150);
    }
    set height(value) {
      this.setAttribute("height", value >>> 0);
    }
    /** @param {string} _contextId */
    getContext(_contextId) {
      return null;
    }
  }

  /** @type {Record<string, typeof HTMLElement>} */
  const elementInterfaces = {
    html: HTMLHtmlElement,
    head: HTMLHeadElement,
    body: HTMLBodyElement,
    title: HTMLTitleElement,
    div: HTMLDivElement,
    span: HTMLSpanElement,
    p: HTMLParagraphElement,
    pre: HTMLPreElement,
    script: HTMLScriptElement,
    canvas: HTMLCanvasElement,
  };

  /**
   * Element interfaces a runner environment file may define as globals
   * (subclasses of `HTMLElement`); `createElement` uses them when present.
   * @type {Record<string, string>}
   */
  const environmentInterfaceNames = {
    video: "HTMLVideoElement",
    audio: "HTMLAudioElement",
  };

  /**
   * The interface for a local name: the table, then an environment file's.
   * @param {string} name
   * @returns {typeof HTMLElement}
   */
  function interfaceFor(name) {
    if (Object.hasOwn(elementInterfaces, name)) return elementInterfaces[name];
    const globalName = environmentInterfaceNames[name];
    const ctor = globalName === undefined
      ? undefined
      : Reflect.get(globalThis, globalName);
    if (typeof ctor === "function" && ctor.prototype instanceof HTMLElement) {
      return ctor;
    }
    return HTMLUnknownElement;
  }

  /**
   * @template {Node} T
   * @param {new (...args: any[]) => T} ctor
   * @param {...any} args
   * @returns {T}
   */
  function createNode(ctor, ...args) {
    constructing = true;
    try {
      return new ctor(...args);
    } finally {
      constructing = false;
    }
  }

  const setCurrentScript = Symbol.for("deno.wpt.setCurrentScript");
  const loadMarkup = Symbol.for("deno.wpt.loadMarkup");
  const finishMarkup = Symbol.for("deno.wpt.finishMarkup");
  const defineElement = Symbol.for("deno.wpt.defineElement");

  /**
   * The test file's markup in document order (see `MarkupNode` in runner.ts):
   * `p` is the parent's index (-1 for `<html>`), `t` an element's local name
   * and `a` its attributes, `x` a text node's data.
   * @typedef {{ p: number, t?: string, a?: [string, string][], x?: string }} MarkupNode
   */

  /**
   * The `<title>` made from the page's title (`META_TITLE`) before the markup
   * came; the markup's own `<title>` takes its place.
   * @type {Element | null}
   */
  let titleFromMeta = null;

  class Document extends Node {
    /** @type {HTMLScriptElement | null} */
    #currentScript = null;
    readyState = "complete";
    contentType = "text/html";

    get documentElement() {
      return this.children[0] ?? null;
    }
    get children() {
      return this.childNodes.filter((node) => node instanceof Element);
    }
    get head() {
      return this.getElementsByTagName("head")[0] ?? null;
    }
    get body() {
      return this.getElementsByTagName("body")[0] ?? null;
    }
    get title() {
      return this.getElementsByTagName("title")[0]?.textContent.trim() ?? "";
    }
    set title(value) {
      let title = this.getElementsByTagName("title")[0];
      if (!title) {
        title = this.createElement("title");
        this.head?.appendChild(title);
      }
      title.textContent = value;
    }
    get currentScript() {
      return this.#currentScript;
    }
    get URL() {
      return location.href;
    }
    get defaultView() {
      return globalThis;
    }

    /** @type {MarkupNode[]} */
    #markup = [];
    /** The markup nodes instantiated so far, by index. @type {Node[]} */
    #markupNodes = [];

    /** @param {string} localName */
    createElement(localName) {
      const name = String(localName).toLowerCase();
      if (!/^[a-z][a-z0-9-]*$/.test(name)) {
        throw domError(
          `"${localName}" is not a valid element name`,
          "InvalidCharacterError",
        );
      }
      const element = createNode(interfaceFor(name), name);
      // An environment file's constructor may not pass the name on.
      if (element.localName !== name) element.localName = name;
      return element;
    }
    /** @param {string} data */
    createTextNode(data) {
      return createNode(Text, String(data));
    }
    /** @param {string} id */
    getElementById(id) {
      return descendants(this, (el) => el.id === String(id))[0] ?? null;
    }

    /**
     * Runner hook: the `<script>` about to run (its index in the markup, or
     * without markup its `src`, or `null` for an inline script), or
     * `undefined` once it has run.
     * @param {number | string | null | undefined} src
     */
    [setCurrentScript](src) {
      if (src === undefined) {
        this.#currentScript = null;
        return;
      }
      if (typeof src === "number") {
        // The markup's `<script>` at that index, with its text, after
        // everything before it.
        let last = src;
        while (this.#markup[last + 1]?.p === src) last++;
        this.#materializeThrough(last);
        const node = this.#markupNodes[src];
        if (node instanceof HTMLScriptElement) {
          this.#currentScript = node;
          return;
        }
        src = null;
      }
      const script =
        /** @type {HTMLScriptElement} */ (this.createElement("script"));
      if (src !== null) script.setAttribute("src", src);
      this.head?.appendChild(script);
      this.#currentScript = script;
    }

    /**
     * Runner hook: the test file's markup, instantiated as the scripts are
     * reached (see the file comment).
     * @param {MarkupNode[]} nodes
     */
    [loadMarkup](nodes) {
      this.#markup = Array.isArray(nodes) ? nodes : [];
      this.#markupNodes = [];
    }

    /** Runner hook: the scripts have all run; the rest of the markup. */
    [finishMarkup]() {
      this.#materializeThrough(this.#markup.length - 1);
    }

    /**
     * Runner hook for environment files: `createElement(localName)` makes a
     * `ctor`, a subclass of `HTMLElement`, which also becomes a global.
     * @param {string} localName @param {typeof HTMLElement} ctor
     */
    [defineElement](localName, ctor) {
      const name = String(localName).toLowerCase();
      if (
        typeof ctor !== "function" || !(ctor.prototype instanceof HTMLElement)
      ) {
        throw new TypeError("An element interface extends HTMLElement");
      }
      elementInterfaces[name] = ctor;
      if (!(ctor.name in globalThis) && ctor.name !== "") {
        Object.defineProperty(globalThis, ctor.name, {
          value: ctor,
          writable: true,
          configurable: true,
          enumerable: false,
        });
      }
    }

    /** @param {number} last */
    #materializeThrough(last) {
      const html = this.documentElement;
      for (let i = this.#markupNodes.length; i <= last; i++) {
        const entry = this.#markup[i];
        if (entry === undefined) break;
        const parent = entry.p === -1 ? this : this.#markupNodes[entry.p];
        /** @type {Node} */
        let node;
        if (entry.t === undefined) {
          node = this.createTextNode(entry.x ?? "");
          (parent ?? this.body ?? this).appendChild(node);
          this.#markupNodes[i] = node;
          continue;
        }
        // The document's own `<html>`, `<head>` and `<body>` (and the
        // `<title>` made from the page's title) stand for the parsed ones.
        /** @type {Element | null} */
        let existing = null;
        if (entry.p === -1 && entry.t === "html") existing = html;
        else if (parent === html && entry.t === "head") existing = this.head;
        else if (parent === html && entry.t === "body") existing = this.body;
        else if (
          entry.t === "title" && titleFromMeta !== null &&
          titleFromMeta.parentNode === parent
        ) {
          existing = titleFromMeta;
          existing.textContent = "";
          titleFromMeta = null;
        }
        /** @type {Element} */
        let element;
        if (existing !== null) {
          element = existing;
        } else {
          try {
            element = this.createElement(entry.t);
          } catch {
            element = createNode(HTMLUnknownElement, entry.t);
          }
        }
        for (const [name, value] of entry.a ?? []) {
          try {
            element.setAttribute(name, value);
          } catch {
            // An attribute name the shim can't hold; the parser kept it.
          }
        }
        if (existing === null) {
          (parent ?? this.body ?? this).appendChild(element);
        }
        this.#markupNodes[i] = element;
      }
    }
  }

  /** The ids already exposed as window properties. @type {Set<string>} */
  const namedProperties = new Set();

  /**
   * Named access on the window (HTML 7.2.2.3): `window[id]` is the element
   * with that id, unless the global object already has that property. Live:
   * the getter looks the id up each time, so a removed element is gone.
   * @param {string} id
   */
  function exposeNamed(id) {
    if (id === "" || namedProperties.has(id) || id in globalThis) return;
    namedProperties.add(id);
    Object.defineProperty(globalThis, id, {
      get: () => document.getElementById(id) ?? undefined,
      // A script assigning the name (`var video1 = ...`) replaces it.
      set: (value) => {
        Object.defineProperty(globalThis, id, {
          value,
          writable: true,
          configurable: true,
          enumerable: true,
        });
      },
      configurable: true,
      enumerable: false,
    });
  }

  onConnected = (node) => {
    if (node instanceof Element) exposeNamed(node.id);
    for (const element of descendants(node, (el) => el.id !== "")) {
      exposeNamed(element.id);
    }
  };

  const document = createNode(Document);
  const html = document.appendChild(document.createElement("html"));
  html.appendChild(document.createElement("head"));
  html.appendChild(document.createElement("body"));
  if (typeof Reflect.get(globalThis, "META_TITLE") === "string") {
    document.title = Reflect.get(globalThis, "META_TITLE");
    titleFromMeta = document.getElementsByTagName("title")[0] ?? null;
  }

  const globals = {
    Node,
    Text,
    Element,
    Document,
    HTMLElement,
    HTMLUnknownElement,
    HTMLHtmlElement,
    HTMLHeadElement,
    HTMLBodyElement,
    HTMLTitleElement,
    HTMLDivElement,
    HTMLSpanElement,
    HTMLParagraphElement,
    HTMLPreElement,
    HTMLScriptElement,
    HTMLCanvasElement,
  };
  for (const [name, value] of Object.entries(globals)) {
    if (name in globalThis) continue;
    Object.defineProperty(globalThis, name, {
      value,
      writable: true,
      configurable: true,
      enumerable: false,
    });
  }
  Object.defineProperty(globalThis, "document", {
    get: () => document,
    configurable: true,
    enumerable: true,
  });
})();
