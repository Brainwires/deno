// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use serde_json::json;
use thalora_dom::NodeId;
use url::Url;

use crate::ConsoleLevel;
use crate::FormSubmission;
use crate::Idle;
use crate::NavigationRequest;
use crate::PageError;
use crate::PageOptions;
use crate::PageRuntime;
use crate::op_allowlist::ALLOWED_OPS;
use crate::op_allowlist::PAGE_OPS;
use crate::op_policy::DISABLED_OPS;

const PAGE_URL: &str = "https://example.test/dir/page.html";

fn options(html: &str) -> PageOptions {
  let mut options = PageOptions::new(Url::parse(PAGE_URL).unwrap(), html);
  options.user_agent = "TestAgent/1.0".to_string();
  options
}

fn page(html: &str) -> PageRuntime {
  PageRuntime::new(options(html)).unwrap()
}

fn eval(page: &mut PageRuntime, source: &str) -> Value {
  page.evaluate_json(source.to_string()).unwrap()
}

async fn eval_async(
  page: &mut PageRuntime,
  source: &str,
) -> Result<Value, PageError> {
  page
    .evaluate_json_async(source.to_string(), Duration::from_secs(20))
    .await
}

fn run(page: &mut PageRuntime, source: &str) {
  page.execute_script("test.js", source.to_string()).unwrap();
}

fn by_id(page: &PageRuntime, id: &str) -> NodeId {
  page
    .with_document(|doc| doc.get_element_by_id(id))
    .unwrap_or_else(|| panic!("no element with id {id}"))
}

fn console_text(page: &mut PageRuntime) -> String {
  page
    .take_console_messages()
    .into_iter()
    .map(|message| message.text)
    .collect::<Vec<_>>()
    .join("\n")
}

#[tokio::test(flavor = "current_thread")]
async fn dom_round_trip() {
  let mut page = page(
    r#"<!doctype html><html><head><title> The
      title </title></head><body><div id="a" class="x y"><p>one</p></div></body></html>"#,
  );

  assert_eq!(
    eval(&mut page, r##"document.querySelector("#a p").textContent"##),
    json!("one")
  );
  assert_eq!(
    eval(&mut page, r#"document.getElementById("a").innerHTML"#),
    json!("<p>one</p>")
  );
  assert_eq!(eval(&mut page, "document.title"), json!("The title"));
  assert_eq!(eval(&mut page, "document.readyState"), json!("loading"));
  assert_eq!(eval(&mut page, "document.URL"), json!(PAGE_URL));

  // innerHTML set from JS, read back from JS and from Rust.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const a = document.getElementById("a");
        a.innerHTML = "<span>two</span><b>3</b>";
        return [a.children.length, a.firstElementChild.outerHTML, a.textContent];
      })()"#,
    ),
    json!([2, "<span>two</span>", "two3"])
  );
  let a = by_id(&page, "a");
  assert_eq!(
    page.with_document(|doc| doc.inner_html(a).unwrap()),
    "<span>two</span><b>3</b>"
  );

  // A mutation made through the DOM API is the document's.
  run(
    &mut page,
    r#"{
      const section = document.createElement("section");
      section.setAttribute("data-k", "v");
      section.append("text ", document.createElement("i"));
      document.body.appendChild(section);
      document.getElementById("a").classList.add("z");
    }"#,
  );
  page.with_document(|doc| {
    let section = doc
      .query_selector(doc.document(), "body > section[data-k=v]")
      .unwrap()
      .expect("the section a script appended");
    assert_eq!(doc.inner_html(section).unwrap(), "text <i></i>");
    assert_eq!(doc.get_attribute(a, "class").unwrap(), Some("x y z"));
  });

  // One wrapper per node, the right interfaces, illegal constructors.
  assert_eq!(
    eval(
      &mut page,
      r##"[
        document.body === document.body,
        document.querySelector("#a") === document.getElementById("a"),
        document.body.parentNode === document.documentElement,
        Object.prototype.toString.call(document.body),
        Object.prototype.toString.call(document),
        document.body instanceof HTMLElement && document.body instanceof Element &&
          document.body instanceof Node && document.body instanceof EventTarget,
        document.createTextNode("t") instanceof Text,
        document.body.nodeType, document.nodeType, document.body.tagName,
        Object.keys(document.body).length,
        Object.getOwnPropertyDescriptor(Node.prototype, "parentNode").get !== undefined,
        (() => { try { new HTMLElement(); return "constructed"; } catch (e) { return e instanceof TypeError; } })(),
        (() => { try { new Node(); return "constructed"; } catch (e) { return e instanceof TypeError; } })(),
        new Text("made").data,
        document.getElementById("a").dataset.k === undefined,
        document.querySelector("section").dataset.k,
      ]"##,
    ),
    json!([
      true,
      true,
      true,
      "[object HTMLBodyElement]",
      "[object Document]",
      true,
      true,
      1,
      9,
      "BODY",
      0,
      true,
      true,
      true,
      "made",
      true,
      "v",
    ])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn op_table_is_the_allowlist() {
  let page = page("<p>ops</p>");
  let allowed: HashSet<&str> = ALLOWED_OPS
    .iter()
    .flat_map(|list| list.iter().copied())
    .collect();
  let registered = page.op_names();

  // Nothing but the allowed ops: no file system, process, listening socket,
  // FFI, environment, KV or Node op is in a page runtime.
  let unexpected: Vec<&str> = registered
    .iter()
    .copied()
    .filter(|name| name.starts_with("op_") && !allowed.contains(name))
    .collect();
  assert!(
    unexpected.is_empty(),
    "ops outside the page runtime's allowlist: {unexpected:?}"
  );

  // The page's own ops are all there (the list is not stale the other way).
  let registered: HashSet<&str> = registered.into_iter().collect();
  let missing: Vec<&str> = PAGE_OPS
    .iter()
    .copied()
    .filter(|name| !registered.contains(name))
    .collect();
  assert!(
    missing.is_empty(),
    "ops missing from the table: {missing:?}"
  );

  // Every disabled op is a registered one (a renamed op would otherwise
  // come back enabled). That each one throws is checked by the bootstrap of
  // every page, this one included.
  let stale: Vec<&str> = DISABLED_OPS
    .iter()
    .copied()
    .filter(|name| !registered.contains(name) || !allowed.contains(name))
    .collect();
  assert!(
    stale.is_empty(),
    "disabled ops that do not exist: {stale:?}"
  );
  // The `Deno.webrtc` server ops and the ones that reach the process.
  for name in [
    "op_webrtc_network_create",
    "op_webrtc_set_log_level",
    "op_webrtc_sender_write_rtp",
    "op_webrtc_sender_write_frame",
    "op_webrtc_receiver_read_rtp",
    "op_webrtc_fake_media_devices",
    "op_panic",
    "op_eval_context",
  ] {
    assert!(DISABLED_OPS.contains(&name), "{name} is not disabled");
  }
  // `op_print` is replaced, not disabled: it writes to the page's console.
  assert!(!DISABLED_OPS.contains(&"op_print"));
}

#[tokio::test(flavor = "current_thread")]
async fn no_deno_no_node_no_ops() {
  let mut page = page("<p>globals</p>");
  assert_eq!(
    eval(
      &mut page,
      r#"[
        typeof Deno, typeof require, typeof process, typeof Buffer,
        typeof __bootstrap, typeof global,
        globalThis.window === globalThis, self === window, top === window,
        document.defaultView === window,
        window instanceof Window && window instanceof EventTarget,
        navigator.userAgent, location.href, location.pathname, origin,
        Object.getOwnPropertyNames(globalThis)
          .filter((name) => /^op_|^Deno$|bootstrap/i.test(name)).length,
        typeof fetch, typeof setTimeout, typeof URL, typeof crypto.randomUUID(),
        typeof Worker, typeof SharedWorker, typeof caches,
        // What the web extensions have for Deno programs only.
        typeof RTCIdentityAssertion, typeof RTCRtpScriptTransform,
        typeof SFrameEncryptorStream, typeof navigator.mediaDevices,
        typeof navigator.gpu,
      ]"#,
    ),
    json!([
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      true,
      true,
      true,
      true,
      true,
      "TestAgent/1.0",
      PAGE_URL,
      "/dir/page.html",
      "https://example.test",
      0,
      "function",
      "function",
      "function",
      "string",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
      "undefined",
    ])
  );

  // The extension modules cannot be imported from a page.
  run(
    &mut page,
    r#"globalThis.imported = "pending";
      import("ext:core/ops").then(
        () => { imported = "resolved"; },
        () => { imported = "rejected"; },
      );"#,
  );
  page.run_until_idle(Duration::from_secs(5)).await.unwrap();
  assert_eq!(eval(&mut page, "imported"), json!("rejected"));
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_is_confined() {
  let mut options = options("<p>fetch</p>");
  // Allowed by name, and still refused: loopback is on the default deny
  // list, which is checked where the connection is made.
  options.net_allow = vec!["127.0.0.1:1".to_string()];
  let mut page = PageRuntime::new(options).unwrap();
  run(
    &mut page,
    r#"globalThis.denied = "pending";
      fetch("http://127.0.0.1:1/").then(
        () => { denied = "resolved"; },
        (e) => {
          denied = [e instanceof TypeError, String(e.cause && e.cause.code)];
          globalThis.deniedDetail = `${e.name}: ${e.message} / cause: ${e.cause}`;
        },
      );
      globalThis.notAllowed = "pending";
      fetch("http://not-allowed.example/").then(
        () => { notAllowed = "resolved"; },
        (e) => { notAllowed = e.name; },
      );
      globalThis.fileScheme = "pending";
      fetch("file:///etc/hosts").then(
        () => { fileScheme = "resolved"; },
        () => { fileScheme = "rejected"; },
      );"#,
  );
  assert_eq!(
    page.run_until_idle(Duration::from_secs(20)).await.unwrap(),
    Idle::Quiescent
  );
  assert_eq!(
    eval(&mut page, "denied"),
    json!([true, "ERR_ADDRESS_DENIED"]),
    "{}",
    eval(&mut page, "globalThis.deniedDetail ?? null")
  );
  assert_eq!(eval(&mut page, "notAllowed"), json!("NotCapable"));
  assert_eq!(eval(&mut page, "fileScheme"), json!("rejected"));
}

#[tokio::test(flavor = "current_thread")]
async fn timers_run_and_loops_can_be_stopped() {
  let mut page = page("<p>timers</p>");
  run(
    &mut page,
    r#"globalThis.fired = [];
      setTimeout(() => fired.push("timeout"), 10);
      Promise.resolve().then(() => fired.push("microtask"));
      fired.push("script");"#,
  );
  // Microtasks ran when the script ended; the timer needs the event loop.
  assert_eq!(eval(&mut page, "fired"), json!(["script", "microtask"]));
  assert_eq!(
    page.run_until_idle(Duration::from_secs(10)).await.unwrap(),
    Idle::Quiescent
  );
  assert_eq!(
    eval(&mut page, "fired"),
    json!(["script", "microtask", "timeout"])
  );

  // An interval never goes idle: the budget ends the wait.
  run(
    &mut page,
    "globalThis.ticks = 0; setInterval(() => ticks++, 5);",
  );
  assert_eq!(
    page
      .run_until_idle(Duration::from_millis(200))
      .await
      .unwrap(),
    Idle::BudgetExhausted
  );
  assert!(eval(&mut page, "ticks").as_u64().unwrap() > 0);

  // A watchdog on another thread ends a loop that never returns.
  let handle = page.terminate_handle();
  let watchdog = std::thread::spawn(move || {
    std::thread::sleep(Duration::from_millis(300));
    handle.terminate_execution();
  });
  let result = page.execute_script("loop.js", "for (;;) {}".to_string());
  watchdog.join().unwrap();
  assert!(
    matches!(result, Err(PageError::Terminated)),
    "the loop ended with {result:?}"
  );
  // The page is still usable.
  assert_eq!(eval(&mut page, "1 + 1"), json!(2));
}

#[tokio::test(flavor = "current_thread")]
async fn click_runs_default_actions() {
  let mut page = page(
    r#"<form id="f" action="/submit?x=1" method="POST">
        <input type="checkbox" id="c" name="agree">
        <input type="text" id="q" name="q" value="hello">
        <select id="s" name="pick"><option value="1">One<option value="2">Two</select>
        <input type="text" name="off" value="no" disabled>
        <button id="b" name="go" value="1">Go</button>
      </form>
      <a id="link" href="../other?y=2">link</a>"#,
  );
  run(
    &mut page,
    r#"globalThis.log = [];
      {
        const c = document.getElementById("c");
        for (const type of ["click", "input", "change"]) {
          c.addEventListener(type, (e) => {
            log.push(`${type}:${c.checked}:${e.isTrusted}:${e.target === c}`);
          });
        }
      }"#,
  );

  // A checkbox toggles before `click` is dispatched, then `input` and
  // `change` fire.
  let checkbox = by_id(&page, "c");
  let outcome = page.click(checkbox).unwrap();
  assert!(!outcome.default_prevented);
  assert_eq!(outcome.navigation, None);
  assert_eq!(
    eval(&mut page, "log"),
    json!([
      "click:true:true:true",
      "input:true:true:true",
      "change:true:true:true"
    ])
  );
  assert!(page.form_state(checkbox).unwrap().unwrap().checked);

  // A canceled click puts the checkbox back, and fires nothing else.
  run(
    &mut page,
    r#"log.length = 0;
      document.getElementById("c").addEventListener(
        "click", (e) => e.preventDefault(), { once: true });"#,
  );
  let outcome = page.click(checkbox).unwrap();
  assert!(outcome.default_prevented);
  assert_eq!(eval(&mut page, "log"), json!(["click:false:true:true"]));
  assert!(page.form_state(checkbox).unwrap().unwrap().checked);

  // Typing and choosing change the live values and fire input and change.
  run(
    &mut page,
    r#"log.length = 0;
      for (const id of ["q", "s"]) {
        const control = document.getElementById(id);
        control.addEventListener("input", () => log.push(`input:${id}:${control.value}`));
        control.addEventListener("change", () => log.push(`change:${id}:${control.value}`));
      }"#,
  );
  let text = by_id(&page, "q");
  page.set_value(text, "typed").unwrap();
  page.set_value(by_id(&page, "s"), "Two").unwrap();
  assert_eq!(page.form_state(text).unwrap().unwrap().value, "typed");
  assert_eq!(
    eval(&mut page, "log"),
    json!(["input:q:typed", "change:q:typed", "input:s:2", "change:s:2"])
  );
  // The content attribute is the default value, untouched.
  assert_eq!(
    page.with_document(|doc| doc
      .get_attribute(text, "value")
      .unwrap()
      .map(String::from)),
    Some("hello".to_string())
  );
  assert!(matches!(
    page.set_value(by_id(&page, "link"), "x"),
    Err(PageError::NotAFormControl)
  ));

  // A submit button submits its form: the host gets the submission.
  let button = by_id(&page, "b");
  let outcome = page.click(button).unwrap();
  assert!(!outcome.default_prevented);
  assert_eq!(
    outcome.navigation,
    Some(NavigationRequest::Form(FormSubmission {
      action: "https://example.test/submit?x=1".to_string(),
      method: "post".to_string(),
      enctype: "application/x-www-form-urlencoded".to_string(),
      target: String::new(),
      entries: vec![
        ("agree".to_string(), "on".to_string()),
        ("q".to_string(), "typed".to_string()),
        ("pick".to_string(), "2".to_string()),
        ("go".to_string(), "1".to_string()),
      ],
    }))
  );

  // A `submit` listener can cancel it.
  run(
    &mut page,
    r#"document.getElementById("f").addEventListener("submit", (e) => {
        globalThis.submitter = e.submitter.id;
        e.preventDefault();
      });"#,
  );
  let outcome = page.click(button).unwrap();
  assert_eq!(outcome.navigation, None);
  assert_eq!(eval(&mut page, "submitter"), json!("b"));

  // A link is followed by the host, too.
  let outcome = page.click(by_id(&page, "link")).unwrap();
  assert_eq!(
    outcome.navigation,
    Some(NavigationRequest::Link {
      url: "https://example.test/other?y=2".to_string(),
      target: String::new(),
    })
  );

  // Navigations scripts ask for outside a click are queued.
  run(&mut page, r#"location.href = "/next";"#);
  assert_eq!(
    page.take_navigation_requests(),
    vec![NavigationRequest::Location {
      url: "https://example.test/next".to_string(),
      replace: false,
    }]
  );
}

#[tokio::test(flavor = "current_thread")]
async fn dom_errors_are_dom_exceptions() {
  let mut page = page("<p>errors</p>");
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const caught = (f) => {
          try { f(); return "no error"; } catch (e) {
            return [e instanceof DOMException, e.name];
          }
        };
        const parent = document.createElement("div");
        const child = parent.appendChild(document.createElement("span"));
        return [
          caught(() => child.appendChild(parent)),
          caught(() => document.querySelector("!!")),
          caught(() => document.createElement("a b")),
          caught(() => parent.removeChild(document.body)),
          caught(() => parent.classList.add("a b")),
          (() => { try { parent.appendChild("text"); } catch (e) { return e instanceof TypeError; } })(),
          (() => { try { Node.prototype.appendChild.call({}, parent); } catch (e) { return e instanceof TypeError; } })(),
        ];
      })()"#,
    ),
    json!([
      [true, "HierarchyRequestError"],
      [true, "SyntaxError"],
      [true, "InvalidCharacterError"],
      [true, "NotFoundError"],
      [true, "InvalidCharacterError"],
      true,
      true,
    ])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn events_propagate_along_the_tree() {
  let mut page = page(
    r#"<div id="outer" onclick="log.push('attribute:' + (this.id))"><span id="inner"></span></div>"#,
  );
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        globalThis.log = [];
        const inner = document.getElementById("inner");
        const outer = document.getElementById("outer");
        window.addEventListener("ping", (e) => log.push(`window-capture:${e.eventPhase}`), true);
        document.addEventListener("ping", () => log.push("document-capture"), true);
        outer.addEventListener("ping", () => log.push("outer-capture"), true);
        inner.addEventListener("ping", (e) =>
          log.push(`inner:${e.eventPhase}:${e.target === inner}:${e.currentTarget === inner}`));
        outer.addEventListener("ping", (e) =>
          log.push(`outer-bubble:${e.eventPhase}:${e.target === inner}:${e.composedPath().length}`));
        window.addEventListener("ping", (e) => log.push(`window-bubble:${e.target === inner}`));
        const notCanceled = inner.dispatchEvent(new CustomEvent("ping", { bubbles: true }));
        log.push(notCanceled);
        inner.dispatchEvent(new Event("ping"));
        return log;
      })()"#,
    ),
    json!([
      "window-capture:1",
      "document-capture",
      "outer-capture",
      "inner:2:true:true",
      "outer-bubble:3:true:6",
      "window-bubble:true",
      true,
      // Not bubbling: capture and target only.
      "window-capture:1",
      "document-capture",
      "outer-capture",
      "inner:2:true:true",
    ])
  );

  // stopPropagation, and an event handler content attribute.
  run(
    &mut page,
    r#"log.length = 0;
      document.body.addEventListener("click", () => log.push("body"));
      document.addEventListener("click", (e) => { log.push("document"); e.stopPropagation(); });
      window.addEventListener("click", () => log.push("window"));"#,
  );
  page.click(by_id(&page, "inner")).unwrap();
  assert_eq!(
    eval(&mut page, "log"),
    json!(["attribute:outer", "body", "document"])
  );

  // The host's lifecycle calls.
  run(
    &mut page,
    r#"log.length = 0;
      document.addEventListener("DOMContentLoaded", () => log.push(`dcl:${document.readyState}`));
      window.addEventListener("DOMContentLoaded", () => log.push("dcl at window"));
      window.onload = (e) => log.push(`load:${document.readyState}:${e.target === window}`);
      document.addEventListener("load", () => log.push("load at document"), true);"#,
  );
  page.fire_dom_content_loaded().unwrap();
  page.fire_load().unwrap();
  assert_eq!(
    eval(&mut page, "log"),
    json!(["dcl:interactive", "dcl at window", "load:complete:true"])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn uncaught_errors_do_not_end_the_page() {
  let mut page = page(r#"<p id="p">errors</p>"#);

  // A script's own exception comes back to the caller.
  let error = page
    .execute_script(
      "boom.js",
      "function f() { throw new RangeError('boom'); } f();".to_string(),
    )
    .unwrap_err();
  match error {
    PageError::Js(info) => {
      assert!(info.message.contains("RangeError: boom"), "{info:?}");
      assert!(info.stack.unwrap().contains("boom.js"));
    }
    other => panic!("expected a JavaScript error, got {other:?}"),
  }

  // A listener's, a timer's and a promise's go to the console.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const p = document.getElementById("p");
        p.addEventListener("x", () => { throw new Error("from a listener"); });
        p.addEventListener("x", () => { globalThis.second = true; });
        p.dispatchEvent(new Event("x"));
        setTimeout(() => { throw new Error("from a timer"); }, 0);
        Promise.reject(new Error("from a promise"));
        console.warn("a warning", { a: 1 });
        return second;
      })()"#,
    ),
    json!(true)
  );
  assert_eq!(
    page.run_until_idle(Duration::from_secs(10)).await.unwrap(),
    Idle::Quiescent
  );
  let messages = page.take_console_messages();
  assert!(
    messages
      .iter()
      .any(|m| m.level == ConsoleLevel::Warn && m.text.contains("a warning")),
    "{messages:?}"
  );
  let text = messages
    .iter()
    .map(|m| m.text.as_str())
    .collect::<Vec<_>>()
    .join("\n");
  for expected in ["from a listener", "from a timer", "from a promise"] {
    assert!(text.contains(expected), "no {expected:?} in {text}");
  }
  assert!(page.take_uncaught_errors().is_empty());
  assert_eq!(eval(&mut page, "1 + 1"), json!(2));
  assert_eq!(console_text(&mut page), "");
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_json_serializes_what_it_can() {
  let mut page = page(r#"<p id="p">text</p>"#);
  assert_eq!(eval(&mut page, "undefined"), Value::Null);
  assert_eq!(
    eval(&mut page, "({ a: [1, 'two', null] })"),
    json!({ "a": [1, "two", null] })
  );
  assert_eq!(eval(&mut page, "10n"), json!("10"));
  assert_eq!(eval(&mut page, "Symbol('s')"), json!("Symbol(s)"));
  assert_eq!(
    eval(&mut page, "(() => { const o = {}; o.o = o; return o; })()"),
    json!("[object Object]")
  );
  assert_eq!(
    eval(&mut page, r#"document.getElementById("p")"#),
    json!(r#"<p id="p">text</p>"#)
  );
}

#[tokio::test(flavor = "current_thread")]
async fn local_storage_is_keyed_by_origin() {
  let dir = std::env::temp_dir().join(format!(
    "deno_web_page_storage_{}_{:?}",
    std::process::id(),
    std::thread::current().id()
  ));
  let _ = std::fs::remove_dir_all(&dir);

  let mut with_storage = options("<p>storage</p>");
  with_storage.storage_dir = Some(dir.clone());
  {
    let mut page = PageRuntime::new(with_storage.clone()).unwrap();
    run(
      &mut page,
      r#"localStorage.setItem("k", "v"); sessionStorage.setItem("s", "1");"#,
    );
  }
  assert!(dir.join("https_3a_2f_2fexample.test").is_dir());
  {
    // The same origin, another page: the item is there, the session's is not.
    let mut page = PageRuntime::new(with_storage.clone()).unwrap();
    assert_eq!(
      eval(
        &mut page,
        r#"[localStorage.getItem("k"), sessionStorage.getItem("s")]"#
      ),
      json!(["v", null])
    );
  }
  {
    // Another origin has its own.
    let mut other = with_storage;
    other.url = Url::parse("https://other.test/").unwrap();
    let mut page = PageRuntime::new(other).unwrap();
    assert_eq!(eval(&mut page, r#"localStorage.getItem("k")"#), Value::Null);
  }
  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn heap_limit_ends_the_page_not_the_process() {
  let mut options = options("<p>memory</p>");
  options.heap_limit_bytes = Some(64 * 1024 * 1024);
  let mut page = PageRuntime::new(options).unwrap();
  let result = page.execute_script(
    "hog.js",
    "const hog = []; for (;;) { hog.push(new Array(100000).fill(hog.length)); }"
      .to_string(),
  );
  assert!(
    matches!(result, Err(PageError::OutOfMemory)),
    "the allocation loop ended with {result:?}"
  );
  // The runtime is finished.
  assert!(matches!(
    page.evaluate_json("1".to_string()),
    Err(PageError::OutOfMemory)
  ));
}

#[test]
fn a_page_runtime_needs_a_tokio_runtime() {
  assert!(matches!(
    PageRuntime::new(options("<p>none</p>")),
    Err(PageError::NoTokioRuntime)
  ));
}

#[tokio::test(flavor = "current_thread")]
async fn bad_options_are_refused() {
  let mut bad_deny = options("<p>options</p>");
  bad_deny.deny_addresses = Some(vec!["not an address".to_string()]);
  assert!(matches!(
    PageRuntime::new(bad_deny),
    Err(PageError::InvalidOptions(_))
  ));
}

#[tokio::test(flavor = "current_thread")]
async fn websocket_is_confined() {
  let mut options = options("<p>websocket</p>");
  // Allowed by name, and still refused: the deny list holds for the
  // connector a WebSocket makes, as it does for fetch.
  options.net_allow = vec!["127.0.0.1:1".to_string()];
  let mut page = PageRuntime::new(options).unwrap();
  run(
    &mut page,
    r#"globalThis.events = [];
      {
        const ws = new WebSocket("ws://127.0.0.1:1/");
        ws.onerror = (e) => events.push([
          "error",
          e.error instanceof DOMException,
          e.error.name,
          String(e.error.cause && e.error.cause.code),
        ]);
        ws.onclose = (e) => events.push(["close", e.code, ws.readyState]);
      }
      globalThis.stream = "pending";
      new WebSocketStream("ws://127.0.0.1:1/").opened.then(
        () => { stream = "opened"; },
        (e) => { stream = [e instanceof DOMException, String(e.cause && e.cause.code)]; },
      );"#,
  );
  assert_eq!(
    page.run_until_idle(Duration::from_secs(20)).await.unwrap(),
    Idle::Quiescent
  );
  assert_eq!(
    eval(&mut page, "events"),
    json!([
      ["error", true, "NetworkError", "ERR_ADDRESS_DENIED"],
      ["close", 1006, 3]
    ])
  );
  assert_eq!(
    eval(&mut page, "stream"),
    json!([true, "ERR_ADDRESS_DENIED"])
  );

  // A host outside `net_allow` is refused before anything connects, and the
  // interface is the browser's: no request headers, no client, no
  // dispatcher (whose options would turn certificate checks off).
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const caught = (f) => {
          try { f(); return "no error"; } catch (e) { return e.name; }
        };
        return [
          caught(() => new WebSocket("ws://not-allowed.example/")),
          caught(() => new WebSocket("ws://127.0.0.1:1/", { headers: { "x-a": "b" } })),
          caught(() => new WebSocket("ws://127.0.0.1:1/", {
            dispatcher: {
              [Symbol.for("Deno.internal.node.undici.dispatcherOptions")]: {
                unsafelyIgnoreCertificateErrors: true,
              },
            },
          })),
          caught(() => new WebSocket("ws://127.0.0.1:1/", ["a", "a"])),
          WebSocket.name, WebSocket.OPEN, WebSocket.prototype.constructor === WebSocket,
          Object.getOwnPropertyDescriptor(globalThis, "WebSocket").get === undefined,
          typeof CloseEvent, typeof MessageEvent, typeof WebSocketError,
        ];
      })()"#,
    ),
    json!([
      "NotCapable",
      "TypeError",
      "TypeError",
      "SyntaxError",
      "WebSocket",
      1,
      true,
      true,
      "function",
      "function",
      "function",
    ])
  );
  let _ = page.run_until_idle(Duration::from_secs(20)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn webrtc_negotiates_without_the_server_extension() {
  let mut page = page("<p>webrtc</p>");
  // No network is needed for an offer: nothing is gathered until
  // `setLocalDescription()`.
  assert_eq!(
    eval_async(
      &mut page,
      r#"const pc = new RTCPeerConnection();
          const channel = pc.createDataChannel("data");
          const offer = await pc.createOffer();
          const result = [
            pc instanceof RTCPeerConnection && pc instanceof EventTarget,
            channel instanceof RTCDataChannel,
            channel.label,
            offer.type,
            offer.sdp.includes("m=application"),
            pc.signalingState,
          ];
          pc.close();
          return result;"#,
    )
    .await
    .unwrap(),
    json!([true, true, "data", "offer", true, "stable"])
  );

  // The interfaces are data properties, as in a browser; `Deno.webrtc` and
  // the interfaces only Deno programs get are not there.
  assert_eq!(
    eval(
      &mut page,
      r#"[
        typeof RTCSessionDescription, typeof RTCIceCandidate,
        typeof RTCCertificate, typeof RTCRtpSender, typeof MediaStream,
        typeof RTCPeerConnection.generateCertificate,
        Object.getOwnPropertyDescriptor(globalThis, "RTCPeerConnection").get === undefined,
        Object.getOwnPropertyDescriptor(globalThis, "RTCPeerConnection").enumerable,
        typeof Deno,
        Object.getOwnPropertyNames(globalThis)
          .filter((name) => /EncodedMediaTrack|RtcNetwork|createNetwork/.test(name)).length,
      ]"#,
    ),
    json!([
      "function",
      "function",
      "function",
      "function",
      "function",
      "function",
      true,
      false,
      "undefined",
      0,
    ])
  );

  // WebRTC Identity needs a worker for the identity provider: a clean
  // NotSupportedError, not a failure to load `deno_runtime`'s worker module.
  assert_eq!(
    eval_async(
      &mut page,
      r#"const pc = new RTCPeerConnection();
          const result = [];
          try {
            pc.setIdentityProvider("idp.example");
            result.push("no error");
          } catch (e) {
            result.push([e instanceof DOMException, e.name]);
          }
          const outcome = (promise) => promise.then(
            () => "resolved",
            (e) => [e instanceof DOMException, e.name],
          );
          result.push(await outcome(pc.peerIdentity));
          result.push(await outcome(pc.getIdentityAssertion()));
          pc.close();
          return result;"#,
    )
    .await
    .unwrap(),
    json!([
      [true, "NotSupportedError"],
      [true, "NotSupportedError"],
      [true, "NotSupportedError"]
    ])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn offscreen_canvas_has_no_gpu() {
  let mut page = page("<p>canvas</p>");
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const canvas = new OffscreenCanvas(4, 2);
        const context = canvas.getContext("bitmaprenderer");
        return [
          context instanceof ImageBitmapRenderingContext,
          context.canvas === canvas,
          canvas.width, canvas.height,
          new OffscreenCanvas(1, 1).getContext("webgpu"),
          typeof navigator.gpu, typeof GPUDevice,
          typeof createImageBitmap, typeof ImageBitmap, typeof ImageData,
        ];
      })()"#,
    ),
    json!([
      true,
      true,
      4,
      2,
      null,
      "undefined",
      "undefined",
      "function",
      "function",
      "function",
    ])
  );

  // createImageBitmap(): from pixels, and an op's error as a DOMException.
  assert_eq!(
    eval_async(
      &mut page,
        r#"const bitmap = await createImageBitmap(new ImageData(3, 2));
          const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);
          const broken = await createImageBitmap(new Blob([png])).then(
            () => "resolved",
            (e) => [e instanceof DOMException, e.name],
          );
          return [bitmap instanceof ImageBitmap, bitmap.width, bitmap.height, broken];"#,
    )
    .await
      .unwrap(),
    json!([true, 3, 2, [true, "InvalidStateError"]])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_json_async_awaits() {
  let mut page = page(r#"<p id="p">async</p>"#);

  // Top-level await, as an expression (with or without the `;`).
  assert_eq!(
    eval_async(&mut page, "await Promise.resolve(42)")
      .await
      .unwrap(),
    json!(42)
  );
  assert_eq!(
    eval_async(
      &mut page,
      "await new Promise((r) => setTimeout(() => r('late'), 20));"
    )
    .await
    .unwrap(),
    json!("late")
  );
  assert_eq!(
    eval_async(&mut page, "await Promise.resolve('c') // a comment")
      .await
      .unwrap(),
    json!("c")
  );
  // Statements: the value is what `return` gives.
  assert_eq!(
    eval_async(
      &mut page,
      "const a = await Promise.resolve(40); const b = await (async () => 2)(); return a + b;"
    )
    .await
    .unwrap(),
    json!(42)
  );
  // No `await`: a classic script, whose completion value is awaited when
  // it is a promise, and whose declarations are the page's.
  assert_eq!(
    eval_async(&mut page, "var kept = 7; Promise.resolve(kept + 1)")
      .await
      .unwrap(),
    json!(8)
  );
  assert_eq!(eval(&mut page, "kept"), json!(7));
  assert_eq!(
    eval_async(&mut page, r#"document.getElementById("p")"#)
      .await
      .unwrap(),
    json!(r#"<p id="p">async</p>"#)
  );
  // The word in a string is not the keyword.
  assert_eq!(
    eval_async(&mut page, "var word = 'await'; word.length")
      .await
      .unwrap(),
    json!(5)
  );

  // A rejection and a throw are the caller's errors.
  for source in [
    "await Promise.reject(new RangeError('rejected'))",
    "await 1; throw new RangeError('rejected')",
  ] {
    match eval_async(&mut page, source).await {
      Err(PageError::Js(info)) => {
        assert!(info.message.contains("RangeError: rejected"), "{info:?}")
      }
      other => panic!("expected a JavaScript error, got {other:?}"),
    }
  }
  match eval_async(&mut page, "await (;").await {
    Err(PageError::Js(info)) => {
      assert!(info.message.contains("SyntaxError"), "{info:?}")
    }
    other => panic!("expected a syntax error, got {other:?}"),
  }

  // A promise nothing will settle, and one that is too late.
  assert!(matches!(
    eval_async(&mut page, "await new Promise(() => {})").await,
    Err(PageError::Stalled)
  ));
  assert!(matches!(
    page
      .evaluate_json_async(
        "await new Promise((r) => setTimeout(r, 60000))".to_string(),
        Duration::from_millis(100),
      )
      .await,
    Err(PageError::Timeout)
  ));
  // The page is still usable.
  assert_eq!(eval(&mut page, "1 + 1"), json!(2));
  assert!(page.take_uncaught_errors().is_empty());
}
