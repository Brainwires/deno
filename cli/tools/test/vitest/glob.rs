// Copyright 2018-2026 the Deno authors. MIT license.

//! Vitest's `include` / `exclude` globs (picomatch syntax) as regexes.
//!
//! Supports `**`, `*`, `?`, character classes, brace expansion (`{a,b}`) and
//! extglobs (`?(..)`, `*(..)`, `+(..)`, `@(..)`, `!(..)`), which is what
//! Vitest's defaults (`**/*.{test,spec}.?(c|m)[jt]s?(x)`) and real-world
//! configs use. Patterns match paths relative to the project root, with `/`
//! separators.

use regex::Regex;

/// A compiled glob.
#[derive(Debug, Clone)]
pub struct Glob {
  regex: Regex,
}

impl Glob {
  pub fn new(pattern: &str) -> Result<Self, regex::Error> {
    let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
    let mut out = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    translate(&chars, &mut out);
    out.push('$');
    Ok(Self {
      regex: Regex::new(&out)?,
    })
  }

  /// Whether `path` (relative, `/`-separated) matches.
  pub fn is_match(&self, path: &str) -> bool {
    self.regex.is_match(path)
  }
}

/// Returns the index of the `)` or `}` closing the group opened at `start`.
fn find_close(chars: &[char], start: usize, open: char, close: char) -> usize {
  let mut depth = 0;
  let mut i = start;
  while i < chars.len() {
    match chars[i] {
      '\\' => i += 1,
      c if c == open => depth += 1,
      c if c == close => {
        depth -= 1;
        if depth == 0 {
          return i;
        }
      }
      _ => {}
    }
    i += 1;
  }
  chars.len()
}

/// Splits a group's contents on top-level `sep`.
fn split_top_level(chars: &[char], sep: char) -> Vec<&[char]> {
  let mut parts = Vec::new();
  let mut depth = 0i32;
  let mut start = 0;
  let mut i = 0;
  while i < chars.len() {
    match chars[i] {
      '\\' => i += 1,
      '(' | '{' | '[' => depth += 1,
      ')' | '}' | ']' => depth -= 1,
      c if c == sep && depth == 0 => {
        parts.push(&chars[start..i]);
        start = i + 1;
      }
      _ => {}
    }
    i += 1;
  }
  parts.push(&chars[start..]);
  parts
}

fn translate_alternatives(parts: Vec<&[char]>, out: &mut String) {
  out.push_str("(?:");
  for (i, part) in parts.into_iter().enumerate() {
    if i > 0 {
      out.push('|');
    }
    translate(part, out);
  }
  out.push(')');
}

fn translate(chars: &[char], out: &mut String) {
  let mut i = 0;
  while i < chars.len() {
    let c = chars[i];
    let next = chars.get(i + 1).copied();
    // extglobs: ?(..) *(..) +(..) @(..) !(..)
    if matches!(c, '?' | '*' | '+' | '@' | '!') && next == Some('(') {
      let close = find_close(chars, i + 1, '(', ')');
      let inner = &chars[(i + 2).min(chars.len())..close.min(chars.len())];
      let parts = split_top_level(inner, '|');
      if c == '!' {
        out.push_str("(?:[^/]*?)");
      } else {
        translate_alternatives(parts, out);
        match c {
          '?' => out.push('?'),
          '*' => out.push('*'),
          '+' => out.push('+'),
          _ => {}
        }
      }
      i = close + 1;
      continue;
    }
    match c {
      '\\' => {
        if let Some(n) = next {
          out.push_str(&regex::escape(&n.to_string()));
          i += 2;
          continue;
        }
      }
      '*' => {
        if next == Some('*') {
          let at_segment_start = i == 0 || chars[i - 1] == '/';
          let after = chars.get(i + 2).copied();
          if at_segment_start && after == Some('/') {
            out.push_str("(?:.*/)?");
            i += 3;
            continue;
          }
          if at_segment_start && after.is_none() {
            out.push_str(".*");
            i += 2;
            continue;
          }
          out.push_str("[^/]*");
          i += 2;
          continue;
        }
        out.push_str("[^/]*");
      }
      '?' => out.push_str("[^/]"),
      '[' => {
        let close = find_close(chars, i, '[', ']');
        if close >= chars.len() {
          out.push_str("\\[");
        } else {
          out.push('[');
          let mut j = i + 1;
          if matches!(chars.get(j), Some('!') | Some('^')) {
            out.push('^');
            j += 1;
          }
          while j < close {
            let ch = chars[j];
            if ch == '\\' || ch == '[' {
              out.push('\\');
            }
            out.push(ch);
            j += 1;
          }
          out.push(']');
          i = close + 1;
          continue;
        }
      }
      '{' => {
        let close = find_close(chars, i, '{', '}');
        if close >= chars.len() {
          out.push_str("\\{");
        } else {
          let inner = &chars[i + 1..close];
          translate_alternatives(split_top_level(inner, ','), out);
          i = close + 1;
          continue;
        }
      }
      _ => out.push_str(&regex::escape(&c.to_string())),
    }
    i += 1;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn matches(pattern: &str, path: &str) -> bool {
    Glob::new(pattern).unwrap().is_match(path)
  }

  #[test]
  fn vitest_default_include() {
    let p = "**/*.{test,spec}.?(c|m)[jt]s?(x)";
    assert!(matches(p, "a.test.ts"));
    assert!(matches(p, "src/a/b.spec.tsx"));
    assert!(matches(p, "src/b.test.mjs"));
    assert!(matches(p, "src/b.test.cts"));
    assert!(matches(p, "src/b.spec.jsx"));
    assert!(!matches(p, "src/b.ts"));
    assert!(!matches(p, "src/b.test.tsxx"));
    assert!(!matches(p, "src/b.test.xts"));
  }

  #[test]
  fn vitest_default_exclude() {
    assert!(matches("**/node_modules/**", "node_modules/a/b.test.ts"));
    assert!(matches("**/node_modules/**", "x/node_modules/a.test.ts"));
    assert!(!matches("**/node_modules/**", "src/a.test.ts"));
    assert!(matches("**/.git/**", ".git/x"));
  }

  #[test]
  fn star_and_braces() {
    assert!(matches("src/**/*.test.{ts,tsx}", "src/a.test.ts"));
    assert!(matches("src/**/*.test.{ts,tsx}", "src/x/y/a.test.tsx"));
    assert!(!matches("src/**/*.test.{ts,tsx}", "lib/a.test.ts"));
    assert!(matches("./src/*.ts", "src/a.ts"));
    assert!(!matches("src/*.ts", "src/x/a.ts"));
    assert!(matches("src/a?.ts", "src/ab.ts"));
    assert!(matches("{a,b/{c,d}}.ts", "b/d.ts"));
  }

  #[test]
  fn escapes_regex_metacharacters() {
    assert!(matches("a+b(1).ts", "a+b(1).ts"));
    assert!(matches("a.ts", "a.ts"));
    assert!(!matches("a.ts", "abts"));
  }
}
