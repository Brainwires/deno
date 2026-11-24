// Copyright 2018-2026 the Deno authors. MIT license.

//! denext (DENEXT.md item 5NN): `DENO_NARROW_PERMISSIONS`, permission flags
//! that can only narrow a compiled program's baked grant.
//!
//! A `deno compile`d program runs with the permissions baked into it; the
//! person who runs it had no way to hold it to less. The variable takes
//! permission flags in the CLI's own syntax (`--allow-read=/data
//! --deny-net=10.0.0.0`, whitespace-separated, `"…"` / `'…'` quoting a token
//! with spaces, `,,` a literal comma in a path list) and describes a grant:
//!
//! - `--allow-<kind>=<list>` holds that kind to the list: the program keeps
//!   the part of its baked grant the list covers (the intersection, as the
//!   kernel sandbox's inherited layers do — `kernel_inherit::clamp`). A bare
//!   `--allow-<kind>` leaves the kind as baked; a kind not named is left as
//!   baked. `--allow-net` holds both directions, `--allow-net-connect` /
//!   `--allow-net-listen` one each (several flags of one kind add up, and a
//!   bare one beside a listed one adds nothing, as on the command line).
//! - `--deny-<kind>[=<list>]` adds denies (a bare one denies the whole kind).
//! - `--no-prompt` is accepted; prompting is always off under a narrowing
//!   (a prompt could grant back what the narrowing removed).
//!
//! Nothing it says can widen the baked grant: an allow list is intersected,
//! a deny only adds. `--allow-all` / `-A`, `--ignore-*`, `--allow-import` /
//! `--deny-import` (a compiled program loads no remote modules), any other
//! flag, an unterminated quote or an empty list entry is refused, and the
//! program does not start. The kinds: `read`, `write`, `ffi`, `net`,
//! `net-connect`, `net-listen`, `env`, `run`, `sys`, `midi`, and the short
//! `-R`, `-W`, `-N`, `-E`, `-S`.

use std::path::Path;

use deno_runtime::deno_permissions::PermissionsOptions;

use crate::kernel_inherit;

/// The variable a compiled program reads its narrowing from.
pub const NARROW_PERMISSIONS_ENV: &str = "DENO_NARROW_PERMISSIONS";

/// A parsed narrowing. Each allow: `None` not named, an empty list bare
/// (unchanged), else the list; each deny as on the command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Narrowing {
  pub allow_read: Option<Vec<String>>,
  pub allow_write: Option<Vec<String>>,
  pub allow_ffi: Option<Vec<String>>,
  pub allow_net: Option<Vec<String>>,
  pub allow_net_connect: Option<Vec<String>>,
  pub allow_net_listen: Option<Vec<String>>,
  pub allow_env: Option<Vec<String>>,
  pub allow_run: Option<Vec<String>>,
  pub allow_sys: Option<Vec<String>>,
  pub allow_midi: Option<Vec<String>>,
  pub deny_read: Option<Vec<String>>,
  pub deny_write: Option<Vec<String>>,
  pub deny_ffi: Option<Vec<String>>,
  pub deny_net: Option<Vec<String>>,
  pub deny_net_connect: Option<Vec<String>>,
  pub deny_net_listen: Option<Vec<String>>,
  pub deny_env: Option<Vec<String>>,
  pub deny_run: Option<Vec<String>>,
  pub deny_sys: Option<Vec<String>>,
  pub deny_midi: Option<Vec<String>>,
}

/// The value split into tokens: whitespace separates, `"…"` and `'…'`
/// group (the quotes are dropped).
fn tokens(value: &str) -> Result<Vec<String>, String> {
  let mut out = Vec::new();
  let mut current = String::new();
  let mut in_token = false;
  let mut quote: Option<char> = None;
  for c in value.chars() {
    match quote {
      Some(q) if c == q => quote = None,
      Some(_) => current.push(c),
      None if c == '"' || c == '\'' => {
        quote = Some(c);
        in_token = true;
      }
      None if c.is_whitespace() => {
        if in_token {
          out.push(std::mem::take(&mut current));
          in_token = false;
        }
      }
      None => {
        current.push(c);
        in_token = true;
      }
    }
  }
  if quote.is_some() {
    return Err("unterminated quote".to_string());
  }
  if in_token {
    out.push(current);
  }
  Ok(out)
}

/// A path list: comma-separated, `,,` a literal comma (the CLI's
/// `escape_and_split_commas`).
fn split_paths(s: &str) -> Result<Vec<String>, String> {
  let mut out = Vec::new();
  let mut current = String::new();
  let mut chars = s.chars().peekable();
  while let Some(c) = chars.next() {
    if c == ',' {
      if chars.peek() == Some(&',') {
        chars.next();
        current.push(',');
        continue;
      }
      if current.is_empty() {
        return Err("empty values are not allowed".to_string());
      }
      out.push(std::mem::take(&mut current));
    } else {
      current.push(c);
    }
  }
  if current.is_empty() {
    return Err("empty values are not allowed".to_string());
  }
  out.push(current);
  Ok(out)
}

/// A plain comma-separated list.
fn split_names(s: &str) -> Result<Vec<String>, String> {
  s.split(',')
    .map(|e| {
      if e.is_empty() {
        Err("empty values are not allowed".to_string())
      } else {
        Ok(e.to_string())
      }
    })
    .collect()
}

/// A network list with bare ports expanded as the CLI does (`:8000` is
/// `0.0.0.0:8000`, `127.0.0.1:8000` and `localhost:8000`).
fn split_net(s: &str) -> Result<Vec<String>, String> {
  let mut out = Vec::new();
  for entry in split_names(s)? {
    if let Some(port) = entry.strip_prefix(':')
      && port.parse::<u16>().is_ok()
    {
      for host in ["0.0.0.0", "127.0.0.1", "localhost"] {
        out.push(format!("{host}:{port}"));
      }
      continue;
    }
    out.push(entry);
  }
  Ok(out)
}

/// Adds a flag's values to `slot`, as the CLI does: a bare flag grants the
/// whole kind only when no occurrence of it lists values.
fn add(slot: &mut Option<Vec<String>>, values: Option<Vec<String>>) {
  match values {
    None => {
      slot.get_or_insert_with(Vec::new);
    }
    Some(values) => slot.get_or_insert_with(Vec::new).extend(values),
  }
}

/// Parses the variable's value.
pub fn parse(value: &str) -> Result<Narrowing, String> {
  let mut n = Narrowing::default();
  for token in tokens(value)? {
    let (flag, value) = match token.split_once('=') {
      Some((f, v)) => (f.to_string(), Some(v.to_string())),
      None => (token.clone(), None),
    };
    let flag = match flag.as_str() {
      "-R" => "--allow-read",
      "-W" => "--allow-write",
      "-N" => "--allow-net",
      "-E" => "--allow-env",
      "-S" => "--allow-sys",
      other => other,
    };
    if flag == "--no-prompt" && value.is_none() {
      continue;
    }
    let refuse = |why: &str| format!("{token}: {why}");
    let list = |split: fn(&str) -> Result<Vec<String>, String>| {
      value
        .as_deref()
        .map(split)
        .transpose()
        .map_err(|e| refuse(&e))
    };
    let (slot, values) = match flag {
      "--allow-read" => (&mut n.allow_read, list(split_paths)?),
      "--allow-write" => (&mut n.allow_write, list(split_paths)?),
      "--allow-ffi" => (&mut n.allow_ffi, list(split_paths)?),
      "--allow-net" => (&mut n.allow_net, list(split_net)?),
      "--allow-net-connect" => (&mut n.allow_net_connect, list(split_net)?),
      "--allow-net-listen" => (&mut n.allow_net_listen, list(split_net)?),
      "--allow-env" => (&mut n.allow_env, list(split_names)?),
      "--allow-run" => (&mut n.allow_run, list(split_names)?),
      "--allow-sys" => (&mut n.allow_sys, list(split_names)?),
      "--allow-midi" => (&mut n.allow_midi, list(split_names)?),
      "--deny-read" => (&mut n.deny_read, list(split_paths)?),
      "--deny-write" => (&mut n.deny_write, list(split_paths)?),
      "--deny-ffi" => (&mut n.deny_ffi, list(split_paths)?),
      "--deny-net" => (&mut n.deny_net, list(split_net)?),
      "--deny-net-connect" => (&mut n.deny_net_connect, list(split_net)?),
      "--deny-net-listen" => (&mut n.deny_net_listen, list(split_net)?),
      "--deny-env" => (&mut n.deny_env, list(split_names)?),
      "--deny-run" => (&mut n.deny_run, list(split_names)?),
      "--deny-sys" => (&mut n.deny_sys, list(split_names)?),
      "--deny-midi" => (&mut n.deny_midi, list(split_names)?),
      "-A" | "--allow-all" => {
        return Err(refuse(
          "it would grant everything; this variable can only narrow the \
           baked permissions",
        ));
      }
      "--allow-import" | "--deny-import" | "-I" => {
        return Err(refuse("a compiled program loads no remote modules"));
      }
      f if f.starts_with("--ignore-") => {
        return Err(refuse("not supported here"));
      }
      _ => {
        return Err(refuse(
          "not a permission flag (expected --allow-<kind>[=<list>] or \
           --deny-<kind>[=<list>])",
        ));
      }
    };
    add(slot, values);
  }
  Ok(n)
}

/// `a` and `b` together as one grant: not named (both `None`) is
/// everything, a bare flag is everything.
fn union(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Vec<String> {
  if a.as_ref().is_some_and(|l| l.is_empty())
    || b.as_ref().is_some_and(|l| l.is_empty())
  {
    return Vec::new();
  }
  a.iter().chain(b.iter()).flatten().cloned().collect()
}

/// Adds `extra` denies to `list` (a bare deny denies the whole kind).
fn add_denies(list: &mut Option<Vec<String>>, extra: &Option<Vec<String>>) {
  let Some(extra) = extra else {
    return;
  };
  if extra.is_empty() {
    *list = Some(Vec::new());
    return;
  }
  match list {
    Some(l) if l.is_empty() => {}
    Some(l) => {
      for e in extra {
        if !l.contains(e) {
          l.push(e.clone());
        }
      }
    }
    None => *list = Some(extra.clone()),
  }
}

/// Holds `options` (a baked grant) to `n`: allows intersected, denies
/// added, prompting off. Relative entries resolve against `cwd`.
pub fn apply(options: &mut PermissionsOptions, n: &Narrowing, cwd: &Path) {
  let all = |l: &Option<Vec<String>>| Some(l.clone().unwrap_or_default());
  // the narrowing as a grant, a kind it doesn't name granted entirely
  let grant = PermissionsOptions {
    allow_read: all(&n.allow_read),
    allow_write: all(&n.allow_write),
    allow_ffi: all(&n.allow_ffi),
    allow_net: None,
    allow_net_connect: Some(union(&n.allow_net, &n.allow_net_connect)),
    allow_net_listen: Some(union(&n.allow_net, &n.allow_net_listen)),
    allow_env: all(&n.allow_env),
    allow_run: all(&n.allow_run),
    allow_sys: all(&n.allow_sys),
    allow_midi: all(&n.allow_midi),
    ..Default::default()
  };
  let layer = kernel_inherit::layer_for(&grant, cwd, None);
  kernel_inherit::clamp(options, &[layer], cwd);
  add_denies(&mut options.deny_read, &n.deny_read);
  add_denies(&mut options.deny_write, &n.deny_write);
  add_denies(&mut options.deny_ffi, &n.deny_ffi);
  add_denies(&mut options.deny_net, &n.deny_net);
  add_denies(&mut options.deny_net_connect, &n.deny_net_connect);
  add_denies(&mut options.deny_net_listen, &n.deny_net_listen);
  add_denies(&mut options.deny_env, &n.deny_env);
  add_denies(&mut options.deny_run, &n.deny_run);
  add_denies(&mut options.deny_sys, &n.deny_sys);
  add_denies(&mut options.deny_midi, &n.deny_midi);
  options.prompt = false;
}

/// Applies [`NARROW_PERMISSIONS_ENV`] to a compiled program's baked
/// `options`, when it is set and not blank. `Err` is the message to refuse
/// to start with.
#[allow(
  clippy::disallowed_methods,
  reason = "a compiled program's own environment and cwd, at startup"
)]
pub fn apply_from_env(options: &mut PermissionsOptions) -> Result<(), String> {
  let Some(raw) = std::env::var_os(NARROW_PERMISSIONS_ENV) else {
    return Ok(());
  };
  let fail = |why: &str| {
    format!(
      "{NARROW_PERMISSIONS_ENV}: {why}. It holds permission flags that can \
       only narrow this program's baked permissions, such as \
       \"--allow-read=./data --deny-net\"."
    )
  };
  let value = raw.to_str().ok_or_else(|| fail("not valid UTF-8"))?;
  if value.trim().is_empty() {
    return Ok(());
  }
  let n = parse(value).map_err(|e| fail(&e))?;
  let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
  apply(options, &n, &cwd);
  Ok(())
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;

  fn s(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|x| x.to_string()).collect())
  }

  #[allow(clippy::disallowed_methods, reason = "a real folder for the tests")]
  fn tmp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "deno_narrow_{}_{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("data/sub")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    deno_path_util::strip_unc_prefix(dir.canonicalize().unwrap())
  }

  fn baked_all() -> PermissionsOptions {
    PermissionsOptions {
      allow_read: Some(vec![]),
      allow_write: Some(vec![]),
      allow_net: Some(vec![]),
      allow_env: Some(vec![]),
      allow_run: Some(vec![]),
      allow_sys: Some(vec![]),
      allow_ffi: Some(vec![]),
      prompt: true,
      ..Default::default()
    }
  }

  #[test]
  fn parses_the_cli_syntax() {
    let n = parse(
      "--allow-read=a,b,,c -R=d --deny-net  --allow-net=:8000 \
       \"--allow-write=with space\" --allow-env --no-prompt -N=x.com",
    )
    .unwrap();
    assert_eq!(n.allow_read, s(&["a", "b,c", "d"]));
    assert_eq!(n.deny_net, s(&[]));
    assert_eq!(
      n.allow_net,
      s(&["0.0.0.0:8000", "127.0.0.1:8000", "localhost:8000", "x.com"])
    );
    assert_eq!(n.allow_write, s(&["with space"]));
    assert_eq!(n.allow_env, s(&[]));
    assert_eq!(
      parse("--allow-env --allow-env=HOME").unwrap().allow_env,
      s(&["HOME"])
    );
    assert_eq!(parse("  ").unwrap(), Narrowing::default());
  }

  #[test]
  fn refuses_what_could_widen_or_is_malformed() {
    for bad in [
      "-A",
      "--allow-all",
      "--allow-import=x.com",
      "--deny-import",
      "--ignore-read=a",
      "--allow-hrtime",
      "--kernel-sandbox=off",
      "--allow-read=",
      "--allow-read=a,",
      "--allow-env=A,,B",
      "--allow-read=\"open",
      "read",
    ] {
      assert!(parse(bad).is_err(), "{bad}");
    }
    let err = parse("--allow-all").unwrap_err();
    assert!(err.contains("can only narrow"), "{err}");
  }

  #[test]
  fn allow_lists_intersect_with_the_baked_grant() {
    let root = tmp();
    let data = root.join("data");
    let mut o = baked_all();
    let n = parse(&format!(
      "--allow-read={} --allow-env=HOME,LANG --allow-net=deno.land \
       --allow-run=git",
      data.display()
    ))
    .unwrap();
    apply(&mut o, &n, &root);
    assert_eq!(o.allow_read, s(&[&data.to_string_lossy()]));
    assert_eq!(o.allow_env, s(&["HOME", "LANG"]));
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, s(&["deno.land"]));
    assert_eq!(o.allow_net_listen, s(&["deno.land"]));
    assert_eq!(o.allow_run, s(&["git"]));
    // kinds it doesn't name stay as baked
    assert_eq!(o.allow_write, Some(vec![]));
    assert_eq!(o.allow_sys, Some(vec![]));
    assert!(!o.prompt);
  }

  #[test]
  fn never_widens() {
    let root = tmp();
    let data = root.join("data");
    let sub = data.join("sub");
    let other = root.join("other");
    let mut o = PermissionsOptions {
      allow_read: s(&[&sub.to_string_lossy()]),
      allow_env: s(&["HOME"]),
      allow_net: None,
      allow_run: None,
      prompt: true,
      ..Default::default()
    };
    let n = parse(&format!(
      "--allow-read={},{} --allow-env=HOME,SECRET --allow-net --allow-run=sh \
       --allow-write=/",
      data.display(),
      other.display()
    ))
    .unwrap();
    apply(&mut o, &n, &root);
    // a wider list keeps the narrower baked entry; nothing outside it
    assert_eq!(o.allow_read, s(&[&sub.to_string_lossy()]));
    assert_eq!(o.allow_env, s(&["HOME"]));
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, None);
    assert_eq!(o.allow_net_listen, None);
    assert_eq!(o.allow_run, None);
    assert_eq!(o.allow_write, None);
    assert!(!o.prompt);
  }

  #[test]
  fn a_disjoint_list_removes_the_kind() {
    let root = tmp();
    let mut o = PermissionsOptions {
      allow_read: s(&[&root.join("data").to_string_lossy()]),
      ..Default::default()
    };
    let n =
      parse(&format!("--allow-read={}", root.join("other").display())).unwrap();
    apply(&mut o, &n, &root);
    assert_eq!(o.allow_read, None);
  }

  #[test]
  fn directions_narrow_on_their_own() {
    let root = tmp();
    let mut o = baked_all();
    let n = parse("--allow-net-connect=api.example.com:443").unwrap();
    apply(&mut o, &n, &root);
    assert_eq!(o.allow_net, None);
    assert_eq!(o.allow_net_connect, s(&["api.example.com:443"]));
    assert_eq!(o.allow_net_listen, Some(vec![]));
  }

  #[test]
  fn denies_only_add() {
    let root = tmp();
    let mut o = PermissionsOptions {
      deny_env: s(&["A"]),
      deny_net: Some(vec![]),
      ..baked_all()
    };
    let n = parse("--deny-env=B,A --deny-net=x.com --deny-write --deny-read=k")
      .unwrap();
    apply(&mut o, &n, &root);
    assert_eq!(o.deny_env, s(&["A", "B"]));
    assert_eq!(o.deny_net, Some(vec![])); // already denied entirely
    assert_eq!(o.deny_write, Some(vec![]));
    assert_eq!(o.deny_read, s(&["k"]));
    // the allows it doesn't name are untouched
    assert_eq!(o.allow_read, Some(vec![]));
  }
}
