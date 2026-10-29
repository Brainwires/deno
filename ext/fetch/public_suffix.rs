// Copyright 2018-2026 the Deno authors. MIT license.

//! Public suffix lookups for the cookie jar's `Domain=` check.
//!
//! The list is a pinned snapshot of the Mozilla Public Suffix List compiled
//! into the binary (`public_suffix_list.dat` next to this file). Nothing is
//! fetched at run time.
//!
//! Snapshot: <https://github.com/publicsuffix/list>, commit
//! `9dfbe81931367b3f0af21035c36ae7eeb830dd88` (2026-10-28), file
//! `public_suffix_list.dat`, SHA-256
//! `d3e6243b51695327984abb3e2a5aa7077096867f75121e031e6a5a3840ca8bcd`. The
//! file is unmodified and licensed under the MPL 2.0 (see its header).
//!
//! To update it, download the list from the canonical location
//! (<https://publicsuffix.org/list/public_suffix_list.dat>, or the raw file at
//! a given commit of the GitHub repository), replace the `.dat` file, and
//! update the commit, date and SHA-256 above and in `DENEXT.md` item 160.
//! The `public_suffix_snapshot_is_pinned` test fails until the hash here is
//! updated, so a changed list can't land unnoticed.
//!
//! Matching follows the list's format specification
//! (<https://github.com/publicsuffix/list/wiki/Format>): a rule matches when
//! its labels equal the domain's rightmost labels (`*` matching any one
//! label), an exception rule (`!`) wins over every other rule, otherwise the
//! rule with the most labels wins, and when nothing matches the prevailing
//! rule is `*`. Both the ICANN and the PRIVATE sections are used, as browsers
//! do.

use std::collections::HashSet;
use std::sync::LazyLock;

use deno_core::url::Url;

pub(crate) const PUBLIC_SUFFIX_LIST: &str =
  include_str!("./public_suffix_list.dat");

#[derive(Default)]
struct Rules {
  /// Plain rules, such as `co.uk`.
  normal: HashSet<String>,
  /// Wildcard rules without their leading `*.`, such as `ck` for `*.ck`.
  wildcard: HashSet<String>,
  /// Exception rules without their leading `!`, such as `www.ck`.
  exception: HashSet<String>,
}

/// Lowercases and converts a rule (which may hold Unicode labels) to the
/// ASCII form hosts take after URL parsing.
fn to_ascii(rule: &str) -> Option<String> {
  if rule.is_ascii() {
    return Some(rule.to_ascii_lowercase());
  }
  let url = Url::parse(&format!("http://{rule}/")).ok()?;
  url.host_str().map(|h| h.to_string())
}

static RULES: LazyLock<Rules> = LazyLock::new(|| {
  let mut rules = Rules::default();
  for line in PUBLIC_SUFFIX_LIST.lines() {
    // A rule is the first whitespace-delimited token of a line.
    let Some(rule) = line.split_whitespace().next() else {
      continue;
    };
    if rule.starts_with("//") {
      continue;
    }
    if let Some(rest) = rule.strip_prefix('!') {
      if let Some(r) = to_ascii(rest) {
        rules.exception.insert(r);
      }
    } else if let Some(rest) = rule.strip_prefix("*.") {
      if let Some(r) = to_ascii(rest) {
        rules.wildcard.insert(r);
      }
    } else if let Some(r) = to_ascii(rule) {
      rules.normal.insert(r);
    }
  }
  rules
});

/// Returns the number of labels in `domain`'s public suffix. `domain` must be
/// a canonical (lowercase, ASCII) host name with no trailing dot.
fn public_suffix_labels(domain: &str) -> usize {
  let rules = &*RULES;
  let labels: Vec<&str> = domain.split('.').collect();
  let n = labels.len();
  // Byte offset of the suffix that starts at label `i`.
  let mut offsets = Vec::with_capacity(n);
  let mut offset = 0;
  for label in &labels {
    offsets.push(offset);
    offset += label.len() + 1;
  }
  let suffix = |i: usize| &domain[offsets[i]..];

  // An exception rule prevails over every other rule; its leftmost label is
  // removed to form the public suffix.
  for i in 0..n {
    if rules.exception.contains(suffix(i)) {
      return n - i - 1;
    }
  }
  // Otherwise the matching rule with the most labels prevails.
  for i in 0..n {
    if rules.normal.contains(suffix(i))
      || (i + 1 < n && rules.wildcard.contains(suffix(i + 1)))
    {
      return n - i;
    }
  }
  // The default rule is `*`.
  1
}

/// Whether `domain` is itself a public suffix (for example `com`, `co.uk` or
/// `github.io`).
pub fn is_public_suffix(domain: &str) -> bool {
  public_suffix_labels(domain) >= domain.split('.').count()
}

/// The registrable domain of `host`: its public suffix plus one label.
/// `None` when the host is itself a public suffix.
pub fn registrable_domain(host: &str) -> Option<&str> {
  let labels = host.split('.').count();
  let keep = public_suffix_labels(host) + 1;
  if keep > labels {
    return None;
  }
  let skip = labels - keep;
  let start = host
    .split('.')
    .take(skip)
    .map(|l| l.len() + 1)
    .sum::<usize>();
  Some(&host[start..])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn public_suffix_snapshot_is_pinned() {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(PUBLIC_SUFFIX_LIST.as_bytes());
    let hex = digest
      .iter()
      .map(|b| format!("{b:02x}"))
      .collect::<String>();
    assert_eq!(
      hex, "d3e6243b51695327984abb3e2a5aa7077096867f75121e031e6a5a3840ca8bcd",
      "public_suffix_list.dat changed: update the snapshot notes in public_suffix.rs and DENEXT.md item 160"
    );
  }

  #[test]
  fn registrable_domains() {
    assert_eq!(registrable_domain("example.com"), Some("example.com"));
    assert_eq!(registrable_domain("www.example.com"), Some("example.com"));
    assert_eq!(
      registrable_domain("a.b.example.co.uk"),
      Some("example.co.uk")
    );
    assert_eq!(registrable_domain("co.uk"), None);
    assert_eq!(registrable_domain("com"), None);
    // PRIVATE section.
    assert_eq!(registrable_domain("foo.github.io"), Some("foo.github.io"));
    assert_eq!(registrable_domain("github.io"), None);
    // Wildcard (`*.ck`) and exception (`!www.ck`) rules.
    assert_eq!(registrable_domain("a.b.ck"), Some("a.b.ck"));
    assert_eq!(registrable_domain("b.ck"), None);
    assert_eq!(registrable_domain("www.ck"), Some("www.ck"));
    assert_eq!(registrable_domain("x.www.ck"), Some("www.ck"));
    // The default `*` rule for names the list doesn't know.
    assert_eq!(registrable_domain("a.b.internal"), Some("b.internal"));
    assert_eq!(registrable_domain("localhost"), None);
    // Unicode rules match their punycode form.
    assert_eq!(
      registrable_domain("example.xn--p1ai"),
      Some("example.xn--p1ai")
    );
  }

  #[test]
  fn public_suffixes() {
    assert!(is_public_suffix("com"));
    assert!(is_public_suffix("co.uk"));
    assert!(is_public_suffix("github.io"));
    assert!(!is_public_suffix("example.com"));
    assert!(!is_public_suffix("www.ck"));
  }
}
