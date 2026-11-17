// Copyright 2018-2026 the Deno authors. MIT license.

mod deno_dir;
mod disk_cache;
mod emit;
#[cfg(feature = "deno_ast")]
mod parsed_source;

pub use deno_dir::DenoDir;
pub use deno_dir::DenoDirOptions;
pub use deno_dir::DenoDirProvider;
pub use deno_dir::DenoDirProviderRc;
pub use deno_dir::DenoDirSys;
pub use deno_dir::SANDBOX_OVERLAY_DIR;
pub use deno_dir::is_sandbox_overlay_path;
pub use deno_dir::set_sandbox_overlay;
pub use disk_cache::DiskCache;
pub use disk_cache::DiskCacheSys;
pub use emit::EmitCache;
pub use emit::EmitCacheRc;
pub use emit::EmitCacheSys;
#[cfg(feature = "deno_ast")]
pub use parsed_source::LazyGraphSourceParser;
#[cfg(feature = "deno_ast")]
pub use parsed_source::ParsedSourceCache;
#[cfg(feature = "deno_ast")]
pub use parsed_source::ParsedSourceCacheRc;
