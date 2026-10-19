// Copyright 2018-2026 the Deno authors. MIT license.

//! Stand-ins for the web services denext's npm supply-chain gates call
//! (DENEXT.md items 81 and 82), so their spec tests run offline:
//!
//! - OSV (`POST /v1/querybatch`): a version is malicious (an `MAL-` advisory)
//!   when its package name contains `malicious` and the version starts with
//!   `2.`; a name containing `vulnerable` gets a non-`MAL` advisory (which the
//!   gate ignores); everything else is clean.
//! - npm's downloads API (`GET /downloads/point/last-week/<name>`): a name
//!   containing `unpopular` has 12 weekly downloads, `modest` 5000, one
//!   containing `unknown-downloads` has no count (404); everything else
//!   1000000.
//!
//! Under `/down/` both answer 503 (an unreachable service).

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
  let path = req.uri().path().to_string();
  if path.starts_with("/down/") {
    return Ok(
      Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(empty_body())?,
    );
  }
  if path == "/v1/querybatch" && req.method() == hyper::Method::POST {
    let body = req.into_body().collect().await?.to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    let queries = body["queries"].as_array().cloned().unwrap_or_default();
    let results = queries
      .iter()
      .map(|q| {
        let name = q["package"]["name"].as_str().unwrap_or_default();
        let version = q["version"].as_str().unwrap_or_default();
        if q["package"]["ecosystem"] != "npm" {
          return json!({});
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
      .collect::<Vec<_>>();
    return json_response(json!({ "results": results }));
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
    let downloads = if name.contains("unpopular") {
      12
    } else if name.contains("modest") {
      5000
    } else {
      1_000_000
    };
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
