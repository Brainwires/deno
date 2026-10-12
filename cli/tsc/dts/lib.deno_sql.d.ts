// Copyright 2018-2026 the Deno authors. MIT license.

/// <reference no-default-lib="true" />
/// <reference lib="esnext" />
/// <reference lib="esnext.disposable" />

declare namespace Deno {
  /** A value that can be bound as a query parameter. Objects are sent as JSON
   * and arrays as Postgres arrays (or JSON for a `json` / `jsonb` parameter).
   *
   * @category SQL */
  export type SQLValue =
    | null
    | undefined
    | boolean
    | number
    | bigint
    | string
    | Date
    | Uint8Array
    | ArrayBuffer
    | ArrayBufferView
    | readonly SQLValue[]
    | { [key: string]: unknown };

  /** The rows of a query, plus what the server reported about it.
   *
   * @category SQL */
  export interface SQLResult<T> extends Array<T> {
    /** Rows affected (INSERT / UPDATE / DELETE) or returned (SELECT). */
    count: number;
    /** The statement's command, such as `"SELECT"` or `"INSERT"`. */
    command: string;
    /** SQLite only: the rowid of the last inserted row. */
    lastInsertRowid?: number | bigint;
  }

  /** A query. It runs when awaited (or `execute()`d) and resolves to its
   * rows; until then it can be interpolated into another query as a
   * fragment.
   *
   * @category SQL */
  export interface SQLQuery<T = Record<string, unknown>>
    extends PromiseLike<SQLResult<T>> {
    /** Rows as arrays of values instead of objects. */
    values(): SQLQuery<unknown[]>;
    /** Rows as arrays of each value's raw bytes. */
    raw(): SQLQuery<(Uint8Array | null)[]>;
    /** Uses the simple query protocol: several statements, no parameters,
     * every value as text. */
    simple(): SQLQuery<T>;
    /** Starts the query now. */
    execute(): this;
    /** Cancels the query if it has not started. */
    cancel(): this;
    /** Whether the query has started. */
    readonly active: boolean;
    /** Whether `cancel()` was called before the query started. */
    readonly cancelled: boolean;
    /** Attaches a rejection handler. */
    catch<R = never>(
      onrejected?: ((reason: unknown) => R | PromiseLike<R>) | null,
    ): Promise<SQLResult<T> | R>;
    /** Attaches a handler run when the query settles. */
    finally(onfinally?: (() => void) | null): Promise<SQLResult<T>>;
  }

  /** The value of `sql(...)` called with an object, an array or names
   * instead of a template: rendered by the keyword before it (`INSERT`,
   * `UPDATE ... SET`, `VALUES`, `IN`, `SELECT`), or as identifiers.
   *
   * @category SQL */
  export interface SQLHelper {
    /** The value or values the helper was created with. */
    readonly first: unknown;
    /** The column names picked, if any. */
    readonly rest: unknown[];
  }

  /** TLS options for a Postgres connection.
   *
   * @category SQL */
  export interface SQLTLSOptions {
    /** Verify the server's certificate and name (default `true`). `false`
     * is `sslmode=require`: encrypted but not verified. */
    rejectUnauthorized?: boolean;
    /** Extra trusted CA certificates (PEM). */
    ca?: string | string[];
    /** The name to verify the certificate against (default: the host). */
    servername?: string;
  }

  /** Options of {@linkcode Deno.SQL}. Times are in seconds.
   *
   * @category SQL */
  export interface SQLOptions {
    /** A connection string: `postgres://`, `sqlite://`, `file:` or
     * `:memory:`. */
    url?: string | URL;
    /** `"postgres"` (default) or `"sqlite"`. */
    adapter?: "postgres" | "postgresql" | "sqlite";
    /** Postgres host (default `PGHOST`, else `localhost`). */
    hostname?: string;
    /** Alias of `hostname`. */
    host?: string;
    /** Postgres port (default `PGPORT`, else 5432). */
    port?: number;
    /** Postgres user (default `PGUSER`, then `USER`, else `postgres`). */
    username?: string;
    /** Alias of `username`. */
    user?: string;
    /** Postgres password, or a function returning it per connection. */
    password?: string | (() => string | Promise<string>);
    /** Postgres database (default `PGDATABASE`, else the user name). */
    database?: string;
    /** Alias of `database`. */
    db?: string;
    /** `true` is `sslmode=verify-full`, `false` is `disable`. */
    tls?: boolean | SQLTLSOptions;
    /** Alias of `tls`. */
    ssl?: boolean | SQLTLSOptions;
    /** libpq's `sslmode` (default `prefer`). */
    sslmode?:
      | "disable"
      | "allow"
      | "prefer"
      | "require"
      | "verify-ca"
      | "verify-full";
    /** Most connections in the pool (default 10; SQLite always 1). */
    max?: number;
    /** Close a connection idle this long (default 0: never). */
    idleTimeout?: number;
    /** Close a connection this old when it is released (default 0: never). */
    maxLifetime?: number;
    /** Give up connecting, or waiting for a pooled connection, after this
     * long (default 30; 0: never). */
    connectionTimeout?: number;
    /** Keep prepared statements per connection (default `true`); `false`
     * runs every query unnamed, for transaction-mode poolers. */
    prepare?: boolean;
    /** Return 64-bit integers as `bigint` instead of strings (Postgres
     * `int8`) or numbers (SQLite). */
    bigint?: boolean;
    /** Runtime parameters sent at connection start (`search_path`, ...). */
    connection?: Record<string, string | number | boolean>;
    /** SQLite database file (default `:memory:`). */
    filename?: string;
    /** SQLite: open read-only. */
    readonly?: boolean;
    /** SQLite: create the file if missing (default `true`). */
    create?: boolean;
    /** Called after each new connection. */
    onconnect?: () => void;
    /** Called when a pooled connection closes. */
    onclose?: (error?: unknown) => void;
  }

  /** A SQL client: a tagged-template function that builds parameterized
   * queries, with the client methods on it.
   *
   * ```ts
   * const sql = new Deno.SQL("postgres://user:pass@localhost/app");
   * const [user] = await sql`select * from users where id = ${id}`;
   * await sql`insert into users ${sql({ name: "Ada", age: 36 })}`;
   * await sql.begin(async (tx) => {
   *   await tx`update accounts set balance = balance - ${n} where id = ${a}`;
   *   await tx`update accounts set balance = balance + ${n} where id = ${b}`;
   * });
   * ```
   *
   * @category SQL */
  export interface SQL {
    /** A query; every interpolated value becomes a parameter. */
    <T = Record<string, unknown>>(
      strings: TemplateStringsArray,
      ...values: unknown[]
    ): SQLQuery<T>;
    /** An identifier (`sql("table")`) or a helper for INSERT / UPDATE / IN /
     * VALUES / SELECT (`sql(object, ...columns)`, `sql(array)`). */
    (value: unknown, ...columns: (string | string[])[]): SQLHelper;

    /** A query from raw text, with optional parameters (`$1`... / `?1`...).
     * The text is not escaped: never build it from untrusted input. */
    unsafe<T = Record<string, unknown>>(
      text: string,
      params?: unknown[],
    ): SQLQuery<T>;
    /** Runs `fn` in a transaction: COMMIT when it settles, ROLLBACK (and
     * rethrow) when it throws. */
    begin<T>(fn: (sql: SQL) => T | Promise<T>): Promise<Awaited<T>>;
    /** Runs `fn` in a transaction, with `options` appended to `BEGIN`. */
    begin<T>(
      options: string,
      fn: (sql: SQL) => T | Promise<T>,
    ): Promise<Awaited<T>>;
    /** Alias of `begin`. */
    transaction<T>(fn: (sql: SQL) => T | Promise<T>): Promise<Awaited<T>>;
    /** A savepoint inside a transaction. */
    savepoint<T>(fn: (sql: SQL) => T | Promise<T>): Promise<Awaited<T>>;
    /** A named savepoint inside a transaction. */
    savepoint<T>(
      name: string,
      fn: (sql: SQL) => T | Promise<T>,
    ): Promise<Awaited<T>>;
    /** A pooled connection for this caller alone, until `release()`. */
    reserve(): Promise<SQL & Disposable>;
    /** Returns a reserved connection to the pool. */
    release(): void;
    /** Closes every connection, waiting for running queries (at most
     * `timeout` seconds; `timeout: 0` closes at once). */
    close(options?: { timeout?: number }): Promise<void>;
    /** Alias of `close`. */
    end(options?: { timeout?: number }): Promise<void>;
    /** The resolved options (without the password). */
    readonly options: Record<string, unknown>;
    /** Closes the client. */
    [Symbol.asyncDispose](): Promise<void>;
  }

  /** An error from `Deno.SQL`.
   *
   * @category SQL */
  export interface SQLError extends Error {
    /** A Postgres SQLSTATE (`"23505"`), a SQLite code
     * (`"SQLITE_CONSTRAINT_UNIQUE"`) or a client code
     * (`"ERR_POSTGRES_CONNECTION_CLOSED"`). */
    code?: string;
  }

  /** An error from Postgres (with the server's fields) or the Postgres
   * client.
   *
   * @category SQL */
  export interface PostgresError extends SQLError {
    /** The SQLSTATE (also in `code`). */
    errno?: string;
    /** `ERROR`, `FATAL`, ... */
    severity?: string;
    /** The server's detail message. */
    detail?: string;
    /** The server's hint. */
    hint?: string;
    /** 1-based character position of the error in the query. */
    position?: number;
    /** Position in an internally generated query. */
    internalPosition?: number;
    /** The internally generated query. */
    internalQuery?: string;
    /** Context of the error. */
    where?: string;
    /** Schema of the object involved. */
    schema?: string;
    /** Table involved. */
    table?: string;
    /** Column involved. */
    column?: string;
    /** Data type involved. */
    dataType?: string;
    /** Constraint violated. */
    constraint?: string;
    /** Server source file. */
    file?: string;
    /** Server source line. */
    line?: string;
    /** Server source routine. */
    routine?: string;
  }

  /** An error from SQLite.
   *
   * @category SQL */
  export interface SQLiteError extends SQLError {
    /** The extended SQLite result code. */
    errno?: number;
    /** Byte offset of a syntax error in the query. */
    byteOffset?: number;
  }

  /** The constructor of a {@linkcode Deno.SQL} error class.
   *
   * @category SQL */
  export interface SQLErrorConstructor<E extends SQLError> {
    /** Creates an error; `options` may carry the error's fields. */
    new (message?: string, options?: ErrorOptions & Partial<E>): E;
    /** The prototype. */
    readonly prototype: E;
  }

  /** The constructor of {@linkcode Deno.SQL} clients.
   *
   * @category SQL */
  export interface SQLConstructor {
    /** A client for a connection string (`postgres://`, `sqlite://`,
     * `file:`, `:memory:`); without one, `POSTGRES_URL` / `DATABASE_URL` /
     * `PGURL` / `PG_URL`, else the `PG*` variables. Nothing connects until
     * the first query. */
    new (url?: string | URL, options?: SQLOptions): SQL;
    /** A client from options. */
    new (options?: SQLOptions): SQL;
    /** Base error class. */
    readonly SQLError: SQLErrorConstructor<SQLError>;
    /** Postgres error class. */
    readonly PostgresError: SQLErrorConstructor<PostgresError>;
    /** SQLite error class. */
    readonly SQLiteError: SQLErrorConstructor<SQLiteError>;
  }

  /** Bun-compatible SQL client for Postgres and SQLite.
   *
   * Requires `--allow-net` for the Postgres host, `--allow-read` and
   * `--allow-write` for a SQLite file, and `--allow-env` to read a default
   * connection string.
   *
   * @category SQL */
  export const SQL: SQLConstructor;

  /** A client for `POSTGRES_URL` / `DATABASE_URL` / `PGURL` / `PG_URL` (else
   * the `PG*` variables), created on first access.
   *
   * @category SQL */
  export const sql: SQL;
}
