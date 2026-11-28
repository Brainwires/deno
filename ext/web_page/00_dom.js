// Copyright 2018-2026 the Deno authors. MIT license.

// The DOM core of a page context: Node, Element, CharacterData, Text,
// Comment, DocumentType, DocumentFragment, Document and their collections,
// over the `op_web_page_*` ops (`dom_ops.rs`), plus event dispatch along the
// tree.
//
// The tree lives in Rust (`thalora_dom::Document`). A wrapper holds nothing
// but its node's id, as two numbers (the arena index and the generation of
// `NodeId`), in private fields. There is one wrapper per live node, kept in
// `wrappers` by index, and in this cut it is never dropped: a node a page
// creates stays allocated, with its wrapper, until the page goes away.
//
// Everything a page can reach from here is hand-written. A WebIDL generator
// replaces the interface shells later, so descriptors are right where that
// is cheap (members on prototypes, enumerable, `Symbol.toStringTag`, illegal
// constructors) and no further.

import { core, primordials } from "ext:core/mod.js";
import {
  op_web_page_attributes,
  op_web_page_character_data,
  op_web_page_child_list,
  op_web_page_clone_node,
  op_web_page_closest,
  op_web_page_contains,
  op_web_page_create_element,
  op_web_page_create_element_ns,
  op_web_page_create_node,
  op_web_page_doctype_info,
  op_web_page_document_node,
  op_web_page_element_name,
  op_web_page_get_attribute,
  op_web_page_get_element_by_id,
  op_web_page_get_elements_by,
  op_web_page_has_attribute,
  op_web_page_insert_before,
  op_web_page_is_connected,
  op_web_page_matches,
  op_web_page_node_name,
  op_web_page_node_relative,
  op_web_page_parse_fragment,
  op_web_page_query_selector,
  op_web_page_query_selector_all,
  op_web_page_remove,
  op_web_page_remove_attribute,
  op_web_page_remove_child,
  op_web_page_replace_child,
  op_web_page_serialize,
  op_web_page_set_attribute,
  op_web_page_set_character_data,
  op_web_page_set_inner_html,
  op_web_page_set_text_content,
  op_web_page_text_content,
} from "ext:core/ops";
const {
  ArrayPrototypeIncludes,
  ArrayPrototypeIndexOf,
  ArrayPrototypeJoin,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  ArrayPrototypeSplice,
  Function,
  FunctionPrototypeCall,
  MapPrototypeGet,
  MapPrototypeSet,
  ObjectDefineProperty,
  ObjectGetOwnPropertyDescriptors,
  ObjectPrototypeIsPrototypeOf,
  Proxy,
  ReflectHas,
  ReflectOwnKeys,
  RegExpPrototypeTest,
  SafeMap,
  SafeSet,
  SafeWeakMap,
  String,
  StringPrototypeCharCodeAt,
  StringPrototypeIndexOf,
  StringPrototypeReplace,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  StringPrototypeToUpperCase,
  StringPrototypeTrim,
  Symbol,
  SymbolIterator,
  SafeRegExp,
  TypeError,
  Uint32Array,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const event = core.loadExtScript("ext:deno_web/02_event.js");
const { EventTarget, getEventTargetData, reportException } = event;
const removeEventListener = EventTarget.prototype.removeEventListener;

// Relations of `op_web_page_node_relative` and well-known nodes of
// `op_web_page_document_node`. Kept in step with `dom_ops.rs`.
const REL_PARENT = 0;
const REL_FIRST_CHILD = 1;
const REL_LAST_CHILD = 2;
const REL_PREVIOUS_SIBLING = 3;
const REL_NEXT_SIBLING = 4;
const REL_PARENT_ELEMENT = 5;
const REL_FIRST_ELEMENT_CHILD = 6;
const REL_LAST_ELEMENT_CHILD = 7;
const REL_PREVIOUS_ELEMENT_SIBLING = 8;
const REL_NEXT_ELEMENT_SIBLING = 9;
const REL_ROOT = 10;
const REL_TEMPLATE_CONTENTS = 11;
const REL_SELF = 12;
const DOC_DOCUMENT = 0;
const DOC_DOCUMENT_ELEMENT = 1;
const DOC_HEAD = 2;
const DOC_BODY = 3;
const DOC_DOCTYPE = 4;

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;
const PROCESSING_INSTRUCTION_NODE = 7;
const COMMENT_NODE = 8;
const DOCUMENT_NODE = 9;
const DOCUMENT_TYPE_NODE = 10;
const DOCUMENT_FRAGMENT_NODE = 11;

// The only way to construct a wrapper. Never leaves the extension's modules.
const kConstruct = Symbol("construct");

// Where node-returning ops write `[index, generation, nodeType, flavor]`.
// Any op call may overwrite it: read it before calling another.
const out = new Uint32Array(4);

// Arena index -> wrapper. An index names one live node at a time, so a
// wrapper found with another generation belongs to a node that was freed.
const wrappers = new SafeMap();

// HTML local name -> interface, filled by `02_html.js`.
const htmlInterfaces = { __proto__: null };
let defaultHtmlInterface = null;

function registerHtmlInterface(localName, interface_) {
  htmlInterfaces[localName] = interface_;
}

function setDefaultHtmlInterface(interface_) {
  defaultHtmlInterface = interface_;
}

// The window (the global object) and what the document needs from it, set
// by `99_page_main.js`.
let windowRef = null;
let documentUrl = "about:blank";
let readyState = "loading";
let activeElement = null;

function setWindow(window, url) {
  windowRef = window;
  documentUrl = url;
}

function setDocumentUrl(url) {
  documentUrl = url;
}

function setReadyState(state) {
  readyState = state;
}

function setActiveElement(element) {
  activeElement = element;
}

function getActiveElement() {
  return activeElement;
}

function getDocumentUrl() {
  return documentUrl;
}

function getWindow() {
  return windowRef;
}

// The click activation behavior, set by `02_html.js`: `begin(target, event)`
// runs the legacy-pre-activation steps and answers with a token, or null
// when the event activates nothing; `end(token, notCanceled, event)` runs
// the activation behavior or undoes the pre-activation.
let clickActivation = null;

function setClickActivation(hooks) {
  clickActivation = hooks;
}

function toStr(value) {
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "symbol") {
    throw new TypeError("Cannot convert a Symbol value to a string");
  }
  return String(value);
}

function illegalInvocation() {
  return new TypeError("Illegal invocation");
}

function notANode(method, position = 1) {
  return new TypeError(
    `Failed to execute '${method}' on 'Node': parameter ${position} is not of type 'Node'.`,
  );
}

function wrapNode(index, generation, type, flavor) {
  const existing = MapPrototypeGet(wrappers, index);
  if (existing !== undefined && generationOf(existing) === generation) {
    return existing;
  }
  switch (type) {
    case ELEMENT_NODE: {
      if (flavor === 2) {
        const localName = op_web_page_element_name(index, generation)[0];
        const Interface = htmlInterfaces[localName] ?? defaultHtmlInterface;
        return new Interface(kConstruct, index, generation);
      }
      return new Element(kConstruct, index, generation);
    }
    case TEXT_NODE:
      return new Text(kConstruct, index, generation);
    case COMMENT_NODE:
    case PROCESSING_INSTRUCTION_NODE:
      return new Comment(kConstruct, index, generation);
    case DOCUMENT_NODE:
      return new Document(kConstruct, index, generation);
    case DOCUMENT_TYPE_NODE:
      return new DocumentType(kConstruct, index, generation);
    case DOCUMENT_FRAGMENT_NODE:
      return new DocumentFragment(kConstruct, index, generation);
    default:
      throw new TypeError("Unknown node type");
  }
}

/** The wrapper of the node an op just wrote to `out`. */
function wrapOut() {
  return wrapNode(out[0], out[1], out[2], out[3]);
}

/** The wrappers of a flattened node list an op returned. */
function wrapList(flat) {
  const nodes = [];
  for (let i = 0; i + 3 < flat.length; i += 4) {
    ArrayPrototypePush(
      nodes,
      wrapNode(flat[i], flat[i + 1], flat[i + 2], flat[i + 3]),
    );
  }
  return nodes;
}

/** The wrapper of a node the host names, or null if there is none. */
function nodeFromId(index, generation) {
  if (!op_web_page_node_relative(index, generation, REL_SELF, out)) {
    return null;
  }
  return wrapOut();
}

// Private-field access for code outside the class bodies. Assigned in the
// static blocks below.
let indexOf;
let generationOf;
let isNode;
let isElement;
let isCharacterData;
let isDocument;
let isDocumentFragment;

function relative(node, relation) {
  return op_web_page_node_relative(
      indexOf(node),
      generationOf(node),
      relation,
      out,
    )
    ? wrapOut()
    : null;
}

function documentNode(which) {
  return op_web_page_document_node(which, out) ? wrapOut() : null;
}

function insertBefore(parent, node, reference) {
  if (reference === null) {
    op_web_page_insert_before(
      indexOf(parent),
      generationOf(parent),
      indexOf(node),
      generationOf(node),
      false,
      0,
      0,
    );
  } else {
    op_web_page_insert_before(
      indexOf(parent),
      generationOf(parent),
      indexOf(node),
      generationOf(node),
      true,
      indexOf(reference),
      generationOf(reference),
    );
  }
}

function createText(data) {
  op_web_page_create_node(0, data, out);
  return wrapOut();
}

function createFragment() {
  op_web_page_create_node(2, "", out);
  return wrapOut();
}

/**
 * "Convert nodes into a node": one node as it is, anything else as a
 * fragment of the nodes and of text nodes for the strings.
 */
function nodesIntoNode(nodes) {
  if (nodes.length === 1 && isNode(nodes[0])) {
    return nodes[0];
  }
  const fragment = createFragment();
  for (let i = 0; i < nodes.length; i++) {
    const item = nodes[i];
    insertBefore(fragment, isNode(item) ? item : createText(toStr(item)), null);
  }
  return fragment;
}

// --- collections -----------------------------------------------------------

function defineIndexedItems(list, items) {
  for (let i = 0; i < items.length; i++) {
    ObjectDefineProperty(list, i, {
      __proto__: null,
      value: items[i],
      enumerable: true,
      configurable: true,
      writable: false,
    });
  }
}

function* iterateValues(items) {
  for (let i = 0; i < items.length; i++) {
    yield items[i];
  }
}

function* iterateKeys(items) {
  for (let i = 0; i < items.length; i++) {
    yield i;
  }
}

function* iterateEntries(items) {
  for (let i = 0; i < items.length; i++) {
    yield [i, items[i]];
  }
}

let nodeListItems;

/** A NodeList. In this cut every list is a snapshot, never live. */
class NodeList {
  #items;

  static {
    nodeListItems = (list) => list.#items;
  }

  constructor(key, items) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    this.#items = items;
    defineIndexedItems(this, items);
  }

  get length() {
    return this.#items.length;
  }

  item(index) {
    return this.#items[index >>> 0] ?? null;
  }

  forEach(callback, thisArg = undefined) {
    const items = this.#items;
    for (let i = 0; i < items.length; i++) {
      FunctionPrototypeCall(callback, thisArg, items[i], i, this);
    }
  }

  keys() {
    return iterateKeys(this.#items);
  }

  values() {
    return iterateValues(this.#items);
  }

  entries() {
    return iterateEntries(this.#items);
  }

  [SymbolIterator]() {
    return iterateValues(this.#items);
  }
}
webidl.configureInterface(NodeList);

/** An HTMLCollection. A snapshot too. */
class HTMLCollection {
  #items;

  constructor(key, items) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    this.#items = items;
    defineIndexedItems(this, items);
  }

  get length() {
    return this.#items.length;
  }

  item(index) {
    return this.#items[index >>> 0] ?? null;
  }

  namedItem(name) {
    name = toStr(name);
    if (name === "") {
      return null;
    }
    const items = this.#items;
    for (let i = 0; i < items.length; i++) {
      const element = items[i];
      if (
        attributeOf(element, "id") === name ||
        attributeOf(element, "name") === name
      ) {
        return element;
      }
    }
    return null;
  }

  [SymbolIterator]() {
    return iterateValues(this.#items);
  }
}
webidl.configureInterface(HTMLCollection);

function nodeList(nodes) {
  return new NodeList(kConstruct, nodes);
}

function htmlCollection(elements) {
  return new HTMLCollection(kConstruct, elements);
}

/** An attribute, as `Element.attributes` shows it. Not a Node in this cut. */
class Attr {
  #name;
  #value;
  #owner;

  constructor(key, name, value, owner) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    this.#name = name;
    this.#value = value;
    this.#owner = owner;
  }

  get name() {
    return this.#name;
  }

  get localName() {
    return this.#name;
  }

  get value() {
    return this.#value;
  }

  set value(value) {
    value = toStr(value);
    setAttributeOf(this.#owner, this.#name, value);
    this.#value = value;
  }

  get ownerElement() {
    return this.#owner;
  }

  get specified() {
    return true;
  }
}
webidl.configureInterface(Attr);

/** `Element.attributes`, as a snapshot. */
class NamedNodeMap {
  #items;

  constructor(key, items) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    this.#items = items;
    defineIndexedItems(this, items);
  }

  get length() {
    return this.#items.length;
  }

  item(index) {
    return this.#items[index >>> 0] ?? null;
  }

  getNamedItem(name) {
    name = toStr(name);
    const items = this.#items;
    for (let i = 0; i < items.length; i++) {
      if (items[i].name === name) {
        return items[i];
      }
    }
    return null;
  }

  [SymbolIterator]() {
    return iterateValues(this.#items);
  }
}
webidl.configureInterface(NamedNodeMap);

const ASCII_WHITESPACE = new SafeRegExp("[\\t\\n\\f\\r ]+");
const HAS_ASCII_WHITESPACE = new SafeRegExp("[\\t\\n\\f\\r ]");
const ASCII_WHITESPACE_ALL = new SafeRegExp("[\\t\\n\\f\\r ]+", "g");

function splitTokens(value) {
  const tokens = [];
  const parts = StringPrototypeSplit(value, ASCII_WHITESPACE);
  for (let i = 0; i < parts.length; i++) {
    const part = parts[i];
    if (part !== "" && !ArrayPrototypeIncludes(tokens, part)) {
      ArrayPrototypePush(tokens, part);
    }
  }
  return tokens;
}

function validToken(token) {
  token = toStr(token);
  if (token === "") {
    throw new DOMException("The token must not be empty.", "SyntaxError");
  }
  if (RegExpPrototypeTest(HAS_ASCII_WHITESPACE, token)) {
    throw new DOMException(
      "The token must not contain whitespace.",
      "InvalidCharacterError",
    );
  }
  return token;
}

/** `classList` and `relList`: a view of one attribute, read on every use. */
class DOMTokenList {
  #element;
  #attribute;

  constructor(key, element, attribute) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    this.#element = element;
    this.#attribute = attribute;
  }

  #tokens() {
    return splitTokens(attributeOf(this.#element, this.#attribute) ?? "");
  }

  #write(tokens) {
    setAttributeOf(
      this.#element,
      this.#attribute,
      ArrayPrototypeJoin(tokens, " "),
    );
  }

  get length() {
    return this.#tokens().length;
  }

  get value() {
    return attributeOf(this.#element, this.#attribute) ?? "";
  }

  set value(value) {
    setAttributeOf(this.#element, this.#attribute, toStr(value));
  }

  item(index) {
    return this.#tokens()[index >>> 0] ?? null;
  }

  contains(token) {
    return ArrayPrototypeIncludes(this.#tokens(), toStr(token));
  }

  add(...tokens) {
    const list = this.#tokens();
    for (let i = 0; i < tokens.length; i++) {
      const token = validToken(tokens[i]);
      if (!ArrayPrototypeIncludes(list, token)) {
        ArrayPrototypePush(list, token);
      }
    }
    this.#write(list);
  }

  remove(...tokens) {
    const list = this.#tokens();
    for (let i = 0; i < tokens.length; i++) {
      const at = ArrayPrototypeIndexOf(list, validToken(tokens[i]));
      if (at !== -1) {
        ArrayPrototypeSplice(list, at, 1);
      }
    }
    this.#write(list);
  }

  toggle(token, force = undefined) {
    token = validToken(token);
    const list = this.#tokens();
    const at = ArrayPrototypeIndexOf(list, token);
    if (at !== -1) {
      if (force === undefined || !force) {
        ArrayPrototypeSplice(list, at, 1);
        this.#write(list);
        return false;
      }
      return true;
    }
    if (force === undefined || force) {
      ArrayPrototypePush(list, token);
      this.#write(list);
      return true;
    }
    return false;
  }

  replace(token, newToken) {
    token = validToken(token);
    newToken = validToken(newToken);
    const list = this.#tokens();
    const at = ArrayPrototypeIndexOf(list, token);
    if (at === -1) {
      return false;
    }
    const existing = ArrayPrototypeIndexOf(list, newToken);
    if (existing === -1) {
      list[at] = newToken;
    } else if (existing !== at) {
      ArrayPrototypeSplice(list, at, 1);
    }
    this.#write(list);
    return true;
  }

  forEach(callback, thisArg = undefined) {
    const list = this.#tokens();
    for (let i = 0; i < list.length; i++) {
      FunctionPrototypeCall(callback, thisArg, list[i], i, this);
    }
  }

  keys() {
    return iterateKeys(this.#tokens());
  }

  values() {
    return iterateValues(this.#tokens());
  }

  entries() {
    return iterateEntries(this.#tokens());
  }

  toString() {
    return this.value;
  }

  [SymbolIterator]() {
    return iterateValues(this.#tokens());
  }
}
webidl.configureInterface(DOMTokenList);

function tokenList(element, attribute) {
  return new DOMTokenList(kConstruct, element, attribute);
}

// --- attribute helpers (no receiver checks: callers hold real elements) ----

function attributeOf(element, name) {
  return op_web_page_get_attribute(
    indexOf(element),
    generationOf(element),
    name,
  );
}

function hasAttributeOf(element, name) {
  return op_web_page_has_attribute(
    indexOf(element),
    generationOf(element),
    name,
  );
}

function setAttributeOf(element, name, value) {
  op_web_page_set_attribute(
    indexOf(element),
    generationOf(element),
    name,
    value,
  );
}

function removeAttributeOf(element, name) {
  return op_web_page_remove_attribute(
    indexOf(element),
    generationOf(element),
    name,
  );
}

/** `[localName, namespaceURI, prefix]` of an element. */
function elementName(element) {
  return op_web_page_element_name(indexOf(element), generationOf(element));
}

// Tree queries for the extension's own use. Unlike the public methods they
// cannot be replaced by a page.

function queryAll(node, selectors) {
  return wrapList(
    op_web_page_query_selector_all(
      indexOf(node),
      generationOf(node),
      selectors,
    ),
  );
}

function queryFirst(node, selectors) {
  return op_web_page_query_selector(
      indexOf(node),
      generationOf(node),
      selectors,
      out,
    )
    ? wrapOut()
    : null;
}

function closestOf(element, selectors) {
  return op_web_page_closest(
      indexOf(element),
      generationOf(element),
      selectors,
      out,
    )
    ? wrapOut()
    : null;
}

function matchesOf(element, selectors) {
  return op_web_page_matches(
    indexOf(element),
    generationOf(element),
    selectors,
  );
}

function dataOf(node) {
  return op_web_page_character_data(indexOf(node), generationOf(node));
}

function textOf(node) {
  return op_web_page_text_content(indexOf(node), generationOf(node)) ?? "";
}

function setTextOf(node, text) {
  op_web_page_set_text_content(indexOf(node), generationOf(node), text);
}

function isConnectedOf(node) {
  return op_web_page_is_connected(indexOf(node), generationOf(node));
}

function containsOf(node, other) {
  return op_web_page_contains(
    indexOf(node),
    generationOf(node),
    indexOf(other),
    generationOf(other),
  );
}

function elementById(id) {
  return op_web_page_get_element_by_id(id, out) ? wrapOut() : null;
}

/** The lowercase local name of an HTML element, "" for anything else. */
function htmlLocalName(node) {
  if (!isElement(node)) {
    return "";
  }
  const name = elementName(node);
  return name[1] === "http://www.w3.org/1999/xhtml" ? name[0] : "";
}

// --- event dispatch along the tree ------------------------------------------
//
// `deno_web`'s `dispatch()` cannot be used for nodes: its `dispatchEvent`
// returns early when the target itself has no listener, and its path
// building puts a Window parent in the wrong branch (as a retargeted target
// instead of a plain ancestor). So the DOM Standard's dispatch is written
// out here, over `deno_web`'s own Event objects and listener lists. That
// needs the Event's internal slots and the listener lists, which
// `02_event.js` keeps where no script reaches them (private fields, a weak
// map) and hands to extension scripts as `eventInternals` and
// `getEventTargetData()`.

const {
  isEvent,
  getAttributes,
  getCanceled,
  getDispatched,
  setDispatched,
  setPath,
  getStopPropagation,
  setStopPropagation,
  getStopImmediatePropagation,
  setStopImmediatePropagation,
  setInPassiveListener,
} = event.eventInternals;

const CAPTURING_PHASE = 1;
const AT_TARGET = 2;
const BUBBLING_PHASE = 3;

function invokeListeners(item, ev, attributes, phase, capture) {
  const listeners = getEventTargetData(item)?.listeners;
  if (listeners === undefined) {
    return;
  }
  const type = attributes.type;
  const list = listeners[type];
  if (list === undefined || list.length === 0) {
    return;
  }
  attributes.currentTarget = item;
  attributes.eventPhase = phase;
  // Listeners added during the dispatch do not run; removed ones do not
  // either.
  const snapshot = ArrayPrototypeSlice(list);
  for (let i = 0; i < snapshot.length; i++) {
    const listener = snapshot[i];
    if (!ArrayPrototypeIncludes(list, listener)) {
      continue;
    }
    const options = listener.options;
    const isObject = typeof options === "object" && options !== null;
    const listensOnCapture = isObject ? !!options.capture : !!options;
    if (listensOnCapture !== capture) {
      continue;
    }
    const callback = listener.callback;
    if (isObject && options.once) {
      FunctionPrototypeCall(removeEventListener, item, type, callback, options);
    }
    const passive = isObject && !!options.passive;
    if (passive) {
      setInPassiveListener(ev, true);
    }
    try {
      if (typeof callback === "object") {
        if (typeof callback.handleEvent === "function") {
          callback.handleEvent(ev);
        }
      } else {
        FunctionPrototypeCall(callback, item, ev);
      }
    } catch (error) {
      reportException(error);
    }
    if (passive) {
      setInPassiveListener(ev, false);
    }
    if (getStopImmediatePropagation(ev)) {
      break;
    }
  }
}

function pathItem(item, target) {
  return {
    item,
    itemInShadowTree: false,
    relatedTarget: null,
    rootOfClosedTree: false,
    slotInClosedTree: false,
    target,
    touchTargetList: [],
  };
}

// Event handler content attributes (`onclick="..."`). The names that are
// event handlers are registered by `02_html.js`. An attribute is compiled
// when an event of its type is about to reach its element, and again when
// its text has changed since. The body sees `event` and `this` (the
// element); the scope chain of the HTML Standard (the element, its form
// owner, the document) is not set up.
const eventHandlerNames = new SafeSet();
const compiledHandlers = new SafeWeakMap();

function registerEventHandlerName(name) {
  eventHandlerNames.add(name);
}

function syncHandlerAttribute(element, type) {
  const source = attributeOf(element, "on" + type);
  let compiled = compiledHandlers.get(element);
  const previous = compiled?.[type];
  if (source === previous || (source === null && previous === undefined)) {
    return;
  }
  if (compiled === undefined) {
    compiled = { __proto__: null };
    compiledHandlers.set(element, compiled);
  }
  if (source === null) {
    delete compiled[type];
    element["on" + type] = null;
    return;
  }
  compiled[type] = source;
  let handler = null;
  try {
    handler = new Function("event", source);
  } catch (error) {
    reportException(error);
  }
  element["on" + type] = handler;
}

/** https://dom.spec.whatwg.org/#concept-event-dispatch, without shadow trees. */
function dispatchAlongTree(target, ev) {
  const attributes = getAttributes(ev);
  setDispatched(ev, true);

  const items = [target];
  let parent = relative(target, REL_PARENT);
  while (parent !== null) {
    ArrayPrototypePush(items, parent);
    parent = relative(parent, REL_PARENT);
  }
  // A document's parent is its window, except for `load`.
  if (
    windowRef !== null && attributes.type !== "load" &&
    isDocument(items[items.length - 1])
  ) {
    ArrayPrototypePush(items, windowRef);
  }
  const path = [];
  const hasHandlerAttribute = defaultHtmlInterface !== null &&
    eventHandlerNames.has(attributes.type);
  for (let i = 0; i < items.length; i++) {
    ArrayPrototypePush(path, pathItem(items[i], i === 0 ? target : null));
    if (
      hasHandlerAttribute &&
      ObjectPrototypeIsPrototypeOf(defaultHtmlInterface.prototype, items[i])
    ) {
      syncHandlerAttribute(items[i], attributes.type);
    }
  }
  setPath(ev, path);
  attributes.target = target;

  try {
    for (let i = items.length - 1; i >= 0; i--) {
      if (getStopPropagation(ev)) {
        break;
      }
      invokeListeners(
        items[i],
        ev,
        attributes,
        i === 0 ? AT_TARGET : CAPTURING_PHASE,
        true,
      );
    }
    for (let i = 0; i < items.length; i++) {
      if (getStopPropagation(ev) || (i > 0 && !attributes.bubbles)) {
        break;
      }
      invokeListeners(
        items[i],
        ev,
        attributes,
        i === 0 ? AT_TARGET : BUBBLING_PHASE,
        false,
      );
    }
  } finally {
    attributes.eventPhase = 0;
    attributes.currentTarget = null;
    setPath(ev, []);
    setDispatched(ev, false);
    setStopPropagation(ev, false);
    setStopImmediatePropagation(ev, false);
  }
  return !getCanceled(ev);
}

/**
 * Dispatches an event at a node, with the click activation behavior around
 * it. Answers whether the event was not canceled.
 */
function dispatchNodeEvent(target, ev) {
  const activation =
    clickActivation !== null && getAttributes(ev).type === "click"
      ? clickActivation.begin(target, ev)
      : null;
  let notCanceled;
  try {
    notCanceled = dispatchAlongTree(target, ev);
  } catch (error) {
    if (activation !== null) {
      clickActivation.end(activation, false, ev);
    }
    throw error;
  }
  if (activation !== null) {
    clickActivation.end(activation, notCanceled, ev);
  }
  return notCanceled;
}

/** Builds an event with `isTrusted` set, and dispatches it at a node. */
function fireEvent(target, EventInterface, type, init) {
  const ev = new EventInterface(type, init);
  event.setIsTrusted(ev, true);
  return dispatchNodeEvent(target, ev);
}

// The web extensions dispatch their own events with `02_event.js`'s
// `dispatchTrusted()`, which reads no property of the target (a page can
// replace `dispatchEvent`). At a node it has to be the dispatch above, so it
// is registered there, once.
event.setNodeDispatcher((value) => isNode(value), dispatchNodeEvent);

// --- Node ---------------------------------------------------------------------

class Node extends EventTarget {
  #index;
  #generation;

  static {
    isNode = (value) =>
      typeof value === "object" && value !== null && #index in value;
    indexOf = (node) => {
      if (!isNode(node)) {
        throw illegalInvocation();
      }
      return node.#index;
    };
    generationOf = (node) => node.#generation;
  }

  constructor(key = undefined, index = 0, generation = 0) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
    super();
    this.#index = index;
    this.#generation = generation;
    MapPrototypeSet(wrappers, index, this);
  }

  get nodeType() {
    indexOf(this);
    if (isElement(this)) {
      return ELEMENT_NODE;
    }
    if (isCharacterData(this)) {
      return ObjectPrototypeIsPrototypeOf(TextPrototype, this)
        ? TEXT_NODE
        : COMMENT_NODE;
    }
    if (isDocument(this)) {
      return DOCUMENT_NODE;
    }
    if (isDocumentFragment(this)) {
      return DOCUMENT_FRAGMENT_NODE;
    }
    return DOCUMENT_TYPE_NODE;
  }

  get nodeName() {
    return op_web_page_node_name(indexOf(this), this.#generation);
  }

  get baseURI() {
    indexOf(this);
    return documentUrl;
  }

  get isConnected() {
    return op_web_page_is_connected(indexOf(this), this.#generation);
  }

  get ownerDocument() {
    indexOf(this);
    return isDocument(this) ? null : documentNode(DOC_DOCUMENT);
  }

  getRootNode(_options = undefined) {
    return relative(this, REL_ROOT);
  }

  get parentNode() {
    return relative(this, REL_PARENT);
  }

  get parentElement() {
    return relative(this, REL_PARENT_ELEMENT);
  }

  hasChildNodes() {
    return relative(this, REL_FIRST_CHILD) !== null;
  }

  get childNodes() {
    return nodeList(
      wrapList(
        op_web_page_child_list(indexOf(this), this.#generation, false),
      ),
    );
  }

  get firstChild() {
    return relative(this, REL_FIRST_CHILD);
  }

  get lastChild() {
    return relative(this, REL_LAST_CHILD);
  }

  get previousSibling() {
    return relative(this, REL_PREVIOUS_SIBLING);
  }

  get nextSibling() {
    return relative(this, REL_NEXT_SIBLING);
  }

  get nodeValue() {
    indexOf(this);
    return isCharacterData(this)
      ? op_web_page_character_data(this.#index, this.#generation)
      : null;
  }

  set nodeValue(value) {
    indexOf(this);
    if (isCharacterData(this)) {
      op_web_page_set_character_data(
        this.#index,
        this.#generation,
        value === null ? "" : toStr(value),
      );
    }
  }

  get textContent() {
    return op_web_page_text_content(indexOf(this), this.#generation);
  }

  set textContent(value) {
    op_web_page_set_text_content(
      indexOf(this),
      this.#generation,
      value === null || value === undefined ? "" : toStr(value),
    );
  }

  cloneNode(deep = false) {
    op_web_page_clone_node(indexOf(this), this.#generation, !!deep, out);
    const copy = wrapOut();
    cloneState?.(this, copy, !!deep);
    return copy;
  }

  isSameNode(other) {
    indexOf(this);
    return other === this;
  }

  contains(other) {
    const index = indexOf(this);
    if (other === null || other === undefined) {
      return false;
    }
    if (!isNode(other)) {
      throw notANode("contains");
    }
    return op_web_page_contains(
      index,
      this.#generation,
      other.#index,
      other.#generation,
    );
  }

  compareDocumentPosition(other) {
    indexOf(this);
    if (!isNode(other)) {
      throw notANode("compareDocumentPosition");
    }
    if (other === this) {
      return 0;
    }
    const mine = ancestorChain(this);
    const theirs = ancestorChain(other);
    if (mine[mine.length - 1] !== theirs[theirs.length - 1]) {
      // DISCONNECTED | IMPLEMENTATION_SPECIFIC | PRECEDING, consistently.
      return 1 | 32 | (this.#index < other.#index ? 4 : 2);
    }
    // Walk down from the common root to where the chains part.
    let i = mine.length - 1;
    let j = theirs.length - 1;
    while (i >= 0 && j >= 0 && mine[i] === theirs[j]) {
      i--;
      j--;
    }
    if (i < 0) {
      // `this` is an ancestor of `other`: CONTAINED_BY | FOLLOWING.
      return 16 | 4;
    }
    if (j < 0) {
      // `other` is an ancestor of `this`: CONTAINS | PRECEDING.
      return 8 | 2;
    }
    // Siblings under the common ancestor: which one comes first?
    let sibling = relative(mine[i], REL_NEXT_SIBLING);
    while (sibling !== null) {
      if (sibling === theirs[j]) {
        return 4;
      }
      sibling = relative(sibling, REL_NEXT_SIBLING);
    }
    return 2;
  }

  insertBefore(node, child) {
    indexOf(this);
    webidl.requiredArguments(
      arguments.length,
      2,
      "Failed to execute 'insertBefore' on 'Node'",
    );
    if (!isNode(node)) {
      throw notANode("insertBefore");
    }
    if (child !== null && child !== undefined && !isNode(child)) {
      throw notANode("insertBefore", 2);
    }
    insertBefore(this, node, child ?? null);
    return node;
  }

  appendChild(node) {
    indexOf(this);
    if (!isNode(node)) {
      throw notANode("appendChild");
    }
    insertBefore(this, node, null);
    return node;
  }

  replaceChild(node, child) {
    const index = indexOf(this);
    if (!isNode(node)) {
      throw notANode("replaceChild");
    }
    if (!isNode(child)) {
      throw notANode("replaceChild", 2);
    }
    op_web_page_replace_child(
      index,
      this.#generation,
      node.#index,
      node.#generation,
      child.#index,
      child.#generation,
    );
    return child;
  }

  removeChild(child) {
    const index = indexOf(this);
    if (!isNode(child)) {
      throw notANode("removeChild");
    }
    op_web_page_remove_child(
      index,
      this.#generation,
      child.#index,
      child.#generation,
    );
    return child;
  }

  dispatchEvent(ev) {
    indexOf(this);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'dispatchEvent' on 'EventTarget'",
    );
    // The brand, not the prototype chain: a `Proxy` of an event, or an
    // object that inherits from one, is not an event.
    if (!isEvent(ev)) {
      throw new TypeError(
        "Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.",
      );
    }
    if (getDispatched(ev)) {
      throw new DOMException(
        "The event is already being dispatched.",
        "InvalidStateError",
      );
    }
    event.setIsTrusted(ev, false);
    return dispatchNodeEvent(this, ev);
  }
}

/** The inclusive ancestors of a node, the node first and its root last. */
function ancestorChain(node) {
  const chain = [node];
  let parent = relative(node, REL_PARENT);
  while (parent !== null) {
    ArrayPrototypePush(chain, parent);
    parent = relative(parent, REL_PARENT);
  }
  return chain;
}

// Copies wrapper-held state (form control values) onto a clone. Set by
// `02_html.js`.
let cloneState = null;

function setCloneState(callback) {
  cloneState = callback;
}

const NODE_CONSTANTS = {
  __proto__: null,
  ELEMENT_NODE,
  ATTRIBUTE_NODE: 2,
  TEXT_NODE,
  CDATA_SECTION_NODE: 4,
  ENTITY_REFERENCE_NODE: 5,
  ENTITY_NODE: 6,
  PROCESSING_INSTRUCTION_NODE,
  COMMENT_NODE,
  DOCUMENT_NODE,
  DOCUMENT_TYPE_NODE,
  DOCUMENT_FRAGMENT_NODE,
  NOTATION_NODE: 12,
  DOCUMENT_POSITION_DISCONNECTED: 1,
  DOCUMENT_POSITION_PRECEDING: 2,
  DOCUMENT_POSITION_FOLLOWING: 4,
  DOCUMENT_POSITION_CONTAINS: 8,
  DOCUMENT_POSITION_CONTAINED_BY: 16,
  DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32,
};

function defineConstants(interface_, constants) {
  const names = ReflectOwnKeys(constants);
  for (let i = 0; i < names.length; i++) {
    const descriptor = {
      __proto__: null,
      value: constants[names[i]],
      enumerable: true,
      configurable: false,
      writable: false,
    };
    ObjectDefineProperty(interface_, names[i], descriptor);
    ObjectDefineProperty(interface_.prototype, names[i], descriptor);
  }
}

// --- mixins -------------------------------------------------------------------

/** Copies the members of a mixin class onto an interface's prototype. */
function mixin(interface_, mixinClass) {
  const descriptors = ObjectGetOwnPropertyDescriptors(mixinClass.prototype);
  const names = ReflectOwnKeys(descriptors);
  for (let i = 0; i < names.length; i++) {
    if (names[i] !== "constructor") {
      ObjectDefineProperty(interface_.prototype, names[i], {
        __proto__: null,
        ...descriptors[names[i]],
      });
    }
  }
}

function selectorArgument(selectors) {
  return toStr(selectors);
}

class ParentNodeMixin {
  get children() {
    return htmlCollection(
      wrapList(
        op_web_page_child_list(indexOf(this), generationOf(this), true),
      ),
    );
  }

  get firstElementChild() {
    return relative(this, REL_FIRST_ELEMENT_CHILD);
  }

  get lastElementChild() {
    return relative(this, REL_LAST_ELEMENT_CHILD);
  }

  get childElementCount() {
    return op_web_page_child_list(indexOf(this), generationOf(this), true)
      .length / 4;
  }

  prepend(...nodes) {
    indexOf(this);
    const node = nodesIntoNode(nodes);
    insertBefore(this, node, relative(this, REL_FIRST_CHILD));
  }

  append(...nodes) {
    indexOf(this);
    insertBefore(this, nodesIntoNode(nodes), null);
  }

  replaceChildren(...nodes) {
    indexOf(this);
    const node = nodesIntoNode(nodes);
    let child = relative(this, REL_FIRST_CHILD);
    while (child !== null) {
      op_web_page_remove(indexOf(child), generationOf(child));
      child = relative(this, REL_FIRST_CHILD);
    }
    insertBefore(this, node, null);
  }

  querySelector(selectors) {
    return op_web_page_query_selector(
        indexOf(this),
        generationOf(this),
        selectorArgument(selectors),
        out,
      )
      ? wrapOut()
      : null;
  }

  querySelectorAll(selectors) {
    return nodeList(
      wrapList(
        op_web_page_query_selector_all(
          indexOf(this),
          generationOf(this),
          selectorArgument(selectors),
        ),
      ),
    );
  }
}

class ChildNodeMixin {
  before(...nodes) {
    indexOf(this);
    const parent = relative(this, REL_PARENT);
    if (parent === null) {
      return;
    }
    // The first preceding sibling that is not one of the nodes.
    let viablePrevious = relative(this, REL_PREVIOUS_SIBLING);
    while (
      viablePrevious !== null && ArrayPrototypeIncludes(nodes, viablePrevious)
    ) {
      viablePrevious = relative(viablePrevious, REL_PREVIOUS_SIBLING);
    }
    const node = nodesIntoNode(nodes);
    const reference = viablePrevious === null
      ? relative(parent, REL_FIRST_CHILD)
      : relative(viablePrevious, REL_NEXT_SIBLING);
    insertBefore(parent, node, reference);
  }

  after(...nodes) {
    indexOf(this);
    const parent = relative(this, REL_PARENT);
    if (parent === null) {
      return;
    }
    let viableNext = relative(this, REL_NEXT_SIBLING);
    while (viableNext !== null && ArrayPrototypeIncludes(nodes, viableNext)) {
      viableNext = relative(viableNext, REL_NEXT_SIBLING);
    }
    insertBefore(parent, nodesIntoNode(nodes), viableNext);
  }

  replaceWith(...nodes) {
    indexOf(this);
    const parent = relative(this, REL_PARENT);
    if (parent === null) {
      return;
    }
    let viableNext = relative(this, REL_NEXT_SIBLING);
    while (viableNext !== null && ArrayPrototypeIncludes(nodes, viableNext)) {
      viableNext = relative(viableNext, REL_NEXT_SIBLING);
    }
    const node = nodesIntoNode(nodes);
    if (relative(this, REL_PARENT) === parent) {
      op_web_page_replace_child(
        indexOf(parent),
        generationOf(parent),
        indexOf(node),
        generationOf(node),
        indexOf(this),
        generationOf(this),
      );
    } else {
      insertBefore(parent, node, viableNext);
    }
  }

  remove() {
    op_web_page_remove(indexOf(this), generationOf(this));
  }
}

class NonDocumentTypeChildNodeMixin {
  get previousElementSibling() {
    return relative(this, REL_PREVIOUS_ELEMENT_SIBLING);
  }

  get nextElementSibling() {
    return relative(this, REL_NEXT_ELEMENT_SIBLING);
  }
}

function elementsBy(node, byClass, query) {
  return htmlCollection(
    wrapList(
      op_web_page_get_elements_by(
        indexOf(node),
        generationOf(node),
        byClass,
        toStr(query),
      ),
    ),
  );
}

// --- Element ------------------------------------------------------------------

const INSERT_POSITIONS = {
  __proto__: null,
  beforebegin: 0,
  afterbegin: 1,
  beforeend: 2,
  afterend: 3,
};

function insertAdjacent(element, where, node) {
  const position = INSERT_POSITIONS[StringPrototypeToLowerCase(toStr(where))];
  switch (position) {
    case 0: {
      const parent = relative(element, REL_PARENT);
      if (parent === null) {
        return false;
      }
      insertBefore(parent, node, element);
      return true;
    }
    case 1:
      insertBefore(element, node, relative(element, REL_FIRST_CHILD));
      return true;
    case 2:
      insertBefore(element, node, null);
      return true;
    case 3: {
      const parent = relative(element, REL_PARENT);
      if (parent === null) {
        return false;
      }
      insertBefore(parent, node, relative(element, REL_NEXT_SIBLING));
      return true;
    }
    default:
      throw new DOMException(
        "The value provided is not one of 'beforebegin', 'afterbegin', 'beforeend' or 'afterend'.",
        "SyntaxError",
      );
  }
}

/** Parses markup in the context of an element, into a detached fragment. */
function parseFragment(context, html) {
  op_web_page_parse_fragment(
    indexOf(context),
    generationOf(context),
    html,
    out,
  );
  return wrapOut();
}

class Element extends Node {
  // A brand: `#element in value` is true for elements only.
  #element() {}

  static {
    isElement = (value) =>
      typeof value === "object" && value !== null && #element in value;
  }

  static #check(value) {
    if (!isElement(value)) {
      throw illegalInvocation();
    }
    return indexOf(value);
  }

  get namespaceURI() {
    Element.#check(this);
    return elementName(this)[1];
  }

  get prefix() {
    Element.#check(this);
    return elementName(this)[2];
  }

  get localName() {
    Element.#check(this);
    return elementName(this)[0];
  }

  get tagName() {
    return op_web_page_node_name(Element.#check(this), generationOf(this));
  }

  get id() {
    Element.#check(this);
    return attributeOf(this, "id") ?? "";
  }

  set id(value) {
    Element.#check(this);
    setAttributeOf(this, "id", toStr(value));
  }

  get className() {
    Element.#check(this);
    return attributeOf(this, "class") ?? "";
  }

  set className(value) {
    Element.#check(this);
    setAttributeOf(this, "class", toStr(value));
  }

  get classList() {
    Element.#check(this);
    return tokenList(this, "class");
  }

  set classList(value) {
    Element.#check(this);
    setAttributeOf(this, "class", toStr(value));
  }

  get slot() {
    Element.#check(this);
    return attributeOf(this, "slot") ?? "";
  }

  set slot(value) {
    Element.#check(this);
    setAttributeOf(this, "slot", toStr(value));
  }

  hasAttributes() {
    return op_web_page_attributes(Element.#check(this), generationOf(this))
      .length > 0;
  }

  get attributes() {
    const flat = op_web_page_attributes(
      Element.#check(this),
      generationOf(this),
    );
    const items = [];
    for (let i = 0; i + 1 < flat.length; i += 2) {
      ArrayPrototypePush(
        items,
        new Attr(kConstruct, flat[i], flat[i + 1], this),
      );
    }
    return new NamedNodeMap(kConstruct, items);
  }

  getAttributeNames() {
    const flat = op_web_page_attributes(
      Element.#check(this),
      generationOf(this),
    );
    const names = [];
    for (let i = 0; i < flat.length; i += 2) {
      ArrayPrototypePush(names, flat[i]);
    }
    return names;
  }

  getAttribute(name) {
    Element.#check(this);
    return attributeOf(this, toStr(name));
  }

  setAttribute(name, value) {
    Element.#check(this);
    webidl.requiredArguments(
      arguments.length,
      2,
      "Failed to execute 'setAttribute' on 'Element'",
    );
    setAttributeOf(this, toStr(name), toStr(value));
  }

  removeAttribute(name) {
    Element.#check(this);
    removeAttributeOf(this, toStr(name));
  }

  toggleAttribute(name, force = undefined) {
    Element.#check(this);
    name = toStr(name);
    if (hasAttributeOf(this, name)) {
      if (force === undefined || !force) {
        removeAttributeOf(this, name);
        return false;
      }
      return true;
    }
    if (force === undefined || force) {
      setAttributeOf(this, name, "");
      return true;
    }
    return false;
  }

  hasAttribute(name) {
    Element.#check(this);
    return hasAttributeOf(this, toStr(name));
  }

  closest(selectors) {
    return op_web_page_closest(
        Element.#check(this),
        generationOf(this),
        selectorArgument(selectors),
        out,
      )
      ? wrapOut()
      : null;
  }

  matches(selectors) {
    return op_web_page_matches(
      Element.#check(this),
      generationOf(this),
      selectorArgument(selectors),
    );
  }

  webkitMatchesSelector(selectors) {
    return op_web_page_matches(
      Element.#check(this),
      generationOf(this),
      selectorArgument(selectors),
    );
  }

  getElementsByTagName(qualifiedName) {
    Element.#check(this);
    return elementsBy(this, false, qualifiedName);
  }

  getElementsByClassName(classNames) {
    Element.#check(this);
    return elementsBy(this, true, classNames);
  }

  get innerHTML() {
    return op_web_page_serialize(
      Element.#check(this),
      generationOf(this),
      false,
    );
  }

  set innerHTML(value) {
    op_web_page_set_inner_html(
      Element.#check(this),
      generationOf(this),
      value === null ? "" : toStr(value),
    );
  }

  get outerHTML() {
    return op_web_page_serialize(
      Element.#check(this),
      generationOf(this),
      true,
    );
  }

  set outerHTML(value) {
    Element.#check(this);
    value = value === null ? "" : toStr(value);
    const parent = relative(this, REL_PARENT);
    if (parent === null) {
      return;
    }
    if (isDocument(parent)) {
      throw new DOMException(
        "This element's parent is a document, which does not support this operation.",
        "NoModificationAllowedError",
      );
    }
    // A fragment parent parses as if in a `body`; the nearest stand-in here
    // is the element itself.
    const context = isElement(parent) ? parent : this;
    const fragment = parseFragment(context, value);
    op_web_page_replace_child(
      indexOf(parent),
      generationOf(parent),
      indexOf(fragment),
      generationOf(fragment),
      indexOf(this),
      generationOf(this),
    );
  }

  insertAdjacentElement(where, element) {
    Element.#check(this);
    if (!isElement(element)) {
      throw new TypeError(
        "Failed to execute 'insertAdjacentElement' on 'Element': parameter 2 is not of type 'Element'.",
      );
    }
    return insertAdjacent(this, where, element) ? element : null;
  }

  insertAdjacentText(where, data) {
    Element.#check(this);
    insertAdjacent(this, where, createText(toStr(data)));
  }

  insertAdjacentHTML(position, text) {
    Element.#check(this);
    const where = StringPrototypeToLowerCase(toStr(position));
    // The markup is parsed in the context of the node it goes into; inside
    // a fragment the nearest stand-in is the element itself.
    let parent = null;
    if (where === "beforebegin" || where === "afterend") {
      parent = relative(this, REL_PARENT);
      if (parent === null || isDocument(parent)) {
        throw new DOMException(
          "The element has no parent that markup can be inserted into.",
          "NoModificationAllowedError",
        );
      }
    }
    const fragment = parseFragment(
      parent !== null && isElement(parent) ? parent : this,
      toStr(text),
    );
    insertAdjacent(this, where, fragment);
  }
}
mixin(Element, ParentNodeMixin);
mixin(Element, ChildNodeMixin);
mixin(Element, NonDocumentTypeChildNodeMixin);

// --- CharacterData, Text, Comment --------------------------------------------

class CharacterData extends Node {
  #characterData() {}

  static {
    isCharacterData = (value) =>
      typeof value === "object" && value !== null && #characterData in value;
  }

  static #check(value) {
    if (!isCharacterData(value)) {
      throw illegalInvocation();
    }
    return indexOf(value);
  }

  get data() {
    return op_web_page_character_data(
      CharacterData.#check(this),
      generationOf(this),
    );
  }

  set data(value) {
    op_web_page_set_character_data(
      CharacterData.#check(this),
      generationOf(this),
      value === null ? "" : toStr(value),
    );
  }

  get length() {
    return op_web_page_character_data(
      CharacterData.#check(this),
      generationOf(this),
    ).length;
  }

  substringData(offset, count) {
    const data = this.data;
    offset = offset >>> 0;
    if (offset > data.length) {
      throw new DOMException("The offset is out of range.", "IndexSizeError");
    }
    return StringPrototypeSlice(data, offset, offset + (count >>> 0));
  }

  appendData(data) {
    this.data = this.data + toStr(data);
  }

  insertData(offset, data) {
    this.replaceData(offset, 0, data);
  }

  deleteData(offset, count) {
    this.replaceData(offset, count, "");
  }

  replaceData(offset, count, data) {
    const current = this.data;
    offset = offset >>> 0;
    if (offset > current.length) {
      throw new DOMException("The offset is out of range.", "IndexSizeError");
    }
    this.data = StringPrototypeSlice(current, 0, offset) + toStr(data) +
      StringPrototypeSlice(current, offset + (count >>> 0));
  }
}
mixin(CharacterData, ChildNodeMixin);
mixin(CharacterData, NonDocumentTypeChildNodeMixin);

class Text extends CharacterData {
  constructor(data = "", index = 0, generation = 0) {
    if (data !== kConstruct) {
      op_web_page_create_node(0, toStr(data), out);
      index = out[0];
      generation = out[1];
    }
    super(kConstruct, index, generation);
  }

  get wholeText() {
    let text = this.data;
    let node = relative(this, REL_PREVIOUS_SIBLING);
    while (node !== null && ObjectPrototypeIsPrototypeOf(TextPrototype, node)) {
      text = dataOf(node) + text;
      node = relative(node, REL_PREVIOUS_SIBLING);
    }
    node = relative(this, REL_NEXT_SIBLING);
    while (node !== null && ObjectPrototypeIsPrototypeOf(TextPrototype, node)) {
      text += dataOf(node);
      node = relative(node, REL_NEXT_SIBLING);
    }
    return text;
  }

  splitText(offset) {
    const data = this.data;
    offset = offset >>> 0;
    if (offset > data.length) {
      throw new DOMException("The offset is out of range.", "IndexSizeError");
    }
    const rest = createText(StringPrototypeSlice(data, offset));
    const parent = relative(this, REL_PARENT);
    if (parent !== null) {
      insertBefore(parent, rest, relative(this, REL_NEXT_SIBLING));
    }
    this.data = StringPrototypeSlice(data, 0, offset);
    return rest;
  }
}
const TextPrototype = Text.prototype;

class Comment extends CharacterData {
  constructor(data = "", index = 0, generation = 0) {
    if (data !== kConstruct) {
      op_web_page_create_node(1, toStr(data), out);
      index = out[0];
      generation = out[1];
    }
    super(kConstruct, index, generation);
  }
}

// --- DocumentType, DocumentFragment -------------------------------------------

class DocumentType extends Node {
  #doctype() {}

  static #info(value) {
    if (
      typeof value !== "object" || value === null || !(#doctype in value)
    ) {
      throw illegalInvocation();
    }
    return op_web_page_doctype_info(indexOf(value), generationOf(value));
  }

  get name() {
    return DocumentType.#info(this)[0];
  }

  get publicId() {
    return DocumentType.#info(this)[1];
  }

  get systemId() {
    return DocumentType.#info(this)[2];
  }
}
mixin(DocumentType, ChildNodeMixin);

class DocumentFragment extends Node {
  #fragment() {}

  static {
    isDocumentFragment = (value) =>
      typeof value === "object" && value !== null && #fragment in value;
  }

  constructor(key = undefined, index = 0, generation = 0) {
    if (key !== kConstruct) {
      op_web_page_create_node(2, "", out);
      index = out[0];
      generation = out[1];
    }
    super(kConstruct, index, generation);
  }

  getElementById(elementId) {
    indexOf(this);
    elementId = toStr(elementId);
    const all = wrapList(
      op_web_page_get_elements_by(
        indexOf(this),
        generationOf(this),
        false,
        "*",
      ),
    );
    for (let i = 0; i < all.length; i++) {
      if (attributeOf(all[i], "id") === elementId) {
        return all[i];
      }
    }
    return null;
  }
}
mixin(DocumentFragment, ParentNodeMixin);

// --- Document -----------------------------------------------------------------

class Document extends Node {
  #document() {}

  static {
    isDocument = (value) =>
      typeof value === "object" && value !== null && #document in value;
  }

  static #check(value) {
    if (!isDocument(value)) {
      throw illegalInvocation();
    }
    return indexOf(value);
  }

  get URL() {
    Document.#check(this);
    return documentUrl;
  }

  get documentURI() {
    Document.#check(this);
    return documentUrl;
  }

  get compatMode() {
    Document.#check(this);
    return "CSS1Compat";
  }

  get characterSet() {
    Document.#check(this);
    return "UTF-8";
  }

  get charset() {
    Document.#check(this);
    return "UTF-8";
  }

  get inputEncoding() {
    Document.#check(this);
    return "UTF-8";
  }

  get contentType() {
    Document.#check(this);
    return "text/html";
  }

  get doctype() {
    Document.#check(this);
    return documentNode(DOC_DOCTYPE);
  }

  get documentElement() {
    Document.#check(this);
    return documentNode(DOC_DOCUMENT_ELEMENT);
  }

  get head() {
    Document.#check(this);
    return documentNode(DOC_HEAD);
  }

  get body() {
    Document.#check(this);
    return documentNode(DOC_BODY);
  }

  get title() {
    Document.#check(this);
    const title = queryFirst(this, "title");
    if (title === null) {
      return "";
    }
    // Strip and collapse ASCII whitespace.
    return StringPrototypeTrim(
      StringPrototypeReplace(textOf(title), ASCII_WHITESPACE_ALL, " "),
    );
  }

  set title(value) {
    Document.#check(this);
    value = toStr(value);
    let title = queryFirst(this, "title");
    if (title === null) {
      const head = documentNode(DOC_HEAD);
      if (head === null) {
        return;
      }
      op_web_page_create_element("title", out);
      title = wrapOut();
      insertBefore(head, title, null);
    }
    setTextOf(title, value);
  }

  get readyState() {
    Document.#check(this);
    return readyState;
  }

  get location() {
    Document.#check(this);
    return windowRef?.location ?? null;
  }

  set location(value) {
    Document.#check(this);
    if (windowRef !== null) {
      windowRef.location.href = value;
    }
  }

  get defaultView() {
    Document.#check(this);
    return windowRef;
  }

  get activeElement() {
    Document.#check(this);
    if (activeElement !== null && isConnectedOf(activeElement)) {
      return activeElement;
    }
    return documentNode(DOC_BODY);
  }

  get referrer() {
    Document.#check(this);
    return "";
  }

  // Cookies are not wired to the page's cookie jar in this cut.
  get cookie() {
    Document.#check(this);
    return "";
  }

  set cookie(_value) {
    Document.#check(this);
  }

  get hidden() {
    Document.#check(this);
    return false;
  }

  get visibilityState() {
    Document.#check(this);
    return "visible";
  }

  get currentScript() {
    Document.#check(this);
    return null;
  }

  get forms() {
    Document.#check(this);
    return elementsBy(this, false, "form");
  }

  get images() {
    Document.#check(this);
    return elementsBy(this, false, "img");
  }

  get scripts() {
    Document.#check(this);
    return elementsBy(this, false, "script");
  }

  get links() {
    Document.#check(this);
    return htmlCollection(
      wrapList(
        op_web_page_query_selector_all(
          indexOf(this),
          generationOf(this),
          "a[href], area[href]",
        ),
      ),
    );
  }

  hasFocus() {
    Document.#check(this);
    return true;
  }

  getElementById(elementId) {
    Document.#check(this);
    return op_web_page_get_element_by_id(toStr(elementId), out)
      ? wrapOut()
      : null;
  }

  getElementsByTagName(qualifiedName) {
    Document.#check(this);
    return elementsBy(this, false, qualifiedName);
  }

  getElementsByClassName(classNames) {
    Document.#check(this);
    return elementsBy(this, true, classNames);
  }

  getElementsByName(elementName) {
    Document.#check(this);
    elementName = toStr(elementName);
    const all = wrapList(
      op_web_page_get_elements_by(
        indexOf(this),
        generationOf(this),
        false,
        "*",
      ),
    );
    const named = [];
    for (let i = 0; i < all.length; i++) {
      if (attributeOf(all[i], "name") === elementName) {
        ArrayPrototypePush(named, all[i]);
      }
    }
    return nodeList(named);
  }

  createElement(localName, _options = undefined) {
    Document.#check(this);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'createElement' on 'Document'",
    );
    op_web_page_create_element(toStr(localName), out);
    return wrapOut();
  }

  createElementNS(namespace, qualifiedName, _options = undefined) {
    Document.#check(this);
    webidl.requiredArguments(
      arguments.length,
      2,
      "Failed to execute 'createElementNS' on 'Document'",
    );
    op_web_page_create_element_ns(
      namespace === null || namespace === undefined ? "" : toStr(namespace),
      toStr(qualifiedName),
      out,
    );
    return wrapOut();
  }

  createTextNode(data) {
    Document.#check(this);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'createTextNode' on 'Document'",
    );
    return createText(toStr(data));
  }

  createComment(data) {
    Document.#check(this);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'createComment' on 'Document'",
    );
    op_web_page_create_node(1, toStr(data), out);
    return wrapOut();
  }

  createDocumentFragment() {
    Document.#check(this);
    return createFragment();
  }

  importNode(node, deep = false) {
    Document.#check(this);
    if (!isNode(node)) {
      throw notANode("importNode");
    }
    return node.cloneNode(deep);
  }

  // One document per page: every node is already this document's.
  adoptNode(node) {
    Document.#check(this);
    if (!isNode(node)) {
      throw notANode("adoptNode");
    }
    if (isDocument(node)) {
      throw new DOMException(
        "A document cannot be adopted.",
        "NotSupportedError",
      );
    }
    op_web_page_remove(indexOf(node), generationOf(node));
    return node;
  }
}
mixin(Document, ParentNodeMixin);

const interfaces = [
  Node,
  Element,
  CharacterData,
  Text,
  Comment,
  DocumentType,
  DocumentFragment,
  Document,
];
for (let i = 0; i < interfaces.length; i++) {
  webidl.configureInterface(interfaces[i]);
}
defineConstants(Node, NODE_CONSTANTS);

// --- dataset --------------------------------------------------------------------

const UPPERCASE = new SafeRegExp("[A-Z]", "g");
const DASH_LOWERCASE = new SafeRegExp("-([a-z])", "g");
const DASH_LOWERCASE_ONCE = new SafeRegExp("-[a-z]");

function datasetAttribute(property) {
  return "data-" +
    StringPrototypeReplace(
      property,
      UPPERCASE,
      (letter) => "-" + StringPrototypeToLowerCase(letter),
    );
}

function datasetProperty(attribute) {
  return StringPrototypeReplace(
    StringPrototypeSlice(attribute, 5),
    DASH_LOWERCASE,
    (_match, letter) => StringPrototypeToUpperCase(letter),
  );
}

function datasetKeys(element) {
  const flat = op_web_page_attributes(indexOf(element), generationOf(element));
  const keys = [];
  for (let i = 0; i < flat.length; i += 2) {
    if (StringPrototypeStartsWith(flat[i], "data-")) {
      ArrayPrototypePush(keys, datasetProperty(flat[i]));
    }
  }
  return keys;
}

class DOMStringMap {
  constructor(key = undefined) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
  }
}
webidl.configureInterface(DOMStringMap);

/** `HTMLElement.dataset`: the `data-*` attributes of an element. */
function createDataset(element) {
  return new Proxy(new DOMStringMap(kConstruct), {
    __proto__: null,
    get(target, property) {
      if (typeof property !== "string") {
        return target[property];
      }
      const value = attributeOf(element, datasetAttribute(property));
      if (value !== null) {
        return value;
      }
      return ReflectHas(target, property) ? target[property] : undefined;
    },
    set(_target, property, value) {
      if (typeof property !== "string") {
        return false;
      }
      if (RegExpPrototypeTest(DASH_LOWERCASE_ONCE, property)) {
        throw new DOMException(
          "The property name is not a valid dataset name.",
          "SyntaxError",
        );
      }
      setAttributeOf(element, datasetAttribute(property), toStr(value));
      return true;
    },
    has(target, property) {
      if (typeof property !== "string") {
        return ReflectHas(target, property);
      }
      return hasAttributeOf(element, datasetAttribute(property)) ||
        ReflectHas(target, property);
    },
    deleteProperty(_target, property) {
      if (typeof property === "string") {
        removeAttributeOf(element, datasetAttribute(property));
      }
      return true;
    },
    ownKeys(_target) {
      return datasetKeys(element);
    },
    getOwnPropertyDescriptor(_target, property) {
      if (typeof property !== "string") {
        return undefined;
      }
      const value = attributeOf(element, datasetAttribute(property));
      if (value === null) {
        return undefined;
      }
      return {
        __proto__: null,
        value,
        writable: true,
        enumerable: true,
        configurable: true,
      };
    },
  });
}

// --- small string helpers shared with 02_html.js ---------------------------------

/** Strips leading and trailing ASCII whitespace. */
function stripAsciiWhitespace(value) {
  let start = 0;
  let end = value.length;
  const isSpace = (code) =>
    code === 0x20 || code === 0x09 || code === 0x0a || code === 0x0c ||
    code === 0x0d;
  while (start < end && isSpace(StringPrototypeCharCodeAt(value, start))) {
    start++;
  }
  while (end > start && isSpace(StringPrototypeCharCodeAt(value, end - 1))) {
    end--;
  }
  return StringPrototypeSlice(value, start, end);
}

/** `name: value` pairs of a `style` attribute. Naive: splits on `;` and `:`. */
function parseDeclarations(text) {
  const declarations = [];
  const parts = StringPrototypeSplit(text, ";");
  for (let i = 0; i < parts.length; i++) {
    const colon = StringPrototypeIndexOf(parts[i], ":");
    if (colon === -1) {
      continue;
    }
    const name = StringPrototypeToLowerCase(
      stripAsciiWhitespace(StringPrototypeSlice(parts[i], 0, colon)),
    );
    const value = stripAsciiWhitespace(
      StringPrototypeSlice(parts[i], colon + 1),
    );
    if (name !== "" && value !== "") {
      ArrayPrototypePush(declarations, [name, value]);
    }
  }
  return declarations;
}

export {
  Attr,
  attributeOf,
  CharacterData,
  closestOf,
  Comment,
  containsOf,
  createDataset,
  createText,
  dispatchNodeEvent,
  Document,
  DocumentFragment,
  documentNode,
  DocumentType,
  DOMStringMap,
  DOMTokenList,
  Element,
  elementById,
  elementName,
  elementsBy,
  fireEvent,
  generationOf,
  getActiveElement,
  getDocumentUrl,
  getWindow,
  hasAttributeOf,
  HTMLCollection,
  htmlCollection,
  htmlLocalName,
  indexOf,
  isConnectedOf,
  isElement,
  isNode,
  kConstruct,
  matchesOf,
  NamedNodeMap,
  Node,
  nodeFromId,
  NodeList,
  nodeList,
  nodeListItems,
  parseDeclarations,
  queryAll,
  queryFirst,
  registerEventHandlerName,
  registerHtmlInterface,
  relative,
  removeAttributeOf,
  setActiveElement,
  setAttributeOf,
  setClickActivation,
  setCloneState,
  setDefaultHtmlInterface,
  setDocumentUrl,
  setReadyState,
  setTextOf,
  setWindow,
  stripAsciiWhitespace,
  Text,
  textOf,
  tokenList,
  toStr,
  wrapList,
};

export const DOC = {
  __proto__: null,
  DOCUMENT: DOC_DOCUMENT,
  DOCUMENT_ELEMENT: DOC_DOCUMENT_ELEMENT,
  HEAD: DOC_HEAD,
  BODY: DOC_BODY,
  DOCTYPE: DOC_DOCTYPE,
};

export const REL = {
  __proto__: null,
  PARENT: REL_PARENT,
  FIRST_CHILD: REL_FIRST_CHILD,
  LAST_CHILD: REL_LAST_CHILD,
  PREVIOUS_SIBLING: REL_PREVIOUS_SIBLING,
  NEXT_SIBLING: REL_NEXT_SIBLING,
  PARENT_ELEMENT: REL_PARENT_ELEMENT,
  ROOT: REL_ROOT,
  TEMPLATE_CONTENTS: REL_TEMPLATE_CONTENTS,
};
