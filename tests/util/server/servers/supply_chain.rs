// Copyright 2018-2026 the Deno authors. MIT license.

//! Stand-ins for the web services denext's npm supply-chain gates call
//! (DENEXT.md items 84 and 85), so their spec tests run offline:
//!
//! - OSV (`POST /v1/querybatch`): a version is malicious (an `MAL-` advisory)
//!   when its package name contains `malicious` and the version starts with
//!   `2.`; a name containing `vulnerable` gets a non-`MAL` advisory (which the
//!   gate ignores); a name containing `paged-advisories` gets its advisories
//!   over three pages (`next_page_token`), the `MAL-` one, for a `2.`
//!   version, on the last; everything else is clean.
//! - npm's downloads API (`GET /downloads/point/last-week/<name>`): a name
//!   containing `unpopular` has 12 weekly downloads, `modest` 5000, one
//!   containing `unknown-downloads` has no count (404), and `@denotest/chalkk`
//!   (one edit from `chalk`) 7; everything else 1000000. Several
//!   comma-separated unscoped names get npm's bulk answer (an object of
//!   counts by name, `null` for a name with no count).
//!
//! Under `/down/` both answer 503 (an unreachable service). Under
//! `/paging-fails/` OSV answers the first page with a `next_page_token` for
//! every query, then 503 for any later page (an answer cut short partway).
//! Under `/rate-limited/` the downloads API answers 429 with
//! `Retry-After: 0`.

use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use futures::FutureExt;
use futures::future::LocalBoxFuture;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::Request;
use hyper::Response;
use hyper::StatusCode;
use serde_json::json;

use super::ServerKind;
use super::ServerOptions;
use super::empty_body;
use super::run_server;
use super::string_body;

pub fn api(port: u16) -> Vec<LocalBoxFuture<'static, ()>> {
  let addr = SocketAddr::from(([127, 0, 0, 1], port));
  vec![
    run_server(
      ServerOptions {
        addr,
        kind: ServerKind::Auto,
        error_msg: "supply chain services server error",
      },
      handler,
    )
    .boxed_local(),
  ]
}

fn json_response(
  value: serde_json::Value,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, anyhow::Error> {
  Ok(
    Response::builder()
      .status(StatusCode::OK)
      .header("Content-Type", "application/json")
      .body(string_body(&value.to_string()))?,
  )
}

async fn handler(
  req: Request<hyper::body::Incoming>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, anyhow::Error> {
  let mut path = req.uri().path().to_string();
  let paging_fails = path.starts_with("/paging-fails/");
  if paging_fails {
    path = path["/paging-fails".len()..].to_string();
  }
  if path.starts_with("/down/") {
    return Ok(
      Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(empty_body())?,
    );
  }
  if path.starts_with("/rate-limited/") {
    return Ok(
      Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Retry-After", "0")
        .body(empty_body())?,
    );
  }
  if path == "/v1/querybatch" && req.method() == hyper::Method::POST {
    let body = req.into_body().collect().await?.to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    let queries = body["queries"].as_array().cloned().unwrap_or_default();
    if paging_fails && queries.iter().any(|q| q["page_token"].is_string()) {
      return Ok(
        Response::builder()
          .status(StatusCode::SERVICE_UNAVAILABLE)
          .body(empty_body())?,
      );
    }
    let results = queries
      .iter()
      .map(|q| {
        let name = q["package"]["name"].as_str().unwrap_or_default();
        let version = q["version"].as_str().unwrap_or_default();
        if q["package"]["ecosystem"] != "npm" {
          return json!({});
        }
        if name.contains("paged-advisories") {
          let vuln =
            |id: &str| json!({ "id": id, "modified": "2026-01-01T00:00:00Z" });
          return match q["page_token"].as_str() {
            None => json!({
              "vulns": [vuln("GHSA-2222-2222-2222")],
              "next_page_token": "page-2",
            }),
            Some("page-2") => json!({
              "vulns": [vuln("GHSA-3333-3333-3333")],
              "next_page_token": "page-3",
            }),
            Some(_) if version.starts_with("2.") => {
              json!({ "vulns": [vuln("MAL-2026-0002")] })
            }
            Some(_) => json!({}),
          };
        }
        if name.contains("malicious") && version.starts_with("2.") {
          json!({ "vulns": [
            { "id": "GHSA-0000-0000-0000", "modified": "2026-01-01T00:00:00Z" },
            { "id": "MAL-2026-0001", "modified": "2026-01-01T00:00:00Z" },
          ] })
        } else if name.contains("vulnerable") {
          json!({ "vulns": [
            { "id": "GHSA-1111-1111-1111", "modified": "2026-01-01T00:00:00Z" },
          ] })
        } else {
          json!({})
        }
      })
      .map(|mut result| {
        if paging_fails {
          result["next_page_token"] = json!("page-2");
        }
        result
      })
      .collect::<Vec<_>>();
    return json_response(json!({ "results": results }));
  }
  if let Some(names) = path.strip_prefix("/downloads/point/last-week/")
    && names.contains(',')
  {
    let point = |name: &str| {
      let name = percent_encoding::percent_decode_str(name)
        .decode_utf8_lossy()
        .into_owned();
      if name.starts_with('@') || name.contains("unknown-downloads") {
        return (name, serde_json::Value::Null);
      }
      let downloads = weekly_downloads(&name);
      (
        name.clone(),
        json!({ "downloads": downloads, "start": "2026-10-12",
          "end": "2026-10-18", "package": name }),
      )
    };
    let points = names
      .split(',')
      .map(point)
      .collect::<serde_json::Map<_, _>>();
    return json_response(serde_json::Value::Object(points));
  }
  if let Some(name) = path.strip_prefix("/downloads/point/last-week/") {
    let name = percent_encoding::percent_decode_str(name)
      .decode_utf8_lossy()
      .into_owned();
    if name.contains("unknown-downloads") {
      return Ok(
        Response::builder()
          .status(StatusCode::NOT_FOUND)
          .header("Content-Type", "application/json")
          .body(string_body(
            &json!({ "error": format!("package {name} not found") })
              .to_string(),
          ))?,
      );
    }
    let downloads = weekly_downloads(&name);
    return json_response(json!({
      "downloads": downloads,
      "start": "2026-10-12",
      "end": "2026-10-18",
      "package": name,
    }));
  }
  Ok(
    Response::builder()
      .status(StatusCode::NOT_FOUND)
      .body(empty_body())?,
  )
}

fn weekly_downloads(name: &str) -> u64 {
  if name.contains("unpopular") {
    12
  } else if name == "@denotest/chalkk" {
    7
  } else if name.contains("modest") {
    5000
  } else {
    1_000_000
  }
}
