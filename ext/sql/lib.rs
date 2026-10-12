// Copyright 2018-2026 the Deno authors. MIT license.

//! `Deno.SQL` / `Deno.sql`: a built-in SQL client with Bun's `Bun.sql`
//! surface. Postgres (on `tokio-postgres`) and SQLite (on the runtime's
//! bundled SQLite) behind one tagged-template API; the query builder and the
//! connection pool are in `01_sql.js`, the engines here.

mod error;
mod files;
mod params;
mod pg;
mod pg_decode;
mod rows;
mod sqlite;

pub use error::SqlError;

deno_core::extension!(
  deno_sql,
  deps = [deno_web, deno_net],
  ops = [
    pg::op_sql_pg_connect,
    pg::op_sql_pg_query,
    pg::op_sql_pg_simple,
    pg::op_sql_pg_finish,
    pg::op_sql_pg_is_closed,
    pg::op_sql_pg_next_event,
    pg::op_sql_pg_cancel,
    files::op_sql_read_text_file,
    sqlite::op_sql_sqlite_open,
    sqlite::op_sql_sqlite_query,
    sqlite::op_sql_sqlite_exec,
  ],
  lazy_loaded_js = ["01_sql.js"],
);
