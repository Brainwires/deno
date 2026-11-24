// Copyright 2018-2026 the Deno authors. MIT license.

// The UI event interfaces `deno_web` does not have: UIEvent, MouseEvent,
// PointerEvent, KeyboardEvent, InputEvent, FocusEvent and SubmitEvent, over
// `deno_web`'s Event. Enough for scripts to construct them and for the host
// to fire them (`click`, `set_value`); the full member lists come with the
// WebIDL generator.
//
// Event propagation along the tree is in `00_dom.js`.

import { core, primordials } from "ext:core/mod.js";
const {
  ObjectFreeze,
  String,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { Event } = core.loadExtScript("ext:deno_web/02_event.js");

const EMPTY_INIT = ObjectFreeze({ __proto__: null });

function initOf(init) {
  return init === undefined || init === null ? EMPTY_INIT : init;
}

function numberOf(value) {
  return value === undefined ? 0 : (+value || 0);
}

function stringOf(value, fallback = "") {
  return value === undefined ? fallback : String(value);
}

function objectOrNull(value) {
  return value === undefined ? null : value;
}

class UIEvent extends Event {
  #view;
  #detail;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'UIEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#view = objectOrNull(init.view);
    this.#detail = numberOf(init.detail);
  }

  get view() {
    return this.#view;
  }

  get detail() {
    return this.#detail;
  }

  get which() {
    return 0;
  }
}

class MouseEvent extends UIEvent {
  #screenX;
  #screenY;
  #clientX;
  #clientY;
  #ctrlKey;
  #shiftKey;
  #altKey;
  #metaKey;
  #button;
  #buttons;
  #relatedTarget;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'MouseEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#screenX = numberOf(init.screenX);
    this.#screenY = numberOf(init.screenY);
    this.#clientX = numberOf(init.clientX);
    this.#clientY = numberOf(init.clientY);
    this.#ctrlKey = !!init.ctrlKey;
    this.#shiftKey = !!init.shiftKey;
    this.#altKey = !!init.altKey;
    this.#metaKey = !!init.metaKey;
    this.#button = numberOf(init.button);
    this.#buttons = numberOf(init.buttons);
    this.#relatedTarget = objectOrNull(init.relatedTarget);
  }

  get screenX() {
    return this.#screenX;
  }

  get screenY() {
    return this.#screenY;
  }

  get clientX() {
    return this.#clientX;
  }

  get clientY() {
    return this.#clientY;
  }

  get pageX() {
    return this.#clientX;
  }

  get pageY() {
    return this.#clientY;
  }

  get x() {
    return this.#clientX;
  }

  get y() {
    return this.#clientY;
  }

  get offsetX() {
    return 0;
  }

  get offsetY() {
    return 0;
  }

  get movementX() {
    return 0;
  }

  get movementY() {
    return 0;
  }

  get ctrlKey() {
    return this.#ctrlKey;
  }

  get shiftKey() {
    return this.#shiftKey;
  }

  get altKey() {
    return this.#altKey;
  }

  get metaKey() {
    return this.#metaKey;
  }

  get button() {
    return this.#button;
  }

  get buttons() {
    return this.#buttons;
  }

  get relatedTarget() {
    return this.#relatedTarget;
  }

  getModifierState(key) {
    switch (String(key)) {
      case "Control":
        return this.#ctrlKey;
      case "Shift":
        return this.#shiftKey;
      case "Alt":
        return this.#altKey;
      case "Meta":
        return this.#metaKey;
      default:
        return false;
    }
  }
}

class PointerEvent extends MouseEvent {
  #pointerId;
  #pointerType;
  #isPrimary;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'PointerEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#pointerId = numberOf(init.pointerId);
    this.#pointerType = stringOf(init.pointerType);
    this.#isPrimary = !!init.isPrimary;
  }

  get pointerId() {
    return this.#pointerId;
  }

  get width() {
    return 1;
  }

  get height() {
    return 1;
  }

  get pressure() {
    return 0;
  }

  get pointerType() {
    return this.#pointerType;
  }

  get isPrimary() {
    return this.#isPrimary;
  }
}

class KeyboardEvent extends UIEvent {
  #key;
  #code;
  #location;
  #ctrlKey;
  #shiftKey;
  #altKey;
  #metaKey;
  #repeat;
  #isComposing;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'KeyboardEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#key = stringOf(init.key);
    this.#code = stringOf(init.code);
    this.#location = numberOf(init.location);
    this.#ctrlKey = !!init.ctrlKey;
    this.#shiftKey = !!init.shiftKey;
    this.#altKey = !!init.altKey;
    this.#metaKey = !!init.metaKey;
    this.#repeat = !!init.repeat;
    this.#isComposing = !!init.isComposing;
  }

  get key() {
    return this.#key;
  }

  get code() {
    return this.#code;
  }

  get location() {
    return this.#location;
  }

  get ctrlKey() {
    return this.#ctrlKey;
  }

  get shiftKey() {
    return this.#shiftKey;
  }

  get altKey() {
    return this.#altKey;
  }

  get metaKey() {
    return this.#metaKey;
  }

  get repeat() {
    return this.#repeat;
  }

  get isComposing() {
    return this.#isComposing;
  }

  get keyCode() {
    return 0;
  }

  get charCode() {
    return 0;
  }

  getModifierState(key) {
    switch (String(key)) {
      case "Control":
        return this.#ctrlKey;
      case "Shift":
        return this.#shiftKey;
      case "Alt":
        return this.#altKey;
      case "Meta":
        return this.#metaKey;
      default:
        return false;
    }
  }
}

class InputEvent extends UIEvent {
  #data;
  #inputType;
  #isComposing;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'InputEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    const data = init.data;
    this.#data = data === undefined || data === null ? null : String(data);
    this.#inputType = stringOf(init.inputType);
    this.#isComposing = !!init.isComposing;
  }

  get data() {
    return this.#data;
  }

  get inputType() {
    return this.#inputType;
  }

  get isComposing() {
    return this.#isComposing;
  }
}

class FocusEvent extends UIEvent {
  #relatedTarget;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'FocusEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#relatedTarget = objectOrNull(init.relatedTarget);
  }

  get relatedTarget() {
    return this.#relatedTarget;
  }
}

class SubmitEvent extends Event {
  #submitter;

  constructor(type, eventInitDict = undefined) {
    webidl.requiredArguments(
      arguments.length,
      1,
      "Failed to construct 'SubmitEvent'",
    );
    const init = initOf(eventInitDict);
    super(type, init);
    this.#submitter = objectOrNull(init.submitter);
  }

  get submitter() {
    return this.#submitter;
  }
}

const interfaces = [
  UIEvent,
  MouseEvent,
  PointerEvent,
  KeyboardEvent,
  InputEvent,
  FocusEvent,
  SubmitEvent,
];
for (let i = 0; i < interfaces.length; i++) {
  webidl.configureInterface(interfaces[i]);
}

export {
  FocusEvent,
  InputEvent,
  KeyboardEvent,
  MouseEvent,
  PointerEvent,
  SubmitEvent,
  UIEvent,
};
