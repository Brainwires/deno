// Copyright 2018-2026 the Deno authors. MIT license.

//! `node:sea`'s assets: the files a `deno compile` program carries.

use std::path::Path;

use deno_core::OpState;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_error::JsErrorBox;
use deno_fs::FileSystemRc;

/// The contents of the embedded file at the absolute `path`. A `NotFound`
/// error when this program carries no such file (or isn't a compiled
/// program): only the program's own embedded files are ever read, so no
/// permission applies.
#[op2]
pub fn op_node_sea_get_asset(
  state: &mut OpState,
  #[string] path: String,
) -> Result<Uint8Array, JsErrorBox> {
  let fs = state.borrow::<FileSystemRc>();
  match fs.read_embedded_file(Path::new(&path)) {
    Some(Ok(bytes)) => Ok(bytes.into_owned().into()),
    Some(Err(err)) if err.kind() != std::io::ErrorKind::NotFound => Err(
      JsErrorBox::generic(format!("Failed to read the asset {path}: {err}")),
    ),
    _ => Err(JsErrorBox::new(
      "NotFound",
      format!("No embedded file at {path}"),
    )),
  }
}
