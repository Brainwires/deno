// Copyright 2018-2026 the Deno authors. MIT license.

// What a page sees of `deno_web`'s Event and EventTarget, made to be what a
// browser shows. Runs once, with the bootstrap, before any page script:
//
// - Trust. `02_event.js` keeps an event's slots in private fields and the
//   listener lists in a weak map, so a page reaches neither. Here it is told
//   that scripts are hostile: `isTrusted` becomes an own, non-configurable
//   accessor of every event ([LegacyUnforgeable]), and `dispatchEvent()`
//   takes the trust of an event that was dispatched before.
// - Shape. The members `deno_web` has for Deno and a browser does not have
//   are removed (`Deno.inspect`'s hook, `EventTarget.prototype.getParent`,
//   `Event.prototype.initialized`, the `isTrusted` of the prototype), the
//   members are enumerable and the phase constants are values, as Web IDL
//   defines them. Nothing of the extensions reads what is removed: the
//   dispatch uses functions of its module, never the methods.
//
// A side effect for the host: the page's console (`01_console.js`) formats an
// event as a plain object, without the list of attributes `Deno.inspect`
// shows.

import { core, primordials } from "ext:core/mod.js";
const {
  ObjectDefineProperty,
  ReflectDeleteProperty,
  ReflectGetOwnPropertyDescriptor,
  ReflectHas,
  ReflectOwnKeys,
  SymbolFor,
  TypeError,
} = primordials;

const event = core.loadExtScript("ext:deno_web/02_event.js");

event.enableUnforgeableEvents();

const privateCustomInspect = SymbolFor("Deno.privateCustomInspect");

function remove(prototype, key) {
  if (!ReflectDeleteProperty(prototype, key) || ReflectHas(prototype, key)) {
    throw new TypeError(
      "deno_web_page: a member of the event interfaces could not be removed",
    );
  }
}

/** Every member but `constructor` is enumerable, as Web IDL has it. */
function makeMembersEnumerable(prototype) {
  const keys = ReflectOwnKeys(prototype);
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    if (typeof key !== "string" || key === "constructor") {
      continue;
    }
    const descriptor = ReflectGetOwnPropertyDescriptor(prototype, key);
    if (!descriptor.enumerable && descriptor.configurable) {
      ObjectDefineProperty(prototype, key, {
        __proto__: null,
        enumerable: true,
      });
    }
  }
}

const INTERFACES = [
  event.EventTarget,
  event.Event,
  event.CustomEvent,
  event.ErrorEvent,
  event.CloseEvent,
  event.MessageEvent,
  event.ProgressEvent,
  event.PromiseRejectionEvent,
];
for (let i = 0; i < INTERFACES.length; i++) {
  const prototype = INTERFACES[i].prototype;
  remove(prototype, privateCustomInspect);
  makeMembersEnumerable(prototype);
}

remove(event.EventTarget.prototype, "getParent");

const EventPrototype = event.Event.prototype;
// Every event has the accessor itself from here on.
remove(EventPrototype, "isTrusted");
remove(EventPrototype, "initialized");

// `const unsigned short NONE = 0;` and the others: `deno_web` has getters.
const PHASES = ["NONE", "CAPTURING_PHASE", "AT_TARGET", "BUBBLING_PHASE"];
for (let i = 0; i < PHASES.length; i++) {
  ObjectDefineProperty(EventPrototype, PHASES[i], {
    __proto__: null,
    value: i,
    writable: false,
    enumerable: true,
    configurable: false,
  });
}
