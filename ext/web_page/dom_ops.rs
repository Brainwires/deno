// Copyright 2018-2026 the Deno authors. MIT license.

//! The DOM ops: a thin, synchronous binding of `thalora_dom::Document`.
//!
//! A node crosses the op boundary as two `u32`s, `(index, generation)`, the
//! halves of `NodeId::to_bits()`. Ops that answer with a node write
//! `[index, generation, nodeType, flavor]` into a `Uint32Array` the caller
//! passes (`flavor`: 0 not an element, 1 an element outside the HTML
//! namespace, 2 an HTML element) and return whether there was a node. Ops
//! that answer with a list return the same four numbers per node, flattened.
//!
//! Every argument comes from page JavaScript: nothing here panics on a stale
//! id, a bad name or a short buffer. A `DomError` becomes the `DOMException`
//! the DOM Standard names for it.
//!
//! Fast calls and `DOMException`s do not mix: an error leaving a fast call is
//! built natively, without running JavaScript, so a class that only has a
//! JavaScript builder (as every `DOMException*` class does) arrives as a
//! plain `Error`. Hence two kinds of ops here:
//!
//! * `#[op2(fast)]`: the traversal and read ops. They fail only on an id
//!   that names no live node or a node of the wrong kind, which the wrappers
//!   rule out before calling (private brands, and nodes are never freed in
//!   this cut).
//! * `#[op2(nofast)]`: the ops a page can make fail (pre-insertion validity,
//!   invalid names, selector syntax). They always throw through the builder,
//!   as the ops that return strings and lists do.

use std::cell::Ref;
use std::cell::RefCell;
use std::cell::RefMut;
use std::rc::Rc;

use deno_core::OpState;
use deno_core::op2;
use deno_error::JsErrorBox;
use thalora_dom::Document;
use thalora_dom::DomError;
use thalora_dom::NodeData;
use thalora_dom::NodeId;

/// The page's document, shared between the ops and the host
/// (`PageRuntime::with_document`).
#[derive(Clone)]
pub(crate) struct PageDom(pub(crate) Rc<RefCell<Document>>);

impl PageDom {
  fn get(&self) -> Result<Ref<'_, Document>, JsErrorBox> {
    self.0.try_borrow().map_err(|_| busy())
  }

  fn get_mut(&self) -> Result<RefMut<'_, Document>, JsErrorBox> {
    self.0.try_borrow_mut().map_err(|_| busy())
  }
}

fn busy() -> JsErrorBox {
  JsErrorBox::new(
    "DOMExceptionInvalidStateError",
    "The document is in use by the host",
  )
}

/// The registered error class (see `99_page_main.js`) and the message for a
/// DOM error. The messages are ours, not the DOM crate's: a selector parse
/// error's detail is not shown to pages.
pub(crate) fn dom_err(err: DomError) -> JsErrorBox {
  let (class, message) = match err {
    DomError::HierarchyRequest => (
      "DOMExceptionHierarchyRequestError",
      "The operation would yield an incorrect node tree.",
    ),
    DomError::NotFound => (
      "DOMExceptionNotFoundError",
      "The object can not be found here.",
    ),
    DomError::InvalidCharacter => (
      "DOMExceptionInvalidCharacterError",
      "The string contains invalid characters.",
    ),
    DomError::Namespace => (
      "DOMExceptionNamespaceError",
      "The operation is not allowed by Namespaces in XML.",
    ),
    DomError::Syntax(_) => (
      "DOMExceptionSyntaxError",
      "The string did not match the expected pattern.",
    ),
    DomError::NotSupported => (
      "DOMExceptionNotSupportedError",
      "The operation is not supported.",
    ),
    DomError::InvalidNodeType => (
      "DOMExceptionInvalidNodeTypeError",
      "The supplied node is incorrect or has an incorrect ancestor for this operation.",
    ),
    DomError::StaleNode(_) => (
      "DOMExceptionInvalidStateError",
      "The node no longer exists.",
    ),
  };
  JsErrorBox::new(class, message)
}

pub(crate) fn node_id(index: u32, generation: u32) -> NodeId {
  NodeId::from_bits((u64::from(generation) << 32) | u64::from(index))
}

fn flavor(data: &NodeData) -> u32 {
  match data.as_element() {
    None => 0,
    Some(element) if element.is_html() => 2,
    Some(_) => 1,
  }
}

fn write_node(
  doc: &Document,
  id: NodeId,
  out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let data = doc.data(id).map_err(dom_err)?;
  let [index, generation, node_type, kind, ..] = out else {
    return Err(JsErrorBox::type_error("The node buffer is too short"));
  };
  *index = id.index();
  *generation = id.generation();
  *node_type = u32::from(data.node_type().code());
  *kind = flavor(data);
  Ok(())
}

fn write_maybe_node(
  doc: &Document,
  id: Option<NodeId>,
  out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  match id {
    Some(id) => {
      write_node(doc, id, out)?;
      Ok(true)
    }
    None => Ok(false),
  }
}

fn push_node(
  doc: &Document,
  id: NodeId,
  list: &mut Vec<u32>,
) -> Result<(), JsErrorBox> {
  let data = doc.data(id).map_err(dom_err)?;
  list.extend([
    id.index(),
    id.generation(),
    u32::from(data.node_type().code()),
    flavor(data),
  ]);
  Ok(())
}

fn node_list(
  doc: &Document,
  ids: impl IntoIterator<Item = NodeId>,
) -> Result<Vec<u32>, JsErrorBox> {
  let mut list = Vec::new();
  for id in ids {
    push_node(doc, id, &mut list)?;
  }
  Ok(list)
}

// Relations `op_web_page_node_relative` answers. Kept in step with `REL_*`
// in `00_dom.js`.
const REL_PARENT: u32 = 0;
const REL_FIRST_CHILD: u32 = 1;
const REL_LAST_CHILD: u32 = 2;
const REL_PREVIOUS_SIBLING: u32 = 3;
const REL_NEXT_SIBLING: u32 = 4;
const REL_PARENT_ELEMENT: u32 = 5;
const REL_FIRST_ELEMENT_CHILD: u32 = 6;
const REL_LAST_ELEMENT_CHILD: u32 = 7;
const REL_PREVIOUS_ELEMENT_SIBLING: u32 = 8;
const REL_NEXT_ELEMENT_SIBLING: u32 = 9;
const REL_ROOT: u32 = 10;
const REL_TEMPLATE_CONTENTS: u32 = 11;

// Well-known nodes `op_web_page_document_node` answers. Kept in step with
// `DOC_*` in `00_dom.js`.
const DOC_DOCUMENT: u32 = 0;
const DOC_DOCUMENT_ELEMENT: u32 = 1;
const DOC_HEAD: u32 = 2;
const DOC_BODY: u32 = 3;
const DOC_DOCTYPE: u32 = 4;

fn is_element(doc: &Document, id: NodeId) -> bool {
  doc.data(id).is_ok_and(|data| data.as_element().is_some())
}

/// The nearest sibling of `id` that is an element, walking with `step`.
fn element_sibling(
  doc: &Document,
  id: NodeId,
  step: impl Fn(&Document, NodeId) -> Result<Option<NodeId>, DomError>,
) -> Result<Option<NodeId>, DomError> {
  let mut next = step(doc, id)?;
  while let Some(candidate) = next {
    if is_element(doc, candidate) {
      return Ok(Some(candidate));
    }
    next = step(doc, candidate)?;
  }
  Ok(None)
}

#[op2(fast)]
pub fn op_web_page_document_node(
  state: &OpState,
  which: u32,
  #[buffer] out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let id = match which {
    DOC_DOCUMENT => Some(doc.document()),
    DOC_DOCUMENT_ELEMENT => doc.document_element(),
    DOC_HEAD => doc.head(),
    DOC_BODY => doc.body(),
    DOC_DOCTYPE => doc.doctype(),
    _ => None,
  };
  write_maybe_node(&doc, id, out)
}

#[op2(fast)]
pub fn op_web_page_node_relative(
  state: &OpState,
  index: u32,
  generation: u32,
  relation: u32,
  #[buffer] out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let id = node_id(index, generation);
  let relative = match relation {
    REL_PARENT => doc.parent(id),
    REL_FIRST_CHILD => doc.first_child(id),
    REL_LAST_CHILD => doc.last_child(id),
    REL_PREVIOUS_SIBLING => doc.previous_sibling(id),
    REL_NEXT_SIBLING => doc.next_sibling(id),
    REL_PARENT_ELEMENT => doc.parent_element(id),
    REL_FIRST_ELEMENT_CHILD => {
      doc.element_children(id).map(|mut children| children.next())
    }
    REL_LAST_ELEMENT_CHILD => {
      doc.element_children(id).map(|children| children.last())
    }
    REL_PREVIOUS_ELEMENT_SIBLING => {
      element_sibling(&doc, id, Document::previous_sibling)
    }
    REL_NEXT_ELEMENT_SIBLING => {
      element_sibling(&doc, id, Document::next_sibling)
    }
    REL_ROOT => doc.root_of(id).map(Some),
    REL_TEMPLATE_CONTENTS => match doc.data(id) {
      Ok(data) if data.as_element().is_some() => doc.template_contents(id),
      Ok(_) => Ok(None),
      Err(err) => Err(err),
    },
    _ => Ok(None),
  }
  .map_err(dom_err)?;
  write_maybe_node(&doc, relative, out)
}

/// `childNodes` (`elements_only` false) or `children` (true), as a snapshot.
#[op2]
#[serde]
pub fn op_web_page_child_list(
  state: &OpState,
  index: u32,
  generation: u32,
  elements_only: bool,
) -> Result<Vec<u32>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let id = node_id(index, generation);
  if elements_only {
    node_list(&doc, doc.element_children(id).map_err(dom_err)?)
  } else {
    node_list(&doc, doc.children(id).map_err(dom_err)?)
  }
}

#[op2]
#[string]
pub fn op_web_page_node_name(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<String, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .node_name(node_id(index, generation))
    .map_err(dom_err)
}

/// `[localName, namespaceURI, prefix]` of an element.
#[op2]
#[serde]
pub fn op_web_page_element_name(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<(String, Option<String>, Option<String>), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let element = doc
    .element_data(node_id(index, generation))
    .map_err(dom_err)?;
  let namespace = element.namespace();
  Ok((
    element.local_name().to_string(),
    (!namespace.is_empty()).then(|| namespace.to_string()),
    element.prefix().map(|prefix| prefix.to_string()),
  ))
}

/// `[name, publicId, systemId]` of a doctype.
#[op2]
#[serde]
pub fn op_web_page_doctype_info(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<(String, String, String), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  match doc.data(node_id(index, generation)).map_err(dom_err)? {
    NodeData::Doctype {
      name,
      public_id,
      system_id,
    } => Ok((name.clone(), public_id.clone(), system_id.clone())),
    _ => Err(dom_err(DomError::InvalidNodeType)),
  }
}

#[op2]
#[string]
pub fn op_web_page_text_content(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<Option<String>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .text_content(node_id(index, generation))
    .map_err(dom_err)
}

#[op2(fast)]
pub fn op_web_page_set_text_content(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] text: &str,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .set_text_content(node_id(index, generation), text)
    .map_err(dom_err)
}

/// The data of a text, comment or processing instruction node.
#[op2]
#[string]
pub fn op_web_page_character_data(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<String, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  doc
    .data(node_id(index, generation))
    .map_err(dom_err)?
    .character_data()
    .map(str::to_string)
    .ok_or_else(|| dom_err(DomError::InvalidNodeType))
}

#[op2(fast)]
pub fn op_web_page_set_character_data(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] data: &str,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .set_data(node_id(index, generation), data)
    .map_err(dom_err)
}

#[op2(fast)]
pub fn op_web_page_is_connected(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .is_connected(node_id(index, generation))
    .map_err(dom_err)
}

#[op2(fast)]
pub fn op_web_page_contains(
  state: &OpState,
  index: u32,
  generation: u32,
  other_index: u32,
  other_generation: u32,
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .contains(
      node_id(index, generation),
      node_id(other_index, other_generation),
    )
    .map_err(dom_err)
}

/// `parent.insertBefore(node, reference)`; `has_reference` false appends.
#[op2(nofast)]
#[allow(clippy::too_many_arguments, reason = "three nodes, two numbers each")]
pub fn op_web_page_insert_before(
  state: &OpState,
  parent_index: u32,
  parent_generation: u32,
  node_index: u32,
  node_generation: u32,
  has_reference: bool,
  reference_index: u32,
  reference_generation: u32,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let reference =
    has_reference.then(|| node_id(reference_index, reference_generation));
  dom
    .get_mut()?
    .insert_before(
      node_id(parent_index, parent_generation),
      node_id(node_index, node_generation),
      reference,
    )
    .map(|_| ())
    .map_err(dom_err)
}

#[op2(nofast)]
pub fn op_web_page_remove_child(
  state: &OpState,
  parent_index: u32,
  parent_generation: u32,
  child_index: u32,
  child_generation: u32,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .remove_child(
      node_id(parent_index, parent_generation),
      node_id(child_index, child_generation),
    )
    .map(|_| ())
    .map_err(dom_err)
}

/// `parent.replaceChild(node, child)`.
#[op2(nofast)]
pub fn op_web_page_replace_child(
  state: &OpState,
  parent_index: u32,
  parent_generation: u32,
  node_index: u32,
  node_generation: u32,
  child_index: u32,
  child_generation: u32,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .replace_child(
      node_id(parent_index, parent_generation),
      node_id(node_index, node_generation),
      node_id(child_index, child_generation),
    )
    .map(|_| ())
    .map_err(dom_err)
}

/// `ChildNode.remove()`.
#[op2(fast)]
pub fn op_web_page_remove(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .remove(node_id(index, generation))
    .map_err(dom_err)
}

#[op2(nofast)]
pub fn op_web_page_clone_node(
  state: &OpState,
  index: u32,
  generation: u32,
  deep: bool,
  #[buffer] out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let mut doc = dom.get_mut()?;
  let copy = doc
    .clone_node(node_id(index, generation), deep)
    .map_err(dom_err)?;
  write_node(&doc, copy, out)
}

#[op2(nofast)]
pub fn op_web_page_create_element(
  state: &OpState,
  #[string] local_name: &str,
  #[buffer] out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let mut doc = dom.get_mut()?;
  let id = doc.create_element(local_name).map_err(dom_err)?;
  write_node(&doc, id, out)
}

/// `createElementNS`; an empty `namespace` is the null namespace.
#[op2(nofast)]
pub fn op_web_page_create_element_ns(
  state: &OpState,
  #[string] namespace: &str,
  #[string] qualified_name: &str,
  #[buffer] out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let mut doc = dom.get_mut()?;
  let namespace = (!namespace.is_empty()).then_some(namespace);
  let id = doc
    .create_element_ns(namespace, qualified_name)
    .map_err(dom_err)?;
  write_node(&doc, id, out)
}

/// A text node (`kind` 0), a comment (1) or a document fragment (2).
#[op2(fast)]
pub fn op_web_page_create_node(
  state: &OpState,
  kind: u32,
  #[string] data: &str,
  #[buffer] out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let mut doc = dom.get_mut()?;
  let id = match kind {
    0 => doc.create_text_node(data),
    1 => doc.create_comment(data),
    2 => doc.create_document_fragment(),
    _ => return Err(dom_err(DomError::NotSupported)),
  };
  write_node(&doc, id, out)
}

#[op2]
#[string]
pub fn op_web_page_get_attribute(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] name: &str,
) -> Result<Option<String>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let value = doc
    .get_attribute(node_id(index, generation), name)
    .map_err(dom_err)?;
  Ok(value.map(str::to_string))
}

#[op2(fast)]
pub fn op_web_page_has_attribute(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] name: &str,
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .has_attribute(node_id(index, generation), name)
    .map_err(dom_err)
}

#[op2(nofast)]
pub fn op_web_page_set_attribute(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] name: &str,
  #[string] value: &str,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .set_attribute(node_id(index, generation), name, value)
    .map_err(dom_err)
}

#[op2(fast)]
pub fn op_web_page_remove_attribute(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] name: &str,
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .remove_attribute(node_id(index, generation), name)
    .map_err(dom_err)
}

/// The attributes of an element, as `[name, value, name, value, ...]` with
/// qualified names, in order.
#[op2]
#[serde]
pub fn op_web_page_attributes(
  state: &OpState,
  index: u32,
  generation: u32,
) -> Result<Vec<String>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let attrs = doc
    .attributes(node_id(index, generation))
    .map_err(dom_err)?;
  let mut list = Vec::with_capacity(attrs.len() * 2);
  for attr in attrs {
    list.push(attr.qualified_name());
    list.push(attr.value.clone());
  }
  Ok(list)
}

/// `innerHTML` (`outer` false) or `outerHTML` (true).
#[op2]
#[string]
pub fn op_web_page_serialize(
  state: &OpState,
  index: u32,
  generation: u32,
  outer: bool,
) -> Result<String, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let id = node_id(index, generation);
  if outer {
    doc.outer_html(id)
  } else {
    doc.inner_html(id)
  }
  .map_err(dom_err)
}

#[op2(nofast)]
pub fn op_web_page_set_inner_html(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] html: &str,
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get_mut()?
    .set_inner_html(node_id(index, generation), html)
    .map_err(dom_err)
}

/// Parses `html` with the element `(index, generation)` as the context and
/// answers with a new detached `DocumentFragment`.
#[op2(nofast)]
pub fn op_web_page_parse_fragment(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] html: &str,
  #[buffer] out: &mut [u32],
) -> Result<(), JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let mut doc = dom.get_mut()?;
  let fragment = doc
    .parse_fragment(node_id(index, generation), html)
    .map_err(dom_err)?;
  write_node(&doc, fragment, out)
}

#[op2(nofast)]
pub fn op_web_page_query_selector(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] selectors: &str,
  #[buffer] out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let found = doc
    .query_selector(node_id(index, generation), selectors)
    .map_err(dom_err)?;
  write_maybe_node(&doc, found, out)
}

#[op2]
#[serde]
pub fn op_web_page_query_selector_all(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] selectors: &str,
) -> Result<Vec<u32>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let found = doc
    .query_selector_all(node_id(index, generation), selectors)
    .map_err(dom_err)?;
  node_list(&doc, found)
}

#[op2(nofast)]
pub fn op_web_page_matches(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] selectors: &str,
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  dom
    .get()?
    .matches(node_id(index, generation), selectors)
    .map_err(dom_err)
}

#[op2(nofast)]
pub fn op_web_page_closest(
  state: &OpState,
  index: u32,
  generation: u32,
  #[string] selectors: &str,
  #[buffer] out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let found = doc
    .closest(node_id(index, generation), selectors)
    .map_err(dom_err)?;
  write_maybe_node(&doc, found, out)
}

#[op2(fast)]
pub fn op_web_page_get_element_by_id(
  state: &OpState,
  #[string] id: &str,
  #[buffer] out: &mut [u32],
) -> Result<bool, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let found = doc.get_element_by_id(id);
  write_maybe_node(&doc, found, out)
}

/// `getElementsByTagName` (`by_class` false) or `getElementsByClassName`
/// (true) under a node, as a snapshot in tree order.
#[op2]
#[serde]
pub fn op_web_page_get_elements_by(
  state: &OpState,
  index: u32,
  generation: u32,
  by_class: bool,
  #[string] query: &str,
) -> Result<Vec<u32>, JsErrorBox> {
  let dom = state.borrow::<PageDom>();
  let doc = dom.get()?;
  let root = node_id(index, generation);
  let mut list = Vec::new();
  if by_class {
    let wanted: Vec<&str> = query.split_ascii_whitespace().collect();
    if wanted.is_empty() {
      return Ok(list);
    }
    for id in doc.descendants(root).map_err(dom_err)? {
      let Some(element) = doc.data(id).ok().and_then(NodeData::as_element)
      else {
        continue;
      };
      let class = element.attr("class").unwrap_or("");
      let has = |name: &str| class.split_ascii_whitespace().any(|c| c == name);
      if wanted.iter().all(|name| has(name)) {
        push_node(&doc, id, &mut list)?;
      }
    }
  } else {
    let lowered = query.to_ascii_lowercase();
    for id in doc.descendants(root).map_err(dom_err)? {
      let Some(element) = doc.data(id).ok().and_then(NodeData::as_element)
      else {
        continue;
      };
      // HTML elements match the lowercased name, others the name as given.
      let wanted = if element.is_html() {
        lowered.as_str()
      } else {
        query
      };
      if query == "*" || &**element.local_name() == wanted {
        push_node(&doc, id, &mut list)?;
      }
    }
  }
  Ok(list)
}
