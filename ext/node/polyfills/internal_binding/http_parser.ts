// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Joyent, Inc. and other Node contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.

// deno-lint-ignore-file no-explicit-any deno-internal/prefer-primordials

(function () {
const { core } = __bootstrap;
const { HTTPParser: NativeHTTPParser } = core.ops;
const { Buffer } = core.loadExtScript("ext:deno_node/internal/buffer.mjs");
const {
  captureAsyncContextFrame,
  constants: { kInit, kDestroy },
  emitDestroy,
  emitInit,
  enterNativeCallback,
  exitNativeCallback,
  getDefaultTriggerAsyncId,
  hasHooks,
  kAsyncContextFrame,
  newAsyncId,
} = core.loadExtScript("ext:deno_node/internal/async_hooks.ts");

// Method names indexed by llhttp method enum values.
// Order must match llhttp_method_t in llhttp.h.
const methods = [
  "DELETE",
  "GET",
  "HEAD",
  "POST",
  "PUT",
  "CONNECT",
  "OPTIONS",
  "TRACE",
  "COPY",
  "LOCK",
  "MKCOL",
  "MOVE",
  "PROPFIND",
  "PROPPATCH",
  "SEARCH",
  "UNLOCK",
  "BIND",
  "REBIND",
  "UNBIND",
  "ACL",
  "REPORT",
  "MKACTIVITY",
  "CHECKOUT",
  "MERGE",
  "M-SEARCH",
  "NOTIFY",
  "SUBSCRIBE",
  "UNSUBSCRIBE",
  "PATCH",
  "PURGE",
  "MKCALENDAR",
  "LINK",
  "UNLINK",
  "SOURCE",
  "QUERY",
];

const allMethods = [
  "DELETE",
  "GET",
  "HEAD",
  "POST",
  "PUT",
  "CONNECT",
  "OPTIONS",
  "TRACE",
  "COPY",
  "LOCK",
  "MKCOL",
  "MOVE",
  "PROPFIND",
  "PROPPATCH",
  "SEARCH",
  "UNLOCK",
  "BIND",
  "REBIND",
  "UNBIND",
  "ACL",
  "REPORT",
  "MKACTIVITY",
  "CHECKOUT",
  "MERGE",
  "M-SEARCH",
  "NOTIFY",
  "SUBSCRIBE",
  "UNSUBSCRIBE",
  "PATCH",
  "PURGE",
  "MKCALENDAR",
  "LINK",
  "UNLINK",
  "SOURCE",
  "PRI",
  "DESCRIBE",
  "ANNOUNCE",
  "SETUP",
  "PLAY",
  "PAUSE",
  "TEARDOWN",
  "GET_PARAMETER",
  "SET_PARAMETER",
  "REDIRECT",
  "RECORD",
  "FLUSH",
  "QUERY",
];

// Callback indices - used as indexed properties on the parser instance.
const kOnMessageBegin = 0;
const kOnHeaders = 1;
const kOnHeadersComplete = 2;
const kOnBody = 3;
const kOnMessageComplete = 4;
const kOnExecute = 5;
const kOnTimeout = 6;

// The parser is an async resource, as Node's (an AsyncWrap of type
// HTTPINCOMINGMESSAGE for a request parser, HTTPCLIENTREQUEST for a response
// parser): `initialize()` gives it a new async id (destroying the previous
// one when the parser is reused) and captures the async context, and every
// callback the native parser makes runs in that context between `before` and
// `after`, as node_http_parser.cc calls each one through MakeCallback. That
// is what keeps AsyncLocalStorage (APM agents' context) in a request handler
// whose connection was accepted by a server listening in another context.
const kAsyncId = Symbol("kAsyncId");
const kTriggerAsyncId = Symbol("kTriggerAsyncId");
const kResource = Symbol("kResource");
const kScopedCallbacks = Symbol("kScopedCallbacks");
const kCallbackCache = Symbol("kCallbackCache");
const kLastCallbackIndex = 6; // kOnTimeout

function scopedCallback(parser: any, fn: any) {
  return function (...args: unknown[]) {
    const asyncId = parser[kAsyncId];
    const prior = enterNativeCallback(
      parser[kAsyncContextFrame],
      asyncId,
      parser[kTriggerAsyncId],
      parser[kResource],
    );
    try {
      return Reflect.apply(fn, parser, args);
    } finally {
      exitNativeCallback(prior, asyncId);
    }
  };
}

// The object handed to the native parser as its callbacks: index `i` reads
// the parser's current `parser[i]` (callbacks may change during parsing)
// wrapped in the parser's async scope, and the exception slot the native
// side writes is the parser's own.
function scopedCallbacks(parser: any) {
  let callbacks = parser[kScopedCallbacks];
  if (callbacks !== undefined) return callbacks;
  const cache: any[] = [];
  parser[kCallbackCache] = cache;
  callbacks = { __proto__: null };
  for (let i = 0; i <= kLastCallbackIndex; i++) {
    Object.defineProperty(callbacks, i, {
      __proto__: null,
      get() {
        const fn = parser[i];
        if (typeof fn !== "function") return fn;
        const entry = cache[i];
        if (entry !== undefined && entry[0] === fn) return entry[1];
        const wrapped = scopedCallback(parser, fn);
        cache[i] = [fn, wrapped];
        return wrapped;
      },
    } as PropertyDescriptor);
  }
  Object.defineProperty(callbacks, "__lastException", {
    __proto__: null,
    get() {
      return parser.__lastException;
    },
    set(value) {
      parser.__lastException = value;
    },
  } as PropertyDescriptor);
  parser[kScopedCallbacks] = callbacks;
  return callbacks;
}

function callbacksFor(parser: any) {
  return parser[kAsyncId] === undefined ? parser : scopedCallbacks(parser);
}

function destroyAsyncId(parser: any) {
  const asyncId = parser[kAsyncId];
  if (asyncId === undefined) return;
  parser[kAsyncId] = undefined;
  if (hasHooks(kDestroy)) emitDestroy(asyncId);
}

/**
 * JS wrapper around the native llhttp-based HTTPParser cppgc object.
 *
 * Node.js's `_http_common.js` sets callbacks as indexed properties:
 *   parser[kOnHeaders] = function(headers, url) { ... }
 *   parser[kOnHeadersComplete] = function(major, minor, headers, ...) { ... }
 *
 * The native parser reads these during execute() and calls them
 * synchronously from the C callbacks.
 */
function HTTPParser(this: any, type?: number) {
  // Create the native cppgc parser
  this._native = new NativeHTTPParser();

  // If type is provided in constructor, initialize immediately
  if (type !== undefined) {
    this._native.initialize(type, 0, 0);
  }
}

HTTPParser.prototype.initialize = function (
  this: any,
  type: number,
  asyncResource?: any,
  maxHeaderSize?: number,
  lenientFlags?: number,
) {
  // Default to the global maxHeaderSize (16384) when not specified,
  // matching Node.js behavior where 0 means "use the default".
  const effectiveMaxHeaderSize = maxHeaderSize || 16384;
  if (asyncResource) {
    // AsyncWrap::AsyncReset: a reused parser's previous id is destroyed.
    destroyAsyncId(this);
    const asyncId = newAsyncId();
    const triggerAsyncId = getDefaultTriggerAsyncId();
    this[kAsyncId] = asyncId;
    this[kTriggerAsyncId] = triggerAsyncId;
    this[kResource] = asyncResource;
    this[kAsyncContextFrame] = captureAsyncContextFrame();
    if (hasHooks(kInit)) {
      emitInit(
        asyncId,
        type === HTTPParser.REQUEST
          ? "HTTPINCOMINGMESSAGE"
          : "HTTPCLIENTREQUEST",
        triggerAsyncId,
        asyncResource,
      );
    }
  }
  this._native.initialize(
    type,
    effectiveMaxHeaderSize,
    lenientFlags ?? 0,
  );
};

HTTPParser.prototype.execute = function (
  this: any,
  buffer: Uint8Array,
  offset?: number,
  length?: number,
) {
  // Node.js calls execute(buffer) or execute(buffer, offset, length)
  let data: Uint8Array;
  if (
    offset !== undefined && length !== undefined &&
    (offset !== 0 || length !== buffer.length)
  ) {
    data = buffer.subarray(offset, offset + length);
  } else {
    data = buffer;
  }

  // Wrap the kOnBody callback to convert Uint8Array to Buffer.
  // Node.js passes Buffer objects to body callbacks.
  // deno-lint-ignore no-this-alias
  const parser = this;
  const origOnBody = this[kOnBody];
  if (origOnBody) {
    this[kOnBody] = function (buf: Uint8Array) {
      return origOnBody.call(
        parser,
        Buffer.from(buf.buffer, buf.byteOffset, buf.byteLength),
      );
    };
  }

  // Pass the JS wrapper (or its scoped view, which reads it live) so
  // callbacks set during parsing (e.g. trailer headers set in
  // kOnHeadersComplete) are visible to subsequent callbacks.
  const result = this._native.execute(
    callbacksFor(this),
    new Uint8Array(data.buffer, data.byteOffset, data.byteLength),
  );

  // Restore original callback
  this[kOnBody] = origOnBody;

  // Re-throw any JS exception that was captured during parsing.
  // The #[op2(reentrant)] framework swallows pending v8 exceptions,
  // so we capture them in a TryCatch in the Rust callbacks and
  // store them on the JS parser object as __lastException.
  if (this.__lastException !== undefined) {
    const err = this.__lastException;
    this.__lastException = undefined;
    throw err;
  }

  // On parser error, create an Error object with code/reason/bytesParsed
  // matching Node.js behavior (node_http_parser.cc returns an Error object).
  if (result < 0) {
    const err: any = new Error("Parse Error");
    if (this._native.hasHeaderOverflow()) {
      err.code = "HPE_HEADER_OVERFLOW";
      err.reason = "Header overflow";
      err.message = "Parse Error: Header overflow";
    } else {
      const code = this._native.getLastErrorCode();
      const reason = this._native.getLastErrorReason();
      err.code = code && code !== "HPE_OK" ? code : "HPE_ERROR";
      err.reason = reason || "Parse Error";
      if (reason) {
        err.message = `Parse Error: ${reason}`;
      }
    }
    err.bytesParsed = this._native.getLastBytesParsed();
    return err;
  }

  return result;
};

HTTPParser.prototype.finish = function (this: any) {
  return this._native.finish(callbacksFor(this));
};

HTTPParser.prototype.pause = function (this: any) {
  this._native.pause();
};

HTTPParser.prototype.resume = function (this: any) {
  this._native.resume();
};

HTTPParser.prototype.close = function (this: any) {
  destroyAsyncId(this);
  this._native.close();
};

HTTPParser.prototype.free = function (this: any) {
  // Parser::Free emits the destroy hook (the parser goes back to the pool).
  destroyAsyncId(this);
  this._native.free();
};

HTTPParser.prototype.remove = function (this: any) {
  this._native.remove();
};

HTTPParser.prototype.getCurrentBuffer = function (this: any) {
  return this._native.getCurrentBuffer();
};

// consume/unconsume - server optimization: data flows directly from the
// TCP handle to the parser, bypassing the JS readable stream layer.
HTTPParser.prototype.consume = function (this: any, handle: any) {
  this._native.consume(callbacksFor(this), handle);
};

HTTPParser.prototype.unconsume = function (this: any) {
  this._native.unconsume();
};

// Static constants
HTTPParser.REQUEST = 1;
HTTPParser.RESPONSE = 2;
HTTPParser.kOnMessageBegin = kOnMessageBegin;
HTTPParser.kOnHeaders = kOnHeaders;
HTTPParser.kOnHeadersComplete = kOnHeadersComplete;
HTTPParser.kOnBody = kOnBody;
HTTPParser.kOnMessageComplete = kOnMessageComplete;
HTTPParser.kOnExecute = kOnExecute;
HTTPParser.kOnTimeout = kOnTimeout;
HTTPParser.kLenientNone = 0;
HTTPParser.kLenientHeaders = 1;
HTTPParser.kLenientChunkedLength = 2;
HTTPParser.kLenientKeepAlive = 4;
HTTPParser.kLenientTransferEncoding = 8;
HTTPParser.kLenientVersion = 16;
HTTPParser.kLenientDataAfterClose = 32;
HTTPParser.kLenientOptionalLFAfterCR = 64;
HTTPParser.kLenientOptionalCRLFAfterChunk = 128;
HTTPParser.kLenientOptionalCRBeforeLF = 256;
HTTPParser.kLenientSpacesAfterChunkSize = 512;
HTTPParser.kLenientAll = 1023;

return { methods, allMethods, HTTPParser };
})();
