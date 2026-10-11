// Vendored from vitest@5.0.3 (dist/chunks/index.k8L_ZRzq.js, environments) by tools/vendor_vitest.ts. Do not edit.
// Copyright (c) 2021-Present VoidZero Inc. and Vitest contributors. MIT License, see LICENSE-vitest.md (which also lists bundled code).

import { createRequire } from "node:module";
import { URL as URL$1 } from "node:url";
import { Console } from "node:console";
import { denoVitestReportUnhandledError, denoVitestRootRequireBase } from "../shims/environment_support.js";

const LIVING_KEYS = [
	"DOMException",
	"EventTarget",
	"NamedNodeMap",
	"Node",
	"Attr",
	"Element",
	"DocumentFragment",
	"DOMImplementation",
	"Document",
	"XMLDocument",
	"CharacterData",
	"Text",
	"CDATASection",
	"ProcessingInstruction",
	"Comment",
	"DocumentType",
	"NodeList",
	"RadioNodeList",
	"HTMLCollection",
	"HTMLOptionsCollection",
	"DOMStringMap",
	"DOMTokenList",
	"StyleSheetList",
	"HTMLElement",
	"HTMLHeadElement",
	"HTMLTitleElement",
	"HTMLBaseElement",
	"HTMLLinkElement",
	"HTMLMetaElement",
	"HTMLStyleElement",
	"HTMLBodyElement",
	"HTMLHeadingElement",
	"HTMLParagraphElement",
	"HTMLHRElement",
	"HTMLPreElement",
	"HTMLUListElement",
	"HTMLOListElement",
	"HTMLLIElement",
	"HTMLMenuElement",
	"HTMLDListElement",
	"HTMLDivElement",
	"HTMLAnchorElement",
	"HTMLAreaElement",
	"HTMLBRElement",
	"HTMLButtonElement",
	"HTMLCanvasElement",
	"HTMLDataElement",
	"HTMLDataListElement",
	"HTMLDetailsElement",
	"HTMLDialogElement",
	"HTMLDirectoryElement",
	"HTMLFieldSetElement",
	"HTMLFontElement",
	"HTMLFormElement",
	"HTMLHtmlElement",
	"HTMLImageElement",
	"HTMLInputElement",
	"HTMLLabelElement",
	"HTMLLegendElement",
	"HTMLMapElement",
	"HTMLMarqueeElement",
	"HTMLMediaElement",
	"HTMLMeterElement",
	"HTMLModElement",
	"HTMLOptGroupElement",
	"HTMLOptionElement",
	"HTMLOutputElement",
	"HTMLPictureElement",
	"HTMLProgressElement",
	"HTMLQuoteElement",
	"HTMLScriptElement",
	"HTMLSelectElement",
	"HTMLSlotElement",
	"HTMLSourceElement",
	"HTMLSpanElement",
	"HTMLTableCaptionElement",
	"HTMLTableCellElement",
	"HTMLTableColElement",
	"HTMLTableElement",
	"HTMLTimeElement",
	"HTMLTableRowElement",
	"HTMLTableSectionElement",
	"HTMLTemplateElement",
	"HTMLTextAreaElement",
	"HTMLUnknownElement",
	"HTMLFrameElement",
	"HTMLFrameSetElement",
	"HTMLIFrameElement",
	"HTMLEmbedElement",
	"HTMLObjectElement",
	"HTMLParamElement",
	"HTMLVideoElement",
	"HTMLAudioElement",
	"HTMLTrackElement",
	"HTMLFormControlsCollection",
	"SVGElement",
	"SVGGraphicsElement",
	"SVGSVGElement",
	"SVGTitleElement",
	"SVGAnimatedString",
	"SVGNumber",
	"SVGStringList",
	"Event",
	"CloseEvent",
	"CustomEvent",
	"MessageEvent",
	"ErrorEvent",
	"HashChangeEvent",
	"PopStateEvent",
	"StorageEvent",
	"ProgressEvent",
	"PageTransitionEvent",
	"SubmitEvent",
	"UIEvent",
	"FocusEvent",
	"InputEvent",
	"MouseEvent",
	"KeyboardEvent",
	"TouchEvent",
	"CompositionEvent",
	"WheelEvent",
	"BarProp",
	"External",
	"Location",
	"History",
	"Screen",
	"Crypto",
	"Performance",
	"Navigator",
	"PluginArray",
	"MimeTypeArray",
	"Plugin",
	"MimeType",
	"FileReader",
	"FormData",
	"Blob",
	"File",
	"FileList",
	"ValidityState",
	"DOMParser",
	"XMLSerializer",
	"XMLHttpRequestEventTarget",
	"XMLHttpRequestUpload",
	"XMLHttpRequest",
	"WebSocket",
	"NodeFilter",
	"NodeIterator",
	"TreeWalker",
	"AbstractRange",
	"Range",
	"StaticRange",
	"Selection",
	"Storage",
	"CustomElementRegistry",
	"ShadowRoot",
	"MutationObserver",
	"MutationRecord",
	"Uint8Array",
	"Uint16Array",
	"Uint32Array",
	"Uint8ClampedArray",
	"Int8Array",
	"Int16Array",
	"Int32Array",
	"Float32Array",
	"Float64Array",
	"ArrayBuffer",
	"DOMRectReadOnly",
	"DOMRect",
	"Image",
	"Audio",
	"Option",
	"CSS"
];
const OTHER_KEYS = [
	"addEventListener",
	"alert",
	"blur",
	"cancelAnimationFrame",
	"close",
	"confirm",
	"createPopup",
	"dispatchEvent",
	"document",
	"focus",
	"frames",
	"getComputedStyle",
	"history",
	"innerHeight",
	"innerWidth",
	"length",
	"localStorage",
	"location",
	"matchMedia",
	"moveBy",
	"moveTo",
	"name",
	"navigator",
	"open",
	"outerHeight",
	"outerWidth",
	"pageXOffset",
	"pageYOffset",
	"parent",
	"postMessage",
	"print",
	"prompt",
	"removeEventListener",
	"requestAnimationFrame",
	"resizeBy",
	"resizeTo",
	"screen",
	"screenLeft",
	"screenTop",
	"screenX",
	"screenY",
	"scroll",
	"scrollBy",
	"scrollLeft",
	"scrollTo",
	"scrollTop",
	"scrollX",
	"scrollY",
	"self",
	"sessionStorage",
	"stop",
	"top",
	"Window",
	"window"
];
const KEYS = LIVING_KEYS.concat(OTHER_KEYS);

const skipKeys = [
	"window",
	"self",
	"top",
	"parent"
];
function getWindowKeys(global, win, additionalKeys = []) {
	const keysArray = [...additionalKeys, ...KEYS];
	return new Set(keysArray.concat(Object.getOwnPropertyNames(win)).filter((k) => {
		if (skipKeys.includes(k)) return false;
		if (k in global) return keysArray.includes(k);
		return true;
	}));
}
function isClassLikeName(name) {
	return name[0] === name[0].toUpperCase();
}
function populateGlobal(global, win, options = {}) {
	const { bindFunctions = false } = options;
	const keys = getWindowKeys(global, win, options.additionalKeys);
	const originals = /* @__PURE__ */ new Map();
	const overriddenKeys = /* @__PURE__ */ new Set([...KEYS, ...options.additionalKeys || []]);
	const overrideObject = /* @__PURE__ */ new Map();
	for (const key of keys) {
		const boundFunction = bindFunctions && typeof win[key] === "function" && !isClassLikeName(key) && win[key].bind(win);
		if (overriddenKeys.has(key) && key in global) {
			// capture the descriptor instead of the value to avoid invoking native
			// lazy getters such as Node's `localStorage`, which warns when accessed
			// without `--localstorage-file`
			const descriptor = Object.getOwnPropertyDescriptor(global, key) ?? {
				value: global[key],
				configurable: true,
				writable: true,
				enumerable: true
			};
			originals.set(key, descriptor);
		}
		Object.defineProperty(global, key, {
			get() {
				if (overrideObject.has(key)) return overrideObject.get(key);
				if (boundFunction) return boundFunction;
				return win[key];
			},
			set(v) {
				overrideObject.set(key, v);
				// propagate changes to underlying window implementation,
				// which can affect other window API behavior internally, e.g.
				// updating `innerWidth` affects `matchMedia("(max-width: *)")` on happy-dom.
				win[key] = v;
			},
			configurable: true
		});
	}
	global.window = global;
	global.self = global;
	global.top = global;
	global.parent = global;
	if (global.global) global.global = global;
	// rewrite defaultView to reference the same global context
	if (global.document && global.document.defaultView) Object.defineProperty(global.document, "defaultView", {
		get: () => global,
		enumerable: true,
		configurable: true
	});
	skipKeys.forEach((k) => keys.add(k));
	return {
		keys,
		skipKeys,
		originals
	};
}

var edge = {
	name: "edge-runtime",
	viteEnvironment: "ssr",
	async setupVM() {
		const { EdgeVM } = await import('@edge-runtime/vm');
		const vm = new EdgeVM({ extend: (context) => {
			context.global = context;
			context.Buffer = Buffer;
			return context;
		} });
		return {
			getVmContext() {
				return vm.context;
			},
			teardown() {
				// nothing to teardown
			}
		};
	},
	async setup(global) {
		const { EdgeVM } = await import('@edge-runtime/vm');
		const vm = new EdgeVM({ extend: (context) => {
			context.global = context;
			context.Buffer = Buffer;
			KEYS.forEach((key) => {
				if (key in global) context[key] = global[key];
			});
			return context;
		} });
		const { keys, originals } = populateGlobal(global, vm.context, { bindFunctions: true });
		return { teardown(global) {
			keys.forEach((key) => delete global[key]);
			originals.forEach((d, k) => Object.defineProperty(global, k, d));
		} };
	}
};

async function teardownWindow(win) {
	if (win.close && win.happyDOM.abort) {
		await win.happyDOM.abort();
		win.close();
	} else win.happyDOM.cancelAsync();
}
var happy = {
	name: "happy-dom",
	viteEnvironment: "client",
	async setupVM({ happyDOM = {} }) {
		const { Window } = await import('happy-dom');
		let win = new Window({
			...happyDOM,
			console: console && globalThis.console ? globalThis.console : void 0,
			url: happyDOM.url || "http://localhost:3000",
			settings: {
				...happyDOM.settings,
				disableErrorCapturing: true
			}
		});
		// TODO: browser doesn't expose Buffer, but a lot of dependencies use it
		win.Buffer = Buffer;
		// inject structuredClone if it exists
		if (typeof structuredClone !== "undefined" && !win.structuredClone) win.structuredClone = structuredClone;
		return {
			getVmContext() {
				return win;
			},
			async teardown() {
				await teardownWindow(win);
				win = void 0;
			}
		};
	},
	async setup(global, { happyDOM = {} }) {
		// happy-dom v3 introduced a breaking change to Window, but
		// provides GlobalWindow as a way to use previous behaviour
		const { Window, GlobalWindow } = await import('happy-dom');
		const win = new (GlobalWindow || Window)({
			...happyDOM,
			console: console && global.console ? global.console : void 0,
			url: happyDOM.url || "http://localhost:3000",
			settings: {
				...happyDOM.settings,
				disableErrorCapturing: true
			}
		});
		const { keys, originals } = populateGlobal(global, win, {
			bindFunctions: true,
			// jsdom doesn't support fetch API, but happy-dom does
			additionalKeys: [
				"Request",
				"Response",
				"MessagePort",
				"fetch",
				"Headers",
				"AbortController",
				"AbortSignal",
				"URL",
				"URLSearchParams",
				"FormData"
			]
		});
		return { async teardown(global) {
			await teardownWindow(win);
			keys.forEach((key) => delete global[key]);
			originals.forEach((d, k) => Object.defineProperty(global, k, d));
		} };
	}
};

function catchWindowErrors(window) {
	let userErrorListenerCount = 0;
	function throwUnhandlerError(e) {
		if (userErrorListenerCount === 0 && e.error != null) {
			e.preventDefault();
			denoVitestReportUnhandledError(e.error);
		}
	}
	const addEventListener = window.addEventListener.bind(window);
	const removeEventListener = window.removeEventListener.bind(window);
	window.addEventListener("error", throwUnhandlerError);
	window.addEventListener = function(...args) {
		if (args[0] === "error") userErrorListenerCount++;
		return addEventListener.apply(this, args);
	};
	window.removeEventListener = function(...args) {
		if (args[0] === "error" && userErrorListenerCount) userErrorListenerCount--;
		return removeEventListener.apply(this, args);
	};
	return function clearErrorHandlers() {
		window.removeEventListener("error", throwUnhandlerError);
	};
}
let NodeFormData_;
let NodeBlob_;
let NodeRequest_;
function getResourceOptions(jsdom, resources, userAgent) {
	const ResourceLoader = jsdom.ResourceLoader;
	// jsdom 28 replaced ResourceLoader with a resources options object.
	if (!ResourceLoader) return { resources: userAgent ? { userAgent } : resources };
	return {
		resources: resources ?? (userAgent ? new ResourceLoader({ userAgent }) : void 0),
		userAgent
	};
}
var jsdom = {
	name: "jsdom",
	viteEnvironment: "client",
	async setupVM({ jsdom = {} }) {
		// delay initialization because it takes ~1s
		NodeFormData_ = globalThis.FormData;
		NodeBlob_ = globalThis.Blob;
		NodeRequest_ = globalThis.Request;
		const jsdomModule = await import('jsdom');
		const { CookieJar, JSDOM, VirtualConsole } = jsdomModule;
		const { html = "<!DOCTYPE html>", userAgent, url = "http://localhost:3000", contentType = "text/html", pretendToBeVisual = true, includeNodeLocations = false, runScripts = "dangerously", resources, console = false, cookieJar = false, ...restOptions } = jsdom;
		let virtualConsole;
		if (console && globalThis.console) {
			virtualConsole = new VirtualConsole();
			// jsdom <27
			if ("sendTo" in virtualConsole) virtualConsole.sendTo(globalThis.console);
			else virtualConsole.forwardTo(globalThis.console);
		}
		let dom = new JSDOM(html, {
			pretendToBeVisual,
			runScripts,
			url,
			virtualConsole,
			cookieJar: cookieJar ? new CookieJar() : void 0,
			includeNodeLocations,
			contentType,
			...getResourceOptions(jsdomModule, resources, userAgent),
			...restOptions
		});
		const clearAddEventListenerPatch = patchAddEventListener(dom.window);
		const clearWindowErrors = catchWindowErrors(dom.window);
		const utils = createCompatUtils(dom.window);
		// TODO: browser doesn't expose Buffer, but a lot of dependencies use it
		dom.window.Buffer = Buffer;
		dom.window.jsdom = dom;
		dom.window.Request = createCompatRequest(utils);
		dom.window.URL = createJSDOMCompatURL(utils);
		for (const name of [
			"structuredClone",
			"BroadcastChannel",
			"MessageChannel",
			"MessagePort",
			"TextEncoder",
			"TextDecoder"
		]) {
			const value = globalThis[name];
			if (typeof value !== "undefined" && typeof dom.window[name] === "undefined") dom.window[name] = value;
		}
		for (const name of [
			"fetch",
			"Response",
			"Headers",
			"AbortController",
			"AbortSignal",
			"URLSearchParams"
		]) {
			const value = globalThis[name];
			if (typeof value !== "undefined") dom.window[name] = value;
		}
		return {
			getVmContext() {
				return dom.getInternalVMContext();
			},
			teardown() {
				clearAddEventListenerPatch();
				clearWindowErrors();
				dom.window.close();
				dom = void 0;
			}
		};
	},
	async setup(global, { jsdom = {} }) {
		// delay initialization because it takes ~1s
		NodeFormData_ = globalThis.FormData;
		NodeBlob_ = globalThis.Blob;
		NodeRequest_ = globalThis.Request;
		const jsdomModule = await import('jsdom');
		const { CookieJar, JSDOM, VirtualConsole } = jsdomModule;
		const { html = "<!DOCTYPE html>", userAgent, url = "http://localhost:3000", contentType = "text/html", pretendToBeVisual = true, includeNodeLocations = false, runScripts = "dangerously", resources, console = false, cookieJar = false, ...restOptions } = jsdom;
		let virtualConsole;
		if (console && globalThis.console) {
			virtualConsole = new VirtualConsole();
			// jsdom <27
			if ("sendTo" in virtualConsole) virtualConsole.sendTo(globalThis.console);
			else virtualConsole.forwardTo(globalThis.console);
		}
		const dom = new JSDOM(html, {
			pretendToBeVisual,
			runScripts,
			url,
			virtualConsole,
			cookieJar: cookieJar ? new CookieJar() : void 0,
			includeNodeLocations,
			contentType,
			...getResourceOptions(jsdomModule, resources, userAgent),
			...restOptions
		});
		const clearAddEventListenerPatch = patchAddEventListener(dom.window);
		const { keys, originals } = populateGlobal(global, dom.window, { bindFunctions: true });
		const clearWindowErrors = catchWindowErrors(global);
		const utils = createCompatUtils(dom.window);
		global.jsdom = dom;
		global.Request = createCompatRequest(utils);
		global.URL = createJSDOMCompatURL(utils);
		return { teardown(global) {
			clearAddEventListenerPatch();
			clearWindowErrors();
			dom.window.close();
			delete global.jsdom;
			keys.forEach((key) => delete global[key]);
			originals.forEach((d, k) => Object.defineProperty(global, k, d));
		} };
	}
};
function createCompatRequest(utils) {
	class Request extends NodeRequest_ {
		constructor(...args) {
			const [input, init] = args;
			if (init?.body != null) {
				const compatInit = { ...init };
				if (init.body instanceof utils.window.Blob) compatInit.body = utils.makeCompatBlob(init.body);
				if (init.body instanceof utils.window.FormData) compatInit.body = utils.makeCompatFormData(init.body);
				super(input, compatInit);
			} else super(...args);
		}
		static [Symbol.hasInstance](instance) {
			return instance instanceof NodeRequest_;
		}
	}
	return Request;
}
function createJSDOMCompatURL(utils) {
	class URL extends URL$1 {
		static createObjectURL(blob) {
			if (blob instanceof utils.window.Blob) {
				const compatBlob = utils.makeCompatBlob(blob);
				return URL$1.createObjectURL(compatBlob);
			}
			return URL$1.createObjectURL(blob);
		}
		static [Symbol.hasInstance](instance) {
			return instance instanceof URL$1;
		}
	}
	return URL;
}
// jsdom keeps Blob bytes on an internal "impl" object and exposes no synchronous
// public way to read them, so this reaches into its generated bindings
function createBlobImplGetter(window) {
	const _require = createRequire(denoVitestRootRequireBase());
	// jsdom 28.1 moved the generated bindings; jsdom has no "exports" map, so both subpaths resolve
	for (const id of ["jsdom/lib/generated/idl/utils.js", "jsdom/lib/jsdom/living/generated/utils.js"]) try {
		const { implForWrapper } = _require(id);
		if (typeof implForWrapper === "function") return implForWrapper;
	} catch {}
	// jsdom < 30.1 also stores the impl under an own Symbol("impl")
	const implSymbol = Object.getOwnPropertySymbols(new window.Blob())[0];
	return (blob) => blob[implSymbol];
}
let getBlobImpl;
function createCompatUtils(window) {
	getBlobImpl ??= createBlobImplGetter(window);
	const utils = {
		window,
		makeCompatFormData(formData) {
			const nodeFormData = new NodeFormData_();
			formData.forEach((value, key) => {
				if (value instanceof window.Blob) nodeFormData.append(key, utils.makeCompatBlob(value));
				else nodeFormData.append(key, value);
			});
			return nodeFormData;
		},
		makeCompatBlob(blob) {
			const impl = getBlobImpl(blob);
			if (!impl) throw new TypeError("Vitest cannot read the bytes of a jsdom Blob. This is a Vitest bug, please report it with your jsdom version.");
			// jsdom 28 renamed `_buffer` to `_bytes`
			return new NodeBlob_([impl._bytes ?? impl._buffer], { type: blob.type });
		}
	};
	return utils;
}
function patchAddEventListener(window) {
	const abortControllers = /* @__PURE__ */ new WeakMap();
	const JSDOMAbortSignal = window.AbortSignal;
	const JSDOMAbortController = window.AbortController;
	const originalAddEventListener = window.EventTarget.prototype.addEventListener;
	function getJsdomAbortController(signal) {
		if (!abortControllers.has(signal)) {
			const jsdomAbortController = new JSDOMAbortController();
			signal.addEventListener("abort", () => {
				jsdomAbortController.abort(signal.reason);
			});
			abortControllers.set(signal, jsdomAbortController);
		}
		return abortControllers.get(signal);
	}
	window.EventTarget.prototype.addEventListener = function addEventListener(type, callback, options) {
		if (typeof options === "object" && options?.signal != null) {
			const { signal, ...otherOptions } = options;
			// - this happens because AbortSignal is provided by Node.js,
			// but jsdom APIs require jsdom's AbortSignal, while Node APIs
			// (like fetch and Request) require a Node.js AbortSignal
			// - disable narrow typing with "as any" because we need it later
			if (!(signal instanceof JSDOMAbortSignal)) {
				const jsdomCompatOptions = Object.create(null);
				Object.assign(jsdomCompatOptions, otherOptions);
				jsdomCompatOptions.signal = getJsdomAbortController(signal).signal;
				return originalAddEventListener.call(this, type, callback, jsdomCompatOptions);
			}
		}
		return originalAddEventListener.call(this, type, callback, options);
	};
	return () => {
		window.EventTarget.prototype.addEventListener = originalAddEventListener;
	};
}

// some globals we do not want, either because deprecated or we set it ourselves
const denyList = /* @__PURE__ */ new Set([
	"GLOBAL",
	"root",
	"global",
	"Buffer",
	"ArrayBuffer",
	"Uint8Array"
]);
const nodeGlobals = /* @__PURE__ */ new Map();
function populateNodeGlobals() {
	if (nodeGlobals.size !== 0) return;
	const names = Object.getOwnPropertyNames(globalThis);
	const length = names.length;
	for (let i = 0; i < length; i++) {
		const globalName = names[i];
		if (!denyList.has(globalName)) {
			const descriptor = Object.getOwnPropertyDescriptor(globalThis, globalName);
			if (!descriptor) throw new Error(`No property descriptor for ${globalName}, this is a bug in Vitest.`);
			nodeGlobals.set(globalName, descriptor);
		}
	}
}
var node = {
	name: "node",
	viteEnvironment: "ssr",
	prewarmModules: false,
	// this is largely copied from jest's node environment
	async setupVM() {
		populateNodeGlobals();
		const vm = await import('node:vm');
		let context = vm.createContext();
		let global = vm.runInContext("this", context);
		const contextGlobals = new Set(Object.getOwnPropertyNames(global));
		for (const [nodeGlobalsKey, descriptor] of nodeGlobals) if (!contextGlobals.has(nodeGlobalsKey)) if (descriptor.configurable) Object.defineProperty(global, nodeGlobalsKey, {
			configurable: true,
			enumerable: descriptor.enumerable,
			get() {
				// @ts-expect-error: no index signature
				const val = globalThis[nodeGlobalsKey];
				// override lazy getter
				Object.defineProperty(global, nodeGlobalsKey, {
					configurable: true,
					enumerable: descriptor.enumerable,
					value: val,
					writable: descriptor.writable === true || nodeGlobalsKey === "performance"
				});
				return val;
			},
			set(val) {
				// override lazy getter
				Object.defineProperty(global, nodeGlobalsKey, {
					configurable: true,
					enumerable: descriptor.enumerable,
					value: val,
					writable: true
				});
			}
		});
		else if ("value" in descriptor) Object.defineProperty(global, nodeGlobalsKey, {
			configurable: false,
			enumerable: descriptor.enumerable,
			value: descriptor.value,
			writable: descriptor.writable
		});
		else Object.defineProperty(global, nodeGlobalsKey, {
			configurable: false,
			enumerable: descriptor.enumerable,
			get: descriptor.get,
			set: descriptor.set
		});
		global.global = global;
		global.Buffer = Buffer;
		global.ArrayBuffer = ArrayBuffer;
		// TextEncoder (global or via 'util') references a Uint8Array constructor
		// different than the global one used by users in tests. This makes sure the
		// same constructor is referenced by both.
		global.Uint8Array = Uint8Array;
		return {
			getVmContext() {
				return context;
			},
			teardown() {
				context = void 0;
				global = void 0;
			}
		};
	},
	async setup(global) {
		global.console.Console = Console;
		return { teardown(global) {
			delete global.console.Console;
		} };
	}
};

const environments = {
	node,
	jsdom,
	"happy-dom": happy,
	"edge-runtime": edge
};

export { environments, populateGlobal };
