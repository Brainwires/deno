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
  let read_err = |e: std::io::Error| {
    SqlError::Io(std::io::Error::new(e.kind(), format!("{e}: read '{path}'")))
  };
  // denext: the file that opened is checked again as the object it is (a
  // link swapped in after the path check is refused, not read)
  let mut file = tokio::fs::File::open(&checked).await.map_err(read_err)?;
  state
    .borrow_mut()
    .borrow_mut::<PermissionsContainer>()
    .check_opened(&file, OpenAccessKind::Read, Some(API_NAME))?;
  let mut text = String::new();
  tokio::io::AsyncReadExt::read_to_string(&mut file, &mut text)
    .await
    .map_err(read_err)?;
  Ok(text)
}
