// Copyright 2018-2026 the Deno authors. MIT license.
// @ts-check

// A small, layout-free `document` for the WPT runner's `--dom-shim` flag.
//
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
//   Every tag outside the small table below (`video`, `audio`, `iframe`,
//   `img`, ...) is an `HTMLUnknownElement`: this runtime has no such element.
//
// There is no layout, style, parsing, rendering, media or browsing context,
// and no event is ever dispatched by the shim. It must never make a test pass
// without exercising the API under test; a gap stays a failure.
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
      this.#attributes.set(String(name).toLowerCase(), String(value));
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

    /** @param {string} localName */
    createElement(localName) {
      const name = String(localName).toLowerCase();
      if (!/^[a-z][a-z0-9-]*$/.test(name)) {
        throw domError(
          `"${localName}" is not a valid element name`,
          "InvalidCharacterError",
        );
      }
      return createNode(elementInterfaces[name] ?? HTMLUnknownElement, name);
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
     * Runner hook: the `<script>` about to run (its `src`, or `null` for an
     * inline script), or `undefined` once it has run.
     * @param {string | null | undefined} src
     */
    [setCurrentScript](src) {
      if (src === undefined) {
        this.#currentScript = null;
        return;
      }
      const script =
        /** @type {HTMLScriptElement} */ (this.createElement("script"));
      if (src !== null) script.setAttribute("src", src);
      this.head?.appendChild(script);
      this.#currentScript = script;
    }
  }

  const document = createNode(Document);
  const html = document.appendChild(document.createElement("html"));
  html.appendChild(document.createElement("head"));
  html.appendChild(document.createElement("body"));
  if (typeof Reflect.get(globalThis, "META_TITLE") === "string") {
    document.title = Reflect.get(globalThis, "META_TITLE");
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
