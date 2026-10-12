// Copyright 2018-2026 the Deno authors. MIT license.

// `Deno.SQL` / `Deno.sql`: Bun's `Bun.sql` surface over Postgres and SQLite.
// The tagged-template builder, the helpers, the pool and transactions live
// here; `lib.rs` has the engines. Values are always sent as parameters
// (`$1`... / `?1`...), never spliced into the query text.

(function () {
const { core, primordials } = __bootstrap;
const {
  op_sql_pg_connect,
  op_sql_pg_is_closed,
  op_sql_pg_query,
  op_sql_pg_simple,
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
  ArrayPrototypeSplice,
  DateNow,
  Error,
  FunctionPrototype,
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
  SafeRegExp,
  SafeSet,
  SetPrototypeAdd,
  SetPrototypeDelete,
  SetPrototypeGetSize,
  SetPrototypeValues,
  ArrayFrom,
  String,
  StringPrototypeCharCodeAt,
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

// Must match `rows.rs`.
const FLAG_VALUES = 1;
const FLAG_RAW = 2;
const FLAG_BIGINT = 4;

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

const postgres = {
  name: "postgres",
  Error: PostgresError,
  closedCode: "ERR_POSTGRES_CONNECTION_CLOSED",
  timeoutCode: "ERR_POSTGRES_CONNECTION_TIMEOUT",
  placeholder: (n) => "$" + n,
  async connect(options) {
    let password = options.password;
    if (typeof password === "function") password = await password();
    if (password !== undefined && password !== null) {
      password = String(password);
    }
    return await op_sql_pg_connect({
      hostname: options.hostname,
      port: options.port,
      username: options.username,
      password: password ?? null,
      database: options.database ?? null,
      sslMode: options.sslMode,
      applicationName: options.applicationName ?? null,
      options: options.startupOptions ?? null,
      connectTimeoutMs: options.connectionTimeout * 1000,
      ca: options.ca,
      serverName: options.serverName ?? null,
      statementCacheSize: options.prepare ? STATEMENT_CACHE_SIZE : 0,
    });
  },
  query: (rid, text, params, flags) =>
    op_sql_pg_query(rid, text, params, flags),
  simple: (rid, text, flags) => op_sql_pg_simple(rid, text, flags),
  async control(rid, text) {
    await op_sql_pg_simple(rid, text, 0);
  },
  isClosed: (rid) => op_sql_pg_is_closed(rid),
};

const sqlite = {
  name: "sqlite",
  Error: SQLiteError,
  closedCode: "ERR_SQLITE_CLOSED",
  timeoutCode: "ERR_SQLITE_CONNECTION_TIMEOUT",
  placeholder: (n) => "?" + n,
  // deno-lint-ignore require-await
  async connect(options) {
    return op_sql_sqlite_open(
      options.filename,
      options.readonly,
      options.create,
    );
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
};

// ---------------------------------------------------------------------------
// Pool

class Pool {
  #adapter;
  #options;
  #idle = [];
  #busy = new SafeSet();
  #pending = 0;
  #waiters = [];
  #drained = [];
  closed = false;

  constructor(adapter, options) {
    this.#adapter = adapter;
    this.#options = options;
  }

  get #size() {
    return this.#idle.length + SetPrototypeGetSize(this.#busy) +
      this.#pending;
  }

  acquire() {
    if (this.closed) {
      return PromiseReject(
        connectionError(this.#adapter, "Connection pool is closed"),
      );
    }
    while (this.#idle.length > 0) {
      const conn = ArrayPrototypePop(this.#idle);
      this.#clearIdle(conn);
      if (this.#usable(conn)) {
        SetPrototypeAdd(this.#busy, conn);
        return PromiseResolve(conn);
      }
      this.#destroy(conn);
    }
    if (this.#size < this.#options.max) return this.#open();
    return new Promise((resolve, reject) => {
      const waiter = { resolve, reject, timer: undefined };
      const seconds = this.#options.connectionTimeout;
      if (seconds > 0) {
        waiter.timer = setTimeout(() => {
          const i = ArrayPrototypeIndexOf(this.#waiters, waiter);
          if (i !== -1) ArrayPrototypeSplice(this.#waiters, i, 1);
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

  async #open() {
    this.#pending++;
    let rid;
    try {
      rid = await this.#adapter.connect(this.#options);
    } catch (error) {
      this.#pending--;
      this.#dispatch();
      this.#checkDrained();
      throw error;
    }
    this.#pending--;
    const conn = { rid, createdAt: DateNow(), idleTimer: undefined };
    if (this.closed) {
      this.#destroy(conn);
      this.#checkDrained();
      throw connectionError(this.#adapter, "Connection pool is closed");
    }
    SetPrototypeAdd(this.#busy, conn);
    if (typeof this.#options.onconnect === "function") {
      this.#options.onconnect();
    }
    return conn;
  }

  release(conn, broken = false) {
    if (!SetPrototypeDelete(this.#busy, conn)) return;
    if (broken || this.closed || !this.#usable(conn)) {
      this.#destroy(conn, broken);
      this.#dispatch();
      this.#checkDrained();
      return;
    }
    const waiter = ArrayPrototypeShift(this.#waiters);
    if (waiter) {
      clearTimeout(waiter.timer);
      SetPrototypeAdd(this.#busy, conn);
      waiter.resolve(conn);
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
    core.tryClose(conn.rid);
    if (typeof this.#options.onclose === "function") {
      this.#options.onclose(error === true ? undefined : error);
    }
  }

  // Waiters get new connections when others close (or fail to open).
  #dispatch() {
    while (this.#waiters.length > 0 && this.#size < this.#options.max) {
      const waiter = ArrayPrototypeShift(this.#waiters);
      clearTimeout(waiter.timer);
      PromisePrototypeThen(this.#open(), waiter.resolve, waiter.reject);
    }
  }

  #checkDrained() {
    if (
      this.closed && SetPrototypeGetSize(this.#busy) === 0 &&
      this.#pending === 0
    ) {
      const drained = this.#drained;
      this.#drained = [];
      for (let i = 0; i < drained.length; i++) drained[i]();
    }
  }

  async close(timeout) {
    this.closed = true;
    const waiters = this.#waiters;
    this.#waiters = [];
    for (let i = 0; i < waiters.length; i++) {
      clearTimeout(waiters[i].timer);
      waiters[i].reject(
        connectionError(this.#adapter, "Connection pool is closed"),
      );
    }
    while (this.#idle.length > 0) this.#destroy(ArrayPrototypePop(this.#idle));
    if (SetPrototypeGetSize(this.#busy) === 0 && this.#pending === 0) return;
    const forceClose = () => {
      const busy = ArrayFrom(SetPrototypeValues(this.#busy));
      for (let i = 0; i < busy.length; i++) this.release(busy[i], true);
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

class Builder {
  constructor(placeholder) {
    this.placeholder = placeholder;
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
    if (value === undefined) value = null;
    ArrayPrototypePush(this.params, value);
    return this.placeholder(this.params.length);
  }

  query(q) {
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
      if (value === undefined) value = null;
      ArrayPrototypePush(this.params, value);
      this.text += this.placeholder(this.params.length);
    }
  }
}

function buildQuery(q, adapter) {
  const b = new Builder(adapter.placeholder);
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

async function execute(state, conn, query) {
  const adapter = state.adapter;
  const b = buildQuery(query, adapter);
  let flags = query[kFlags];
  if (state.options.bigint) flags |= FLAG_BIGINT;
  try {
    if (query[kSimple]) {
      if (b.params.length) {
        throw new SQLError("simple() queries cannot have parameters");
      }
      const results = await adapter.simple(conn.rid, b.text, flags);
      if (results.length === 1) return finishResult(results[0], b.text);
      return ArrayPrototypeMap(results, (r) => finishResult(r, ""));
    }
    return finishResult(
      await adapter.query(conn.rid, b.text, b.params, flags),
      b.text,
    );
  } catch (error) {
    if (isConnectionError(error)) conn.broken = true;
    throw error;
  }
}

// ---------------------------------------------------------------------------
// Query

const kFlags = Symbol("sql.flags");
const kSimple = Symbol("sql.simple");
const kPromise = Symbol("sql.promise");
const kCancelled = Symbol("sql.cancelled");

/** A query: lazy (runs when awaited or `.execute()`d), usable as a fragment
 * inside another query until then. */
class Query {
  constructor(ctx, strings, args, unsafe, unsafeParams) {
    this[kCtx] = ctx;
    this[kStrings] = strings;
    this[kArgs] = args;
    this[kUnsafe] = unsafe;
    this[kUnsafeParams] = unsafeParams;
    this[kFlags] = 0;
    this[kSimple] = false;
    this[kPromise] = undefined;
    this[kCancelled] = false;
  }

  #run() {
    if (this[kPromise] === undefined) {
      if (this[kCancelled]) {
        this[kPromise] = PromiseReject(
          new SQLError("Query cancelled", { code: "ERR_SQL_QUERY_CANCELLED" }),
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

  /** Cancels a query that has not started. */
  cancel() {
    if (this[kPromise] === undefined) this[kCancelled] = true;
    return this;
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
    async run(query) {
      const conn = await state.pool.acquire();
      try {
        return await execute(state, conn, query);
      } finally {
        state.pool.release(conn, conn.broken === true);
      }
    },
  };
}

function connectionContext(state, kind, conn) {
  const ctx = {
    kind,
    conn,
    done: false,
    savepoints: 0,
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
      return execute(state, conn, query);
    },
  };
  return ctx;
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

async function runTransaction(state, conn, options, fn) {
  const adapter = state.adapter;
  await adapter.control(conn.rid, options ? "BEGIN " + options : "BEGIN");
  const ctx = connectionContext(state, "transaction", conn);
  const tx = makeSql(state, ctx);
  let result;
  try {
    result = await settle(fn(tx));
  } catch (error) {
    ctx.done = true;
    try {
      await adapter.control(conn.rid, "ROLLBACK");
    } catch {
      conn.broken = true;
    }
    throw error;
  }
  ctx.done = true;
  try {
    await adapter.control(conn.rid, "COMMIT");
  } catch (error) {
    try {
      await adapter.control(conn.rid, "ROLLBACK");
    } catch {
      conn.broken = true;
    }
    throw error;
  }
  return result;
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
  return { mode, ca, serverName };
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
  if (hostname[0] === "/") {
    throw new TypeError(
      "Unix-socket Postgres hosts are not supported by Deno.SQL yet",
    );
  }
  let port = options.port ?? u.port ?? params.port ?? softEnv("PGPORT") ??
    5432;
  port = typeof port === "string" ? NumberParseInt(port, 10) : port;
  if (!NumberIsInteger(port) || port < 1 || port > 65535) {
    throw new TypeError("Invalid port");
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
  ]);
  const urlKeys = ObjectKeys(params);
  for (let i = 0; i < urlKeys.length; i++) {
    if (!urlKnown.has(urlKeys[i])) connection[urlKeys[i]] = params[urlKeys[i]];
  }
  const { mode, ca, serverName } = resolveSslMode(options, params);
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
    sslMode: mode,
    ca,
    serverName,
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
        const conn = await state.pool.acquire();
        try {
          return await runTransaction(state, conn, options, fn);
        } finally {
          state.pool.release(conn, conn.broken === true);
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
    const adapter = this[kState].adapter;
    const id = escapeIdentifier(
      name !== undefined ? String(name) : "s" + (++ctx.savepoints),
    );
    await adapter.control(ctx.conn.rid, "SAVEPOINT " + id);
    let result;
    try {
      result = await settle(fn(this));
    } catch (error) {
      await adapter.control(ctx.conn.rid, "ROLLBACK TO SAVEPOINT " + id);
      throw error;
    }
    await adapter.control(ctx.conn.rid, "RELEASE SAVEPOINT " + id);
    return result;
  }

  /** A connection of the pool for this caller alone, until `release()`. */
  async reserve() {
    const state = this[kState];
    if (this[kCtx].kind !== "pool") {
      throw new SQLError("reserve() is only available on the pool");
    }
    const conn = await state.pool.acquire();
    return makeSql(state, connectionContext(state, "reserved", conn));
  }

  /** Returns a reserved connection to the pool. */
  release() {
    const ctx = this[kCtx];
    if (ctx.kind !== "reserved") return;
    if (ctx.done) return;
    ctx.done = true;
    this[kState].pool.release(ctx.conn, ctx.conn.broken === true);
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
    return this[kState].pool.close(options?.timeout);
  }

  end(options) {
    return this.close(options);
  }

  [SymbolAsyncDispose]() {
    return this.close();
  }

  get options() {
    const { password: _password, ...rest } = this[kState].options;
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
