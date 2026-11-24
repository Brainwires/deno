// Copyright 2018-2026 the Deno authors. MIT license.

// Identity for WebRTC 1.0 (W3C webrtc-identity; RFC 8827 section 7): the
// identity provider (IdP) members of `RTCPeerConnection`
// (`setIdentityProvider()`, `getIdentityAssertion()`, `peerIdentity`,
// `idpLoginUrl`, `idpErrorInfo`), `RTCIdentityAssertion`, the `a=identity`
// attribute of offers and answers and its validation in
// `setRemoteDescription()`, and `MediaStreamTrack`'s isolation attributes.
//
// The IdP proxy is the script at
// `https://<domain>/.well-known/idp-proxy/<protocol>`. The runtime fetches it
// (the net permission applies, like any fetch) and runs it in a worker of
// its own, with no permission at all and the script's URL as its location
// and origin: an `RTCIdentityProviderGlobalScope` whose
// `rtcIdentityProvider.register()` takes the IdP's two callbacks. Each
// generation or validation gets a fresh worker, terminated when it answers
// or after the 15 seconds the spec allows (section 9.5).
//
// 01_webrtc.js loads this module and calls `installIdentity()` once; nothing
// here runs until an application calls one of the members.

/// <reference path="../../core/internal.d.ts" />

(function () {
const { core, internals: runtimeInternals, primordials } = __bootstrap;
const {
  op_base64_encode_from_buffer,
  op_webrtc_pc_create_answer,
  op_webrtc_pc_create_offer,
} = core.ops;
const {
  ArrayIsArray,
  ArrayPrototypeIncludes,
  ArrayPrototypeJoin,
  ArrayPrototypeMap,
  ArrayPrototypePush,
  ArrayPrototypeSort,
  FunctionPrototypeCall,
  JSONParse,
  JSONStringify,
  ObjectDefineProperty,
  ObjectGetOwnPropertyDescriptor,
  Promise,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  SafeSet,
  SafeWeakMap,
  SetPrototypeAdd,
  SetPrototypeHas,
  String,
  StringPrototypeCharCodeAt,
  StringPrototypeEndsWith,
  StringPrototypeIncludes,
  StringPrototypeIndexOf,
  StringPrototypeLastIndexOf,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  StringPrototypeToUpperCase,
  StringPrototypeTrim,
  Symbol,
  TypeError,
  Uint8Array,
  WeakMapPrototypeGet,
  WeakMapPrototypeSet,
} = primordials;

const webidl = core.loadExtScript("ext:deno_webidl/00_webidl.js");
const { DOMException } = core.loadExtScript(
  "ext:deno_web/01_dom_exception.js",
);
const { URL } = core.loadExtScript("ext:deno_web/00_url.js");
const { atob } = core.loadExtScript("ext:deno_web/05_base64.js");
const { getLocationHref } = core.loadExtScript("ext:deno_web/12_location.js");
const { clearTimeout, setTimeout } = core.loadExtScript(
  "ext:deno_web/02_timers.js",
);
// The runtime's own fetch and Worker (not the globals an application can
// replace), loaded with the first IdP call.
const loadFetch = () => core.loadExtScript("ext:deno_fetch/26_fetch.js");
const loadWorkers = () => core.loadExtScript("ext:runtime/11_workers.js");

// W3C webrtc-identity section 9.5: "The user agent SHOULD limit the time
// that it allows for an IdP to 15 seconds", loading included.
const IDP_TIMEOUT_MS = 15000;

// The error details an IdP can report itself (section 9.5); any other
// failure of its callbacks is "idp-execution-failure".
const IDP_REPORTED_DETAILS = [
  "idp-need-login",
  "idp-token-expired",
  "idp-token-invalid",
];

const MULTIPLE = Symbol("several a=identity values");

function noop() {}

// denext: an IdP proxy runs in a worker (`callIdp`), which is
// `deno_runtime`'s. A runtime made without it (ext/web_page: one that runs
// web pages on a plain `JsRuntime`) sets `internals.webrtcIdentityUnavailable`
// before it loads WebRTC: there `setIdentityProvider()` throws,
// `getIdentityAssertion()` and `peerIdentity` reject and a remote
// description's `a=identity` fails to validate, all with a
// `NotSupportedError`, and no IdP proxy is fetched. Deno never sets it.
function identityUnavailable() {
  return runtimeInternals.webrtcIdentityUnavailable === true;
}

function identityNotSupported() {
  return new DOMException(
    "WebRTC identity is not supported: this runtime has no worker to run an identity provider in",
    "NotSupportedError",
  );
}

function operationError(message) {
  return new DOMException(message, "OperationError");
}

function errorMessage(error) {
  try {
    const message = error?.message;
    return message === undefined ? String(error) : String(message);
  } catch {
    return "unknown error";
  }
}

// The worker side of the IdP proxy: run as a script (the IdP's URL is the
// worker's location), it installs `rtcIdentityProvider`, evaluates the IdP's
// script (passed as its argument), reports whether it registered, then
// answers one request at a time. `location` is a plain object with the URL's
// parts, as Firefox gives its IdP sandbox (so an IdP can serialize it). The
// worker's `postMessage` is hidden from the IdP, and the proxy's own message
// listener, added first, stops every message from reaching the IdP's: the
// IdP talks to the browser only through its registered callbacks.
const PROXY_BOOTSTRAP = `(function (idpSource) {
  "use strict";
  var g = globalThis;
  var apply = Reflect.apply;
  var defineProperty = Object.defineProperty;
  var freeze = Object.freeze;
  var post = g.postMessage;
  var addListener = g.addEventListener;
  var indirectEval = g.eval;
  var PromiseCtor = Promise;
  var then = Promise.prototype.then;
  var toString = String;
  var url = new URL(g.location.href);
  var registered = null;
  var evaluating = true;
  var registrarKey = {};
  var registrar = null;
  function send(message) {
    apply(post, g, [message]);
  }
  function isObject(value) {
    return value !== null &&
      (typeof value === "object" || typeof value === "function");
  }
  function domString(value, name) {
    if (typeof value === "symbol") {
      throw new TypeError(name + " can't be a symbol");
    }
    return toString(value);
  }
  function dictionary(value, name) {
    if (value === undefined || value === null) return {};
    if (!isObject(value)) throw new TypeError(name + " is not a dictionary");
    return value;
  }
  function required(dict, key, name) {
    var value = dict[key];
    if (value === undefined) {
      throw new TypeError(name + ": '" + key + "' is required");
    }
    return value;
  }
  function assertionResult(value) {
    var name = "RTCIdentityAssertionResult";
    var dict = dictionary(value, name);
    var assertion = domString(required(dict, "assertion", name), name);
    var details = dictionary(required(dict, "idp", name),
      "RTCIdentityProviderDetails");
    var domain = domString(
      required(details, "domain", "RTCIdentityProviderDetails"), "domain");
    var protocol = details.protocol;
    protocol = protocol === undefined
      ? "default"
      : domString(protocol, "protocol");
    return { idp: { domain: domain, protocol: protocol }, assertion: assertion };
  }
  function validationResult(value) {
    var name = "RTCIdentityValidationResult";
    var dict = dictionary(value, name);
    var contents = domString(required(dict, "contents", name), name);
    var identity = domString(required(dict, "identity", name), name);
    return { identity: identity, contents: contents };
  }
  function describe(error) {
    var out = {
      message: "",
      errorDetail: null,
      idpErrorInfo: null,
      idpLoginUrl: null,
    };
    try {
      if (isObject(error)) {
        if (error.message !== undefined) {
          out.message = domString(error.message, "message");
        }
        if (typeof error.errorDetail === "string") {
          out.errorDetail = error.errorDetail;
        }
        if (error.idpErrorInfo !== undefined && error.idpErrorInfo !== null) {
          out.idpErrorInfo = domString(error.idpErrorInfo, "idpErrorInfo");
        }
        if (error.idpLoginUrl !== undefined && error.idpLoginUrl !== null) {
          out.idpLoginUrl = domString(error.idpLoginUrl, "idpLoginUrl");
        }
      } else {
        out.message = domString(error, "error");
      }
    } catch (_) {
      // An error that can't be described is still an error.
    }
    return out;
  }
  class RTCIdentityProviderRegistrar {
    constructor(key) {
      if (key !== registrarKey) throw new TypeError("Illegal constructor");
    }
    register(idp) {
      var prefix =
        "Failed to execute 'register' on 'RTCIdentityProviderRegistrar'";
      if (this !== registrar) throw new TypeError("Illegal invocation");
      if (arguments.length < 1) {
        throw new TypeError(prefix + ": 1 argument required, but only 0 present");
      }
      var dict = dictionary(idp, prefix + ": 'idp'");
      var generateAssertion = required(dict, "generateAssertion", prefix);
      if (typeof generateAssertion !== "function") {
        throw new TypeError(prefix + ": 'generateAssertion' is not a function");
      }
      var validateAssertion = required(dict, "validateAssertion", prefix);
      if (typeof validateAssertion !== "function") {
        throw new TypeError(prefix + ": 'validateAssertion' is not a function");
      }
      if (evaluating) {
        registered = {
          generateAssertion: generateAssertion,
          validateAssertion: validateAssertion,
        };
      }
    }
  }
  registrar = new RTCIdentityProviderRegistrar(registrarKey);
  defineProperty(g, "RTCIdentityProviderRegistrar", {
    value: RTCIdentityProviderRegistrar,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  defineProperty(g, "rtcIdentityProvider", {
    get: function () {
      return registrar;
    },
    enumerable: true,
    configurable: true,
  });
  var plainLocation = {
    href: url.href,
    origin: url.origin,
    protocol: url.protocol,
    host: url.host,
    hostname: url.hostname,
    port: url.port,
    pathname: url.pathname,
    search: url.search,
    hash: url.hash,
  };
  defineProperty(plainLocation, "toString", {
    value: function () {
      return url.href;
    },
  });
  function shadow(name, value) {
    try {
      defineProperty(g, name, {
        value: value,
        writable: false,
        enumerable: true,
        configurable: true,
      });
    } catch (_) {
      // A property the runtime made non-configurable stays.
    }
  }
  shadow("location", freeze(plainLocation));
  shadow("postMessage", undefined);
  apply(addListener, g, ["message", function (event) {
    event.stopImmediatePropagation();
    var request = event.data;
    if (!isObject(request) || request.id === undefined) return;
    var id = request.id;
    var generate = request.op === "generate";
    var callback = generate
      ? registered.generateAssertion
      : registered.validateAssertion;
    var args = generate
      ? [request.contents, request.origin, request.options]
      : [request.assertion, request.origin];
    var promise = new PromiseCtor(function (resolve) {
      resolve(apply(callback, undefined, args));
    });
    apply(then, promise, [function (value) {
      var result;
      try {
        result = generate ? assertionResult(value) : validationResult(value);
      } catch (error) {
        send({ id: id, ok: false, kind: "invalid", error: describe(error) });
        return;
      }
      send({ id: id, ok: true, result: result });
    }, function (error) {
      send({ id: id, ok: false, kind: "execution", error: describe(error) });
    }]);
  }]);
  try {
    indirectEval(idpSource);
  } catch (error) {
    evaluating = false;
    send({ loaded: false, message: describe(error).message });
    return;
  }
  evaluating = false;
  send({
    loaded: registered !== null,
    message: registered === null
      ? "The IdP proxy did not call rtcIdentityProvider.register()"
      : "",
  });
})(`;

function proxySource(idpSource) {
  return PROXY_BOOTSTRAP + JSONStringify(idpSource) + ");\n";
}

function idpError(RTCError, detail, message, httpRequestStatusCode) {
  const init = { errorDetail: detail };
  if (httpRequestStatusCode !== undefined) {
    init.httpRequestStatusCode = httpRequestStatusCode;
  }
  return new RTCError(init, message);
}

// A TLS failure of the IdP load: the fetch's error names the certificate or
// the handshake.
function isTlsFailure(error) {
  const message = StringPrototypeToLowerCase(errorMessage(error));
  return StringPrototypeIncludes(message, "certificate") ||
    StringPrototypeIncludes(message, "handshake");
}

// The serialized origin of the RTCPeerConnection's realm (the `origin`
// argument of both callbacks, section 9.2.1).
function realmOrigin() {
  const href = getLocationHref();
  if (href === undefined || href === null) return "null";
  try {
    return new URL(href).origin;
  } catch {
    return "null";
  }
}

// An IdP domain is a host, with an optional port (RFC 8827 section 7.5):
// nothing that would make the URL's userinfo, path, query or fragment.
function isIdpDomain(domain) {
  for (let i = 0; i < domain.length; ++i) {
    const c = domain[i];
    if (c === "/" || c === "\\" || c === "?" || c === "#" || c === "@") {
      return false;
    }
  }
  return domain.length > 0 && hostOf(domain) !== null;
}

// The DOMString host of a domain as the URL parser reads it (lower case,
// IDNA, no port), or null.
function hostOf(domain) {
  try {
    return new URL(`https://${domain}/`).hostname;
  } catch {
    return null;
  }
}

function toBase64Json(value) {
  const bytes = core.encode(JSONStringify(value));
  return op_base64_encode_from_buffer(bytes, 0, bytes.length);
}

function fromBase64Json(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; ++i) {
    bytes[i] = StringPrototypeCharCodeAt(binary, i);
  }
  return JSONParse(core.decode(bytes));
}

// --- SDP -------------------------------------------------------------------

// Every a=fingerprint of a description (session and media level), once
// each: `{ algorithm, digest }`, the algorithm in lower case and the digest
// in upper case (RFC 8122 section 5 compares both case-insensitively).
function fingerprintsOf(sdp) {
  const out = [];
  const seen = new SafeSet();
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (!StringPrototypeStartsWith(line, "a=fingerprint:")) continue;
    const rest = StringPrototypeTrim(StringPrototypeSlice(line, 14));
    const space = StringPrototypeIndexOf(rest, " ");
    if (space <= 0) continue;
    const algorithm = StringPrototypeToLowerCase(
      StringPrototypeSlice(rest, 0, space),
    );
    const digest = StringPrototypeToUpperCase(
      StringPrototypeTrim(StringPrototypeSlice(rest, space + 1)),
    );
    const key = `${algorithm} ${digest}`;
    if (SetPrototypeHas(seen, key)) continue;
    SetPrototypeAdd(seen, key);
    ArrayPrototypePush(out, { algorithm, digest });
  }
  return out;
}

function fingerprintKey(fingerprints) {
  const keys = ArrayPrototypeMap(
    fingerprints,
    (f) => `${f.algorithm} ${f.digest}`,
  );
  ArrayPrototypeSort(keys);
  return ArrayPrototypeJoin(keys, "\n");
}

// The a=identity value of a description (RFC 8827 section 7.2: the
// assertion is the token before any extension), null when there is none,
// MULTIPLE when it has different ones.
function identityAttributeOf(sdp) {
  let value = null;
  const lines = StringPrototypeSplit(sdp, "\n");
  for (let i = 0; i < lines.length; ++i) {
    const line = StringPrototypeTrim(lines[i]);
    if (!StringPrototypeStartsWith(line, "a=identity:")) continue;
    let token = StringPrototypeSlice(line, 11);
    const space = StringPrototypeIndexOf(token, " ");
    if (space !== -1) token = StringPrototypeSlice(token, 0, space);
    if (value !== null && value !== token) return MULTIPLE;
    value = token;
  }
  return value;
}

function lineEnding(sdp) {
  return StringPrototypeIncludes(sdp, "\r\n") ? "\r\n" : "\n";
}

// Adds the session-level `a=identity:<value>` line (RFC 8827 section 7.2),
// before the first media section.
function addIdentityLine(sdp, value) {
  const eol = lineEnding(sdp);
  const line = `a=identity:${value}`;
  const at = StringPrototypeIndexOf(sdp, `${eol}m=`);
  if (at === -1) {
    return StringPrototypeEndsWith(sdp, eol)
      ? `${sdp}${line}${eol}`
      : `${sdp}${eol}${line}${eol}`;
  }
  return `${StringPrototypeSlice(sdp, 0, at)}${eol}${line}${
    StringPrototypeSlice(sdp, at)
  }`;
}

function removeIdentityLines(sdp) {
  const eol = lineEnding(sdp);
  const lines = StringPrototypeSplit(sdp, eol);
  const kept = [];
  for (let i = 0; i < lines.length; ++i) {
    if (!StringPrototypeStartsWith(lines[i], "a=identity:")) {
      ArrayPrototypePush(kept, lines[i]);
    }
  }
  return ArrayPrototypeJoin(kept, eol);
}

// --- install -----------------------------------------------------------------

function installIdentity(internals) {
  const {
    _rid,
    _state,
    brandError,
    defineHandler,
    invalidState,
    MediaStreamTrackPrototype,
    RTCError,
    RTCPeerConnectionPrototype,
  } = internals;
  const localDescriptionGetter = ObjectGetOwnPropertyDescriptor(
    RTCPeerConnectionPrototype,
    "localDescription",
  ).get;

  // --- RTCIdentityAssertion (section 8.3) -----------------------------------

  class RTCIdentityAssertion {
    #idp;
    #name;

    constructor(idp, name) {
      const prefix = "Failed to construct 'RTCIdentityAssertion'";
      webidl.requiredArguments(arguments.length, 2, prefix);
      this.#idp = webidl.converters.DOMString(idp, prefix, "Argument 1");
      this.#name = webidl.converters.DOMString(name, prefix, "Argument 2");
      this[webidl.brand] = webidl.brand;
    }

    get idp() {
      webidl.assertBranded(this, RTCIdentityAssertionPrototype);
      return this.#idp;
    }
    set idp(value) {
      webidl.assertBranded(this, RTCIdentityAssertionPrototype);
      this.#idp = webidl.converters.DOMString(
        value,
        "Failed to set 'idp' on 'RTCIdentityAssertion'",
        "Value",
      );
    }
    get name() {
      webidl.assertBranded(this, RTCIdentityAssertionPrototype);
      return this.#name;
    }
    set name(value) {
      webidl.assertBranded(this, RTCIdentityAssertionPrototype);
      this.#name = webidl.converters.DOMString(
        value,
        "Failed to set 'name' on 'RTCIdentityAssertion'",
        "Value",
      );
    }
  }
  webidl.configureInterface(RTCIdentityAssertion);
  const RTCIdentityAssertionPrototype = RTCIdentityAssertion.prototype;

  // --- per-connection state ------------------------------------------------

  const identities = new SafeWeakMap();

  function existingIdentity(pc) {
    return WeakMapPrototypeGet(identities, pc);
  }

  function identityOf(pc) {
    let id = WeakMapPrototypeGet(identities, pc);
    if (id === undefined) {
      id = {
        // setIdentityProvider()'s (provider, options), or null.
        provider: null,
        // Bumped whenever the provider changes: an assertion generated for
        // an older one is discarded (section 8.2, setIdentityProvider step 4).
        generation: 0,
        // The stored assertion: { generation, key, value }, `key` the
        // fingerprints it covers and `value` the a=identity value.
        assertion: null,
        // Every a=identity value this connection produced (they are taken
        // off again before the engine applies a local description).
        produced: new SafeSet(),
        // The a=identity value of the last local description applied.
        localValue: null,
        // A description this connection created, for the fingerprints of an
        // assertion asked for before createOffer() / createAnswer().
        fingerprintSdp: null,
        idpLoginUrl: null,
        idpErrorInfo: null,
        // The peerIdentity promise: { promise, resolve, reject, settled }.
        peer: null,
        // The validated RTCIdentityAssertion, once peerIdentity resolved.
        resolved: null,
        // Identity validations run one at a time (section 9.4 step 1).
        validation: PromiseResolve(),
      };
      WeakMapPrototypeSet(identities, pc, id);
    }
    return id;
  }

  function peerRecord(id) {
    if (id.peer === null) {
      const record = { promise: null, resolve: null, reject: null };
      record.settled = false;
      record.promise = new Promise((resolve, reject) => {
        record.resolve = resolve;
        record.reject = reject;
      });
      // A rejected peerIdentity that nobody reads is not an unhandled
      // rejection of the application's.
      PromisePrototypeThen(record.promise, undefined, noop);
      id.peer = record;
    }
    return id.peer;
  }

  // The target peer identity (section 4): RTCConfiguration.peerIdentity, or
  // the identity already validated.
  function targetPeerIdentity(pc, id) {
    const configured = pc[_state].config.peerIdentity ?? null;
    if (configured !== null) return configured;
    return id?.resolved?.name ?? null;
  }

  // --- the IdP proxy ------------------------------------------------------

  // Loads the IdP proxy of (domain, protocol) in a fresh worker and makes one
  // request of it (`{ op: "generate", contents, origin, options }` or
  // `{ op: "validate", assertion, origin }`). Resolves with the converted
  // result; rejects with an RTCError for a failure of the IdP (section 9.5),
  // or an OperationError for a result that isn't the dictionary the
  // callback promised. An error the IdP reports sets the connection's
  // idpErrorInfo / idpLoginUrl.
  function callIdp(id, domain, protocol, request) {
    if (identityUnavailable()) return PromiseReject(identityNotSupported());
    return new Promise((resolve, reject) => {
      let worker = null;
      let loaded = false;
      let done = false;
      let timer = null;
      const finish = (error, value) => {
        if (done) return;
        done = true;
        if (timer !== null) clearTimeout(timer);
        if (worker !== null) worker.terminate();
        if (error !== null) {
          reject(error);
        } else {
          resolve(value);
        }
      };
      timer = setTimeout(() => {
        timer = null;
        finish(
          idpError(
            RTCError,
            "idp-timeout",
            "The identity provider did not answer in 15 seconds",
          ),
          undefined,
        );
      }, IDP_TIMEOUT_MS);

      const startProxy = (url, source) => {
        const { internalWorkerOptions, Worker } = loadWorkers();
        try {
          worker = new Worker(url, {
            type: "module",
            name: "RTCIdentityProvider",
            [internalWorkerOptions]: {
              sourceCode: proxySource(source),
              noPermissions: true,
            },
          });
        } catch (error) {
          finish(
            idpError(
              RTCError,
              "idp-load-failure",
              `The identity provider proxy can't start: ${errorMessage(error)}`,
            ),
            undefined,
          );
          return;
        }
        worker.addEventListener("error", (event) => {
          event.preventDefault();
          finish(
            idpError(
              RTCError,
              loaded ? "idp-execution-failure" : "idp-bad-script-failure",
              `The identity provider proxy failed: ${event.message ?? ""}`,
            ),
            undefined,
          );
        });
        worker.addEventListener("messageerror", () => {
          finish(
            idpError(
              RTCError,
              "idp-execution-failure",
              "The identity provider proxy sent a message that can't be read",
            ),
            undefined,
          );
        });
        worker.addEventListener("message", (event) => {
          const data = event.data;
          if (data === null || typeof data !== "object") return;
          if (!loaded) {
            if (data.loaded !== true) {
              finish(
                idpError(
                  RTCError,
                  "idp-bad-script-failure",
                  `The identity provider proxy is not usable: ${
                    data.message ?? ""
                  }`,
                ),
                undefined,
              );
              return;
            }
            loaded = true;
            const message = {
              id: 1,
              op: request.op,
              origin: request.origin,
            };
            if (request.op === "generate") {
              message.contents = request.contents;
              message.options = request.options;
            } else {
              message.assertion = request.assertion;
            }
            worker.postMessage(message);
            return;
          }
          if (data.id !== 1) return;
          if (data.ok === true) {
            finish(null, data.result);
            return;
          }
          const info = data.error ?? {};
          if (data.kind === "invalid") {
            finish(
              operationError(
                `The identity provider's result is not valid: ${
                  info.message ?? ""
                }`,
              ),
              undefined,
            );
            return;
          }
          let detail = "idp-execution-failure";
          if (
            typeof info.errorDetail === "string" &&
            ArrayPrototypeIncludes(IDP_REPORTED_DETAILS, info.errorDetail)
          ) {
            detail = info.errorDetail;
          }
          if (typeof info.idpErrorInfo === "string") {
            id.idpErrorInfo = info.idpErrorInfo;
          }
          if (
            detail === "idp-need-login" && typeof info.idpLoginUrl === "string"
          ) {
            id.idpLoginUrl = info.idpLoginUrl;
          }
          finish(
            idpError(
              RTCError,
              detail,
              `The identity provider failed: ${info.message ?? ""}`,
            ),
            undefined,
          );
        });
      };

      let href;
      try {
        if (!isIdpDomain(domain)) throw new TypeError("not a domain");
        href = new URL(`https://${domain}/.well-known/idp-proxy/${protocol}`)
          .href;
      } catch {
        finish(
          idpError(
            RTCError,
            "idp-load-failure",
            `'${domain}' and '${protocol}' don't make an IdP proxy URL`,
          ),
          undefined,
        );
        return;
      }
      let response;
      PromisePrototypeThen(
        loadFetch().fetch(href, { redirect: "follow" }),
        (value) => {
          response = value;
          if (done) return undefined;
          // Section 9.1.2: a redirect to another https origin is allowed,
          // to any other scheme a fatal error.
          if (!StringPrototypeStartsWith(response.url, "https:")) {
            finish(
              idpError(
                RTCError,
                "idp-load-failure",
                "The IdP proxy redirected to a URL that isn't https",
              ),
              undefined,
            );
            return PromisePrototypeThen(response.arrayBuffer(), noop, noop);
          }
          if (!response.ok) {
            finish(
              idpError(
                RTCError,
                "idp-load-failure",
                `Loading the IdP proxy failed with HTTP ${response.status}`,
                response.status,
              ),
              undefined,
            );
            return PromisePrototypeThen(response.arrayBuffer(), noop, noop);
          }
          return PromisePrototypeThen(response.text(), (source) => {
            if (!done) startProxy(response.url, source);
          }, (error) => {
            finish(
              idpError(
                RTCError,
                "idp-load-failure",
                `Reading the IdP proxy failed: ${errorMessage(error)}`,
              ),
              undefined,
            );
          });
        },
        (error) => {
          finish(
            idpError(
              RTCError,
              isTlsFailure(error) ? "idp-tls-failure" : "idp-load-failure",
              `Loading the IdP proxy failed: ${errorMessage(error)}`,
            ),
            undefined,
          );
        },
      );
    });
  }

  // --- requesting an assertion (section 9.3) ---------------------------------

  // The fingerprints an assertion asked for outside createOffer() /
  // createAnswer() covers: the local description's, else those of a
  // description this connection created, else those of one the engine
  // creates now (the DTLS certificate is the connection's from its
  // construction, so every description carries the same).
  function fingerprintsForAssertion(pc, id) {
    let sdp = FunctionPrototypeCall(localDescriptionGetter, pc)?.sdp ??
      id.fingerprintSdp;
    if (sdp === null || sdp === undefined) {
      try {
        sdp = op_webrtc_pc_create_offer(pc[_rid], false);
      } catch {
        try {
          sdp = op_webrtc_pc_create_answer(pc[_rid]);
        } catch {
          sdp = "";
        }
      }
      id.fingerprintSdp = sdp;
    }
    return fingerprintsOf(sdp);
  }

  // The stored assertion when it was made for the current provider and
  // these fingerprints, else a new one from the IdP.
  function requestAssertion(pc, id, fingerprints) {
    const key = fingerprintKey(fingerprints);
    const generation = id.generation;
    const stored = id.assertion;
    if (
      stored !== null && stored.generation === generation && stored.key === key
    ) {
      return PromiseResolve(stored);
    }
    const provider = id.provider;
    // RTCIdentityProviderOptions with its defaults (section 8.4); a missing
    // peerIdentity is RTCConfiguration's.
    const options = { protocol: provider.protocol };
    if (provider.usernameHint !== undefined) {
      options.usernameHint = provider.usernameHint;
    }
    const peerIdentity = provider.peerIdentity ??
      pc[_state].config.peerIdentity ?? null;
    if (peerIdentity !== null) options.peerIdentity = peerIdentity;
    // RFC 8827 section 7.4: the contents bind the DTLS fingerprints.
    const contents = JSONStringify({ fingerprint: fingerprints });
    return PromisePrototypeThen(
      callIdp(id, provider.domain, provider.protocol, {
        op: "generate",
        contents,
        origin: realmOrigin(),
        options,
      }),
      (result) => {
        const value = toBase64Json({
          idp: result.idp,
          assertion: result.assertion,
        });
        const assertion = { generation, key, value };
        SetPrototypeAdd(id.produced, value);
        if (id.generation === generation) id.assertion = assertion;
        return assertion;
      },
    );
  }

  // createOffer() / createAnswer() with a provider set: the description,
  // once created, gets the a=identity line; a failed assertion rejects with
  // a new OperationError (section 9.3). Without a provider, `promise` as it
  // is.
  function withAssertion(pc, promise) {
    const id = existingIdentity(pc);
    if (id === undefined || id.provider === null) return promise;
    return PromisePrototypeThen(promise, (description) => {
      const sdp = description.sdp;
      id.fingerprintSdp = sdp;
      return PromisePrototypeThen(
        requestAssertion(pc, id, fingerprintsOf(sdp)),
        (assertion) => {
          description.sdp = addIdentityLine(sdp, assertion.value);
          return description;
        },
        (error) => {
          throw operationError(
            `The identity assertion failed: ${errorMessage(error)}`,
          );
        },
      );
    });
  }

  // setLocalDescription(): the engine applies the description without an
  // a=identity line this connection added (it only takes the descriptions
  // it created); the line is shown again in localDescription.
  function engineSdp(pc, sdp) {
    if (typeof sdp !== "string") return sdp;
    const id = existingIdentity(pc);
    if (id === undefined) return sdp;
    const value = identityAttributeOf(sdp);
    if (value === null || value === MULTIPLE) return sdp;
    if (!SetPrototypeHas(id.produced, value)) return sdp;
    id.localValue = value;
    return removeIdentityLines(sdp);
  }

  function reportedSdp(pc, slot, sdp) {
    if (slot !== "pendingLocal" && slot !== "currentLocal") return sdp;
    const id = existingIdentity(pc);
    if (id === undefined || id.localValue === null) return sdp;
    if (identityAttributeOf(sdp) !== null) return sdp;
    return addIdentityLine(sdp, id.localValue);
  }

  // --- verifying an assertion (section 9.4) ---------------------------------

  // Validates the a=identity value `attribute` of the remote description
  // `sdp`: resolves with the RTCIdentityAssertion, rejects with the IdP's
  // RTCError or an OperationError.
  function validateAssertion(id, sdp, attribute, target) {
    if (attribute === null) {
      return PromiseReject(operationError(
        "The remote description has no identity assertion, but the connection has a target peer identity",
      ));
    }
    if (attribute === MULTIPLE) {
      return PromiseReject(operationError(
        "The remote description has different identity assertions",
      ));
    }
    let domain, protocol, assertion;
    try {
      const decoded = fromBase64Json(attribute);
      const idp = decoded?.idp;
      domain = idp?.domain;
      protocol = idp?.protocol ?? "default";
      assertion = decoded?.assertion;
      if (
        typeof domain !== "string" || typeof protocol !== "string" ||
        typeof assertion !== "string" || !isIdpDomain(domain)
      ) {
        throw new TypeError("not an RTCIdentityAssertionResult");
      }
    } catch (error) {
      return PromiseReject(operationError(
        `The identity assertion is malformed: ${errorMessage(error)}`,
      ));
    }
    if (
      StringPrototypeIncludes(protocol, "/") ||
      StringPrototypeIncludes(protocol, "\\")
    ) {
      return PromiseReject(operationError(
        "The identity assertion's protocol includes '/' or '\\'",
      ));
    }
    return PromisePrototypeThen(
      callIdp(id, domain, protocol, {
        op: "validate",
        assertion,
        origin: realmOrigin(),
      }),
      (result) => {
        // Step 8: the contents cover every a=fingerprint of the description.
        let covered;
        try {
          const contents = JSONParse(result.contents);
          const list = contents?.fingerprint;
          if (!ArrayIsArray(list)) throw new TypeError("no fingerprint list");
          covered = new SafeSet();
          for (let i = 0; i < list.length; ++i) {
            const f = list[i];
            if (
              typeof f?.algorithm === "string" && typeof f?.digest === "string"
            ) {
              SetPrototypeAdd(
                covered,
                `${StringPrototypeToLowerCase(f.algorithm)} ${
                  StringPrototypeToUpperCase(f.digest)
                }`,
              );
            }
          }
        } catch (error) {
          throw operationError(
            `The identity assertion's contents are not valid: ${
              errorMessage(error)
            }`,
          );
        }
        const fingerprints = fingerprintsOf(sdp);
        for (let i = 0; i < fingerprints.length; ++i) {
          const f = fingerprints[i];
          if (!SetPrototypeHas(covered, `${f.algorithm} ${f.digest}`)) {
            throw operationError(
              "The identity assertion doesn't cover the description's fingerprints",
            );
          }
        }
        // Step 9: the domain of the identity is the IdP's (RFC 8827
        // section 7.6; the port of the IdP doesn't count).
        const identity = result.identity;
        const at = StringPrototypeLastIndexOf(identity, "@");
        const identityHost = at > 0
          ? hostOf(StringPrototypeSlice(identity, at + 1))
          : null;
        if (identityHost === null || identityHost !== hostOf(domain)) {
          throw operationError(
            `The identity '${identity}' is not of the identity provider's domain '${domain}'`,
          );
        }
        if (target !== null && identity !== target) {
          throw operationError(
            `The peer identity '${identity}' is not the target peer identity '${target}'`,
          );
        }
        return new RTCIdentityAssertion(domain, identity);
      },
    );
  }

  // Runs a validation after the previous one and settles peerIdentity with
  // its outcome.
  function queueValidation(pc, id, sdp, attribute, target) {
    const result = PromisePrototypeThen(
      id.validation,
      () => validateAssertion(id, sdp, attribute, target),
    );
    id.validation = PromisePrototypeThen(result, noop, noop);
    PromisePrototypeThen(result, (assertion) => {
      if (pc[_state].closed) return;
      const peer = peerRecord(id);
      if (!peer.settled) {
        peer.settled = true;
        id.resolved = assertion;
        peer.resolve(assertion);
      }
    }, (error) => {
      if (pc[_state].closed) return;
      const peer = peerRecord(id);
      if (!peer.settled) {
        peer.settled = true;
        peer.reject(error);
      }
      // Without a target peer identity a later description can still
      // establish one: peerIdentity becomes a new, pending promise.
      if (targetPeerIdentity(pc, id) === null) id.peer = null;
    });
    return result;
  }

  // setRemoteDescription(): the work that applies the description
  // (`apply`, run in a task), checked against its a=identity. With a target
  // peer identity the description is validated first and the operation
  // rejects with the validation's error; without one it is applied and the
  // validation runs on its own (section 9.4). With neither an a=identity nor
  // a target peer identity, `apply` itself.
  function withRemoteIdentity(pc, type, sdpValue, apply) {
    if (type === "rollback") return apply;
    const sdp = sdpValue === undefined || sdpValue === null
      ? ""
      : String(sdpValue);
    const attribute = identityAttributeOf(sdp);
    const existing = existingIdentity(pc);
    const target = targetPeerIdentity(pc, existing);
    if (attribute === null && target === null) return apply;
    const id = existing ?? identityOf(pc);
    if (target !== null) {
      return () =>
        PromisePrototypeThen(
          queueValidation(pc, id, sdp, attribute, target),
          () => {
            if (pc[_state].closed) return undefined;
            return apply();
          },
        );
    }
    return () => {
      const applied = apply();
      PromisePrototypeThen(
        queueValidation(pc, id, sdp, attribute, null),
        noop,
        noop,
      );
      return applied;
    };
  }

  // --- the RTCPeerConnection members (section 8.2) ---------------------------

  function convertProviderOptions(options, prefix) {
    if (options === undefined || options === null) {
      return {
        peerIdentity: undefined,
        protocol: "default",
        usernameHint: undefined,
      };
    }
    if (typeof options !== "object" && typeof options !== "function") {
      throw new TypeError(
        `${prefix}: 'options' is not an RTCIdentityProviderOptions dictionary`,
      );
    }
    // Members in Web IDL dictionary order.
    const peerIdentity = options.peerIdentity === undefined
      ? undefined
      : webidl.converters.DOMString(
        options.peerIdentity,
        prefix,
        "peerIdentity",
      );
    const protocol = options.protocol === undefined
      ? "default"
      : webidl.converters.DOMString(options.protocol, prefix, "protocol");
    const usernameHint = options.usernameHint === undefined
      ? undefined
      : webidl.converters.DOMString(
        options.usernameHint,
        prefix,
        "usernameHint",
      );
    return { peerIdentity, protocol, usernameHint };
  }

  const members = {
    setIdentityProvider(provider, options = undefined) {
      webidl.assertBranded(this, RTCPeerConnectionPrototype);
      if (identityUnavailable()) throw identityNotSupported();
      const prefix =
        "Failed to execute 'setIdentityProvider' on 'RTCPeerConnection'";
      webidl.requiredArguments(arguments.length, 1, prefix);
      provider = webidl.converters.DOMString(provider, prefix, "Argument 1");
      const opts = convertProviderOptions(options, prefix);
      if (this[_state].closed) {
        throw invalidState("RTCPeerConnection is closed");
      }
      if (
        StringPrototypeIncludes(opts.protocol, "/") ||
        StringPrototypeIncludes(opts.protocol, "\\")
      ) {
        throw new DOMException(
          `${prefix}: the protocol must not include '/' or '\\'`,
          "SyntaxError",
        );
      }
      const id = identityOf(this);
      const previous = id.provider;
      if (
        previous === null || previous.domain !== provider ||
        previous.protocol !== opts.protocol ||
        previous.usernameHint !== opts.usernameHint ||
        previous.peerIdentity !== opts.peerIdentity
      ) {
        id.generation++;
        id.assertion = null;
      }
      id.provider = {
        domain: provider,
        protocol: opts.protocol,
        usernameHint: opts.usernameHint,
        peerIdentity: opts.peerIdentity,
      };
    },

    getIdentityAssertion() {
      const branded = brandError(this, RTCPeerConnectionPrototype);
      if (branded !== null) return PromiseReject(branded);
      if (identityUnavailable()) return PromiseReject(identityNotSupported());
      return new Promise((resolve, reject) => {
        // "queue a task to run the following steps".
        setTimeout(() => {
          if (this[_state].closed) {
            reject(invalidState("RTCPeerConnection is closed"));
            return;
          }
          const id = existingIdentity(this);
          if (id === undefined || id.provider === null) {
            reject(invalidState("No identity provider is set"));
            return;
          }
          let fingerprints;
          try {
            fingerprints = fingerprintsForAssertion(this, id);
          } catch (error) {
            reject(error);
            return;
          }
          PromisePrototypeThen(
            requestAssertion(this, id, fingerprints),
            (assertion) => resolve(assertion.value),
            reject,
          );
        }, 0);
      });
    },

    get peerIdentity() {
      webidl.assertBranded(this, RTCPeerConnectionPrototype);
      if (identityUnavailable()) {
        // Not an unhandled rejection when nobody reads it.
        const rejected = PromiseReject(identityNotSupported());
        PromisePrototypeThen(rejected, undefined, noop);
        return rejected;
      }
      return peerRecord(identityOf(this)).promise;
    },

    get idpLoginUrl() {
      webidl.assertBranded(this, RTCPeerConnectionPrototype);
      return existingIdentity(this)?.idpLoginUrl ?? null;
    },

    get idpErrorInfo() {
      webidl.assertBranded(this, RTCPeerConnectionPrototype);
      return existingIdentity(this)?.idpErrorInfo ?? null;
    },
  };
  defineMembers(RTCPeerConnectionPrototype, members, [
    "setIdentityProvider",
    "getIdentityAssertion",
    "peerIdentity",
    "idpLoginUrl",
    "idpErrorInfo",
  ]);

  // --- MediaStreamTrack (section 6) ----------------------------------------

  // `isolated`: whether the track's content is unavailable to the
  // application. Deno never isolates a track (it has no peerIdentity
  // constraint on capture and no isolated rendering), so it is false.
  const trackMembers = {
    get isolated() {
      webidl.assertBranded(this, MediaStreamTrackPrototype);
      return false;
    },
  };
  defineMembers(MediaStreamTrackPrototype, trackMembers, ["isolated"]);
  defineHandler(MediaStreamTrackPrototype, "isolationchange");

  return {
    RTCIdentityAssertion,
    engineSdp,
    reportedSdp,
    withAssertion,
    withRemoteIdentity,
  };
}

// Copies `names` from the object literal `source` (methods and getters,
// named as Web IDL has them) onto an interface prototype as Web IDL
// properties: enumerable, configurable, operations writable.
function defineMembers(prototype, source, names) {
  for (let i = 0; i < names.length; ++i) {
    const name = names[i];
    const descriptor = ObjectGetOwnPropertyDescriptor(source, name);
    if (descriptor.get !== undefined) {
      ObjectDefineProperty(prototype, name, {
        __proto__: null,
        get: descriptor.get,
        enumerable: true,
        configurable: true,
      });
    } else {
      ObjectDefineProperty(prototype, name, {
        __proto__: null,
        value: descriptor.value,
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
  }
}

return { installIdentity };
})();
