// Copyright 2018-2026 the Deno authors. MIT license.

//! `deno_web_page`: a runtime for hostile web-page JavaScript.
//!
//! A [`PageRuntime`] is a plain `deno_core::JsRuntime` composed of the web
//! extensions only (`deno_webidl`, `deno_web`, `deno_fetch`,
//! `deno_webstorage`, `deno_crypto`) plus this crate's own, which binds
//! Thalora's engine-neutral DOM ([`thalora_dom`]) to V8. There is no
//! `deno_runtime` worker, no `Deno` namespace and no Node: see the README
//! for the security model.
//!
//! This is not a Deno API. Nothing in `deno` or `denort` depends on it;
//! Thalora links it by git revision.

mod dom_ops;
mod host_ops;
mod page;

pub use host_ops::ConsoleLevel;
pub use host_ops::ConsoleMessage;
pub use host_ops::FormSubmission;
pub use host_ops::NavigationRequest;
pub use page::ClickOutcome;
pub use page::DEFAULT_DENY_ADDRESSES;
pub use page::FormState;
pub use page::Idle;
pub use page::JsErrorInfo;
pub use page::NET_ALLOW_ANY;
pub use page::PageError;
pub use page::PageOptions;
pub use page::PageRuntime;
/// The DOM crate the page's document comes from, so that an embedder names
/// the same version this crate was built against.
pub use thalora_dom;
pub use thalora_dom::NodeId;

deno_core::extension!(
  deno_web_page,
  deps = [deno_webidl, deno_web, deno_fetch, deno_webstorage, deno_crypto],
  ops = [
    dom_ops::op_web_page_document_node,
    dom_ops::op_web_page_node_relative,
    dom_ops::op_web_page_child_list,
    dom_ops::op_web_page_node_name,
    dom_ops::op_web_page_element_name,
    dom_ops::op_web_page_doctype_info,
    dom_ops::op_web_page_text_content,
    dom_ops::op_web_page_set_text_content,
    dom_ops::op_web_page_character_data,
    dom_ops::op_web_page_set_character_data,
    dom_ops::op_web_page_is_connected,
    dom_ops::op_web_page_contains,
    dom_ops::op_web_page_insert_before,
    dom_ops::op_web_page_remove_child,
    dom_ops::op_web_page_replace_child,
    dom_ops::op_web_page_remove,
    dom_ops::op_web_page_clone_node,
    dom_ops::op_web_page_create_element,
    dom_ops::op_web_page_create_element_ns,
    dom_ops::op_web_page_create_node,
    dom_ops::op_web_page_get_attribute,
    dom_ops::op_web_page_has_attribute,
    dom_ops::op_web_page_set_attribute,
    dom_ops::op_web_page_remove_attribute,
    dom_ops::op_web_page_attributes,
    dom_ops::op_web_page_serialize,
    dom_ops::op_web_page_set_inner_html,
    dom_ops::op_web_page_parse_fragment,
    dom_ops::op_web_page_query_selector,
    dom_ops::op_web_page_query_selector_all,
    dom_ops::op_web_page_matches,
    dom_ops::op_web_page_closest,
    dom_ops::op_web_page_get_element_by_id,
    dom_ops::op_web_page_get_elements_by,
    host_ops::op_web_page_bootstrap_info,
    host_ops::op_web_page_register_hooks,
    host_ops::op_web_page_console,
    host_ops::op_web_page_navigate,
  ],
  esm_entry_point = "ext:deno_web_page/99_page_main.js",
  esm = [
    "00_dom.js",
    "01_events.js",
    "02_html.js",
    "03_window.js",
    "99_page_main.js",
  ],
  options = {
    dom: dom_ops::PageDom,
    host: host_ops::PageHost,
    permissions: deno_permissions::PermissionsContainer,
    client: deno_fetch::Client,
  },
  state = |state, options| {
    state.put(options.dom);
    state.put(options.host);
    // `deno_fetch`'s ops take the permissions from the op state, and use a
    // client found there before building one of their own.
    state.put(options.permissions);
    state.put(options.client);
  },
);
