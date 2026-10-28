// Copyright 2018-2026 the Deno authors. MIT license.

use std::sync::Arc;

use deno_config::deno_json::NewestDependencyDate;
use deno_config::deno_json::NodeModulesDirMode;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_graph::packages::JsrPackageInfo;
use deno_path_util::ResolveUrlOrPathError;
use deno_runtime::WorkerExecutionMode;
use deno_runtime::deno_permissions::PermissionsContainer;

use crate::args::ConfigFlag;
use crate::args::DeployFlags;
use crate::args::Flags;
use crate::factory::CliFactory;
use crate::ops;

pub async fn deploy(
  mut flags: Flags,
  deploy_flags: DeployFlags,
) -> Result<i32, AnyError> {
  flags.node_modules_dir = Some(NodeModulesDirMode::None);
  flags.no_lock = true;
  flags.minimum_dependency_age = Some(NewestDependencyDate::Disabled);
  if deploy_flags.sandbox {
    // SAFETY: only this subcommand is running, nothing else, so it's safe to set an env var.
    unsafe {
      std::env::set_var("DENO_DEPLOY_CLI_SANDBOX", "1");
    }
  }

  let dev_specifier = std::env::var("DENO_DEPLOY_CLI_SPECIFIER").ok();

  // The jsr-fetched CLI resolves its imports from JSR package metadata and
  // reads the user's config itself, in-process. Disable config discovery so
  // the workspace at the user's cwd isn't applied to the deploy CLI's own
  // module graph (which otherwise breaks when a workspace member's name
  // collides with one of the CLI's dependencies). The dev path
  // (DENO_DEPLOY_CLI_SPECIFIER) still wants discovery relative to the CLI
  // source.
  if dev_specifier.is_none() {
    flags.config_flag = ConfigFlag::Disabled;
  }

  let mut factory = CliFactory::from_flags(Arc::new(flags));

  let specifier = if let Some(specifier) = dev_specifier {
    let specifier =
      Url::parse(&specifier).map_err(ResolveUrlOrPathError::UrlParse)?;
    if let Ok(path) = specifier.to_file_path() {
      factory.set_initial_cwd(path);
    }

    specifier
  } else {
    let registry_url = crate::args::jsr_url();
    let file = factory
      .file_fetcher()?
      .fetch_with_options(
        &registry_url.join("@deno/deploy/meta.json").unwrap(),
        deno_resolver::file_fetcher::FetchPermissionsOptionRef::AllowAll,
        deno_resolver::file_fetcher::FetchOptions {
          // jsr.io serves meta.json with `cache-control: max-age=60`, so
          // respect those headers rather than pinning the first cached copy
          // forever (see denoland/deno#35736).
          maybe_cache_setting: Some(
            &deno_cache_dir::file_fetcher::CacheSetting::RespectHeaders,
          ),
          ..Default::default()
        },
      )
      .await?;
    let info = serde_json::from_slice::<JsrPackageInfo>(&file.source)?;
    let latest_version = resolve_deploy_cli_version(&info)
      .expect("expected @deno/deploy to be published");
    Url::parse(&format!("jsr:@deno/deploy@{latest_version}"))
      .map_err(ResolveUrlOrPathError::UrlParse)?
  };

  let worker_factory =
    Arc::new(factory.create_cli_main_worker_factory().await?);

  let mut worker = worker_factory
    .create_custom_worker(
      WorkerExecutionMode::Deploy,
      specifier,
      vec![],
      vec![],
      PermissionsContainer::allow_all(
        factory.permission_desc_parser()?.clone(),
      ),
      vec![ops::deploy::deno_deploy::init()],
      Default::default(),
      None,
    )
    .await?;

  Ok(worker.run().await?)
}

fn resolve_deploy_cli_version(
  info: &JsrPackageInfo,
) -> Option<&deno_semver::Version> {
  info.latest.as_ref().or_else(|| info.versions.keys().max())
}

pub fn get_token_entry() -> Result<keyring::Entry, keyring::Error> {
  keyring::Entry::new("Deno Deploy Token", "Deno Deploy")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn resolve_deploy_cli_version_uses_jsr_latest() {
    let info = serde_json::from_value::<JsrPackageInfo>(serde_json::json!({
      "latest": "1.0.0",
      "versions": {
        "1.0.0": {},
        "2.0.0": {}
      }
    }))
    .unwrap();

    assert_eq!(
      resolve_deploy_cli_version(&info).unwrap().to_string(),
      "1.0.0"
    );
  }

  /// Set on the child process `keyring_opens_an_encrypted_session` starts:
  /// the address of its private bus.
  #[cfg(target_os = "linux")]
  const KEYRING_CHILD_BUS: &str = "DENO_TEST_KEYRING_CHILD_BUS";

  /// The child half of `keyring_opens_an_encrypted_session`: a token lookup
  /// through `keyring`, on the bus the parent hands it. Does nothing in a
  /// normal run.
  #[cfg(target_os = "linux")]
  #[test]
  fn keyring_session_child() {
    if std::env::var_os(KEYRING_CHILD_BUS).is_none() {
      return;
    }
    // The stand-in refuses the session, so this fails; what matters is the
    // OpenSession call it made.
    let _ = get_token_entry().and_then(|e| e.get_password());
  }

  /// The Deno Deploy token travels to the Secret Service over the session
  /// bus. `keyring` opens a "plain" transfer session (the secret in clear on
  /// the bus) unless it is built with a crypto feature (DENEXT.md item 141),
  /// so a stand-in Secret Service on a private bus records the algorithm a
  /// lookup asks for and the test requires the Diffie-Hellman one.
  #[cfg(target_os = "linux")]
  #[test]
  #[allow(clippy::print_stderr, reason = "test code: the skip notice")]
  fn keyring_opens_an_encrypted_session() {
    use std::io::BufRead;
    use std::process::Command;
    use std::process::Stdio;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use dbus::Message;
    use dbus::channel::Channel;
    use dbus::message::MessageType;
    use dbus::strings::ErrorName;

    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
      fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
      }
    }

    let dir = tempfile::tempdir().unwrap();
    let address = format!("unix:path={}", dir.path().join("bus").display());
    let daemon = Command::new("dbus-daemon")
      .args(["--session", "--nofork", "--print-address=1"])
      .arg(format!("--address={address}"))
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .spawn();
    let mut daemon = match daemon {
      Ok(child) => KillOnDrop(child),
      Err(err) if std::env::var_os("CI").is_none() => {
        eprintln!("skipped: no dbus-daemon to run a stand-in on ({err})");
        return;
      }
      Err(err) => panic!("CI needs dbus-daemon for this test: {err}"),
    };
    // The daemon prints its address once it listens.
    let mut printed = String::new();
    std::io::BufReader::new(daemon.0.stdout.take().unwrap())
      .read_line(&mut printed)
      .unwrap();
    assert!(
      printed.starts_with("unix:"),
      "dbus-daemon printed no address: {printed:?}"
    );
    let bus = printed.trim().to_string();

    // The stand-in: owns org.freedesktop.secrets, records every OpenSession
    // algorithm and refuses the session.
    let mut channel = Channel::open_private(&bus).unwrap();
    channel.register().unwrap();
    let request = Message::new_method_call(
      "org.freedesktop.DBus",
      "/org/freedesktop/DBus",
      "org.freedesktop.DBus",
      "RequestName",
    )
    .unwrap()
    .append2("org.freedesktop.secrets", 4u32);
    let reply = channel
      .send_with_reply_and_block(request, Duration::from_secs(5))
      .unwrap();
    assert_eq!(
      reply.get1::<u32>(),
      Some(1),
      "the stand-in must own the Secret Service's name"
    );
    let algorithms = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let service = {
      let algorithms = algorithms.clone();
      let stop = stop.clone();
      std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
          let Some(msg) = channel
            .blocking_pop_message(Duration::from_millis(50))
            .unwrap()
          else {
            continue;
          };
          if msg.msg_type() != MessageType::MethodCall {
            continue;
          }
          if msg.member().as_deref() == Some("OpenSession") {
            algorithms
              .lock()
              .unwrap()
              .push(msg.get1::<String>().unwrap_or_default());
          }
          let name =
            ErrorName::new("org.freedesktop.DBus.Error.NotSupported").unwrap();
          let text = std::ffi::CString::new("stand-in").unwrap();
          let _ = channel.send(msg.error(&name, &text));
        }
      })
    };

    let output = Command::new(std::env::current_exe().unwrap())
      .args([
        "--exact",
        "tools::deploy::tests::keyring_session_child",
        "--nocapture",
      ])
      .env(KEYRING_CHILD_BUS, &bus)
      .env("DBUS_SESSION_BUS_ADDRESS", &bus)
      .output()
      .unwrap();
    stop.store(true, Ordering::SeqCst);
    service.join().unwrap();
    assert!(
      output.status.success(),
      "the child failed: {}{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr)
    );
    let algorithms = algorithms.lock().unwrap().clone();
    assert!(
      !algorithms.is_empty(),
      "keyring never opened a Secret Service session; child output: {}",
      String::from_utf8_lossy(&output.stdout)
    );
    for algorithm in &algorithms {
      assert_ne!(algorithm, "plain", "a plain transfer session was opened");
      assert_eq!(algorithm, "dh-ietf1024-sha256-aes128-cbc-pkcs7");
    }
  }
}
