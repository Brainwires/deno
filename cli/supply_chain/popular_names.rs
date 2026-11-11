// Copyright 2018-2026 the Deno authors. MIT license.

//! The low-download check's "did you mean" (DENEXT.md R11a): a package with
//! few downloads whose name is one or two edits away from a widely used
//! package (or, under a scope, is that package's name or close to it) is
//! named with the package it may imitate. Offline: the names are a fixed
//! list of npm packages with tens of millions of weekly downloads.

/// Widely used npm packages, sorted.
const POPULAR: &[&str] = &[
  "@babel/core",
  "@babel/preset-env",
  "@types/node",
  "@types/react",
  "acorn",
  "ajv",
  "ansi-regex",
  "ansi-styles",
  "argparse",
  "async",
  "axios",
  "babel-loader",
  "bluebird",
  "body-parser",
  "chalk",
  "cheerio",
  "chokidar",
  "classnames",
  "colors",
  "commander",
  "cookie",
  "cors",
  "cross-env",
  "cross-spawn",
  "date-fns",
  "dayjs",
  "debug",
  "dotenv",
  "electron",
  "esbuild",
  "eslint",
  "express",
  "fs-extra",
  "glob",
  "graceful-fs",
  "inquirer",
  "jest",
  "jquery",
  "js-yaml",
  "jsonwebtoken",
  "lodash",
  "minimatch",
  "minimist",
  "mkdirp",
  "moment",
  "mongoose",
  "ms",
  "nanoid",
  "next",
  "node-fetch",
  "nodemon",
  "prettier",
  "prop-types",
  "qs",
  "react",
  "react-dom",
  "redux",
  "request",
  "rimraf",
  "rxjs",
  "semver",
  "sharp",
  "socket.io",
  "supports-color",
  "tslib",
  "typescript",
  "uuid",
  "vite",
  "vue",
  "webpack",
  "ws",
  "yargs",
  "zod",
];

/// The optimal-string-alignment distance (an adjacent swap is one edit),
/// or `None` once it exceeds `max`.
fn distance(a: &str, b: &str, max: usize) -> Option<usize> {
  let a = a.as_bytes();
  let b = b.as_bytes();
  if a.len().abs_diff(b.len()) > max {
    return None;
  }
  let width = b.len() + 1;
  let mut rows = vec![0usize; (a.len() + 1) * width];
  for (j, cell) in rows.iter_mut().enumerate().take(width) {
    *cell = j;
  }
  for i in 1..=a.len() {
    rows[i * width] = i;
    for j in 1..=b.len() {
      let cost = usize::from(a[i - 1] != b[j - 1]);
      let mut best = (rows[(i - 1) * width + j] + 1)
        .min(rows[i * width + j - 1] + 1)
        .min(rows[(i - 1) * width + j - 1] + cost);
      if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
        best = best.min(rows[(i - 2) * width + j - 2] + 1);
      }
      rows[i * width + j] = best;
    }
  }
  let d = rows[a.len() * width + b.len()];
  (d <= max).then_some(d)
}

/// How many edits still read as the same name: one for a short name, two
/// otherwise.
fn max_edits(name: &str) -> usize {
  if name.len() <= 5 { 1 } else { 2 }
}

/// The widely used package `name` may imitate, if any.
pub fn did_you_mean(name: &str) -> Option<&'static str> {
  if POPULAR.binary_search(&name).is_ok() {
    return None;
  }
  let bare = name
    .strip_prefix('@')
    .and_then(|rest| rest.split_once('/'))
    .map(|(_, bare)| bare);
  let mut best: Option<(usize, &'static str)> = None;
  for popular in POPULAR {
    let mut found = distance(name, popular, max_edits(popular));
    // a scoped copy of an unscoped name (`@x/lodash`, `@x/lodahs`)
    if let Some(bare) = bare
      && !popular.starts_with('@')
      && let Some(d) = distance(bare, popular, max_edits(popular))
    {
      found = Some(found.map_or(d, |f| f.min(d)));
    }
    if let Some(d) = found
      && best.is_none_or(|(b, _)| d < b)
    {
      best = Some((d, popular));
    }
  }
  best.map(|(_, popular)| popular)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sorted_and_unique() {
    for pair in POPULAR.windows(2) {
      assert!(pair[0] < pair[1], "{} / {}", pair[0], pair[1]);
    }
  }

  #[test]
  fn suggests_near_names() {
    assert_eq!(did_you_mean("expresss"), Some("express"));
    assert_eq!(did_you_mean("lodahs"), Some("lodash"));
    assert_eq!(did_you_mean("raect"), Some("react"));
    assert_eq!(did_you_mean("@types/nodee"), Some("@types/node"));
    assert_eq!(did_you_mean("@denotest/chalkk"), Some("chalk"));
    assert_eq!(did_you_mean("@someone/lodash"), Some("lodash"));
    assert_eq!(did_you_mean("cross-envv"), Some("cross-env"));
  }

  #[test]
  fn leaves_others_alone() {
    assert_eq!(did_you_mean("express"), None);
    assert_eq!(did_you_mean("@types/node"), None);
    assert_eq!(did_you_mean("@denotest/unpopular-pkg"), None);
    assert_eq!(did_you_mean("left-pad"), None);
    // a short name needs to be closer
    assert_eq!(did_you_mean("abc"), None);
    assert_eq!(did_you_mean("wsx"), Some("ws"));
    assert_eq!(did_you_mean("my-react-widgets"), None);
  }

  #[test]
  fn osa_distance() {
    assert_eq!(distance("abc", "abc", 2), Some(0));
    assert_eq!(distance("abc", "acb", 2), Some(1));
    assert_eq!(distance("abc", "xyz", 2), None);
    assert_eq!(distance("kitten", "sitting", 3), Some(3));
  }
}
