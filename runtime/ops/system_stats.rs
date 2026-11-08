// Copyright 2018-2026 the Deno authors. MIT license.

//! `Deno.desktop.systemStats`: aggregate CPU, GPU and memory figures of the
//! machine, sampled natively inside the runtime (no subprocess, no FFI from
//! JavaScript, no file reads under the app's permissions).
//!
//! - CPU: the busy share of the tick counters between this sampler's
//!   previous snapshot and now (macOS `host_processor_info`, Linux
//!   `/proc/stat`, Windows `GetSystemTimes` and
//!   `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`).
//! - GPU: the driver's own utilization figure (macOS IOKit's
//!   `PerformanceStatistics`, Linux amdgpu's `gpu_busy_percent` and NVIDIA's
//!   NVML when the driver's library is installed, Windows PDH's
//!   `\GPU Engine(*)\Utilization Percentage`).
//! - Memory: the OS's pressure figure (macOS `kern.memorystatus_level`,
//!   Linux `MemAvailable`, Windows `dwMemoryLoad`).
//!
//! Only aggregate figures: no per-process data, no host names, no serials.
//! The arithmetic and the parsers are pure functions with unit tests; the OS
//! calls are behind [`Sampler::sample`], which blocks (the first call sleeps
//! [`PRIME_INTERVAL`]) and so runs on the blocking pool.

use std::time::Duration;

use serde::Serialize;

/// How far apart the two snapshots of a sampler's first sample are.
pub const PRIME_INTERVAL: Duration = Duration::from_millis(100);

/// Busy and idle ticks of one CPU (or of all of them), cumulative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTicks {
  pub busy: u64,
  pub idle: u64,
}

#[cfg_attr(not(any(test, target_os = "linux", windows)), allow(dead_code))]
impl CpuTicks {
  /// The ticks from `prev` to `self`; `None` when a counter went backwards
  /// (a CPU taken offline and back, a counter reset).
  pub fn since(self, prev: CpuTicks) -> Option<CpuTicks> {
    Some(CpuTicks {
      busy: self.busy.checked_sub(prev.busy)?,
      idle: self.idle.checked_sub(prev.idle)?,
    })
  }
}

/// A CPU snapshot: the aggregate ticks and each core's.
#[cfg_attr(not(any(test, target_os = "linux", windows)), allow(dead_code))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuSnapshot {
  pub total: Option<CpuTicks>,
  pub cores: Vec<CpuTicks>,
}

/// `value` clamped to 0..=100 and rounded to one decimal; `None` for NaN.
pub fn percent(value: f64) -> Option<f64> {
  if value.is_nan() {
    return None;
  }
  Some((value.clamp(0.0, 100.0) * 10.0).round() / 10.0)
}

/// The busy share of `delta`, in percent; `None` when no time passed.
pub fn busy_percent(delta: CpuTicks) -> Option<f64> {
  let total = delta.busy.checked_add(delta.idle)?;
  if total == 0 {
    return None;
  }
  percent(delta.busy as f64 * 100.0 / total as f64)
}

/// The CPU figures between two snapshots: the aggregate (from `total`, else
/// the sum of the cores) and each core's (0 for a core with no ticks between
/// them; cores beyond the shorter snapshot are left out).
#[cfg_attr(not(any(test, target_os = "linux", windows)), allow(dead_code))]
pub fn cpu_between(
  prev: &CpuSnapshot,
  next: &CpuSnapshot,
) -> (Option<f64>, Vec<f64>) {
  let cores: Vec<Option<CpuTicks>> = prev
    .cores
    .iter()
    .zip(next.cores.iter())
    .map(|(p, n)| n.since(*p))
    .collect();
  let total = match (prev.total, next.total) {
    (Some(p), Some(n)) => n.since(p),
    _ if !cores.is_empty() && cores.iter().all(Option::is_some) => {
      Some(cores.iter().flatten().fold(CpuTicks::default(), |acc, d| {
        CpuTicks {
          busy: acc.busy.saturating_add(d.busy),
          idle: acc.idle.saturating_add(d.idle),
        }
      }))
    }
    _ => None,
  };
  (
    total.and_then(busy_percent),
    cores
      .into_iter()
      .map(|d| d.and_then(busy_percent).unwrap_or(0.0))
      .collect(),
  )
}

/// Mach's per-CPU `[user, system, idle, nice]` tick counters
/// (`natural_t`, which wrap at 2^32), as a delta: each counter's forward
/// distance, so a wrap between two snapshots is not a negative figure.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn mach_ticks_between(prev: [u32; 4], next: [u32; 4]) -> CpuTicks {
  let d = |i: usize| u64::from(next[i].wrapping_sub(prev[i]));
  CpuTicks {
    busy: d(0) + d(1) + d(3),
    idle: d(2),
  }
}

/// The CPU figures between two Mach snapshots (one `[user, system, idle,
/// nice]` per CPU): the aggregate is the sum of the cores' deltas.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn mach_cpu_between(
  prev: &[[u32; 4]],
  next: &[[u32; 4]],
) -> (Option<f64>, Vec<f64>) {
  let deltas: Vec<CpuTicks> = prev
    .iter()
    .zip(next.iter())
    .map(|(p, n)| mach_ticks_between(*p, *n))
    .collect();
  if deltas.is_empty() {
    return (None, Vec::new());
  }
  let sum = deltas.iter().fold(CpuTicks::default(), |acc, d| CpuTicks {
    busy: acc.busy + d.busy,
    idle: acc.idle + d.idle,
  });
  (
    busy_percent(sum),
    deltas
      .into_iter()
      .map(|d| busy_percent(d).unwrap_or(0.0))
      .collect(),
  )
}

/// One `cpu` / `cpuN` line of Linux's `/proc/stat` (user nice system idle
/// iowait irq softirq steal; guest time is already in user and nice): idle
/// and iowait are the idle ticks.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn parse_proc_stat_line(fields: &str) -> Option<CpuTicks> {
  let mut n = [0u64; 8];
  let mut count = 0;
  for (i, f) in fields.split_ascii_whitespace().take(8).enumerate() {
    n[i] = f.parse().ok()?;
    count += 1;
  }
  if count < 4 {
    return None;
  }
  let [user, nice, system, idle, iowait, irq, softirq, steal] = n;
  Some(CpuTicks {
    busy: user + nice + system + irq + softirq + steal,
    idle: idle + iowait,
  })
}

/// Linux's `/proc/stat`: the aggregate `cpu` line and each `cpuN` line.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
pub fn parse_proc_stat(text: &str) -> CpuSnapshot {
  let mut snapshot = CpuSnapshot::default();
  for line in text.lines() {
    let Some(rest) = line.strip_prefix("cpu") else {
      continue;
    };
    if let Some(fields) = rest.strip_prefix([' ', '\t']) {
      snapshot.total = parse_proc_stat_line(fields);
    } else {
      let digits = rest.find([' ', '\t']).unwrap_or(rest.len());
      if digits == 0 || !rest[..digits].bytes().all(|b| b.is_ascii_digit()) {
        continue;
      }
      if let Some(ticks) = parse_proc_stat_line(&rest[digits..]) {
        snapshot.cores.push(ticks);
      }
    }
  }
  snapshot
}

/// Memory figures: the pressure in percent and, where known, the bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
  pub pressure: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub used_bytes: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub total_bytes: Option<f64>,
}

/// Memory from Linux's `/proc/meminfo`: pressure = 100 × (1 − MemAvailable
/// / MemTotal), used = total − available.
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
pub fn parse_meminfo(text: &str) -> MemoryStats {
  let kib = |key: &str| -> Option<u64> {
    text.lines().find_map(|line| {
      let rest = line.strip_prefix(key)?.strip_prefix(':')?;
      rest.split_ascii_whitespace().next()?.parse().ok()
    })
  };
  let (Some(total), Some(available)) = (kib("MemTotal"), kib("MemAvailable"))
  else {
    return MemoryStats::default();
  };
  if total == 0 {
    return MemoryStats::default();
  }
  let available = available.min(total);
  MemoryStats {
    pressure: percent(100.0 * (1.0 - available as f64 / total as f64)),
    used_bytes: Some(((total - available) * 1024) as f64),
    total_bytes: Some((total * 1024) as f64),
  }
}

/// Memory pressure from macOS's `kern.memorystatus_level` (the share of
/// memory still available, 0-100).
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn mac_memory_pressure(level: i32) -> Option<f64> {
  (0..=100).contains(&level).then(|| f64::from(100 - level))
}

/// The keys IOKit's `PerformanceStatistics` names a GPU's busy share by,
/// best first: Apple silicon and Intel graphics, then AMD on macOS.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub const GPU_UTILIZATION_KEYS: &[&str] =
  &["Device Utilization %", "GPU Activity(%)"];

/// The first of [`GPU_UTILIZATION_KEYS`] `lookup` answers, as a percent.
#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
pub fn performance_statistics_utilization(
  lookup: impl Fn(&str) -> Option<i64>,
) -> Option<f64> {
  GPU_UTILIZATION_KEYS
    .iter()
    .find_map(|key| lookup(key))
    .and_then(|v| percent(v as f64))
}

/// One GPU's figure.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuDevice {
  pub name: String,
  pub utilization: Option<f64>,
}

/// GPU figures: the busiest device's utilization and the devices.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GpuStats {
  pub utilization: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub devices: Option<Vec<GpuDevice>>,
}

/// The GPU figures of a set of named devices (from every source the OS has,
/// e.g. amdgpu's sysfs and NVML): the busiest one's utilization; `devices`
/// only when there are any.
#[cfg_attr(
  not(any(test, target_os = "macos", target_os = "linux")),
  allow(dead_code)
)]
pub fn gpu_from_devices(devices: Vec<GpuDevice>) -> GpuStats {
  let utilization = devices
    .iter()
    .filter_map(|d| d.utilization)
    .fold(None, |best: Option<f64>, v| {
      Some(best.map_or(v, |b| b.max(v)))
    });
  GpuStats {
    utilization,
    devices: (!devices.is_empty()).then_some(devices),
  }
}

/// amdgpu's sysfs `gpu_busy_percent` (a bare number).
#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
pub fn parse_busy_percent(text: &str) -> Option<f64> {
  let n: f64 = text.trim().parse().ok()?;
  n.is_finite().then(|| percent(n)).flatten()
}

/// A PDH `\GPU Engine(*)` instance name
/// (`pid_<pid>_luid_<hi>_<lo>_phys_<n>_eng_<n>_engtype_<type>`): the
/// adapter (`luid_…_phys_<n>`) and the engine type. The process id is
/// dropped: the figures are summed over every process.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub fn gpu_engine_key(instance: &str) -> Option<(&str, &str)> {
  let adapter_start = instance.find("luid_")?;
  let eng = instance[adapter_start..].find("_eng_")? + adapter_start;
  let engtype = instance.rfind("_engtype_")?;
  if engtype < eng {
    return None;
  }
  Some((
    &instance[adapter_start..eng],
    &instance[engtype + "_engtype_".len()..],
  ))
}

/// The GPU utilization from PDH's `\GPU Engine(*)\Utilization Percentage`
/// items: summed per adapter and engine type (every process's share of the
/// 3D engine, …), the busiest of those; `None` when there are none.
#[cfg_attr(not(any(test, windows)), allow(dead_code))]
pub fn gpu_from_engines<'a>(
  items: impl IntoIterator<Item = (&'a str, f64)>,
) -> Option<f64> {
  let mut sums: Vec<((&'a str, &'a str), f64)> = Vec::new();
  for (name, value) in items {
    let Some(key) = gpu_engine_key(name) else {
      continue;
    };
    if !value.is_finite() {
      continue;
    }
    match sums.iter_mut().find(|(k, _)| *k == key) {
      Some((_, sum)) => *sum += value,
      None => sums.push((key, value)),
    }
  }
  sums
    .into_iter()
    .map(|(_, v)| v)
    .fold(None, |best: Option<f64>, v| {
      Some(best.map_or(v, |b| b.max(v)))
    })
    .and_then(percent)
}

/// CPU figures.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CpuStats {
  pub total: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub cores: Option<Vec<f64>>,
}

/// One `Deno.desktop.SystemStats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SystemStats {
  /// Milliseconds since the epoch.
  pub at: f64,
  pub cpu: CpuStats,
  pub gpu: GpuStats,
  pub memory: MemoryStats,
}

/// Whether this runtime samples on this OS.
pub const SUPPORTED: bool = cfg!(any(
  target_os = "macos",
  target_os = "linux",
  target_os = "windows"
));

fn now_ms() -> f64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_millis() as f64)
    .unwrap_or(0.0)
}

/// One sampler's state: its previous CPU snapshot (and, on Windows, its PDH
/// query), so its figure is "since this sampler's previous sample".
#[derive(Default)]
pub struct Sampler {
  prev: Option<os::Cpu>,
  #[cfg(windows)]
  pdh: Option<os::GpuQuery>,
}

impl Sampler {
  /// One sample. Blocks: the first call takes two snapshots
  /// [`PRIME_INTERVAL`] apart, and the OS calls may take a few
  /// milliseconds; call it on the blocking pool.
  pub fn sample(&mut self, per_core: bool) -> SystemStats {
    #[cfg(windows)]
    let pdh = self.pdh.get_or_insert_with(os::GpuQuery::open);
    let prev = match self.prev.take() {
      Some(prev) => Some(prev),
      None => {
        let first = os::cpu();
        std::thread::sleep(PRIME_INTERVAL);
        first
      }
    };
    let next = os::cpu();
    let (total, cores) = match (&prev, &next) {
      (Some(p), Some(n)) => os::cpu_between(p, n),
      _ => (None, Vec::new()),
    };
    if next.is_some() {
      self.prev = next;
    }
    #[cfg(windows)]
    let gpu = pdh.sample();
    #[cfg(not(windows))]
    let gpu = os::gpu();
    SystemStats {
      at: now_ms(),
      cpu: CpuStats {
        total,
        cores: per_core.then_some(cores),
      },
      gpu,
      memory: os::memory(),
    }
  }
}

#[cfg(target_os = "linux")]
mod os {
  use std::sync::OnceLock;

  use super::CpuSnapshot;
  use super::GpuDevice;
  use super::GpuStats;
  use super::MemoryStats;

  pub type Cpu = CpuSnapshot;

  pub fn cpu() -> Option<Cpu> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let snapshot = super::parse_proc_stat(&text);
    (snapshot.total.is_some() || !snapshot.cores.is_empty()).then_some(snapshot)
  }

  pub fn cpu_between(prev: &Cpu, next: &Cpu) -> (Option<f64>, Vec<f64>) {
    super::cpu_between(prev, next)
  }

  pub fn memory() -> MemoryStats {
    std::fs::read_to_string("/proc/meminfo")
      .map(|t| super::parse_meminfo(&t))
      .unwrap_or_default()
  }

  /// amdgpu's `gpu_busy_percent` files, by card.
  fn amdgpu() -> Vec<GpuDevice> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
      return Vec::new();
    };
    let mut cards: Vec<String> = entries
      .flatten()
      .filter_map(|e| e.file_name().into_string().ok())
      .filter(|n| {
        n.strip_prefix("card").is_some_and(|d| {
          !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
        })
      })
      .collect();
    cards.sort();
    cards
      .into_iter()
      .filter_map(|card| {
        let device = format!("/sys/class/drm/{card}/device");
        let busy =
          std::fs::read_to_string(format!("{device}/gpu_busy_percent")).ok()?;
        let name = std::fs::read_to_string(format!("{device}/product_name"))
          .ok()
          .map(|n| n.trim().to_string())
          .filter(|n| !n.is_empty() && !n.chars().any(char::is_control))
          .unwrap_or(card);
        Some(GpuDevice {
          name,
          utilization: super::parse_busy_percent(&busy),
        })
      })
      .collect()
  }

  type NvmlDevice = *mut std::ffi::c_void;

  /// `nvmlUtilization_t`; only `gpu` is read.
  #[allow(dead_code)]
  #[repr(C)]
  struct NvmlUtilization {
    gpu: u32,
    memory: u32,
  }

  type InitFn = unsafe extern "C" fn() -> i32;
  type CountFn = unsafe extern "C" fn(*mut u32) -> i32;
  type HandleFn = unsafe extern "C" fn(u32, *mut NvmlDevice) -> i32;
  type UtilizationFn =
    unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> i32;
  type NameFn = unsafe extern "C" fn(NvmlDevice, *mut libc::c_char, u32) -> i32;

  /// NVIDIA's management library, loaded once when the driver installed it.
  struct Nvml {
    get_count: CountFn,
    get_handle: HandleFn,
    get_utilization: UtilizationFn,
    get_name: NameFn,
  }

  /// Loaded and initialized once per process; a failure is cached too.
  fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML
      .get_or_init(|| {
        // SAFETY: dlopen / dlsym with NUL-terminated names; the library is
        // never unloaded, so the symbols stay valid for the process's life,
        // and each symbol has NVML's documented C signature.
        unsafe {
          let lib = libc::dlopen(
            c"libnvidia-ml.so.1".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
          );
          if lib.is_null() {
            return None;
          }
          let sym = |name: &std::ffi::CStr| {
            let p = libc::dlsym(lib, name.as_ptr());
            (!p.is_null()).then_some(p)
          };
          let init = std::mem::transmute::<*mut libc::c_void, InitFn>(sym(
            c"nvmlInit_v2",
          )?);
          let nvml = Nvml {
            get_count: std::mem::transmute::<*mut libc::c_void, CountFn>(sym(
              c"nvmlDeviceGetCount_v2",
            )?),
            get_handle: std::mem::transmute::<*mut libc::c_void, HandleFn>(
              sym(c"nvmlDeviceGetHandleByIndex_v2")?,
            ),
            get_utilization: std::mem::transmute::<
              *mut libc::c_void,
              UtilizationFn,
            >(sym(
              c"nvmlDeviceGetUtilizationRates",
            )?),
            get_name: std::mem::transmute::<*mut libc::c_void, NameFn>(sym(
              c"nvmlDeviceGetName",
            )?),
          };
          (init() == 0).then_some(nvml)
        }
      })
      .as_ref()
  }

  fn nvidia() -> Vec<GpuDevice> {
    let Some(nvml) = nvml() else {
      return Vec::new();
    };
    let mut count = 0u32;
    // SAFETY: NVML's C API with valid out-pointers; the name buffer's size
    // is passed in and NVML NUL-terminates what it writes.
    unsafe {
      if (nvml.get_count)(&mut count) != 0 {
        return Vec::new();
      }
      (0..count.min(64))
        .filter_map(|i| {
          let mut device: NvmlDevice = std::ptr::null_mut();
          if (nvml.get_handle)(i, &mut device) != 0 {
            return None;
          }
          let mut util = NvmlUtilization { gpu: 0, memory: 0 };
          let utilization = ((nvml.get_utilization)(device, &mut util) == 0)
            .then(|| super::percent(f64::from(util.gpu)))
            .flatten();
          let mut name = [0 as libc::c_char; 96];
          let name = if (nvml.get_name)(device, name.as_mut_ptr(), 96) == 0 {
            std::ffi::CStr::from_ptr(name.as_ptr())
              .to_string_lossy()
              .into_owned()
          } else {
            format!("NVIDIA GPU {i}")
          };
          Some(GpuDevice { name, utilization })
        })
        .collect()
    }
  }

  pub fn gpu() -> GpuStats {
    let mut devices = amdgpu();
    devices.extend(nvidia());
    super::gpu_from_devices(devices)
  }
}

#[cfg(target_os = "macos")]
mod os {
  use std::ffi::c_char;
  use std::ffi::c_void;
  use std::sync::OnceLock;

  use super::GpuDevice;
  use super::GpuStats;
  use super::MemoryStats;

  /// One `[user, system, idle, nice]` per CPU.
  pub type Cpu = Vec<[u32; 4]>;

  type MachPort = u32;
  type KernReturn = i32;
  type CFTypeRef = *const c_void;

  const KERN_SUCCESS: KernReturn = 0;
  const PROCESSOR_CPU_LOAD_INFO: i32 = 2;
  const CPU_STATE_MAX: usize = 4;
  const HOST_VM_INFO64: i32 = 4;
  const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
  const K_CF_NUMBER_SINT64_TYPE: isize = 4;

  /// `vm_statistics64` (mach/vm_statistics.h); most fields mirror the C
  /// layout and are never read.
  #[allow(dead_code)]
  #[repr(C, align(8))]
  #[derive(Default)]
  struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
  }

  unsafe extern "C" {
    static mach_task_self_: MachPort;
    fn mach_host_self() -> MachPort;
    fn host_processor_info(
      host: MachPort,
      flavor: i32,
      out_processor_count: *mut u32,
      out_processor_info: *mut *mut i32,
      out_processor_info_count: *mut u32,
    ) -> KernReturn;
    fn host_statistics64(
      host: MachPort,
      flavor: i32,
      info: *mut i32,
      count: *mut u32,
    ) -> KernReturn;
    fn vm_deallocate(task: MachPort, address: usize, size: usize)
    -> KernReturn;
  }

  #[link(name = "IOKit", kind = "framework")]
  unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
      main_port: MachPort,
      matching: *mut c_void,
      existing: *mut MachPort,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: MachPort) -> MachPort;
    fn IOObjectRelease(object: MachPort) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
      entry: MachPort,
      key: CFTypeRef,
      allocator: CFTypeRef,
      options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntryGetName(entry: MachPort, name: *mut c_char)
    -> KernReturn;
  }

  #[link(name = "CoreFoundation", kind = "framework")]
  unsafe extern "C" {
    fn CFStringCreateWithCString(
      allocator: CFTypeRef,
      cstr: *const c_char,
      encoding: u32,
    ) -> CFTypeRef;
    fn CFStringGetCString(
      string: CFTypeRef,
      buffer: *mut c_char,
      size: isize,
      encoding: u32,
    ) -> u8;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, out: *mut i64) -> u8;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFRelease(cf: CFTypeRef);
  }

  /// One send right for the process's life (each `mach_host_self` call adds
  /// a reference).
  fn host() -> MachPort {
    static HOST: OnceLock<MachPort> = OnceLock::new();
    // SAFETY: no arguments; returns the host port.
    *HOST.get_or_init(|| unsafe { mach_host_self() })
  }

  pub fn cpu() -> Option<Cpu> {
    let mut count = 0u32;
    let mut info: *mut i32 = std::ptr::null_mut();
    let mut info_count = 0u32;
    // SAFETY: valid out-pointers; on success the kernel hands us an array
    // of `info_count` integers, which we copy and then deallocate.
    unsafe {
      if host_processor_info(
        host(),
        PROCESSOR_CPU_LOAD_INFO,
        &mut count,
        &mut info,
        &mut info_count,
      ) != KERN_SUCCESS
        || info.is_null()
      {
        return None;
      }
      let len = (count as usize * CPU_STATE_MAX).min(info_count as usize);
      let ticks = std::slice::from_raw_parts(info, len);
      let cpus = ticks
        .chunks_exact(CPU_STATE_MAX)
        .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32, c[3] as u32])
        .collect();
      vm_deallocate(
        mach_task_self_,
        info as usize,
        info_count as usize * std::mem::size_of::<i32>(),
      );
      Some(cpus)
    }
  }

  pub fn cpu_between(
    prev: &[[u32; 4]],
    next: &[[u32; 4]],
  ) -> (Option<f64>, Vec<f64>) {
    super::mach_cpu_between(prev, next)
  }

  fn sysctl<T: Default + Copy>(name: &std::ffi::CStr) -> Option<T> {
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: a NUL-terminated name and a buffer of `size` bytes.
    let rc = unsafe {
      libc::sysctlbyname(
        name.as_ptr(),
        (&mut value as *mut T).cast(),
        &mut size,
        std::ptr::null_mut(),
        0,
      )
    };
    (rc == 0 && size == std::mem::size_of::<T>()).then_some(value)
  }

  pub fn memory() -> MemoryStats {
    let pressure = sysctl::<i32>(c"kern.memorystatus_level")
      .and_then(super::mac_memory_pressure);
    let total = sysctl::<u64>(c"hw.memsize").filter(|t| *t > 0);
    let mut vm = VmStatistics64::default();
    let mut count = (std::mem::size_of::<VmStatistics64>()
      / std::mem::size_of::<i32>()) as u32;
    // SAFETY: a buffer of `count` integers for HOST_VM_INFO64.
    let vm_ok = unsafe {
      host_statistics64(
        host(),
        HOST_VM_INFO64,
        (&mut vm as *mut VmStatistics64).cast(),
        &mut count,
      )
    } == KERN_SUCCESS;
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // Activity Monitor's "Memory Used": app memory (anonymous pages that
    // can't be purged) + wired + compressed.
    let used = (vm_ok && page > 0).then(|| {
      let pages = u64::from(vm.internal_page_count)
        .saturating_sub(u64::from(vm.purgeable_count))
        + u64::from(vm.wire_count)
        + u64::from(vm.compressor_page_count);
      pages * page as u64
    });
    MemoryStats {
      pressure,
      used_bytes: used.map(|u| total.map_or(u, |t| u.min(t)) as f64),
      total_bytes: total.map(|t| t as f64),
    }
  }

  /// A CFString we created (released on drop).
  struct CfString(CFTypeRef);

  impl CfString {
    fn new(s: &std::ffi::CStr) -> Option<Self> {
      // SAFETY: a NUL-terminated UTF-8 string.
      let r = unsafe {
        CFStringCreateWithCString(
          std::ptr::null(),
          s.as_ptr(),
          K_CF_STRING_ENCODING_UTF8,
        )
      };
      (!r.is_null()).then_some(Self(r))
    }
  }

  impl Drop for CfString {
    fn drop(&mut self) {
      // SAFETY: we own this reference.
      unsafe { CFRelease(self.0) }
    }
  }

  /// A CF property copied from the registry (released on drop).
  struct CfOwned(CFTypeRef);

  impl Drop for CfOwned {
    fn drop(&mut self) {
      // SAFETY: a +1 reference from IORegistryEntryCreateCFProperty.
      unsafe { CFRelease(self.0) }
    }
  }

  fn property(entry: MachPort, key: &std::ffi::CStr) -> Option<CfOwned> {
    let key = CfString::new(key)?;
    // SAFETY: a live registry entry and a CFString key.
    let value = unsafe {
      IORegistryEntryCreateCFProperty(entry, key.0, std::ptr::null(), 0)
    };
    (!value.is_null()).then_some(CfOwned(value))
  }

  /// The `PerformanceStatistics` figure of one accelerator.
  fn utilization(entry: MachPort) -> Option<f64> {
    let stats = property(entry, c"PerformanceStatistics")?;
    // SAFETY: CF calls on a live dictionary; values are borrowed from it
    // and only read while it is held.
    unsafe {
      if CFGetTypeID(stats.0) != CFDictionaryGetTypeID() {
        return None;
      }
      super::performance_statistics_utilization(|key| {
        let key = std::ffi::CString::new(key).ok()?;
        let key = CfString::new(&key)?;
        let value = CFDictionaryGetValue(stats.0, key.0);
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
          return None;
        }
        let mut out = 0i64;
        (CFNumberGetValue(value, K_CF_NUMBER_SINT64_TYPE, &mut out) != 0)
          .then_some(out)
      })
    }
  }

  /// A readable name: the entry's `model` (a string on Apple silicon, data
  /// on the PCI GPUs of Intel Macs), else its registry name.
  fn device_name(entry: MachPort) -> String {
    let clean = |s: String| {
      let s = s.trim_matches(char::from(0)).trim().to_string();
      (!s.is_empty() && !s.chars().any(char::is_control)).then_some(s)
    };
    if let Some(model) = property(entry, c"model") {
      // SAFETY: CF calls on a live property.
      let name = unsafe {
        let id = CFGetTypeID(model.0);
        if id == CFStringGetTypeID() {
          let mut buf = [0 as c_char; 256];
          (CFStringGetCString(
            model.0,
            buf.as_mut_ptr(),
            buf.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
          ) != 0)
            .then(|| {
              std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned()
            })
        } else if id == CFDataGetTypeID() {
          let len = CFDataGetLength(model.0).clamp(0, 256) as usize;
          let ptr = CFDataGetBytePtr(model.0);
          (!ptr.is_null()).then(|| {
            String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len))
              .into_owned()
          })
        } else {
          None
        }
      };
      if let Some(name) = name.and_then(clean) {
        return name;
      }
    }
    let mut buf = [0 as c_char; 128];
    // SAFETY: io_name_t is 128 bytes.
    if unsafe { IORegistryEntryGetName(entry, buf.as_mut_ptr()) }
      == KERN_SUCCESS
    {
      // SAFETY: NUL-terminated by IOKit.
      let name = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
      if let Some(name) = clean(name) {
        return name;
      }
    }
    "GPU".to_string()
  }

  pub fn gpu() -> GpuStats {
    let mut devices = Vec::new();
    // SAFETY: IOServiceMatching's dictionary is consumed by
    // IOServiceGetMatchingServices; every object the iterator returns, and
    // the iterator, are released.
    unsafe {
      let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
      if matching.is_null() {
        return GpuStats::default();
      }
      let mut iter: MachPort = 0;
      if IOServiceGetMatchingServices(0, matching, &mut iter) != KERN_SUCCESS {
        return GpuStats::default();
      }
      loop {
        let entry = IOIteratorNext(iter);
        if entry == 0 {
          break;
        }
        if devices.len() < 16 {
          devices.push(GpuDevice {
            name: device_name(entry),
            utilization: utilization(entry),
          });
        }
        IOObjectRelease(entry);
      }
      IOObjectRelease(iter);
    }
    super::gpu_from_devices(devices)
  }
}

#[cfg(windows)]
mod os {
  use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
  use windows_sys::Wdk::System::SystemInformation::SystemProcessorPerformanceInformation;
  use windows_sys::Win32::Foundation::FILETIME;
  use windows_sys::Win32::System::Performance::PDH_FMT_COUNTERVALUE_ITEM_W;
  use windows_sys::Win32::System::Performance::PDH_FMT_DOUBLE;
  use windows_sys::Win32::System::Performance::PDH_MORE_DATA;
  use windows_sys::Win32::System::Performance::PdhAddEnglishCounterW;
  use windows_sys::Win32::System::Performance::PdhCloseQuery;
  use windows_sys::Win32::System::Performance::PdhCollectQueryData;
  use windows_sys::Win32::System::Performance::PdhGetFormattedCounterArrayW;
  use windows_sys::Win32::System::Performance::PdhOpenQueryW;
  use windows_sys::Win32::System::SystemInformation::GlobalMemoryStatusEx;
  use windows_sys::Win32::System::SystemInformation::MEMORYSTATUSEX;
  use windows_sys::Win32::System::Threading::GetSystemTimes;
  use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION;

  use super::CpuSnapshot;
  use super::CpuTicks;
  use super::GpuStats;
  use super::MemoryStats;

  pub type Cpu = CpuSnapshot;

  const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;

  fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
  }

  /// Kernel time includes idle time.
  fn ticks(idle: u64, kernel: u64, user: u64) -> CpuTicks {
    CpuTicks {
      busy: kernel.saturating_sub(idle).saturating_add(user),
      idle,
    }
  }

  fn cores() -> Vec<CpuTicks> {
    let mut len = 64usize;
    loop {
      let mut buf: Vec<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION> =
        Vec::with_capacity(len);
      let bytes = (len
        * std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>())
        as u32;
      let mut returned = 0u32;
      // SAFETY: a buffer of `bytes` bytes for this information class.
      let status = unsafe {
        NtQuerySystemInformation(
          SystemProcessorPerformanceInformation,
          buf.as_mut_ptr().cast(),
          bytes,
          &mut returned,
        )
      };
      if status == STATUS_INFO_LENGTH_MISMATCH && len < 4096 {
        len *= 2;
        continue;
      }
      if status < 0 {
        return Vec::new();
      }
      let n = (returned as usize
        / std::mem::size_of::<SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION>())
      .min(len);
      // SAFETY: the call wrote `n` entries.
      unsafe { buf.set_len(n) };
      return buf
        .iter()
        .map(|c| {
          ticks(c.IdleTime as u64, c.KernelTime as u64, c.UserTime as u64)
        })
        .collect();
    }
  }

  pub fn cpu() -> Option<Cpu> {
    let zero = FILETIME {
      dwLowDateTime: 0,
      dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: three valid out-pointers.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    let snapshot = CpuSnapshot {
      total: ok
        .then(|| ticks(filetime(idle), filetime(kernel), filetime(user))),
      cores: cores(),
    };
    (snapshot.total.is_some() || !snapshot.cores.is_empty()).then_some(snapshot)
  }

  pub fn cpu_between(prev: &Cpu, next: &Cpu) -> (Option<f64>, Vec<f64>) {
    super::cpu_between(prev, next)
  }

  pub fn memory() -> MemoryStats {
    // SAFETY: MEMORYSTATUSEX is plain data; dwLength is set as required.
    unsafe {
      let mut status: MEMORYSTATUSEX = std::mem::zeroed();
      status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
      if GlobalMemoryStatusEx(&mut status) == 0 {
        return MemoryStats::default();
      }
      let total = status.ullTotalPhys;
      MemoryStats {
        pressure: super::percent(f64::from(status.dwMemoryLoad)),
        used_bytes: (total > 0)
          .then(|| total.saturating_sub(status.ullAvailPhys) as f64),
        total_bytes: (total > 0).then_some(total as f64),
      }
    }
  }

  /// A sampler's PDH query over `\GPU Engine(*)\Utilization Percentage`;
  /// rates need two collections, so the first `sample` primes it.
  pub struct GpuQuery {
    query: isize,
    counter: Option<isize>,
  }

  // SAFETY: PDH handles may be used from any thread; a sampler is used by
  // one blocking task at a time (behind a mutex).
  unsafe impl Send for GpuQuery {}

  impl GpuQuery {
    pub fn open() -> Self {
      let mut query = 0isize;
      // SAFETY: a valid out-pointer; a null data source is the live system.
      if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
        return Self {
          query: 0,
          counter: None,
        };
      }
      let path: Vec<u16> = r"\GPU Engine(*)\Utilization Percentage"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
      let mut counter = 0isize;
      // SAFETY: a NUL-terminated wide path and a valid out-pointer.
      let added =
        unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) }
          == 0;
      let this = Self {
        query,
        counter: added.then_some(counter),
      };
      if added {
        // SAFETY: a live query. The first collection primes the rates.
        unsafe { PdhCollectQueryData(query) };
      }
      this
    }

    pub fn sample(&mut self) -> GpuStats {
      let Some(counter) = self.counter else {
        return GpuStats::default();
      };
      // SAFETY: a live query.
      if unsafe { PdhCollectQueryData(self.query) } != 0 {
        return GpuStats::default();
      }
      // The instances can change between the size query and the read: try
      // a few times.
      let items: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = 'read: {
        for _ in 0..4 {
          let mut bytes = 0u32;
          let mut count = 0u32;
          // SAFETY: a size query (null buffer of zero bytes).
          let status = unsafe {
            PdhGetFormattedCounterArrayW(
              counter,
              PDH_FMT_DOUBLE,
              &mut bytes,
              &mut count,
              std::ptr::null_mut(),
            )
          };
          if status != PDH_MORE_DATA || bytes == 0 {
            return GpuStats::default();
          }
          let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
          let cap = (bytes as usize).div_ceil(item);
          let mut items = Vec::with_capacity(cap);
          let mut bytes = (cap * item) as u32;
          // SAFETY: a buffer of `bytes` bytes, aligned for the items; PDH
          // writes `count` items followed by the strings they point to.
          let status = unsafe {
            PdhGetFormattedCounterArrayW(
              counter,
              PDH_FMT_DOUBLE,
              &mut bytes,
              &mut count,
              items.as_mut_ptr(),
            )
          };
          if status == PDH_MORE_DATA {
            continue;
          }
          if status != 0 {
            return GpuStats::default();
          }
          // SAFETY: PDH wrote `count` items.
          unsafe { items.set_len((count as usize).min(cap)) };
          break 'read items;
        }
        Vec::new()
      };
      let named: Vec<(String, f64)> = items
        .iter()
        .filter(|i| i.FmtValue.CStatus == 0 && !i.szName.is_null())
        .map(|i| {
          // SAFETY: a NUL-terminated wide string inside `items`' buffer,
          // and the double member of a PDH_FMT_DOUBLE value.
          unsafe {
            let mut len = 0;
            while *i.szName.add(len) != 0 {
              len += 1;
            }
            (
              String::from_utf16_lossy(std::slice::from_raw_parts(
                i.szName, len,
              )),
              i.FmtValue.Anonymous.doubleValue,
            )
          }
        })
        .collect();
      GpuStats {
        utilization: super::gpu_from_engines(
          named.iter().map(|(n, v)| (n.as_str(), *v)),
        ),
        // PDH names adapters by LUID only.
        devices: None,
      }
    }
  }

  impl Drop for GpuQuery {
    fn drop(&mut self) {
      if self.query != 0 {
        // SAFETY: a query we opened.
        unsafe { PdhCloseQuery(self.query) };
      }
    }
  }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod os {
  use super::GpuStats;
  use super::MemoryStats;

  pub type Cpu = ();

  pub fn cpu() -> Option<Cpu> {
    None
  }

  pub fn cpu_between(_prev: &Cpu, _next: &Cpu) -> (Option<f64>, Vec<f64>) {
    (None, Vec::new())
  }

  pub fn memory() -> MemoryStats {
    MemoryStats::default()
  }

  pub fn gpu() -> GpuStats {
    GpuStats::default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn t(busy: u64, idle: u64) -> CpuTicks {
    CpuTicks { busy, idle }
  }

  #[test]
  fn percent_clamps_and_rounds_to_one_decimal() {
    assert_eq!(percent(12.345), Some(12.3));
    assert_eq!(percent(12.35), Some(12.4));
    assert_eq!(percent(-3.0), Some(0.0));
    assert_eq!(percent(140.0), Some(100.0));
    assert_eq!(percent(f64::NAN), None);
    assert_eq!(busy_percent(t(1, 3)), Some(25.0));
    assert_eq!(busy_percent(t(0, 0)), None);
    assert_eq!(busy_percent(t(2, 1)), Some(66.7));
  }

  #[test]
  fn cpu_between_uses_the_aggregate_else_the_cores() {
    let prev = CpuSnapshot {
      total: Some(t(100, 300)),
      cores: vec![t(50, 150), t(50, 150)],
    };
    let next = CpuSnapshot {
      total: Some(t(150, 350)),
      cores: vec![t(90, 160), t(60, 190)],
    };
    assert_eq!(cpu_between(&prev, &next), (Some(50.0), vec![80.0, 20.0]));
    // No aggregate: the cores' sum (50 busy of 100).
    let (p, n) = (
      CpuSnapshot {
        total: None,
        ..prev.clone()
      },
      CpuSnapshot {
        total: None,
        ..next.clone()
      },
    );
    assert_eq!(cpu_between(&p, &n), (Some(50.0), vec![80.0, 20.0]));
    // A counter that went backwards: unknown, never negative.
    let back = CpuSnapshot {
      total: Some(t(90, 350)),
      cores: vec![t(40, 160), t(60, 190)],
    };
    assert_eq!(cpu_between(&prev, &back), (None, vec![0.0, 20.0]));
    // No time passed.
    assert_eq!(cpu_between(&prev, &prev), (None, vec![0.0, 0.0]));
    // A core that came online between the snapshots is left out.
    let more = CpuSnapshot {
      total: Some(t(150, 350)),
      cores: vec![t(90, 160), t(60, 190), t(1, 1)],
    };
    assert_eq!(cpu_between(&prev, &more).1.len(), 2);
  }

  #[test]
  fn mach_ticks_wrap_at_32_bits() {
    // user, system, idle, nice.
    let prev = [u32::MAX - 9, 100, 1000, 0];
    let next = [10, 120, 1060, 0];
    assert_eq!(mach_ticks_between(prev, next), t(20 + 20, 60));
    let (total, cores) =
      mach_cpu_between(&[prev, [0, 0, 0, 0]], &[next, [10, 10, 80, 0]]);
    // (40 + 20) busy of (40 + 60 + 20 + 80).
    assert_eq!(total, Some(30.0));
    assert_eq!(cores, vec![40.0, 20.0]);
    assert_eq!(mach_cpu_between(&[], &[]), (None, vec![]));
  }

  #[test]
  fn proc_stat_parses_the_aggregate_and_each_core() {
    let text = "cpu  100 5 50 800 20 3 2 1 9 0\n\
                cpu0 50 2 25 400 10 1 1 0 4 0\n\
                cpu1 50 3 25 400 10 2 1 1 5 0\n\
                intr 12345 0 0\n\
                ctxt 999\n\
                cpufreq 1\n\
                cpu2 x 1 2 3\n";
    let s = parse_proc_stat(text);
    assert_eq!(s.total, Some(t(100 + 5 + 50 + 3 + 2 + 1, 800 + 20)));
    assert_eq!(s.cores, vec![t(50 + 2 + 25 + 1 + 1, 410), t(82, 410)]);
    // An old kernel's four columns.
    let s = parse_proc_stat("cpu 1 2 3 4\n");
    assert_eq!(s.total, Some(t(6, 4)));
    assert!(s.cores.is_empty());
    let s = parse_proc_stat("cpu 1 2\n");
    assert_eq!(s.total, None);
    assert_eq!(parse_proc_stat(""), CpuSnapshot::default());
  }

  #[test]
  fn meminfo_gives_pressure_and_bytes() {
    let text = "MemTotal:       16000000 kB\n\
                MemFree:         1000000 kB\n\
                MemAvailable:    4000000 kB\n\
                Buffers:          100000 kB\n";
    assert_eq!(
      parse_meminfo(text),
      MemoryStats {
        pressure: Some(75.0),
        used_bytes: Some(12_000_000.0 * 1024.0),
        total_bytes: Some(16_000_000.0 * 1024.0),
      }
    );
    // No MemAvailable (a kernel before 3.14): unknown.
    assert_eq!(
      parse_meminfo("MemTotal: 100 kB\nMemFree: 50 kB\n"),
      MemoryStats::default()
    );
    assert_eq!(
      parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n"),
      MemoryStats::default()
    );
    // MemAvailableX is not MemAvailable.
    assert_eq!(
      parse_meminfo("MemTotal: 100 kB\nMemAvailableX: 1 kB\n").pressure,
      None
    );
  }

  #[test]
  fn mac_memory_pressure_is_100_minus_the_level() {
    assert_eq!(mac_memory_pressure(70), Some(30.0));
    assert_eq!(mac_memory_pressure(0), Some(100.0));
    assert_eq!(mac_memory_pressure(100), Some(0.0));
    assert_eq!(mac_memory_pressure(-1), None);
    assert_eq!(mac_memory_pressure(101), None);
  }

  #[test]
  fn performance_statistics_prefers_device_utilization() {
    let apple = |k: &str| (k == "Device Utilization %").then_some(42);
    assert_eq!(performance_statistics_utilization(apple), Some(42.0));
    let amd = |k: &str| (k == "GPU Activity(%)").then_some(7);
    assert_eq!(performance_statistics_utilization(amd), Some(7.0));
    let both = |k: &str| match k {
      "Device Utilization %" => Some(10),
      "GPU Activity(%)" => Some(90),
      _ => None,
    };
    assert_eq!(performance_statistics_utilization(both), Some(10.0));
    assert_eq!(performance_statistics_utilization(|_| None), None);
    assert_eq!(
      performance_statistics_utilization(|_| Some(250)),
      Some(100.0)
    );
  }

  #[test]
  fn gpu_takes_the_busiest_device() {
    let dev = |name: &str, u: Option<f64>| GpuDevice {
      name: name.to_string(),
      utilization: u,
    };
    let gpu = gpu_from_devices(vec![
      dev("card0", Some(12.0)),
      dev("NVIDIA GeForce", Some(64.0)),
      dev("card1", None),
    ]);
    assert_eq!(gpu.utilization, Some(64.0));
    assert_eq!(gpu.devices.as_ref().map(Vec::len), Some(3));
    let gpu = gpu_from_devices(vec![dev("card0", None)]);
    assert_eq!(gpu.utilization, None);
    assert!(gpu.devices.is_some());
    assert_eq!(gpu_from_devices(vec![]), GpuStats::default());
    assert_eq!(parse_busy_percent("37\n"), Some(37.0));
    assert_eq!(parse_busy_percent(" "), None);
    assert_eq!(parse_busy_percent("busy"), None);
    assert_eq!(parse_busy_percent("inf"), None);
  }

  #[test]
  fn pdh_gpu_engines_sum_per_adapter_and_type() {
    let a3d = "pid_100_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D";
    let b3d = "pid_200_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D";
    let copy = "pid_200_luid_0x00000000_0x0000D1F2_phys_0_eng_1_engtype_Copy";
    let other =
      "pid_300_luid_0x00000000_0x0000AAAA_phys_0_eng_4_engtype_VideoDecode";
    assert_eq!(
      gpu_engine_key(a3d),
      Some(("luid_0x00000000_0x0000D1F2_phys_0", "3D"))
    );
    assert_eq!(gpu_engine_key("_Total"), None);
    assert_eq!(
      gpu_from_engines([(a3d, 30.0), (b3d, 25.5), (copy, 70.0), (other, 5.0)]),
      Some(70.0)
    );
    assert_eq!(gpu_from_engines([(a3d, 30.0), (b3d, 25.5)]), Some(55.5));
    // Sums past 100 (rounding across processes) are clamped.
    assert_eq!(gpu_from_engines([(a3d, 80.0), (b3d, 40.0)]), Some(100.0));
    assert_eq!(gpu_from_engines([("_Total", 9.0)]), None);
    assert_eq!(gpu_from_engines(std::iter::empty()), None);
    assert_eq!(gpu_from_engines([(a3d, f64::NAN)]), None);
  }

  #[test]
  fn stats_serialize_in_the_api_shape() {
    let stats = SystemStats {
      at: 1.0,
      cpu: CpuStats {
        total: Some(5.0),
        cores: None,
      },
      gpu: GpuStats::default(),
      memory: MemoryStats {
        pressure: None,
        used_bytes: None,
        total_bytes: Some(8.0),
      },
    };
    assert_eq!(
      deno_core::serde_json::to_value(&stats).unwrap(),
      deno_core::serde_json::json!({
        "at": 1.0,
        "cpu": { "total": 5.0 },
        "gpu": { "utilization": null },
        "memory": { "pressure": null, "totalBytes": 8.0 },
      })
    );
  }

  /// The OS calls on this host: the shape and the ranges (a machine may
  /// have no readable GPU, so its figure may be null).
  #[test]
  fn sampler_smoke_test_on_this_host() {
    let in_range =
      |v: Option<f64>| v.is_none_or(|v| (0.0..=100.0).contains(&v));
    let mut sampler = Sampler::default();
    let first = sampler.sample(true);
    let second = sampler.sample(false);
    for s in [&first, &second] {
      assert!(s.at > 1.6e12, "{s:?}");
      assert!(in_range(s.cpu.total), "{s:?}");
      assert!(in_range(s.gpu.utilization), "{s:?}");
      assert!(in_range(s.memory.pressure), "{s:?}");
      for d in s.gpu.devices.iter().flatten() {
        assert!(in_range(d.utilization), "{d:?}");
        assert!(!d.name.is_empty());
      }
    }
    assert!(second.cpu.cores.is_none());
    if SUPPORTED {
      // The first sample primed itself, so it has a CPU figure.
      assert!(first.cpu.total.is_some(), "{first:?}");
      let cores = first.cpu.cores.as_ref().unwrap();
      assert!(!cores.is_empty());
      assert!(cores.iter().all(|c| (0.0..=100.0).contains(c)));
      assert!(first.memory.pressure.is_some(), "{first:?}");
      assert!(first.memory.total_bytes.is_some_and(|t| t > 0.0));
      assert!(
        first
          .memory
          .used_bytes
          .is_some_and(|u| u <= first.memory.total_bytes.unwrap())
      );
    }
  }
}
