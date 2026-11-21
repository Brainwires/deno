// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (kernel sandbox, WP-A of denext.30): the runs only a terminal
//! shows. A spec has no pseudo-terminal, so these are here: the deny
//! ceiling of a run where a prompt can happen (D1), and a compiled
//! program's refused-prompt line (review D4).

use test_util as util;
use test_util::test;
use util::TestContextBuilder;

/// Whether `--kernel-sandbox=enforce` can run here (`deno info --json`), as
/// the specs' `kernelSandboxEnforce` condition decides it.
fn enforce_available() -> bool {
  if !cfg!(unix) {
    return false;
  }
  let Ok(output) = std::process::Command::new(util::deno_exe_path())
    .args(["info", "--json"])
    .output()
  else {
    return false;
  };
  let Ok(info) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
  else {
    return false;
  };
  let ks = &info["kernelSandbox"];
  if ks["enforcerBuilt"] != serde_json::Value::Bool(true) {
    return false;
  }
  if cfg!(target_os = "linux") {
    ks["landlockAbi"].as_u64().is_some()
      && ks["capabilities"]["seccomp"] == serde_json::Value::Bool(true)
  } else {
    ks["capabilities"]["seatbelt"] == serde_json::Value::Bool(true)
  }
}

/// A program that reads a file Deno asks for (the prompt grants it), then
/// tries the denied path from native code and from a child, then says
/// whether no-new-privileges is set (Linux).
const DENY_CEILING_PROGRAM: &str = r#"
const isMac = Deno.build.os === "darwin";
const libc = Deno.dlopen(isMac ? "/usr/lib/libSystem.B.dylib" : "libc.so.6", {
  open: { parameters: ["buffer", "i32"], result: "i32" },
  close: { parameters: ["i32"], result: "i32" },
});
const c = (s: string) => new TextEncoder().encode(s + "\0");
console.log(`granted: ${Deno.readTextFileSync("open.txt").trim()}`);
const fd = libc.symbols.open(c("secret/key.txt"), 0);
console.log(`native read of the deny: ${fd >= 0 ? "ok" : "refused"}`);
if (fd >= 0) libc.symbols.close(fd);
const child = new Deno.Command("cat", {
  args: ["secret/key.txt"],
  stdout: "null",
  stderr: "null",
}).outputSync();
console.log(`child read of the deny: ${child.success ? "ok" : "refused"}`);
if (!isMac) {
  const status = Deno.readTextFileSync("/proc/self/status");
  console.log(`no_new_privs: ${/NoNewPrivs:\s*(\d)/.exec(status)?.[1]}`);
}
console.log("done");
"#;

fn deny_ceiling_context() -> util::TestContext {
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let dir = context.temp_dir();
  dir.write("main.ts", DENY_CEILING_PROGRAM);
  dir.write("open.txt", "outside the deny\n");
  dir.create_dir_all("secret");
  dir.write("secret/key.txt", "TOP-SECRET\n");
  context
}

/// D1 on macOS: with a terminal and a `--deny-read`, `auto` at R4 enforces
/// the deny alone. A prompt still grants a path outside it; native code and
/// a child can't read the denied path.
#[cfg(target_os = "macos")]
#[test(flaky)]
fn deny_ceiling_prompt_grants_outside_the_deny_macos() {
  if !enforce_available() {
    return;
  }
  let context = deny_ceiling_context();
  context
    .new_command()
    .env("DENO_INTERNAL_KERNEL_SANDBOX_STAGE", "r4")
    .args_vec([
      "run",
      "--quiet",
      "--allow-ffi",
      "--allow-run=cat",
      "--deny-read=secret",
      "main.ts",
    ])
    .with_pty(|mut console| {
      console.expect("Deno requests read access to \"open.txt\"");
      console.human_delay();
      console.write_line_raw("y");
      console.expect("granted: outside the deny");
      console.expect("native read of the deny: refused");
      console.expect("child read of the deny: refused");
      console.expect("done");
    });
}

/// D1 on Linux: no overmount, so the deny is Deno's check only (reported,
/// decision O4) and nothing else changes: the prompt works and no
/// no-new-privileges is set (a setuid program a prompt allows still runs).
#[cfg(target_os = "linux")]
#[test(flaky)]
fn deny_ceiling_prompt_changes_nothing_linux() {
  if !enforce_available() {
    return;
  }
  let own = std::fs::read_to_string("/proc/self/status")
    .ok()
    .and_then(|s| {
      s.lines()
        .find_map(|l| l.strip_prefix("NoNewPrivs:"))
        .map(|v| v.trim().to_string())
    })
    .unwrap_or_default();
  let context = deny_ceiling_context();
  context
    .new_command()
    .env("DENO_INTERNAL_KERNEL_SANDBOX_STAGE", "r4")
    .args_vec([
      "run",
      "--quiet",
      "--allow-ffi",
      "--allow-run=cat",
      "--deny-read=secret",
      "main.ts",
    ])
    .with_pty(|mut console| {
      console.expect("Deno requests read access to \"open.txt\"");
      console.human_delay();
      console.write_line_raw("y");
      console.expect("granted: outside the deny");
      console.expect("child read of the deny:");
      console.expect("Deno requests read access to \"/proc/self/status\"");
      console.human_delay();
      console.write_line_raw("y");
      console.expect(format!("no_new_privs: {own}"));
      console.expect("done");
    });
}

/// Review D4: a compiled program whose permissions the kernel sandbox fixed
/// refuses every prompt, and the first refusal says once why, naming the
/// switch that keeps prompts.
#[test(flaky)]
fn compiled_prompt_refused_line() {
  if !enforce_available() {
    return;
  }
  let context = TestContextBuilder::new().use_temp_cwd().build();
  let dir = context.temp_dir();
  dir.write(
    "main.ts",
    r#"
for (const name of ["HOME", "PATH"]) {
  try {
    Deno.env.get(name);
    console.log("env read");
  } catch (err) {
    console.log(`env ${name}: ${(err as Error).name}`);
  }
}
console.log(`net: ${(await Deno.permissions.request({ name: "net" })).state}`);
console.log("done");
"#,
  );
  let exe = dir.path().join("prog_prompt");
  context
    .new_command()
    .args_vec([
      "compile",
      "--quiet",
      "--kernel-sandbox=enforce",
      "--output",
      &exe.to_string_lossy(),
      "main.ts",
    ])
    .run()
    .skip_output_check()
    .assert_exit_code(0);
  context.new_command().name(&exe).with_pty(|mut console| {
    console.expect("deno compile --kernel-sandbox=off");
    console.expect("env HOME: NotCapable");
    console.expect("env PATH: NotCapable");
    console.expect("net: denied");
    console.expect("done");
    let output = console.all_output();
    assert_eq!(
      output.matches("deno compile --kernel-sandbox=off").count(),
      1,
      "{output}"
    );
    assert!(!output.contains("Allow?"), "{output}");
  });
}
