// Copyright 2018-2026 the Deno authors. MIT license.

//! Validation for bundle-time `define` substitutions (`deno bundle --define`,
//! `Deno.bundle({ define })`), following esbuild's rules so that a bad define
//! is reported with a clear message before esbuild starts.
//!
//! - The name is an identifier (not a reserved word; `this` and `import.meta`
//!   are allowed) followed by any number of `.name` or `["string"]` member
//!   accesses, for example `DEBUG`, `process.env.NODE_ENV` or
//!   `process.env["MY-VAR"]`.
//! - The value is either an entity name (identifiers joined by `.`, such as
//!   `undefined` or `globalThis.fetch`) or a JSON literal, which esbuild reads
//!   leniently: single-quoted strings, trailing commas, comments and integer
//!   `BigInt`s (`10n`) are accepted.

/// Reserved words that can't start a define name. `this` and `import` are
/// handled separately (`this` is allowed, `import` only as `import.meta`).
const RESERVED_WORDS: &[&str] = &[
  "break",
  "case",
  "catch",
  "class",
  "const",
  "continue",
  "debugger",
  "default",
  "delete",
  "do",
  "else",
  "enum",
  "export",
  "extends",
  "false",
  "finally",
  "for",
  "function",
  "if",
  "in",
  "instanceof",
  "new",
  "null",
  "return",
  "super",
  "switch",
  "throw",
  "true",
  "try",
  "typeof",
  "var",
  "void",
  "while",
  "with",
];

/// Splits a `KEY=VALUE` define at the first `=`.
pub fn parse_define(input: &str) -> Result<(String, String), String> {
  match input.split_once('=') {
    Some((key, value)) => Ok((key.to_string(), value.to_string())),
    None => Err(format!(
      "Invalid define \"{input}\": expected KEY=VALUE, for example --define DEBUG=false or --define 'process.env.NODE_ENV=\"production\"'"
    )),
  }
}

/// Validates one define's name and value the way esbuild does.
pub fn validate_define(key: &str, value: &str) -> Result<(), String> {
  validate_define_name(key).map_err(|reason| {
    format!(
      "Invalid define name \"{key}\": {reason}. A define name is an identifier or a member expression, for example DEBUG, process.env.NODE_ENV or process.env[\"MY-VAR\"]"
    )
  })?;
  if value.trim().is_empty() {
    return Err(format!(
      "Invalid define value for \"{key}\": the value is empty. To substitute a string, quote it as a JSON string, for example --define '{key}=\"text\"'"
    ));
  }
  if !is_entity_name(value) && !is_define_literal(value) {
    return Err(format!(
      "Invalid define value for \"{key}\": {value} is not an entity name or a JS literal. Use an identifier or a member expression (such as undefined or globalThis.fetch), or a JSON value (true, 42, \"text\", {{\"a\": 1}}); to substitute a string, quote it, for example --define '{key}=\"{value}\"'"
    ));
  }
  Ok(())
}

/// Validates every define, returning the first error.
pub fn validate_defines<'a>(
  defines: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), String> {
  for (key, value) in defines {
    validate_define(key, value)?;
  }
  Ok(())
}

fn is_id_start(c: char) -> bool {
  c == '$'
    || c == '_'
    || c.is_ascii_alphabetic()
    || (!c.is_ascii() && c.is_alphabetic())
}

fn is_id_continue(c: char) -> bool {
  is_id_start(c)
    || c.is_ascii_digit()
    || (!c.is_ascii() && c.is_alphanumeric())
    || c == '\u{200c}'
    || c == '\u{200d}'
}

/// `IsIdentifier`: a whole string that is one identifier (no escapes).
fn is_identifier(s: &str) -> bool {
  let mut chars = s.chars();
  match chars.next() {
    Some(c) if is_id_start(c) => chars.all(is_id_continue),
    _ => false,
  }
}

/// An entity name: identifiers joined by `.`, with no whitespace.
fn is_entity_name(s: &str) -> bool {
  s.split('.').all(is_identifier)
}

struct Cursor<'a> {
  chars: std::iter::Peekable<std::str::CharIndices<'a>>,
  src: &'a str,
}

impl<'a> Cursor<'a> {
  fn new(src: &'a str) -> Self {
    Self {
      chars: src.char_indices().peekable(),
      src,
    }
  }

  fn peek(&mut self) -> Option<char> {
    self.chars.peek().map(|(_, c)| *c)
  }

  fn pos(&mut self) -> usize {
    self.chars.peek().map(|(i, _)| *i).unwrap_or(self.src.len())
  }

  fn bump(&mut self) -> Option<char> {
    self.chars.next().map(|(_, c)| c)
  }

  fn eat(&mut self, c: char) -> bool {
    if self.peek() == Some(c) {
      self.bump();
      true
    } else {
      false
    }
  }

  fn skip_whitespace(&mut self) {
    while matches!(self.peek(), Some(c) if c.is_whitespace()) {
      self.bump();
    }
  }

  /// Whitespace and comments, as esbuild's JSON lexer skips them.
  fn skip_trivia(&mut self) -> bool {
    loop {
      self.skip_whitespace();
      let rest = &self.src[self.pos()..];
      if rest.starts_with("//") {
        while !matches!(self.peek(), None | Some('\n' | '\r')) {
          self.bump();
        }
      } else if rest.starts_with("/*") {
        self.bump();
        self.bump();
        loop {
          match self.bump() {
            None => return false,
            Some('*') if self.eat('/') => break,
            Some(_) => {}
          }
        }
      } else {
        return true;
      }
    }
  }

  /// An identifier name, allowing `\uXXXX` / `\u{X}` escapes as the JS lexer
  /// does. Returns `None` when there is none at the cursor.
  fn identifier_name(&mut self) -> Option<String> {
    let mut name = String::new();
    loop {
      let c = match self.peek() {
        Some('\\') => {
          self.bump();
          if !self.eat('u') {
            return None;
          }
          let code = if self.eat('{') {
            let mut hex = String::new();
            while let Some(c) = self.peek().filter(|c| *c != '}') {
              hex.push(c);
              self.bump();
            }
            if !self.eat('}') {
              return None;
            }
            u32::from_str_radix(&hex, 16).ok()?
          } else {
            let mut hex = String::new();
            for _ in 0..4 {
              hex.push(self.bump()?);
            }
            u32::from_str_radix(&hex, 16).ok()?
          };
          char::from_u32(code)?
        }
        Some(c) => {
          self.bump();
          c
        }
        None => break,
      };
      let ok = if name.is_empty() {
        is_id_start(c)
      } else {
        is_id_continue(c)
      };
      if !ok {
        return None;
      }
      name.push(c);
      if !matches!(self.peek(), Some(c) if c == '\\' || is_id_continue(c)) {
        break;
      }
    }
    if name.is_empty() { None } else { Some(name) }
  }

  /// A quoted string, double or single quoted. A define value's string takes
  /// JSON's escapes only (esbuild reads it as JSON); a name's `["..."]` is a
  /// JavaScript string, so any escape goes there (`lenient`).
  fn string(&mut self, lenient: bool) -> bool {
    let quote = match self.peek() {
      Some(q @ ('"' | '\'')) => q,
      _ => return false,
    };
    self.bump();
    loop {
      match self.bump() {
        None | Some('\n' | '\r') => return false,
        Some(c) if c == quote => return true,
        Some('\\') => match self.bump() {
          Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => {}
          Some(_) if lenient => {}
          Some('u') => {
            for _ in 0..4 {
              if !matches!(self.bump(), Some(c) if c.is_ascii_hexdigit()) {
                return false;
              }
            }
          }
          _ => return false,
        },
        Some(_) => {}
      }
    }
  }

  fn digits(&mut self) -> usize {
    let mut n = 0;
    while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
      self.bump();
      n += 1;
    }
    n
  }

  /// A JSON number, or a non-negative integer `BigInt` (`10n`).
  fn number(&mut self) -> bool {
    let negative = self.eat('-');
    let int_start = self.pos();
    if self.digits() == 0 {
      return false;
    }
    let int = &self.src[int_start..self.pos()];
    if int.len() > 1 && int.starts_with('0') {
      return false;
    }
    if !negative && self.eat('n') {
      return true;
    }
    if self.eat('.') && self.digits() == 0 {
      return false;
    }
    if self.eat('e') || self.eat('E') {
      if !self.eat('+') {
        self.eat('-');
      }
      if self.digits() == 0 {
        return false;
      }
    }
    !matches!(self.peek(), Some(c) if is_id_continue(c) || c == '.')
  }

  fn keyword(&mut self, word: &str) -> bool {
    if !self.src[self.pos()..].starts_with(word) {
      return false;
    }
    for _ in word.chars() {
      self.bump();
    }
    !matches!(self.peek(), Some(c) if is_id_continue(c))
  }

  fn json_value(&mut self, depth: usize) -> bool {
    if depth > 128 || !self.skip_trivia() {
      return false;
    }
    let ok = match self.peek() {
      Some('"' | '\'') => self.string(false),
      Some('-' | '0'..='9') => self.number(),
      Some('t') => self.keyword("true"),
      Some('f') => self.keyword("false"),
      Some('n') => self.keyword("null"),
      Some('[') => {
        self.bump();
        self.json_list(']', depth, |cursor, depth| cursor.json_value(depth))
      }
      Some('{') => {
        self.bump();
        self.json_list('}', depth, |cursor, depth| {
          cursor.skip_trivia()
            && cursor.string(false)
            && cursor.skip_trivia()
            && cursor.eat(':')
            && cursor.json_value(depth)
        })
      }
      _ => false,
    };
    ok && self.skip_trivia()
  }

  /// The rest of an array or object after its opening bracket: items
  /// separated by commas, a trailing comma allowed.
  fn json_list(
    &mut self,
    close: char,
    depth: usize,
    item: impl Fn(&mut Self, usize) -> bool,
  ) -> bool {
    loop {
      if !self.skip_trivia() {
        return false;
      }
      if self.eat(close) {
        return true;
      }
      if !item(self, depth + 1) || !self.skip_trivia() {
        return false;
      }
      if self.eat(close) {
        return true;
      }
      if !self.eat(',') {
        return false;
      }
    }
  }
}

/// A JSON literal as esbuild reads a define value.
fn is_define_literal(value: &str) -> bool {
  let mut cursor = Cursor::new(value);
  cursor.json_value(0) && cursor.peek().is_none()
}

/// `name(.name | ["string"])*`, whitespace allowed between tokens.
fn validate_define_name(key: &str) -> Result<(), String> {
  let mut cursor = Cursor::new(key);
  cursor.skip_whitespace();
  if cursor.peek().is_none() {
    return Err("the name is empty".to_string());
  }
  let first = cursor
    .identifier_name()
    .ok_or_else(|| "it must start with an identifier".to_string())?;
  if first == "import" {
    cursor.skip_whitespace();
    let is_meta = cursor.eat('.') && {
      cursor.skip_whitespace();
      cursor.identifier_name().as_deref() == Some("meta")
    };
    if !is_meta {
      return Err("`import` is only allowed as `import.meta`".to_string());
    }
  } else if RESERVED_WORDS.contains(&first.as_str()) {
    return Err(format!("`{first}` is a reserved word"));
  }
  loop {
    cursor.skip_whitespace();
    match cursor.peek() {
      None => return Ok(()),
      Some('.') => {
        cursor.bump();
        cursor.skip_whitespace();
        if cursor.identifier_name().is_none() {
          return Err("expected a property name after `.`".to_string());
        }
      }
      Some('[') => {
        cursor.bump();
        cursor.skip_whitespace();
        if !cursor.string(true) {
          return Err(
            "expected a quoted string inside `[...]`, such as [\"name\"]"
              .to_string(),
          );
        }
        cursor.skip_whitespace();
        if !cursor.eat(']') {
          return Err("expected `]`".to_string());
        }
      }
      Some(c) => {
        return Err(format!("unexpected `{c}`; expected `.` or `[`"));
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_define_splits_at_the_first_equals() {
    assert_eq!(
      parse_define("A=1").unwrap(),
      ("A".to_string(), "1".to_string())
    );
    assert_eq!(
      parse_define("A=b=c").unwrap(),
      ("A".to_string(), "b=c".to_string())
    );
    assert_eq!(
      parse_define("A=").unwrap(),
      ("A".to_string(), String::new())
    );
    let err = parse_define("DEBUG").unwrap_err();
    assert!(err.contains("expected KEY=VALUE"), "{err}");
  }

  // Each case was checked against esbuild 0.25.5's `--define`.
  #[test]
  fn names_esbuild_accepts() {
    for key in [
      "A",
      "$",
      "_a$",
      "DEBUG",
      "process.env.NODE_ENV",
      "a.b.c",
      "a[\"b-c\"]",
      "a['b']",
      "a[ \"b\" ]",
      "a[\"b\"].c",
      "a.b[\"c\"]",
      "process.env[\"NODE-ENV\"]",
      "a[\"b\\\"c\"]",
      "a[\"b\\x41\"]",
      "a['b\\'c']",
      "import.meta",
      "import.meta.env",
      "this",
      "this.x",
      "undefined",
      "let",
      "yield",
      "await",
      "static",
      "eval",
      "a.class",
      "A.if",
      " a",
      "a ",
      "a\\u0062",
      "é",
    ] {
      assert!(
        validate_define(key, "1").is_ok(),
        "{key}: {:?}",
        validate_define(key, "1")
      );
    }
  }

  #[test]
  fn names_esbuild_rejects() {
    for key in [
      "",
      " ",
      "a-b",
      "1a",
      "a..b",
      "a.",
      "a[0]",
      "a[b]",
      "a?.b",
      "a[\"b\"]x",
      "if",
      "class",
      "null",
      "true",
      "new.target",
      "super",
      "enum",
      "import",
      "import.foo",
    ] {
      let err = validate_define(key, "1").unwrap_err();
      assert!(err.starts_with("Invalid define name"), "{key}: {err}");
    }
  }

  #[test]
  fn values_esbuild_accepts() {
    for value in [
      "1",
      " 1",
      "1 ",
      "-1",
      "-0",
      "0.5",
      "1.0",
      "1e3",
      "1E3",
      "1e+5",
      "1e-5",
      "0n",
      "1n",
      "true",
      "false",
      "null",
      "undefined",
      "NaN",
      "Infinity",
      "true1",
      "foo.bar",
      "a.if",
      "import.meta",
      "this.x",
      "é",
      "\"x\"",
      " \"x\"",
      "'x'",
      "'a\"b'",
      "\"a\\nb\"",
      "\"\\u0041\"",
      "{\"a\":1}",
      "{\"a\":1,}",
      "{'a':1}",
      "{}",
      "[]",
      "[1,2]",
      "[1,]",
      "[\"a\",]",
      "[true,null,\"x\",1.5,-2]",
      "{\"a\":[1,{\"b\":null}]}",
      "/*c*/1",
      "1/*x*/",
      "1//c",
    ] {
      assert!(
        validate_define("A", value).is_ok(),
        "{value}: {:?}",
        validate_define("A", value)
      );
    }
  }

  #[test]
  fn values_esbuild_rejects() {
    for value in [
      "foo-bar",
      "0x10",
      "foo()",
      ".5",
      "1.",
      "+1",
      "`x`",
      "{a:1}",
      "{1:2}",
      "{\"a\":foo}",
      "-Infinity",
      "-NaN",
      "-a",
      "-\"x\"",
      "01",
      "00n",
      "-1n",
      "1.5n",
      "1_000",
      "0b1",
      "1e",
      "-",
      "\"unterminated",
      "\"\\x41\"",
      "'a\\'b'",
      "[1 2]",
      "[,]",
      "[undefined]",
      "1 2",
      "\"a\" \"b\"",
      "{\"a\":1}x",
      "x y",
      "a. b",
      "a .b",
      " a.b ",
      "a[\"b\"]",
      "a[0]",
      "// x",
      "/* x",
    ] {
      let err = validate_define("A", value).unwrap_err();
      assert!(err.starts_with("Invalid define value"), "{value}: {err}");
    }
  }

  #[test]
  fn empty_value_is_rejected_with_a_hint() {
    for value in ["", "  "] {
      let err = validate_define("A", value).unwrap_err();
      assert!(err.contains("the value is empty"), "{err}");
    }
  }

  #[test]
  fn validate_defines_reports_the_first_error() {
    assert!(validate_defines([("A", "1"), ("B", "\"x\"")]).is_ok());
    let err = validate_defines([("A", "1"), ("b-c", "1")]).unwrap_err();
    assert!(err.contains("\"b-c\""), "{err}");
  }

  #[test]
  fn deeply_nested_literals_are_rejected_not_overflowed() {
    let value = "[".repeat(10_000);
    assert!(validate_define("A", &value).is_err());
  }
}
