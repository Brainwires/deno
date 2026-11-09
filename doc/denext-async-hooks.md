# `async_hooks` fidelity for APM tools: design note

## Goal

Application performance monitoring (APM) agents must keep their trace context
across every async boundary a request crosses, and some still read the
`async_hooks` resource graph. This note records what the current agents actually
use, where the runtime's Node layer fell short, what this branch changes, and
what is left.

## What the agents use today

Surveyed from their published npm sources (read as data):
`@opentelemetry/context-async-hooks` 3.1.0, `@opentelemetry/instrumentation`,
`-http`, `-fs`, `-net` 0.301 / 0.44 / 0.69, and `dd-trace` 6.24.

### OpenTelemetry

- Context propagation is **`AsyncLocalStorage` only**. The
  `AsyncHooksContextManager` built on `createHook` is gone from 3.x; the only
  context manager left is `AsyncLocalStorageContextManager`: `run()` for
  `with()`, `getStore()` for `active()`, `disable()`, and since Node 25.9
  `withScope()` for `attach()` (falling back to `enterWith()` plus a manual
  token on older runtimes).
- `bind()` wraps functions, and patches an `EventEmitter`'s `on` / `once` /
  `addListener` / `removeListener` so listeners run in a given context.
- `instrumentation-http` wraps `http.Server.prototype.emit` (the `request` event
  runs inside `context.with(span)`), `http.request` / `http.get` (the response
  callback is bound to the parent context), and binds `req` / `res`. It needs
  the store to survive every boundary the handler crosses after that: `await`,
  timers, `nextTick`, `setImmediate`, fs callbacks, request body events,
  `finish` / `close` on the response, and outgoing requests' socket events.
- `instrumentation-fs` wraps each fs function in `context.with(...)` and binds
  the callback: the fs implementation's own continuations must keep the
  suppressed-tracing context it was called in.
- `instrumentation-net` patches `net.Socket.prototype.connect` and listens on
  socket events; no `async_hooks`.
- `instrumentation-undici` (global `fetch`) subscribes to the `undici:request:*`
  `diagnostics_channel` channels.

### dd-trace

- Its storage is an `AsyncLocalStorage` subclass. It **feature-detects Node's
  AsyncContextFrame backend** (Node's default ALS since 24) by overriding
  `enterWith` on a probe instance and calling `run()`: under ACF, `run()` goes
  through `enterWith()`. Without that it takes its legacy path (a WeakMap of
  handles, an `AsyncLocalStorage.prototype._propagate` override, its own
  `run()`), and its profiler turns on a `createHook({ before })` to sample the
  active span on every async callback.
- Spans are entered with `enterWith()` from `diagnostics_channel` subscribers
  (`apm:http:server:request:start`, published from a wrapped
  `Server.prototype.emit`); nothing restores them explicitly, so each native
  callback must start from its own context, as MakeCallback does.
- `AsyncResource` (`bind`, `runInAsyncScope`) for promise and pool helpers.
- Node's built-in channels: `http.server.request.start`, and the `undici:*`
  channels for global `fetch`.
- `createHook` only for that profiler `before` hook, and only when ACF is not
  detected.

### Conclusion

The two agents that matter rely on **ALS propagation** and on **ALS behaving
like the AsyncContextFrame backend**, not on the hook graph. The hook graph
still matters for older agents, for CLS libraries built on
`executionAsyncResource()`, and for Node's `async-hooks` test suite, so the
branch fixes the request path's graph too, where that came cheaply.

## Where the runtime fell short (measured on 3.0.0-denext.14)

A probe of the OpenTelemetry pattern (ALS context manager, `emit('request')`
wrapper, `http.request` wrapper, fs callback binding) over http + fs + timers
passed every check: core's continuation-preserved embedder data already carries
the store through promises, timers, `nextTick`, immediates, op completions and
the HTTP paths that run in JS. What it lost:

| Boundary                                                             | Node                   | Before                                  |
| -------------------------------------------------------------------- | ---------------------- | --------------------------------------- |
| request handler of a server that listened inside `run()`             | the listen-time store  | none (the native parser's consume path) |
| `data` / `end` / `close` of a client socket connected inside `run()` | the connect-time store | none (native read callback)             |
| write completion callbacks                                           | the write-time store   | whatever was current                    |
| `run()` through `enterWith()` (ACF detection)                        | yes                    | no                                      |
| `withScope()`                                                        | yes (25.9)             | missing                                 |

And the hook graph of one request (`createHook` init / before / after):

| Callback                 | Node runs it as                                  | Before                                 |
| ------------------------ | ------------------------------------------------ | -------------------------------------- |
| `connection` event       | TCPSERVERWRAP                                    | no resource (TCPSERVERWRAP never init) |
| request handler          | HTTPINCOMINGMESSAGE < TCPWRAP < TCPSERVERWRAP    | no resource                            |
| fs callback              | FSREQCALLBACK, triggered by the caller           | an internal PROMISE                    |
| client response callback | HTTPCLIENTREQUEST                                | "HTTPINCOMINGMESSAGE" (wrong type)     |
| timer callback           | Timeout, `executionAsyncResource()` is the timer | Timeout, the resource was not entered  |

The root cause is one pattern: Node's `AsyncWrap` captures the async context
frame when the wrap is made and `MakeCallback` runs each native callback in that
frame between the wrap's `before` / `after`, restoring the caller's frame
afterwards. The runtime's libuv-style handles (`TCPWrap`, `PipeWrap`, the llhttp
`HTTPParser`) call JS straight from Rust with whatever context happens to be
current.

## What this branch does (all in `ext/node/polyfills`, no Rust)

1. `internal/async_hooks.ts`: `kAsyncContextFrame`,
   `captureAsyncContextFrame()`, `enterNativeCallback()` /
   `exitNativeCallback()`: the MakeCallback emulation (enter the captured frame,
   enter the resource for `executionAsyncResource()`, `before`; then `after` and
   restore the caller's frame).
2. `AsyncLocalStorage.run()` goes through `enterWith()` when `enterWith` is
   overridden (the fast path is unchanged otherwise), and an overridden
   `enterWith` sees the prior store on exit, as under Node's ACF; the exit still
   restores the whole snapshot. `withScope()` returns Node's `RunScope`
   (`dispose()`, `[Symbol.dispose]`).
3. Sockets (`net.ts`, `internal/stream_base_commons.ts`): a socket handle
   captures its frame when it is set up (or at `connect()`, as before); reads
   (`onStreamRead`) and the `close` event run in it, as the socket's TCPWRAP
   resource when it was reported; write completions run in the write's frame. An
   accepted handle gets an empty frame, as in Node (only the server handle's own
   callbacks inherit the listen-time context). The listening handle is reported
   (TCPSERVERWRAP / PIPESERVERWRAP), `connection` runs as it, and an accepted
   socket is reported once (it was reported twice).
4. The HTTP parser (`internal_binding/http_parser.ts`, `_http_server.js`) is an
   async resource as in Node: `initialize()` assigns a new id (destroying a
   reused parser's previous id), type HTTPINCOMINGMESSAGE for a request parser
   and HTTPCLIENTREQUEST for a response parser, the resource object the caller
   passes (`HTTPServerAsyncResource` with the socket, now passed by the server),
   trigger from `defaultTriggerAsyncIdScope(socket)` as Node's
   `connectionListener`; `close()` / `free()` destroy it. Every callback the
   native parser makes, on both the `execute()` path and the `consume()` fast
   path, runs in the parser's frame between its `before` / `after`: the native
   side is handed a view of the parser whose indexed callbacks are the parser's
   current ones wrapped in that scope. The server's per-request
   `executionAsyncResource()` override is dropped: the request is emitted inside
   the parser's scope, so the resource is the connection's parser resource, as
   in Node.
5. fs: while any hook is enabled, a callback-style call becomes an FSREQCALLBACK
   resource (`init` with the caller as trigger, `before` / `after` / `destroy`
   around the callback) through `makeCallback` (stat, access, open, close,
   mkdir, readlink, chown, ...) and `readFile`, `writeFile`, `unlink`, `rename`.
   With no hook enabled nothing changes.
6. Timers enter the `Timeout` as `executionAsyncResource()` while hooks run.

## Measured overhead

`ab -k -n 20000 -c 20` against a `node:http` server answering from
`setImmediate`, median of three runs, debug build, same machine:

See the branch report for the numbers; recorded here once measured.

## What is left (later steps)

- **The PROMISE resources the polyfills create.** Callback APIs implemented over
  promises (`fs`, `dns`, ...) still report their internal promises as PROMISE
  resources, which Node never does; an FSREQCALLBACK now wraps the callback, but
  the promise's own init / before stays in the graph. Hiding them means creating
  those promises inside core's internal-promise scope
  (`isCreatingInternalPromise()`), a change per call site or in core's op
  promise machinery.
- **Exact request counts.** Node's `readFile` is four FSREQCALLBACKs (open,
  fstat, read, close); ours is one per user call. `test-graph.fsreq-readFile`
  and `test-fsreqcallback-readFile` need the four.
- **Write / shutdown / connect requests as resources** (WRITEWRAP, SHUTDOWNWRAP,
  TCPCONNECTWRAP, GETADDRINFOREQWRAP, QUERYWRAP), PIPEWRAP / PROCESSWRAP for
  child processes, STATWATCHER, FSEVENTWRAP, SIGNALWRAP, UDP, ZLIB, TLSWRAP.
  Each is the same MakeCallback emulation at its own callback sites; the cleaner
  long-term fix is in Rust: capture the frame in `AsyncWrap` and give the
  libuv-compat layer one `make_callback` that sets the continuation data around
  every JS call, so the per-site JS wrappers go away.
- **Destroy timing.** Node queues `destroy` hooks and runs them after the
  current callback; several tests (`test-enable-disable`, `test-immediate`,
  `test-http-agent-handle-reuse-serial`) expect that order.
- **`diagnostics_channel`**: `boundedChannel` and `Channel#withStoreScope` (Node
  26), and the `undici:request:*` channels for global `fetch` that
  OpenTelemetry's undici instrumentation and dd-trace subscribe to (that is an
  `ext/fetch` change).
- **`internal/async_context_frame`** is not exposed (`require('internal/...')`
  is out of scope by design).
