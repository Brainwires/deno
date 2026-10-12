// Copyright 2018-2026 the Deno authors. MIT license.

//! Files the client reads itself: `sql.file(path)` queries and the
//! `sslcert` / `sslkey` / `sslrootcert` files of a connection string, each
//! needing read permission for the path.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_permissions::OpenAccessKind;
use deno_permissions::PermissionsContainer;

use crate::error::SqlError;

const API_NAME: &str = "Deno.SQL";

#[op2]
#[string]
pub async fn op_sql_read_text_file(
  state: Rc<RefCell<OpState>>,
  #[string] path: String,
) -> Result<String, SqlError> {
  let checked = state
    .borrow_mut()
    .borrow_mut::<PermissionsContainer>()
    .check_open(
      Cow::Borrowed(Path::new(&path)),
      OpenAccessKind::Read,
      Some(API_NAME),
    )?
    .into_owned_path();
  tokio::fs::read_to_string(&checked).await.map_err(|e| {
    SqlError::Io(std::io::Error::new(e.kind(), format!("{e}: read '{path}'")))
  })
}
