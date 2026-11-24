// Copyright 2018-2026 the Deno authors. MIT license.

//! The files a program list needs to execute, for a strict exec scope
//! (`--kernel-sandbox-exec=strict`): each program, the interpreter a script
//! names on its `#!` line (and, through `/usr/bin/env`, the program it
//! runs), and on Linux an ELF program's `PT_INTERP` (the dynamic loader,
//! which the kernel opens for execution too); on macOS, the shell `/bin/sh`
//! launches. Followed a few levels deep, canonical paths (the kernel checks
//! the file it reaches, not the link). Of those files, the ones an `exec`
//! system call names are told apart ([`ProgramClosure::targets`]): the kernel
//! opens an interpreter and a loader itself while it loads the file a call
//! named. Other launchers aren't followed: the developer tools in macOS's
//! `/usr/bin` (git, python3, clang) start the real tool through xcrun, which
//! a strict list must name too.

#![allow(
  clippy::disallowed_methods,
  reason = "the programs the kernel executes are on the real file system"
)]

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

/// How many interpreters deep a chain is followed (`#!` lines naming
/// scripts that name scripts).
const MAX_DEPTH: usize = 4;

/// The canonical files `programs` need to execute (see the module docs),
/// sorted. A program that doesn't exist is left out (it can't run anyway).
/// `path` is the `PATH` an `env` interpreter searches.
pub fn program_closure(
  programs: &[PathBuf],
  path: Option<&OsStr>,
) -> Vec<PathBuf> {
  program_closure_parts(programs, path).files
}

/// What a program list needs ([`program_closure_parts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramClosure {
  /// Every file the kernel opens for execution: [`program_closure`].
  pub files: Vec<PathBuf>,
  /// The files among them that an `exec` system call names: the programs
  /// themselves, and the program an `env` interpreter line runs (`env`
  /// starts it with a call of its own). A `#!` interpreter and an ELF
  /// loader are not among them: the kernel opens those itself while it
  /// loads the file the call named.
  pub targets: Vec<PathBuf>,
}

/// [`program_closure`] with the files an `exec` system call names told
/// apart ([`ProgramClosure::targets`]); both lists are canonical and sorted.
pub fn program_closure_parts(
  programs: &[PathBuf],
  path: Option<&OsStr>,
) -> ProgramClosure {
  let mut out = ProgramClosure::default();
  for program in programs {
    visit(program, path, 0, true, &mut out);
  }
  out.files.sort();
  out.files.dedup();
  out.targets.sort();
  out.targets.dedup();
  out
}

/// `target`: an `exec` system call names this file (see
/// [`ProgramClosure::targets`]).
fn visit(
  program: &Path,
  path: Option<&OsStr>,
  depth: usize,
  target: bool,
  out: &mut ProgramClosure,
) {
  let Ok(real) = std::fs::canonicalize(program) else {
    return;
  };
  if depth > MAX_DEPTH {
    return;
  }
  // a file reached first as an interpreter may be a program of the list too
  if target && !out.targets.contains(&real) {
    out.targets.push(real.clone());
  }
  if out.files.contains(&real) {
    return;
  }
  out.files.push(real.clone());
  // macOS's /bin/sh is a launcher that executes the shell
  // /private/var/select/sh names (bash, dash or zsh)
  #[cfg(target_os = "macos")]
  if real == Path::new("/bin/sh") {
    visit(
      Path::new("/private/var/select/sh"),
      path,
      depth + 1,
      true,
      out,
    );
  }
  let Ok(head) = read_head(&real) else {
    return;
  };
  if let Some((interpreter, arg)) = shebang(&head) {
    visit(&interpreter, path, depth + 1, false, out);
    // `#!/usr/bin/env node`: env executes the program it names
    if interpreter.file_name() == Some(OsStr::new("env"))
      && let Some(name) = arg
      && let Some(found) = find_on_path(&name, path)
    {
      visit(&found, path, depth + 1, true, out);
    }
  } else if let Some(loader) = elf_interpreter(&real) {
    visit(&loader, path, depth + 1, false, out);
  }
}

fn read_head(file: &Path) -> std::io::Result<Vec<u8>> {
  let mut head = vec![0u8; 256];
  let mut f = std::fs::File::open(file)?;
  let n = f.read(&mut head)?;
  head.truncate(n);
  Ok(head)
}

/// The interpreter of a `#!` line and its first argument (for `env`, the
/// first that isn't an option or an assignment).
pub fn shebang(head: &[u8]) -> Option<(PathBuf, Option<String>)> {
  let line = head.strip_prefix(b"#!")?;
  let end = line.iter().position(|b| *b == b'\n').unwrap_or(line.len());
  let line = std::str::from_utf8(&line[..end]).ok()?;
  let mut words = line.split_whitespace();
  let interpreter = PathBuf::from(words.next()?);
  if !interpreter.is_absolute() {
    return None;
  }
  let arg = words
    .find(|w| !w.starts_with('-') && !w.contains('='))
    .map(|w| w.to_string());
  Some((interpreter, arg))
}

fn find_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
  if name.contains('/') {
    return Some(PathBuf::from(name));
  }
  std::env::split_paths(path?)
    .map(|dir| dir.join(name))
    .find(|p| p.is_file())
}

/// An ELF program's `PT_INTERP` (64-bit, either byte order); `None` for
/// anything else.
pub fn elf_interpreter(file: &Path) -> Option<PathBuf> {
  // the program headers and the interpreter's name sit at the start
  let mut bytes = Vec::new();
  std::fs::File::open(file)
    .ok()?
    .take(64 * 1024)
    .read_to_end(&mut bytes)
    .ok()?;
  elf_interpreter_of(&bytes)
}

pub fn elf_interpreter_of(bytes: &[u8]) -> Option<PathBuf> {
  const PT_INTERP: u32 = 3;
  if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 {
    return None;
  }
  let little = match bytes[5] {
    1 => true,
    2 => false,
    _ => return None,
  };
  let u16_at = |at: usize| -> Option<u16> {
    let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
    Some(if little {
      u16::from_le_bytes(b)
    } else {
      u16::from_be_bytes(b)
    })
  };
  let u32_at = |at: usize| -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if little {
      u32::from_le_bytes(b)
    } else {
      u32::from_be_bytes(b)
    })
  };
  let u64_at = |at: usize| -> Option<u64> {
    let b: [u8; 8] = bytes.get(at..at + 8)?.try_into().ok()?;
    Some(if little {
      u64::from_le_bytes(b)
    } else {
      u64::from_be_bytes(b)
    })
  };
  let phoff = usize::try_from(u64_at(32)?).ok()?;
  let phentsize = u16_at(54)? as usize;
  let phnum = u16_at(56)? as usize;
  if phentsize < 56 {
    return None;
  }
  for i in 0..phnum.min(256) {
    let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
    if u32_at(ph)? != PT_INTERP {
      continue;
    }
    let offset = usize::try_from(u64_at(ph + 8)?).ok()?;
    let size = usize::try_from(u64_at(ph + 32)?).ok()?;
    let raw = bytes.get(offset..offset.checked_add(size)?)?;
    let text = raw.split(|b| *b == 0).next()?;
    let text = std::str::from_utf8(text).ok()?;
    return text.starts_with('/').then(|| PathBuf::from(text));
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn shebang_lines() {
    assert_eq!(
      shebang(b"#!/bin/sh\necho"),
      Some((PathBuf::from("/bin/sh"), None))
    );
    assert_eq!(
      shebang(b"#! /usr/bin/env -S FOO=1 node --x\n"),
      Some((PathBuf::from("/usr/bin/env"), Some("node".to_string())))
    );
    assert_eq!(shebang(b"#!relative\n"), None);
    assert_eq!(shebang(b"\x7fELF"), None);
  }

  #[test]
  fn elf_interpreter_from_a_program_header() {
    // a minimal little-endian ELF64 with one PT_INTERP header
    let mut elf = vec![0u8; 64 + 56];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2;
    elf[5] = 1;
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    let interp = b"/lib/ld-test.so\0";
    let offset = elf.len() as u64;
    elf[64..68].copy_from_slice(&3u32.to_le_bytes());
    elf[72..80].copy_from_slice(&offset.to_le_bytes());
    elf[96..104].copy_from_slice(&(interp.len() as u64).to_le_bytes());
    elf.extend_from_slice(interp);
    assert_eq!(
      elf_interpreter_of(&elf),
      Some(PathBuf::from("/lib/ld-test.so"))
    );
    // truncated, or not ELF64: nothing
    assert_eq!(elf_interpreter_of(&elf[..80]), None);
    let mut elf32 = elf.clone();
    elf32[4] = 1;
    assert_eq!(elf_interpreter_of(&elf32), None);
  }

  #[test]
  fn closure_follows_scripts_to_their_interpreters() {
    let dir = std::env::temp_dir()
      .join(format!("deno-exec-closure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("tool");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let closure = program_closure(std::slice::from_ref(&script), None);
    let sh = std::fs::canonicalize("/bin/sh").unwrap();
    assert!(closure.contains(&std::fs::canonicalize(&script).unwrap()));
    assert!(closure.contains(&sh), "{closure:?}");
    // macOS: the shell the /bin/sh launcher starts
    #[cfg(target_os = "macos")]
    if let Ok(shell) = std::fs::canonicalize("/private/var/select/sh") {
      assert!(closure.contains(&shell), "{closure:?}");
    }
    // a script through env: env and the program it runs
    let via_env = dir.join("via-env");
    std::fs::write(&via_env, "#!/usr/bin/env sh\n").unwrap();
    let path = std::env::var_os("PATH");
    let closure = program_closure(&[via_env], path.as_deref());
    assert!(closure.contains(&sh), "{closure:?}");
    if Path::new("/usr/bin/env").exists() {
      assert!(
        closure.contains(&std::fs::canonicalize("/usr/bin/env").unwrap())
      );
    }
    // a missing program: nothing
    assert!(program_closure(&[dir.join("missing")], None).is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn targets_are_the_files_a_call_names() {
    let dir = std::env::temp_dir()
      .join(format!("deno-exec-targets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sh = std::fs::canonicalize("/bin/sh").unwrap();
    let script = dir.join("tool");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let real_script = std::fs::canonicalize(&script).unwrap();
    // a script: the kernel opens its interpreter, no call names it
    let parts = program_closure_parts(std::slice::from_ref(&script), None);
    assert!(parts.files.contains(&sh), "{parts:?}");
    assert!(parts.targets.contains(&real_script), "{parts:?}");
    #[cfg(target_os = "linux")]
    assert_eq!(parts.targets, vec![real_script.clone()]);
    // every target is one of the files
    assert!(parts.targets.iter().all(|t| parts.files.contains(t)));
    // ... unless the list names the interpreter too, in either order
    for programs in [
      vec![script.clone(), PathBuf::from("/bin/sh")],
      vec![PathBuf::from("/bin/sh"), script.clone()],
    ] {
      let parts = program_closure_parts(&programs, None);
      assert!(parts.targets.contains(&sh), "{parts:?}");
      assert!(parts.targets.contains(&real_script), "{parts:?}");
    }
    // a script through env: env starts the program it names with a call
    // of its own
    let via_env = dir.join("via-env");
    std::fs::write(&via_env, "#!/usr/bin/env sh\n").unwrap();
    let path = std::env::var_os("PATH");
    let parts =
      program_closure_parts(std::slice::from_ref(&via_env), path.as_deref());
    if let Ok(env) = std::fs::canonicalize("/usr/bin/env")
      && let Some(found) = find_on_path("sh", path.as_deref())
      && let Ok(found) = std::fs::canonicalize(found)
    {
      assert!(parts.files.contains(&env), "{parts:?}");
      assert!(parts.targets.contains(&found), "{parts:?}");
      // (one file under both names where a multi-call binary provides them)
      if env != found {
        assert!(!parts.targets.contains(&env), "{parts:?}");
      }
    }
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn closure_of_a_dynamic_program_has_its_loader() {
    let sh = std::fs::canonicalize("/bin/sh").unwrap();
    if let Some(loader) = elf_interpreter(&sh) {
      let closure = program_closure(&[sh], None);
      assert!(
        closure.contains(&std::fs::canonicalize(loader).unwrap()),
        "{closure:?}"
      );
    }
  }
}
