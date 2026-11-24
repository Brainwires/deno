// Copyright 2018-2026 the Deno authors. MIT license.

// The HTML element interfaces of a page context, the state of form controls
// (which lives on the wrappers: the DOM crate holds content attributes
// only), the click activation behavior, form submission and focus.
//
// Minimal by design: what the host's tools need (click, type, read) and
// what ordinary page scripts touch first. No layout, so nothing here has a
// size or a position; `style` is a view of the `style` attribute and
// nothing more.

import { core, primordials } from "ext:core/mod.js";
import { op_web_page_navigate } from "ext:core/ops";
import {
  attributeOf,
  closestOf,
  containsOf,
  createDataset,
  dispatchNodeEvent,
  DOC,
  Document,
  documentNode,
  Element,
  elementById,
  fireEvent,
  getActiveElement,
  getDocumentUrl,
  getWindow,
  hasAttributeOf,
  htmlCollection,
  htmlLocalName,
  isConnectedOf,
  isElement,
  kConstruct,
  nodeFromId,
  parseDeclarations,
  queryAll,
  queryFirst,
  registerEventHandlerName,
  registerHtmlInterface,
  REL,
  relative,
  removeAttributeOf,
  setActiveElement,
  setAttributeOf,
  setClickActivation,
  setCloneState,
  setDefaultHtmlInterface,
  setTextOf,
  stripAsciiWhitespace,
  textOf,
  tokenList,
  toStr,
} from "ext:deno_web_page/00_dom.js";
import {
  FocusEvent,
  InputEvent,
  MouseEvent,
  PointerEvent,
  SubmitEvent,
} from "ext:deno_web_page/01_events.js";
const {
  ArrayPrototypeIncludes,
  ArrayPrototypeIndexOf,
  ArrayPrototypeJoin,
  ArrayPrototypePush,
  NumberIsNaN,
  NumberParseInt,
  ObjectDefineProperty,
  ObjectHasOwn,
  ObjectPrototypeIsPrototypeOf,
  Proxy,
  ReflectHas,
  SafeRegExp,
  SafeSet,
  String,
  StringPrototypeReplace,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  TypeError,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript("ext:deno_web/01_dom_exception.js");
const event = core.loadExtScript("ext:deno_web/02_event.js");
const { Event } = event;
const { URL } = core.loadExtScript("ext:deno_web/00_url.js");
const MouseEventPrototype = MouseEvent.prototype;

function illegalInvocation() {
  return new TypeError("Illegal invocation");
}

/** A URL resolved against the document's, or null if it does not parse. */
function resolveUrl(value) {
  try {
    return new URL(value, getDocumentUrl()).href;
  } catch {
    return null;
  }
}

const WHITESPACE_RUN = new SafeRegExp("[\\t\\n\\f\\r ]+", "g");

function collapseWhitespace(value) {
  return stripAsciiWhitespace(
    StringPrototypeReplace(value, WHITESPACE_RUN, " "),
  );
}

// --- style ----------------------------------------------------------------------

const UPPERCASE = new SafeRegExp("[A-Z]", "g");

/** `backgroundColor` -> `background-color`, `cssFloat` -> `float`. */
function cssPropertyName(property) {
  if (property === "cssFloat") {
    return "float";
  }
  let name = StringPrototypeReplace(
    property,
    UPPERCASE,
    (letter) => "-" + StringPrototypeToLowerCase(letter),
  );
  if (StringPrototypeStartsWith(name, "webkit-")) {
    name = "-" + name;
  }
  return name;
}

class CSSStyleDeclaration {
  constructor(key = undefined) {
    if (key !== kConstruct) {
      throw new TypeError("Illegal constructor");
    }
  }
}
webidl.configureInterface(CSSStyleDeclaration);

/**
 * `HTMLElement.style`: the declarations of the `style` attribute, read and
 * written through on every use. Values are not parsed or validated.
 */
function createStyle(element) {
  const read = () => parseDeclarations(attributeOf(element, "style") ?? "");
  const write = (declarations) => {
    const parts = [];
    for (let i = 0; i < declarations.length; i++) {
      ArrayPrototypePush(
        parts,
        declarations[i][0] + ": " + declarations[i][1] + ";",
      );
    }
    setAttributeOf(element, "style", ArrayPrototypeJoin(parts, " "));
  };
  const valueOf = (name) => {
    const declarations = read();
    for (let i = declarations.length - 1; i >= 0; i--) {
      if (declarations[i][0] === name) {
        return declarations[i][1];
      }
    }
    return "";
  };
  const assign = (name, value) => {
    const declarations = read();
    const kept = [];
    for (let i = 0; i < declarations.length; i++) {
      if (declarations[i][0] !== name) {
        ArrayPrototypePush(kept, declarations[i]);
      }
    }
    if (value !== "") {
      ArrayPrototypePush(kept, [name, value]);
    }
    write(kept);
  };
  const methods = {
    __proto__: null,
    getPropertyValue(property) {
      return valueOf(StringPrototypeToLowerCase(toStr(property)));
    },
    getPropertyPriority(_property) {
      return "";
    },
    setProperty(property, value, _priority = undefined) {
      assign(
        StringPrototypeToLowerCase(toStr(property)),
        value === null ? "" : stripAsciiWhitespace(toStr(value)),
      );
    },
    removeProperty(property) {
      const name = StringPrototypeToLowerCase(toStr(property));
      const previous = valueOf(name);
      assign(name, "");
      return previous;
    },
    item(index) {
      return read()[index >>> 0]?.[0] ?? "";
    },
  };
  return new Proxy(new CSSStyleDeclaration(kConstruct), {
    __proto__: null,
    get(target, property) {
      if (typeof property !== "string") {
        return target[property];
      }
      if (property === "cssText") {
        return attributeOf(element, "style") ?? "";
      }
      if (property === "length") {
        return read().length;
      }
      if (ObjectHasOwn(methods, property)) {
        return methods[property];
      }
      if (ReflectHas(target, property)) {
        return target[property];
      }
      return valueOf(cssPropertyName(property));
    },
    set(_target, property, value) {
      if (typeof property !== "string") {
        return false;
      }
      value = value === null ? "" : stripAsciiWhitespace(toStr(value));
      if (property === "cssText") {
        setAttributeOf(element, "style", value);
      } else {
        assign(cssPropertyName(property), value);
      }
      return true;
    },
  });
}

// --- reflection helpers -----------------------------------------------------------

let isHtmlElement;

function checkHtml(value) {
  if (!isHtmlElement(value)) {
    throw illegalInvocation();
  }
}

function defineAttribute(interface_, property, get, set) {
  ObjectDefineProperty(interface_.prototype, property, {
    __proto__: null,
    get,
    set,
    enumerable: true,
    configurable: true,
  });
}

function reflectString(
  interface_,
  property,
  attribute = StringPrototypeToLowerCase(property),
) {
  defineAttribute(interface_, property, function () {
    checkHtml(this);
    return attributeOf(this, attribute) ?? "";
  }, function (value) {
    checkHtml(this);
    setAttributeOf(this, attribute, toStr(value));
  });
}

function reflectBoolean(
  interface_,
  property,
  attribute = StringPrototypeToLowerCase(property),
) {
  defineAttribute(interface_, property, function () {
    checkHtml(this);
    return hasAttributeOf(this, attribute);
  }, function (value) {
    checkHtml(this);
    if (value) {
      setAttributeOf(this, attribute, "");
    } else {
      removeAttributeOf(this, attribute);
    }
  });
}

function reflectUrl(
  interface_,
  property,
  attribute = StringPrototypeToLowerCase(property),
) {
  defineAttribute(interface_, property, function () {
    checkHtml(this);
    const value = attributeOf(this, attribute);
    if (value === null) {
      return "";
    }
    return resolveUrl(value) ?? value;
  }, function (value) {
    checkHtml(this);
    setAttributeOf(this, attribute, toStr(value));
  });
}

function reflectForm(interface_) {
  defineAttribute(interface_, "form", function () {
    checkHtml(this);
    return formOwner(this);
  }, undefined);
}

// --- forms: owners, disabledness, state --------------------------------------------

const INPUT_TYPES = new SafeSet([
  "hidden",
  "text",
  "search",
  "tel",
  "url",
  "email",
  "password",
  "date",
  "month",
  "week",
  "time",
  "datetime-local",
  "number",
  "range",
  "color",
  "checkbox",
  "radio",
  "file",
  "submit",
  "image",
  "reset",
  "button",
]);

function inputTypeOf(input) {
  const type = attributeOf(input, "type");
  if (type === null) {
    return "text";
  }
  const lowered = StringPrototypeToLowerCase(type);
  return INPUT_TYPES.has(lowered) ? lowered : "text";
}

/** How `value` behaves for an input type: the HTML Standard's value modes. */
function inputValueMode(type) {
  switch (type) {
    case "checkbox":
    case "radio":
      return "default/on";
    case "hidden":
    case "submit":
    case "image":
    case "reset":
    case "button":
      return "default";
    case "file":
      return "filename";
    default:
      return "value";
  }
}

function buttonTypeOf(button) {
  const type = StringPrototypeToLowerCase(attributeOf(button, "type") ?? "");
  return type === "reset" || type === "button" ? type : "submit";
}

function formOwner(control) {
  const id = attributeOf(control, "form");
  if (id !== null) {
    const form = elementById(id);
    return form !== null && htmlLocalName(form) === "form" ? form : null;
  }
  const parent = relative(control, REL.PARENT_ELEMENT);
  return parent === null ? null : closestOf(parent, "form");
}

const DISABLEABLE = new SafeSet([
  "button",
  "input",
  "select",
  "textarea",
  "fieldset",
]);

function isDisabled(element) {
  const name = htmlLocalName(element);
  if (name === "option") {
    if (hasAttributeOf(element, "disabled")) {
      return true;
    }
    const parent = relative(element, REL.PARENT_ELEMENT);
    return parent !== null && htmlLocalName(parent) === "optgroup" &&
      hasAttributeOf(parent, "disabled");
  }
  if (!DISABLEABLE.has(name)) {
    return false;
  }
  if (hasAttributeOf(element, "disabled")) {
    return true;
  }
  const parent = relative(element, REL.PARENT_ELEMENT);
  return parent !== null && closestOf(parent, "fieldset[disabled]") !== null;
}

/** The listed elements whose form owner is `form`, in tree order. */
function formControls(form) {
  const scope = isConnectedOf(form) ? documentNode(DOC.DOCUMENT) : form;
  const candidates = queryAll(
    scope,
    "button, fieldset, input, object, output, select, textarea",
  );
  const controls = [];
  for (let i = 0; i < candidates.length; i++) {
    if (formOwner(candidates[i]) === form) {
      ArrayPrototypePush(controls, candidates[i]);
    }
  }
  return controls;
}

// Accessors for the private state of form controls. Assigned in the static
// blocks of their classes.
let inputState;
let textareaState;
let optionState;

function selectOptions(select) {
  return queryAll(select, "option");
}

/** The options of a select that count as selected. */
function selectedOptionsOf(select) {
  const options = selectOptions(select);
  const chosen = [];
  for (let i = 0; i < options.length; i++) {
    if (optionState.selected(options[i])) {
      ArrayPrototypePush(chosen, options[i]);
    }
  }
  if (hasAttributeOf(select, "multiple")) {
    return chosen;
  }
  if (chosen.length > 0) {
    return [chosen[chosen.length - 1]];
  }
  // A single-choice select shows its first option that is not disabled.
  for (let i = 0; i < options.length; i++) {
    if (!isDisabled(options[i])) {
      return [options[i]];
    }
  }
  return [];
}

function optionText(option) {
  return collapseWhitespace(textOf(option));
}

function optionValue(option) {
  return attributeOf(option, "value") ?? optionText(option);
}

function selectValue(select) {
  const selected = selectedOptionsOf(select);
  return selected.length > 0 ? optionValue(selected[0]) : "";
}

/** Selects exactly `chosen` (or nothing, for null) in a select. */
function selectOnly(select, chosen) {
  const options = selectOptions(select);
  for (let i = 0; i < options.length; i++) {
    optionState.setSelected(options[i], options[i] === chosen);
  }
}

/** The other radio buttons of a radio button's group. */
function radioGroup(input) {
  const name = attributeOf(input, "name");
  if (name === null || name === "") {
    return [];
  }
  const form = formOwner(input);
  const scope = isConnectedOf(input)
    ? documentNode(DOC.DOCUMENT)
    : relative(input, REL.ROOT);
  const candidates = queryAll(scope, "input");
  const group = [];
  for (let i = 0; i < candidates.length; i++) {
    const candidate = candidates[i];
    if (
      candidate !== input && inputTypeOf(candidate) === "radio" &&
      attributeOf(candidate, "name") === name && formOwner(candidate) === form
    ) {
      ArrayPrototypePush(group, candidate);
    }
  }
  return group;
}

function setChecked(input, checked) {
  inputState.setChecked(input, checked);
  if (checked && inputTypeOf(input) === "radio") {
    const group = radioGroup(input);
    for (let i = 0; i < group.length; i++) {
      inputState.setChecked(group[i], false);
    }
  }
}

// --- form submission ----------------------------------------------------------------

/** https://html.spec.whatwg.org/#constructing-the-form-data-set, without files. */
function constructEntryList(form, submitter) {
  const entries = [];
  const controls = formControls(form);
  for (let i = 0; i < controls.length; i++) {
    const control = controls[i];
    if (isDisabled(control)) {
      continue;
    }
    const localName = htmlLocalName(control);
    const name = attributeOf(control, "name");
    if (localName === "button") {
      if (control === submitter && name !== null && name !== "") {
        ArrayPrototypePush(entries, [
          name,
          attributeOf(control, "value") ?? "",
        ]);
      }
      continue;
    }
    if (name === null || name === "") {
      continue;
    }
    if (localName === "input") {
      const type = inputTypeOf(control);
      switch (type) {
        case "submit":
          if (control === submitter) {
            ArrayPrototypePush(entries, [name, inputState.value(control)]);
          }
          break;
        case "image":
        case "reset":
        case "button":
        case "file":
          break;
        case "checkbox":
        case "radio":
          if (inputState.checked(control)) {
            ArrayPrototypePush(entries, [name, inputState.value(control)]);
          }
          break;
        default:
          ArrayPrototypePush(entries, [name, inputState.value(control)]);
      }
    } else if (localName === "select") {
      const selected = selectedOptionsOf(control);
      for (let j = 0; j < selected.length; j++) {
        if (!isDisabled(selected[j])) {
          ArrayPrototypePush(entries, [name, optionValue(selected[j])]);
        }
      }
    } else if (localName === "textarea") {
      ArrayPrototypePush(entries, [name, textareaState.value(control)]);
    }
  }
  return entries;
}

function formMethod(value) {
  const method = StringPrototypeToLowerCase(value ?? "");
  return method === "post" || method === "dialog" ? method : "get";
}

function formEnctype(value) {
  const enctype = StringPrototypeToLowerCase(value ?? "");
  return enctype === "multipart/form-data" || enctype === "text/plain"
    ? enctype
    : "application/x-www-form-urlencoded";
}

/**
 * Submits a form: fires `submit` (unless `submit()` was called), and if
 * nothing cancels it hands the submission to the host. The page context
 * never navigates.
 */
function submitForm(form, submitter, fromSubmitMethod) {
  if (!fromSubmitMethod) {
    const proceed = fireEvent(form, SubmitEvent, "submit", {
      __proto__: null,
      bubbles: true,
      cancelable: true,
      submitter,
    });
    if (!proceed) {
      return;
    }
  }
  if (!isConnectedOf(form)) {
    return;
  }
  const override = (attribute) =>
    submitter === null ? null : attributeOf(submitter, attribute);
  const method = formMethod(
    override("formmethod") ?? attributeOf(form, "method"),
  );
  if (method === "dialog") {
    return;
  }
  const action = override("formaction") ?? attributeOf(form, "action") ?? "";
  op_web_page_navigate({
    __proto__: null,
    kind: "form",
    url: resolveUrl(action) ?? getDocumentUrl(),
    method,
    enctype: formEnctype(
      override("formenctype") ?? attributeOf(form, "enctype"),
    ),
    target: override("formtarget") ?? attributeOf(form, "target") ?? "",
    entries: constructEntryList(form, submitter),
  });
}

function resetForm(form) {
  const proceed = fireEvent(form, Event, "reset", {
    __proto__: null,
    bubbles: true,
    cancelable: true,
  });
  if (!proceed) {
    return;
  }
  const controls = formControls(form);
  for (let i = 0; i < controls.length; i++) {
    const control = controls[i];
    switch (htmlLocalName(control)) {
      case "input":
        inputState.reset(control);
        break;
      case "textarea":
        textareaState.reset(control);
        break;
      case "select": {
        const options = selectOptions(control);
        for (let j = 0; j < options.length; j++) {
          optionState.reset(options[j]);
        }
        break;
      }
    }
  }
}

// --- focus ---------------------------------------------------------------------------

function isFocusable(element) {
  if (isDisabled(element)) {
    return false;
  }
  switch (htmlLocalName(element)) {
    case "input":
      return inputTypeOf(element) !== "hidden";
    case "select":
    case "textarea":
    case "button":
      return true;
    case "a":
    case "area":
      if (hasAttributeOf(element, "href")) {
        return true;
      }
      break;
  }
  return hasAttributeOf(element, "tabindex") ||
    hasAttributeOf(element, "contenteditable");
}

/** Moves focus to `element` (null: to nothing), firing the focus events. */
function focusElement(element) {
  const previous = getActiveElement();
  if (previous === element) {
    return;
  }
  setActiveElement(element);
  if (previous !== null && isConnectedOf(previous)) {
    fireEvent(previous, FocusEvent, "blur", {
      __proto__: null,
      composed: true,
      relatedTarget: element,
    });
    fireEvent(previous, FocusEvent, "focusout", {
      __proto__: null,
      bubbles: true,
      composed: true,
      relatedTarget: element,
    });
  }
  if (element !== null && getActiveElement() === element) {
    fireEvent(element, FocusEvent, "focus", {
      __proto__: null,
      composed: true,
      relatedTarget: previous,
    });
    fireEvent(element, FocusEvent, "focusin", {
      __proto__: null,
      bubbles: true,
      composed: true,
      relatedTarget: previous,
    });
  }
}

// --- click and its activation behavior ---------------------------------------------

function fireClick(target, trusted) {
  const ev = new PointerEvent("click", {
    __proto__: null,
    bubbles: true,
    cancelable: true,
    composed: true,
    view: getWindow(),
    detail: 1,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
  });
  event.setIsTrusted(ev, trusted);
  return dispatchNodeEvent(target, ev);
}

/** What activating an element does, or null if it does nothing. */
function activationKind(element) {
  switch (htmlLocalName(element)) {
    case "input": {
      if (isDisabled(element)) {
        return null;
      }
      const type = inputTypeOf(element);
      if (type === "checkbox" || type === "radio" || type === "reset") {
        return type;
      }
      return type === "submit" || type === "image" ? "submit" : null;
    }
    case "button": {
      if (isDisabled(element)) {
        return null;
      }
      const type = buttonTypeOf(element);
      return type === "button" ? null : type;
    }
    case "a":
    case "area":
      return hasAttributeOf(element, "href") ? "link" : null;
    case "label":
      return "label";
    default:
      return null;
  }
}

function labeledControl(label) {
  const id = attributeOf(label, "for");
  if (id !== null) {
    const control = elementById(id);
    if (control === null) {
      return null;
    }
    const name = htmlLocalName(control);
    return ArrayPrototypeIncludes(LABELABLE, name) &&
        !(name === "input" && inputTypeOf(control) === "hidden")
      ? control
      : null;
  }
  return queryFirst(
    label,
    "button, input:not([type=hidden]), meter, output, progress, select, textarea",
  );
}

const LABELABLE = [
  "button",
  "input",
  "meter",
  "output",
  "progress",
  "select",
  "textarea",
];

setClickActivation({
  __proto__: null,
  // The legacy-pre-activation steps: a checkbox or radio button changes
  // state before the event is dispatched, so listeners see the new state.
  begin(target, ev) {
    if (!ObjectPrototypeIsPrototypeOf(MouseEventPrototype, ev)) {
      return null;
    }
    let element = isElement(target)
      ? target
      : relative(target, REL.PARENT_ELEMENT);
    let kind = null;
    while (element !== null) {
      kind = activationKind(element);
      if (kind !== null || !ev.bubbles) {
        break;
      }
      element = relative(element, REL.PARENT_ELEMENT);
    }
    if (element === null || kind === null) {
      return null;
    }
    const token = {
      __proto__: null,
      kind,
      element,
      target,
      wasChecked: false,
      previousRadio: null,
    };
    if (kind === "checkbox") {
      token.wasChecked = inputState.checked(element);
      setChecked(element, !token.wasChecked);
    } else if (kind === "radio") {
      token.wasChecked = inputState.checked(element);
      const group = radioGroup(element);
      for (let i = 0; i < group.length; i++) {
        if (inputState.checked(group[i])) {
          token.previousRadio = group[i];
          break;
        }
      }
      setChecked(element, true);
    }
    return token;
  },
  // The activation behavior, or the legacy-canceled-activation steps.
  end(token, notCanceled, ev) {
    const { kind, element } = token;
    if (!notCanceled) {
      if (kind === "checkbox") {
        setChecked(element, token.wasChecked);
      } else if (kind === "radio" && !token.wasChecked) {
        inputState.setChecked(element, false);
        if (token.previousRadio !== null) {
          inputState.setChecked(token.previousRadio, true);
        }
      }
      return;
    }
    switch (kind) {
      case "checkbox":
      case "radio":
        if (
          isConnectedOf(element) &&
          (kind === "checkbox" || !token.wasChecked)
        ) {
          fireEvent(element, Event, "input", {
            __proto__: null,
            bubbles: true,
            composed: true,
          });
          fireEvent(element, Event, "change", {
            __proto__: null,
            bubbles: true,
          });
        }
        break;
      case "submit": {
        const form = formOwner(element);
        if (form !== null) {
          submitForm(form, element, false);
        }
        break;
      }
      case "reset": {
        const form = formOwner(element);
        if (form !== null) {
          resetForm(form);
        }
        break;
      }
      case "link": {
        const url = resolveUrl(attributeOf(element, "href") ?? "");
        // A `javascript:` URL is not run in this cut, and is no navigation.
        if (url !== null && !StringPrototypeStartsWith(url, "javascript:")) {
          op_web_page_navigate({
            __proto__: null,
            kind: "link",
            url,
            target: attributeOf(element, "target") ?? "",
          });
        }
        break;
      }
      case "label": {
        const control = labeledControl(element);
        if (
          control !== null && !containsOf(control, token.target) &&
          !isDisabled(control)
        ) {
          fireClick(control, ev.isTrusted === true);
        }
        break;
      }
    }
  },
});

// --- HTMLElement -----------------------------------------------------------------------

class HTMLElement extends Element {
  #dataset = null;
  #style = null;

  static {
    isHtmlElement = (value) =>
      typeof value === "object" && value !== null && #dataset in value;
  }

  get dataset() {
    checkHtml(this);
    this.#dataset ??= createDataset(this);
    return this.#dataset;
  }

  get style() {
    checkHtml(this);
    this.#style ??= createStyle(this);
    return this.#style;
  }

  set style(value) {
    checkHtml(this);
    setAttributeOf(this, "style", toStr(value));
  }

  get tabIndex() {
    checkHtml(this);
    const value = NumberParseInt(attributeOf(this, "tabindex") ?? "", 10);
    if (!NumberIsNaN(value)) {
      return value;
    }
    return isFocusable(this) ? 0 : -1;
  }

  set tabIndex(value) {
    checkHtml(this);
    setAttributeOf(this, "tabindex", String(value | 0));
  }

  get innerText() {
    checkHtml(this);
    return textOf(this);
  }

  set innerText(value) {
    checkHtml(this);
    setTextOf(this, value === null ? "" : toStr(value));
  }

  get outerText() {
    checkHtml(this);
    return textOf(this);
  }

  get isContentEditable() {
    checkHtml(this);
    return false;
  }

  click() {
    checkHtml(this);
    if (isDisabled(this)) {
      return;
    }
    fireClick(this, false);
  }

  focus(_options = undefined) {
    checkHtml(this);
    if (isConnectedOf(this) && isFocusable(this)) {
      focusElement(this);
    }
  }

  blur() {
    checkHtml(this);
    if (getActiveElement() === this) {
      focusElement(null);
    }
  }
}
reflectString(HTMLElement, "title");
reflectString(HTMLElement, "lang");
reflectString(HTMLElement, "dir");
reflectString(HTMLElement, "accessKey");
reflectBoolean(HTMLElement, "hidden");
reflectBoolean(HTMLElement, "inert");
reflectBoolean(HTMLElement, "draggable");
setDefaultHtmlInterface(HTMLElement);

// --- form controls -------------------------------------------------------------------

class HTMLInputElement extends HTMLElement {
  // The dirty value, and the checkedness once it left the `checked`
  // attribute's lead. null: the content attribute decides.
  #value = null;
  #checked = null;

  static {
    inputState = {
      __proto__: null,
      value(input) {
        switch (inputValueMode(inputTypeOf(input))) {
          case "value":
            return input.#value ?? attributeOf(input, "value") ?? "";
          case "default":
            return attributeOf(input, "value") ?? "";
          case "default/on":
            return attributeOf(input, "value") ?? "on";
          default:
            return "";
        }
      },
      setValue(input, value) {
        input.#value = value;
      },
      checked(input) {
        return input.#checked ?? hasAttributeOf(input, "checked");
      },
      setChecked(input, checked) {
        input.#checked = checked;
      },
      reset(input) {
        input.#value = null;
        input.#checked = null;
      },
      copy(from, to) {
        to.#value = from.#value;
        to.#checked = from.#checked;
      },
      is(value) {
        return typeof value === "object" && value !== null && #value in value;
      },
    };
  }

  static #check(value) {
    if (!inputState.is(value)) {
      throw illegalInvocation();
    }
  }

  get type() {
    HTMLInputElement.#check(this);
    return inputTypeOf(this);
  }

  set type(value) {
    HTMLInputElement.#check(this);
    setAttributeOf(this, "type", toStr(value));
  }

  get value() {
    HTMLInputElement.#check(this);
    return inputState.value(this);
  }

  set value(value) {
    HTMLInputElement.#check(this);
    value = value === null ? "" : toStr(value);
    switch (inputValueMode(inputTypeOf(this))) {
      case "value":
        this.#value = value;
        break;
      case "filename":
        if (value !== "") {
          throw new DOMException(
            "This input element accepts a filename, which may only be programmatically set to the empty string.",
            "InvalidStateError",
          );
        }
        break;
      default:
        setAttributeOf(this, "value", value);
    }
  }

  get defaultValue() {
    HTMLInputElement.#check(this);
    return attributeOf(this, "value") ?? "";
  }

  set defaultValue(value) {
    HTMLInputElement.#check(this);
    setAttributeOf(this, "value", toStr(value));
  }

  get checked() {
    HTMLInputElement.#check(this);
    return inputState.checked(this);
  }

  set checked(value) {
    HTMLInputElement.#check(this);
    setChecked(this, !!value);
  }

  get files() {
    HTMLInputElement.#check(this);
    return null;
  }

  select() {
    HTMLInputElement.#check(this);
  }

  checkValidity() {
    HTMLInputElement.#check(this);
    return true;
  }

  reportValidity() {
    HTMLInputElement.#check(this);
    return true;
  }
}
reflectString(HTMLInputElement, "name");
reflectString(HTMLInputElement, "placeholder");
reflectString(HTMLInputElement, "alt");
reflectString(HTMLInputElement, "accept");
reflectString(HTMLInputElement, "autocomplete");
reflectString(HTMLInputElement, "min");
reflectString(HTMLInputElement, "max");
reflectString(HTMLInputElement, "step");
reflectString(HTMLInputElement, "pattern");
reflectUrl(HTMLInputElement, "src");
reflectBoolean(HTMLInputElement, "disabled");
reflectBoolean(HTMLInputElement, "required");
reflectBoolean(HTMLInputElement, "readOnly");
reflectBoolean(HTMLInputElement, "multiple");
reflectBoolean(HTMLInputElement, "autofocus");
reflectBoolean(HTMLInputElement, "defaultChecked", "checked");
reflectForm(HTMLInputElement);

class HTMLTextAreaElement extends HTMLElement {
  // The raw value once it was set; null: the text content decides.
  #value = null;

  static {
    textareaState = {
      __proto__: null,
      value(textarea) {
        return textarea.#value ?? textOf(textarea);
      },
      setValue(textarea, value) {
        textarea.#value = value;
      },
      reset(textarea) {
        textarea.#value = null;
      },
      copy(from, to) {
        to.#value = from.#value;
      },
      is(value) {
        return typeof value === "object" && value !== null && #value in value;
      },
    };
  }

  static #check(value) {
    if (!textareaState.is(value)) {
      throw illegalInvocation();
    }
  }

  get type() {
    HTMLTextAreaElement.#check(this);
    return "textarea";
  }

  get value() {
    HTMLTextAreaElement.#check(this);
    return textareaState.value(this);
  }

  set value(value) {
    HTMLTextAreaElement.#check(this);
    this.#value = value === null ? "" : toStr(value);
  }

  get defaultValue() {
    HTMLTextAreaElement.#check(this);
    return textOf(this);
  }

  set defaultValue(value) {
    HTMLTextAreaElement.#check(this);
    setTextOf(this, toStr(value));
  }

  get textLength() {
    HTMLTextAreaElement.#check(this);
    return textareaState.value(this).length;
  }

  select() {
    HTMLTextAreaElement.#check(this);
  }

  checkValidity() {
    HTMLTextAreaElement.#check(this);
    return true;
  }

  reportValidity() {
    HTMLTextAreaElement.#check(this);
    return true;
  }
}
reflectString(HTMLTextAreaElement, "name");
reflectString(HTMLTextAreaElement, "placeholder");
reflectString(HTMLTextAreaElement, "autocomplete");
reflectBoolean(HTMLTextAreaElement, "disabled");
reflectBoolean(HTMLTextAreaElement, "required");
reflectBoolean(HTMLTextAreaElement, "readOnly");
reflectBoolean(HTMLTextAreaElement, "autofocus");
reflectForm(HTMLTextAreaElement);

class HTMLOptionElement extends HTMLElement {
  // The selectedness once it left the `selected` attribute's lead.
  #selected = null;

  static {
    optionState = {
      __proto__: null,
      selected(option) {
        return option.#selected ?? hasAttributeOf(option, "selected");
      },
      setSelected(option, selected) {
        option.#selected = selected;
      },
      reset(option) {
        option.#selected = null;
      },
      copy(from, to) {
        to.#selected = from.#selected;
      },
      is(value) {
        return typeof value === "object" && value !== null &&
          #selected in value;
      },
    };
  }

  static #check(value) {
    if (!optionState.is(value)) {
      throw illegalInvocation();
    }
  }

  static #select(option) {
    const parent = relative(option, REL.PARENT_ELEMENT);
    return parent === null ? null : closestOf(parent, "select");
  }

  get value() {
    HTMLOptionElement.#check(this);
    return optionValue(this);
  }

  set value(value) {
    HTMLOptionElement.#check(this);
    setAttributeOf(this, "value", toStr(value));
  }

  get text() {
    HTMLOptionElement.#check(this);
    return optionText(this);
  }

  set text(value) {
    HTMLOptionElement.#check(this);
    setTextOf(this, toStr(value));
  }

  get label() {
    HTMLOptionElement.#check(this);
    return attributeOf(this, "label") ?? optionText(this);
  }

  set label(value) {
    HTMLOptionElement.#check(this);
    setAttributeOf(this, "label", toStr(value));
  }

  get selected() {
    HTMLOptionElement.#check(this);
    const select = HTMLOptionElement.#select(this);
    if (select === null) {
      return optionState.selected(this);
    }
    return ArrayPrototypeIncludes(selectedOptionsOf(select), this);
  }

  set selected(value) {
    HTMLOptionElement.#check(this);
    const select = HTMLOptionElement.#select(this);
    if (value && select !== null && !hasAttributeOf(select, "multiple")) {
      selectOnly(select, this);
    } else {
      this.#selected = !!value;
    }
  }

  get index() {
    HTMLOptionElement.#check(this);
    const select = HTMLOptionElement.#select(this);
    return select === null
      ? 0
      : ArrayPrototypeIndexOf(selectOptions(select), this);
  }

  get form() {
    HTMLOptionElement.#check(this);
    const select = HTMLOptionElement.#select(this);
    return select === null ? null : formOwner(select);
  }
}
reflectBoolean(HTMLOptionElement, "disabled");
reflectBoolean(HTMLOptionElement, "defaultSelected", "selected");

class HTMLSelectElement extends HTMLElement {
  #select() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#select in value)) {
      throw illegalInvocation();
    }
  }

  get type() {
    HTMLSelectElement.#check(this);
    return hasAttributeOf(this, "multiple") ? "select-multiple" : "select-one";
  }

  get options() {
    HTMLSelectElement.#check(this);
    return htmlCollection(selectOptions(this));
  }

  get selectedOptions() {
    HTMLSelectElement.#check(this);
    return htmlCollection(selectedOptionsOf(this));
  }

  get length() {
    HTMLSelectElement.#check(this);
    return selectOptions(this).length;
  }

  item(index) {
    HTMLSelectElement.#check(this);
    return selectOptions(this)[index >>> 0] ?? null;
  }

  get selectedIndex() {
    HTMLSelectElement.#check(this);
    const selected = selectedOptionsOf(this);
    return selected.length === 0
      ? -1
      : ArrayPrototypeIndexOf(selectOptions(this), selected[0]);
  }

  set selectedIndex(value) {
    HTMLSelectElement.#check(this);
    selectOnly(this, selectOptions(this)[value | 0] ?? null);
  }

  get value() {
    HTMLSelectElement.#check(this);
    return selectValue(this);
  }

  set value(value) {
    HTMLSelectElement.#check(this);
    value = toStr(value);
    const options = selectOptions(this);
    let chosen = null;
    for (let i = 0; i < options.length; i++) {
      if (optionValue(options[i]) === value) {
        chosen = options[i];
        break;
      }
    }
    selectOnly(this, chosen);
  }

  checkValidity() {
    HTMLSelectElement.#check(this);
    return true;
  }

  reportValidity() {
    HTMLSelectElement.#check(this);
    return true;
  }
}
reflectString(HTMLSelectElement, "name");
reflectString(HTMLSelectElement, "autocomplete");
reflectBoolean(HTMLSelectElement, "disabled");
reflectBoolean(HTMLSelectElement, "required");
reflectBoolean(HTMLSelectElement, "multiple");
reflectBoolean(HTMLSelectElement, "autofocus");
reflectForm(HTMLSelectElement);

class HTMLButtonElement extends HTMLElement {
  #button() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#button in value)) {
      throw illegalInvocation();
    }
  }

  get type() {
    HTMLButtonElement.#check(this);
    return buttonTypeOf(this);
  }

  set type(value) {
    HTMLButtonElement.#check(this);
    setAttributeOf(this, "type", toStr(value));
  }

  checkValidity() {
    HTMLButtonElement.#check(this);
    return true;
  }

  reportValidity() {
    HTMLButtonElement.#check(this);
    return true;
  }
}
reflectString(HTMLButtonElement, "name");
reflectString(HTMLButtonElement, "value");
reflectBoolean(HTMLButtonElement, "disabled");
reflectBoolean(HTMLButtonElement, "autofocus");
reflectForm(HTMLButtonElement);

class HTMLFormElement extends HTMLElement {
  #form() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#form in value)) {
      throw illegalInvocation();
    }
  }

  get action() {
    HTMLFormElement.#check(this);
    const action = attributeOf(this, "action");
    if (action === null || action === "") {
      return getDocumentUrl();
    }
    return resolveUrl(action) ?? action;
  }

  set action(value) {
    HTMLFormElement.#check(this);
    setAttributeOf(this, "action", toStr(value));
  }

  get method() {
    HTMLFormElement.#check(this);
    return formMethod(attributeOf(this, "method"));
  }

  set method(value) {
    HTMLFormElement.#check(this);
    setAttributeOf(this, "method", toStr(value));
  }

  get enctype() {
    HTMLFormElement.#check(this);
    return formEnctype(attributeOf(this, "enctype"));
  }

  set enctype(value) {
    HTMLFormElement.#check(this);
    setAttributeOf(this, "enctype", toStr(value));
  }

  get encoding() {
    HTMLFormElement.#check(this);
    return formEnctype(attributeOf(this, "enctype"));
  }

  set encoding(value) {
    HTMLFormElement.#check(this);
    setAttributeOf(this, "enctype", toStr(value));
  }

  get elements() {
    HTMLFormElement.#check(this);
    return htmlCollection(listedControls(this));
  }

  get length() {
    HTMLFormElement.#check(this);
    return listedControls(this).length;
  }

  submit() {
    HTMLFormElement.#check(this);
    submitForm(this, null, true);
  }

  requestSubmit(submitter = undefined) {
    HTMLFormElement.#check(this);
    if (submitter === undefined || submitter === null) {
      submitForm(this, null, false);
      return;
    }
    if (!isElement(submitter) || activationKind(submitter) !== "submit") {
      throw new TypeError(
        "Failed to execute 'requestSubmit' on 'HTMLFormElement': The specified element is not a submit button.",
      );
    }
    if (formOwner(submitter) !== this) {
      throw new DOMException(
        "The specified element is not owned by this form element.",
        "NotFoundError",
      );
    }
    submitForm(this, submitter, false);
  }

  reset() {
    HTMLFormElement.#check(this);
    resetForm(this);
  }

  checkValidity() {
    HTMLFormElement.#check(this);
    return true;
  }

  reportValidity() {
    HTMLFormElement.#check(this);
    return true;
  }
}
reflectString(HTMLFormElement, "name");
reflectString(HTMLFormElement, "target");
reflectString(HTMLFormElement, "acceptCharset", "accept-charset");
reflectBoolean(HTMLFormElement, "noValidate");

/** `form.elements`: the listed controls, image buttons left out. */
function listedControls(form) {
  const controls = formControls(form);
  const listed = [];
  for (let i = 0; i < controls.length; i++) {
    if (
      htmlLocalName(controls[i]) !== "input" ||
      inputTypeOf(controls[i]) !== "image"
    ) {
      ArrayPrototypePush(listed, controls[i]);
    }
  }
  return listed;
}

class HTMLLabelElement extends HTMLElement {
  #label() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#label in value)) {
      throw illegalInvocation();
    }
  }

  get control() {
    HTMLLabelElement.#check(this);
    return labeledControl(this);
  }

  get form() {
    HTMLLabelElement.#check(this);
    const control = labeledControl(this);
    return control === null ? null : formOwner(control);
  }
}
reflectString(HTMLLabelElement, "htmlFor", "for");

// --- links and the rest ----------------------------------------------------------------

/** The parsed `href` of a link, or null. */
function linkUrl(element) {
  const href = attributeOf(element, "href");
  if (href === null) {
    return null;
  }
  try {
    return new URL(href, getDocumentUrl());
  } catch {
    return null;
  }
}

class HTMLAnchorElement extends HTMLElement {
  #anchor() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#anchor in value)) {
      throw illegalInvocation();
    }
  }

  get href() {
    HTMLAnchorElement.#check(this);
    const href = attributeOf(this, "href");
    if (href === null) {
      return "";
    }
    return resolveUrl(href) ?? href;
  }

  set href(value) {
    HTMLAnchorElement.#check(this);
    setAttributeOf(this, "href", toStr(value));
  }

  get origin() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.origin ?? "";
  }

  get protocol() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.protocol ?? ":";
  }

  get host() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.host ?? "";
  }

  get hostname() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.hostname ?? "";
  }

  get port() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.port ?? "";
  }

  get pathname() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.pathname ?? "";
  }

  get search() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.search ?? "";
  }

  get hash() {
    HTMLAnchorElement.#check(this);
    return linkUrl(this)?.hash ?? "";
  }

  get text() {
    HTMLAnchorElement.#check(this);
    return textOf(this);
  }

  set text(value) {
    HTMLAnchorElement.#check(this);
    setTextOf(this, toStr(value));
  }

  get relList() {
    HTMLAnchorElement.#check(this);
    return tokenList(this, "rel");
  }

  toString() {
    HTMLAnchorElement.#check(this);
    return this.href;
  }
}
reflectString(HTMLAnchorElement, "target");
reflectString(HTMLAnchorElement, "rel");
reflectString(HTMLAnchorElement, "download");
reflectString(HTMLAnchorElement, "hreflang");
reflectString(HTMLAnchorElement, "type");

class HTMLImageElement extends HTMLElement {}
reflectUrl(HTMLImageElement, "src");
reflectString(HTMLImageElement, "alt");
reflectString(HTMLImageElement, "srcset");
reflectString(HTMLImageElement, "loading");

class HTMLScriptElement extends HTMLElement {
  #script() {}

  static #check(value) {
    if (typeof value !== "object" || value === null || !(#script in value)) {
      throw illegalInvocation();
    }
  }

  get text() {
    HTMLScriptElement.#check(this);
    return textOf(this);
  }

  set text(value) {
    HTMLScriptElement.#check(this);
    setTextOf(this, toStr(value));
  }
}
reflectUrl(HTMLScriptElement, "src");
reflectString(HTMLScriptElement, "type");
reflectString(HTMLScriptElement, "integrity");
reflectString(HTMLScriptElement, "crossOrigin", "crossorigin");
reflectBoolean(HTMLScriptElement, "async");
reflectBoolean(HTMLScriptElement, "defer");
reflectBoolean(HTMLScriptElement, "noModule");

class HTMLLinkElement extends HTMLElement {
  #link() {}

  get relList() {
    if (typeof this !== "object" || this === null || !(#link in this)) {
      throw illegalInvocation();
    }
    return tokenList(this, "rel");
  }
}
reflectUrl(HTMLLinkElement, "href");
reflectString(HTMLLinkElement, "rel");
reflectString(HTMLLinkElement, "type");
reflectString(HTMLLinkElement, "media");
reflectString(HTMLLinkElement, "as");

class HTMLMetaElement extends HTMLElement {}
reflectString(HTMLMetaElement, "name");
reflectString(HTMLMetaElement, "content");
reflectString(HTMLMetaElement, "httpEquiv", "http-equiv");

class HTMLTemplateElement extends HTMLElement {
  #template() {}

  get content() {
    if (typeof this !== "object" || this === null || !(#template in this)) {
      throw illegalInvocation();
    }
    return relative(this, REL.TEMPLATE_CONTENTS);
  }
}

/** An interface with no members of its own, named `name`. */
function plainInterface(name) {
  return { [name]: class extends HTMLElement {} }[name];
}

const HTMLDivElement = plainInterface("HTMLDivElement");
const HTMLSpanElement = plainInterface("HTMLSpanElement");
const HTMLParagraphElement = plainInterface("HTMLParagraphElement");
const HTMLHtmlElement = plainInterface("HTMLHtmlElement");
const HTMLHeadElement = plainInterface("HTMLHeadElement");
const HTMLBodyElement = plainInterface("HTMLBodyElement");
const HTMLTitleElement = plainInterface("HTMLTitleElement");
const HTMLStyleElement = plainInterface("HTMLStyleElement");
const HTMLHeadingElement = plainInterface("HTMLHeadingElement");
const HTMLUListElement = plainInterface("HTMLUListElement");
const HTMLOListElement = plainInterface("HTMLOListElement");
const HTMLLIElement = plainInterface("HTMLLIElement");
const HTMLTableElement = plainInterface("HTMLTableElement");
const HTMLPreElement = plainInterface("HTMLPreElement");
const HTMLBRElement = plainInterface("HTMLBRElement");
const HTMLFieldSetElement = plainInterface("HTMLFieldSetElement");
const HTMLOptGroupElement = plainInterface("HTMLOptGroupElement");
reflectBoolean(HTMLFieldSetElement, "disabled");
reflectString(HTMLFieldSetElement, "name");
reflectBoolean(HTMLOptGroupElement, "disabled");
reflectString(HTMLOptGroupElement, "label");

// Local name -> interface. Every other HTML element is a plain HTMLElement
// in this cut (HTMLUnknownElement included).
const htmlInterfaces = {
  __proto__: null,
  a: HTMLAnchorElement,
  body: HTMLBodyElement,
  br: HTMLBRElement,
  button: HTMLButtonElement,
  div: HTMLDivElement,
  fieldset: HTMLFieldSetElement,
  form: HTMLFormElement,
  h1: HTMLHeadingElement,
  h2: HTMLHeadingElement,
  h3: HTMLHeadingElement,
  h4: HTMLHeadingElement,
  h5: HTMLHeadingElement,
  h6: HTMLHeadingElement,
  head: HTMLHeadElement,
  html: HTMLHtmlElement,
  img: HTMLImageElement,
  input: HTMLInputElement,
  label: HTMLLabelElement,
  li: HTMLLIElement,
  link: HTMLLinkElement,
  meta: HTMLMetaElement,
  ol: HTMLOListElement,
  optgroup: HTMLOptGroupElement,
  option: HTMLOptionElement,
  p: HTMLParagraphElement,
  pre: HTMLPreElement,
  script: HTMLScriptElement,
  select: HTMLSelectElement,
  span: HTMLSpanElement,
  style: HTMLStyleElement,
  table: HTMLTableElement,
  template: HTMLTemplateElement,
  textarea: HTMLTextAreaElement,
  title: HTMLTitleElement,
  ul: HTMLUListElement,
};

// Interface name -> interface, for the globals.
const interfaces = { __proto__: null, HTMLElement, CSSStyleDeclaration };
{
  webidl.configureInterface(HTMLElement);
  const localNames = [
    "a",
    "body",
    "br",
    "button",
    "div",
    "fieldset",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "html",
    "img",
    "input",
    "label",
    "li",
    "link",
    "meta",
    "ol",
    "optgroup",
    "option",
    "p",
    "pre",
    "script",
    "select",
    "span",
    "style",
    "table",
    "template",
    "textarea",
    "title",
    "ul",
  ];
  for (let i = 0; i < localNames.length; i++) {
    const interface_ = htmlInterfaces[localNames[i]];
    registerHtmlInterface(localNames[i], interface_);
    if (interfaces[interface_.name] === undefined) {
      webidl.configureInterface(interface_);
      interfaces[interface_.name] = interface_;
    }
  }
}

// --- event handler attributes -------------------------------------------------------

// The event handlers of GlobalEventHandlers this cut knows: IDL attributes
// on HTMLElement and Document, and content attributes on HTML elements.
const EVENT_HANDLERS = [
  "abort",
  "blur",
  "change",
  "click",
  "contextmenu",
  "dblclick",
  "error",
  "focus",
  "focusin",
  "focusout",
  "input",
  "invalid",
  "keydown",
  "keypress",
  "keyup",
  "load",
  "mousedown",
  "mouseenter",
  "mouseleave",
  "mousemove",
  "mouseout",
  "mouseover",
  "mouseup",
  "pointerdown",
  "pointermove",
  "pointerup",
  "reset",
  "scroll",
  "select",
  "submit",
  "toggle",
  "touchend",
  "touchmove",
  "touchstart",
  "wheel",
];
for (let i = 0; i < EVENT_HANDLERS.length; i++) {
  event.defineEventHandler(HTMLElement.prototype, EVENT_HANDLERS[i]);
  event.defineEventHandler(Document.prototype, EVENT_HANDLERS[i]);
  registerEventHandlerName(EVENT_HANDLERS[i]);
}
event.defineEventHandler(Document.prototype, "readystatechange");
event.defineEventHandler(Document.prototype, "visibilitychange");

// --- wrapper state on clones ---------------------------------------------------------

function copyControlState(from, to) {
  if (inputState.is(from) && inputState.is(to)) {
    inputState.copy(from, to);
  } else if (textareaState.is(from) && textareaState.is(to)) {
    textareaState.copy(from, to);
  } else if (optionState.is(from) && optionState.is(to)) {
    optionState.copy(from, to);
  }
}

const STATEFUL = "input, textarea, option";

// `cloneNode` copies the values users typed, as the cloning steps of input,
// textarea and option do.
setCloneState((from, to, deep) => {
  if (!isElement(from) || !isElement(to)) {
    if (!deep) {
      return;
    }
  } else {
    copyControlState(from, to);
  }
  if (!deep) {
    return;
  }
  const originals = queryAll(from, STATEFUL);
  const copies = queryAll(to, STATEFUL);
  if (originals.length !== copies.length) {
    return;
  }
  for (let i = 0; i < originals.length; i++) {
    copyControlState(originals[i], copies[i]);
  }
});

// --- what the host calls ---------------------------------------------------------------

function fireMouse(target, EventInterface, type, buttons) {
  return fireEvent(target, EventInterface, type, {
    __proto__: null,
    bubbles: true,
    cancelable: true,
    composed: true,
    view: getWindow(),
    detail: 1,
    buttons,
    pointerId: 1,
    pointerType: "mouse",
    isPrimary: true,
  });
}

/**
 * A user's click on a node: the pointer and mouse events, the focus change,
 * then `click` with its default action. Answers whether `click` was not
 * canceled.
 */
function hostClick(index, generation) {
  let target = nodeFromId(index, generation);
  if (target !== null && !isElement(target)) {
    target = relative(target, REL.PARENT_ELEMENT);
  }
  // A disabled control gets no mouse events.
  if (target === null || isDisabled(target)) {
    return true;
  }
  fireMouse(target, PointerEvent, "pointerdown", 1);
  if (fireMouse(target, MouseEvent, "mousedown", 1)) {
    let focusable = target;
    while (focusable !== null && !isFocusable(focusable)) {
      focusable = relative(focusable, REL.PARENT_ELEMENT);
    }
    focusElement(focusable);
  }
  fireMouse(target, PointerEvent, "pointerup", 0);
  fireMouse(target, MouseEvent, "mouseup", 0);
  return fireClick(target, true);
}

function fireInputAndChange(target, data) {
  if (data === null) {
    fireEvent(target, Event, "input", {
      __proto__: null,
      bubbles: true,
      composed: true,
    });
  } else {
    fireEvent(target, InputEvent, "input", {
      __proto__: null,
      bubbles: true,
      composed: true,
      data,
      inputType: "insertText",
    });
  }
  fireEvent(target, Event, "change", { __proto__: null, bubbles: true });
}

/**
 * A user's typing into, or choosing in, a form control. Answers false when
 * the node has no value a user could set.
 */
function hostSetValue(index, generation, value) {
  const target = nodeFromId(index, generation);
  if (target === null || !isElement(target) || isDisabled(target)) {
    return false;
  }
  switch (htmlLocalName(target)) {
    case "input":
      if (
        inputValueMode(inputTypeOf(target)) !== "value" ||
        hasAttributeOf(target, "readonly")
      ) {
        return false;
      }
      focusElement(target);
      inputState.setValue(target, value);
      fireInputAndChange(target, value);
      return true;
    case "textarea":
      if (hasAttributeOf(target, "readonly")) {
        return false;
      }
      focusElement(target);
      textareaState.setValue(target, value);
      fireInputAndChange(target, value);
      return true;
    case "select": {
      // By value first, then by the text the user sees.
      const options = selectOptions(target);
      let chosen = null;
      for (let i = 0; i < options.length && chosen === null; i++) {
        if (optionValue(options[i]) === value) {
          chosen = options[i];
        }
      }
      for (let i = 0; i < options.length && chosen === null; i++) {
        if (optionText(options[i]) === value) {
          chosen = options[i];
        }
      }
      if (chosen === null || isDisabled(chosen)) {
        return false;
      }
      focusElement(target);
      selectOnly(target, chosen);
      fireInputAndChange(target, null);
      return true;
    }
    default:
      return false;
  }
}

/**
 * The live state of a form control, as "0" or "1" (checked or selected)
 * followed by its value; null for any other node.
 */
function hostFormState(index, generation) {
  const target = nodeFromId(index, generation);
  if (target === null || !isElement(target)) {
    return null;
  }
  switch (htmlLocalName(target)) {
    case "input":
      return (inputState.checked(target) &&
          inputValueMode(inputTypeOf(target)) === "default/on"
        ? "1"
        : "0") + inputState.value(target);
    case "textarea":
      return "0" + textareaState.value(target);
    case "select":
      return "0" + selectValue(target);
    case "option": {
      const parent = relative(target, REL.PARENT_ELEMENT);
      const select = parent === null ? null : closestOf(parent, "select");
      const selected = select === null
        ? optionState.selected(target)
        : ArrayPrototypeIncludes(selectedOptionsOf(select), target);
      return (selected ? "1" : "0") + optionValue(target);
    }
    case "button":
      return "0" + (attributeOf(target, "value") ?? "");
    default:
      return null;
  }
}

export { hostClick, hostFormState, hostSetValue, interfaces as htmlInterfaces };
