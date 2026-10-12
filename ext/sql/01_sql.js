// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.SQL` / `Deno.sql`: Bun's `Bun.sql` surface over Postgres and SQLite.
// The tagged-template builder, the helpers, the pool and transactions live
// here; `lib.rs` has the engines. Values are always sent as parameters
// (`$1`... / `?1`...), never spliced into the query text.

(function () {
const { core, primordials } = __bootstrap;
const {
  op_sql_pg_cancel,
  op_sql_pg_connect,
  op_sql_pg_finish,
  op_sql_pg_is_closed,
  op_sql_pg_next_event,
  op_sql_pg_query,
  op_sql_pg_simple,
  op_sql_read_text_file,
  op_sql_sqlite_exec,
  op_sql_sqlite_open,
  op_sql_sqlite_query,
} = core.ops;
const {
  ArrayIsArray,
  ArrayPrototypeConcat,
  ArrayPrototypeIndexOf,
  ArrayPrototypeJoin,
  ArrayPrototypeMap,
  ArrayPrototypePop,
  ArrayPrototypePush,
  ArrayPrototypeShift,
  ArrayPrototypeSlice,
  ArrayPrototypeSplice,
  DateNow,
  Error,
  FunctionPrototype,
  MapPrototypeClear,
  MapPrototypeDelete,
  MapPrototypeGet,
  MapPrototypeGetSize,
  MapPrototypeKeys,
  MapPrototypeSet,
  MapPrototypeValues,
  MathMin,
  NumberIsFinite,
  NumberIsInteger,
  NumberParseInt,
  ObjectDefineProperties,
  ObjectDefineProperty,
  ObjectKeys,
  ObjectPrototypeIsPrototypeOf,
  ObjectSetPrototypeOf,
  Promise,
  PromisePrototypeCatch,
  PromisePrototypeThen,
  PromiseReject,
  PromiseResolve,
  RegExpPrototypeExec,
  SafePromiseAll,
  SafePromisePrototypeFinally,
  SafeMap,
  SafeRegExp,
  SafeSet,
  SetPrototypeAdd,
  SetPrototypeDelete,
  SetPrototypeGetSize,
  SetPrototypeHas,
  SetPrototypeValues,
  ArrayFrom,
  String,
  StringPrototypeCharCodeAt,
  StringPrototypeEndsWith,
  StringPrototypeIndexOf,
  StringPrototypeReplace,
  StringPrototypeSearch,
  StringPrototypeSlice,
  StringPrototypeSplit,
  StringPrototypeStartsWith,
  StringPrototypeToLowerCase,
  StringPrototypeToUpperCase,
  Symbol,
  SymbolAsyncDispose,
  SymbolAsyncIterator,
  SymbolDispose,
  TypeError,
  decodeURIComponent,
} = primordials;

const { clearTimeout, setTimeout, unrefTimer } = core.loadExtScript(
  "ext:deno_web/02_timers.js",
);
let _os;
const lazyOs = () => _os ?? (_os = core.loadExtScript("ext:deno_os/30_os.js"));
let _permissions;
const lazyPermissions = () =>
  _permissions ??
    (_permissions = core.loadExtScript("ext:runtime/10_permissions.js"));
let _event;
const lazyEvent = () =>
  _event ?? (_event = core.loadExtScript("ext:deno_web/02_event.js"));

// Must match `rows.rs`.
const FLAG_VALUES = 1;
const FLAG_RAW = 2;
const FLAG_BIGINT = 4;
const FLAG_UNNAMED = 8;
const FLAG_GUARDED = 16;
const FLAG_ORDERED = 32;

// Must match `pg.rs`.
const EVENTS_NOTICES = 1;
const EVENTS_NOTIFICATIONS = 2;

const STATEMENT_CACHE_SIZE = 100;

// ---------------------------------------------------------------------------
// Errors

const ERROR_FIELDS = [
  "code",
  "errno",
  "severity",
  "detail",
  "hint",
  "position",
  "internalPosition",
  "internalQuery",
  "where",
  "schema",
  "table",
  "column",
  "dataType",
  "constraint",
  "file",
  "line",
  "routine",
  "byteOffset",
];

function assignErrorFields(error, options) {
  if (options === null || typeof options !== "object") return;
  for (let i = 0; i < ERROR_FIELDS.length; i++) {
    const key = ERROR_FIELDS[i];
    if (options[key] !== undefined) error[key] = options[key];
  }
}

class SQLError extends Error {
  constructor(message, options) {
    super(message, options);
    this.name = "SQLError";
    assignErrorFields(this, options);
  }
}

class PostgresError extends SQLError {
  constructor(message, options) {
    super(message, options);
    this.name = "PostgresError";
  }
}

class SQLiteError extends SQLError {
  constructor(message, options) {
    super(message, options);
    this.name = "SQLiteError";
  }
}

// The class names `error.rs` builds errors with.
core.registerErrorClass("DenoSqlPostgresError", PostgresError);
core.registerErrorClass("DenoSqlSQLiteError", SQLiteError);

// ---------------------------------------------------------------------------
// Engines

function connectionError(adapter, message, code) {
  return new adapter.Error(message, {
    code: code ?? adapter.closedCode,
  });
}

async function readPem(path) {
  return path === undefined ? undefined : await op_sql_read_text_file(path);
}

const postgres = {
  name: "postgres",
  Error: PostgresError,
  closedCode: "ERR_POSTGRES_CONNECTION_CLOSED",
  timeoutCode: "ERR_POSTGRES_CONNECTION_TIMEOUT",
  pipeline: true,
  placeholder: (n) => "$" + n,
  async connect(options, events) {
    let password = options.password;
    if (typeof password === "function") password = await password();
    if (password !== undefined && password !== null) {
      password = String(password);
    }
    // `sslcert` / `sslkey` / `sslrootcert` files are read per connection
    // (read permission for each).
    const ca = options.caFile !== undefined
      ? ArrayPrototypeConcat(options.ca, [await readPem(options.caFile)])
      : options.ca;
    return await op_sql_pg_connect({
      hostname: options.hostname,
      port: options.port,
      socketPath: options.socketPath ?? null,
      username: options.username,
      password: password ?? null,
      database: options.database ?? null,
      sslMode: options.sslMode,
      applicationName: options.applicationName ?? null,
      options: options.startupOptions ?? null,
      connectTimeoutMs: options.connectionTimeout * 1000,
      ca,
      serverName: options.serverName ?? null,
      cert: options.cert ?? (await readPem(options.certFile)) ?? null,
      key: options.key ?? (await readPem(options.keyFile)) ?? null,
      statementCacheSize: options.prepare ? STATEMENT_CACHE_SIZE : 0,
      events,
    });
  },
  query: (rid, text, params, flags, id) =>
    op_sql_pg_query(rid, text, params, flags, id),
  simple: (rid, text, flags, id) => op_sql_pg_simple(rid, text, flags, id),
  async control(rid, text) {
    await op_sql_pg_simple(rid, text, 0, 0);
  },
  // Ends a pipelined transaction / savepoint (see `op_sql_pg_finish`).
  finish: (rid, ok, fail) => op_sql_pg_finish(rid, ok, fail),
  isClosed: (rid) => op_sql_pg_is_closed(rid),
  nextEvent: (rid) => op_sql_pg_next_event(rid),
  cancel: (rid, id) => op_sql_pg_cancel(rid, id),
};

const sqlite = {
  name: "sqlite",
  Error: SQLiteError,
  closedCode: "ERR_SQLITE_CLOSED",
  timeoutCode: "ERR_SQLITE_CONNECTION_TIMEOUT",
  pipeline: false,
  placeholder: (n) => "?" + n,
  // deno-lint-ignore require-await
  async connect(options) {
    return {
      rid: op_sql_sqlite_open(
        options.filename,
        options.readonly,
        options.create,
      ),
      parameters: {},
    };
  },
  // deno-lint-ignore require-await
  async query(rid, text, params, flags) {
    return op_sql_sqlite_query(rid, text, params, flags);
  },
  // deno-lint-ignore require-await
  async simple(rid, text) {
    op_sql_sqlite_exec(rid, text);
    return [[[], 0, undefined]];
  },
  // deno-lint-ignore require-await
  async control(rid, text) {
    op_sql_sqlite_exec(rid, text);
  },
  isClosed: () => false,
  nextEvent: undefined,
  cancel: undefined,
};

// ---------------------------------------------------------------------------
// Pool

// Queries one pooled Postgres connection may have in flight at once (sent
// back to back without waiting for the previous result: the server runs them
// in order). Transactions and reserved connections are never shared.
const PIPELINE_DEPTH = 64;

/** What `onconnect` / `onclose` receive about a pooled connection (after
 * the error, or `null`, which comes first as in Bun). */
function connectionInfo(conn) {
  return {
    id: conn.id,
    parameters: conn.parameters ?? {},
  };
}

class Pool {
  #adapter;
  #options;
  /** Open connections nobody uses. */
  #idle = [];
  /** Connections in use or being opened (`conn.ready` until open). */
  #busy = new SafeSet();
  /** The busy connections whose queries may be pipelined. */
  #shared = [];
  #waiters = [];
  #exclusiveWaiters = 0;
  #drained = [];
  #nextId = 0;
  #forced = false;
  closed = false;

  constructor(adapter, options) {
    this.#adapter = adapter;
    this.#options = options;
  }

  get #size() {
    return this.#idle.length + SetPrototypeGetSize(this.#busy);
  }

  /** A connection for one query (`exclusive` false: possibly shared with
   * other single queries) or for a transaction / reservation. */
  acquire(exclusive) {
    if (this.closed) {
      return PromiseReject(
        connectionError(this.#adapter, "Connection pool is closed"),
      );
    }
    const shared = !exclusive && this.#adapter.pipeline;
    while (this.#idle.length > 0) {
      const conn = ArrayPrototypePop(this.#idle);
      this.#clearIdle(conn);
      if (this.#usable(conn)) {
        this.#take(conn, shared);
        return PromiseResolve(conn);
      }
      this.#destroy(conn);
    }
    if (this.#size < this.#options.max) return this.#open(shared);
    if (shared && this.#exclusiveWaiters === 0) {
      const conn = this.#leastLoaded();
      if (conn !== undefined) {
        conn.users++;
        return conn.ready ?? PromiseResolve(conn);
      }
    }
    return new Promise((resolve, reject) => {
      const waiter = { resolve, reject, timer: undefined, shared };
      if (!shared) this.#exclusiveWaiters++;
      const seconds = this.#options.connectionTimeout;
      if (seconds > 0) {
        waiter.timer = setTimeout(() => {
          const i = ArrayPrototypeIndexOf(this.#waiters, waiter);
          if (i !== -1) {
            ArrayPrototypeSplice(this.#waiters, i, 1);
            if (!shared) this.#exclusiveWaiters--;
          }
          reject(connectionError(
            this.#adapter,
            `Timed out after ${seconds}s waiting for a connection from the pool (max: ${this.#options.max})`,
            this.#adapter.timeoutCode,
          ));
        }, seconds * 1000);
      }
      ArrayPrototypePush(this.#waiters, waiter);
    });
  }

  #take(conn, shared) {
    SetPrototypeAdd(this.#busy, conn);
    conn.users = 1;
    conn.shared = shared;
    if (shared) ArrayPrototypePush(this.#shared, conn);
  }

  #unshare(conn) {
    const i = ArrayPrototypeIndexOf(this.#shared, conn);
    if (i !== -1) ArrayPrototypeSplice(this.#shared, i, 1);
  }

  #leastLoaded() {
    let best;
    for (let i = 0; i < this.#shared.length; i++) {
      const conn = this.#shared[i];
      if (
        conn.broken || conn.cancelling > 0 || conn.users >= PIPELINE_DEPTH
      ) {
        continue;
      }
      if (best === undefined || conn.users < best.users) best = conn;
    }
    return best;
  }

  #open(shared) {
    const conn = {
      id: ++this.#nextId,
      rid: undefined,
      parameters: undefined,
      createdAt: 0,
      idleTimer: undefined,
      users: 0,
      shared: false,
      broken: undefined,
      cancelling: 0,
      ready: undefined,
    };
    this.#take(conn, shared);
    conn.ready = (async () => {
      let info;
      try {
        info = await this.#adapter.connect(
          this.#options,
          typeof this.#options.onnotice === "function" ? EVENTS_NOTICES : 0,
        );
      } catch (error) {
        conn.ready = undefined;
        SetPrototypeDelete(this.#busy, conn);
        this.#unshare(conn);
        this.#dispatch();
        this.#checkDrained();
        callHook(this.#options.onconnect, error, undefined);
        throw error;
      }
      conn.ready = undefined;
      conn.rid = info.rid;
      conn.parameters = info.parameters;
      conn.createdAt = DateNow();
      // After close(), the query this connection was opened for still runs
      // (close() waits for it), unless close() stopped waiting.
      if (this.#forced) {
        SetPrototypeDelete(this.#busy, conn);
        this.#unshare(conn);
        this.#destroy(conn);
        this.#checkDrained();
        throw connectionError(this.#adapter, "Connection pool is closed");
      }
      callHook(this.#options.onconnect, null, connectionInfo(conn));
      if (typeof this.#options.onnotice === "function") {
        pumpNotices(this.#adapter, this.#options, conn.rid);
      }
      return conn;
    })();
    return conn.ready;
  }

  /** One user of `conn` is done with it; `error` (a connection error) marks
   * it broken. */
  release(conn, error) {
    if (!SetPrototypeHas(this.#busy, conn)) return;
    if (error) conn.broken = error;
    if (conn.shared) {
      conn.users--;
      if (conn.users > 0) {
        if (!conn.broken) this.#serveSharedWaiters();
        return;
      }
      this.#unshare(conn);
    }
    SetPrototypeDelete(this.#busy, conn);
    this.#recycle(conn);
  }

  #recycle(conn) {
    if (conn.broken || this.closed || !this.#usable(conn)) {
      this.#destroy(conn, conn.broken);
      this.#dispatch();
      this.#checkDrained();
      return;
    }
    const waiter = this.#nextWaiter();
    if (waiter) {
      this.#take(conn, waiter.shared);
      waiter.resolve(conn);
      if (waiter.shared) this.#serveSharedWaiters();
      return;
    }
    ArrayPrototypePush(this.#idle, conn);
    const seconds = this.#options.idleTimeout;
    if (seconds > 0) {
      conn.idleTimer = setTimeout(() => {
        conn.idleTimer = undefined;
        const i = ArrayPrototypeIndexOf(this.#idle, conn);
        if (i !== -1) {
          ArrayPrototypeSplice(this.#idle, i, 1);
          this.#destroy(conn);
        }
      }, seconds * 1000);
      // An idle connection never keeps the process alive.
      unrefTimer(conn.idleTimer);
    }
  }

  #nextWaiter() {
    const waiter = ArrayPrototypeShift(this.#waiters);
    if (waiter !== undefined) {
      clearTimeout(waiter.timer);
      if (!waiter.shared) this.#exclusiveWaiters--;
    }
    return waiter;
  }

  /** Waiting single queries (up to the first transaction / reservation in
   * line) join shared connections with room in their pipeline. */
  #serveSharedWaiters() {
    while (this.#waiters.length > 0 && this.#waiters[0].shared) {
      const conn = this.#leastLoaded();
      if (conn === undefined) return;
      const waiter = this.#nextWaiter();
      conn.users++;
      if (conn.ready) {
        PromisePrototypeThen(conn.ready, waiter.resolve, waiter.reject);
      } else {
        waiter.resolve(conn);
      }
    }
  }

  #usable(conn) {
    if (this.#adapter.isClosed(conn.rid)) return false;
    const lifetime = this.#options.maxLifetime;
    return !(lifetime > 0 && DateNow() - conn.createdAt >= lifetime * 1000);
  }

  #clearIdle(conn) {
    if (conn.idleTimer !== undefined) {
      clearTimeout(conn.idleTimer);
      conn.idleTimer = undefined;
    }
  }

  #destroy(conn, error) {
    this.#clearIdle(conn);
    if (conn.closed) return;
    conn.closed = true;
    core.tryClose(conn.rid);
    callHook(this.#options.onclose, error ?? null, connectionInfo(conn));
  }

  // Waiters get new connections when others close (or fail to open).
  #dispatch() {
    while (this.#waiters.length > 0 && this.#size < this.#options.max) {
      const waiter = this.#nextWaiter();
      PromisePrototypeThen(
        this.#open(waiter.shared),
        waiter.resolve,
        waiter.reject,
      );
    }
    this.#serveSharedWaiters();
  }

  #checkDrained() {
    if (this.closed && SetPrototypeGetSize(this.#busy) === 0) {
      const drained = this.#drained;
      this.#drained = [];
      for (let i = 0; i < drained.length; i++) drained[i]();
    }
  }

  async close(timeout) {
    this.closed = true;
    const waiters = this.#waiters;
    this.#waiters = [];
    this.#exclusiveWaiters = 0;
    for (let i = 0; i < waiters.length; i++) {
      clearTimeout(waiters[i].timer);
      waiters[i].reject(
        connectionError(this.#adapter, "Connection pool is closed"),
      );
    }
    while (this.#idle.length > 0) this.#destroy(ArrayPrototypePop(this.#idle));
    if (SetPrototypeGetSize(this.#busy) === 0) return;
    const forceClose = () => {
      this.#forced = true;
      // Connections still opening close as soon as they are open.
      const busy = ArrayFrom(SetPrototypeValues(this.#busy));
      for (let i = 0; i < busy.length; i++) {
        const conn = busy[i];
        if (conn.rid === undefined) continue;
        SetPrototypeDelete(this.#busy, conn);
        this.#unshare(conn);
        this.#destroy(conn);
      }
      this.#checkDrained();
    };
    if (timeout === 0) {
      forceClose();
      return;
    }
    await new Promise((resolve) => {
      let timer;
      ArrayPrototypePush(this.#drained, () => {
        if (timer !== undefined) clearTimeout(timer);
        resolve();
      });
      if (timeout !== undefined && timeout !== null) {
        timer = setTimeout(forceClose, timeout * 1000);
      }
    });
  }
}

/** Calls `onnotice` for each notice a pooled connection receives, until it
 * closes; never keeps the process alive. */
async function pumpNotices(adapter, options, rid) {
  while (true) {
    let event;
    try {
      const promise = adapter.nextEvent(rid);
      core.unrefOpPromise(promise);
      event = await promise;
    } catch {
      return;
    }
    if (event === null) return;
    if (event.kind === "notice") deliverNotice(options, event);
  }
}

function deliverNotice(options, event) {
  const { kind: _kind, ...notice } = event;
  try {
    options.onnotice(notice);
  } catch (error) {
    reportCallbackError(error);
  }
}

function callHook(fn, error, info) {
  if (typeof fn !== "function") return;
  try {
    fn(error, info);
  } catch (e) {
    reportCallbackError(e);
  }
}

// An exception thrown by a user callback (onconnect, onclose, onnotice, a
// listen handler) is reported like an uncaught one.
function reportCallbackError(error) {
  lazyEvent().reportException(error);
}

// ---------------------------------------------------------------------------
// Query building

const kStrings = Symbol("sql.strings");
const kArgs = Symbol("sql.args");
const kUnsafe = Symbol("sql.unsafe");
const kUnsafeParams = Symbol("sql.unsafeParams");
const kCtx = Symbol("sql.context");
const kState = Symbol("sql.state");

function isTemplateStrings(value) {
  return ArrayIsArray(value) && ArrayIsArray(value.raw);
}

function escapeIdentifier(name) {
  if (typeof name !== "string") {
    throw new TypeError(
      `SQL identifiers must be strings, received ${typeof name}`,
    );
  }
  return '"' +
    StringPrototypeReplace(
      StringPrototypeReplace(name, new SafeRegExp('"', "g"), '""'),
      new SafeRegExp("\\.", "g"),
      '"."',
    ) +
    '"';
}

function escapeIdentifiers(names) {
  return ArrayPrototypeJoin(ArrayPrototypeMap(names, escapeIdentifier), ",");
}

function flatColumns(rest) {
  const out = [];
  for (let i = 0; i < rest.length; i++) {
    if (ArrayIsArray(rest[i])) {
      for (let j = 0; j < rest[i].length; j++) {
        ArrayPrototypePush(out, rest[i][j]);
      }
    } else {
      ArrayPrototypePush(out, rest[i]);
    }
  }
  return out;
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !ArrayIsArray(value) &&
    !isQuery(value) && !isHelper(value);
}

/** The value of `sql(...)` called without a template: rendered by the SQL
 * keyword it follows (`insert`, `update ... set`, `in`, `values`, `select`),
 * or as identifiers. */
class Helper {
  constructor(first, rest) {
    this.first = first;
    this.rest = rest;
  }
}

// Each builder applies after the LAST occurrence of its keyword before the
// interpolation; the latest keyword wins (postgres.js' rule, which Bun.sql
// follows).
const BUILDERS = ArrayPrototypeMap([
  ["values", buildValues],
  ["in", buildIn],
  ["select", buildSelect],
  ["as", buildSelect],
  ["returning", buildSelect],
  ["\\(", buildSelect],
  ["update", buildUpdate],
  ["insert", buildInsert],
], (entry) => [
  new SafeRegExp(
    "((?:^|[\\s(])" + entry[0] + "(?:$|[\\s(]))(?![\\s\\S]*\\1)",
    "i",
  ),
  entry[1],
]);

function renderHelper(helper, before, b) {
  // `sql("name")` / `sql("a", "b")` is always an identifier list, whatever
  // keyword precedes it (`insert into ${sql("t")} ${sql(rows)}`).
  if (typeof helper.first === "string") {
    return escapeIdentifiers(
      ArrayPrototypeConcat([helper.first], flatColumns(helper.rest)),
    );
  }
  let best = -1;
  let builder;
  for (let i = 0; i < BUILDERS.length; i++) {
    const at = StringPrototypeSearch(before, BUILDERS[i][0]);
    if (at > best) {
      best = at;
      builder = BUILDERS[i][1];
    }
  }
  if (builder) return builder(helper.first, helper.rest, b);
  // No keyword: identifiers (`sql(["a", "b"])`).
  if (ArrayIsArray(helper.first)) return escapeIdentifiers(helper.first);
  throw new SQLError(
    "sql(object) must follow INSERT, UPDATE ... SET, VALUES, IN or SELECT",
  );
}

function rowList(rows, columns, b) {
  const out = [];
  for (let r = 0; r < rows.length; r++) {
    const row = rows[r];
    const cells = [];
    for (let c = 0; c < columns.length; c++) {
      ArrayPrototypePush(cells, b.value(row[columns[c]]));
    }
    ArrayPrototypePush(out, "(" + ArrayPrototypeJoin(cells, ",") + ")");
  }
  return ArrayPrototypeJoin(out, ",");
}

function objectRows(first) {
  const rows = ArrayIsArray(first) ? first : [first];
  if (rows.length === 0 || !isPlainObject(rows[0])) {
    throw new SQLError("sql() needs an object or a non-empty array of objects");
  }
  return rows;
}

function buildInsert(first, rest, b) {
  const rows = objectRows(first);
  const columns = rest.length ? flatColumns(rest) : ObjectKeys(rows[0]);
  return "(" + escapeIdentifiers(columns) + ")values" +
    rowList(rows, columns, b);
}

function buildUpdate(first, rest, b) {
  if (!isPlainObject(first)) {
    throw new SQLError("sql() after UPDATE ... SET needs an object");
  }
  const columns = rest.length ? flatColumns(rest) : ObjectKeys(first);
  const parts = [];
  for (let i = 0; i < columns.length; i++) {
    ArrayPrototypePush(
      parts,
      escapeIdentifier(columns[i]) + "=" + b.value(first[columns[i]]),
    );
  }
  return ArrayPrototypeJoin(parts, ",");
}

function buildValues(first, rest, b) {
  if (ArrayIsArray(first) && first.length > 0 && ArrayIsArray(first[0])) {
    // Rows given as arrays.
    const out = [];
    for (let r = 0; r < first.length; r++) {
      ArrayPrototypePush(
        out,
        "(" +
          ArrayPrototypeJoin(
            ArrayPrototypeMap(first[r], (v) => b.value(v)),
            ",",
          ) +
          ")",
      );
    }
    return ArrayPrototypeJoin(out, ",");
  }
  if (ArrayIsArray(first) && (first.length === 0 || !isPlainObject(first[0]))) {
    return "(" +
      ArrayPrototypeJoin(ArrayPrototypeMap(first, (v) => b.value(v)), ",") +
      ")";
  }
  const rows = objectRows(first);
  const columns = rest.length ? flatColumns(rest) : ObjectKeys(rows[0]);
  return rowList(rows, columns, b);
}

function buildIn(first, rest, b) {
  let values = ArrayIsArray(first) ? first : [first];
  if (rest.length && values.length && isPlainObject(values[0])) {
    // `in ${sql(users, "id")}`: that column of each object.
    const column = flatColumns(rest)[0];
    values = ArrayPrototypeMap(values, (row) => row[column]);
  }
  if (values.length === 0) return "(null)";
  return "(" +
    ArrayPrototypeJoin(ArrayPrototypeMap(values, (v) => b.value(v)), ",") +
    ")";
}

function buildSelect(first, rest, b) {
  if (ArrayIsArray(first)) return escapeIdentifiers(first);
  if (!isPlainObject(first)) {
    throw new SQLError("sql() after SELECT needs column names or an object");
  }
  const columns = rest.length ? flatColumns(rest) : ObjectKeys(first);
  const parts = [];
  for (let i = 0; i < columns.length; i++) {
    ArrayPrototypePush(
      parts,
      b.value(first[columns[i]]) + " as " + escapeIdentifier(columns[i]),
    );
  }
  return ArrayPrototypeJoin(parts, ",");
}

/** `sql.array(values, type)`: an array parameter cast to `type[]`. */
class SQLArray {
  constructor(values, cast) {
    this.values = values;
    this.cast = cast;
  }
}

// A type name as it may follow `::` (`int4`, `double precision`,
// `public.mood`, `varchar(20)`): letters, digits, `_`, spaces, dots and a
// length in parentheses.
const TYPE_NAME = new SafeRegExp(
  "^[A-Za-z_][A-Za-z0-9_ .]*(?:\\(\\d+(?:,\\d+)?\\))?(?:\\[\\])*$",
);

function arrayCast(type) {
  if (type === undefined || type === null) return undefined;
  if (
    typeof type !== "string" || RegExpPrototypeExec(TYPE_NAME, type) === null
  ) {
    throw new TypeError(
      `sql.array(): invalid type name ${
        typeof type === "string" ? `"${type}"` : typeof type
      }`,
    );
  }
  return StringPrototypeEndsWith(type, "[]") ? type : type + "[]";
}

class Builder {
  constructor(adapter) {
    this.placeholder = adapter.placeholder;
    this.casts = adapter.name === "postgres";
    this.params = [];
    this.text = "";
  }

  /** A value inside a helper: a fragment inline, anything else a parameter. */
  value(value) {
    if (isQuery(value)) {
      const saved = this.text;
      this.text = "";
      this.query(value);
      const out = this.text;
      this.text = saved;
      return out;
    }
    return this.param(value);
  }

  param(value) {
    if (value === undefined) value = null;
    if (ObjectPrototypeIsPrototypeOf(SQLArrayPrototype, value)) {
      ArrayPrototypePush(this.params, value.values);
      const placeholder = this.placeholder(this.params.length);
      return value.cast !== undefined && this.casts
        ? placeholder + "::" + value.cast
        : placeholder;
    }
    ArrayPrototypePush(this.params, value);
    return this.placeholder(this.params.length);
  }

  query(q) {
    if (q[kUnsafe] === undefined && q[kStrings] === undefined) {
      throw new SQLError(
        "A sql.file() query cannot be used inside another query",
      );
    }
    if (q[kUnsafe] !== undefined) {
      const params = q[kUnsafeParams];
      if (params && params.length) {
        if (this.text !== "" || this.params.length !== 0) {
          throw new SQLError(
            "sql.unsafe() with parameters cannot be used inside another query",
          );
        }
        for (let i = 0; i < params.length; i++) {
          ArrayPrototypePush(this.params, params[i]);
        }
      }
      this.text += q[kUnsafe];
      return;
    }
    const strings = q[kStrings];
    const args = q[kArgs];
    this.text += strings[0];
    for (let i = 0; i < args.length; i++) {
      this.arg(args[i]);
      this.text += strings[i + 1];
    }
  }

  arg(value) {
    if (isQuery(value)) {
      this.query(value);
    } else if (isHelper(value)) {
      this.text += renderHelper(value, this.text, this);
    } else if (
      ArrayIsArray(value) && value.length > 0 && isQuery(value[0])
    ) {
      // An array of fragments, space-separated.
      for (let i = 0; i < value.length; i++) {
        if (i > 0) this.text += " ";
        this.query(value[i]);
      }
    } else {
      this.text += this.param(value);
    }
  }
}

const SQLArrayPrototype = SQLArray.prototype;

function buildQuery(q, adapter) {
  const b = new Builder(adapter);
  b.query(q);
  return b;
}

// ---------------------------------------------------------------------------
// Results

function skipSpaceAndComments(text, i) {
  const n = text.length;
  while (i < n) {
    const c = StringPrototypeCharCodeAt(text, i);
    if (c === 32 || (c >= 9 && c <= 13) || c === 40 /* ( */) {
      i++;
    } else if (c === 45 && StringPrototypeCharCodeAt(text, i + 1) === 45) {
      const nl = StringPrototypeIndexOf(text, "\n", i);
      i = nl === -1 ? n : nl + 1;
    } else if (c === 47 && StringPrototypeCharCodeAt(text, i + 1) === 42) {
      const end = StringPrototypeIndexOf(text, "*/", i + 2);
      i = end === -1 ? n : end + 2;
    } else {
      break;
    }
  }
  return i;
}

const WORD = new SafeRegExp("^[A-Za-z]+");
const MAIN_COMMANDS = new SafeSet([
  "SELECT",
  "INSERT",
  "UPDATE",
  "DELETE",
  "MERGE",
  "VALUES",
]);

/** The command of a statement (`SELECT`, `INSERT`, ...); for `WITH`, the
 * statement after the common table expressions. */
function commandOf(text) {
  let i = skipSpaceAndComments(text, 0);
  const match = RegExpPrototypeExec(WORD, StringPrototypeSlice(text, i));
  if (match === null) return "";
  const word = StringPrototypeToUpperCase(match[0]);
  if (word !== "WITH") return word;
  // Scan for the first main keyword outside parentheses and quotes.
  let depth = 0;
  i += 4;
  const n = text.length;
  while (i < n) {
    const c = StringPrototypeCharCodeAt(text, i);
    if (c === 40) {
      depth++;
      i++;
    } else if (c === 41) {
      depth--;
      i++;
    } else if (c === 39 || c === 34) {
      const end = StringPrototypeIndexOf(text, c === 39 ? "'" : '"', i + 1);
      i = end === -1 ? n : end + 1;
    } else if (depth === 0 && ((c | 32) >= 97 && (c | 32) <= 122)) {
      const m = RegExpPrototypeExec(WORD, StringPrototypeSlice(text, i));
      const w = StringPrototypeToUpperCase(m[0]);
      if (MAIN_COMMANDS.has(w)) return w;
      i += m[0].length;
    } else {
      i++;
    }
  }
  return "WITH";
}

function finishResult(tuple, text) {
  const rows = tuple[0];
  const props = {
    count: { __proto__: null, value: tuple[1], writable: true },
    command: { __proto__: null, value: commandOf(text), writable: true },
  };
  if (tuple[2] !== undefined) {
    props.lastInsertRowid = {
      __proto__: null,
      value: tuple[2],
      writable: true,
    };
  }
  ObjectDefineProperties(rows, props);
  return rows;
}

function isConnectionError(error) {
  return ObjectPrototypeIsPrototypeOf(SQLError.prototype, error)
    ? error.code === postgres.closedCode || error.code === sqlite.closedCode
    : error?.name === "BadResource" || error?.name === "Interrupted";
}

let lastQueryId = 0;
/** An id for a query's op, so `cancel()` can name it (a small integer). */
function nextQueryId() {
  lastQueryId = lastQueryId >= 0x3fffffff ? 1 : lastQueryId + 1;
  return lastQueryId;
}

function cancelledError(cause) {
  return new SQLError(
    "Query cancelled",
    cause === undefined
      ? { code: "ERR_SQL_QUERY_CANCELLED" }
      : { code: "ERR_SQL_QUERY_CANCELLED", cause },
  );
}

// SQLSTATE query_canceled: what the server answers a cancel request with.
const QUERY_CANCELED = "57014";

function queryFlags(state, query) {
  let flags = query[kFlags];
  if (state.options.bigint) flags |= FLAG_BIGINT;
  return flags;
}

/** Runs `query` on `conn`. `ordered`: on the wire after every request made
 * before it on `conn` (a transaction's or a reserved connection's
 * statements), not just sent as soon as it can be (the pool's). */
async function execute(state, conn, query, ordered = false) {
  const adapter = state.adapter;
  let flags = queryFlags(state, query);
  if (ordered) flags |= FLAG_ORDERED;
  const id = nextQueryId();
  let b;
  let sent = false;
  query[kRunning] = { conn, id };
  try {
    b = buildQuery(query, adapter);
    if (query[kCancelled]) throw cancelledError();
    if (query[kSimple]) {
      if (b.params.length) {
        throw new SQLError("simple() queries cannot have parameters");
      }
      sent = true;
      const results = await adapter.simple(conn.rid, b.text, flags, id);
      if (results.length === 1) return finishResult(results[0], b.text);
      return ArrayPrototypeMap(results, (r) => finishResult(r, ""));
    }
    sent = true;
    return finishResult(
      await adapter.query(conn.rid, b.text, b.params, flags, id),
      b.text,
    );
  } catch (error) {
    // Reached synchronously when it fails before the op is called, so a
    // pipelined end sent right after sees it.
    if (!sent && flags & FLAG_GUARDED) conn.guardedFailed = true;
    throw queryError(conn, query, error);
  } finally {
    query[kRunning] = undefined;
  }
}

function queryError(conn, query, error) {
  if (isConnectionError(error)) conn.broken = error;
  if (query[kCancelled] && error?.code === QUERY_CANCELED) {
    return cancelledError(error);
  }
  return error;
}

/** Waits for a pooled connection; `query.cancel()` meanwhile rejects at
 * once (the connection, when it comes, goes straight back). */
function acquireFor(pool, query, exclusive) {
  return new Promise((resolve, reject) => {
    let done = false;
    query[kAbort] = (error) => {
      if (done) return;
      done = true;
      query[kAbort] = undefined;
      reject(error);
    };
    PromisePrototypeThen(pool.acquire(exclusive), (conn) => {
      if (done) {
        pool.release(conn);
        return;
      }
      done = true;
      query[kAbort] = undefined;
      resolve(conn);
    }, (error) => {
      if (done) return;
      done = true;
      query[kAbort] = undefined;
      reject(error);
    });
  });
}

/** Reads a `sql.file()` query's text (once). */
async function loadFile(query) {
  if (query[kFile] !== undefined && query[kUnsafe] === undefined) {
    query[kUnsafe] = await op_sql_read_text_file(query[kFile]);
  }
}

// ---------------------------------------------------------------------------
// Cursors

let lastCursorId = 0;
// Rows fetched per round trip when a query is iterated row by row.
const ROW_ITERATION_BATCH = 100;

/** The rows of `query` in batches of at most `size`: a server-side cursor
 * (`DECLARE ... CURSOR` + `FETCH`) inside a transaction, so only one batch
 * is in memory and the next is fetched when the consumer asks for it. On the
 * pool it holds a connection for itself, in a transaction of its own; on a
 * reserved connection or in a transaction it uses that one. Leaving the loop
 * early closes the cursor. SQLite runs the query and hands out its rows in
 * batches. */
async function* cursorBatches(query, size, rowByRow = false) {
  if (!NumberIsInteger(size) || size < 1) {
    throw new TypeError("cursor() needs a positive integer batch size");
  }
  if (query[kPromise] !== undefined) {
    throw new SQLError("The query has already started");
  }
  query[kPromise] = PromiseReject(
    new SQLError("The query was consumed by a cursor"),
  );
  PromisePrototypeCatch(query[kPromise], () => {});
  if (query[kCancelled]) throw cancelledError();
  const ctx = query[kCtx];
  const state = ctx.state;
  const adapter = state.adapter;
  await loadFile(query);
  if (query[kSimple]) {
    throw new SQLError("simple() queries cannot use a cursor");
  }

  if (adapter.name !== "postgres") {
    query[kPromise] = undefined;
    const rows = await query;
    if (rowByRow) {
      for (let i = 0; i < rows.length; i++) yield rows[i];
      return;
    }
    for (let i = 0; i < rows.length; i += size) {
      const batch = ArrayPrototypeSlice(rows, i, i + size);
      yield finishResult([batch, batch.length, undefined], "SELECT");
    }
    return;
  }

  let conn;
  let owned = false;
  if (ctx.kind === "pool") {
    conn = await acquireFor(state.pool, query, true);
    owned = true;
  } else {
    if (ctx.done) {
      throw new SQLError(
        ctx.kind === "transaction"
          ? "The transaction has already finished"
          : "The reserved connection has been released",
      );
    }
    // A cursor reacts to "not in a transaction" itself, so it waits for a
    // pipelined BEGIN / SAVEPOINT to succeed first.
    const gateError = await gateOutcome(ctx.gate);
    if (gateError !== undefined) throw gateError;
    conn = ctx.conn;
  }
  const b = buildQuery(query, adapter);
  const flags = queryFlags(state, query) | FLAG_UNNAMED | FLAG_ORDERED;
  const name = "deno_sql_cursor_" + (++lastCursorId);
  const declare = "declare " + name + " no scroll cursor for " + b.text;
  const fetch = "fetch forward " + size + " from " + name;
  let began = false;
  let failed = false;
  try {
    if (owned) {
      await adapter.control(conn.rid, "BEGIN");
      began = true;
    }
    try {
      await adapter.query(
        conn.rid,
        declare,
        b.params,
        FLAG_UNNAMED | FLAG_ORDERED,
        0,
      );
    } catch (error) {
      // A reserved connection outside a transaction: open one for the
      // cursor's lifetime.
      if (owned || error?.code !== "25P01") throw error;
      await adapter.control(conn.rid, "BEGIN");
      began = true;
      await adapter.query(
        conn.rid,
        declare,
        b.params,
        FLAG_UNNAMED | FLAG_ORDERED,
        0,
      );
    }
    while (true) {
      const id = nextQueryId();
      query[kRunning] = { conn, id };
      let tuple;
      try {
        if (query[kCancelled]) throw cancelledError();
        tuple = await adapter.query(conn.rid, fetch, [], flags, id);
      } catch (error) {
        throw queryError(conn, query, error);
      } finally {
        query[kRunning] = undefined;
      }
      const rows = finishResult(tuple, b.text);
      if (rowByRow) {
        for (let i = 0; i < rows.length; i++) yield rows[i];
      } else if (rows.length > 0) {
        yield rows;
      }
      if (rows.length < size) break;
    }
  } catch (error) {
    failed = true;
    throw error;
  } finally {
    // Also reached when the consumer leaves the loop early.
    try {
      if (began) {
        await adapter.control(conn.rid, failed ? "ROLLBACK" : "COMMIT");
      } else if (!failed) {
        await adapter.control(conn.rid, "CLOSE " + name);
      }
    } catch (error) {
      if (isConnectionError(error) || began) conn.broken = error;
    }
    if (owned) state.pool.release(conn, conn.broken);
  }
}

// ---------------------------------------------------------------------------
// Query

const kFlags = Symbol("sql.flags");
const kSimple = Symbol("sql.simple");
const kPromise = Symbol("sql.promise");
const kCancelled = Symbol("sql.cancelled");
const kRunning = Symbol("sql.running");
const kAbort = Symbol("sql.abort");
const kFile = Symbol("sql.file");

/** A query: lazy (runs when awaited or `.execute()`d), usable as a fragment
 * inside another query until then. */
class Query {
  constructor(ctx, strings, args, unsafe, unsafeParams, file) {
    this[kCtx] = ctx;
    this[kStrings] = strings;
    this[kArgs] = args;
    this[kUnsafe] = unsafe;
    this[kUnsafeParams] = unsafeParams;
    this[kFile] = file;
    this[kFlags] = 0;
    this[kSimple] = false;
    this[kPromise] = undefined;
    this[kCancelled] = false;
    this[kRunning] = undefined;
    this[kAbort] = undefined;
  }

  #run() {
    if (this[kPromise] === undefined) {
      if (this[kCancelled]) {
        this[kPromise] = PromiseReject(cancelledError());
      } else if (this[kFile] !== undefined && this[kUnsafe] === undefined) {
        this[kPromise] = PromisePrototypeThen(
          loadFile(this),
          () => this[kCtx].run(this),
        );
      } else {
        try {
          this[kPromise] = this[kCtx].run(this);
        } catch (error) {
          this[kPromise] = PromiseReject(error);
        }
      }
    }
    return this[kPromise];
  }

  /** Rows as arrays of values instead of objects. */
  values() {
    this[kFlags] |= FLAG_VALUES;
    return this;
  }

  /** Rows as arrays of each value's raw bytes. */
  raw() {
    this[kFlags] |= FLAG_RAW;
    return this;
  }

  /** Runs over the simple query protocol: several statements, no
   * parameters, values as text. */
  simple() {
    this[kSimple] = true;
    return this;
  }

  /** Starts the query now. */
  execute() {
    this.#run();
    return this;
  }

  /** Cancels the query: one that has not started never runs, one waiting
   * for a connection rejects at once, and one the server is running gets a
   * cancel request (Postgres) and rejects with `ERR_SQL_QUERY_CANCELLED`
   * unless it finished first. */
  cancel() {
    if (this[kCancelled]) return this;
    this[kCancelled] = true;
    if (this[kAbort] !== undefined) {
      this[kAbort](cancelledError());
      return this;
    }
    const running = this[kRunning];
    const adapter = this[kCtx].state.adapter;
    if (running !== undefined && adapter.cancel !== undefined) {
      const conn = running.conn;
      conn.cancelling++;
      const done = () => {
        conn.cancelling--;
      };
      PromisePrototypeThen(adapter.cancel(conn.rid, running.id), done, done);
    }
    return this;
  }

  /** The rows in batches of at most `size` (default 1), fetched from a
   * server-side cursor as the loop asks for them. */
  cursor(size = 1) {
    return {
      __proto__: null,
      [SymbolAsyncIterator]: () => cursorBatches(this, size),
    };
  }

  /** The rows one at a time, fetched from a server-side cursor in batches
   * of 100. */
  [SymbolAsyncIterator]() {
    return cursorBatches(this, ROW_ITERATION_BATCH, true);
  }

  get active() {
    return this[kPromise] !== undefined;
  }

  get cancelled() {
    return this[kCancelled];
  }

  then(onFulfilled, onRejected) {
    return PromisePrototypeThen(this.#run(), onFulfilled, onRejected);
  }

  catch(onRejected) {
    return PromisePrototypeCatch(this.#run(), onRejected);
  }

  finally(onFinally) {
    return SafePromisePrototypeFinally(this.#run(), onFinally);
  }
}

function isQuery(value) {
  return ObjectPrototypeIsPrototypeOf(QueryPrototype, value);
}

function isHelper(value) {
  return ObjectPrototypeIsPrototypeOf(HelperPrototype, value);
}

const QueryPrototype = Query.prototype;
const HelperPrototype = Helper.prototype;

// ---------------------------------------------------------------------------
// The `sql` function

function makeSql(state, ctx) {
  const sql = function sql(first, ...rest) {
    if (isTemplateStrings(first)) return new Query(ctx, first, rest);
    return new Helper(first, rest);
  };
  ObjectSetPrototypeOf(sql, SQL.prototype);
  ObjectDefineProperty(sql, kState, { __proto__: null, value: state });
  ObjectDefineProperty(sql, kCtx, { __proto__: null, value: ctx });
  return sql;
}

function poolContext(state) {
  return {
    kind: "pool",
    state,
    async run(query) {
      const conn = await acquireFor(state.pool, query, false);
      try {
        return await execute(state, conn, query);
      } finally {
        state.pool.release(conn, conn.broken);
      }
    },
  };
}

function connectionContext(state, kind, conn) {
  const ctx = {
    kind,
    state,
    conn,
    done: false,
    savepoints: 0,
    // The pipelined BEGIN / SAVEPOINT the statements after it depend on
    // (`openGate`), or undefined.
    gate: undefined,
    run(query) {
      if (ctx.done) {
        return PromiseReject(
          new SQLError(
            kind === "transaction"
              ? "The transaction has already finished"
              : "The reserved connection has been released",
          ),
        );
      }
      const gate = ctx.gate;
      if (gate === undefined || (gate.settled && !gate.failed)) {
        return execute(state, conn, query, true);
      }
      if (gate.failed) {
        if (query[kFlags] & FLAG_GUARDED) conn.guardedFailed = true;
        return PromiseReject(gate.error);
      }
      // Sent now, right behind the gate; its outcome is only reported once
      // the gate's is known, and the gate's error wins.
      const result = execute(state, conn, query, true);
      PromisePrototypeCatch(result, noop);
      return PromisePrototypeThen(gate.promise, () => result);
    },
  };
  return ctx;
}

// ---------------------------------------------------------------------------
// LISTEN / NOTIFY

// Reconnect delays of the listening connection: the first retry is
// immediate, then they double up to the cap.
const LISTEN_RETRY_FIRST_MS = 100;
const LISTEN_RETRY_MAX_MS = 5000;

function quoteChannel(channel) {
  return '"' +
    StringPrototypeReplace(channel, new SafeRegExp('"', "g"), '""') + '"';
}

/** The dedicated connection `sql.listen()` uses: every channel of one
 * client on it, LISTENed again after a reconnect. Notifications are
 * delivered in the order the server sent them; one that arrives while the
 * connection is down is lost (Postgres keeps none for a closed session). */
class Listener {
  #state;
  /** channel name -> Set of `{ fn, onlisten }` */
  #channels = new SafeMap();
  #conn = undefined;
  #connecting = undefined;
  #reconnecting = false;
  #closed = false;

  constructor(state) {
    this.#state = state;
  }

  async listen(channel, fn, onlisten) {
    if (this.#closed) {
      throw connectionError(this.#state.adapter, "Connection pool is closed");
    }
    const entry = { fn, onlisten };
    let handlers = MapPrototypeGet(this.#channels, channel);
    const first = handlers === undefined;
    if (first) {
      handlers = new SafeSet();
      MapPrototypeSet(this.#channels, channel, handlers);
    }
    SetPrototypeAdd(handlers, entry);
    try {
      const conn = await this.#ensure();
      // Also when #ensure() just LISTENed it: LISTEN twice is a no-op.
      if (first) {
        await this.#state.adapter.control(
          conn.rid,
          "LISTEN " + quoteChannel(channel),
        );
      }
    } catch (error) {
      this.#remove(channel, entry);
      throw error;
    }
    if (typeof onlisten === "function") onlisten();
    let listening = true;
    return {
      __proto__: null,
      channel,
      unlisten: () => {
        if (!listening) return PromiseResolve();
        listening = false;
        return this.#unlisten(channel, entry);
      },
    };
  }

  #remove(channel, entry) {
    const handlers = MapPrototypeGet(this.#channels, channel);
    if (handlers === undefined) return false;
    SetPrototypeDelete(handlers, entry);
    if (SetPrototypeGetSize(handlers) > 0) return false;
    MapPrototypeDelete(this.#channels, channel);
    return true;
  }

  async #unlisten(channel, entry) {
    if (!this.#remove(channel, entry)) return;
    const conn = this.#conn;
    if (MapPrototypeGetSize(this.#channels) === 0) {
      // Nothing left to listen to: close the connection (it no longer keeps
      // the process alive).
      this.#retire();
      return;
    }
    if (conn !== undefined) {
      try {
        await this.#state.adapter.control(
          conn.rid,
          "UNLISTEN " + quoteChannel(channel),
        );
      } catch {
        // The connection is gone, and with it the subscription.
      }
    }
  }

  #retire() {
    const conn = this.#conn;
    this.#conn = undefined;
    if (conn !== undefined) {
      conn.retired = true;
      core.tryClose(conn.rid);
    }
  }

  #ensure() {
    if (this.#conn !== undefined) return PromiseResolve(this.#conn);
    if (this.#connecting === undefined) {
      this.#connecting = SafePromisePrototypeFinally(
        this.#connect(),
        () => {
          this.#connecting = undefined;
        },
      );
    }
    return this.#connecting;
  }

  async #connect() {
    const state = this.#state;
    const options = state.options;
    const events = EVENTS_NOTIFICATIONS |
      (typeof options.onnotice === "function" ? EVENTS_NOTICES : 0);
    const info = await state.adapter.connect(options, events);
    const conn = { rid: info.rid, retired: false };
    try {
      const channels = ArrayFrom(MapPrototypeKeys(this.#channels));
      for (let i = 0; i < channels.length; i++) {
        await state.adapter.control(
          conn.rid,
          "LISTEN " + quoteChannel(channels[i]),
        );
      }
    } catch (error) {
      core.tryClose(conn.rid);
      throw error;
    }
    if (this.#closed || MapPrototypeGetSize(this.#channels) === 0) {
      core.tryClose(conn.rid);
      if (this.#closed) {
        throw connectionError(state.adapter, "Connection pool is closed");
      }
      return conn;
    }
    this.#conn = conn;
    this.#pump(conn);
    return conn;
  }

  async #pump(conn) {
    const adapter = this.#state.adapter;
    while (true) {
      let event;
      try {
        event = await adapter.nextEvent(conn.rid);
      } catch {
        event = null;
      }
      if (event === null) break;
      if (event.kind === "notification") {
        const handlers = MapPrototypeGet(this.#channels, event.channel);
        if (handlers === undefined) continue;
        const entries = ArrayFrom(SetPrototypeValues(handlers));
        for (let i = 0; i < entries.length; i++) {
          try {
            entries[i].fn(event.payload, {
              channel: event.channel,
              pid: event.pid,
            });
          } catch (error) {
            reportCallbackError(error);
          }
        }
      } else if (event.kind === "notice") {
        deliverNotice(this.#state.options, event);
      }
    }
    // The connection ended: closed by us, or lost.
    core.tryClose(conn.rid);
    if (conn.retired) return;
    if (this.#conn === conn) this.#conn = undefined;
    this.#reconnect();
  }

  async #reconnect() {
    if (this.#reconnecting) return;
    this.#reconnecting = true;
    let delay = 0;
    try {
      while (!this.#closed && MapPrototypeGetSize(this.#channels) > 0) {
        if (delay > 0) {
          await new Promise((resolve) => setTimeout(resolve, delay));
          if (this.#closed || MapPrototypeGetSize(this.#channels) === 0) {
            return;
          }
        }
        try {
          await this.#ensure();
        } catch {
          delay = delay === 0
            ? LISTEN_RETRY_FIRST_MS
            : MathMin(delay * 2, LISTEN_RETRY_MAX_MS);
          continue;
        }
        // Subscribed again: tell every listener, as after the first LISTEN.
        const all = ArrayFrom(MapPrototypeValues(this.#channels));
        for (let i = 0; i < all.length; i++) {
          const entries = ArrayFrom(SetPrototypeValues(all[i]));
          for (let j = 0; j < entries.length; j++) {
            if (typeof entries[j].onlisten !== "function") continue;
            try {
              entries[j].onlisten();
            } catch (error) {
              reportCallbackError(error);
            }
          }
        }
        return;
      }
    } finally {
      this.#reconnecting = false;
    }
  }

  close() {
    this.#closed = true;
    MapPrototypeClear(this.#channels);
    this.#retire();
  }
}

function splitOptionsAndFn(a, b) {
  const options = typeof a === "function" ? "" : a;
  const fn = typeof a === "function" ? a : b;
  if (typeof fn !== "function") {
    throw new TypeError("Expected a callback function");
  }
  if (typeof options !== "string") {
    throw new TypeError("Transaction options must be a string");
  }
  return [options, fn];
}

async function settle(result) {
  result = await result;
  return ArrayIsArray(result) ? await SafePromiseAll(result) : result;
}

const noop = () => {};

/** A pipelined BEGIN / SAVEPOINT: statements sent after it go out at once,
 * and report their results once its own is known (its error, if any). */
function openGate(promise) {
  const gate = { promise, settled: false, failed: false, error: undefined };
  PromisePrototypeThen(promise, () => {
    gate.settled = true;
  }, (error) => {
    gate.settled = true;
    gate.failed = true;
    gate.error = error;
  });
  return gate;
}

/** The gate's error once it has settled, or undefined. */
async function gateOutcome(gate) {
  if (gate === undefined) return undefined;
  if (!gate.settled) {
    try {
      await gate.promise;
    } catch {
      // Recorded by openGate's handler, which ran first.
    }
  }
  return gate.failed ? gate.error : undefined;
}

// `BEGIN` options a pipelined BEGIN takes: the transaction modes, and
// nothing else, so BEGIN can't fail on its syntax (the statements sent
// behind it would then run outside any transaction). Postgres separates
// tokens by ASCII whitespace only.
const WS = "[ \\t\\n\\r\\f]";
const TRANSACTION_MODE = "(?:isolation" + WS + "+level" + WS +
  "+(?:serializable|repeatable" + WS + "+read|read" + WS + "+committed|read" +
  WS + "+uncommitted)|read" + WS + "+(?:write|only)|(?:not" + WS +
  "+)?deferrable)";
const TRANSACTION_MODES = new SafeRegExp(
  "^" + WS + "*" + TRANSACTION_MODE + "(?:(?:" + WS + "*," + WS + "*|" + WS +
    "+)" + TRANSACTION_MODE + ")*" + WS + "*$",
  "i",
);

/** Whether BEGIN with these options can go on the wire ahead of the
 * transaction's first statements without waiting for its answer. */
function canPipelineBegin(adapter, options) {
  return adapter.finish !== undefined &&
    (options === "" ||
      RegExpPrototypeExec(TRANSACTION_MODES, options) !== null);
}

/** The queries to start when a callback's result allows a pipelined end
 * (COMMIT / RELEASE right behind its last statement): a query of this
 * transaction or an array of them (and plain values), none started yet;
 * an empty array when there is nothing to start; undefined when the result
 * has to be awaited first (a promise, an async callback, a query of another
 * client, a `sql.file()` query still to be read...). */
function pipelinedEnd(ctx, result) {
  if (ctx.state.adapter.finish === undefined) return undefined;
  const items = ArrayIsArray(result) ? result : [result];
  const queries = [];
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (isQuery(item)) {
      if (
        item[kCtx] !== ctx || item[kPromise] !== undefined ||
        item[kCancelled] ||
        (item[kFile] !== undefined && item[kUnsafe] === undefined)
      ) {
        return undefined;
      }
      ArrayPrototypePush(queries, item);
    } else if (
      item !== null && (typeof item === "object" || typeof item === "function")
    ) {
      // A thenable (or anything that might be one) is awaited first.
      if (typeof item.then === "function") return undefined;
    }
  }
  return queries;
}

/** Starts `queries` (guarded: one that fails before it is sent makes the end
 * roll back), then sends the end (`ok`, or `fail` when one of them already
 * failed). Returns the end's promise, already handled. */
function startWithEnd(conn, adapter, queries, ok, fail) {
  for (let i = 0; i < queries.length; i++) {
    queries[i][kFlags] |= FLAG_GUARDED;
    queries[i].execute();
  }
  const failed = conn.guardedFailed;
  conn.guardedFailed = false;
  const end = adapter.finish(conn.rid, failed ? fail : ok, fail);
  PromisePrototypeCatch(end, noop);
  return end;
}

function callTransactionFn(fn, tx) {
  try {
    return fn(tx);
  } catch (error) {
    return PromiseReject(error);
  }
}

async function rollback(adapter, conn) {
  try {
    await adapter.control(conn.rid, "ROLLBACK");
  } catch {
    conn.broken = true;
  }
}

/** BEGIN, `fn(tx)`, COMMIT; ROLLBACK when it throws. On Postgres BEGIN is
 * pipelined with the first statements (their results wait for its answer),
 * and when `fn` returns its queries without awaiting them COMMIT goes right
 * behind them, so `begin(tx => [tx`...`, tx`...`])` is one round trip. */
async function runTransaction(state, conn, options, fn) {
  const adapter = state.adapter;
  const begin = options ? "BEGIN " + options : "BEGIN";
  const ctx = connectionContext(state, "transaction", conn);
  const tx = makeSql(state, ctx);
  if (canPipelineBegin(adapter, options)) {
    conn.guardedFailed = false;
    ctx.gate = openGate(adapter.control(conn.rid, begin));
  } else {
    await adapter.control(conn.rid, begin);
  }
  const result = callTransactionFn(fn, tx);

  const queries = pipelinedEnd(ctx, result);
  if (queries !== undefined) {
    const end = startWithEnd(conn, adapter, queries, "COMMIT", "ROLLBACK");
    ctx.done = true;
    let value;
    try {
      value = await settle(result);
    } catch (error) {
      // A statement failed: the server has aborted the transaction (its
      // COMMIT rolls back), or it never went out (`end` was a ROLLBACK).
      try {
        await end;
      } catch {
        conn.broken = true;
      }
      throw (await gateOutcome(ctx.gate)) ?? error;
    }
    const gateError = await gateOutcome(ctx.gate);
    let committed;
    try {
      committed = await end;
    } catch (error) {
      if (gateError !== undefined) throw gateError;
      await rollback(adapter, conn);
      throw error;
    }
    if (gateError !== undefined) throw gateError;
    if (!committed) throw new SQLError("The transaction was rolled back");
    return value;
  }

  let value;
  try {
    value = await settle(result);
  } catch (error) {
    ctx.done = true;
    const gateError = await gateOutcome(ctx.gate);
    if (gateError !== undefined) throw gateError;
    await rollback(adapter, conn);
    throw error;
  }
  ctx.done = true;
  const gateError = await gateOutcome(ctx.gate);
  if (gateError !== undefined) throw gateError;
  try {
    await adapter.control(conn.rid, "COMMIT");
  } catch (error) {
    await rollback(adapter, conn);
    throw error;
  }
  return value;
}

/** SAVEPOINT, `fn(tx)`, RELEASE; ROLLBACK TO when it throws. Pipelined as
 * `runTransaction` is (a SAVEPOINT can only fail in an aborted transaction,
 * where the statements behind it fail too). */
async function runSavepoint(state, ctx, tx, id, fn) {
  const adapter = state.adapter;
  const conn = ctx.conn;
  const outer = ctx.gate;
  const pipelined = adapter.finish !== undefined;
  const release = "RELEASE SAVEPOINT " + id;
  const rollbackTo = "ROLLBACK TO SAVEPOINT " + id;
  if (pipelined) {
    ctx.gate = openGate(adapter.control(conn.rid, "SAVEPOINT " + id));
  } else {
    await adapter.control(conn.rid, "SAVEPOINT " + id);
  }
  const gate = ctx.gate;
  try {
    const result = callTransactionFn(fn, tx);
    const queries = pipelined ? pipelinedEnd(ctx, result) : undefined;
    if (queries !== undefined) {
      const end = startWithEnd(conn, adapter, queries, release, rollbackTo);
      let value;
      try {
        value = await settle(result);
      } catch (error) {
        // RELEASE failed in the aborted transaction (or never went out:
        // `end` was the ROLLBACK TO already).
        let released;
        try {
          released = await end;
        } catch {
          released = undefined;
        }
        const gateError = await gateOutcome(gate);
        if (gateError !== undefined) throw gateError;
        if (released !== false) {
          await adapter.control(conn.rid, rollbackTo);
        }
        throw error;
      }
      const gateError = await gateOutcome(gate);
      if (gateError !== undefined) {
        await PromisePrototypeCatch(end, noop);
        throw gateError;
      }
      if (!(await end)) {
        throw new SQLError("The savepoint was rolled back");
      }
      return value;
    }

    let value;
    try {
      value = await settle(result);
    } catch (error) {
      const gateError = await gateOutcome(gate);
      if (gateError !== undefined) throw gateError;
      await adapter.control(conn.rid, rollbackTo);
      throw error;
    }
    const gateError = await gateOutcome(gate);
    if (gateError !== undefined) throw gateError;
    await adapter.control(conn.rid, release);
    return value;
  } finally {
    if (ctx.gate === gate) ctx.gate = outer;
  }
}

function readEnv(name) {
  return lazyOs().env.get(name);
}

// Reads `name` only when the env permission for it is already granted:
// never prompts, never throws (defaults for fields the caller left out).
function softEnv(name) {
  try {
    const { permissions } = lazyPermissions();
    if (
      permissions.querySync({ name: "env", variable: name }).state !== "granted"
    ) {
      return undefined;
    }
    return readEnv(name);
  } catch {
    return undefined;
  }
}

const DEFAULT_URL_VARS = ["POSTGRES_URL", "DATABASE_URL", "PGURL", "PG_URL"];
const CONNECTION_FIELDS = [
  "adapter",
  "filename",
  "hostname",
  "host",
  "port",
  "username",
  "user",
  "password",
  "pass",
  "database",
  "db",
];

// The default connection string is read only for a client given nothing to
// connect to (`new SQL()`, `Deno.sql`, options without connection fields).
function hasConnectionFields(options) {
  for (let i = 0; i < CONNECTION_FIELDS.length; i++) {
    if (options[CONNECTION_FIELDS[i]] !== undefined) return true;
  }
  return false;
}

function seconds(value, name, fallback) {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== "number" || !NumberIsFinite(value) || value < 0) {
    throw new TypeError(`"${name}" must be a non-negative number of seconds`);
  }
  return value;
}

const PG_URL = new SafeRegExp(
  "^(postgres(?:ql)?):\\/\\/(?:([^:@\\/?#]*)(?::([^@\\/?#]*))?@)?(\\[[^\\]]*\\]|[^:\\/?#]*)(?::(\\d*))?(?:\\/([^?#]*))?(?:\\?([^#]*))?(?:#.*)?$",
  "i",
);

function decode(component) {
  return component === undefined ? undefined : decodeURIComponent(component);
}

function parsePostgresUrl(url) {
  const match = RegExpPrototypeExec(PG_URL, url);
  if (match === null) {
    throw new TypeError(`Invalid Postgres connection string`);
  }
  let hostname = decode(match[4]);
  if (hostname && hostname[0] === "[") {
    hostname = StringPrototypeSlice(hostname, 1, -1);
  }
  const params = { __proto__: null };
  if (match[7]) {
    const pairs = StringPrototypeSplit(match[7], "&");
    for (let i = 0; i < pairs.length; i++) {
      if (pairs[i] === "") continue;
      const eq = StringPrototypeIndexOf(pairs[i], "=");
      const key = decode(
        eq === -1 ? pairs[i] : StringPrototypeSlice(pairs[i], 0, eq),
      );
      params[key] = eq === -1
        ? ""
        : decode(StringPrototypeSlice(pairs[i], eq + 1));
    }
  }
  return {
    username: decode(match[2]) || undefined,
    password: match[3] !== undefined ? decode(match[3]) : undefined,
    hostname: hostname || undefined,
    port: match[5] ? NumberParseInt(match[5], 10) : undefined,
    database: decode(match[6]) || undefined,
    params,
  };
}

function sqliteFilenameFromUrl(url) {
  // sqlite://path, sqlite:///abs/path, sqlite::memory:, file:///abs/path
  let rest;
  if (StringPrototypeStartsWith(url, "sqlite://")) {
    rest = StringPrototypeSlice(url, 9);
  } else if (StringPrototypeStartsWith(url, "sqlite:")) {
    rest = StringPrototypeSlice(url, 7);
  } else if (StringPrototypeStartsWith(url, "file://")) {
    rest = StringPrototypeSlice(url, 7);
    // file:///C:/x on Windows
    if (RegExpPrototypeExec(new SafeRegExp("^/[A-Za-z]:"), rest) !== null) {
      rest = StringPrototypeSlice(rest, 1);
    }
  } else {
    rest = StringPrototypeSlice(url, 5); // file:
  }
  let query = "";
  const q = StringPrototypeIndexOf(rest, "?");
  if (q !== -1) {
    query = StringPrototypeSlice(rest, q + 1);
    rest = StringPrototypeSlice(rest, 0, q);
  }
  const filename = decode(rest);
  let mode;
  const pairs = StringPrototypeSplit(query, "&");
  for (let i = 0; i < pairs.length; i++) {
    if (StringPrototypeStartsWith(pairs[i], "mode=")) {
      mode = StringPrototypeSlice(pairs[i], 5);
    }
  }
  return { filename: filename === "" ? ":memory:" : filename, mode };
}

function startupOptions(connection) {
  // Runtime parameters for the startup packet, as `-c name=value` (spaces
  // and backslashes escaped the way the server's option parser expects).
  const parts = [];
  const keys = ObjectKeys(connection);
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    if (key === "application_name") continue;
    const value = String(connection[key]);
    ArrayPrototypePush(
      parts,
      "-c " +
        StringPrototypeReplace(
          key + "=" + value,
          new SafeRegExp("[\\\\ ]", "g"),
          (c) => "\\" + c,
        ),
    );
  }
  return parts.length ? ArrayPrototypeJoin(parts, " ") : undefined;
}

const SSL_MODES = new SafeSet([
  "disable",
  "allow",
  "prefer",
  "require",
  "verify-ca",
  "verify-full",
]);

function resolveSslMode(options, urlParams) {
  const tls = options.tls ?? options.ssl;
  let ca = [];
  let serverName;
  let mode;
  let cert;
  let key;
  if (tls === true) {
    mode = "verify-full";
  } else if (tls === false) {
    mode = "disable";
  } else if (tls !== undefined && tls !== null && typeof tls === "object") {
    // An explicit sslmode (verify-ca, ...) keeps its meaning; the object adds
    // the CA and the name to verify.
    mode = tls.rejectUnauthorized === false
      ? "require"
      : (options.sslmode ?? urlParams.sslmode ?? "verify-full");
    if (tls.ca !== undefined) {
      ca = ArrayIsArray(tls.ca)
        ? ArrayPrototypeMap(tls.ca, (c) => String(c))
        : [String(tls.ca)];
    }
    serverName = tls.servername ?? tls.serverName;
    if (tls.cert !== undefined) cert = String(tls.cert);
    if (tls.key !== undefined) key = String(tls.key);
  } else if (typeof tls === "string") {
    mode = tls;
  }
  mode ??= options.sslmode ?? urlParams.sslmode;
  if (mode === undefined && urlParams.ssl !== undefined) {
    mode = urlParams.ssl === "false" ? "disable" : "require";
  }
  mode ??= softEnv("PGSSLMODE") ?? "prefer";
  mode = StringPrototypeToLowerCase(String(mode));
  if (!SSL_MODES.has(mode)) {
    throw new TypeError(`Unsupported sslmode "${mode}"`);
  }
  if (mode === "allow") mode = "prefer";
  return { mode, ca, serverName, cert, key };
}

function parseOptions(a, b) {
  let url;
  let options;
  if (typeof a === "string") {
    url = a;
    options = b ?? {};
  } else if (
    a !== null && typeof a === "object" && typeof a.href === "string" &&
    typeof a.protocol === "string"
  ) {
    url = a.href;
    options = b ?? {};
  } else {
    options = a ?? {};
    url = options.url;
    if (url !== undefined && typeof url !== "string") url = String(url);
  }
  if (typeof options !== "object") {
    throw new TypeError("SQL options must be an object");
  }
  let adapter = options.adapter;
  if (url === undefined && !hasConnectionFields(options)) {
    for (let i = 0; i < DEFAULT_URL_VARS.length; i++) {
      const value = readEnv(DEFAULT_URL_VARS[i]);
      if (value) {
        url = value;
        break;
      }
    }
  }

  let sqliteFile;
  let sqliteMode;
  let pgUrl;
  if (url !== undefined) {
    const lower = StringPrototypeToLowerCase(url);
    if (url === ":memory:") {
      sqliteFile = ":memory:";
    } else if (
      StringPrototypeStartsWith(lower, "sqlite:") ||
      StringPrototypeStartsWith(lower, "file:")
    ) {
      ({ filename: sqliteFile, mode: sqliteMode } = sqliteFilenameFromUrl(url));
    } else if (
      StringPrototypeStartsWith(lower, "postgres://") ||
      StringPrototypeStartsWith(lower, "postgresql://")
    ) {
      pgUrl = parsePostgresUrl(url);
    } else if (
      StringPrototypeStartsWith(lower, "mysql:") ||
      StringPrototypeStartsWith(lower, "mariadb:")
    ) {
      throw new TypeError("MySQL is not supported by Deno.SQL yet");
    } else if (adapter === "sqlite") {
      sqliteFile = url;
    } else {
      throw new TypeError(
        "Unsupported connection string: expected postgres://, sqlite://, file: or :memory:",
      );
    }
    if (sqliteFile !== undefined) {
      if (adapter !== undefined && adapter !== "sqlite") {
        throw new TypeError(`A SQLite URL can't use the "${adapter}" adapter`);
      }
      adapter = "sqlite";
    }
  }
  if (adapter === undefined && options.filename !== undefined) {
    adapter = "sqlite";
  }
  adapter ??= "postgres";
  if (adapter === "postgresql") adapter = "postgres";
  if (adapter !== "postgres" && adapter !== "sqlite") {
    throw new TypeError(`Unsupported adapter "${adapter}"`);
  }

  const common = {
    adapter,
    bigint: options.bigint === true || options.safeIntegers === true,
    onconnect: options.onconnect,
    onclose: options.onclose,
    onnotice: options.onnotice,
  };

  if (adapter === "sqlite") {
    const filename = sqliteFile ?? options.filename ?? ":memory:";
    let readonly = options.readonly === true || sqliteMode === "ro";
    const create = options.create !== false && sqliteMode !== "rw";
    if (sqliteMode === "rwc") readonly = false;
    return {
      ...common,
      filename: String(filename),
      readonly,
      create,
      // One connection: a transaction holds it, every other query waits.
      max: 1,
      idleTimeout: 0,
      maxLifetime: 0,
      connectionTimeout: seconds(
        options.connectionTimeout ?? options.connection_timeout,
        "connectionTimeout",
        0,
      ),
    };
  }

  const u = pgUrl ?? { params: { __proto__: null } };
  const params = u.params;
  const hostname = options.hostname ?? options.host ?? u.hostname ??
    params.host ?? softEnv("PGHOST") ?? "localhost";
  if (typeof hostname !== "string" || hostname === "") {
    throw new TypeError("Invalid hostname");
  }
  let port = options.port ?? u.port ?? params.port ?? softEnv("PGPORT") ??
    5432;
  port = typeof port === "string" ? NumberParseInt(port, 10) : port;
  if (!NumberIsInteger(port) || port < 1 || port > 65535) {
    throw new TypeError("Invalid port");
  }
  // A host that is a directory is where the server's Unix-domain socket is
  // (as in libpq: `host=/var/run/postgresql`); `path` names the socket
  // itself.
  let socketPath = options.path;
  if (socketPath === undefined && hostname[0] === "/") {
    socketPath = hostname + "/.s.PGSQL." + port;
  }
  if (socketPath !== undefined && typeof socketPath !== "string") {
    throw new TypeError("Invalid socket path");
  }
  const username = options.username ?? options.user ?? u.username ??
    params.user ?? softEnv("PGUSER") ?? softEnv("PGUSERNAME") ??
    softEnv("USER") ?? softEnv("USERNAME") ?? "postgres";
  const password = options.password ?? options.pass ?? u.password ??
    params.password ?? softEnv("PGPASSWORD");
  const database = options.database ?? options.db ?? u.database ??
    params.dbname ?? softEnv("PGDATABASE") ?? String(username);
  const max = options.max ?? 10;
  if (!NumberIsInteger(max) || max < 1) {
    throw new TypeError('"max" must be a positive integer');
  }
  const connection = { __proto__: null, ...(options.connection ?? {}) };
  const urlKnown = new SafeSet([
    "sslmode",
    "ssl",
    "host",
    "port",
    "user",
    "password",
    "dbname",
    "application_name",
    "connect_timeout",
    "options",
    "sslcert",
    "sslkey",
    "sslrootcert",
  ]);
  const urlKeys = ObjectKeys(params);
  for (let i = 0; i < urlKeys.length; i++) {
    if (!urlKnown.has(urlKeys[i])) connection[urlKeys[i]] = params[urlKeys[i]];
  }
  const { mode, ca, serverName, cert, key } = resolveSslMode(options, params);
  let startup = startupOptions(connection);
  if (params.options) {
    startup = startup ? params.options + " " + startup : params.options;
  }
  return {
    ...common,
    hostname,
    port,
    username: String(username),
    password,
    database: String(database),
    socketPath,
    sslMode: mode,
    ca,
    serverName,
    cert,
    key,
    certFile: cert === undefined ? params.sslcert : undefined,
    keyFile: key === undefined ? params.sslkey : undefined,
    caFile: params.sslrootcert !== undefined &&
        params.sslrootcert !== "system"
      ? params.sslrootcert
      : undefined,
    applicationName: connection.application_name ?? params.application_name,
    startupOptions: startup,
    prepare: options.prepare !== false,
    max,
    idleTimeout: seconds(
      options.idleTimeout ?? options.idle_timeout,
      "idleTimeout",
      0,
    ),
    maxLifetime: seconds(
      options.maxLifetime ?? options.max_lifetime,
      "maxLifetime",
      0,
    ),
    connectionTimeout: seconds(
      options.connectionTimeout ?? options.connection_timeout ??
        options.connectTimeout ?? options.connect_timeout ??
        (params.connect_timeout !== undefined
          ? NumberParseInt(params.connect_timeout, 10)
          : undefined),
      "connectionTimeout",
      30,
    ),
  };
}

/** A SQL client: `new SQL(url | options)` returns the tagged-template
 * function itself (`sql\`select ...\``), with the client methods on it. */
class SQL {
  constructor(a, b) {
    const options = parseOptions(a, b);
    const adapter = options.adapter === "sqlite" ? sqlite : postgres;
    const state = { adapter, options, pool: new Pool(adapter, options) };
    return makeSql(state, poolContext(state));
  }

  /** A query from raw text, with optional parameters (`$1`... / `?1`...).
   * The text is NOT escaped: never build it from untrusted input. */
  unsafe(text, params) {
    if (typeof text !== "string") {
      throw new TypeError("sql.unsafe() expects a string");
    }
    if (params !== undefined && !ArrayIsArray(params)) {
      throw new TypeError("sql.unsafe() parameters must be an array");
    }
    return new Query(this[kCtx], undefined, undefined, text, params);
  }

  /** A query from the text of the file at `path` (read when it runs; read
   * permission), with optional parameters (`$1`... / `?1`...). */
  file(path, params) {
    if (
      path !== null && typeof path === "object" &&
      typeof path.href === "string" && path.protocol === "file:"
    ) {
      path = decodeURIComponent(path.pathname);
    }
    if (typeof path !== "string") {
      throw new TypeError("sql.file() expects a path");
    }
    if (params !== undefined && !ArrayIsArray(params)) {
      throw new TypeError("sql.file() parameters must be an array");
    }
    return new Query(
      this[kCtx],
      undefined,
      undefined,
      undefined,
      params,
      path,
    );
  }

  /** An array parameter sent as `type[]` (`sql.array([1, 2], "int4")`); with
   * no type the server infers it. */
  array(values, type) {
    if (!ArrayIsArray(values)) {
      throw new TypeError("sql.array() expects an array");
    }
    return new SQLArray(values, arrayCast(type));
  }

  /** Subscribes to `channel` (LISTEN) on a dedicated connection: `fn`
   * receives each notification's payload, `onlisten` runs once subscribed
   * (and again after a reconnect). */
  async listen(channel, fn, onlisten) {
    if (typeof channel !== "string" || channel === "") {
      throw new TypeError("sql.listen() expects a channel name");
    }
    if (typeof fn !== "function") {
      throw new TypeError("sql.listen() expects a callback");
    }
    const state = this[kState];
    if (state.adapter.name !== "postgres") {
      throw new SQLError("LISTEN / NOTIFY needs a Postgres connection");
    }
    if (state.pool.closed) {
      throw connectionError(state.adapter, "Connection pool is closed");
    }
    state.listener ??= new Listener(state);
    return await state.listener.listen(channel, fn, onlisten);
  }

  /** Sends `payload` to the listeners of `channel` (`pg_notify`). */
  notify(channel, payload) {
    if (typeof channel !== "string" || channel === "") {
      throw new TypeError("sql.notify() expects a channel name");
    }
    if (this[kState].adapter.name !== "postgres") {
      throw new SQLError("LISTEN / NOTIFY needs a Postgres connection");
    }
    return this`select pg_notify(${channel}, ${payload ?? ""})`.execute();
  }

  /** Runs `fn` in a transaction: COMMIT when it settles, ROLLBACK (and
   * rethrow) when it throws. `options` is appended to `BEGIN`. */
  begin(a, b) {
    try {
      const { 0: options, 1: fn } = splitOptionsAndFn(a, b);
      const state = this[kState];
      const ctx = this[kCtx];
      if (ctx.kind === "transaction") {
        throw new SQLError(
          "Already in a transaction: use savepoint() to nest one",
        );
      }
      if (ctx.kind === "reserved") {
        return runTransaction(state, ctx.conn, options, fn);
      }
      return (async () => {
        const conn = await state.pool.acquire(true);
        try {
          return await runTransaction(state, conn, options, fn);
        } finally {
          state.pool.release(conn, conn.broken);
        }
      })();
    } catch (error) {
      return PromiseReject(error);
    }
  }

  transaction(a, b) {
    return this.begin(a, b);
  }

  /** A savepoint inside a transaction: RELEASE when `fn` settles, ROLLBACK
   * TO (and rethrow) when it throws. */
  async savepoint(a, b) {
    const name = typeof a === "function" ? undefined : a;
    const fn = typeof a === "function" ? a : b;
    if (typeof fn !== "function") {
      throw new TypeError("Expected a callback function");
    }
    const ctx = this[kCtx];
    if (ctx.kind !== "transaction" || ctx.done) {
      throw new SQLError("savepoint() is only available inside a transaction");
    }
    const id = escapeIdentifier(
      name !== undefined ? String(name) : "s" + (++ctx.savepoints),
    );
    return await runSavepoint(this[kState], ctx, this, id, fn);
  }

  /** A connection of the pool for this caller alone, until `release()`. */
  async reserve() {
    const state = this[kState];
    if (this[kCtx].kind !== "pool") {
      throw new SQLError("reserve() is only available on the pool");
    }
    const conn = await state.pool.acquire(true);
    return makeSql(state, connectionContext(state, "reserved", conn));
  }

  /** Returns a reserved connection to the pool. */
  release() {
    const ctx = this[kCtx];
    if (ctx.kind !== "reserved") return;
    if (ctx.done) return;
    ctx.done = true;
    this[kState].pool.release(ctx.conn, ctx.conn.broken);
  }

  [SymbolDispose]() {
    this.release();
  }

  /** Closes every connection: waits for running queries (at most `timeout`
   * seconds when given; `timeout: 0` closes at once). */
  close(options) {
    const ctx = this[kCtx];
    if (ctx.kind !== "pool") {
      return PromiseReject(
        new SQLError("close() is only available on the pool"),
      );
    }
    const state = this[kState];
    state.listener?.close();
    return state.pool.close(options?.timeout);
  }

  end(options) {
    return this.close(options);
  }

  [SymbolAsyncDispose]() {
    return this.close();
  }

  get options() {
    const { password: _password, key: _key, ...rest } = this[kState].options;
    return rest;
  }
}

// `sql` is a function: keep `call` / `apply` / `bind` reachable.
ObjectSetPrototypeOf(SQL.prototype, FunctionPrototype);
SQL.SQLError = SQLError;
SQL.PostgresError = PostgresError;
SQL.SQLiteError = SQLiteError;

let defaultSql;
/** `Deno.sql`: a client for `POSTGRES_URL` / `DATABASE_URL` / `PGURL` /
 * `PG_URL` (else the `PG*` variables), created on first access. Connecting
 * waits for the first query. */
function getDefaultSql() {
  return defaultSql ?? (defaultSql = new SQL());
}

return {
  SQL,
  SQLError,
  PostgresError,
  SQLiteError,
  getDefaultSql,
  // For tests of the builder.
  _commandOf: commandOf,
};
})();
