// Copyright 2018-2026 the Deno authors. MIT license.

//! The cost of a race-safe open against a plain one: a file five
//! directories below an allowed root, opened by `File::open`, by
//! `open_checked` (`openat2` on Linux), by `open_checked` with the component
//! walk, and with a deny inside the root (the walk plus the post-open check).
//! Root descriptors are cached by the scope, as in a running program.

#![allow(
  clippy::disallowed_methods,
  reason = "the benchmark builds a real directory tree"
)]

#[cfg(unix)]
fn bench(c: &mut criterion::Criterion) {
  use deno_permissions::checked_open::CheckedOpenOptions;
  use deno_permissions::checked_open::OpenScope;
  use deno_permissions::checked_open::open_checked;

  let tmp = tempfile::tempdir().unwrap();
  let root = std::fs::canonicalize(tmp.path()).unwrap();
  let dir = root.join("a/b/c/d/e");
  std::fs::create_dir_all(&dir).unwrap();
  std::fs::create_dir_all(root.join("denied")).unwrap();
  let file = dir.join("file.txt");
  std::fs::write(&file, "x").unwrap();
  let read = CheckedOpenOptions::new().read(true);
  let scoped = OpenScope::new(Some(vec![root.clone()]), vec![]).unwrap();
  let walk = OpenScope::new(Some(vec![root.clone()]), vec![])
    .unwrap()
    .force_component_walk(true);
  let deny =
    OpenScope::new(Some(vec![root.clone()]), vec![root.join("denied")])
      .unwrap();

  let mut group = c.benchmark_group("open");
  group.bench_function("std_file_open", |b| {
    b.iter(|| std::fs::File::open(&file).unwrap())
  });
  group.bench_function("open_checked", |b| {
    b.iter(|| open_checked(&scoped, &file, &read).unwrap())
  });
  group.bench_function("open_checked_walk", |b| {
    b.iter(|| open_checked(&walk, &file, &read).unwrap())
  });
  group.bench_function("open_checked_deny_inside", |b| {
    b.iter(|| open_checked(&deny, &file, &read).unwrap())
  });
  group.finish();
}

#[cfg(not(unix))]
fn bench(_c: &mut criterion::Criterion) {}

criterion::criterion_group!(benches, bench);
criterion::criterion_main!(benches);
