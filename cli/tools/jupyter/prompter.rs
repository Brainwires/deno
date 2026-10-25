// Copyright 2018-2026 the Deno authors. MIT license.

use deno_runtime::deno_permissions::prompter::GetFormattedStackFn;
use deno_runtime::deno_permissions::prompter::PermissionPrompter;
use deno_runtime::deno_permissions::prompter::PromptResponse;
use tokio::sync::mpsc;

use crate::ops::jupyter::PendingInputRequest;
use crate::ops::jupyter::request_input_blocking;

/// denext: asks the notebook frontend, over Jupyter's `stdin` channel, for
/// the permission a cell needs, instead of the terminal a kernel does not
/// have (denoland/deno#35587).
pub struct JupyterPrompter {
  input_tx: mpsc::UnboundedSender<PendingInputRequest>,
}

impl JupyterPrompter {
  pub fn new(input_tx: mpsc::UnboundedSender<PendingInputRequest>) -> Self {
    Self { input_tx }
  }
}

impl PermissionPrompter for JupyterPrompter {
  fn prompt(
    &mut self,
    message: &str,
    name: &str,
    _api_name: Option<&str>,
    is_unary: bool,
    _get_stack: Option<GetFormattedStackFn>,
  ) -> PromptResponse {
    let prompt = prompt_text(message, name, is_unary);
    // `None` is a frontend without stdin support, or a dismissed prompt: a
    // denial, as the terminal prompter denies when it cannot ask.
    parse_answer(
      request_input_blocking(&self.input_tx, prompt, false).as_deref(),
      is_unary,
    )
  }
}

fn prompt_text(message: &str, name: &str, is_unary: bool) -> String {
  let options = if is_unary {
    format!("[y = allow once, A = allow all {name} access, n = deny]")
  } else {
    "[y = allow, n = deny]".to_string()
  };
  format!("Deno requests {message}.\nAllow? {options}")
}

fn parse_answer(answer: Option<&str>, is_unary: bool) -> PromptResponse {
  match answer.map(str::trim) {
    Some("y" | "Y") => PromptResponse::Allow,
    Some("A") if is_unary => PromptResponse::AllowAll,
    _ => PromptResponse::Deny,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn prompt_names_the_choices() {
    assert_eq!(
      prompt_text("env access to \"X\"", "env", true),
      "Deno requests env access to \"X\".\nAllow? [y = allow once, A = allow all env access, n = deny]"
    );
    assert_eq!(
      prompt_text("run access", "run", false),
      "Deno requests run access.\nAllow? [y = allow, n = deny]"
    );
  }

  #[test]
  fn answers() {
    assert!(matches!(parse_answer(Some("y"), true), PromptResponse::Allow));
    assert!(matches!(parse_answer(Some(" Y\n"), false), PromptResponse::Allow));
    assert!(matches!(
      parse_answer(Some("A"), true),
      PromptResponse::AllowAll
    ));
    // "Allow all" is only offered for a unary permission.
    assert!(matches!(parse_answer(Some("A"), false), PromptResponse::Deny));
    assert!(matches!(parse_answer(Some("n"), true), PromptResponse::Deny));
    assert!(matches!(parse_answer(Some("yes"), true), PromptResponse::Deny));
    assert!(matches!(parse_answer(None, true), PromptResponse::Deny));
  }
}
