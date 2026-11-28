// Copyright 2018-2026 the Deno authors. MIT license.

// This module follows most of the WHATWG Living Standard for the DOM logic.
// Many parts of the DOM are not implemented in Deno, but the logic for those
// parts still exists.  This means you will observe a lot of strange structures
// and impossible logic branches based on what Deno currently supports.

(function () {
const { core, internals, primordials } = __bootstrap;
const {
  ArrayPrototypeIncludes,
  ArrayPrototypeIndexOf,
  ArrayPrototypeMap,
  ArrayPrototypePush,
  ArrayPrototypeSlice,
  ArrayPrototypeSplice,
  ArrayPrototypeUnshift,
  Boolean,
  Error,
  FunctionPrototypeCall,
  JSONStringify,
  MapPrototypeDelete,
  MapPrototypeGet,
  MapPrototypeSet,
  ObjectCreate,
  ObjectDefineProperty,
  ObjectFreeze,
  ObjectGetOwnPropertyDescriptor,
  ObjectGetPrototypeOf,
  ObjectPrototype,
  ObjectPrototypeIsPrototypeOf,
  ObjectSetPrototypeOf,
  ReflectDefineProperty,
  SafeArrayIterator,
  SafeMap,
  SafeWeakMap,
  String,
  StringPrototypeStartsWith,
  Symbol,
  SymbolFor,
  SymbolIterator,
  SymbolToStringTag,
  TypeError,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");

// Lazy-load createFilteredInspectProxy from console to avoid
// circular dependency at load time. Only needed for custom inspect.
let _createFilteredInspectProxy;
function getCreateFilteredInspectProxy() {
  if (!_createFilteredInspectProxy) {
    _createFilteredInspectProxy = core.loadExtScript(
      "ext:deno_web/01_console.js",
    ).createFilteredInspectProxy;
  }
  return _createFilteredInspectProxy;
}

// This should be set via setGlobalThis this is required so that if even
// user deletes globalThis it is still usable
let globalThis_;

function saveGlobalThisReference(val) {
  globalThis_ = val;
}

// denext: an Event's internal slots are private fields of the class, and an
// EventTarget's listeners are in module-private weak maps (further down).
// They were own symbol-keyed properties, which any script could list with
// `Object.getOwnPropertySymbols()` and write: `isTrusted`, the flags and the
// listener lists could all be forged. The functions below are the only way
// in; the class's static block fills them in.

let isEvent;
let getDispatched;
let getPath;
let getStopImmediatePropagation;
let setCurrentTarget;
let setIsTrusted;
let setDispatched;
let setEventPhase;
let setInPassiveListener;
let setPath;
let setRelatedTarget;
let setTarget;
let setStopImmediatePropagation;
let prepareScriptDispatch;
let isTrusted;
let eventInternals;

// denext: off in Deno. An embedder that runs script it does not trust (the
// page runtime, ext/web_page) turns it on before that script runs, with
// `enableUnforgeableEvents()`; nothing turns it off. With it:
// - `isTrusted` is what the DOM Standard says, [LegacyUnforgeable]: an own,
//   non-configurable accessor of every event, so that no own property and no
//   subclass can shadow it. (Deno keeps it on the prototype: defining it per
//   event costs too much for a runtime whose scripts are trusted.)
// - `dispatchEvent()` checks its argument and the dispatch flag first, and
//   clears `isTrusted`, as the DOM Standard says: what a script dispatches
//   is never trusted, whatever the event was before.
// - The extensions' own events do not go through a target's `dispatchEvent`
//   property, which a script can replace (on the object or on a prototype)
//   to be handed a trusted event before its dispatch: `dispatchTrusted()`
//   below runs the dispatch itself, and marks the event trusted.
let unforgeableEvents = false;

function enableUnforgeableEvents() {
  unforgeableEvents = true;
}

function unforgeableEventsEnabled() {
  return unforgeableEvents;
}

// denext: the targets an embedder dispatches at with an algorithm of its own
// (ext/web_page: nodes, along the tree). Set once, by the embedder's
// extension script; `dispatchTrusted()` asks it first.
let nodeDispatcher = null;

/**
 * @param {(target: unknown) => boolean} isNode
 * @param {(target: EventTarget, event: Event) => boolean} dispatchNode
 *   dispatches an event that is not being dispatched, as it is (the trust is
 *   the caller's), and answers whether it was not canceled
 */
function setNodeDispatcher(isNode, dispatchNode) {
  if (nodeDispatcher !== null) {
    throw new TypeError("The node dispatcher is already set");
  }
  nodeDispatcher = { __proto__: null, isNode, dispatchNode };
}

const isTrustedDescriptor = {
  __proto__: null,
  get: undefined,
  set: undefined,
  enumerable: true,
  configurable: false,
};

const EMPTY_EVENT_INIT = ObjectFreeze(ObjectSetPrototypeOf({}, null));

class Event {
  #attributes;
  #canceledFlag = false;
  #stopPropagationFlag = false;
  #stopImmediatePropagationFlag = false;
  #inPassiveListener = false;
  #dispatched = false;
  #isTrusted = false;
  #path = [];

  static {
    isEvent = (value) =>
      typeof value === "object" && value !== null && #attributes in value;
    getDispatched = (event) => event.#dispatched;
    getPath = (event) => event.#path;
    getStopImmediatePropagation = (event) =>
      event.#stopImmediatePropagationFlag;
    setCurrentTarget = (event, value) => {
      event.#attributes.currentTarget = value;
    };
    setIsTrusted = (event, value) => {
      event.#isTrusted = value;
    };
    setDispatched = (event, value) => {
      event.#dispatched = value;
    };
    setEventPhase = (event, value) => {
      event.#attributes.eventPhase = value;
    };
    setInPassiveListener = (event, value) => {
      event.#inPassiveListener = value;
    };
    setPath = (event, value) => {
      event.#path = value;
    };
    setRelatedTarget = (event, value) => {
      event.#attributes.relatedTarget = value;
    };
    setTarget = (event, value) => {
      event.#attributes.target = value;
    };
    setStopImmediatePropagation = (event, value) => {
      event.#stopImmediatePropagationFlag = value;
    };

    // The steps of `dispatchEvent()` before "dispatch", with
    // `unforgeableEvents`: https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent
    // The extensions' trusted events do not come this way
    // (`dispatchTrusted()`).
    prepareScriptDispatch = (event) => {
      if (event.#dispatched) {
        throw new DOMException("Invalid event state", "InvalidStateError");
      }
      event.#isTrusted = false;
    };

    isTrusted = ObjectGetOwnPropertyDescriptor({
      get isTrusted() {
        if (isEvent(this)) {
          return this.#isTrusted;
        }
        if (unforgeableEvents) {
          throw new TypeError("Illegal invocation");
        }
        return undefined;
      },
    }, "isTrusted").get;
    isTrustedDescriptor.get = isTrusted;
    ObjectFreeze(isTrustedDescriptor);

    // denext: the slots an embedder's extension needs to dispatch events
    // along a tree of its own (ext/web_page: the DOM Standard's dispatch over
    // nodes, which `dispatch()` below does not do). Only this module's result
    // carries them; nothing here uses the object.
    eventInternals = ObjectFreeze({
      __proto__: null,
      isEvent,
      /** The object behind `type`, `target`, `currentTarget`, `eventPhase`,
       * `bubbles`, `cancelable`, `composed` and `timeStamp`. */
      getAttributes: (event) => event.#attributes,
      getCanceled: (event) => event.#canceledFlag,
      getDispatched,
      setDispatched,
      setPath,
      getStopPropagation: (event) => event.#stopPropagationFlag,
      setStopPropagation: (event, value) => {
        event.#stopPropagationFlag = value;
      },
      getStopImmediatePropagation,
      setStopImmediatePropagation,
      setInPassiveListener,
    });
  }

  constructor(type, eventInitDict = EMPTY_EVENT_INIT) {
    if (unforgeableEvents) {
      ObjectDefineProperty(this, "isTrusted", isTrustedDescriptor);
    }

    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'Event'",
    );
    type = webidl.converters.DOMString(
      type,
      "Failed to construct 'Event'",
      "Argument 1",
    );

    this.#attributes = {
      type,
      bubbles: !!eventInitDict.bubbles,
      cancelable: !!eventInitDict.cancelable,
      composed: !!eventInitDict.composed,
      currentTarget: null,
      eventPhase: Event.NONE,
      target: null,
      timeStamp: 0,
    };
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: EVENT_PROPS,
      }),
      inspectOptions,
    );
  }

  get type() {
    return this.#attributes.type;
  }

  get target() {
    return this.#attributes.target;
  }

  get srcElement() {
    return null;
  }

  set srcElement(_) {
    // this member is deprecated
  }

  get currentTarget() {
    return this.#attributes.currentTarget;
  }

  composedPath() {
    const path = this.#path;
    if (path.length === 0) {
      return [];
    }

    if (!this.currentTarget) {
      throw new Error("assertion error");
    }
    const composedPath = [
      {
        item: this.currentTarget,
        itemInShadowTree: false,
        relatedTarget: null,
        rootOfClosedTree: false,
        slotInClosedTree: false,
        target: null,
        touchTargetList: [],
      },
    ];

    let currentTargetIndex = 0;
    let currentTargetHiddenSubtreeLevel = 0;

    for (let index = path.length - 1; index >= 0; index--) {
      const { item, rootOfClosedTree, slotInClosedTree } = path[index];

      if (rootOfClosedTree) {
        currentTargetHiddenSubtreeLevel++;
      }

      if (item === this.currentTarget) {
        currentTargetIndex = index;
        break;
      }

      if (slotInClosedTree) {
        currentTargetHiddenSubtreeLevel--;
      }
    }

    let currentHiddenLevel = currentTargetHiddenSubtreeLevel;
    let maxHiddenLevel = currentTargetHiddenSubtreeLevel;

    for (let i = currentTargetIndex - 1; i >= 0; i--) {
      const { item, rootOfClosedTree, slotInClosedTree } = path[i];

      if (rootOfClosedTree) {
        currentHiddenLevel++;
      }

      if (currentHiddenLevel <= maxHiddenLevel) {
        ArrayPrototypeUnshift(composedPath, {
          item,
          itemInShadowTree: false,
          relatedTarget: null,
          rootOfClosedTree: false,
          slotInClosedTree: false,
          target: null,
          touchTargetList: [],
        });
      }

      if (slotInClosedTree) {
        currentHiddenLevel--;

        if (currentHiddenLevel < maxHiddenLevel) {
          maxHiddenLevel = currentHiddenLevel;
        }
      }
    }

    currentHiddenLevel = currentTargetHiddenSubtreeLevel;
    maxHiddenLevel = currentTargetHiddenSubtreeLevel;

    for (let index = currentTargetIndex + 1; index < path.length; index++) {
      const { item, rootOfClosedTree, slotInClosedTree } = path[index];

      if (slotInClosedTree) {
        currentHiddenLevel++;
      }

      if (currentHiddenLevel <= maxHiddenLevel) {
        ArrayPrototypePush(composedPath, {
          item,
          itemInShadowTree: false,
          relatedTarget: null,
          rootOfClosedTree: false,
          slotInClosedTree: false,
          target: null,
          touchTargetList: [],
        });
      }

      if (rootOfClosedTree) {
        currentHiddenLevel--;

        if (currentHiddenLevel < maxHiddenLevel) {
          maxHiddenLevel = currentHiddenLevel;
        }
      }
    }
    return ArrayPrototypeMap(composedPath, (p) => p.item);
  }

  get NONE() {
    return Event.NONE;
  }

  get CAPTURING_PHASE() {
    return Event.CAPTURING_PHASE;
  }

  get AT_TARGET() {
    return Event.AT_TARGET;
  }

  get BUBBLING_PHASE() {
    return Event.BUBBLING_PHASE;
  }

  get eventPhase() {
    return this.#attributes.eventPhase;
  }

  stopPropagation() {
    this.#stopPropagationFlag = true;
  }

  get cancelBubble() {
    return this.#stopPropagationFlag;
  }

  set cancelBubble(value) {
    this.#stopPropagationFlag = webidl.converters.boolean(value);
  }

  stopImmediatePropagation() {
    this.#stopPropagationFlag = true;
    this.#stopImmediatePropagationFlag = true;
  }

  get bubbles() {
    return this.#attributes.bubbles;
  }

  get cancelable() {
    return this.#attributes.cancelable;
  }

  get returnValue() {
    return !this.#canceledFlag;
  }

  set returnValue(value) {
    if (!webidl.converters.boolean(value)) {
      if (this.#attributes.cancelable && !this.#inPassiveListener) {
        this.#canceledFlag = true;
      }
    }
  }

  preventDefault() {
    if (this.#attributes.cancelable && !this.#inPassiveListener) {
      this.#canceledFlag = true;
    }
  }

  get defaultPrevented() {
    return this.#canceledFlag;
  }

  get composed() {
    return this.#attributes.composed;
  }

  get initialized() {
    return true;
  }

  get timeStamp() {
    return this.#attributes.timeStamp;
  }
}

ObjectDefineProperty(Event, "NONE", {
  __proto__: null,
  value: 0,
  writable: false,
  enumerable: true,
  configurable: false,
});

ObjectDefineProperty(Event, "CAPTURING_PHASE", {
  __proto__: null,
  value: 1,
  writable: false,
  enumerable: true,
  configurable: false,
});

ObjectDefineProperty(Event, "AT_TARGET", {
  __proto__: null,
  value: 2,
  writable: false,
  enumerable: true,
  configurable: false,
});

ObjectDefineProperty(Event, "BUBBLING_PHASE", {
  __proto__: null,
  value: 3,
  writable: false,
  enumerable: true,
  configurable: false,
});

const EventPrototype = Event.prototype;

ObjectDefineProperty(EventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "Event",
  writable: false,
  enumerable: false,
  configurable: true,
});

// Not spec compliant. The spec defines it as [LegacyUnforgeable]
// but doing so has a big performance hit
// denext: configurable, so that an embedder that turned
// `enableUnforgeableEvents()` on, where every event has the accessor itself,
// can delete this one.
ReflectDefineProperty(Event.prototype, "isTrusted", {
  __proto__: null,
  enumerable: true,
  configurable: true,
  get: isTrusted,
});

function defineEnumerableProps(
  Ctor,
  props,
) {
  for (let i = 0; i < props.length; ++i) {
    const prop = props[i];
    ReflectDefineProperty(Ctor.prototype, prop, {
      __proto__: null,
      enumerable: true,
    });
  }
}

const EVENT_PROPS = [
  "bubbles",
  "cancelable",
  "composed",
  "currentTarget",
  "defaultPrevented",
  "eventPhase",
  "srcElement",
  "target",
  "returnValue",
  "timeStamp",
  "type",
];

defineEnumerableProps(Event, EVENT_PROPS);

// This is currently the only node type we are using, so instead of implementing
// the whole of the Node interface at the moment, this just gives us the one
// value to power the standards based logic
const DOCUMENT_FRAGMENT_NODE = 11;

// DOM Logic Helper functions and type guards

/** Get the parent node, for event targets that have a parent.
 *
 * Ref: https://dom.spec.whatwg.org/#get-the-parent */
function getParent(eventTarget) {
  return isNode(eventTarget) ? eventTarget.parentNode : null;
}

function getRoot(eventTarget) {
  return isNode(eventTarget)
    ? eventTarget.getRootNode({ composed: true })
    : null;
}

function isNode(
  eventTarget,
) {
  return eventTarget?.nodeType !== undefined;
}

// https://dom.spec.whatwg.org/#concept-shadow-including-inclusive-ancestor
function isShadowInclusiveAncestor(
  ancestor,
  node,
) {
  while (isNode(node)) {
    if (node === ancestor) {
      return true;
    }

    if (isShadowRoot(node)) {
      node = node && getHost(node);
    } else {
      node = getParent(node);
    }
  }

  return false;
}

function isShadowRoot(nodeImpl) {
  return Boolean(
    nodeImpl &&
      isNode(nodeImpl) &&
      nodeImpl.nodeType === DOCUMENT_FRAGMENT_NODE &&
      getHost(nodeImpl) != null,
  );
}

function isSlottable(
  /* nodeImpl, */
) {
  // TODO(marcosc90) currently there aren't any slottables nodes
  // https://dom.spec.whatwg.org/#concept-slotable
  // return isNode(nodeImpl) && ReflectHas(nodeImpl, "assignedSlot");
  return false;
}

// DOM Logic functions

/** Append a path item to an event's path.
 *
 * Ref: https://dom.spec.whatwg.org/#concept-event-path-append
 */
function appendToEventPath(
  eventImpl,
  target,
  targetOverride,
  relatedTarget,
  touchTargets,
  slotInClosedTree,
) {
  const itemInShadowTree = isNode(target) && isShadowRoot(getRoot(target));
  const rootOfClosedTree = isShadowRoot(target) &&
    getMode(target) === "closed";

  ArrayPrototypePush(getPath(eventImpl), {
    item: target,
    itemInShadowTree,
    target: targetOverride,
    relatedTarget,
    touchTargetList: touchTargets,
    rootOfClosedTree,
    slotInClosedTree,
  });
}

function dispatch(
  targetImpl,
  eventImpl,
  targetOverride,
) {
  let clearTargets = false;
  let activationTarget = null;

  setDispatched(eventImpl, true);

  targetOverride = targetOverride ?? targetImpl;
  const eventRelatedTarget = eventImpl.relatedTarget;
  let relatedTarget = retarget(eventRelatedTarget, targetImpl);

  if (targetImpl !== relatedTarget || targetImpl === eventRelatedTarget) {
    const touchTargets = [];

    appendToEventPath(
      eventImpl,
      targetImpl,
      targetOverride,
      relatedTarget,
      touchTargets,
      false,
    );

    const isActivationEvent = eventImpl.type === "click";

    if (isActivationEvent && getHasActivationBehavior(targetImpl)) {
      activationTarget = targetImpl;
    }

    let slotInClosedTree = false;
    let slottable = isSlottable(targetImpl) && getAssignedSlot(targetImpl)
      ? targetImpl
      : null;
    let parent = getParent(targetImpl);

    // Populate event path
    // https://dom.spec.whatwg.org/#event-path
    while (parent !== null) {
      if (slottable !== null) {
        slottable = null;

        const parentRoot = getRoot(parent);
        if (
          isShadowRoot(parentRoot) &&
          parentRoot &&
          getMode(parentRoot) === "closed"
        ) {
          slotInClosedTree = true;
        }
      }

      relatedTarget = retarget(eventRelatedTarget, parent);

      if (
        isNode(parent) &&
        isShadowInclusiveAncestor(getRoot(targetImpl), parent)
      ) {
        appendToEventPath(
          eventImpl,
          parent,
          null,
          relatedTarget,
          touchTargets,
          slotInClosedTree,
        );
      } else if (parent === relatedTarget) {
        parent = null;
      } else {
        targetImpl = parent;

        if (
          isActivationEvent &&
          activationTarget === null &&
          getHasActivationBehavior(targetImpl)
        ) {
          activationTarget = targetImpl;
        }

        appendToEventPath(
          eventImpl,
          parent,
          targetImpl,
          relatedTarget,
          touchTargets,
          slotInClosedTree,
        );
      }

      if (parent !== null) {
        parent = getParent(parent);
      }

      slotInClosedTree = false;
    }

    let clearTargetsTupleIndex = -1;
    const path = getPath(eventImpl);
    for (
      let i = path.length - 1;
      i >= 0 && clearTargetsTupleIndex === -1;
      i--
    ) {
      if (path[i].target !== null) {
        clearTargetsTupleIndex = i;
      }
    }
    const clearTargetsTuple = path[clearTargetsTupleIndex];

    clearTargets = (isNode(clearTargetsTuple.target) &&
      isShadowRoot(getRoot(clearTargetsTuple.target))) ||
      (isNode(clearTargetsTuple.relatedTarget) &&
        isShadowRoot(getRoot(clearTargetsTuple.relatedTarget)));

    setEventPhase(eventImpl, Event.CAPTURING_PHASE);

    for (let i = path.length - 1; i >= 0; --i) {
      const tuple = path[i];

      if (tuple.target === null) {
        invokeEventListeners(tuple, eventImpl);
      }
    }

    for (let i = 0; i < path.length; i++) {
      const tuple = path[i];

      if (tuple.target !== null) {
        setEventPhase(eventImpl, Event.AT_TARGET);
      } else {
        setEventPhase(eventImpl, Event.BUBBLING_PHASE);
      }

      if (
        (eventImpl.eventPhase === Event.BUBBLING_PHASE &&
          eventImpl.bubbles) ||
        eventImpl.eventPhase === Event.AT_TARGET
      ) {
        invokeEventListeners(tuple, eventImpl);
      }
    }
  }

  setEventPhase(eventImpl, Event.NONE);
  setCurrentTarget(eventImpl, null);
  setPath(eventImpl, []);
  setDispatched(eventImpl, false);
  eventImpl.cancelBubble = false;
  setStopImmediatePropagation(eventImpl, false);

  if (clearTargets) {
    setTarget(eventImpl, null);
    setRelatedTarget(eventImpl, null);
  }

  // TODO(bartlomieju): invoke activation targets if HTML nodes will be implemented
  // if (activationTarget !== null) {
  //   if (!eventImpl.defaultPrevented) {
  //     activationTarget._activationBehavior();
  //   }
  // }

  return !eventImpl.defaultPrevented;
}

/** Inner invoking of the event listeners where the resolved listeners are
 * called.
 *
 * Ref: https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke */
function innerInvokeEventListeners(
  eventImpl,
  targetListeners,
) {
  let found = false;

  const { type } = eventImpl;

  if (!targetListeners || !targetListeners[type]) {
    return found;
  }

  let handlers = targetListeners[type];
  const handlersLength = handlers.length;

  // Copy event listeners before iterating since the list can be modified during the iteration.
  if (handlersLength > 1) {
    handlers = ArrayPrototypeSlice(targetListeners[type]);
  }

  for (let i = 0; i < handlersLength; i++) {
    const listener = handlers[i];

    if (
      getStopImmediatePropagation(eventImpl) &&
      !listener.options[kResistStopImmediatePropagation]
    ) {
      continue;
    }

    let capture, once, passive;
    if (typeof listener.options === "boolean") {
      capture = listener.options;
      once = false;
      passive = false;
    } else {
      capture = listener.options.capture;
      once = listener.options.once;
      passive = listener.options.passive;
    }

    // Check if the event listener has been removed since the listeners has been cloned.
    if (!ArrayPrototypeIncludes(targetListeners[type], listener)) {
      continue;
    }

    found = true;

    if (
      (eventImpl.eventPhase === Event.CAPTURING_PHASE && !capture) ||
      (eventImpl.eventPhase === Event.BUBBLING_PHASE && capture)
    ) {
      continue;
    }

    if (once) {
      removeListener(targetListeners, type, listener);
    }

    if (passive) {
      setInPassiveListener(eventImpl, true);
    }

    try {
      if (typeof listener.callback === "object") {
        if (typeof listener.callback.handleEvent === "function") {
          listener.callback.handleEvent(eventImpl);
        }
      } else {
        FunctionPrototypeCall(
          listener.callback,
          eventImpl.currentTarget,
          eventImpl,
        );
      }
    } catch (error) {
      reportException(error);
    }

    setInPassiveListener(eventImpl, false);
  }

  return found;
}

/** Invokes the listeners on a given event path with the supplied event.
 *
 * Ref: https://dom.spec.whatwg.org/#concept-event-listener-invoke */
function invokeEventListeners(tuple, eventImpl) {
  const path = getPath(eventImpl);
  if (path.length === 1) {
    const t = path[0];
    if (t.target) {
      setTarget(eventImpl, t.target);
    }
  } else {
    const tupleIndex = ArrayPrototypeIndexOf(path, tuple);
    for (let i = tupleIndex; i >= 0; i--) {
      const t = path[i];
      if (t.target) {
        setTarget(eventImpl, t.target);
        break;
      }
    }
  }

  setRelatedTarget(eventImpl, tuple.relatedTarget);

  if (eventImpl.cancelBubble) {
    return;
  }

  setCurrentTarget(eventImpl, tuple.item);

  try {
    innerInvokeEventListeners(eventImpl, getListeners(tuple.item));
  } catch (error) {
    reportException(error);
  }
}

// https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
function normalizeEventHandlerOptions(
  options,
) {
  if (
    typeof options === "boolean" || typeof options === "undefined" ||
    options === null
  ) {
    return {
      capture: Boolean(options),
    };
  } else {
    return {
      capture: Boolean(options.capture),
    };
  }
}

function removeListener(
  targetListeners,
  type,
  listener,
  index = ArrayPrototypeIndexOf(targetListeners[type], listener),
) {
  if (index === -1) {
    return;
  }

  ArrayPrototypeSplice(targetListeners[type], index, 1);
  if (listener.abortListener) {
    listener.signal.removeEventListener("abort", listener.abortListener);
    listener.signal = undefined;
    listener.abortListener = undefined;
  }
}

/** Retarget the target following the spec logic.
 *
 * Ref: https://dom.spec.whatwg.org/#retarget */
function retarget(a, b) {
  while (true) {
    if (!isNode(a)) {
      return a;
    }

    const aRoot = a.getRootNode();

    if (aRoot) {
      if (
        !isShadowRoot(aRoot) ||
        (isNode(b) && isShadowInclusiveAncestor(aRoot, b))
      ) {
        return a;
      }

      a = getHost(aRoot);
    }
  }
}

// Accessors for non-public data

// denext: an event target's data (its listener lists), by target. It was an
// own symbol-keyed property of the target, which let any script read, reorder
// and remove the listeners of another. Extension scripts that need it get it
// with `getEventTargetData()`.
const eventTargetData = new SafeWeakMap();
const kResistStopImmediatePropagation = Symbol(
  "kResistStopImmediatePropagation",
);

/** The data of an event target, `undefined` until it has had a listener (or
 * `setEventTargetData()`), and for anything that is not an object. */
function getEventTargetData(target) {
  return eventTargetData.get(target);
}

function setEventTargetData(target) {
  eventTargetData.set(target, getDefaultTargetData());
}

function getAssignedSlot(target) {
  return Boolean(getEventTargetData(target)?.assignedSlot);
}

function getHasActivationBehavior(target) {
  return Boolean(getEventTargetData(target)?.hasActivationBehavior);
}

function getHost(target) {
  return getEventTargetData(target)?.host ?? null;
}

function getListeners(target) {
  return getEventTargetData(target)?.listeners ?? {};
}

function getMode(target) {
  return getEventTargetData(target)?.mode ?? null;
}

function listenerCount(target, type) {
  return getListeners(target)?.[type]?.length ?? 0;
}

function getDefaultTargetData() {
  return {
    assignedSlot: false,
    hasActivationBehavior: false,
    host: null,
    listeners: ObjectCreate(null),
    mode: "",
  };
}

function addEventListenerOptionsConverter(V, prefix) {
  // `signal` is always an own property of the returned object, so that
  // `addEventListener` never picks up a `signal` inherited from a polluted
  // `Object.prototype` when no signal was passed.
  if (webidl.type(V) !== "Object") {
    return { capture: !!V, once: false, passive: false, signal: undefined };
  }

  const options = {
    capture: !!V.capture,
    once: !!V.once,
    passive: !!V.passive,
    signal: undefined,
    // This field exists for simulating Node.js behavior, implemented in https://github.com/nodejs/node/commit/bcd35c334ec75402ee081f1c4da128c339f70c24
    // Some internal event listeners in Node.js can ignore `e.stopImmediatePropagation()` calls
    // from the earlier event listeners.
    [kResistStopImmediatePropagation]: !!V[kResistStopImmediatePropagation],
  };

  const signal = V.signal;
  if (signal !== undefined) {
    options.signal = webidl.converters.AbortSignal(
      signal,
      prefix,
      "'signal' of 'AddEventListenerOptions' (Argument 3)",
    );
  }

  return options;
}

// The steps of `EventTarget.prototype.dispatchEvent()` after the checks of
// its arguments. `self` is the target; `thisArg` is what the method was
// called on (undefined for a bare `dispatchEvent(event)` of the global).
function dispatchAtTarget(self, thisArg, event) {
  // This is an optimization to avoid creating an event listener
  // on each startup.
  // Stores the flag for checking whether unload is dispatched or not.
  // This prevents the recursive dispatches of unload events.
  // See https://github.com/denoland/deno/issues/9201.
  if (event.type === "unload" && self === globalThis_) {
    globalThis_[SymbolFor("Deno.isUnloadDispatched")] = true;
  }

  const data = eventTargetData.get(self);
  if (data === undefined || !data.listeners[event.type]) {
    setTarget(event, thisArg);
    return true;
  }

  if (getDispatched(event)) {
    throw new DOMException("Invalid event state", "InvalidStateError");
  }

  if (event.eventPhase !== Event.NONE) {
    throw new DOMException("Invalid event state", "InvalidStateError");
  }

  return dispatch(self, event);
}

/**
 * denext: how an extension dispatches an event of its own (one it made, and
 * that no script has seen) at a target.
 *
 * In Deno this is `target.dispatchEvent(event)` and nothing else: the method
 * is looked up on the target as before, so a program's override of it (on a
 * subclass, an instance or a prototype) is still called, and `isTrusted` is
 * what the caller set.
 *
 * With `enableUnforgeableEvents()` the dispatch is run from here, without
 * reading any property of the target: a script that replaces `dispatchEvent`
 * is not handed the event. The event is trusted, as every event the user
 * agent fires is; a target of the embedder's node dispatcher is dispatched
 * at by it.
 *
 * @returns {boolean} whether the event was not canceled
 */
function dispatchTrusted(target, event) {
  if (!unforgeableEvents) {
    return target.dispatchEvent(event);
  }
  if (!isEvent(event)) {
    throw new TypeError("dispatchTrusted: parameter 2 is not of type 'Event'.");
  }
  if (getDispatched(event)) {
    throw new DOMException("Invalid event state", "InvalidStateError");
  }
  setIsTrusted(event, true);
  if (nodeDispatcher !== null && nodeDispatcher.isNode(target)) {
    return nodeDispatcher.dispatchNode(target, event);
  }
  return dispatchAtTarget(target, target, event);
}

class EventTarget {
  constructor() {
    // eventTargetData is allocated lazily on first addEventListener (or via
    // setEventTargetData for objects built with webidl.createBranded). Most
    // short-lived EventTarget instances created on the request/response hot
    // path (AbortSignal, transient streams) never have a listener attached
    // and never need the listeners map / 5-field default object.
    this[webidl.brand] = webidl.brand;
  }

  addEventListener(
    type,
    callback,
    options,
  ) {
    const self = this ?? globalThis_;
    webidl.assertBranded(self, EventTargetPrototype);
    const prefix = "Failed to execute 'addEventListener' on 'EventTarget'";

    webidl.requiredArguments(arguments.length, 2, prefix);

    options = addEventListenerOptionsConverter(options, prefix);

    if (callback === null) {
      return;
    }

    let data = eventTargetData.get(self);
    if (data === undefined) {
      data = getDefaultTargetData();
      eventTargetData.set(self, data);
    }
    const { listeners } = data;

    if (!listeners[type]) {
      listeners[type] = [];
    }

    const listenerList = listeners[type];
    for (let i = 0; i < listenerList.length; ++i) {
      const listener = listenerList[i];
      if (
        ((typeof listener.options === "boolean" &&
          listener.options === options.capture) ||
          (typeof listener.options === "object" &&
            listener.options.capture === options.capture)) &&
        listener.callback === callback
      ) {
        return;
      }
    }
    let signal;
    let abortListener;
    if (options?.signal) {
      signal = options.signal;
      if (signal.aborted) {
        // If signal is not null and its aborted flag is set, then return.
        return;
      } else {
        // If listener's signal is not null, then add the following abort
        // abort steps to it: Remove an event listener.
        abortListener = () => {
          self.removeEventListener(type, callback, options);
        };
        signal.addEventListener("abort", abortListener);
      }
    }

    ArrayPrototypePush(listeners[type], {
      callback,
      options,
      signal,
      abortListener,
    });
  }

  removeEventListener(
    type,
    callback,
    options,
  ) {
    const self = this ?? globalThis_;
    webidl.assertBranded(self, EventTargetPrototype);
    webidl.requiredArguments(
      arguments.length,
      2,
      "Failed to execute 'removeEventListener' on 'EventTarget'",
    );

    const data = eventTargetData.get(self);
    if (data === undefined || callback === null || !data.listeners[type]) {
      return;
    }
    const { listeners } = data;

    options = normalizeEventHandlerOptions(options);

    for (let i = 0; i < listeners[type].length; ++i) {
      const listener = listeners[type][i];
      if (
        ((typeof listener.options === "boolean" &&
          listener.options === options.capture) ||
          (typeof listener.options === "object" &&
            listener.options.capture === options.capture)) &&
        listener.callback === callback
      ) {
        removeListener(listeners, type, listener, i);
        break;
      }
    }
  }

  dispatchEvent(event) {
    // If `this` is not present, then fallback to global scope. We don't use
    // `globalThis` directly here, because it could be deleted by user.
    // Instead use saved reference to global scope when the script was
    // executed.
    const self = this ?? globalThis_;
    webidl.assertBranded(self, EventTargetPrototype);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to execute 'dispatchEvent' on 'EventTarget'",
    );
    // denext: an event's slots are private fields, so what is not an Event
    // is refused here, by name, instead of failing on a missing slot below.
    if (!isEvent(event)) {
      throw new TypeError(
        "Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.",
      );
    }
    if (unforgeableEvents) {
      prepareScriptDispatch(event);
    }
    return dispatchAtTarget(self, this, event);
  }

  getParent(_event) {
    return null;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return `${this.constructor.name} ${inspect({}, inspectOptions)}`;
  }
}

webidl.configureInterface(EventTarget);
const EventTargetPrototype = EventTarget.prototype;

defineEnumerableProps(EventTarget, [
  "addEventListener",
  "removeEventListener",
  "dispatchEvent",
]);

class ErrorEvent extends Event {
  #message = "";
  #filename = "";
  #lineno = "";
  #colno = "";
  #error = "";

  get message() {
    return this.#message;
  }
  get filename() {
    return this.#filename;
  }
  get lineno() {
    return this.#lineno;
  }
  get colno() {
    return this.#colno;
  }
  get error() {
    return this.#error;
  }

  constructor(
    type,
    {
      bubbles,
      cancelable,
      composed,
      message = "",
      filename = "",
      lineno = 0,
      colno = 0,
      error,
    } = EMPTY_EVENT_INIT,
  ) {
    super(type, {
      bubbles: bubbles,
      cancelable: cancelable,
      composed: composed,
    });

    this.#message = message;
    this.#filename = filename;
    this.#lineno = lineno;
    this.#colno = colno;
    this.#error = error;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "message",
          "filename",
          "lineno",
          "colno",
          "error",
        ],
      }),
      inspectOptions,
    );
  }
}

const ErrorEventPrototype = ErrorEvent.prototype;
ObjectDefineProperty(ErrorEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "ErrorEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

defineEnumerableProps(ErrorEvent, [
  "message",
  "filename",
  "lineno",
  "colno",
  "error",
]);

class CloseEvent extends Event {
  #wasClean = "";
  #code = "";
  #reason = "";

  get wasClean() {
    return this.#wasClean;
  }
  get code() {
    return this.#code;
  }
  get reason() {
    return this.#reason;
  }

  constructor(type, {
    bubbles,
    cancelable,
    composed,
    wasClean = false,
    code = 0,
    reason = "",
  } = EMPTY_EVENT_INIT) {
    super(type, {
      bubbles: bubbles,
      cancelable: cancelable,
      composed: composed,
    });

    this.#wasClean = wasClean;
    this.#code = code;
    this.#reason = reason;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "wasClean",
          "code",
          "reason",
        ],
      }),
      inspectOptions,
    );
  }
}

const CloseEventPrototype = CloseEvent.prototype;
ObjectDefineProperty(CloseEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "CloseEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

// Lazy reference to MessagePort.prototype - 13_message_port.js depends on
// this module, so the script can't be loaded at definition time without
// causing a load cycle. Cached on first access.
let _MessagePortPrototype;
function getMessagePortPrototype() {
  if (_MessagePortPrototype === undefined) {
    _MessagePortPrototype =
      core.loadExtScript("ext:deno_web/13_message_port.js")
        .MessagePortPrototype;
  }
  return _MessagePortPrototype;
}

// Format an arbitrary JS value for inclusion in a MessageEvent constructor
// error message *for the "must be MessagePort" check*: strings and primitives
// get JSON-quoted (`"null"`, `"1"`, `"{}"`), matching the regex assertions
// Node's tests use for source/ports[i].
function formatForBranded(value) {
  if (value === null) return '"null"';
  if (value === undefined) return '"undefined"';
  const t = typeof value;
  if (t === "string") return JSONStringify(value);
  if (t === "object" || t === "function") return '"{}"';
  return JSONStringify(String(value));
}

// Format for the iterable-check error: the raw value's string representation,
// unquoted. Node's test expects `eventInitDict.ports (0) is not iterable.`
function formatForRaw(value) {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  const t = typeof value;
  if (t === "string") return JSONStringify(value);
  if (t === "object" || t === "function") return "{}";
  return String(value);
}

class MessageEvent extends Event {
  #source = null;

  get source() {
    return this.#source;
  }

  constructor(type, eventInitDict) {
    super(type, {
      bubbles: eventInitDict?.bubbles ?? false,
      cancelable: eventInitDict?.cancelable ?? false,
      composed: eventInitDict?.composed ?? false,
    });

    this.data = eventInitDict?.data ?? null;
    const ports = eventInitDict?.ports;
    if (ports == null) {
      // `ports` is a FrozenArray<MessagePort> per the HTML spec, so the
      // exposed array must be read-only.
      this.ports = ObjectFreeze([]);
    } else {
      // MessageEvent ports: iterable validation + per-element MessagePort
      // type check. Matches the messages Node asserts on so the
      // node_compat tests pass while still being WHATWG-shaped.
      if (
        ports === null || typeof ports !== "object" ||
        ports[SymbolIterator] === undefined
      ) {
        throw new TypeError(
          `MessageEvent constructor: eventInitDict.ports (${
            formatForRaw(ports)
          }) is not iterable.`,
        );
      }
      const MessagePortProto = getMessagePortPrototype();
      const arr = [];
      let i = 0;
      // Iterate using the user's own iterator so values that aren't real
      // arrays (e.g. a `RegExp` with a custom `Symbol.iterator` -- covered
      // by WPT's no-regexp-special-casing test) still produce the
      // expected `ports` array. SafeArrayIterator can't be used here
      // because it walks the value as if it were an Array.
      // deno-lint-ignore deno-internal/prefer-primordials
      for (const p of ports) {
        if (
          p === null || typeof p !== "object" ||
          !ObjectPrototypeIsPrototypeOf(MessagePortProto, p)
        ) {
          throw new TypeError(
            `MessageEvent constructor: Expected eventInitDict.ports[${i}] (${
              formatForBranded(p)
            }) to be an instance of MessagePort.`,
          );
        }
        arr[i++] = p;
      }
      // `ports` is a FrozenArray<MessagePort> per the HTML spec.
      this.ports = ObjectFreeze(arr);
    }
    // origin and lastEventId are USVString per spec, so coerce to string
    // (Node's test passes numbers and expects string coercion).
    this.origin = eventInitDict?.origin === undefined
      ? ""
      : String(eventInitDict.origin);
    this.lastEventId = eventInitDict?.lastEventId === undefined
      ? ""
      : String(eventInitDict.lastEventId);
    const source = eventInitDict?.source;
    if (source != null) {
      // The Web spec types source as `MessageEventSource = MessagePort |
      // ServiceWorker | WindowProxy`. We don't have a JS-level Window or
      // ServiceWorker brand to gate against, so accept anything except
      // primitives (which the node_compat tests expect to throw on,
      // e.g. `source: 1`) and plain `Object` instances like `{}` (also
      // checked by Node). MessagePort, Window-like globals, and
      // user-defined classes all pass through unchanged -- matching both
      // the WPT `messageevent-constructor.https.html` test (which
      // assigns `window` to `source`) and node_compat
      // `test-worker-message-event`.
      const t = typeof source;
      const isPrimitive = t !== "object" && t !== "function";
      // Treat values whose prototype is exactly Object.prototype (i.e.
      // plain object literals like `{}`) as invalid sources. Using the
      // prototype chain rather than `.constructor` avoids the
      // prefer-primordials lint and is more reliable since user code
      // can override `constructor`.
      const isPlainObject = !isPrimitive &&
        ObjectGetPrototypeOf(source) === ObjectPrototype;
      if (isPrimitive || isPlainObject) {
        throw new TypeError(
          `MessageEvent constructor: Expected eventInitDict.source (${
            formatForBranded(source)
          }) to be an instance of MessagePort.`,
        );
      }
      this.#source = source;
    } else {
      this.#source = null;
    }
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "data",
          "origin",
          "lastEventId",
        ],
      }),
      inspectOptions,
    );
  }
}

const MessageEventPrototype = MessageEvent.prototype;
ObjectDefineProperty(MessageEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "MessageEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

class CustomEvent extends Event {
  #detail = null;

  constructor(type, eventInitDict = EMPTY_EVENT_INIT) {
    super(type, eventInitDict);
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'CustomEvent'",
    );
    const { detail } = eventInitDict;
    this.#detail = detail;
  }

  get detail() {
    return this.#detail;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "detail",
        ],
      }),
      inspectOptions,
    );
  }
}

const CustomEventPrototype = CustomEvent.prototype;
ObjectDefineProperty(CustomEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "CustomEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

ReflectDefineProperty(CustomEvent.prototype, "detail", {
  __proto__: null,
  enumerable: true,
});

// ProgressEvent could also be used in other DOM progress event emits.
// Current use is for FileReader.
class ProgressEvent extends Event {
  constructor(type, eventInitDict = EMPTY_EVENT_INIT) {
    super(type, eventInitDict);

    this.lengthComputable = eventInitDict?.lengthComputable ?? false;
    this.loaded = eventInitDict?.loaded ?? 0;
    this.total = eventInitDict?.total ?? 0;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: isEvent(this),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "lengthComputable",
          "loaded",
          "total",
        ],
      }),
      inspectOptions,
    );
  }
}

const ProgressEventPrototype = ProgressEvent.prototype;
ObjectDefineProperty(ProgressEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "ProgressEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

class PromiseRejectionEvent extends Event {
  #promise = null;
  #reason = null;

  get promise() {
    return this.#promise;
  }
  get reason() {
    return this.#reason;
  }

  constructor(
    type,
    {
      bubbles,
      cancelable,
      composed,
      promise,
      reason,
    } = EMPTY_EVENT_INIT,
  ) {
    super(type, {
      bubbles: bubbles,
      cancelable: cancelable,
      composed: composed,
    });

    this.#promise = promise;
    this.#reason = reason;
  }

  [SymbolFor("Deno.privateCustomInspect")](inspect, inspectOptions) {
    return inspect(
      getCreateFilteredInspectProxy()({
        object: this,
        evaluate: ObjectPrototypeIsPrototypeOf(
          PromiseRejectionEventPrototype,
          this,
        ),
        keys: [
          ...new SafeArrayIterator(EVENT_PROPS),
          "promise",
          "reason",
        ],
      }),
      inspectOptions,
    );
  }
}

const PromiseRejectionEventPrototype = PromiseRejectionEvent.prototype;
ObjectDefineProperty(PromiseRejectionEventPrototype, SymbolToStringTag, {
  __proto__: null,
  value: "PromiseRejectionEvent",
  writable: false,
  enumerable: false,
  configurable: true,
});

defineEnumerableProps(PromiseRejectionEvent, [
  "promise",
  "reason",
]);

// denext: the event handlers (`on*`) of an event target, by target: a map of
// event name to wrapped handler. It was an own symbol-keyed property.
const eventHandlers = new SafeWeakMap();

function makeWrappedHandler(handler, isSpecialErrorEventHandler) {
  function wrappedHandler(evt) {
    if (typeof wrappedHandler.handler !== "function") {
      return;
    }

    if (
      isSpecialErrorEventHandler &&
      ObjectPrototypeIsPrototypeOf(ErrorEventPrototype, evt) &&
      evt.type === "error"
    ) {
      const ret = FunctionPrototypeCall(
        wrappedHandler.handler,
        this,
        evt.message,
        evt.filename,
        evt.lineno,
        evt.colno,
        evt.error,
      );
      if (ret === true) {
        evt.preventDefault();
      }
      return;
    }

    return FunctionPrototypeCall(wrappedHandler.handler, this, evt);
  }
  wrappedHandler.handler = handler;
  return wrappedHandler;
}

// `init` is an optional function that will be called whenever the event
// handler property is set to a handler while no handler is active, including
// after it was cleared with `null`. It will be called with the object on which
// the property is set as its argument.
// `isSpecialErrorEventHandler` can be set to true to opt into the special
// behavior of event handlers for the "error" event in a global scope.
function defineEventHandler(
  emitter,
  name,
  init = undefined,
  isSpecialErrorEventHandler = false,
) {
  // HTML specification section 8.1.7.1
  ObjectDefineProperty(emitter, `on${name}`, {
    __proto__: null,
    get() {
      const handlers = eventHandlers.get(this);
      if (handlers === undefined) {
        return null;
      }

      return MapPrototypeGet(handlers, name)?.handler ?? null;
    },
    set(value) {
      // All three Web IDL event handler types are nullable callback functions
      // with the [LegacyTreatNonObjectAsNull] extended attribute, meaning
      // anything other than an object is treated as null.
      if (typeof value !== "object" && typeof value !== "function") {
        value = null;
      }

      let handlers = eventHandlers.get(this);
      if (handlers === undefined) {
        handlers = new SafeMap();
        eventHandlers.set(this, handlers);
      }
      let handlerWrapper = MapPrototypeGet(handlers, name);
      if (value === null) {
        // Deactivate the event handler by removing its listener, so that
        // setting it again appends a new listener at the end of the list.
        if (handlerWrapper) {
          MapPrototypeDelete(handlers, name);
          this.removeEventListener(name, handlerWrapper);
        }
      } else if (handlerWrapper) {
        handlerWrapper.handler = value;
      } else {
        handlerWrapper = makeWrappedHandler(
          value,
          isSpecialErrorEventHandler,
        );
        MapPrototypeSet(handlers, name, handlerWrapper);
        this.addEventListener(name, handlerWrapper);
        init?.(this);
      }
    },
    configurable: true,
    enumerable: true,
  });
}

let reportExceptionStackedCalls = 0;

// https://html.spec.whatwg.org/#report-the-exception
function reportException(error) {
  reportExceptionStackedCalls++;
  const jsError = core.destructureError(error);
  const message = jsError.exceptionMessage;
  let filename = "";
  let lineno = 0;
  let colno = 0;
  if (jsError.frames.length > 0) {
    filename = jsError.frames[0].fileName;
    lineno = jsError.frames[0].lineNumber;
    colno = jsError.frames[0].columnNumber;
  } else {
    const jsError = core.destructureError(new Error());
    const frames = jsError.frames;
    for (let i = 0; i < frames.length; ++i) {
      const frame = frames[i];
      if (
        typeof frame.fileName == "string" &&
        !StringPrototypeStartsWith(frame.fileName, "ext:")
      ) {
        filename = frame.fileName;
        lineno = frame.lineNumber;
        colno = frame.columnNumber;
        break;
      }
    }
  }
  const event = new ErrorEvent("error", {
    cancelable: true,
    message,
    filename,
    lineno,
    colno,
    error,
  });
  // Avoid recursing `reportException()` via error handlers more than once.
  if (reportExceptionStackedCalls > 1 || dispatchTrusted(globalThis_, event)) {
    core.reportUnhandledException(error);
  }
  reportExceptionStackedCalls--;
}

function checkThis(thisArg) {
  if (thisArg !== null && thisArg !== undefined && thisArg !== globalThis_) {
    throw new TypeError("Illegal invocation");
  }
}

// https://html.spec.whatwg.org/#dom-reporterror
function reportError(error) {
  checkThis(this);
  const prefix = "Failed to execute 'reportError'";
  webidl.requiredArguments(arguments.length, 1, prefix);
  reportException(error);
}

internals.defineEventHandler = defineEventHandler;
internals.setEventTargetData = setEventTargetData;

return {
  CloseEvent,
  CustomEvent,
  defineEventHandler,
  dispatch,
  dispatchTrusted,
  enableUnforgeableEvents,
  ErrorEvent,
  Event,
  eventInternals,
  EventTarget,
  EventTargetPrototype,
  getEventTargetData,
  kResistStopImmediatePropagation,
  listenerCount,
  MessageEvent,
  ProgressEvent,
  PromiseRejectionEvent,
  reportError,
  reportException,
  saveGlobalThisReference,
  setEventTargetData,
  setIsTrusted,
  setNodeDispatcher,
  setTarget,
  unforgeableEventsEnabled,
};
})();
