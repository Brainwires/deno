// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::env;
use std::sync::Arc;

use deno_core::error::AnyError;
use deno_lib::util::result::js_error_downcast_ref;
use deno_lib::version::otel_runtime_config;
use deno_runtime::deno_telemetry::OtelConfig;
use deno_runtime::fmt_errors::format_js_error;
use deno_runtime::tokio_util::create_and_run_current_thread_with_maybe_metrics;
use deno_terminal::colors;
use indexmap::IndexMap;

use self::binary::extract_standalone;
use self::file_system::DenoRtSys;

pub mod binary;
mod code_cache;
pub mod desktop;
pub mod file_system;
pub mod hmr;
mod node;
pub mod run;

pub fn unstable_exit_cb(feature: &str, api_name: &str) {
  log::error!(
    "Unstable API '{api_name}'. The `--unstable-{}` flag must be provided.",
    feature
  );
  deno_runtime::exit(70);
}

fn exit_with_message(message: &str, code: i32) -> ! {
  log::error!(
    "{}: {}",
    colors::red_bold("error"),
    message.trim_start_matches("error: ")
  );
  deno_runtime::exit(code);
}

fn unwrap_or_exit<T>(result: Result<T, AnyError>) -> T {
  match result {
    Ok(value) => value,
    Err(error) => {
      // A resource limit that terminated the program surfaces as "execution
      // terminated"; report the limit instead.
      deno_lib::resource_limits::exit_if_exceeded();
      let error_string = match js_error_downcast_ref(&error) {
        Some(js_error) => format_js_error(js_error, None),
        None => format!("{:?}", error),
      };

      exit_with_message(&error_string, 1);
    }
  }
}

pub fn load_env_vars(env_vars: &IndexMap<String, String>) {
  env_vars.iter().for_each(|env_var| {
    if env::var(env_var.0).is_err() {
      // SAFETY: called during single-threaded startup before tokio runtime
      unsafe { std::env::set_var(env_var.0, env_var.1) };
    }
  })
}

/// denext: the opt-in crash report (`DENEXT_CRASH_REPORT_URL`, DENEXT.md
/// item 180). Upstream's denort installs no panic hook; this one sends the
/// report, then runs the default hook (and, in a release build, aborts).
fn setup_panic_hook() {
  deno_runtime::crash_report::install(deno_runtime::crash_report::BinaryInfo {
    binary: "denort",
    version: deno_lib::version::DENO_VERSION_INFO.deno,
    git_hash: deno_lib::version::DENO_VERSION_INFO.git_hash,
    root_cert_store: Some(crash_report_root_cert_store),
  });
  let orig_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |panic_info| {
    deno_runtime::crash_report::report_panic(panic_info);
    orig_hook(panic_info);
  }));
}

/// The root certificates a crash report trusts: `DENO_TLS_CA_STORE`,
/// `DENO_CERT` and `NODE_EXTRA_CA_CERTS`, as the CLI's.
fn crash_report_root_cert_store()
-> Option<deno_runtime::deno_tls::rustls::RootCertStore> {
  deno_lib::args::get_root_cert_store(
    &sys_traits::impls::RealSys,
    None,
    None,
    None,
  )
  .ok()
}

#[inline(always)]
pub fn main() {
  setup_panic_hook();
  init_logging(None, None);

  // Enable ANSI virtual terminal processing on Windows consoles that don't
  // do so by default (e.g. Windows Server / classic conhost), so that
  // compiled binaries render colored output instead of raw escape codes.
  // This mirrors the setup done in the main `deno` binary entrypoint.
  #[cfg(windows)]
  colors::enable_ansi(); // For Windows 10

  deno_runtime::deno_permissions::mark_standalone();

  rustls::crypto::aws_lc_rs::default_provider()
    .install_default()
    .unwrap();
  deno_runtime::crash_report::maybe_selftest();

  let args: Vec<_> = env::args_os().collect();
  let standalone = extract_standalone(Cow::Owned(args));
  let future = async move {
    match standalone {
      Ok(data) => {
        let sys = if data.metadata.self_extracting.is_some() {
          binary::extract_vfs_to_disk(&data.vfs, &data.root_path)?;
          DenoRtSys::new_self_extracting(data.vfs.clone())
        } else {
          DenoRtSys::new(data.vfs.clone())
        };
        deno_runtime::deno_telemetry::init(
          &sys,
          otel_runtime_config(),
          data.metadata.otel_config.clone(),
        )?;
        init_logging(
          data.metadata.log_level,
          Some(data.metadata.otel_config.clone()),
        );
        load_env_vars(&data.metadata.env_vars_from_env_file);
        let exit_code = run::run(Arc::new(sys.clone()), sys, data).await?;
        deno_lib::resource_limits::exit_if_exceeded();
        deno_runtime::exit(exit_code);
      }
      Err(err) => Err(err),
    }
  };

  unwrap_or_exit::<()>(create_and_run_current_thread_with_maybe_metrics(
    future,
  ));
}

pub fn init_logging(
  maybe_level: Option<log::Level>,
  otel_config: Option<OtelConfig>,
) {
  deno_lib::util::logger::init(deno_lib::util::logger::InitLoggingOptions {
    maybe_level,
    otel_config,
    on_log_start: || {},
    on_log_end: || {},
  })
}
