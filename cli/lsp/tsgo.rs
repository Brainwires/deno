// Copyright 2018-2026 the Deno authors. MIT license.

//! The TypeScript 7 (tsgo) language-service backend of `deno lsp` (denext,
//! unstable, off by default).
//!
//! `deno lsp` stays the server the editor talks to. Its Deno features
//! (resolution diagnostics, lint, formatting, `deno.json` / import-map
//! awareness, testing code lenses, registry completions, the `deno/*`
//! requests) are unchanged; only the TypeScript language features that
//! [`super::ts_server::TsServer`] provides go to this backend instead of the
//! in-isolate TypeScript 6 service. The backend runs a pinned native
//! `tsc --lsp` (a dated TypeScript 7.1 nightly, see
//! `cli/tsc/typescript_pins.json`) as a child process and is its LSP client:
//! open documents are mirrored with `didOpen` / `didChange` / `didClose`,
//! watched-file events are forwarded, and each feature is a standard LSP
//! request whose response is mapped back into Deno's conventions.
//!
//! tsgo has no module-resolution hooks over LSP, so it learns a Deno project's
//! `npm:` / `jsr:` / `https:` / import-map specifiers and Deno's own lib types
//! from the stock `tsconfig.json` + `.deno/` tree that `deno sync-types`
//! generates (the same files the native `deno check` consumes). The backend
//! runs `deno sync-types` for each workspace root before starting tsgo and
//! again when configuration or the dependency cache changes.
//!
//! Enable with `"deno.unstable": ["tsgo-lsp"]` in the editor's
//! initialization settings, or with `DENO_UNSTABLE_TSGO_LSP=1` in the
//! environment of `deno lsp`.
//!
//! Derived in part from the earlier fork-based tsgo backend by Nayeem Rahman
//! (denoland/deno#32149, #32016 and follow-ups; removed in #33133), whose
//! transport and response normalization this reuses, and informed by Bartek
//! Iwańczuk's `tsgo --lsp` proxy prototype (`feat/tsgo-npm-lsp`).

use std::collections::HashMap;
use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use deno_core::anyhow::anyhow;
use deno_core::error::AnyError;
use deno_core::parking_lot::Mutex;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use deno_core::serde_json::json;
use lsp_types::Uri;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower_lsp::lsp_types as lsp;

use super::config::WorkspaceSettings;
use super::documents::DocumentModule;
use super::documents::LanguageId;
use super::documents::OpenDocument;
use super::language_server::StateSnapshot;
use crate::http_util::HttpClientProvider;
use crate::lsp::completions::CompletionItemData;
use crate::lsp::logging::lsp_debug;
use crate::lsp::logging::lsp_log;
use crate::lsp::logging::lsp_warn;
use crate::lsp::urls::normalize_uri;
use crate::tsc::IGNORED_DIAGNOSTIC_CODES;
use crate::tsc::native_pin;

/// `DENO_UNSTABLE_TSGO_LSP=1` selects this backend for the whole process.
pub const TSGO_LSP_ENV: &str = "DENO_UNSTABLE_TSGO_LSP";
/// A `tsc[.exe]` to use instead of the vendored or downloaded pinned build.
pub const TSGO_LSP_BIN_ENV: &str = "DENO_TSGO_LSP_BIN";
/// `DENO_TSGO_LSP_SYNC_TYPES=0` stops the backend from running
/// `deno sync-types` (the project manages its own tsconfig / `.deno/`).
pub const TSGO_LSP_SYNC_TYPES_ENV: &str = "DENO_TSGO_LSP_SYNC_TYPES";
/// The `deno.unstable` editor-setting entry that selects this backend.
pub const TSGO_LSP_UNSTABLE_FEATURE: &str = "tsgo-lsp";
/// The pin role in `cli/tsc/typescript_pins.json`.
pub const PIN_ROLE: &str = "lsp";

fn env_flag(name: &str) -> Option<bool> {
  let value = std::env::var(name).ok()?;
  let value = value.trim();
  if value.is_empty() {
    return None;
  }
  Some(!matches!(value, "0" | "false" | "no" | "off"))
}

/// Whether the environment or the initialization settings select tsgo. The
/// environment wins either way.
pub fn is_enabled(settings: Option<&WorkspaceSettings>) -> bool {
  if let Some(enabled) = env_flag(TSGO_LSP_ENV) {
    return enabled;
  }
  settings.is_some_and(|s| {
    s.unstable
      .iter()
      .any(|f| f.as_str() == TSGO_LSP_UNSTABLE_FEATURE)
  })
}

fn sync_types_enabled() -> bool {
  env_flag(TSGO_LSP_SYNC_TYPES_ENV).unwrap_or(true)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TsGoCompletionItemData {
  pub uri: Uri,
  pub data: Value,
}

/// `codeAction.data` of a tsgo action handed to the editor, so
/// `codeAction/resolve` can route it back to tsgo.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TsGoCodeActionData {
  pub tsgo_data: Value,
}

type PendingRequests =
  Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>;
type SharedStdin = Arc<Mutex<std::process::ChildStdin>>;

#[derive(Debug, Default)]
struct DocumentSyncState {
  project_version: usize,
  /// Open documents tsgo knows, with the version it last saw.
  versions: HashMap<Uri, i32>,
}

struct TsGoServerInner {
  stdin: SharedStdin,
  pending: Arc<PendingRequests>,
  next_id: AtomicI64,
  alive: Arc<AtomicBool>,
  child: Mutex<std::process::Child>,
  sync: Mutex<DocumentSyncState>,
  latest_snapshot: Arc<Mutex<Arc<StateSnapshot>>>,
}

impl std::fmt::Debug for TsGoServerInner {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TsGoServerInner")
      .field("next_id", &self.next_id)
      .finish_non_exhaustive()
  }
}

impl Drop for TsGoServerInner {
  fn drop(&mut self) {
    let mut child = self.child.lock();
    let _ = child.kill();
    let _ = child.wait();
  }
}

fn write_message(
  stdin: &mut std::process::ChildStdin,
  message: &Value,
) -> std::io::Result<()> {
  let content = serde_json::to_string(message)?;
  write!(
    stdin,
    "Content-Length: {}\r\n\r\n{}",
    content.len(),
    content
  )?;
  stdin.flush()
}

fn read_message(
  reader: &mut std::io::BufReader<std::process::ChildStdout>,
) -> std::io::Result<Value> {
  let mut content_length: Option<usize> = None;
  loop {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
      return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    let line = line.trim();
    if line.is_empty() {
      if content_length.is_some() {
        break;
      }
      continue;
    }
    if let Some((name, value)) = line.split_once(':')
      && name.trim().eq_ignore_ascii_case("content-length")
    {
      content_length = Some(value.trim().parse().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
      })?);
    }
  }
  let mut buf = vec![0u8; content_length.unwrap()];
  reader.read_exact(&mut buf)?;
  serde_json::from_slice(&buf)
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The LSP language id tsgo expects, for the documents it handles.
fn tsgo_language_id(language_id: LanguageId) -> Option<&'static str> {
  match language_id {
    LanguageId::TypeScript => Some("typescript"),
    LanguageId::Tsx => Some("typescriptreact"),
    LanguageId::JavaScript => Some("javascript"),
    LanguageId::Jsx => Some("javascriptreact"),
    _ => None,
  }
}

fn is_file_uri(uri: &Uri) -> bool {
  uri.scheme().as_str().eq_ignore_ascii_case("file")
}

/// Whether tsgo handles this open document: an on-disk script outside of a
/// notebook.
fn is_tsgo_document(doc: &OpenDocument) -> bool {
  doc.notebook_uri.is_none()
    && is_file_uri(&doc.uri)
    && tsgo_language_id(doc.language_id).is_some()
}

/// Whether requests for `module` go to tsgo: tsgo only sees files on disk,
/// so remote modules, virtual `deno:` documents and notebook cells don't.
fn is_tsgo_module(module: &DocumentModule) -> bool {
  module.notebook_uri.is_none() && is_file_uri(&module.uri)
}

/// The workspace roots to run `deno sync-types` in: the root directory of
/// every workspace that has a Deno config, else the first workspace folder.
pub fn sync_types_roots(snapshot: &StateSnapshot) -> Vec<PathBuf> {
  let mut roots = Vec::new();
  for data in snapshot.config.tree.data_by_scope().values() {
    if data.maybe_deno_json().is_none() {
      continue;
    }
    let root = data.member_dir.workspace.root_dir_path();
    if !roots.contains(&root) {
      roots.push(root);
    }
  }
  if roots.is_empty()
    && let Some(root) = snapshot.config.root_url()
    && let Ok(path) = root.to_file_path()
  {
    roots.push(path);
  }
  roots
}

/// Generate the stock `tsconfig.json` + `.deno/` types tsgo reads, with the
/// same `deno` executable (and so the same generator) the native
/// `deno check` uses. Failures are logged, never fatal: a project without the
/// generated files still gets tsgo's features for plain relative imports.
async fn run_sync_types(snapshot: &StateSnapshot) -> Vec<PathBuf> {
  let mut generated = Vec::new();
  if !sync_types_enabled() {
    return generated;
  }
  let Ok(exe) = std::env::current_exe() else {
    return generated;
  };
  let deno_dir = snapshot.cache.deno_dir().root.clone();
  for root in sync_types_roots(snapshot) {
    let started = std::time::Instant::now();
    let output = tokio::process::Command::new(&exe)
      .arg("sync-types")
      .current_dir(&root)
      .env("DENO_DIR", &deno_dir)
      .env("NO_COLOR", "1")
      .env("PWD", &root)
      .env_remove(TSGO_LSP_ENV)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .output()
      .await;
    match output {
      Ok(output) if output.status.success() => {
        lsp_log!(
          "tsgo: `deno sync-types` in {} ({}ms)",
          root.display(),
          started.elapsed().as_millis()
        );
        generated.push(root);
      }
      Ok(output) => {
        lsp_warn!(
          "tsgo: `deno sync-types` failed in {} ({}); npm:, jsr: and remote types and Deno's lib types may be missing.\n{}",
          root.display(),
          output.status,
          String::from_utf8_lossy(&output.stderr).trim(),
        );
      }
      Err(err) => {
        lsp_warn!("tsgo: could not run `deno sync-types`: {err:#}");
      }
    }
  }
  generated
}

/// The client capabilities Deno, as tsgo's client, advertises: the editor's
/// text-document capabilities (minus synchronization details Deno owns),
/// pull diagnostics, dynamic watched-file registration (Deno forwards the
/// editor's events) and workspace configuration.
fn client_capabilities(snapshot: &StateSnapshot) -> Value {
  let client = &snapshot.config.client_capabilities;
  let mut text_document = lsp::TextDocumentClientCapabilities {
    synchronization: None,
    formatting: None,
    range_formatting: None,
    on_type_formatting: None,
    publish_diagnostics: None,
    ..client.text_document.clone().unwrap_or_default()
  };
  text_document.diagnostic.get_or_insert_default();
  let mut value = json!(lsp::ClientCapabilities {
    text_document: Some(text_document),
    workspace: Some(lsp::WorkspaceClientCapabilities {
      configuration: Some(true),
      workspace_folders: Some(true),
      did_change_watched_files: Some(
        lsp::DidChangeWatchedFilesClientCapabilities {
          dynamic_registration: Some(true),
          relative_pattern_support: Some(true),
        }
      ),
      symbol: client.workspace.as_ref().and_then(|w| w.symbol.clone()),
      ..Default::default()
    }),
    general: Some(lsp::GeneralClientCapabilities {
      position_encodings: Some(vec![lsp::PositionEncodingKind::UTF16]),
      ..Default::default()
    }),
    ..Default::default()
  });
  // Fields `lsp_types` doesn't model yet.
  if let Some(diagnostic) = value
    .pointer_mut("/textDocument/diagnostic")
    .and_then(|d| d.as_object_mut())
  {
    diagnostic.insert("relatedInformation".to_string(), json!(true));
    diagnostic.insert("tagSupport".to_string(), json!({ "valueSet": [1, 2] }));
  }
  value
}

/// A `typescript` / `javascript` settings section as tsgo reads it. Deno
/// module specifiers carry their extension, so auto-imports and import fixes
/// must write `./mod.ts`, which tsgo does for `importModuleSpecifierEnding:
/// "js"` (with `allowImportingTsExtensions`); an explicit user choice other
/// than `auto` is kept.
fn language_configuration(settings: Result<Value, serde_json::Error>) -> Value {
  let mut value = match settings {
    Ok(Value::Object(map)) => Value::Object(map),
    _ => json!({}),
  };
  let preferences = value
    .as_object_mut()
    .unwrap()
    .entry("preferences")
    .or_insert_with(|| json!({}));
  if let Some(preferences) = preferences.as_object_mut() {
    let ending = preferences
      .get("importModuleSpecifierEnding")
      .and_then(|v| v.as_str());
    if matches!(ending, None | Some("auto")) {
      preferences
        .insert("importModuleSpecifierEnding".to_string(), json!("js"));
    }
  }
  value
}

/// Answer one request tsgo sends its client.
fn handle_server_request(
  method: &str,
  params: Option<&Value>,
  snapshot: &StateSnapshot,
) -> Result<Value, (i64, String)> {
  match method {
    "workspace/configuration" => {
      // tsgo asks for ["js/ts", "typescript", "javascript", "editor"].
      let settings = snapshot.config.workspace_settings();
      let items = params
        .and_then(|p| p.get("items"))
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();
      Ok(Value::Array(
        items
          .iter()
          .map(|item| match item.get("section").and_then(|s| s.as_str()) {
            Some("typescript") => {
              language_configuration(serde_json::to_value(&settings.typescript))
            }
            Some("javascript") => {
              language_configuration(serde_json::to_value(&settings.javascript))
            }
            _ => Value::Null,
          })
          .collect(),
      ))
    }
    "client/registerCapability"
    | "client/unregisterCapability"
    | "window/workDoneProgress/create"
    | "workspace/diagnostic/refresh"
    | "workspace/codeLens/refresh"
    | "workspace/inlayHint/refresh"
    | "workspace/semanticTokens/refresh" => Ok(Value::Null),
    "workspace/workspaceFolders" => Ok(json!(
      snapshot
        .config
        .workspace_folders
        .iter()
        .map(|(_, f)| f)
        .collect::<Vec<_>>()
    )),
    "window/showMessageRequest" => Ok(Value::Null),
    _ => Err((-32601, format!("Method not found: \"{method}\""))),
  }
}

fn reader_loop(
  stdout: std::process::ChildStdout,
  stdin: SharedStdin,
  pending: Arc<PendingRequests>,
  alive: Arc<AtomicBool>,
  latest_snapshot: Arc<Mutex<Arc<StateSnapshot>>>,
) {
  let mut reader = std::io::BufReader::new(stdout);
  loop {
    let message = match read_message(&mut reader) {
      Ok(message) => message,
      Err(err) => {
        if err.kind() != std::io::ErrorKind::UnexpectedEof {
          lsp_warn!("tsgo: error reading from the language server: {err}");
        } else {
          lsp_warn!("tsgo: the language server exited");
        }
        break;
      }
    };
    let method = message.get("method").and_then(|m| m.as_str());
    let id = message.get("id").cloned();
    match (method, id) {
      // A request from tsgo.
      (Some(method), Some(id)) => {
        let snapshot = latest_snapshot.lock().clone();
        let response =
          match handle_server_request(method, message.get("params"), &snapshot)
          {
            Ok(result) => {
              json!({ "jsonrpc": "2.0", "id": id, "result": result })
            }
            Err((code, message)) => json!({
              "jsonrpc": "2.0",
              "id": id,
              "error": { "code": code, "message": message },
            }),
          };
        let _ = write_message(&mut stdin.lock(), &response);
      }
      // A notification from tsgo.
      (Some(method), None) => match method {
        "window/logMessage" | "window/showMessage" => {
          let params = message.get("params");
          let typ = params.and_then(|p| p.get("type")).and_then(|t| t.as_i64());
          let text = params
            .and_then(|p| p.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or_default();
          if matches!(typ, Some(1 | 2)) {
            lsp_log!("[tsgo] {text}");
          } else {
            lsp_debug!("[tsgo] {text}");
          }
        }
        _ => {
          lsp_debug!("tsgo: ignoring notification \"{method}\"");
        }
      },
      // A response to one of ours.
      (None, Some(id)) => {
        let Some(id) = id.as_i64() else {
          continue;
        };
        let Some(sender) = pending.lock().remove(&id) else {
          continue;
        };
        let result = if let Some(error) = message.get("error") {
          Err(
            error
              .get("message")
              .and_then(|m| m.as_str())
              .unwrap_or("unknown error")
              .to_string(),
          )
        } else {
          Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = sender.send(result);
      }
      (None, None) => {}
    }
  }
  alive.store(false, Ordering::SeqCst);
  for (_, sender) in pending.lock().drain() {
    let _ = sender.send(Err("the tsgo language server exited".to_string()));
  }
}

/// Every file under `dir` (depth-first, at most `limit` in total).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    if out.len() >= limit {
      return;
    }
    let Ok(file_type) = entry.file_type() else {
      continue;
    };
    if file_type.is_dir() {
      collect_files(&entry.path(), out, limit);
    } else if file_type.is_file() {
      out.push(entry.path());
    }
  }
}

impl TsGoServerInner {
  async fn start(
    tsc: &Path,
    snapshot: Arc<StateSnapshot>,
  ) -> Result<Self, AnyError> {
    let cwd = snapshot
      .config
      .root_url()
      .and_then(|u| u.to_file_path().ok())
      .or_else(|| std::env::current_dir().ok());
    let mut command = std::process::Command::new(tsc);
    command
      .arg("--lsp")
      .arg("--stdio")
      .arg(format!("--clientProcessId={}", std::process::id()))
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit());
    if let Some(cwd) = &cwd {
      // Go's `os.Getwd` trusts `PWD` over `getcwd()`; keep them equal.
      command.current_dir(cwd).env("PWD", cwd);
    }
    let mut child = command
      .spawn()
      .map_err(|e| anyhow!("failed to start {}: {e}", tsc.display()))?;
    let stdin: SharedStdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
    let stdout = child.stdout.take().unwrap();
    let pending: Arc<PendingRequests> = Default::default();
    let alive = Arc::new(AtomicBool::new(true));
    let latest_snapshot = Arc::new(Mutex::new(snapshot.clone()));
    {
      let stdin = stdin.clone();
      let pending = pending.clone();
      let alive = alive.clone();
      let latest_snapshot = latest_snapshot.clone();
      std::thread::Builder::new()
        .name("tsgo-lsp-reader".to_string())
        .spawn(move || {
          reader_loop(stdout, stdin, pending, alive, latest_snapshot)
        })?;
    }
    let inner = Self {
      stdin,
      pending,
      next_id: AtomicI64::new(1),
      alive,
      child: Mutex::new(child),
      sync: Default::default(),
      latest_snapshot,
    };
    let root_uri = snapshot.config.root_url().map(|u| u.to_string());
    let workspace_folders = snapshot
      .config
      .workspace_folders
      .iter()
      .map(|(_, f)| f.clone())
      .collect::<Vec<_>>();
    let _: Value = inner
      .request(
        "initialize",
        json!({
          "processId": std::process::id(),
          "clientInfo": { "name": "deno lsp", "version": deno_lib::version::DENO_VERSION_INFO.deno },
          "rootUri": root_uri,
          "workspaceFolders": if workspace_folders.is_empty() { Value::Null } else { json!(workspace_folders) },
          "capabilities": client_capabilities(&snapshot),
          "initializationOptions": { "disablePushDiagnostics": true },
        }),
        &CancellationToken::new(),
      )
      .await?;
    inner.notify("initialized", json!({}))?;
    Ok(inner)
  }

  fn notify(&self, method: &str, params: Value) -> Result<(), AnyError> {
    let message =
      json!({ "jsonrpc": "2.0", "method": method, "params": params });
    write_message(&mut self.stdin.lock(), &message)?;
    Ok(())
  }

  /// Bring tsgo's open documents in line with `snapshot` (in the same stdin
  /// critical section as the request that follows, so tsgo always answers
  /// against the text the request's positions refer to). Older snapshots
  /// than the last one synced are ignored, so concurrent requests can't roll
  /// tsgo back.
  fn sync_documents(
    &self,
    snapshot: &Arc<StateSnapshot>,
    stdin: &mut std::process::ChildStdin,
  ) -> Result<(), AnyError> {
    let mut state = self.sync.lock();
    if snapshot.project_version < state.project_version {
      return Ok(());
    }
    state.project_version = snapshot.project_version;
    *self.latest_snapshot.lock() = snapshot.clone();
    let mut seen = HashMap::new();
    for doc in snapshot.document_modules.documents.open_docs() {
      if !is_tsgo_document(doc) {
        continue;
      }
      let uri = doc.uri.as_ref().clone();
      seen.insert(uri.clone(), ());
      match state.versions.get(&uri) {
        Some(version) if *version == doc.version => {}
        Some(_) => {
          write_message(
            stdin,
            &json!({
              "jsonrpc": "2.0",
              "method": "textDocument/didChange",
              "params": {
                "textDocument": { "uri": &uri, "version": doc.version },
                "contentChanges": [{ "text": doc.text.as_ref() }],
              },
            }),
          )?;
          state.versions.insert(uri, doc.version);
        }
        None => {
          write_message(
            stdin,
            &json!({
              "jsonrpc": "2.0",
              "method": "textDocument/didOpen",
              "params": {
                "textDocument": {
                  "uri": &uri,
                  "languageId": tsgo_language_id(doc.language_id),
                  "version": doc.version,
                  "text": doc.text.as_ref(),
                },
              },
            }),
          )?;
          state.versions.insert(uri, doc.version);
        }
      }
    }
    let closed = state
      .versions
      .keys()
      .filter(|uri| !seen.contains_key(*uri))
      .cloned()
      .collect::<Vec<_>>();
    for uri in closed {
      write_message(
        stdin,
        &json!({
          "jsonrpc": "2.0",
          "method": "textDocument/didClose",
          "params": { "textDocument": { "uri": &uri } },
        }),
      )?;
      state.versions.remove(&uri);
    }
    Ok(())
  }

  async fn request<R: DeserializeOwned>(
    &self,
    method: &str,
    params: Value,
    token: &CancellationToken,
  ) -> Result<R, AnyError> {
    self.request_with_sync(method, params, None, token).await
  }

  async fn request_with_sync<R: DeserializeOwned>(
    &self,
    method: &str,
    params: Value,
    snapshot: Option<&Arc<StateSnapshot>>,
    token: &CancellationToken,
  ) -> Result<R, AnyError> {
    if !self.alive.load(Ordering::SeqCst) {
      return Err(anyhow!("the tsgo language server is not running"));
    }
    let id = self.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    self.pending.lock().insert(id, tx);
    {
      let mut stdin = self.stdin.lock();
      if let Some(snapshot) = snapshot {
        self.sync_documents(snapshot, &mut stdin)?;
      }
      let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
      if let Err(err) = write_message(&mut stdin, &message) {
        self.pending.lock().remove(&id);
        return Err(err.into());
      }
    }
    let result = tokio::select! {
      result = rx => result,
      _ = token.cancelled() => {
        self.pending.lock().remove(&id);
        let _ = self.notify("$/cancelRequest", json!({ "id": id }));
        return Err(anyhow!("request cancelled"));
      }
    };
    let value = result
      .map_err(|_| anyhow!("request cancelled"))?
      .map_err(|e| anyhow!("tsgo: {method}: {e}"))?;
    Ok(serde_json::from_value(value)?)
  }
}

/// `deno lsp`'s tsgo backend. Starts lazily on the first request.
pub struct TsGoServer {
  http_client_provider: Arc<HttpClientProvider>,
  inner: tokio::sync::OnceCell<Result<Arc<TsGoServerInner>, String>>,
  /// Serializes `deno sync-types` runs after startup.
  sync_types_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for TsGoServer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TsGoServer")
      .field("started", &self.is_started())
      .finish_non_exhaustive()
  }
}

impl TsGoServer {
  pub fn new(http_client_provider: Arc<HttpClientProvider>) -> Self {
    Self {
      http_client_provider,
      inner: Default::default(),
      sync_types_lock: Default::default(),
    }
  }

  pub fn is_started(&self) -> bool {
    self.inner.get().is_some_and(|r| r.is_ok())
  }

  async fn resolve_tsc(
    &self,
    snapshot: &StateSnapshot,
  ) -> Result<PathBuf, AnyError> {
    if let Some(local) = native_pin::find_local(
      PIN_ROLE,
      TSGO_LSP_BIN_ENV,
      std::env::var_os(TSGO_LSP_BIN_ENV),
      std::env::current_exe().ok(),
    )? {
      return Ok(local);
    }
    let http_client = self.http_client_provider.get_or_create()?;
    native_pin::ensure_downloaded(
      PIN_ROLE,
      snapshot.cache.deno_dir(),
      &http_client,
    )
    .await
  }

  async fn inner(
    &self,
    snapshot: &Arc<StateSnapshot>,
  ) -> Result<&Arc<TsGoServerInner>, AnyError> {
    let result = self
      .inner
      .get_or_init(|| async {
        let started = std::time::Instant::now();
        let tsc = self.resolve_tsc(snapshot).await.map_err(|e| {
          format!(
            "could not get TypeScript {}: {e:#}",
            native_pin::pin(PIN_ROLE).version
          )
        })?;
        {
          let _guard = self.sync_types_lock.lock().await;
          run_sync_types(snapshot).await;
        }
        let inner = TsGoServerInner::start(&tsc, snapshot.clone())
          .await
          .map_err(|e| format!("{e:#}"))?;
        lsp_log!(
          "tsgo: started TypeScript {} from {} ({}ms)",
          native_pin::pin(PIN_ROLE).version,
          tsc.display(),
          started.elapsed().as_millis()
        );
        Ok(Arc::new(inner))
      })
      .await;
    match result {
      Ok(inner) => Ok(inner),
      Err(err) => Err(anyhow!("{err}")),
    }
  }

  pub async fn warm_up(&self, snapshot: Arc<StateSnapshot>) {
    if let Err(err) = self.inner(&snapshot).await {
      lsp_warn!("tsgo: failed to start: {err:#}");
    }
  }

  /// A request about `snapshot`'s documents, after syncing them to tsgo.
  async fn request<R: DeserializeOwned>(
    &self,
    method: &str,
    params: Value,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<R, AnyError> {
    let inner = self.inner(snapshot).await?;
    inner
      .request_with_sync(method, params, Some(snapshot), token)
      .await
  }

  /// Push open-document changes to a running tsgo right away, so its
  /// background work (and any pull that races an edit) sees the new text.
  pub fn project_changed(&self, snapshot: Arc<StateSnapshot>) {
    let Some(Ok(inner)) = self.inner.get() else {
      return;
    };
    let mut stdin = inner.stdin.lock();
    if let Err(err) = inner.sync_documents(&snapshot, &mut stdin) {
      lsp_warn!("tsgo: failed to sync documents: {err:#}");
    }
  }

  /// Forward the editor's watched-file events (Deno registers a watcher for
  /// configs and scripts; tsgo registers its own globs with Deno, which acks
  /// them and relies on this forwarding).
  pub fn did_change_watched_files(&self, changes: &[lsp::FileEvent]) {
    let Some(Ok(inner)) = self.inner.get() else {
      return;
    };
    if changes.is_empty() {
      return;
    }
    if let Err(err) = inner.notify(
      "workspace/didChangeWatchedFiles",
      json!({ "changes": changes }),
    ) {
      lsp_warn!("tsgo: failed to forward file changes: {err:#}");
    }
  }

  /// Re-run `deno sync-types` (config or dependencies changed) and tell tsgo
  /// which generated files changed: the root and `.deno/` tsconfigs and every
  /// file under `.deno/` (Deno's lib types and the mirrored dependency types).
  pub async fn refresh_generated_types(&self, snapshot: Arc<StateSnapshot>) {
    let inner = match self.inner(&snapshot).await {
      Ok(inner) => inner.clone(),
      Err(_) => return,
    };
    let roots = {
      let _guard = self.sync_types_lock.lock().await;
      run_sync_types(&snapshot).await
    };
    let mut paths = Vec::new();
    for root in &roots {
      paths.push(root.join("tsconfig.json"));
      collect_files(&root.join(".deno"), &mut paths, 20_000);
    }
    let changes = paths
      .iter()
      .filter_map(|path| {
        let url = deno_path_util::url_from_file_path(path).ok()?;
        let uri = crate::lsp::urls::url_to_uri(&url).ok()?;
        Some(lsp::FileEvent {
          uri,
          typ: lsp::FileChangeType::CHANGED,
        })
      })
      .collect::<Vec<_>>();
    if !changes.is_empty()
      && let Err(err) = inner.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": changes }),
      )
    {
      lsp_warn!("tsgo: failed to announce regenerated types: {err:#}");
    }
  }

  fn text_document_position(
    module: &DocumentModule,
    position: lsp::Position,
  ) -> Value {
    json!({
      "textDocument": { "uri": module.uri.as_ref() },
      "position": position,
    })
  }

  pub async fn provide_diagnostics(
    &self,
    module: &DocumentModule,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Vec<lsp::Diagnostic>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(Vec::new());
    }
    let report: Option<lsp::DocumentDiagnosticReport> = self
      .request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": module.uri.as_ref() } }),
        snapshot,
        token,
      )
      .await?;
    let mut items = match report {
      Some(lsp::DocumentDiagnosticReport::Full(report)) => {
        report.full_document_diagnostic_report.items
      }
      _ => Vec::new(),
    };
    let suggestion_actions_enabled = snapshot
      .config
      .language_settings_for_specifier(&module.specifier)
      .map(|s| s.suggestion_actions.enabled)
      .unwrap_or(true);
    items.retain(|diagnostic| {
      if let Some(lsp::NumberOrString::Number(code)) = &diagnostic.code
        && IGNORED_DIAGNOSTIC_CODES.contains(&(*code as _))
      {
        return false;
      }
      if !suggestion_actions_enabled
        && diagnostic.severity == Some(lsp::DiagnosticSeverity::HINT)
      {
        // Still show deprecated and unused diagnostics, as the JS backend.
        return diagnostic.tags.as_ref().is_some_and(|tags| {
          tags.contains(&lsp::DiagnosticTag::DEPRECATED)
            || tags.contains(&lsp::DiagnosticTag::UNNECESSARY)
        });
      }
      true
    });
    for diagnostic in &mut items {
      normalize_diagnostic(diagnostic);
    }
    Ok(items)
  }

  pub async fn provide_hover(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::Hover>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    self
      .request(
        "textDocument/hover",
        Self::text_document_position(module, position),
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_definition(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::GotoDefinitionResponse>, AnyError> {
    self
      .locations_request(
        "textDocument/definition",
        module,
        position,
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_type_definition(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::GotoDefinitionResponse>, AnyError> {
    self
      .locations_request(
        "textDocument/typeDefinition",
        module,
        position,
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_implementations(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::GotoDefinitionResponse>, AnyError> {
    self
      .locations_request(
        "textDocument/implementation",
        module,
        position,
        snapshot,
        token,
      )
      .await
  }

  async fn locations_request(
    &self,
    method: &str,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::GotoDefinitionResponse>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut response: Option<lsp::GotoDefinitionResponse> = self
      .request(
        method,
        Self::text_document_position(module, position),
        snapshot,
        token,
      )
      .await?;
    if let Some(response) = &mut response {
      normalize_goto_definition_response(response);
    }
    Ok(response)
  }

  pub async fn provide_references(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    context: lsp::ReferenceContext,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::Location>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut params = Self::text_document_position(module, position);
    params["context"] = json!(context);
    let mut locations: Option<Vec<lsp::Location>> = self
      .request("textDocument/references", params, snapshot, token)
      .await?;
    if let Some(locations) = &mut locations {
      for location in locations {
        normalize_location(location);
      }
    }
    Ok(locations)
  }

  pub async fn provide_document_highlights(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::DocumentHighlight>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    self
      .request(
        "textDocument/documentHighlight",
        Self::text_document_position(module, position),
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_rename(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    new_name: &str,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::WorkspaceEdit>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut params = Self::text_document_position(module, position);
    params["newName"] = json!(new_name);
    let mut edit: Option<lsp::WorkspaceEdit> = self
      .request("textDocument/rename", params, snapshot, token)
      .await?;
    if let Some(edit) = &mut edit {
      normalize_workspace_edit(edit);
    }
    Ok(edit)
  }

  pub async fn provide_completion(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    context: Option<&lsp::CompletionContext>,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::CompletionResponse>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut params = Self::text_document_position(module, position);
    if let Some(context) = context {
      params["context"] = json!(context);
    }
    let mut response: Option<lsp::CompletionResponse> = self
      .request("textDocument/completion", params, snapshot, token)
      .await?;
    if let Some(response) = &mut response {
      let items = match response {
        lsp::CompletionResponse::Array(items) => items,
        lsp::CompletionResponse::List(list) => &mut list.items,
      };
      for item in items {
        if let Some(data) = &mut item.data {
          let raw = std::mem::take(data);
          *data = json!(CompletionItemData::TsGo(TsGoCompletionItemData {
            uri: module.uri.as_ref().clone(),
            data: raw,
          }));
        }
      }
    }
    Ok(response)
  }

  pub async fn resolve_completion_item(
    &self,
    mut item: lsp::CompletionItem,
    data: TsGoCompletionItemData,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<lsp::CompletionItem, AnyError> {
    item.data = Some(data.data);
    let mut item: lsp::CompletionItem = self
      .request("completionItem/resolve", json!(item), snapshot, token)
      .await?;
    item.data = None;
    Ok(item)
  }

  pub async fn provide_signature_help(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    context: Option<lsp::SignatureHelpContext>,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::SignatureHelp>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut params = Self::text_document_position(module, position);
    if let Some(context) = context {
      params["context"] = json!(context);
    }
    self
      .request("textDocument/signatureHelp", params, snapshot, token)
      .await
  }

  pub async fn provide_code_actions(
    &self,
    module: &DocumentModule,
    range: lsp::Range,
    context: &lsp::CodeActionContext,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::CodeActionResponse>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    // tsgo only sees its own diagnostics; hand back the ones it reported.
    let mut context = context.clone();
    context
      .diagnostics
      .retain(|d| d.source.as_deref() == Some("deno-ts"));
    for diagnostic in &mut context.diagnostics {
      diagnostic.source = Some("ts".to_string());
    }
    let mut response: Option<lsp::CodeActionResponse> = self
      .request(
        "textDocument/codeAction",
        json!({
          "textDocument": { "uri": module.uri.as_ref() },
          "range": range,
          "context": context,
        }),
        snapshot,
        token,
      )
      .await?;
    if let Some(response) = &mut response {
      normalize_code_action_response(response, snapshot);
    }
    Ok(response)
  }

  pub async fn resolve_code_action(
    &self,
    mut action: lsp::CodeAction,
    data: TsGoCodeActionData,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<lsp::CodeAction, AnyError> {
    action.data = Some(data.tsgo_data);
    let mut action: lsp::CodeAction = self
      .request("codeAction/resolve", json!(action), snapshot, token)
      .await?;
    action.data = None;
    normalize_code_action(&mut action);
    Ok(action)
  }

  pub async fn provide_document_symbols(
    &self,
    module: &DocumentModule,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<lsp::DocumentSymbolResponse>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    self
      .request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": module.uri.as_ref() } }),
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_folding_range(
    &self,
    module: &DocumentModule,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::FoldingRange>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    self
      .request(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": module.uri.as_ref() } }),
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_selection_ranges(
    &self,
    module: &DocumentModule,
    positions: &[lsp::Position],
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::SelectionRange>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    self
      .request(
        "textDocument/selectionRange",
        json!({
          "textDocument": { "uri": module.uri.as_ref() },
          "positions": positions,
        }),
        snapshot,
        token,
      )
      .await
  }

  pub async fn provide_inlay_hint(
    &self,
    module: &DocumentModule,
    range: lsp::Range,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::InlayHint>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut hints: Option<Vec<lsp::InlayHint>> = self
      .request(
        "textDocument/inlayHint",
        json!({
          "textDocument": { "uri": module.uri.as_ref() },
          "range": range,
        }),
        snapshot,
        token,
      )
      .await?;
    if let Some(hints) = &mut hints {
      for hint in hints {
        if let lsp::InlayHintLabel::LabelParts(parts) = &mut hint.label {
          for part in parts {
            if let Some(location) = &mut part.location {
              normalize_location(location);
            }
          }
        }
      }
    }
    Ok(hints)
  }

  pub async fn provide_prepare_call_hierarchy(
    &self,
    module: &DocumentModule,
    position: lsp::Position,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::CallHierarchyItem>>, AnyError> {
    if !is_tsgo_module(module) {
      return Ok(None);
    }
    let mut items: Option<Vec<lsp::CallHierarchyItem>> = self
      .request(
        "textDocument/prepareCallHierarchy",
        Self::text_document_position(module, position),
        snapshot,
        token,
      )
      .await?;
    if let Some(items) = &mut items {
      for item in items {
        item.uri = normalize_uri(&item.uri);
      }
    }
    Ok(items)
  }

  pub async fn provide_call_hierarchy_incoming_calls(
    &self,
    item: &lsp::CallHierarchyItem,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::CallHierarchyIncomingCall>>, AnyError> {
    let mut calls: Option<Vec<lsp::CallHierarchyIncomingCall>> = self
      .request(
        "callHierarchy/incomingCalls",
        json!({ "item": item }),
        snapshot,
        token,
      )
      .await?;
    if let Some(calls) = &mut calls {
      for call in calls {
        call.from.uri = normalize_uri(&call.from.uri);
      }
    }
    Ok(calls)
  }

  pub async fn provide_call_hierarchy_outgoing_calls(
    &self,
    item: &lsp::CallHierarchyItem,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::CallHierarchyOutgoingCall>>, AnyError> {
    let mut calls: Option<Vec<lsp::CallHierarchyOutgoingCall>> = self
      .request(
        "callHierarchy/outgoingCalls",
        json!({ "item": item }),
        snapshot,
        token,
      )
      .await?;
    if let Some(calls) = &mut calls {
      for call in calls {
        call.to.uri = normalize_uri(&call.to.uri);
      }
    }
    Ok(calls)
  }

  pub async fn provide_workspace_symbol(
    &self,
    query: &str,
    snapshot: &Arc<StateSnapshot>,
    token: &CancellationToken,
  ) -> Result<Option<Vec<lsp::SymbolInformation>>, AnyError> {
    let mut symbols: Option<Vec<lsp::SymbolInformation>> = self
      .request(
        "workspace/symbol",
        json!({ "query": query }),
        snapshot,
        token,
      )
      .await?;
    if let Some(symbols) = &mut symbols {
      for symbol in symbols {
        normalize_location(&mut symbol.location);
      }
    }
    Ok(symbols)
  }
}

fn normalize_location(location: &mut lsp::Location) {
  location.uri = normalize_uri(&location.uri);
}

fn normalize_goto_definition_response(
  response: &mut lsp::GotoDefinitionResponse,
) {
  match response {
    lsp::GotoDefinitionResponse::Scalar(location) => {
      normalize_location(location)
    }
    lsp::GotoDefinitionResponse::Array(locations) => {
      locations.iter_mut().for_each(normalize_location)
    }
    lsp::GotoDefinitionResponse::Link(links) => {
      for link in links {
        link.target_uri = normalize_uri(&link.target_uri);
      }
    }
  }
}

fn normalize_diagnostic(diagnostic: &mut lsp::Diagnostic) {
  diagnostic.source = Some("deno-ts".to_string());
  if let Some(related_information) = &mut diagnostic.related_information {
    for info in related_information {
      normalize_location(&mut info.location);
    }
  }
}

fn normalize_workspace_edit(edit: &mut lsp::WorkspaceEdit) {
  if let Some(changes) = &mut edit.changes {
    *changes = std::mem::take(changes)
      .into_iter()
      .map(|(uri, edits)| (normalize_uri(&uri), edits))
      .collect();
  }
  if let Some(document_changes) = &mut edit.document_changes {
    match document_changes {
      lsp::DocumentChanges::Edits(edits) => {
        for edit in edits {
          edit.text_document.uri = normalize_uri(&edit.text_document.uri);
        }
      }
      lsp::DocumentChanges::Operations(operations) => {
        for operation in operations {
          match operation {
            lsp::DocumentChangeOperation::Edit(edit) => {
              edit.text_document.uri = normalize_uri(&edit.text_document.uri);
            }
            lsp::DocumentChangeOperation::Op(op) => match op {
              lsp::ResourceOp::Create(create) => {
                create.uri = normalize_uri(&create.uri);
              }
              lsp::ResourceOp::Rename(rename) => {
                rename.old_uri = normalize_uri(&rename.old_uri);
                rename.new_uri = normalize_uri(&rename.new_uri);
              }
              lsp::ResourceOp::Delete(delete) => {
                delete.uri = normalize_uri(&delete.uri);
              }
            },
          }
        }
      }
    }
  }
}

fn normalize_code_action(action: &mut lsp::CodeAction) {
  if let Some(edit) = &mut action.edit {
    normalize_workspace_edit(edit);
  }
  if let Some(diagnostics) = &mut action.diagnostics {
    diagnostics.iter_mut().for_each(normalize_diagnostic);
  }
  if let Some(data) = action.data.take() {
    action.data = Some(json!(TsGoCodeActionData { tsgo_data: data }));
  }
}

fn is_organize_imports(kind: &lsp::CodeActionKind) -> bool {
  let kind = kind.as_str();
  kind == lsp::CodeActionKind::SOURCE_ORGANIZE_IMPORTS.as_str()
    || kind.starts_with("source.organizeImports.")
}

fn normalize_code_action_response(
  response: &mut lsp::CodeActionResponse,
  snapshot: &StateSnapshot,
) {
  if snapshot.config.client_provided_organize_imports_capable() {
    response.retain(|item| match item {
      lsp::CodeActionOrCommand::CodeAction(action) => {
        action.kind.as_ref().is_none_or(|k| !is_organize_imports(k))
      }
      lsp::CodeActionOrCommand::Command(_) => true,
    });
  }
  for item in response {
    if let lsp::CodeActionOrCommand::CodeAction(action) = item {
      normalize_code_action(action);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn organize_imports_kinds() {
    assert!(is_organize_imports(
      &lsp::CodeActionKind::SOURCE_ORGANIZE_IMPORTS
    ));
    assert!(is_organize_imports(&lsp::CodeActionKind::from(
      "source.organizeImports.ts".to_string()
    )));
    assert!(!is_organize_imports(&lsp::CodeActionKind::QUICKFIX));
  }

  #[test]
  fn language_ids() {
    assert_eq!(tsgo_language_id(LanguageId::Tsx), Some("typescriptreact"));
    assert_eq!(tsgo_language_id(LanguageId::Markdown), None);
  }

  #[test]
  fn diagnostics_are_attributed_to_deno_ts() {
    let mut diagnostic = lsp::Diagnostic {
      source: Some("ts".to_string()),
      ..Default::default()
    };
    normalize_diagnostic(&mut diagnostic);
    assert_eq!(diagnostic.source.as_deref(), Some("deno-ts"));
  }

  #[test]
  fn import_specifiers_keep_their_extension() {
    let value = language_configuration(Ok(json!({ "preferences": {} })));
    assert_eq!(
      value["preferences"]["importModuleSpecifierEnding"],
      json!("js")
    );
    let value = language_configuration(Ok(json!({
      "preferences": { "importModuleSpecifierEnding": "minimal" }
    })));
    assert_eq!(
      value["preferences"]["importModuleSpecifierEnding"],
      json!("minimal")
    );
    let value = language_configuration(Ok(Value::Null));
    assert_eq!(
      value["preferences"]["importModuleSpecifierEnding"],
      json!("js")
    );
  }

  #[test]
  fn code_action_data_round_trips() {
    let mut action = lsp::CodeAction {
      title: "x".to_string(),
      data: Some(json!({ "a": 1 })),
      ..Default::default()
    };
    normalize_code_action(&mut action);
    let data: TsGoCodeActionData =
      serde_json::from_value(action.data.unwrap()).unwrap();
    assert_eq!(data.tsgo_data, json!({ "a": 1 }));
  }
}
