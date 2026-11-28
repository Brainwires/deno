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
  options.net_allow = vec!["127.0.0.1:2".to_string()];
  let mut page = PageRuntime::new(options).unwrap();
  run(
    &mut page,
    r#"globalThis.denied = "pending";
      fetch("http://127.0.0.1:2/").then(
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
      requestAnimationFrame((time) => fired.push(`frame:${typeof time}`));
      cancelAnimationFrame(requestAnimationFrame(() => fired.push("canceled")));
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
    json!(["script", "microtask", "timeout", "frame:number"])
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

#[tokio::test(flavor = "current_thread")]
async fn is_trusted_cannot_be_forged() {
  let mut page = page(
    r#"<button id="b">b</button>
      <label id="l" for="c">l</label><input type="checkbox" id="c">"#,
  );
  run(
    &mut page,
    r#"globalThis.seen = [];
      globalThis.kept = [];
      globalThis.inFlight = null;
      globalThis.b = document.getElementById("b");
      b.addEventListener("click", (e) => {
        seen.push(e.isTrusted);
        if (!e.isTrusted) {
          return;
        }
        kept.push(e);
        // A trusted event cannot be sent anywhere else while it is on its
        // way: not to a node, and not to a target without listeners (which
        // would only have changed its `target`).
        const attempts = [];
        for (const target of [document.body, window, new EventTarget()]) {
          try {
            target.dispatchEvent(e);
            attempts.push("dispatched");
          } catch (error) {
            attempts.push(error.name);
          }
        }
        inFlight = [attempts, e.target === b, e.isTrusted];
      });
      // What a listener of `target` sees of `isTrusted` when a script
      // dispatches `ev` there, or the name of what `dispatchEvent` threw.
      globalThis.observe = (target, ev, type = "click") => {
        let seen = "not delivered";
        const listener = (e) => {
          seen = e.isTrusted;
        };
        target.addEventListener(type, listener);
        try {
          target.dispatchEvent(ev);
        } catch (error) {
          seen = error.name;
        }
        target.removeEventListener(type, listener);
        return seen;
      };"#,
  );

  // The host's clicks are trusted, a script's are not.
  let button = by_id(&page, "b");
  for _ in 0..4 {
    page.click(button).unwrap();
  }
  assert_eq!(eval(&mut page, "seen"), json!([true, true, true, true]));
  assert_eq!(
    eval(&mut page, "inFlight"),
    json!([
      [
        "InvalidStateError",
        "InvalidStateError",
        "InvalidStateError"
      ],
      true,
      true
    ])
  );
  assert_eq!(
    eval(
      &mut page,
      r#"seen.length = 0;
        b.dispatchEvent(new MouseEvent("click", { bubbles: true }));
        b.dispatchEvent(new PointerEvent("click"));
        b.dispatchEvent(new Event("click"));
        b.click();
        seen"#,
    ),
    json!([false, false, false, false])
  );
  // A label passes the trust of its click on to its control's, both ways.
  run(
    &mut page,
    r#"seen.length = 0;
      document.getElementById("c").addEventListener(
        "click", (e) => seen.push(e.isTrusted));"#,
  );
  page.click(by_id(&page, "l")).unwrap();
  assert_eq!(
    eval(&mut page, r#"document.getElementById("l").click(); seen"#),
    json!([true, false])
  );

  // The event's own state: nothing to list, nothing to redefine.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const out = {};
        const name = (f) => {
          try {
            return f();
          } catch (error) {
            return error.name;
          }
        };
        const ev = new Event("click");
        out.symbols = [ev, kept[0], new MouseEvent("click"), new CustomEvent("x")]
          .map((e) => Object.getOwnPropertySymbols(e).length);
        out.ownKeys = Reflect.ownKeys(ev);
        out.defineValue = name(() => {
          Object.defineProperty(ev, "isTrusted", { value: true });
          return "defined";
        });
        out.defineGetter = name(() => {
          Object.defineProperty(ev, "isTrusted", { get: () => true });
          return "defined";
        });
        out.reflectDefine = Reflect.defineProperty(ev, "isTrusted", { value: true });
        out.assign = name(() => {
          ev.isTrusted = true;
          return ev.isTrusted;
        });
        out.strictAssign = name(() => {
          "use strict";
          ev.isTrusted = true;
          return "assigned";
        });
        out.delete = Reflect.deleteProperty(ev, "isTrusted");
        out.after = [ev.isTrusted, observe(b, ev), observe(window, ev)];
        return out;
      })()"#,
    ),
    json!({
      "symbols": [0, 0, 0, 0],
      "ownKeys": ["isTrusted"],
      "defineValue": "TypeError",
      "defineGetter": "TypeError",
      "reflectDefine": false,
      "assign": false,
      "strictAssign": "TypeError",
      "delete": false,
      "after": [false, false, false],
    })
  );

  // The prototype, a subclass, another prototype, an heir, a Proxy, a clone.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const out = {};
        const name = (f) => {
          try {
            return f();
          } catch (error) {
            return error.name;
          }
        };

        Object.defineProperty(Event.prototype, "isTrusted", {
          get: () => true,
          configurable: true,
        });
        out.prototype = [
          new Event("click").isTrusted,
          new MouseEvent("click").isTrusted,
          observe(b, new Event("click")),
          observe(window, new Event("click")),
        ];
        delete Event.prototype.isTrusted;

        class Forged extends Event {
          get isTrusted() {
            return true;
          }
        }
        const forged = new Forged("click");
        out.subclass = [forged.isTrusted, observe(b, forged)];
        // A class field is defined on the event, where the accessor is.
        out.field = name(() => {
          new (class extends Event {
            isTrusted = true;
          })("click");
          return "constructed";
        });
        const made = Reflect.construct(Event, ["click"], function () {});
        out.construct = [
          Object.getOwnPropertyDescriptor(made, "isTrusted").configurable,
          made.isTrusted,
        ];

        const swapped = new Event("click");
        Object.setPrototypeOf(swapped, { isTrusted: true });
        out.setPrototypeOf = [swapped.isTrusted, observe(b, swapped)];

        // An object that inherits from a trusted event is not an event.
        const heir = Object.create(kept[0]);
        out.heir = [
          name(() => heir.isTrusted),
          observe(b, heir),
          observe(window, heir),
        ];
        const adopted = { type: "click" };
        Object.setPrototypeOf(adopted, Event.prototype);
        out.adopted = [observe(b, adopted), observe(window, adopted)];

        // A Proxy answers what it likes, as in a browser, and is not an
        // event either: no listener is ever handed one.
        const proxy = new Proxy(kept[0], {
          get: (target, key) =>
            key === "isTrusted" ? true : Reflect.get(target, key),
        });
        out.proxy = [
          proxy.isTrusted,
          observe(b, proxy),
          observe(window, proxy),
          observe(new EventTarget(), proxy),
        ];
        return out;
      })()"#,
    ),
    json!({
      "prototype": [false, false, false, false],
      "subclass": [false, false],
      "field": "TypeError",
      "construct": [false, false],
      "setPrototypeOf": [false, false],
      "heir": ["TypeError", "TypeError", "TypeError"],
      "adopted": ["TypeError", "TypeError"],
      "proxy": [true, "TypeError", "TypeError", "TypeError"],
    })
  );
  // A structured clone of an event, where there is one (a browser has
  // none), is a plain object.
  let clone = eval(
    &mut page,
    r#"(() => {
      let clone;
      try {
        clone = structuredClone(kept[0]);
      } catch {
        return "not cloned";
      }
      if (clone instanceof Event) {
        return "an event";
      }
      return [observe(b, clone), observe(window, clone)];
    })()"#,
  );
  assert!(
    clone == json!("not cloned") || clone == json!(["TypeError", "TypeError"]),
    "{clone}"
  );

  // A trusted event a script received is trusted once: sent again, to a
  // node, the window or any other target, it is a script's event.
  assert_eq!(
    eval(
      &mut page,
      r#"[
        kept.map((e) => e.isTrusted),
        observe(b, kept[0]),
        observe(window, kept[1]),
        observe(new EventTarget(), kept[2]),
        (() => {
          // Without a listener there, too.
          new EventTarget().dispatchEvent(kept[3]);
          return kept[3].isTrusted;
        })(),
        kept.map((e) => e.isTrusted),
      ]"#,
    ),
    json!([
      [true, true, true, true],
      false,
      false,
      false,
      false,
      [false, false, false, false]
    ])
  );

  // What the web extensions dispatch is trusted as in a browser, once.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const controller = new AbortController();
        let abort = null;
        controller.signal.addEventListener("abort", (e) => {
          abort = e;
        });
        controller.abort();
        return [abort.isTrusted, observe(window, abort, "abort"), abort.isTrusted];
      })()"#,
    ),
    json!([true, false, false])
  );
  assert!(page.take_uncaught_errors().is_empty());
}

// The runtime's own events never pass through a `dispatchEvent` a page can
// replace: on the object, on its interface's prototype or on
// `EventTarget.prototype`. A page that replaces them is handed nothing, the
// listeners still get the event, trusted, and what a script dispatches
// itself never is.
#[tokio::test(flavor = "current_thread")]
async fn trusted_events_bypass_a_replaced_dispatch_event() {
  let mut options = options(r#"<div id="d"><button id="b">b</button></div>"#);
  // Allowed by name and refused by the deny list: an `error` and a `close`.
  options.net_allow = vec!["127.0.0.1:1".to_string()];
  let mut page = PageRuntime::new(options).unwrap();
  run(
    &mut page,
    r#"globalThis.captured = [];
      globalThis.seen = [];
      globalThis.realDispatch = EventTarget.prototype.dispatchEvent;
      const capture = (where) =>
        function (event) {
          captured.push([where, event.type, event.isTrusted]);
          // What a page would do with one: send it to the window.
          Reflect.apply(realDispatch, window, [event]);
          return true;
        };
      const replace = (object, where) => {
        Object.defineProperty(object, "dispatchEvent", {
          value: capture(where),
          writable: true,
          configurable: true,
        });
      };
      replace(EventTarget.prototype, "EventTarget.prototype");
      replace(Node.prototype, "Node.prototype");
      replace(WebSocket.prototype, "WebSocket.prototype");
      replace(AbortSignal.prototype, "AbortSignal.prototype");
      replace(window, "window");
      replace(document, "document");
      const record = (name) => (e) => {
        seen.push([name, e.type, e.isTrusted, e.target === e.currentTarget]);
      };
      // A listener of the window would see a captured event sent there.
      for (
        const type of [
          "open", "message", "close", "abort", "click", "unhandledrejection",
        ]
      ) {
        window.addEventListener(type, (e) => {
          if (e.target === window) {
            seen.push(["window", e.type, e.isTrusted, true]);
          }
        });
      }
      window.addEventListener("error", (e) => {
        if (e instanceof ErrorEvent || e.target === window) {
          seen.push(["window", e.type, e.isTrusted, e.target === window]);
        }
      });

      const ws = new WebSocket("ws://127.0.0.1:1/");
      replace(ws, "a WebSocket");
      ws.addEventListener("error", record("ws"));
      ws.onclose = record("ws");

      const timeout = AbortSignal.timeout(1);
      replace(timeout, "a timeout signal");
      timeout.addEventListener("abort", record("timeout"));
      const controller = new AbortController();
      replace(controller.signal, "a signal");
      controller.signal.onabort = record("signal");
      controller.abort();

      globalThis.b = document.getElementById("b");
      replace(b, "a button");
      b.addEventListener("click", record("button"));

      // The window's own events: an uncaught error, an unhandled rejection.
      setTimeout(() => {
        throw new Error("from a timer");
      }, 0);
      Promise.reject(new Error("from a promise"));
      // Keeps the loop alive for the timeout signal, whose timer does not.
      setTimeout(() => {}, 300);"#,
  );
  page.click(by_id(&page, "b")).unwrap();
  assert_eq!(
    page.run_until_idle(Duration::from_secs(20)).await.unwrap(),
    Idle::Quiescent
  );
  assert_eq!(eval(&mut page, "captured"), json!([]));
  let mut seen = eval(&mut page, "seen")
    .as_array()
    .unwrap()
    .iter()
    .map(|entry| entry.to_string())
    .collect::<Vec<_>>();
  seen.sort();
  assert_eq!(
    seen,
    [
      r#"["button","click",true,true]"#,
      r#"["signal","abort",true,true]"#,
      r#"["timeout","abort",true,true]"#,
      r#"["window","error",true,true]"#,
      r#"["window","unhandledrejection",true,true]"#,
      r#"["ws","close",true,true]"#,
      r#"["ws","error",true,true]"#,
    ]
  );

  // The replaced methods are what a script's own call reaches, and the real
  // one never dispatches a trusted event: not a new one, and not one a
  // listener was given.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        captured.length = 0;
        seen.length = 0;
        b.dispatchEvent(new Event("click"));
        const own = captured.map((entry) => entry[0]);
        let kept = null;
        const signal = AbortSignal.abort();
        const controller = new AbortController();
        controller.signal.addEventListener("abort", (e) => {
          kept = e;
        });
        controller.abort();
        const target = new EventTarget();
        let replayed = null;
        target.addEventListener("abort", (e) => {
          replayed = e.isTrusted;
        });
        const before = kept.isTrusted;
        Reflect.apply(realDispatch, target, [kept]);
        return [own, signal.aborted, before, replayed, kept.isTrusted];
      })()"#,
    ),
    json!([["a button"], true, true, false, false])
  );
  let _ = page.take_console_messages();
  assert!(page.take_uncaught_errors().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn event_interfaces_look_like_a_browsers() {
  let mut page = page(r#"<button id="b">b</button>"#);

  // `isTrusted` is [LegacyUnforgeable]: an own accessor of the event, the
  // same function for every event, and not a member of the prototype.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const d = Object.getOwnPropertyDescriptor(new Event("x"), "isTrusted");
        const other = Object.getOwnPropertyDescriptor(
          new MouseEvent("y"),
          "isTrusted",
        );
        let illegal;
        try {
          illegal = d.get.call({});
        } catch (error) {
          illegal = error.name;
        }
        return {
          keys: Object.keys(d).sort(),
          get: typeof d.get,
          set: d.set === undefined,
          enumerable: d.enumerable,
          configurable: d.configurable,
          name: d.get.name,
          shared: d.get === other.get,
          illegal,
          onPrototype: "isTrusted" in Event.prototype,
          value: new Event("x").isTrusted,
        };
      })()"#,
    ),
    json!({
      "keys": ["configurable", "enumerable", "get", "set"],
      "get": "function",
      "set": true,
      "enumerable": true,
      "configurable": false,
      "name": "get isTrusted",
      "shared": true,
      "illegal": "TypeError",
      "onPrototype": false,
      "value": false,
    })
  );

  // No member a browser does not have, and none of `Deno.inspect`'s hooks.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const standard = {
          EventTarget: ["addEventListener", "dispatchEvent", "removeEventListener"],
          Event: [
            "AT_TARGET", "BUBBLING_PHASE", "CAPTURING_PHASE", "NONE", "bubbles",
            "cancelBubble", "cancelable", "composed", "composedPath",
            "currentTarget", "defaultPrevented", "eventPhase", "initEvent",
            "preventDefault", "returnValue", "srcElement",
            "stopImmediatePropagation", "stopPropagation", "target",
            "timeStamp", "type",
          ],
          CustomEvent: ["detail", "initCustomEvent"],
        };
        const out = { extra: [], notEnumerable: [], symbols: [], hooks: [] };
        for (const [name, members] of Object.entries(standard)) {
          const prototype = globalThis[name].prototype;
          for (const key of Object.getOwnPropertyNames(prototype)) {
            if (key === "constructor") {
              continue;
            }
            if (!members.includes(key)) {
              out.extra.push(`${name}.${key}`);
            }
            if (!Object.getOwnPropertyDescriptor(prototype, key).enumerable) {
              out.notEnumerable.push(`${name}.${key}`);
            }
          }
          for (const symbol of Object.getOwnPropertySymbols(prototype)) {
            if (symbol !== Symbol.toStringTag) {
              out.symbols.push(`${name}.${String(symbol)}`);
            }
          }
        }
        const hook = Symbol.for("Deno.privateCustomInspect");
        for (
          const name of [
            "EventTarget", "Event", "CustomEvent", "ErrorEvent", "CloseEvent",
            "MessageEvent", "ProgressEvent", "PromiseRejectionEvent", "UIEvent",
            "MouseEvent", "PointerEvent", "KeyboardEvent", "InputEvent",
            "FocusEvent", "SubmitEvent", "Window", "Node", "HTMLElement",
          ]
        ) {
          if (hook in globalThis[name].prototype) {
            out.hooks.push(name);
          }
        }
        out.initialized = "initialized" in new Event("x");
        out.getParent = Object.getOwnPropertyNames(EventTarget.prototype)
          .includes("getParent");
        out.phase = Object.getOwnPropertyDescriptor(Event.prototype, "AT_TARGET");
        out.phases = [
          new Event("x").NONE,
          new Event("x").CAPTURING_PHASE,
          Event.AT_TARGET,
          new MouseEvent("x").BUBBLING_PHASE,
        ];
        return out;
      })()"#,
    ),
    json!({
      "extra": [],
      "notEnumerable": [],
      "symbols": [],
      "hooks": [],
      "initialized": false,
      "getParent": false,
      "phase": {
        "value": 2,
        "writable": false,
        "enumerable": true,
        "configurable": false,
      },
      "phases": [0, 1, 2, 3],
    })
  );

  // Listeners and event handlers are not properties of their target.
  assert_eq!(
    eval(
      &mut page,
      r#"(() => {
        const symbols = (target) => Object.getOwnPropertySymbols(target).length;
        // A target, and an event handler it has (`on…`).
        const targets = [
          [document.getElementById("b"), "click"],
          [document, "click"],
          [window, "click"],
          [new EventTarget(), null],
          [new AbortController().signal, "abort"],
        ];
        const before = targets.map(([target]) => symbols(target));
        let calls = 0;
        for (const [target, handler] of targets) {
          target.addEventListener("ping", () => calls++);
          target.dispatchEvent(new Event("ping"));
          if (handler !== null) {
            target["on" + handler] = () => calls++;
            target.dispatchEvent(new Event(handler));
          }
        }
        const after = targets.map(([target]) => symbols(target));
        return [calls, before.every((count, i) => count === after[i])];
      })()"#,
    ),
    json!([9, true])
  );

  // The dispatch still works on what was removed from sight.
  let button = by_id(&page, "b");
  run(
    &mut page,
    r#"globalThis.log = [];
      document.getElementById("b").addEventListener("click", (e) => {
        log.push(e.isTrusted, e.eventPhase === Event.AT_TARGET, e instanceof PointerEvent);
      });
      console.log(new Event("logged"));"#,
  );
  page.click(button).unwrap();
  assert_eq!(eval(&mut page, "log"), json!([true, true, true]));
  assert!(console_text(&mut page).contains("Event"));
  assert!(page.take_uncaught_errors().is_empty());

// --- the same-origin policy of the page's requests ---------------------------
//
// `deno_fetch`'s web-origin mode (`ext/fetch/cors.rs`), which a page runtime
// turns on with the origin of `PageOptions::url`. The servers are in the
// test: three ports of 127.0.0.1, which are three origins. The first is the
// page's.

/// A request a test server received.
#[derive(Debug, Clone)]
struct Seen {
  method: String,
  target: String,
  /// Names in lowercase.
  headers: Vec<(String, String)>,
  body: String,
}

impl Seen {
  fn header(&self, name: &str) -> Option<&str> {
    self
      .headers
      .iter()
      .find(|(header, _)| header == name)
      .map(|(_, value)| value.as_str())
  }
}

type SeenLog = std::sync::Arc<std::sync::Mutex<Vec<Seen>>>;

/// The response to a request, which its query describes: `status` and `h`
/// (a header line, any number of them) and `body`, or for an `OPTIONS`
/// request `pstatus` and `ph`. A preflight nothing was said about gets a
/// 200 without CORS headers, which fails it.
fn test_response(method: &str, target: &str) -> String {
  let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
  let params: Vec<(String, String)> =
    url::form_urlencoded::parse(query.as_bytes())
      .into_owned()
      .collect();
  let preflight = method == "OPTIONS";
  let (status_key, header_key) = if preflight {
    ("pstatus", "ph")
  } else {
    ("status", "h")
  };
  let value = |key: &str| {
    params
      .iter()
      .find(|(name, _)| name == key)
      .map(|(_, value)| value.as_str())
  };
  let status = value(status_key).unwrap_or("200");
  let body = if preflight {
    ""
  } else {
    value("body").unwrap_or("")
  };
  let mut response = format!(
    "HTTP/1.1 {status} Status\r\nConnection: close\r\nContent-Length: {}\r\n",
    body.len()
  );
  for (name, line) in &params {
    if name == header_key {
      response.push_str(line);
      response.push_str("\r\n");
    }
  }
  response.push_str("\r\n");
  response.push_str(body);
  response
}

async fn answer_test_request(mut socket: tokio::net::TcpStream, log: SeenLog) {
  use tokio::io::AsyncReadExt;
  use tokio::io::AsyncWriteExt;

  let mut received = Vec::new();
  let mut chunk = [0u8; 4096];
  let head_end = loop {
    if let Some(at) = received.windows(4).position(|w| w == b"\r\n\r\n") {
      break at + 4;
    }
    match socket.read(&mut chunk).await {
      Ok(0) | Err(_) => return,
      Ok(read) => received.extend_from_slice(&chunk[..read]),
    }
  };
  let head = String::from_utf8_lossy(&received[..head_end]).to_string();
  let mut lines = head.split("\r\n");
  let mut request_line = lines.next().unwrap_or_default().split(' ');
  let method = request_line.next().unwrap_or_default().to_string();
  let target = request_line.next().unwrap_or_default().to_string();
  let headers: Vec<(String, String)> = lines
    .filter_map(|line| line.split_once(':'))
    .map(|(name, value)| {
      (name.trim().to_ascii_lowercase(), value.trim().to_string())
    })
    .collect();
  let length = headers
    .iter()
    .find(|(name, _)| name == "content-length")
    .and_then(|(_, value)| value.parse::<usize>().ok())
    .unwrap_or(0);
  while received.len() < head_end + length {
    match socket.read(&mut chunk).await {
      Ok(0) | Err(_) => break,
      Ok(read) => received.extend_from_slice(&chunk[..read]),
    }
  }
  let body = String::from_utf8_lossy(&received[head_end..]).to_string();
  let response = test_response(&method, &target);
  log.lock().unwrap().push(Seen {
    method,
    target,
    headers,
    body,
  });
  let _ = socket.write_all(response.as_bytes()).await;
  let _ = socket.shutdown().await;
}

/// A server on a loopback port that answers one request per connection and
/// records it.
async fn test_server(log: SeenLog) -> u16 {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((socket, _)) = listener.accept().await else {
        return;
      };
      tokio::spawn(answer_test_request(socket, log.clone()));
    }
  });
  port
}

/// What the test pages have: `u(port, params)` (the URL of a response, see
/// `test_response`), `probe(url, init)` (what a page learns from a fetch)
/// and `ACAO(value)`.
const CORS_HELPERS: &str = r#"
globalThis.u = (port, params = {}) => {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    for (const item of [].concat(value)) query.append(key, item);
  }
  return `http://127.0.0.1:${port}/r?${query}`;
};
globalThis.probe = async (url, init) => {
  try {
    const response = await fetch(url, init);
    return {
      type: response.type,
      status: response.status,
      ok: response.ok,
      statusText: response.statusText,
      url: response.url,
      redirected: response.redirected,
      headers: [...response.headers.keys()],
      body: await response.text(),
    };
  } catch (error) {
    return {
      error: `${error.name}: ${error.message}`,
      cause: error.cause === undefined ? null : String(error.cause),
    };
  }
};
globalThis.ACAO = (value) => `Access-Control-Allow-Origin: ${value}`;
"#;

struct CorsPage {
  page: PageRuntime,
  log: SeenLog,
  /// The page's origin, `null` for an opaque one.
  origin: String,
  /// The URL of the page's origin, `/` included.
  origin_url: String,
  page_url: String,
  same: u16,
  other: u16,
}

impl CorsPage {
  /// A page at `/dir/page.html` of the first of three servers, or at `url`.
  async fn new(url: Option<&str>) -> Self {
    let log = SeenLog::default();
    let same = test_server(log.clone()).await;
    let other = test_server(log.clone()).await;
    let third = test_server(log.clone()).await;
    let origin = match url {
      Some(_) => "null".to_string(),
      None => format!("http://127.0.0.1:{same}"),
    };
    let page_url = match url {
      Some(url) => url.to_string(),
      None => format!("{origin}/dir/page.html?q=1#fragment"),
    };
    let mut options =
      PageOptions::new(Url::parse(&page_url).unwrap(), "<p>cors</p>");
    options.user_agent = "TestAgent/1.0".to_string();
    options.net_allow = [same, other, third]
      .iter()
      .map(|port| format!("127.0.0.1:{port}"))
      .collect();
    // The servers are on loopback.
    options.deny_addresses = Some(Vec::new());
    let mut page = PageRuntime::new(options).unwrap();
    run(
      &mut page,
      &format!(
        "globalThis.SAME = {same}; globalThis.OTHER = {other}; \
         globalThis.THIRD = {third}; globalThis.ORIGIN = '{origin}';\n\
         {CORS_HELPERS}"
      ),
    );
    Self {
      page,
      log,
      origin_url: format!("{origin}/"),
      origin,
      page_url,
      same,
      other,
    }
  }

  async fn eval(&mut self, source: &str) -> Value {
    eval_async(&mut self.page, source)
      .await
      .unwrap_or_else(|error| panic!("{source}: {error}"))
  }

  /// The requests the servers received for the response `id`, in order.
  fn seen(&self, id: &str) -> Vec<Seen> {
    let wanted = format!("id={id}");
    self
      .log
      .lock()
      .unwrap()
      .iter()
      .filter(|seen| seen.target.split(['?', '&']).any(|part| part == wanted))
      .cloned()
      .collect()
  }

  fn methods(&self, id: &str) -> Vec<String> {
    self.seen(id).into_iter().map(|seen| seen.method).collect()
  }
}

/// What a page gets when the policy refuses: no detail.
fn blocked() -> Value {
  json!({ "error": "TypeError: Failed to fetch", "cause": null })
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_shares_cross_origin_responses_by_cors_only() {
  let mut t = CorsPage::new(None).await;

  // Same origin: a basic response, readable, without `Set-Cookie`.
  assert_eq!(
    t.eval(
      r#"const url = u(SAME, { id: "same", body: "hello", h: ["Set-Cookie: s=1", "X-Secret: 1"] });
        const r = await probe(url);
        return [r.type, r.status, r.ok, r.body, r.url === url, r.redirected,
          r.headers.includes("x-secret"), r.headers.includes("set-cookie")];"#
    )
    .await,
    json!(["basic", 200, true, "hello", true, false, true, false])
  );
  let seen = t.seen("same");
  assert_eq!(seen.len(), 1);
  assert_eq!(seen[0].method, "GET");
  assert_eq!(seen[0].header("origin"), None);
  // The referrer of a same-origin request is the document URL, without its
  // fragment.
  assert_eq!(
    seen[0].header("referer"),
    Some(t.page_url.trim_end_matches("#fragment"))
  );
  assert_eq!(
    seen[0].header("host"),
    Some(format!("127.0.0.1:{}", t.same).as_str())
  );
  assert_eq!(seen[0].header("user-agent"), Some("TestAgent/1.0"));

  // Another origin without `Access-Control-Allow-Origin`: the request is
  // made, with the page's origin, and nothing of the response is the
  // page's. The reason goes to the console, which a page cannot read.
  let _ = t.page.take_console_messages();
  assert_eq!(
    t.eval(r#"await probe(u(OTHER, { id: "noacao", body: "secret", h: ["X-Secret: 1"] }))"#)
      .await,
    blocked()
  );
  let seen = t.seen("noacao");
  assert_eq!(seen.len(), 1);
  assert_eq!(seen[0].header("origin"), Some(t.origin.as_str()));
  assert_eq!(seen[0].header("referer"), Some(t.origin_url.as_str()));
  assert_eq!(
    seen[0].header("host"),
    Some(format!("127.0.0.1:{}", t.other).as_str())
  );
  let console = console_text(&mut t.page);
  assert!(
    console.contains("has been blocked")
      && console.contains("Access-Control-Allow-Origin"),
    "{console}"
  );
  assert!(!console.contains("secret"), "{console}");

  // The exact origin, or `*`: a CORS response, with the safelisted headers
  // only.
  assert_eq!(
    t.eval(
      r#"const url = u(OTHER, { id: "exact", body: "shared",
          h: [ACAO(ORIGIN), "X-Secret: 1", "Set-Cookie: s=1", "Content-Type: text/plain"] });
        const r = await probe(url);
        return [r.type, r.status, r.body, r.url === url, r.redirected, r.headers];"#
    )
    .await,
    json!([
      "cors",
      200,
      "shared",
      true,
      false,
      ["content-length", "content-type"]
    ])
  );
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "star", body: "shared", h: [ACAO("*")] }));
        return [r.type, r.body, r.headers];"#
    )
    .await,
    json!(["cors", "shared", ["content-length"]])
  );
  // Another origin's name, or the page's origin written another way, is
  // not it.
  for acao in [
    "http://127.0.0.1:1",
    "http://localhost",
    "null",
    "HTTP://127.0.0.1",
  ] {
    assert_eq!(
      t.eval(&format!(
        r#"await probe(u(OTHER, {{ id: "wrong", body: "secret", h: [ACAO("{acao}")] }}))"#
      ))
      .await,
      blocked(),
      "{acao}"
    );
  }
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "slash", body: "secret", h: [ACAO(ORIGIN + "/")] }))"#
    )
    .await,
    blocked()
  );

  // With credentials: `*` is not enough, and neither is the origin without
  // `Access-Control-Allow-Credentials: true`.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "cred-star", body: "secret", h: [ACAO("*"), "Access-Control-Allow-Credentials: true"] }),
        { credentials: "include" })"#
    )
    .await,
    blocked()
  );
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "cred-origin", body: "secret", h: [ACAO(ORIGIN)] }),
        { credentials: "include" })"#
    )
    .await,
    blocked()
  );
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "cred-ok", body: "mine",
          h: [ACAO(ORIGIN), "Access-Control-Allow-Credentials: true"] }),
          { credentials: "include" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["cors", "mine"])
  );
  // There is no cookie store: no request carried a cookie.
  assert!(
    t.log
      .lock()
      .unwrap()
      .iter()
      .all(|seen| seen.header("cookie").is_none()
        && seen.header("authorization").is_none())
  );

  // `Access-Control-Expose-Headers` names what else a page may read; `*`
  // is everything, unless the request has credentials.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "expose", body: "x",
          h: [ACAO("*"), "Access-Control-Expose-Headers: X-Shown, Set-Cookie", "X-Shown: 1", "X-Secret: 1", "Set-Cookie: s=1"] }));
        return r.headers;"#
    )
    .await,
    json!(["content-length", "x-shown"])
  );
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "expose-star", body: "x",
          h: [ACAO("*"), "Access-Control-Expose-Headers: *", "X-Shown: 1", "Set-Cookie: s=1"] }));
        return [r.headers.includes("x-shown"), r.headers.includes("set-cookie")];"#
    )
    .await,
    json!([true, false])
  );
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "expose-cred", body: "x",
          h: [ACAO(ORIGIN), "Access-Control-Allow-Credentials: true", "Access-Control-Expose-Headers: *", "X-Shown: 1"] }),
          { credentials: "include" });
        return r.headers;"#
    )
    .await,
    json!(["content-length"])
  );

  // A `data:` URL is the page's own, whatever the mode.
  assert_eq!(
    t.eval(
      r#"const r = await probe("data:text/plain,inline", { mode: "same-origin" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["basic", "inline"])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_preflights_what_is_not_a_simple_request() {
  let mut t = CorsPage::new(None).await;

  // A method and a header that are not safelisted: an `OPTIONS` request
  // first, which says what is coming and carries none of it. Its answer is
  // cached for `Access-Control-Max-Age`.
  assert_eq!(
    t.eval(
      r#"const url = u(OTHER, { id: "pf", body: "done", h: [ACAO("*")],
          ph: [ACAO("*"), "Access-Control-Allow-Methods: PUT", "Access-Control-Allow-Headers: X-Custom", "Access-Control-Max-Age: 60"] });
        const init = { method: "PUT", headers: { "X-Custom": "1" }, body: "data" };
        const first = await probe(url, init);
        const second = await probe(url, init);
        return [first.type, first.body, second.type, second.body];"#
    )
    .await,
    json!(["cors", "done", "cors", "done"])
  );
  let seen = t.seen("pf");
  assert_eq!(t.methods("pf"), ["OPTIONS", "PUT", "PUT"]);
  let preflight = &seen[0];
  assert_eq!(
    preflight.header("access-control-request-method"),
    Some("PUT")
  );
  assert_eq!(
    preflight.header("access-control-request-headers"),
    Some("x-custom")
  );
  assert_eq!(preflight.header("origin"), Some(t.origin.as_str()));
  assert_eq!(preflight.header("referer"), Some(t.origin_url.as_str()));
  assert_eq!(preflight.header("x-custom"), None);
  assert_eq!(preflight.header("cookie"), None);
  assert_eq!(preflight.body, "");
  let request = &seen[1];
  assert_eq!(request.header("x-custom"), Some("1"));
  assert_eq!(request.header("origin"), Some(t.origin.as_str()));
  assert_eq!(request.body, "data");

  // A content type that is not a form's.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "pf-json", body: "ok", h: [ACAO("*")],
          ph: [ACAO("*"), "Access-Control-Allow-Headers: content-type"] }),
          { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["cors", "ok"])
  );
  assert_eq!(t.methods("pf-json"), ["OPTIONS", "POST"]);
  assert_eq!(
    t.seen("pf-json")[0].header("access-control-request-headers"),
    Some("content-type")
  );

  // A preflight that does not allow the request: the request is never
  // made.
  for (id, preflight, init) in [
    // no CORS headers at all
    (
      "pf-none",
      "[]",
      r#"{ method: "PUT", headers: { "X-Custom": "1" } }"#,
    ),
    // the method is not allowed
    (
      "pf-method",
      r#"[ACAO("*"), "Access-Control-Allow-Methods: DELETE", "Access-Control-Allow-Headers: x-custom"]"#,
      r#"{ method: "PUT", headers: { "X-Custom": "1" } }"#,
    ),
    // the header is not allowed
    (
      "pf-header",
      r#"[ACAO("*"), "Access-Control-Allow-Methods: PUT"]"#,
      r#"{ method: "PUT", headers: { "X-Custom": "1" } }"#,
    ),
    // `Authorization` is not covered by `*`
    (
      "pf-authorization",
      r#"[ACAO("*"), "Access-Control-Allow-Headers: *"]"#,
      r#"{ headers: { "Authorization": "Bearer token" } }"#,
    ),
    // with credentials, `*` covers nothing
    (
      "pf-credentials",
      r#"[ACAO(ORIGIN), "Access-Control-Allow-Credentials: true", "Access-Control-Allow-Methods: *", "Access-Control-Allow-Headers: *"]"#,
      r#"{ method: "PUT", headers: { "X-Custom": "1" }, credentials: "include" }"#,
    ),
  ] {
    assert_eq!(
      t.eval(&format!(
        r#"await probe(u(OTHER, {{ id: "{id}", body: "secret", h: [ACAO("*"), "Access-Control-Allow-Credentials: true"], ph: {preflight} }}), {init})"#
      ))
      .await,
      blocked(),
      "{id}"
    );
    assert_eq!(t.methods(id), ["OPTIONS"], "{id}");
  }
  // A status that is not an ok status.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "pf-status", body: "secret", h: [ACAO("*")], pstatus: "404",
          ph: [ACAO("*"), "Access-Control-Allow-Methods: PUT"] }), { method: "PUT" })"#
    )
    .await,
    blocked()
  );
  assert_eq!(t.methods("pf-status"), ["OPTIONS"]);

  // With credentials, named by name.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "pf-named", body: "ok",
          h: [ACAO(ORIGIN), "Access-Control-Allow-Credentials: true"],
          ph: [ACAO(ORIGIN), "Access-Control-Allow-Credentials: true", "Access-Control-Allow-Methods: PUT", "Access-Control-Allow-Headers: x-custom"] }),
          { method: "PUT", headers: { "X-Custom": "1" }, credentials: "include" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["cors", "ok"])
  );
  assert_eq!(t.methods("pf-named"), ["OPTIONS", "PUT"]);

  // A preflight passed and the response itself is not shared: still
  // nothing.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "pf-then-none", body: "secret",
          ph: [ACAO("*"), "Access-Control-Allow-Methods: PUT"] }), { method: "PUT" })"#
    )
    .await,
    blocked()
  );
  assert_eq!(t.methods("pf-then-none"), ["OPTIONS", "PUT"]);

  // The page's own origin needs none, and a request that is not a GET
  // says where it comes from.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(SAME, { id: "same-put", body: "ok" }),
          { method: "PUT", headers: { "X-Custom": "1" }, body: "data" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["basic", "ok"])
  );
  assert_eq!(t.methods("same-put"), ["PUT"]);
  assert_eq!(
    t.seen("same-put")[0].header("origin"),
    Some(t.origin.as_str())
  );
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_modes_and_redirects_follow_the_origin() {
  let mut t = CorsPage::new(None).await;

  // no-cors: the request is made and the response is opaque.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "nocors", body: "secret", h: ["X-Secret: 1"] }), { mode: "no-cors" })"#
    )
    .await,
    json!({
      "type": "opaque",
      "status": 0,
      "ok": false,
      "statusText": "",
      "url": "",
      "redirected": false,
      "headers": [],
      "body": "",
    })
  );
  assert_eq!(t.methods("nocors"), ["GET"]);
  // ...and is not preflighted, whatever the page asked to send: only the
  // safelisted headers go.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "nocors-post", body: "secret" }),
          { mode: "no-cors", method: "POST", headers: { "X-Custom": "1", "Content-Type": "application/json", "Accept": "text/x" }, body: "{}" });
        return r.type;"#
    )
    .await,
    json!("opaque")
  );
  let seen = t.seen("nocors-post");
  assert_eq!(t.methods("nocors-post"), ["POST"]);
  assert_eq!(seen[0].header("x-custom"), None);
  // (the content type is the body's own, which is a safelisted one)
  assert_eq!(
    seen[0].header("content-type"),
    Some("text/plain;charset=UTF-8")
  );
  assert_eq!(seen[0].header("accept"), Some("text/x"));
  assert_eq!(seen[0].header("origin"), Some(t.origin.as_str()));

  // same-origin: another origin is refused before anything is sent.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "so-other", body: "secret", h: [ACAO("*")] }), { mode: "same-origin" })"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("so-other").is_empty());
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(SAME, { id: "so-same", body: "mine" }), { mode: "same-origin" });
        return [r.type, r.body];"#
    )
    .await,
    json!(["basic", "mine"])
  );

  // A redirect to another origin: the rules of that origin from there on.
  assert_eq!(
    t.eval(
      r#"const target = u(OTHER, { id: "hop2", body: "far", h: [ACAO(ORIGIN)] });
        const r = await probe(u(SAME, { id: "hop1", status: "302", h: ["Location: " + target] }));
        return [r.type, r.body, r.redirected, r.url === target];"#
    )
    .await,
    json!(["cors", "far", true, true])
  );
  assert_eq!(t.seen("hop2")[0].header("origin"), Some(t.origin.as_str()));
  assert_eq!(
    t.eval(
      r#"const target = u(OTHER, { id: "hop2-none", body: "secret" });
        return await probe(u(SAME, { id: "hop1-none", status: "302", h: ["Location: " + target] }));"#
    )
    .await,
    blocked()
  );
  assert_eq!(t.methods("hop2-none"), ["GET"]);

  // A redirect from another origin to a third taints the request's
  // origin: it is `null` from there on, which the last server has to name.
  assert_eq!(
    t.eval(
      r#"const target = u(THIRD, { id: "taint2", body: "tainted", h: [ACAO("null")] });
        const r = await probe(u(OTHER, { id: "taint1", status: "302", h: [ACAO(ORIGIN), "Location: " + target] }));
        return [r.type, r.body, r.redirected, r.url === target];"#
    )
    .await,
    json!(["cors", "tainted", true, true])
  );
  assert_eq!(
    t.seen("taint1")[0].header("origin"),
    Some(t.origin.as_str())
  );
  assert_eq!(t.seen("taint2")[0].header("origin"), Some("null"));
  assert_eq!(
    t.eval(
      r#"const target = u(THIRD, { id: "taint4", body: "secret", h: [ACAO(ORIGIN)] });
        return await probe(u(OTHER, { id: "taint3", status: "302", h: [ACAO(ORIGIN), "Location: " + target] }));"#
    )
    .await,
    blocked()
  );
  // ...and back to the page's own origin, which is another origin's to
  // answer for now.
  assert_eq!(
    t.eval(
      r#"const target = u(SAME, { id: "back2", body: "secret" });
        return await probe(u(OTHER, { id: "back1", status: "302", h: [ACAO(ORIGIN), "Location: " + target] }));"#
    )
    .await,
    blocked()
  );
  assert_eq!(t.seen("back2")[0].header("origin"), Some("null"));

  // The redirect response is another origin's too: without CORS headers
  // it is not followed.
  assert_eq!(
    t.eval(
      r#"const target = u(THIRD, { id: "unshared2", body: "secret", h: [ACAO("*")] });
        return await probe(u(OTHER, { id: "unshared1", status: "302", h: ["Location: " + target] }));"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("unshared2").is_empty());

  // same-origin mode stops at the hop that leaves the origin.
  assert_eq!(
    t.eval(
      r#"const target = u(OTHER, { id: "leave2", body: "secret", h: [ACAO("*")] });
        return await probe(u(SAME, { id: "leave1", status: "302", h: ["Location: " + target] }), { mode: "same-origin" });"#
    )
    .await,
    blocked()
  );
  assert_eq!(t.methods("leave1"), ["GET"]);
  assert!(t.seen("leave2").is_empty());

  // Credentials in the URL of a cross-origin redirect: a network error.
  assert_eq!(
    t.eval(
      r#"const target = `http://user:pw@127.0.0.1:${OTHER}/r?id=userinfo2&h=${encodeURIComponent(ACAO("*"))}`;
        return await probe(u(SAME, { id: "userinfo1", status: "302", h: ["Location: " + target] }));"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("userinfo2").is_empty());

  // redirect "manual": an opaque-redirect response, nothing followed;
  // "error": a network error.
  assert_eq!(
    t.eval(
      r#"const target = u(SAME, { id: "manual2", body: "secret" });
        const url = u(SAME, { id: "manual1", status: "302", h: ["Location: " + target, "X-Secret: 1"] });
        const r = await probe(url, { redirect: "manual" });
        return [r.type, r.status, r.headers, r.body, r.redirected, r.url === url];"#
    )
    .await,
    json!(["opaqueredirect", 0, [], "", false, true])
  );
  assert!(t.seen("manual2").is_empty());
  assert_eq!(
    t.eval(
      r#"const target = u(SAME, { id: "error2", body: "secret" });
        return await probe(u(SAME, { id: "error1", status: "302", h: ["Location: " + target] }), { redirect: "error" });"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("error2").is_empty());
  // A no-cors request has to follow redirects.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "nocors-manual" }), { mode: "no-cors", redirect: "manual" })"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("nocors-manual").is_empty());
  // A same-origin redirect stays basic.
  assert_eq!(
    t.eval(
      r#"const target = u(SAME, { id: "stay2", body: "near" });
        const r = await probe(u(SAME, { id: "stay1", status: "302", h: ["Location: " + target] }));
        return [r.type, r.body, r.redirected, r.url === target];"#
    )
    .await,
    json!(["basic", "near", true, true])
  );
}

#[tokio::test(flavor = "current_thread")]
async fn a_page_cannot_set_the_headers_a_browser_sets() {
  let mut t = CorsPage::new(None).await;

  // Forbidden request headers are dropped (the request stays a simple one:
  // no preflight), and the server sees the browser's.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(OTHER, { id: "forbidden", body: "ok", h: [ACAO("*")] }), {
          headers: {
            "Host": "evil.example",
            "Cookie": "session=stolen",
            "Origin": "https://evil.example",
            "Referer": "https://evil.example/",
            "Sec-Fetch-Site": "same-origin",
            "Proxy-Authorization": "Basic x",
            "Via": "evil",
            "X-HTTP-Method-Override": "TRACE",
          },
        });
        return [r.type, r.body];"#
    )
    .await,
    json!(["cors", "ok"])
  );
  let seen = t.seen("forbidden");
  assert_eq!(t.methods("forbidden"), ["GET"]);
  assert_eq!(
    seen[0].header("host"),
    Some(format!("127.0.0.1:{}", t.other).as_str())
  );
  assert_eq!(seen[0].header("cookie"), None);
  assert_eq!(seen[0].header("origin"), Some(t.origin.as_str()));
  assert_eq!(seen[0].header("referer"), Some(t.origin_url.as_str()));
  assert_eq!(seen[0].header("sec-fetch-site"), None);
  assert_eq!(seen[0].header("proxy-authorization"), None);
  assert_eq!(seen[0].header("via"), None);
  assert_eq!(seen[0].header("x-http-method-override"), None);

  // What `Request` and `Headers` show a page.
  assert_eq!(
    eval(
      &mut t.page,
      r#"(() => {
        const caught = (f) => {
          try { f(); return "no error"; } catch (e) { return e.name; }
        };
        const url = u(OTHER);
        const request = new Request(url, { headers: { "Cookie": "a=b", "X-Custom": "1" } });
        request.headers.set("Host", "evil.example");
        request.headers.append("Sec-Fetch-Mode", "navigate");
        request.headers.delete("X-Custom");
        const noCors = new Request(url, {
          mode: "no-cors",
          headers: { "X-Custom": "1", "Accept": "text/x", "Content-Type": "application/json" },
        });
        noCors.headers.append("Content-Language", "de");
        noCors.headers.append("Range", "bytes=0-");
        const response = new Response("x");
        response.headers.append("Set-Cookie", "a=b");
        response.headers.append("X-Custom", "1");
        return [
          [...request.headers.keys()], request.mode, request.credentials,
          [...noCors.headers.keys()],
          [...response.headers.keys()],
          // A `Headers` of its own has no guard.
          new Headers({ "Cookie": "a=b" }).has("cookie"),
          caught(() => new Request(url, { method: "TRACE" })),
          caught(() => new Request(url, { method: "connect" })),
          caught(() => new Request(url, { method: "TRACK" })),
          caught(() => new Request(url, { mode: "navigate" })),
          caught(() => new Request(url, { mode: "no-cors", method: "PUT" })),
          caught(() => new Request("http://user:pw@127.0.0.1:1/")),
          new Request(url, { referrer: "https://elsewhere.example/" }).referrer,
          new Request(url, { referrer: "/other" }).referrer === ORIGIN + "/other",
        ];
      })()"#,
    ),
    json!([
      [],
      "cors",
      "same-origin",
      ["accept", "content-language"],
      ["content-type", "x-custom"],
      true,
      "TypeError",
      "TypeError",
      "TypeError",
      "TypeError",
      "TypeError",
      "TypeError",
      "about:client",
      true,
    ])
  );
  assert_eq!(
    t.eval(r#"await probe(u(SAME, { id: "trace" }), { method: "TRACE" })"#)
      .await["error"]
      .as_str()
      .map(|error| error.starts_with("TypeError")),
    Some(true)
  );
  assert!(t.seen("trace").is_empty());

  // The referrer policy and an explicit referrer of the page's own origin.
  assert_eq!(
    t.eval(
      r#"await probe(u(OTHER, { id: "no-referrer", h: [ACAO("*")] }), { referrerPolicy: "no-referrer" });
        await probe(u(OTHER, { id: "unsafe-url", h: [ACAO("*")] }), { referrerPolicy: "unsafe-url", referrer: "/elsewhere?x=1#f" });
        await probe(u(OTHER, { id: "empty-referrer", h: [ACAO("*")] }), { referrer: "" });
        return true;"#
    )
    .await,
    json!(true)
  );
  assert_eq!(t.seen("no-referrer")[0].header("referer"), None);
  assert_eq!(
    t.seen("unsafe-url")[0].header("referer"),
    Some(format!("{}/elsewhere?x=1", t.origin).as_str())
  );
  assert_eq!(t.seen("empty-referrer")[0].header("referer"), None);

  // A WebSocket handshake says where it comes from. (The server answers it
  // as any other request, so the connection fails: what it was sent is the
  // point.)
  run(
    &mut t.page,
    r#"globalThis.socketEvents = [];
      {
        const ws = new WebSocket(`ws://127.0.0.1:${OTHER}/r?id=socket`);
        ws.onerror = () => socketEvents.push("error");
        ws.onclose = () => socketEvents.push("close");
      }"#,
  );
  let _ = t.page.run_until_idle(Duration::from_secs(20)).await;
  assert_eq!(eval(&mut t.page, "socketEvents"), json!(["error", "close"]));
  let seen = t.seen("socket");
  assert_eq!(seen.len(), 1);
  assert_eq!(seen[0].header("origin"), Some(t.origin.as_str()));
  assert_eq!(
    seen[0].header("upgrade").map(str::to_ascii_lowercase),
    Some("websocket".to_string())
  );
  assert_eq!(seen[0].header("cookie"), None);
}

#[tokio::test(flavor = "current_thread")]
async fn a_page_without_a_tuple_origin_is_null() {
  let mut t = CorsPage::new(Some("data:text/html,<p>opaque</p>")).await;
  assert_eq!(eval(&mut t.page, "origin"), json!("null"));

  // Every HTTP origin is another origin, to which the page is `null` and
  // has no referrer.
  assert_eq!(
    t.eval(
      r#"const r = await probe(u(SAME, { id: "named", body: "for null", h: [ACAO("null")] }));
        return [r.type, r.body];"#
    )
    .await,
    json!(["cors", "for null"])
  );
  let seen = t.seen("named");
  assert_eq!(seen[0].header("origin"), Some("null"));
  assert_eq!(seen[0].header("referer"), None);
  assert_eq!(
    t.eval(r#"await probe(u(SAME, { id: "unnamed", body: "secret" }))"#)
      .await,
    blocked()
  );
  assert_eq!(
    t.eval(
      r#"await probe(u(SAME, { id: "same-origin", body: "secret", h: [ACAO("*")] }), { mode: "same-origin" })"#
    )
    .await,
    blocked()
  );
  assert!(t.seen("same-origin").is_empty());
}
