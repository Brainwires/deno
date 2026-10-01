// Copyright 2018-2026 the Deno authors. MIT license.

//! Node-API addons on Windows look the Node-API functions up in the host
//! *executable*, never in the library that implements them:
//!
//! - node-gyp / cmake-js addons import them from `node.exe` as a delay-loaded
//!   import, and node-gyp's `win_delay_load_hook` answers that import with
//!   `GetModuleHandle(NULL)`, the executable of the current process;
//! - napi-rs and neon addons call `GetProcAddress` on
//!   `GetModuleHandle(NULL)` (libloading's `Library::this()`).
//!
//! `deno.exe` and `deno compile` binaries export them (`/DEF:` with
//! `ext/napi/generated_symbol_exports_list_windows.def`). In a Deno Desktop app
//! the executable is laufey's backend host, which exports nothing, and the
//! runtime is this DLL. The DLL already exports every Node-API symbol, but no
//! addon looks there, so the first Node-API call fails: the delay-load helper
//! raises `0xC06D007F` (procedure not found) and the process dies.
//!
//! `install` makes the executable answer those lookups. It builds an export
//! directory, in memory, that keeps every export the executable already has
//! and adds each Node-API symbol it lacks as a small jump stub to this DLL's
//! function, then points the executable's in-memory export data directory at
//! it. `GetProcAddress` reads that directory from the mapped image on every
//! call, so both lookup paths then resolve, before any addon is loaded.
//! Export RVAs are 32-bit offsets from the executable's base, so the table and
//! the stubs live in a region allocated within 2 GiB above the image.
//!
//! The table layout is built by `build_table`, which is plain data and is
//! unit tested on every platform.

#![cfg_attr(
  not(windows),
  allow(
    dead_code,
    reason = "the builder and parser are only called from the Windows installer"
  )
)]

/// The Node-API (and libuv) symbols `deno.exe` exports on Windows.
const WINDOWS_SYMBOLS_DEF: &str =
  include_str!("../../ext/napi/generated_symbol_exports_list_windows.def");

/// Bytes per jump stub. Both encodings below are 16 bytes.
const STUB_SIZE: usize = 16;
/// `IMAGE_EXPORT_DIRECTORY` is 40 bytes.
const EXPORT_DIRECTORY_SIZE: usize = 40;
/// `IMAGE_DIRECTORY_ENTRY_EXPORT`'s offset in a PE32+ optional header.
const PE32_PLUS_EXPORT_DIRECTORY_OFFSET: usize = 112;
const PE32_PLUS_MAGIC: u16 = 0x20b;

/// The symbol names listed in the `.def` file's `EXPORTS` section.
pub fn napi_symbol_names() -> impl Iterator<Item = &'static str> {
  WINDOWS_SYMBOLS_DEF
    .lines()
    .skip_while(|line| line.trim() != "EXPORTS")
    .skip(1)
    .map(str::trim)
    .filter(|line| !line.is_empty() && !line.starts_with(';'))
}

/// One slot of an export address table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportTarget {
  /// An unused ordinal.
  None,
  /// Code or data at this RVA.
  Rva(u32),
  /// A forwarder string ("DLL.Function"); the loader tells it apart from code
  /// by its RVA falling inside the export directory.
  Forwarder(Vec<u8>),
}

/// The export directory of an image, read out of its mapped bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exports {
  pub dll_name: Option<Vec<u8>>,
  pub ordinal_base: u32,
  pub functions: Vec<ExportTarget>,
  /// (name, index into `functions`).
  pub names: Vec<(Vec<u8>, u32)>,
}

/// A finished table: `bytes` go at `region_rva`, the export data directory
/// becomes `(directory_rva, directory_size)`, and stub `i` (for the `i`th new
/// symbol, in the order given) is at `region_rva + i * STUB_SIZE`.
#[derive(Debug)]
pub struct Table {
  pub bytes: Vec<u8>,
  pub directory_rva: u32,
  pub directory_size: u32,
}

fn read_u16(image: &[u8], at: usize) -> Result<u16, String> {
  image
    .get(at..at + 2)
    .map(|b| u16::from_le_bytes([b[0], b[1]]))
    .ok_or_else(|| format!("read past the image at {at:#x}"))
}

fn read_u32(image: &[u8], at: usize) -> Result<u32, String> {
  image
    .get(at..at + 4)
    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    .ok_or_else(|| format!("read past the image at {at:#x}"))
}

fn read_cstr(image: &[u8], at: usize) -> Result<Vec<u8>, String> {
  let tail = image
    .get(at..)
    .ok_or_else(|| format!("string past the image at {at:#x}"))?;
  let len = tail
    .iter()
    .position(|b| *b == 0)
    .ok_or_else(|| format!("unterminated string at {at:#x}"))?;
  Ok(tail[..len].to_vec())
}

/// The offset of the export data directory entry (RVA, size) in a mapped
/// PE32+ image.
pub fn export_data_directory_offset(image: &[u8]) -> Result<usize, String> {
  if image.get(0..2) != Some(b"MZ") {
    return Err("no MZ header".into());
  }
  let nt = read_u32(image, 0x3c)? as usize;
  if image.get(nt..nt + 4) != Some(b"PE\0\0") {
    return Err("no PE signature".into());
  }
  let optional = nt + 24;
  let magic = read_u16(image, optional)?;
  if magic != PE32_PLUS_MAGIC {
    return Err(format!("not a PE32+ image (magic {magic:#x})"));
  }
  let rva_and_sizes = read_u32(image, optional + 108)?;
  if rva_and_sizes == 0 {
    return Err("the image has no data directories".into());
  }
  Ok(optional + PE32_PLUS_EXPORT_DIRECTORY_OFFSET)
}

/// Read the export directory of a mapped PE32+ image (`image[rva]` is the
/// byte at that RVA).
pub fn parse_exports(image: &[u8]) -> Result<Exports, String> {
  let entry = export_data_directory_offset(image)?;
  let dir_rva = read_u32(image, entry)? as usize;
  let dir_size = read_u32(image, entry + 4)? as usize;
  if dir_rva == 0 || dir_size == 0 {
    return Ok(Exports::default());
  }
  let name_rva = read_u32(image, dir_rva + 12)? as usize;
  let ordinal_base = read_u32(image, dir_rva + 16)?;
  let function_count = read_u32(image, dir_rva + 20)? as usize;
  let name_count = read_u32(image, dir_rva + 24)? as usize;
  let functions_rva = read_u32(image, dir_rva + 28)? as usize;
  let names_rva = read_u32(image, dir_rva + 32)? as usize;
  let ordinals_rva = read_u32(image, dir_rva + 36)? as usize;

  let mut functions = Vec::with_capacity(function_count);
  for i in 0..function_count {
    let rva = read_u32(image, functions_rva + 4 * i)?;
    let rva_usize = rva as usize;
    functions.push(if rva == 0 {
      ExportTarget::None
    } else if rva_usize >= dir_rva && rva_usize < dir_rva + dir_size {
      ExportTarget::Forwarder(read_cstr(image, rva_usize)?)
    } else {
      ExportTarget::Rva(rva)
    });
  }
  let mut names = Vec::with_capacity(name_count);
  for i in 0..name_count {
    let name = read_cstr(image, read_u32(image, names_rva + 4 * i)? as usize)?;
    let index = read_u16(image, ordinals_rva + 2 * i)? as u32;
    names.push((name, index));
  }
  Ok(Exports {
    dll_name: if name_rva == 0 {
      None
    } else {
      Some(read_cstr(image, name_rva)?)
    },
    ordinal_base,
    functions,
    names,
  })
}

/// Look a name up the way the loader does: a binary search of the sorted
/// name table.
pub fn lookup<'a>(
  exports: &'a Exports,
  name: &[u8],
) -> Option<&'a ExportTarget> {
  let i = exports
    .names
    .binary_search_by(|(n, _)| n.as_slice().cmp(name))
    .ok()?;
  exports.functions.get(exports.names[i].1 as usize)
}

/// A 16-byte absolute jump to `target`.
pub fn stub_bytes(target: u64) -> [u8; STUB_SIZE] {
  let mut stub = [0u8; STUB_SIZE];
  if cfg!(target_arch = "aarch64") {
    // ldr x16, #8 ; br x16 ; .quad target
    stub[0..4].copy_from_slice(&0x5800_0050u32.to_le_bytes());
    stub[4..8].copy_from_slice(&0xd61f_0200u32.to_le_bytes());
    stub[8..16].copy_from_slice(&target.to_le_bytes());
  } else {
    // jmp qword ptr [rip+0] ; .quad target ; 2 bytes of int3 padding
    stub[0..6].copy_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
    stub[6..14].copy_from_slice(&target.to_le_bytes());
    stub[14..16].copy_from_slice(&[0xcc, 0xcc]);
  }
  stub
}

/// Build an export table that keeps every export in `existing` (same
/// ordinals) and adds `added` (name, absolute target address), each through a
/// jump stub. The result is position dependent: it must be copied to
/// `region_rva`. Names already exported are not added twice.
pub fn build_table(
  existing: &Exports,
  added: &[(&str, u64)],
  region_rva: u32,
) -> Result<Table, String> {
  let added: Vec<(&str, u64)> = added
    .iter()
    .copied()
    .filter(|(name, _)| lookup(existing, name.as_bytes()).is_none())
    .collect();

  let stubs_len = added.len() * STUB_SIZE;
  let dir_off = stubs_len;
  let function_count = existing.functions.len() + added.len();
  let name_count = existing.names.len() + added.len();
  if function_count > u16::MAX as usize + 1 {
    return Err("too many exports".into());
  }
  let functions_off = dir_off + EXPORT_DIRECTORY_SIZE;
  let names_off = functions_off + 4 * function_count;
  let ordinals_off = names_off + 4 * name_count;
  let strings_off = ordinals_off + 2 * name_count;

  let mut bytes = vec![0u8; strings_off];
  let rva_of = |off: usize| -> Result<u32, String> {
    u32::try_from(off)
      .ok()
      .and_then(|off| region_rva.checked_add(off))
      .ok_or_else(|| "export table RVA overflow".to_string())
  };
  let push_str = |bytes: &mut Vec<u8>, s: &[u8]| -> Result<u32, String> {
    let rva = rva_of(bytes.len())?;
    bytes.extend_from_slice(s);
    bytes.push(0);
    Ok(rva)
  };
  let put_u32 = |bytes: &mut Vec<u8>, at: usize, v: u32| {
    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
  };

  // Stubs.
  for (i, (_, target)) in added.iter().enumerate() {
    bytes[i * STUB_SIZE..(i + 1) * STUB_SIZE]
      .copy_from_slice(&stub_bytes(*target));
  }

  // Address table: the existing slots, then one per stub.
  for (i, function) in existing.functions.iter().enumerate() {
    let rva = match function {
      ExportTarget::None => 0,
      ExportTarget::Rva(rva) => *rva,
      ExportTarget::Forwarder(s) => push_str(&mut bytes, s)?,
    };
    put_u32(&mut bytes, functions_off + 4 * i, rva);
  }
  for i in 0..added.len() {
    let rva = rva_of(i * STUB_SIZE)?;
    put_u32(
      &mut bytes,
      functions_off + 4 * (existing.functions.len() + i),
      rva,
    );
  }

  // Name and ordinal tables, sorted by name as the loader's binary search
  // expects.
  let mut names: Vec<(Vec<u8>, u32)> = existing.names.clone();
  for (i, (name, _)) in added.iter().enumerate() {
    names.push((
      name.as_bytes().to_vec(),
      (existing.functions.len() + i) as u32,
    ));
  }
  names.sort_by(|a, b| a.0.cmp(&b.0));
  for (i, (name, index)) in names.iter().enumerate() {
    let rva = push_str(&mut bytes, name)?;
    put_u32(&mut bytes, names_off + 4 * i, rva);
    bytes[ordinals_off + 2 * i..ordinals_off + 2 * i + 2]
      .copy_from_slice(&(*index as u16).to_le_bytes());
  }

  let dll_name_rva = match &existing.dll_name {
    Some(name) => push_str(&mut bytes, name)?,
    None => 0,
  };

  // IMAGE_EXPORT_DIRECTORY. Characteristics, TimeDateStamp and the version
  // stay zero.
  put_u32(&mut bytes, dir_off + 12, dll_name_rva);
  put_u32(&mut bytes, dir_off + 16, existing.ordinal_base.max(1));
  put_u32(&mut bytes, dir_off + 20, function_count as u32);
  put_u32(&mut bytes, dir_off + 24, name_count as u32);
  put_u32(&mut bytes, dir_off + 28, rva_of(functions_off)?);
  put_u32(&mut bytes, dir_off + 32, rva_of(names_off)?);
  put_u32(&mut bytes, dir_off + 36, rva_of(ordinals_off)?);

  let directory_size = u32::try_from(bytes.len() - dir_off)
    .map_err(|_| "export table too large".to_string())?;
  Ok(Table {
    directory_rva: rva_of(dir_off)?,
    directory_size,
    bytes,
  })
}

#[cfg(windows)]
pub use os::install;

#[cfg(windows)]
mod os {
  use std::ffi::CString;
  use std::ffi::c_void;

  use windows_sys::Win32::Foundation::HMODULE;
  use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
  use windows_sys::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS;
  use windows_sys::Win32::System::LibraryLoader::GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
  use windows_sys::Win32::System::LibraryLoader::GetModuleHandleExW;
  use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
  use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
  use windows_sys::Win32::System::Memory::MEM_COMMIT;
  use windows_sys::Win32::System::Memory::MEM_FREE;
  use windows_sys::Win32::System::Memory::MEM_RESERVE;
  use windows_sys::Win32::System::Memory::MEMORY_BASIC_INFORMATION;
  use windows_sys::Win32::System::Memory::PAGE_EXECUTE_READ;
  use windows_sys::Win32::System::Memory::PAGE_READWRITE;
  use windows_sys::Win32::System::Memory::VirtualAlloc;
  use windows_sys::Win32::System::Memory::VirtualProtect;
  use windows_sys::Win32::System::Memory::VirtualQuery;
  use windows_sys::Win32::System::Threading::GetCurrentProcess;

  use super::build_table;
  use super::export_data_directory_offset;
  use super::napi_symbol_names;
  use super::parse_exports;

  const ALLOCATION_GRANULARITY: usize = 0x1_0000;
  /// Keep the table within a signed 32-bit distance of the image base.
  const MAX_DISTANCE: usize = 0x7fff_0000;

  /// Make the host executable export this DLL's Node-API symbols. Returns how
  /// many symbols were added (0 when the executable already exports them, or
  /// when this module is the executable).
  pub fn install() -> Result<usize, String> {
    // SAFETY: Win32 calls on this process's own modules. The executable's
    // image stays mapped for the life of the process; the bytes read are
    // within its SizeOfImage, and the only write to it is the 8-byte export
    // data directory entry, made writable with VirtualProtect first and
    // restored after.
    unsafe {
      let exe = GetModuleHandleW(std::ptr::null()) as *const u8;
      let mut this: HMODULE = std::ptr::null_mut();
      if GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
          | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        install as *const c_void as *const u16,
        &mut this,
      ) == 0
      {
        return Err("GetModuleHandleExW failed for the runtime DLL".into());
      }
      if exe.is_null() || exe == this as *const u8 {
        return Ok(0);
      }

      // The headers' first page is enough to find SizeOfImage.
      let headers = std::slice::from_raw_parts(exe, 0x400);
      let entry = export_data_directory_offset(headers)?;
      let nt = u32::from_le_bytes(headers[0x3c..0x40].try_into().unwrap());
      let size_of_image_at = nt as usize + 24 + 56;
      let size_of_image = u32::from_le_bytes(
        headers[size_of_image_at..size_of_image_at + 4]
          .try_into()
          .unwrap(),
      ) as usize;
      let image = std::slice::from_raw_parts(exe, size_of_image);
      let existing = parse_exports(image)?;

      let mut added = Vec::new();
      for name in napi_symbol_names() {
        let cname = CString::new(name).unwrap();
        if GetProcAddress(exe as HMODULE, cname.as_ptr() as *const u8).is_some()
        {
          continue;
        }
        if let Some(f) = GetProcAddress(this, cname.as_ptr() as *const u8) {
          added.push((name, f as usize as u64));
        }
      }
      if added.is_empty() {
        return Ok(0);
      }

      let len = build_table(&existing, &added, 0)?.bytes.len();
      let region = allocate_above(exe, size_of_image, len)?;
      let region_rva = u32::try_from(region as usize - exe as usize)
        .map_err(|_| "export table out of RVA range".to_string())?;
      let table = build_table(&existing, &added, region_rva)?;
      std::ptr::copy_nonoverlapping(
        table.bytes.as_ptr(),
        region,
        table.bytes.len(),
      );
      let mut old = 0;
      if VirtualProtect(
        region as *const c_void,
        table.bytes.len(),
        PAGE_EXECUTE_READ,
        &mut old,
      ) == 0
      {
        return Err("VirtualProtect(PAGE_EXECUTE_READ) failed".into());
      }
      FlushInstructionCache(
        GetCurrentProcess(),
        region as *const c_void,
        table.bytes.len(),
      );

      let entry_ptr = exe.add(entry) as *mut u8;
      if VirtualProtect(entry_ptr as *const c_void, 8, PAGE_READWRITE, &mut old)
        == 0
      {
        return Err("VirtualProtect failed on the executable's headers".into());
      }
      let mut value = [0u8; 8];
      value[0..4].copy_from_slice(&table.directory_rva.to_le_bytes());
      value[4..8].copy_from_slice(&table.directory_size.to_le_bytes());
      std::ptr::write_unaligned(entry_ptr as *mut [u8; 8], value);
      let mut ignored = 0;
      VirtualProtect(entry_ptr as *const c_void, 8, old, &mut ignored);

      Ok(added.len())
    }
  }

  /// Commit `len` read-write bytes in the first free region above the image
  /// that is within `MAX_DISTANCE` of its base.
  fn allocate_above(
    base: *const u8,
    size_of_image: usize,
    len: usize,
  ) -> Result<*mut u8, String> {
    let base = base as usize;
    let limit = base + MAX_DISTANCE - len;
    let mut addr = (base + size_of_image + ALLOCATION_GRANULARITY - 1)
      & !(ALLOCATION_GRANULARITY - 1);
    while addr < limit {
      // SAFETY: MEMORY_BASIC_INFORMATION is plain data; all zeroes is valid.
      let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
      // SAFETY: VirtualQuery only describes the address space; `info` is a
      // valid out pointer of the size passed.
      let queried = unsafe {
        VirtualQuery(
          addr as *const c_void,
          &mut info,
          std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
      };
      if queried == 0 {
        break;
      }
      let region_end = info.BaseAddress as usize + info.RegionSize;
      if info.State == MEM_FREE && region_end - addr >= len {
        // SAFETY: `addr` starts a free region at least `len` bytes long, so
        // reserving it cannot affect memory in use.
        let p = unsafe {
          VirtualAlloc(
            addr as *const c_void,
            len,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
          )
        };
        if !p.is_null() {
          return Ok(p as *mut u8);
        }
      }
      addr = (region_end.max(addr + 1) + ALLOCATION_GRANULARITY - 1)
        & !(ALLOCATION_GRANULARITY - 1);
    }
    Err("no free address space near the executable".into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A minimal mapped PE32+ image: headers at 0, `extra` copied at
  /// `extra_rva`, and the export data directory set to `dir`.
  fn image(
    size: usize,
    dir: (u32, u32),
    extra: &[u8],
    extra_rva: usize,
  ) -> Vec<u8> {
    let mut img = vec![0u8; size.max(extra_rva + extra.len())];
    img[0..2].copy_from_slice(b"MZ");
    let nt = 0x80usize;
    img[0x3c..0x40].copy_from_slice(&(nt as u32).to_le_bytes());
    img[nt..nt + 4].copy_from_slice(b"PE\0\0");
    let opt = nt + 24;
    img[opt..opt + 2].copy_from_slice(&PE32_PLUS_MAGIC.to_le_bytes());
    img[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
    let e = opt + PE32_PLUS_EXPORT_DIRECTORY_OFFSET;
    img[e..e + 4].copy_from_slice(&dir.0.to_le_bytes());
    img[e + 4..e + 8].copy_from_slice(&dir.1.to_le_bytes());
    img[extra_rva..extra_rva + extra.len()].copy_from_slice(extra);
    img
  }

  fn install_table(existing: &Exports, added: &[(&str, u64)]) -> Exports {
    let region_rva = 0x2000u32;
    let table = build_table(existing, added, region_rva).unwrap();
    let img = image(
      0x1000,
      (table.directory_rva, table.directory_size),
      &table.bytes,
      region_rva as usize,
    );
    parse_exports(&img).unwrap()
  }

  #[test]
  fn def_lists_the_napi_and_uv_symbols() {
    let names: Vec<_> = napi_symbol_names().collect();
    assert!(names.contains(&"napi_create_function"));
    assert!(names.contains(&"napi_module_register"));
    assert!(names.contains(&"node_api_create_syntax_error"));
    assert!(names.contains(&"uv_default_loop"));
    assert!(!names.iter().any(|n| n.contains(' ') || *n == "EXPORTS"));
    assert!(names.len() > 200, "{}", names.len());
  }

  #[test]
  fn no_exports_without_a_directory() {
    let img = image(0x1000, (0, 0), &[], 0);
    assert_eq!(parse_exports(&img).unwrap(), Exports::default());
  }

  #[test]
  fn adds_stubs_to_an_executable_without_exports() {
    let added = [
      ("napi_get_version", 0x7ff6_0000_1000u64),
      ("napi_create_function", 0x7ff6_0000_2000),
      ("uv_default_loop", 0x7ff6_0000_3000),
    ];
    let exports = install_table(&Exports::default(), &added);
    assert_eq!(exports.ordinal_base, 1);
    assert_eq!(exports.functions.len(), 3);
    // Sorted for the loader's binary search.
    let names: Vec<_> = exports.names.iter().map(|(n, _)| n.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    // Each name resolves to its own stub, outside the export directory (so
    // not mistaken for a forwarder), and the stub jumps to its target.
    let table = build_table(&Exports::default(), &added, 0x2000).unwrap();
    for (i, (name, target)) in added.iter().enumerate() {
      let rva = 0x2000 + (i * STUB_SIZE) as u32;
      assert_eq!(
        lookup(&exports, name.as_bytes()),
        Some(&ExportTarget::Rva(rva))
      );
      assert!(rva < table.directory_rva);
      assert_eq!(
        &table.bytes[i * STUB_SIZE..(i + 1) * STUB_SIZE],
        &stub_bytes(*target)
      );
    }
    assert_eq!(lookup(&exports, b"napi_missing"), None);
  }

  #[test]
  fn keeps_existing_exports_and_ordinals() {
    let existing = Exports {
      dll_name: Some(b"laufey_webview.exe".to_vec()),
      ordinal_base: 5,
      functions: vec![
        ExportTarget::Rva(0x1234),
        ExportTarget::None,
        ExportTarget::Forwarder(b"OTHER.Function".to_vec()),
      ],
      names: vec![
        (b"AmdPowerXpressRequestHighPerformance".to_vec(), 0),
        (b"NvOptimusEnablement".to_vec(), 2),
      ],
    };
    let exports = install_table(
      &existing,
      &[
        ("napi_get_version", 0x1000),
        // Already exported: not added again.
        ("NvOptimusEnablement", 0x2000),
      ],
    );
    assert_eq!(
      exports.dll_name.as_deref(),
      Some(&b"laufey_webview.exe"[..])
    );
    assert_eq!(exports.ordinal_base, 5);
    assert_eq!(&exports.functions[..3], &existing.functions[..]);
    assert_eq!(exports.functions.len(), 4);
    assert_eq!(
      lookup(&exports, b"AmdPowerXpressRequestHighPerformance"),
      Some(&ExportTarget::Rva(0x1234))
    );
    assert_eq!(
      lookup(&exports, b"NvOptimusEnablement"),
      Some(&ExportTarget::Forwarder(b"OTHER.Function".to_vec()))
    );
    assert_eq!(
      lookup(&exports, b"napi_get_version"),
      Some(&ExportTarget::Rva(0x2000))
    );
  }

  #[test]
  fn x86_64_stub_is_an_absolute_indirect_jump() {
    if cfg!(target_arch = "aarch64") {
      return;
    }
    let stub = stub_bytes(0x1122_3344_5566_7788);
    assert_eq!(&stub[0..6], &[0xff, 0x25, 0, 0, 0, 0]);
    assert_eq!(&stub[6..14], &0x1122_3344_5566_7788u64.to_le_bytes());
  }

  #[test]
  fn rejects_non_pe32_plus_images() {
    let mut img = image(0x1000, (0, 0), &[], 0);
    img[0x80 + 24] = 0x0b;
    img[0x80 + 25] = 0x01;
    assert!(parse_exports(&img).is_err());
    assert!(parse_exports(b"not an image").is_err());
  }
}
