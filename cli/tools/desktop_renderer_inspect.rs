// Copyright 2018-2026 the Deno authors. MIT license.

//! `deno desktop --inspect-renderer[=<addr>]` on its own (without
//! `--inspect`): the app runs as a development build and its window's
//! renderer is opened to a debugger over the engine's own remote protocol,
//! so a CDP client (Chrome's `chrome://inspect`, Puppeteer, Playwright's
//! `connectOverCDP`, an agent) can drive the page directly. With `--inspect`
//! the renderer port feeds the unified DevTools multiplexer instead
//! (`desktop_devtools.rs`). Design note: `DENEXT-inspect-renderer.md`.
//!
//! What each backend offers:
//!
//! | backend            | protocol                      | how                                         |
//! | ------------------ | ----------------------------- | ------------------------------------------- |
//! | CEF (every OS)     | CDP, `http://<addr>/json`     | `LAUFEY_REMOTE_DEBUGGING_PORT` (the host's  |
//! |                    |                               | `remote_debugging_port`, loopback only)     |
//! | WebView2 (Windows) | CDP, `http://<addr>/json`     | `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`     |
//! |                    |                               | `--remote-debugging-port=<port>`            |
//! | WebKitGTK (Linux)  | WebKit's inspector (not CDP), | `WEBKIT_INSPECTOR_HTTP_SERVER=<addr>`       |
//! |                    | a page at `http://<addr>`     |                                             |
//! | WKWebView (macOS)  | none remote                   | Safari > Develop (the view is inspectable)  |
//!
//! Every one of them needs DevTools on (`LAUFEY_INSPECTABLE`, the default
//! for a development build); a packaged app with DevTools off clears these
//! settings itself.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::time::Duration;

use deno_core::serde_json;
use deno_terminal::colors;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
  Macos,
  Linux,
  Windows,
}

impl HostOs {
  pub fn current() -> Self {
    if cfg!(target_os = "macos") {
      HostOs::Macos
    } else if cfg!(windows) {
      HostOs::Windows
    } else {
      HostOs::Linux
    }
  }
}

/// The remote protocol a backend's renderer is reachable over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendererProtocol {
  /// Chrome DevTools Protocol: `/json/list` and a WebSocket per page.
  Cdp,
  /// WebKitGTK's remote inspector, served as a web page.
  WebKitHttp,
  /// No remote protocol: how to inspect the renderer instead.
  Unavailable(&'static str),
}

/// What to give the backend process to open its renderer at `addr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererInspectPlan {
  pub protocol: RendererProtocol,
  /// The address the renderer is reachable at (CDP backends bind loopback).
  pub addr: SocketAddr,
  pub env: Vec<(String, String)>,
}

/// The plan for `backend` ("cef" or "webview") on `os`, listening at `addr`
/// (its port already allocated, never 0). `existing_wv2_args` is the
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` already in the environment, kept
/// in front of the port switch.
pub fn plan(
  backend: &str,
  os: HostOs,
  addr: SocketAddr,
  existing_wv2_args: Option<&str>,
) -> RendererInspectPlan {
  // Chromium's remote debugging server (CEF's remote_debugging_port, and
  // WebView2's switch) listens on loopback only.
  let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port());
  let port = addr.port().to_string();
  match (backend, os) {
    ("cef", _) => RendererInspectPlan {
      protocol: RendererProtocol::Cdp,
      addr: loopback,
      env: vec![("LAUFEY_REMOTE_DEBUGGING_PORT".to_string(), port)],
    },
    (_, HostOs::Windows) => {
      let switch = format!("--remote-debugging-port={port}");
      let args = match existing_wv2_args.map(str::trim) {
        Some(existing) if !existing.is_empty() => {
          format!("{existing} {switch}")
        }
        _ => switch,
      };
      RendererInspectPlan {
        protocol: RendererProtocol::Cdp,
        addr: loopback,
        env: vec![("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".to_string(), args)],
      }
    }
    (_, HostOs::Linux) => RendererInspectPlan {
      protocol: RendererProtocol::WebKitHttp,
      addr,
      env: vec![("WEBKIT_INSPECTOR_HTTP_SERVER".to_string(), addr.to_string())],
    },
    (_, HostOs::Macos) => RendererInspectPlan {
      protocol: RendererProtocol::Unavailable(
        "WKWebView has no remote debugging port: open Safari > Develop > \
         <this Mac> > the app's page (the view is inspectable in a \
         development build), or use --backend cef for CDP",
      ),
      addr,
      env: vec![],
    },
  }
}

/// A page target from `/json/list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageTarget {
  pub id: String,
  pub title: String,
  pub url: String,
  pub ws_url: String,
  /// A DevTools frontend URL a browser can open (CEF serves the bundled
  /// frontend at `/devtools/inspector.html`).
  pub frontend_url: Option<String>,
}

/// The `"type": "page"` targets of a `/json/list` body served from `addr`.
pub fn parse_page_targets(body: &str, addr: SocketAddr) -> Vec<PageTarget> {
  let Ok(serde_json::Value::Array(targets)) =
    serde_json::from_str::<serde_json::Value>(body)
  else {
    return vec![];
  };
  targets
    .iter()
    .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
    .filter_map(|t| {
      let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_string);
      let ws_url = s("webSocketDebuggerUrl")?;
      let frontend_url = s("devtoolsFrontendUrl").map(|f| {
        if f.starts_with('/') {
          format!("http://{addr}{f}")
        } else {
          f
        }
      });
      Some(PageTarget {
        id: s("id").unwrap_or_default(),
        title: s("title").unwrap_or_default(),
        url: s("url").unwrap_or_default(),
        ws_url,
        frontend_url,
      })
    })
    .collect()
}

/// The body of a raw HTTP/1.x response with a 200 status.
fn http_ok_body(response: &[u8]) -> Option<&str> {
  let text = std::str::from_utf8(response).ok()?;
  let (head, body) = text.split_once("\r\n\r\n")?;
  let status = head.lines().next()?;
  let mut parts = status.split_whitespace();
  let (_version, code) = (parts.next()?, parts.next()?);
  (code == "200").then_some(body)
}

async fn get_json_list(addr: SocketAddr) -> std::io::Result<Vec<u8>> {
  let mut stream = TcpStream::connect(addr).await?;
  let request = format!(
    "GET /json/list HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
  );
  stream.write_all(request.as_bytes()).await?;
  let mut response = Vec::new();
  stream.read_to_end(&mut response).await?;
  Ok(response)
}

/// Announce how to reach the renderer, and for CDP wait (in the
/// background) for the window's page to appear and print its WebSocket.
pub fn announce(plan: &RendererInspectPlan) {
  match &plan.protocol {
    RendererProtocol::Cdp => {
      log::info!(
        "{} renderer CDP on http://{}/json (chrome://inspect > Configure > {})",
        colors::green("Inspector"),
        plan.addr,
        plan.addr,
      );
      let addr = plan.addr;
      tokio::spawn(async move {
        // The window's page appears once the backend has started and
        // navigated; give a slow first launch a minute.
        for _ in 0..240 {
          if let Ok(response) = get_json_list(addr).await
            && let Some(body) = http_ok_body(&response)
          {
            let pages = parse_page_targets(body, addr);
            if !pages.is_empty() {
              for page in pages {
                log::info!(
                  "{} renderer page {:?} ({}): {}",
                  colors::green("Inspector"),
                  page.title,
                  page.url,
                  page.ws_url,
                );
                if let Some(frontend) = page.frontend_url {
                  log::info!("          DevTools: {frontend}");
                }
              }
              return;
            }
          }
          tokio::time::sleep(Duration::from_millis(250)).await;
        }
        log::warn!(
          "{} no renderer page on http://{addr}/json after 60 s",
          colors::yellow("Warning"),
        );
      });
    }
    RendererProtocol::WebKitHttp => log::info!(
      "{} WebKit inspector on http://{} (WebKit's protocol, not CDP; open it \
       in a WebKit browser such as Epiphany)",
      colors::green("Inspector"),
      plan.addr,
    ),
    RendererProtocol::Unavailable(how) => {
      log::warn!("{} --inspect-renderer: {how}", colors::yellow("Warning"),)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
  }

  #[test]
  fn cef_uses_the_host_remote_debugging_port_on_loopback() {
    for os in [HostOs::Macos, HostOs::Linux, HostOs::Windows] {
      let p = plan("cef", os, addr("0.0.0.0:9333"), None);
      assert_eq!(p.protocol, RendererProtocol::Cdp);
      assert_eq!(p.addr, addr("127.0.0.1:9333"));
      assert_eq!(
        p.env,
        vec![("LAUFEY_REMOTE_DEBUGGING_PORT".into(), "9333".into())]
      );
    }
  }

  #[test]
  fn webview2_appends_the_switch_to_existing_arguments() {
    let p = plan("webview", HostOs::Windows, addr("127.0.0.1:9444"), None);
    assert_eq!(p.protocol, RendererProtocol::Cdp);
    assert_eq!(
      p.env,
      vec![(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".into(),
        "--remote-debugging-port=9444".into()
      )]
    );
    let p = plan(
      "webview",
      HostOs::Windows,
      addr("127.0.0.1:9444"),
      Some(" --lang=de "),
    );
    assert_eq!(
      p.env[0].1,
      "--lang=de --remote-debugging-port=9444".to_string()
    );
  }

  #[test]
  fn webkitgtk_serves_its_own_inspector_and_wkwebview_has_none() {
    let p = plan("webview", HostOs::Linux, addr("127.0.0.1:9555"), None);
    assert_eq!(p.protocol, RendererProtocol::WebKitHttp);
    assert_eq!(
      p.env,
      vec![(
        "WEBKIT_INSPECTOR_HTTP_SERVER".into(),
        "127.0.0.1:9555".into()
      )]
    );
    let p = plan("webview", HostOs::Macos, addr("127.0.0.1:9555"), None);
    assert!(matches!(p.protocol, RendererProtocol::Unavailable(_)));
    assert!(p.env.is_empty());
  }

  #[test]
  fn page_targets_from_json_list() {
    let body = r#"[
      {"id":"A1","type":"page","title":"App","url":"myapp://app/",
       "webSocketDebuggerUrl":"ws://127.0.0.1:9333/devtools/page/A1",
       "devtoolsFrontendUrl":"/devtools/inspector.html?ws=127.0.0.1:9333/devtools/page/A1"},
      {"id":"W1","type":"service_worker","title":"sw","url":"myapp://app/sw.js",
       "webSocketDebuggerUrl":"ws://127.0.0.1:9333/devtools/page/W1"},
      {"id":"N1","type":"page","title":"attached elsewhere","url":"about:blank"}
    ]"#;
    let pages = parse_page_targets(body, addr("127.0.0.1:9333"));
    assert_eq!(
      pages,
      vec![PageTarget {
        id: "A1".into(),
        title: "App".into(),
        url: "myapp://app/".into(),
        ws_url: "ws://127.0.0.1:9333/devtools/page/A1".into(),
        frontend_url: Some(
          "http://127.0.0.1:9333/devtools/inspector.html?ws=127.0.0.1:9333/devtools/page/A1"
            .into()
        ),
      }]
    );
    assert!(parse_page_targets("not json", addr("127.0.0.1:1")).is_empty());
    assert!(parse_page_targets("{}", addr("127.0.0.1:1")).is_empty());
  }

  #[test]
  fn http_ok_body_needs_a_200() {
    assert_eq!(
      http_ok_body(b"HTTP/1.1 200 OK\r\nContent-Type: x\r\n\r\n[]"),
      Some("[]")
    );
    assert_eq!(http_ok_body(b"HTTP/1.1 404 Not Found\r\n\r\nno"), None);
    assert_eq!(http_ok_body(b"garbage"), None);
  }
}
