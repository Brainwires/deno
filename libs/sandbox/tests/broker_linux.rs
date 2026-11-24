// Copyright 2018-2026 the Deno authors. MIT license.

//! The broker's exact write deny, end to end on a real kernel, without the
//! runtime: a child applies an enforcement plan whose write allow is
//! expanded around a deny (Landlock, then the seccomp program, as
//! `--kernel-sandbox=broker` applies it), and reaches its parent, the
//! broker, over a channel.
//!
//! What must hold on every kernel with Landlock:
//! - native code in the child can't create, rename or remove directly in
//!   the allowed folder, nor touch the denied folder at all, by any name;
//!   it can write existing files and existing subfolders;
//! - the same changes asked of the broker succeed in the allowed folder
//!   and are refused in the denied one, by its name and through a link.
//!
//! No test harness: the process must have one thread when the plan
//! applies, and forks. Exits 0 when everything holds (or when the kernel
//! has no Landlock, saying so), 1 otherwise.

#![allow(
  clippy::disallowed_methods,
  clippy::print_stdout,
  clippy::print_stderr,
  reason = "a test program acting on a real temporary directory"
)]

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
  linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
  use std::fs;
  use std::io::Write;
  use std::os::fd::FromRawFd;
  use std::os::fd::OwnedFd;
  use std::path::Path;
  use std::path::PathBuf;

  use deno_permissions::kernel_broker::client::BrokerClient;
  use deno_permissions::kernel_broker::fs::FsBroker;
  use deno_permissions::kernel_broker::fs::FsPolicy;
  use deno_permissions::kernel_broker::protocol::OpenFlags;
  use deno_permissions::kernel_broker::protocol::Remove;
  use deno_permissions::kernel_broker::server::Server;
  use deno_sandbox::enforce_linux;
  use deno_sandbox::plan::EnforcePlan;
  use deno_sandbox::plan::ExpandRoot;
  use deno_sandbox::plan::Handled;
  use deno_sandbox::plan::LinuxPlan;
  use deno_sandbox::plan::NetPlan;
  use deno_sandbox::plan::PLAN_FORMAT;
  use deno_sandbox::plan::SeccompPlan;

  struct Checks {
    failed: usize,
  }

  impl Checks {
    fn expect(&mut self, label: &str, ok: bool, detail: String) {
      if ok {
        println!("ok    {label}");
      } else {
        println!("FAIL  {label}: {detail}");
        self.failed += 1;
      }
    }

    /// The OS (the kernel sandbox) must refuse it.
    fn refused<T>(&mut self, label: &str, result: std::io::Result<T>) {
      self.refused_with(label, result, &[]);
    }

    /// As [`Self::refused`], with other errors the kernel refuses this one
    /// with.
    fn refused_with<T>(
      &mut self,
      label: &str,
      result: std::io::Result<T>,
      also: &[i32],
    ) {
      let (ok, detail) = match &result {
        Ok(_) => (false, "it succeeded".to_string()),
        Err(err) => (
          matches!(err.raw_os_error(), Some(libc::EACCES | libc::EPERM))
            || err.raw_os_error().is_some_and(|e| also.contains(&e)),
          err.to_string(),
        ),
      };
      self.expect(label, ok, detail);
    }

    fn works<T>(&mut self, label: &str, result: std::io::Result<T>) {
      let detail = result.as_ref().err().map(|e| e.to_string());
      self.expect(label, result.is_ok(), detail.unwrap_or_default());
    }
  }

  /// The entries the write expansion allows: every entry of `root` but the
  /// denied one (the deny is a direct child here), symlinks left out, as
  /// the policy compiler's walk does it.
  fn expanded(root: &Path, deny: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(root)
      .unwrap()
      .flatten()
      .filter(|e| !e.file_type().unwrap().is_symlink())
      .map(|e| e.path())
      .filter(|p| p != deny)
      .collect();
    out.sort();
    out
  }

  fn child(plan: &EnforcePlan, sock: OwnedFd, out: &Path) -> ! {
    if let Err(err) = enforce_linux::apply_self(plan) {
      println!("FAIL  the plan doesn't apply: {err}");
      std::process::exit(1);
    }
    let broker = BrokerClient::new(sock);
    let mut c = Checks { failed: 0 };
    let p = |rel: &str| out.join(rel);
    let create = OpenFlags::WRITE.with(OpenFlags::CREATE);

    // native code: the kernel alone
    c.refused(
      "native: create in the allowed folder",
      fs::File::create(p("n")),
    );
    c.refused(
      "native: mkdir in the allowed folder",
      fs::create_dir(p("nd")),
    );
    c.refused(
      "native: rename in the allowed folder",
      fs::rename(p("existing"), p("renamed")),
    );
    c.refused(
      "native: remove in the allowed folder",
      fs::remove_file(p("existing")),
    );
    c.refused("native: create in the deny", fs::File::create(p("keep/n")));
    c.refused(
      "native: create in the deny through a link",
      fs::File::create(p("to-keep/n")),
    );
    c.refused(
      "native: overwrite in the deny",
      fs::OpenOptions::new().write(true).open(p("keep/kept")),
    );
    c.refused(
      "native: remove in the deny",
      fs::remove_file(p("keep/kept")),
    );
    // before Landlock ABI 2 (no REFER right) a rename across folders is
    // refused as a cross-device one
    c.refused_with(
      "native: rename out of the deny",
      fs::rename(p("keep/kept"), p("sub/stolen")),
      &[libc::EXDEV],
    );
    c.works(
      "native: overwrite an existing file",
      fs::OpenOptions::new()
        .write(true)
        .open(p("existing"))
        .and_then(|mut f| f.write_all(b"changed")),
    );
    c.works(
      "native: create in an existing subfolder",
      fs::File::create(p("sub/n")),
    );
    c.refused(
      "native: create outside the allow",
      fs::File::create(out.parent().unwrap().join("outside/n")),
    );

    // the broker: Deno's rule, exactly
    c.works(
      "broker: features",
      broker.hello().and_then(|f| {
        if f & 1 != 0 {
          Ok(())
        } else {
          Err(std::io::Error::other("no file operations"))
        }
      }),
    );
    c.works(
      "broker: create in the allowed folder",
      broker
        .open(&p("b"), create, 0o600)
        .and_then(|fd| fs::File::from(fd).write_all(b"b")),
    );
    c.works(
      "broker: mkdir in the allowed folder",
      broker.mkdir(&p("bd"), 0o755),
    );
    // a folder made after the plan applied has no rule: native code can't
    // write in it, the broker can
    c.refused(
      "native: create in a folder made after start",
      fs::File::create(p("bd/n")),
    );
    c.works(
      "broker: create in a folder made after start",
      broker.open(&p("bd/f"), create, 0o600).map(drop),
    );
    c.works(
      "broker: rename in the allowed folder",
      broker.rename(&p("b"), &p("b2"), false),
    );
    c.works(
      "broker: remove in the allowed folder",
      broker.remove(&p("b2"), Remove::File),
    );
    c.works(
      "broker: remove a tree made after start",
      broker.remove(&p("bd"), Remove::Tree),
    );
    c.works("broker: truncate", broker.truncate(&p("existing"), 2));
    for (label, rel) in [
      ("broker: create in the deny", "keep/x"),
      ("broker: create in the deny through a link", "to-keep/x"),
    ] {
      c.refused(label, broker.open(&p(rel), create, 0o600));
    }
    c.refused(
      "broker: overwrite in the deny",
      broker.open(&p("keep/kept"), OpenFlags::WRITE, 0),
    );
    c.refused(
      "broker: remove in the deny",
      broker.remove(&p("keep/kept"), Remove::File),
    );
    c.refused(
      "broker: rename into the deny",
      broker.rename(&p("existing"), &p("keep/in"), false),
    );
    c.refused(
      "broker: rename the deny",
      broker.rename(&p("keep"), &p("gone"), false),
    );
    c.refused(
      "broker: remove the tree that holds the deny",
      broker.remove(out, Remove::Tree),
    );
    c.refused(
      "broker: create outside the allow",
      broker.open(&out.parent().unwrap().join("outside/x"), create, 0o600),
    );
    c.refused(
      "broker: chmod in the deny",
      broker.chmod(&p("keep/kept"), 0o777, false),
    );
    c.works("broker: chmod", broker.chmod(&p("existing"), 0o640, false));
    std::process::exit(if c.failed == 0 { 0 } else { 1 });
  }

  pub fn main() {
    let abi = match deno_sandbox::linux::landlock_abi() {
      Ok(abi) => abi,
      Err(err) => {
        println!("SKIP  no Landlock here: {err}");
        return;
      }
    };
    println!("Landlock ABI {abi}");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let out = root.join("out");
    fs::create_dir_all(out.join("keep")).unwrap();
    fs::create_dir_all(out.join("sub")).unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(out.join("existing"), "existing").unwrap();
    fs::write(out.join("keep/kept"), "kept").unwrap();
    std::os::unix::fs::symlink("keep", out.join("to-keep")).unwrap();

    let deny = out.join("keep");
    let plan = EnforcePlan {
      format: PLAN_FORMAT,
      linux: Some(LinuxPlan {
        handled: Handled {
          read: false,
          write: true,
          exec: false,
        },
        rules: Vec::new(),
        expand: Vec::new(),
        expand_write: vec![ExpandRoot {
          root: out.clone(),
          deny: vec![deny.clone()],
        }],
        net: NetPlan::Open,
        scope_signal: false,
        scope_abstract_unix: false,
        prebound: Vec::new(),
        seccomp: SeccompPlan {
          attack_surface: true,
          clone3_enosys: true,
          refuse_io_uring: true,
          unix_sockets: false,
          ..Default::default()
        },
        exec_gate: None,
      }),
      macos: None,
      windows: None,
      env_keep: None,
      probe: None,
      module_files: Vec::new(),
      expanded: Vec::new(),
      config_env: Vec::new(),
      cache_overlay: None,
      broker: None,
      expanded_write: expanded(&out, &deny),
      entries: Vec::new(),
      member_configs: Vec::new(),
      listed_dirs: Vec::new(),
    };
    let policy = FsPolicy {
      read: None,
      deny_read: Vec::new(),
      write: Some(vec![out.clone()]),
      deny_write: vec![deny.clone()],
    };

    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: an array of two descriptors for the kernel to fill
    let rc = unsafe {
      libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr())
    };
    assert_eq!(rc, 0);
    // SAFETY: just created
    let (ours, theirs) =
      unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    std::io::stdout().flush().unwrap();
    // SAFETY: this process has one thread
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
      drop(ours);
      child(&plan, theirs, &out);
    }
    drop(theirs);
    let server = Server::new(FsBroker::new(&policy).unwrap());
    server.spawn(ours).unwrap();
    let mut status = 0;
    // SAFETY: our child and a valid out parameter
    unsafe { libc::waitpid(pid, &mut status, 0) };
    let ok = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
    // what the deny protects is as it was
    let kept = fs::read_to_string(deny.join("kept")).unwrap();
    let entries = fs::read_dir(&deny).unwrap().count();
    if kept != "kept" || entries != 1 {
      println!("FAIL  the denied folder changed");
      std::process::exit(1);
    }
    if !ok {
      println!("FAILED (child status {status:#x})");
      std::process::exit(1);
    }
    println!("all held");
  }
}
