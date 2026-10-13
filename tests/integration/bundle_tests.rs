// Copyright 2018-2026 the Deno authors. MIT license.

// denext: `deno bundle` uses an esbuild shipped next to the `deno`
// executable (denext's CLI archives) or named by DENO_ESBUILD_BINARY
// instead of downloading one (cli/tools/bundle/esbuild.rs).

use test_util as util;
use util::PathRef;
use util::TestContext;
use util::TestContextBuilder;
use util::test;

/// An npm registry nothing listens on: a run that needs to download esbuild
/// fails.
const UNREACHABLE_REGISTRY: &str = "http://127.0.0.1:9/";

fn esbuild_name() -> &'static str {
  if cfg!(windows) {
    "esbuild.exe"
  } else {
    "esbuild"
  }
}

/// Bundles once through the test npm registry and returns the esbuild that
/// run downloaded into the context's DENO_DIR.
fn download_esbuild(context: &TestContext) -> PathRef {
  let temp_dir = context.temp_dir();
  temp_dir.write("main.ts", "console.log(\"bundled\" as string);\n");
  context
    .new_command()
    .args("bundle --output=first.js main.ts")
    .run()
    .skip_output_check()
    .assert_exit_code(0);
  let dl = context.deno_dir().path().join("dl");
  let esbuild_dir = std::fs::read_dir(dl.as_path())
    .unwrap()
    .map(|e| e.unwrap().path())
    .find(|p| {
      p.file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("esbuild-")
    })
    .expect("deno bundle downloaded no esbuild");
  let esbuild = std::fs::read_dir(&esbuild_dir)
    .unwrap()
    .map(|e| e.unwrap().path())
    .find(|p| p.is_file())
    .expect("no esbuild binary in the download folder");
  PathRef::new(esbuild)
}

fn assert_bundle_ran(context: &TestContext, out: &str) {
  let bundled = context.temp_dir().read_to_string(out);
  assert!(bundled.contains("bundled"), "{bundled}");
}

#[test]
fn bundle_uses_esbuild_next_to_deno_without_network() {
  let context = TestContextBuilder::for_npm().use_temp_cwd().build();
  let esbuild = download_esbuild(&context);

  // A copy of deno with the pinned esbuild beside it, run with an empty
  // DENO_DIR and no reachable registry.
  let bin_dir = context.temp_dir().path().join("bin");
  bin_dir.create_dir_all();
  let deno = bin_dir.join(if cfg!(windows) { "deno.exe" } else { "deno" });
  util::deno_exe_path().copy(&deno);
  deno.mark_executable();
  esbuild.copy(&bin_dir.join(esbuild_name()));
  bin_dir.join(esbuild_name()).mark_executable();

  let empty_deno_dir = context.temp_dir().path().join("empty_deno_dir");
  context
    .new_command()
    .name(deno.as_path())
    .args("bundle --output=offline.js main.ts")
    .env("DENO_DIR", empty_deno_dir.to_string_lossy())
    .env("NPM_CONFIG_REGISTRY", UNREACHABLE_REGISTRY)
    .run()
    .skip_output_check()
    .assert_exit_code(0);
  assert_bundle_ran(&context, "offline.js");
  // Nothing was downloaded.
  assert!(!empty_deno_dir.join("dl").exists());
  assert!(!empty_deno_dir.join("npm").exists());

  // A sibling that reports another version is ignored: with no registry the
  // download fails...
  #[cfg(unix)]
  {
    let fake = bin_dir.join("esbuild");
    // Fails the bundle if it is ever used for more than `--version`.
    fake.write("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 0.0.1; exit 0; fi\nexit 1\n");
    fake.mark_executable();
    let offline = context
      .new_command()
      .name(deno.as_path())
      .args("bundle --output=ignored.js main.ts")
      .env("DENO_DIR", empty_deno_dir.to_string_lossy())
      .env("NPM_CONFIG_REGISTRY", UNREACHABLE_REGISTRY)
      .run();
    offline.skip_output_check().assert_exit_code(1);
    assert!(
      offline.combined_output().contains("esbuild"),
      "{}",
      offline.combined_output()
    );
    // ...and through the test registry deno downloads and uses its own.
    context
      .new_command()
      .name(deno.as_path())
      .args("bundle --output=ignored.js main.ts")
      .env("DENO_DIR", empty_deno_dir.to_string_lossy())
      .run()
      .skip_output_check()
      .assert_exit_code(0);
    assert_bundle_ran(&context, "ignored.js");
  }
}

#[test]
fn bundle_uses_deno_esbuild_binary_without_network() {
  let context = TestContextBuilder::for_npm().use_temp_cwd().build();
  let esbuild = download_esbuild(&context);
  let empty_deno_dir = context.temp_dir().path().join("empty_deno_dir");
  context
    .new_command()
    .args("bundle --output=override.js main.ts")
    .env("DENO_DIR", empty_deno_dir.to_string_lossy())
    .env("NPM_CONFIG_REGISTRY", UNREACHABLE_REGISTRY)
    .env("DENO_ESBUILD_BINARY", esbuild.to_string_lossy())
    .run()
    .skip_output_check()
    .assert_exit_code(0);
  assert_bundle_ran(&context, "override.js");
  assert!(!empty_deno_dir.join("dl").exists());

  // An override that is not a file is an error, not a silent download.
  context
    .new_command()
    .args("bundle --output=missing.js main.ts")
    .env("DENO_DIR", empty_deno_dir.to_string_lossy())
    .env("DENO_ESBUILD_BINARY", "./does-not-exist")
    .run()
    .assert_matches_text(
      "[WILDCARD]DENO_ESBUILD_BINARY is set to ./does-not-exist, which is not a file[WILDCARD]",
    )
    .assert_exit_code(1);
}
