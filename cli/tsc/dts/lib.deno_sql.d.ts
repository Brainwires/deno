// Copyright 2018-2026 the Deno authors. MIT license.

/// <reference no-default-lib="true" />
/// <reference lib="esnext" />
/// <reference lib="esnext.disposable" />

declare namespace Deno {
  /** A value that can be bound as a query parameter. Objects are sent as JSON
   * and arrays as Postgres arrays (or JSON for a `json` / `jsonb` parameter).
   *
   * A `Date` is sent as local time with its UTC offset
   * (`2024-01-15T12:00:00.000-06:00`): a `timestamptz` gets that instant, a
   * `timestamp` the local wall-clock time, a `date` the instant's UTC date.
   * An uncast parameter takes the type the server infers from the query, so
   * `select ${42}` returns the text `"42"` (cast it: `${42}::int`).
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
    /** Cancels the query. One that has not started never runs; one waiting
     * for a pooled connection rejects at once; one the server is running
     * (Postgres) gets a cancel request and rejects with
     * `ERR_SQL_QUERY_CANCELLED` (the server's `57014` error in `cause`)
     * unless it finished first. Queries pipelined on the same connection
     * ahead of it are not affected: the request waits for its turn. */
    cancel(): this;
    /** Whether the query has started. */
    readonly active: boolean;
    /** Whether `cancel()` was called. */
    readonly cancelled: boolean;
    /** The rows in batches of at most `size` (default 1), read from a
     * server-side cursor (`DECLARE` / `FETCH`) one batch at a time as the
     * loop asks for them. On the pool the cursor holds a connection, in a
     * transaction of its own, until the loop ends; on a reserved connection
     * or in a transaction it uses that one. Leaving the loop early closes
     * the cursor. SQLite reads the rows first and hands them out in
     * batches.
     *
     * ```ts
     * for await (const rows of sql`select * from events`.cursor(100)) {
     *   await process(rows);
     * }
     * ```
     */
    cursor(size?: number): AsyncIterable<SQLResult<T>>;
    /** The rows one at a time, from a server-side cursor read 100 rows per
     * round trip (see `cursor()`). */
    [Symbol.asyncIterator](): AsyncIterator<T>;
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

  /** An array parameter with an explicit type, from `sql.array()`.
   *
   * @category SQL */
  export interface SQLArrayValue {
    /** The elements. */
    readonly values: readonly unknown[];
    /** The type cast to (`"int4[]"`), if any. */
    readonly cast?: string;
  }

  /** A pooled connection, as `onconnect` / `onclose` see it.
   *
   * @category SQL */
  export interface SQLConnectionInfo {
    /** A number for the connection, unique within its client. */
    id: number;
    /** Parameters the server reported at startup (`server_version`,
     * `TimeZone`, `application_name`, ...). */
    parameters: Record<string, string>;
  }

  /** A notice from the server (`RAISE NOTICE`, warnings), for `onnotice`.
   *
   * @category SQL */
  export interface SQLNotice {
    /** The notice's text. */
    message: string;
    /** `NOTICE`, `WARNING`, `INFO`, `LOG` or `DEBUG`. */
    severity: string;
    /** Its SQLSTATE (`00000` for a plain notice). */
    code: string;
    /** The server's detail message. */
    detail?: string;
    /** The server's hint. */
    hint?: string;
    /** Context. */
    where?: string;
    /** Schema of the object involved. */
    schema?: string;
    /** Table involved. */
    table?: string;
    /** Column involved. */
    column?: string;
    /** Data type involved. */
    dataType?: string;
    /** Constraint involved. */
    constraint?: string;
    /** Server source routine. */
    routine?: string;
  }

  /** A subscription made with `sql.listen()`.
   *
   * @category SQL */
  export interface SQLListener {
    /** The channel. */
    readonly channel: string;
    /** Stops delivering to this callback (UNLISTEN once a channel has no
     * callbacks left; the listening connection closes once none has). */
    unlisten(): Promise<void>;
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
    /** A client certificate chain (PEM), for servers that ask for one
     * (`sslcert` in a connection string names a file instead). */
    cert?: string;
    /** The client certificate's private key (PEM; `sslkey` in a connection
     * string names a file instead). */
    key?: string;
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
    /** Postgres host (default `PGHOST`, else `localhost`). A path (starting
     * with `/`) is the directory of the server's Unix-domain socket
     * (`<host>/.s.PGSQL.<port>`), as in libpq; `?host=/path` in a
     * connection string works the same. A socket needs read and write
     * permission for its path and net permission for it, as
     * `Deno.connect({ transport: "unix" })` does, and never uses TLS. */
    hostname?: string;
    /** Alias of `hostname`. */
    host?: string;
    /** Postgres port (default `PGPORT`, else 5432). */
    port?: number;
    /** The Postgres server's Unix-domain socket itself
     * (`/var/run/postgresql/.s.PGSQL.5432`), instead of a host. */
    path?: string;
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
    /** Called after each new pooled connection is open (`error` is
     * `null`), or failed to open (`error` set, no connection). */
    onconnect?: (error: unknown, connection?: SQLConnectionInfo) => void;
    /** Called when a pooled connection closes: `error` is what broke it, or
     * `null` when it was closed on purpose (idle timeout, `close()`). */
    onclose?: (error: unknown, connection: SQLConnectionInfo) => void;
    /** Called with each notice the server sends (`RAISE NOTICE`,
     * warnings) on any connection of the client. Without it notices are
     * dropped. */
    onnotice?: (notice: SQLNotice) => void;
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
    /** A query from the text of a file, read when the query runs (read
     * permission), with optional parameters (`$1`... / `?1`...). It can't
     * be used as a fragment of another query. */
    file<T = Record<string, unknown>>(
      path: string | URL,
      params?: unknown[],
    ): SQLQuery<T>;
    /** An array parameter sent as `type[]` (`sql.array(ids, "int8")` is
     * `$1::int8[]`); without a type the server infers it. */
    array(values: readonly unknown[], type?: string): SQLArrayValue;
    /** Postgres: subscribes `fn` to `channel` (`LISTEN`, the name taken
     * exactly as given) on a connection of its own, shared by every channel
     * of the client and outside the pool's `max`. `fn` gets each
     * notification's payload, in the order the server sent them;
     * `onlisten` runs once subscribed, and again whenever the connection
     * was lost and is back (it reconnects with backoff and LISTENs again;
     * notifications sent while it was down are lost). While something
     * listens, the connection keeps the process alive.
     *
     * ```ts
     * const { unlisten } = await sql.listen("jobs", (id) => run(id));
     * await sql.notify("jobs", "42");
     * ```
     */
    listen(
      channel: string,
      fn: (payload: string, info: { channel: string; pid: number }) => void,
      onlisten?: () => void,
    ): Promise<SQLListener>;
    /** Postgres: sends `payload` (under 8000 bytes) to the listeners of
     * `channel` (`pg_notify`); it is delivered when the transaction it is
     * sent in commits. */
    notify(channel: string, payload?: string): SQLQuery;
    /** Runs `fn` in a transaction: COMMIT when it settles, ROLLBACK (and
     * rethrow) when it throws.
     *
     * On Postgres, BEGIN goes out with the transaction's first statements
     * (their results wait for BEGIN's answer), and when `fn` returns its
     * queries without awaiting them (`tx => [tx\`...\`, tx\`...\`]`)
     * COMMIT goes right behind them: one round trip for the whole
     * transaction. */
    begin<T>(fn: (sql: SQL) => T | Promise<T>): Promise<Awaited<T>>;
    /** Runs `fn` in a transaction, with `options` appended to `BEGIN`
     * (`"isolation level serializable, read only"`; BEGIN is pipelined
     * only when `options` are transaction modes). */
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
   * Postgres values arrive as: `int2` / `int4` / `oid` / floats as numbers;
   * `int8` and `numeric` as strings (`bigint: true` makes `int8` a
   * `bigint`); `bool`; text types and enums as strings; `json` / `jsonb`
   * parsed; `bytea` as `Uint8Array`; `timestamptz` as `Date`, `timestamp`
   * (without time zone) as a `Date` at that local wall-clock time (as Bun,
   * postgres.js and node-postgres do; a skipped local time is read as
   * `new Date(y, m, d, h, ...)` reads it, a repeated one as its earlier
   * instant) and `date` as a `Date` at UTC midnight (`"infinity"` /
   * `"-infinity"` as strings); `time`, `timetz`,
   * `interval`, `uuid`, `inet`, `cidr`, `macaddr`, `bit`, `money` and
   * `tsvector` as the server's text; arrays as (nested) arrays; composites
   * as objects; ranges as `{ lower, upper, lowerInclusive, upperInclusive,
   * empty }` (an unbounded side is `null`) and multiranges as arrays of
   * them; `point` as `{ x, y }`, `line` as `{ a, b, c }`, `lseg` and `box`
   * as two points, `path` as `{ closed, points }`, `polygon` as an array of
   * points, `circle` as `{ x, y, radius }`. Other types (`tsquery`, ...)
   * arrive as their raw bytes; cast them to `text` to read them.
   *
   * @category SQL */
  export const SQL: SQLConstructor;

  /** A client for `POSTGRES_URL` / `DATABASE_URL` / `PGURL` / `PG_URL` (else
   * the `PG*` variables), created on first access.
   *
   * @category SQL */
  export const sql: SQL;
}
