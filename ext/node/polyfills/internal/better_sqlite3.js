// Copyright 2018-2026 the Deno authors. MIT license.
// Parts follow better-sqlite3's lib/ (Copyright (c) 2017 Joshua Wise, MIT
// license): the transaction controller and the argument checks.

// deno-lint-ignore-file deno-internal/prefer-primordials

// `better-sqlite3` over `node:sqlite`. better-sqlite3 is a nan addon (built
// against Node's V8 C++ ABI), which Deno can't load, so the package's entry
// modules resolve to this one instead (`maybeBetterSqlite3Override` in
// 01_require.js). It implements the package's public API on the same SQLite
// engine, with better-sqlite3's semantics: statements (run/get/all/iterate,
// pluck/raw/expand, bind, columns, reader/readonly, safeIntegers and lossy
// numbers otherwise), parameter binding and its errors, transactions with
// savepoint nesting, pragma, user functions and aggregates, backup,
// serialize and buffer-backed databases, the busy/iterating rules, unsafe
// mode, verbose, and SqliteError with SQLite's result-code names. Virtual
// tables (`table()`) are not available.

(function () {
const { core } = __bootstrap;
const sqlite = core.loadExtScript("ext:deno_node/sqlite.ts");
const { Buffer } = core.loadExtScript("ext:deno_node/internal/buffer.mjs");
const fs = core.loadExtScript("ext:deno_node/fs.ts");
const { op_node_sqlite_statement_info, op_node_sqlite_total_changes } =
  core.ops;

const { DatabaseSync } = sqlite;

// SQLite's primary result codes, then each one's extended codes in order
// (extended code n of primary p is `p | (n << 8)`).
const RESULT_CODES = [
  [0, "OK", ["LOAD_PERMANENTLY", "SYMLINK"]],
  [1, "ERROR", ["MISSING_COLLSEQ", "RETRY", "SNAPSHOT"]],
  [2, "INTERNAL", []],
  [3, "PERM", []],
  [4, "ABORT", [null, "ROLLBACK"]],
  [5, "BUSY", ["RECOVERY", "SNAPSHOT", "TIMEOUT"]],
  [6, "LOCKED", ["SHAREDCACHE", "VTAB"]],
  [7, "NOMEM", []],
  [8, "READONLY", [
    "RECOVERY",
    "CANTLOCK",
    "ROLLBACK",
    "DBMOVED",
    "CANTINIT",
    "DIRECTORY",
  ]],
  [9, "INTERRUPT", []],
  [10, "IOERR", [
    "READ",
    "SHORT_READ",
    "WRITE",
    "FSYNC",
    "DIR_FSYNC",
    "TRUNCATE",
    "FSTAT",
    "UNLOCK",
    "RDLOCK",
    "DELETE",
    "BLOCKED",
    "NOMEM",
    "ACCESS",
    "CHECKRESERVEDLOCK",
    "LOCK",
    "CLOSE",
    "DIR_CLOSE",
    "SHMOPEN",
    "SHMSIZE",
    "SHMLOCK",
    "SHMMAP",
    "SEEK",
    "DELETE_NOENT",
    "MMAP",
    "GETTEMPPATH",
    "CONVPATH",
    "VNODE",
    "AUTH",
    "BEGIN_ATOMIC",
    "COMMIT_ATOMIC",
    "ROLLBACK_ATOMIC",
    "DATA",
    "CORRUPTFS",
  ]],
  [11, "CORRUPT", ["VTAB", "SEQUENCE", "INDEX"]],
  [12, "NOTFOUND", []],
  [13, "FULL", []],
  [14, "CANTOPEN", [
    "NOTEMPDIR",
    "ISDIR",
    "FULLPATH",
    "CONVPATH",
    "DIRTYWAL",
    "SYMLINK",
  ]],
  [15, "PROTOCOL", []],
  [16, "EMPTY", []],
  [17, "SCHEMA", []],
  [18, "TOOBIG", []],
  [19, "CONSTRAINT", [
    "CHECK",
    "COMMITHOOK",
    "FOREIGNKEY",
    "FUNCTION",
    "NOTNULL",
    "PRIMARYKEY",
    "TRIGGER",
    "UNIQUE",
    "VTAB",
    "ROWID",
    "PINNED",
    "DATATYPE",
  ]],
  [20, "MISMATCH", []],
  [21, "MISUSE", []],
  [22, "NOLFS", []],
  [23, "AUTH", ["USER"]],
  [24, "FORMAT", []],
  [25, "RANGE", []],
  [26, "NOTADB", []],
  [27, "NOTICE", ["RECOVER_WAL", "RECOVER_ROLLBACK", "RBU"]],
  [28, "WARNING", ["AUTOINDEX"]],
];

const CODE_NAMES = new Map();
for (const [primary, name, extended] of RESULT_CODES) {
  CODE_NAMES.set(primary, `SQLITE_${name}`);
  for (let i = 0; i < extended.length; i++) {
    if (extended[i] !== null) {
      CODE_NAMES.set(primary | ((i + 1) << 8), `SQLITE_${name}_${extended[i]}`);
    }
  }
}

function codeName(errcode) {
  return CODE_NAMES.get(errcode) ?? `UNKNOWN_SQLITE_ERROR_${errcode}`;
}

function SqliteError(message, code) {
  if (new.target !== SqliteError) {
    return new SqliteError(message, code);
  }
  if (typeof code !== "string") {
    throw new TypeError("Expected second argument to be a string");
  }
  Error.call(this, message);
  Object.defineProperty(this, "message", {
    value: "" + message,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Error.captureStackTrace(this, SqliteError);
  this.code = code;
}
Object.setPrototypeOf(SqliteError, Error);
Object.setPrototypeOf(SqliteError.prototype, Error.prototype);
Object.defineProperty(SqliteError.prototype, "name", {
  value: "SqliteError",
  writable: true,
  enumerable: false,
  configurable: true,
});

// node:sqlite reports an error SQLite raised as ERR_SQLITE_ERROR with the
// extended result code in `errcode`; better-sqlite3 throws SqliteError with
// the code's name. Its own state errors are TypeErrors.
function convertError(err) {
  if (err === null || typeof err !== "object") return err;
  if (err.code === "ERR_SQLITE_ERROR" && typeof err.errcode === "number") {
    const converted = new SqliteError(err.message, codeName(err.errcode));
    converted.stack = err.stack?.replace(/^Error/, "SqliteError");
    return converted;
  }
  if (
    err.code === "ERR_INVALID_STATE" && err.message === "database is not open"
  ) {
    return new TypeError("The database connection is not open");
  }
  return err;
}

const kDb = Symbol("db");
const kStmt = Symbol("stmt");
const kState = Symbol("state");

const BUSY = "This database connection is busy executing a query";

// better-sqlite3's connection states: `busy` while a statement executes (so
// a user function can't reenter the connection), `iterators` while an
// iterator is open (only reads and prepare() may run, unless unsafe mode).
function assertOpen(database) {
  if (!database[kDb].isOpen) {
    throw new TypeError("The database connection is not open");
  }
}

function assertNotBusy(database) {
  if (database[kState].busy) throw new TypeError(BUSY);
}

function assertUsable(database, { iterating = false } = {}) {
  assertOpen(database);
  assertNotBusy(database);
  const state = database[kState];
  if (!iterating && state.iterators > 0 && !state.unsafe) {
    throw new TypeError(BUSY);
  }
}

function isPlainObject(value) {
  if (value === null || typeof value !== "object") return false;
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

function isBlob(value) {
  return ArrayBuffer.isView(value) && !(value instanceof DataView);
}

const BIND_TYPES =
  "SQLite3 can only bind numbers, strings, bigints, buffers, and null";

function checkBindable(value) {
  if (value === undefined) return null;
  if (
    value === null || typeof value === "number" ||
    typeof value === "string" || typeof value === "bigint"
  ) {
    return value;
  }
  if (isBlob(value)) return value;
  throw new TypeError(BIND_TYPES);
}

// better-sqlite3's binder: arrays spread into the anonymous parameters, one
// plain object binds the named ones (`@a`, `:a`, `$a` by `{ a }`), anything
// else binds the next anonymous parameter; then the counts must match. The
// result is node:sqlite's form: the named object (keyed as the SQL names
// them) first, then the anonymous values.
function bindParams(info, args, copy) {
  const { anonymousCount, named } = info;
  const anonymous = [];
  let namedValues;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (Array.isArray(arg)) {
      for (let j = 0; j < arg.length; j++) {
        if (anonymous.length === anonymousCount) {
          throw new RangeError("Too many parameter values were provided");
        }
        anonymous.push(checkBindable(arg[j]));
      }
    } else if (
      arg !== null && typeof arg === "object" && !isBlob(arg)
    ) {
      if (!isPlainObject(arg)) {
        throw new TypeError(
          "Named parameters can only be passed within plain objects",
        );
      }
      if (namedValues !== undefined) {
        throw new TypeError(
          "You cannot specify named parameters in two different objects",
        );
      }
      namedValues = {};
      for (let k = 0; k < named.length; k++) {
        const [sqlName, key] = named[k];
        if (!(key in arg)) {
          throw new RangeError(`Missing named parameter "${key}"`);
        }
        namedValues[sqlName] = checkBindable(arg[key]);
      }
    } else {
      const value = checkBindable(arg);
      if (anonymous.length === anonymousCount) {
        throw new RangeError("Too many parameter values were provided");
      }
      anonymous.push(value);
    }
  }
  if (anonymous.length < anonymousCount) {
    throw new RangeError("Too few parameter values were provided");
  }
  if (namedValues === undefined && named.length > 0) {
    throw new TypeError(
      "Missing named parameters (an object binding them was not provided)",
    );
  }
  if (copy) {
    // bind() binds the values as they are now
    for (let i = 0; i < anonymous.length; i++) {
      if (isBlob(anonymous[i])) anonymous[i] = Buffer.from(anonymous[i]);
    }
    for (const key in namedValues) {
      if (isBlob(namedValues[key])) {
        namedValues[key] = Buffer.from(namedValues[key]);
      }
    }
  }
  if (namedValues !== undefined) anonymous.unshift(namedValues);
  return anonymous;
}

function statementInfo(stmt) {
  const [readonly, names] = op_node_sqlite_statement_info(stmt);
  let anonymousCount = 0;
  const named = [];
  const seen = new Set();
  for (let i = 0; i < names.length; i++) {
    const name = names[i];
    if (name === null || name[0] === "?") {
      anonymousCount++;
    } else if (!seen.has(name)) {
      seen.add(name);
      named.push([name, name.slice(1)]);
    }
  }
  return { readonly, anonymousCount, named, names };
}

function stripLeadingComments(sql) {
  let text = sql;
  for (;;) {
    const trimmed = text.replace(/^[\s;]+/, "");
    if (trimmed.startsWith("--")) {
      const nl = trimmed.indexOf("\n");
      text = nl === -1 ? "" : trimmed.slice(nl + 1);
    } else if (trimmed.startsWith("/*")) {
      const end = trimmed.indexOf("*/");
      text = end === -1 ? "" : trimmed.slice(end + 2);
    } else {
      return trimmed;
    }
  }
}

function sqlHasTrigger(sql) {
  return /^create\s+(?:temp\s+|temporary\s+)?trigger\b/i.test(
    stripLeadingComments(sql),
  );
}

// Whether anything but whitespace, semicolons and comments follows the
// first statement (node:sqlite's prepare() ignores it), using SQLite's
// tokenizer rules for strings, quoted identifiers and comments; a CREATE
// TRIGGER body (BEGIN ... END, with CASE ... END inside) holds semicolons of
// its own.
function hasTrailingStatement(sql) {
  const isTrigger = sqlHasTrigger(sql);
  const n = sql.length;
  let depth = 0;
  let i = 0;
  while (i < n) {
    const c = sql[i];
    if (c === "-" && sql[i + 1] === "-") {
      const nl = sql.indexOf("\n", i);
      i = nl === -1 ? n : nl + 1;
    } else if (c === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end === -1 ? n : end + 2;
    } else if (c === "'" || c === '"' || c === "`") {
      i++;
      while (i < n && !(sql[i] === c && sql[i + 1] !== c)) {
        i += sql[i] === c ? 2 : 1;
      }
      i++;
    } else if (c === "[") {
      const end = sql.indexOf("]", i);
      i = end === -1 ? n : end + 1;
    } else if (c === ";") {
      i++;
      if (depth === 0) return stripLeadingComments(sql.slice(i)).length > 0;
    } else if (isTrigger && /[A-Za-z_]/.test(c)) {
      let j = i + 1;
      while (j < n && /[A-Za-z0-9_$]/.test(sql[j])) j++;
      const word = sql.slice(i, j).toUpperCase();
      if (word === "BEGIN" || word === "CASE") depth++;
      else if (word === "END" && depth > 0) depth--;
      i = j;
    } else {
      i++;
    }
  }
  return false;
}

function prepareStatement(database, sql, internal) {
  if (typeof sql !== "string") {
    throw new TypeError("Expected first argument to be a string");
  }
  assertOpen(database);
  assertNotBusy(database);
  if (!internal) {
    if (stripLeadingComments(sql).length === 0) {
      throw new RangeError("The supplied SQL string contains no statements");
    }
    if (hasTrailingStatement(sql)) {
      throw new RangeError(
        "The supplied SQL string contains more than one statement",
      );
    }
  }
  let stmt;
  try {
    stmt = database[kDb].prepare(sql);
  } catch (err) {
    throw convertError(err);
  }
  return new Statement(stmt, sql, database, kStmt);
}

// Rows come back from node:sqlite as arrays (so a join's same-named columns
// survive for expand()); blobs become Buffers, and integers too large for a
// number are read as BigInt and converted (lossily, as better-sqlite3 does)
// unless safeIntegers() is on.
function convertValue(value, lossy) {
  if (value instanceof Uint8Array) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }
  if (lossy && typeof value === "bigint") return Number(value);
  return value;
}

function convertArgs(args, safeIntegers) {
  for (let i = 0; i < args.length; i++) {
    args[i] = convertValue(args[i], !safeIntegers);
  }
  return args;
}

function isTooLarge(err) {
  return err !== null && typeof err === "object" &&
    err.code === "ERR_OUT_OF_RANGE";
}

// SQLite's sqlite3_expanded_sql() as better-sqlite3's build prints it to
// the verbose logger (SQLITE_TRACE_SIZE_LIMIT=32 shortens long text and
// blobs). Done here so the logger runs before the statement does, as there.
const TRACE_LIMIT = 32;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function traceLiteral(value) {
  if (value === null) return "NULL";
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number") {
    // bound as a double, which SQLite prints with %!.15g
    if (!Number.isFinite(value)) {
      return value > 0 ? "Inf" : value < 0 ? "-Inf" : "NaN";
    }
    const text = String(Number(value.toPrecision(15)));
    return /[.e]/.test(text) ? text : text + ".0";
  }
  if (typeof value === "string") {
    const bytes = encoder.encode(value);
    let text = value;
    let rest = 0;
    if (bytes.length > TRACE_LIMIT) {
      let end = TRACE_LIMIT;
      while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
      text = decoder.decode(bytes.subarray(0, end));
      rest = bytes.length - end;
    }
    const quoted = `'${text.replaceAll("'", "''")}'`;
    return rest ? `${quoted}/*+${rest} bytes*/` : quoted;
  }
  const bytes = value;
  const shown = bytes.length > TRACE_LIMIT
    ? bytes.subarray(0, TRACE_LIMIT)
    : bytes;
  let hex = "";
  for (let i = 0; i < shown.length; i++) {
    hex += shown[i].toString(16).padStart(2, "0");
  }
  const rest = bytes.length - shown.length;
  return rest ? `x'${hex}'/*+${rest} bytes*/` : `x'${hex}'`;
}

function expandSql(source, info, params) {
  if (info.names.length === 0) return source;
  // the value of each parameter index, from node:sqlite's argument form
  const values = [];
  let named = null;
  let anonymous = 0;
  if (params.length > 0 && isPlainObject(params[0])) {
    named = params[0];
    anonymous = 1;
  }
  for (let i = 0; i < info.names.length; i++) {
    const name = info.names[i];
    values.push(
      name === null || name[0] === "?" ? params[anonymous++] : named?.[name],
    );
  }
  let out = "";
  let last = 0;
  let maxIndex = 0;
  const indexes = new Map();
  const n = source.length;
  let i = 0;
  while (i < n) {
    const c = source[i];
    if (c === "-" && source[i + 1] === "-") {
      const nl = source.indexOf("\n", i);
      i = nl === -1 ? n : nl + 1;
    } else if (c === "/" && source[i + 1] === "*") {
      const end = source.indexOf("*/", i + 2);
      i = end === -1 ? n : end + 2;
    } else if (c === "'" || c === '"' || c === "`") {
      i++;
      while (i < n && !(source[i] === c && source[i + 1] !== c)) {
        i += source[i] === c ? 2 : 1;
      }
      i++;
    } else if (c === "[") {
      const end = source.indexOf("]", i);
      i = end === -1 ? n : end + 1;
    } else if (c === "?" || c === ":" || c === "@" || c === "$") {
      let j = i + 1;
      let index;
      if (c === "?") {
        while (j < n && source[j] >= "0" && source[j] <= "9") j++;
        index = j > i + 1 ? Number(source.slice(i + 1, j)) : maxIndex + 1;
      } else {
        while (j < n && /[A-Za-z0-9_$]/.test(source[j])) j++;
        if (j === i + 1) {
          i = j;
          continue;
        }
        const name = source.slice(i, j);
        index = indexes.get(name) ?? maxIndex + 1;
        indexes.set(name, index);
      }
      maxIndex = Math.max(maxIndex, index);
      out += source.slice(last, i) + traceLiteral(values[index - 1] ?? null);
      last = i = j;
    } else {
      i++;
    }
  }
  return out + source.slice(last);
}

class Statement {
  constructor(stmt, source, database, token) {
    if (token !== kStmt) {
      throw new TypeError(
        "Statements can only be constructed by the db.prepare() method",
      );
    }
    this[kStmt] = stmt;
    let info, columns;
    try {
      info = statementInfo(stmt);
      columns = stmt.columns();
    } catch (err) {
      throw convertError(err);
    }
    this[kState] = {
      info,
      columns,
      mode: "object", // "object" | "pluck" | "raw" | "expand"
      bound: null,
      safeIntegers: database[kState].safeIntegers,
      // integers past 2^53 were seen: read as BigInt, convert to numbers
      lossy: false,
      iterating: false,
    };
    stmt.setReturnArrays(true);
    Object.defineProperty(this, "database", {
      value: database,
      enumerable: true,
    });
    Object.defineProperty(this, "source", { value: source, enumerable: true });
    Object.defineProperty(this, "reader", {
      value: columns.length > 0,
      enumerable: true,
    });
    Object.defineProperty(this, "readonly", {
      value: info.readonly,
      enumerable: true,
    });
  }

  get busy() {
    return this[kState].iterating;
  }

  #assertUsable() {
    const database = this.database;
    assertOpen(database);
    assertNotBusy(database);
    if (this[kState].iterating) {
      throw new TypeError("This statement is busy executing a query");
    }
  }

  // Options may change while the connection is closed, not while this
  // statement is running.
  #assertIdle() {
    assertNotBusy(this.database);
    if (this[kState].iterating) {
      throw new TypeError("This statement is busy executing a query");
    }
  }

  #assertReader(method) {
    if (!this.reader) {
      throw new TypeError(
        method
          ? `The ${method}() method is only for statements that return data`
          : "This statement does not return data. Use run() instead",
      );
    }
  }

  #params(args) {
    const state = this[kState];
    if (state.bound !== null) {
      if (args.length > 0) {
        throw new TypeError("This statement already has bound parameters");
      }
      return state.bound;
    }
    return bindParams(state.info, args, false);
  }

  #trace(params) {
    const verbose = this.database[kState].verbose;
    if (verbose) verbose(expandSql(this.source, this[kState].info, params));
  }

  // Runs `fn` with the connection marked busy, converting node:sqlite's
  // errors to better-sqlite3's.
  #execute(fn) {
    const dbState = this.database[kState];
    dbState.busy = true;
    try {
      return fn(this[kStmt]);
    } catch (err) {
      throw convertError(err);
    } finally {
      dbState.busy = false;
    }
  }

  // Reads rows, falling back to BigInt (converted back to numbers) once an
  // integer too large for a number shows up.
  #read(fn) {
    const state = this[kState];
    const stmt = this[kStmt];
    stmt.setReadBigInts(state.safeIntegers || state.lossy);
    try {
      return this.#execute(fn);
    } catch (err) {
      if (!isTooLarge(err) || state.safeIntegers || state.lossy) throw err;
      state.lossy = true;
      stmt.setReadBigInts(true);
      return this.#execute(fn);
    }
  }

  #shape(row) {
    if (row === undefined) return undefined;
    const state = this[kState];
    if (row.length !== state.columns.length) {
      // the statement was recompiled (the schema changed)
      state.columns = this[kStmt].columns();
    }
    const lossy = !state.safeIntegers;
    switch (state.mode) {
      case "pluck":
        return convertValue(row[0], lossy);
      case "raw":
        for (let i = 0; i < row.length; i++) {
          row[i] = convertValue(row[i], lossy);
        }
        return row;
      case "expand": {
        const out = {};
        const columns = state.columns;
        for (let i = 0; i < columns.length; i++) {
          const table = columns[i].table ?? "$";
          (out[table] ??= {})[columns[i].name] = convertValue(row[i], lossy);
        }
        return out;
      }
      default: {
        const out = {};
        const columns = state.columns;
        for (let i = 0; i < columns.length; i++) {
          out[columns[i].name] = convertValue(row[i], lossy);
        }
        return out;
      }
    }
  }

  run(...args) {
    this.#assertUsable();
    const database = this.database;
    if (!this.readonly) assertUsable(database);
    const params = this.#params(args);
    const state = this[kState];
    const stmt = this[kStmt];
    const db = database[kDb];
    this.#trace(params);
    // lastInsertRowid may not fit a number; read it as a BigInt
    stmt.setReadBigInts(true);
    const result = this.#execute((stmt) => {
      const before = op_node_sqlite_total_changes(db);
      const result = stmt.run(...params);
      const after = op_node_sqlite_total_changes(db);
      return {
        changes: after === before ? 0 : Number(result.changes),
        lastInsertRowid: state.safeIntegers
          ? BigInt(result.lastInsertRowid)
          : Number(result.lastInsertRowid),
      };
    });
    return result;
  }

  get(...args) {
    this.#assertReader();
    this.#assertUsable();
    const params = this.#params(args);
    this.#trace(params);
    const row = this.#read((stmt) => stmt.get(...params));
    return this.#shape(row);
  }

  all(...args) {
    this.#assertReader();
    this.#assertUsable();
    const params = this.#params(args);
    this.#trace(params);
    const rows = this.#read((stmt) => stmt.all(...params));
    for (let i = 0; i < rows.length; i++) {
      rows[i] = this.#shape(rows[i]);
    }
    return rows;
  }

  iterate(...args) {
    this.#assertReader();
    this.#assertUsable();
    const params = this.#params(args);
    const state = this[kState];
    const stmt = this[kStmt];
    const dbState = this.database[kState];
    stmt.setReadBigInts(state.safeIntegers || state.lossy);
    const inner = this.#execute((stmt) => stmt.iterate(...params));
    let done = false;
    let started = false;
    const finish = () => {
      if (!done) {
        done = true;
        state.iterating = false;
        dbState.iterators--;
        try {
          inner.return?.();
        } catch {
          // already finished
        }
      }
    };
    state.iterating = true;
    dbState.iterators++;
    const shape = (row) => this.#shape(row);
    const trace = () => this.#trace(params);
    return {
      next() {
        if (dbState.busy) throw new TypeError(BUSY);
        if (done) return { value: undefined, done: true };
        if (!started) {
          // the statement starts on the first step
          started = true;
          try {
            trace();
          } catch (err) {
            finish();
            throw err;
          }
        }
        let result;
        dbState.busy = true;
        try {
          result = inner.next();
        } catch (err) {
          dbState.busy = false;
          finish();
          throw convertError(err);
        }
        dbState.busy = false;
        if (result.done) {
          finish();
          return { value: undefined, done: true };
        }
        return { value: shape(result.value), done: false };
      },
      return() {
        if (dbState.busy) throw new TypeError(BUSY);
        finish();
        return { value: undefined, done: true };
      },
      [Symbol.iterator]() {
        return this;
      },
    };
  }

  pluck(toggle = true) {
    return this.#setMode("pluck", toggle);
  }

  expand(toggle = true) {
    return this.#setMode("expand", toggle);
  }

  raw(toggle = true) {
    return this.#setMode("raw", toggle);
  }

  #setMode(mode, toggle) {
    if (typeof toggle !== "boolean") {
      throw new TypeError("Expected first argument to be a boolean");
    }
    this.#assertReader(mode);
    this.#assertIdle();
    const state = this[kState];
    if (toggle) state.mode = mode;
    else if (state.mode === mode) state.mode = "object";
    return this;
  }

  safeIntegers(toggle = true) {
    if (typeof toggle !== "boolean") {
      throw new TypeError("Expected first argument to be a boolean");
    }
    this.#assertIdle();
    this[kState].safeIntegers = toggle;
    return this;
  }

  columns() {
    this.#assertReader("columns");
    assertOpen(this.database);
    assertNotBusy(this.database);
    let columns;
    try {
      columns = this[kStmt].columns();
    } catch (err) {
      throw convertError(err);
    }
    this[kState].columns = columns;
    return columns.map((c) => ({
      name: c.name,
      column: c.column ?? null,
      table: c.table ?? null,
      database: c.database ?? null,
      type: c.type ?? null,
    }));
  }

  bind(...args) {
    this.#assertUsable();
    const state = this[kState];
    if (state.bound !== null) {
      throw new TypeError(
        "The bind() method can only be invoked once per statement object",
      );
    }
    state.bound = bindParams(state.info, args, true);
    return this;
  }
}

const controllers = new WeakMap();

function getController(database) {
  let controller = controllers.get(database);
  if (!controller) {
    const shared = {
      commit: prepareStatement(database, "COMMIT", true),
      rollback: prepareStatement(database, "ROLLBACK", true),
      savepoint: prepareStatement(database, "SAVEPOINT `\t_bs3.\t`", true),
      release: prepareStatement(database, "RELEASE `\t_bs3.\t`", true),
      rollbackTo: prepareStatement(database, "ROLLBACK TO `\t_bs3.\t`", true),
    };
    const make = (sql) => ({
      begin: prepareStatement(database, sql, true),
      ...shared,
    });
    controller = {
      default: make("BEGIN"),
      deferred: make("BEGIN DEFERRED"),
      immediate: make("BEGIN IMMEDIATE"),
      exclusive: make("BEGIN EXCLUSIVE"),
    };
    controllers.set(database, controller);
  }
  return controller;
}

function wrapTransaction(fn, database, controller) {
  const { begin, commit, rollback, savepoint, release, rollbackTo } =
    controller;
  return function sqliteTransaction(...args) {
    let before, after, undo;
    if (database.inTransaction) {
      before = savepoint;
      after = release;
      undo = rollbackTo;
    } else {
      before = begin;
      after = commit;
      undo = rollback;
    }
    before.run();
    try {
      const result = Reflect.apply(fn, this, args);
      if (result && typeof result.then === "function") {
        throw new TypeError("Transaction function cannot return a promise");
      }
      after.run();
      return result;
    } catch (err) {
      if (database.inTransaction) {
        undo.run();
        if (undo !== rollback) after.run();
      }
      throw err;
    }
  };
}

function getBooleanOption(options, key) {
  let value = false;
  if (key in options && typeof (value = options[key]) !== "boolean") {
    throw new TypeError(`Expected the "${key}" option to be a boolean`);
  }
  return value;
}

function getFunctionOption(options, key, required) {
  const value = key in options ? options[key] : null;
  if (typeof value === "function") return value;
  if (value != null) {
    throw new TypeError(`Expected the "${key}" option to be a function`);
  }
  if (required) {
    throw new TypeError(`Missing required option "${key}"`);
  }
  return null;
}

// A user function's declared argument count (`fn.length`, less `skip`
// leading accumulator arguments), checked as better-sqlite3 does.
function argumentCount(fn, skip) {
  const length = fn.length;
  if (
    typeof length !== "number" || !Number.isInteger(length) || length < 0
  ) {
    throw new TypeError("Expected function.length to be a positive integer");
  }
  const count = Math.max(length - skip, 0);
  if (count > 100) {
    throw new RangeError(
      "User-defined functions cannot have more than 100 arguments",
    );
  }
  return count;
}

function withLength(fn, length) {
  Object.defineProperty(fn, "length", { value: length });
  return fn;
}

// A user function's result, as better-sqlite3 accepts it.
function checkResult(value, name) {
  if (value === undefined) return null;
  if (
    value === null || typeof value === "number" ||
    typeof value === "string" || typeof value === "bigint" || isBlob(value)
  ) {
    return value;
  }
  throw new TypeError(
    `User-defined function ${name}() returned an invalid value`,
  );
}

function dirExists(filename) {
  const sep = Math.max(filename.lastIndexOf("/"), filename.lastIndexOf("\\"));
  if (sep === -1) return true; // the current directory
  return fs.existsSync(
    sep === 0 ? filename.slice(0, 1) : filename.slice(0, sep),
  );
}

// better-sqlite3's Database may be called without `new`.
function Database(filenameGiven, options) {
  if (new.target == null) return new Database(filenameGiven, options);
  return Reflect.construct(DatabaseImpl, [filenameGiven, options], new.target);
}

class DatabaseImpl {
  constructor(filenameGiven, options) {
    let buffer;
    if (isBlob(filenameGiven)) {
      buffer = filenameGiven;
      filenameGiven = ":memory:";
    }
    if (filenameGiven == null) filenameGiven = "";
    if (options == null) options = {};
    if (typeof filenameGiven !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (typeof options !== "object") {
      throw new TypeError("Expected second argument to be an options object");
    }
    if ("readOnly" in options) {
      throw new TypeError(
        'Misspelled option "readOnly" should be "readonly"',
      );
    }
    if ("memory" in options) {
      throw new TypeError(
        'Option "memory" was removed in v7.0.0 (use ":memory:" filename instead)',
      );
    }

    const filename = filenameGiven.trim();
    const anonymous = filename === "" || filename === ":memory:";
    const readonly = getBooleanOption(options, "readonly");
    const fileMustExist = getBooleanOption(options, "fileMustExist");
    const timeout = "timeout" in options ? options.timeout : 5000;
    const verbose = "verbose" in options ? options.verbose : null;

    if (readonly && anonymous && !buffer) {
      throw new TypeError("In-memory/temporary databases cannot be readonly");
    }
    if (!Number.isInteger(timeout) || timeout < 0) {
      throw new TypeError(
        'Expected the "timeout" option to be a positive integer',
      );
    }
    if (timeout > 0x7fffffff) {
      throw new RangeError(
        'Option "timeout" cannot be greater than 2147483647',
      );
    }
    if (verbose != null && typeof verbose !== "function") {
      throw new TypeError('Expected the "verbose" option to be a function');
    }
    if (!anonymous && !filename.startsWith("file:")) {
      if (!dirExists(filename)) {
        throw new TypeError(
          "Cannot open database because the directory does not exist",
        );
      }
      if ((fileMustExist || readonly) && !fs.existsSync(filename)) {
        throw new SqliteError(
          "unable to open database file",
          "SQLITE_CANTOPEN",
        );
      }
    }

    // better-sqlite3 opens "" as a private temporary database; node:sqlite
    // has none, and an in-memory one behaves the same for one connection.
    const location = anonymous ? ":memory:" : filename;
    let db;
    try {
      db = new DatabaseSync(location, {
        readOnly: readonly && !buffer,
        timeout,
        enableForeignKeyConstraints: true,
        enableDoubleQuotedStringLiterals: false,
      });
      if (buffer) {
        db.deserialize(buffer);
        if (readonly) db.exec("PRAGMA query_only = 1");
      }
      // better-sqlite3's build defaults (docs/compilation.md): a 16 MB
      // page cache, and defensive mode until unsafeMode()
      db.exec("PRAGMA busy_timeout = 0");
      try {
        db.exec("PRAGMA cache_size = -16000");
      } catch {
        // another connection holds a lock: keep SQLite's default
      }
      db.exec(`PRAGMA busy_timeout = ${timeout}`);
      db.enableDefensive(true);
    } catch (err) {
      throw convertError(err);
    }

    this[kDb] = db;
    this[kState] = {
      safeIntegers: false,
      unsafe: false,
      busy: false,
      iterators: 0,
      verbose: verbose ?? null,
    };
    Object.defineProperty(this, "name", {
      value: filenameGiven,
      enumerable: true,
    });
    Object.defineProperty(this, "memory", {
      value: anonymous,
      enumerable: true,
    });
    Object.defineProperty(this, "readonly", {
      value: readonly,
      enumerable: true,
    });
  }

  get open() {
    return this[kDb].isOpen;
  }

  get inTransaction() {
    const db = this[kDb];
    return db.isOpen && db.isTransaction;
  }

  prepare(sql) {
    return prepareStatement(this, sql, false);
  }

  exec(sql) {
    if (typeof sql !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    assertUsable(this);
    const verbose = this[kState].verbose;
    if (verbose) verbose(sql);
    const state = this[kState];
    state.busy = true;
    try {
      this[kDb].exec(sql);
    } catch (err) {
      throw convertError(err);
    } finally {
      state.busy = false;
    }
    return this;
  }

  transaction(fn) {
    if (typeof fn !== "function") {
      throw new TypeError("Expected first argument to be a function");
    }
    assertOpen(this);
    const controller = getController(this);
    const properties = {
      default: { value: wrapTransaction(fn, this, controller.default) },
      deferred: { value: wrapTransaction(fn, this, controller.deferred) },
      immediate: { value: wrapTransaction(fn, this, controller.immediate) },
      exclusive: { value: wrapTransaction(fn, this, controller.exclusive) },
      database: { value: this, enumerable: true },
    };
    Object.defineProperties(properties.default.value, properties);
    Object.defineProperties(properties.deferred.value, properties);
    Object.defineProperties(properties.immediate.value, properties);
    Object.defineProperties(properties.exclusive.value, properties);
    return properties.default.value;
  }

  pragma(source, options) {
    if (options == null) options = {};
    if (typeof source !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (typeof options !== "object") {
      throw new TypeError("Expected second argument to be an options object");
    }
    const simple = getBooleanOption(options, "simple");
    assertUsable(this);
    const sql = `PRAGMA ${source}`;
    if (hasTrailingStatement(sql)) {
      throw new RangeError(
        "The supplied SQL string contains more than one statement",
      );
    }
    const stmt = prepareStatement(this, sql, true);
    if (!stmt.reader) {
      stmt.run();
      return simple ? undefined : [];
    }
    return simple ? stmt.pluck().get() : stmt.all();
  }

  function(name, options, fn) {
    if (options == null) options = {};
    if (typeof options === "function") {
      fn = options;
      options = {};
    }
    if (typeof name !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (typeof fn !== "function") {
      throw new TypeError("Expected last argument to be a function");
    }
    if (typeof options !== "object") {
      throw new TypeError("Expected second argument to be an options object");
    }
    if (!name) {
      throw new TypeError(
        "User-defined function name cannot be an empty string",
      );
    }
    const safeIntegers = "safeIntegers" in options
      ? getBooleanOption(options, "safeIntegers")
      : this[kState].safeIntegers;
    const deterministic = getBooleanOption(options, "deterministic");
    const directOnly = getBooleanOption(options, "directOnly");
    const varargs = getBooleanOption(options, "varargs");
    const argCount = varargs ? -1 : argumentCount(fn, 0);
    assertUsable(this);
    // integers always arrive as BigInt, converted back unless safeIntegers
    const wrapped = function (...args) {
      return checkResult(
        Reflect.apply(fn, this, convertArgs(args, safeIntegers)),
        name,
      );
    };
    try {
      this[kDb].function(name, {
        deterministic,
        directOnly,
        varargs,
        useBigIntArguments: true,
      }, varargs ? wrapped : withLength(wrapped, argCount));
    } catch (err) {
      throw convertError(err);
    }
    return this;
  }

  aggregate(name, options) {
    if (typeof name !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (typeof options !== "object" || options === null) {
      throw new TypeError("Expected second argument to be an options object");
    }
    if (!name) {
      throw new TypeError(
        "User-defined function name cannot be an empty string",
      );
    }
    const start = "start" in options ? options.start : null;
    const step = getFunctionOption(options, "step", true);
    const inverse = getFunctionOption(options, "inverse", false);
    const result = getFunctionOption(options, "result", false);
    const safeIntegers = "safeIntegers" in options
      ? getBooleanOption(options, "safeIntegers")
      : this[kState].safeIntegers;
    const deterministic = getBooleanOption(options, "deterministic");
    const directOnly = getBooleanOption(options, "directOnly");
    const varargs = getBooleanOption(options, "varargs");
    let argCount = -1;
    if (!varargs) {
      argCount = Math.max(
        argumentCount(step, 1),
        inverse ? argumentCount(inverse, 1) : 0,
      );
    }
    assertUsable(this);
    // better-sqlite3 keeps the accumulator when step()/inverse() return
    // undefined, and passes integers per safeIntegers
    const keep = (f) =>
      function (acc, ...args) {
        const next = Reflect.apply(f, this, [
          acc,
          ...convertArgs(args, safeIntegers),
        ]);
        return next === undefined ? acc : next;
      };
    const definition = {
      // a function runs per aggregation; any other value is the start value
      start: typeof start === "function" ? () => start() : () => start,
      step: keep(step),
      deterministic,
      directOnly,
      varargs,
      useBigIntArguments: true,
    };
    if (inverse) definition.inverse = keep(inverse);
    definition.result = result
      ? (acc) => checkResult(result(acc), name)
      : (acc) => checkResult(acc, name);
    if (!varargs) {
      definition.step = withLength(definition.step, argCount + 1);
      if (definition.inverse) {
        definition.inverse = withLength(definition.inverse, argCount + 1);
      }
    }
    try {
      this[kDb].aggregate(name, definition);
    } catch (err) {
      throw convertError(err);
    }
    return this;
  }

  table() {
    assertUsable(this);
    throw new TypeError(
      "Virtual tables (Database#table()) are not available in Deno's better-sqlite3",
    );
  }

  loadExtension(path, entryPoint) {
    if (typeof path !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (entryPoint !== undefined && typeof entryPoint !== "string") {
      throw new TypeError("Expected second argument to be a string");
    }
    assertUsable(this);
    const db = this[kDb];
    try {
      db.enableLoadExtension(true);
      try {
        db.loadExtension(path, entryPoint);
      } finally {
        db.enableLoadExtension(false);
      }
    } catch (err) {
      throw convertError(err);
    }
    return this;
  }

  // better-sqlite3's backup() is an async function: argument errors reject,
  // and the copy starts after the call returns.
  async backup(filename, options) {
    if (options == null) options = {};
    if (typeof filename !== "string") {
      throw new TypeError("Expected first argument to be a string");
    }
    if (typeof options !== "object") {
      throw new TypeError("Expected second argument to be an options object");
    }
    filename = filename.trim();
    const attached = "attached" in options ? options.attached : "main";
    const progress = "progress" in options ? options.progress : null;
    if (!filename) {
      throw new TypeError("Backup filename cannot be an empty string");
    }
    if (filename === ":memory:") {
      throw new TypeError('Invalid backup filename ":memory:"');
    }
    if (typeof attached !== "string") {
      throw new TypeError('Expected the "attached" option to be a string');
    }
    if (!attached) {
      throw new TypeError('The "attached" option cannot be an empty string');
    }
    if (progress != null && typeof progress !== "function") {
      throw new TypeError('Expected the "progress" option to be a function');
    }
    if (!dirExists(filename)) {
      throw new TypeError(
        "Cannot save backup because the directory does not exist",
      );
    }
    assertOpen(this);
    const db = this[kDb];
    await null;
    let lastTotal = 0;
    try {
      const totalPages = await sqlite.backup(db, filename, {
        source: attached,
        target: "main",
        rate: 100,
        progress: progress
          ? ({ totalPages, remainingPages }) => {
            lastTotal = totalPages;
            const next = Reflect.apply(progress, undefined, [{
              totalPages,
              remainingPages,
            }]);
            if (
              next !== undefined &&
              (typeof next !== "number" || next !== next)
            ) {
              throw new TypeError(
                "Expected progress callback to return a number or undefined",
              );
            }
          }
          : undefined,
      });
      return { totalPages: totalPages ?? lastTotal, remainingPages: 0 };
    } catch (err) {
      throw convertError(err);
    }
  }

  serialize(options) {
    if (options == null) options = {};
    if (typeof options !== "object") {
      throw new TypeError("Expected first argument to be an options object");
    }
    const attached = "attached" in options ? options.attached : "main";
    if (typeof attached !== "string") {
      throw new TypeError('Expected the "attached" option to be a string');
    }
    if (!attached) {
      throw new TypeError('The "attached" option cannot be an empty string');
    }
    assertUsable(this, { iterating: true });
    let bytes;
    try {
      bytes = this[kDb].serialize(attached);
    } catch (err) {
      throw convertError(err);
    }
    return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  defaultSafeIntegers(toggle = true) {
    if (typeof toggle !== "boolean") {
      throw new TypeError("Expected first argument to be a boolean");
    }
    this[kState].safeIntegers = toggle;
    return this;
  }

  unsafeMode(toggle = true) {
    if (typeof toggle !== "boolean") {
      throw new TypeError("Expected first argument to be a boolean");
    }
    assertUsable(this, { iterating: true });
    this[kState].unsafe = toggle;
    try {
      this[kDb].enableDefensive(!toggle);
    } catch (err) {
      throw convertError(err);
    }
    return this;
  }

  close() {
    const db = this[kDb];
    if (db.isOpen) {
      assertUsable(this);
      try {
        db.close();
      } catch (err) {
        throw convertError(err);
      }
    }
    return this;
  }
}

Database.prototype = DatabaseImpl.prototype;
Object.defineProperty(DatabaseImpl.prototype, "constructor", {
  value: Database,
  writable: true,
  enumerable: false,
  configurable: true,
});

Database.prototype[Symbol.for("nodejs.util.inspect.custom")] = function (
  _depth,
  options,
  inspect,
) {
  const { name, open, inTransaction, readonly, memory } = this;
  return `Database ${
    inspect({ name, open, inTransaction, readonly, memory }, options)
  }`;
};

Database.SqliteError = SqliteError;

return { Database, SqliteError };
})();
