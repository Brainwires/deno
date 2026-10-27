// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use capacity_builder::BytesAppendable;
use deno_ast::MediaType;
use deno_ast::ModuleKind;
use deno_ast::ModuleSpecifier;
use deno_cache_dir::CACHE_PERM;
use deno_core::anyhow::Context;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::url::Url;
use deno_graph::ModuleGraph;
use deno_lib::args::CaData;
use deno_lib::args::UnstableConfig;
use deno_lib::shared::ReleaseChannel;
use deno_lib::standalone::binary::CjsExportAnalysisEntry;
use deno_lib::standalone::binary::InitialWindowConfig;
use deno_lib::standalone::binary::MAGIC_BYTES;
use deno_lib::standalone::binary::Metadata;
use deno_lib::standalone::binary::NodeModules;
use deno_lib::standalone::binary::RemoteModuleEntry;
use deno_lib::standalone::binary::SerializedResolverWorkspaceJsrPackage;
use deno_lib::standalone::binary::SerializedWorkspaceResolver;
use deno_lib::standalone::binary::SerializedWorkspaceResolverImportMap;
use deno_lib::standalone::binary::SpecifierDataStore;
use deno_lib::standalone::binary::SpecifierId;
use deno_lib::standalone::virtual_fs::BuiltVfs;
use deno_lib::standalone::virtual_fs::DENO_COMPILE_GLOBAL_NODE_MODULES_DIR_NAME;
use deno_lib::standalone::virtual_fs::OffsetWithLength;
use deno_lib::standalone::virtual_fs::VfsBuilder;
use deno_lib::standalone::virtual_fs::VfsEntry;
use deno_lib::standalone::virtual_fs::VirtualDirectory;
use deno_lib::standalone::virtual_fs::VirtualDirectoryEntries;
use deno_lib::standalone::virtual_fs::WindowsSystemRootablePath;
use deno_lib::util::hash::FastInsecureHasher;
use deno_lib::util::text_encoding::is_valid_utf8;
use deno_lib::util::v8::construct_v8_flags;
use deno_lib::version::DENO_VERSION_INFO;
use deno_lib::version::UPSTREAM_DENO_VERSION;
use deno_npm::NpmSystemInfo;
use deno_npm::resolution::SerializedNpmResolutionSnapshot;
use deno_npm::resolution::ValidSerializedNpmResolutionSnapshot;
use deno_path_util::fs::atomic_write_file_with_retries;
use deno_path_util::url_from_directory_path;
use deno_path_util::url_to_file_path;
use deno_resolver::file_fetcher::FetchLocalOptions;
use deno_resolver::file_fetcher::FetchOptions;
use deno_resolver::file_fetcher::FetchPermissionsOptionRef;
use deno_resolver::loader::GraphCjsAnalysisSourceProvider;
use deno_resolver::workspace::WorkspaceResolver;
use deno_semver::npm::NpmPackageReqReference;
use indexmap::IndexMap;
use node_resolver::analyze::CjsAnalysisSourceProvider;
use node_resolver::analyze::ResolvedCjsAnalysis;
use sys_traits::FsRead;

use super::virtual_fs::output_vfs;
use crate::args::CliOptions;
use crate::args::CompileFlags;
use crate::args::JavaScriptEngine;
use crate::args::get_default_v8_flags;
use crate::args::resolve_compile_target;
use crate::cache::DenoDir;
use crate::file_fetcher::CliFileFetcher;
use crate::http_util::HttpClientProvider;
use crate::module_loader::CliEmitter;
use crate::node::CliCjsModuleExportAnalyzer;
use crate::npm::CliNpmResolver;
use crate::resolver::CliCjsTracker;
use crate::sys::CliSys;
use crate::util::archive;
use crate::util::env::handle_dotenv_error;
use crate::util::env::handle_dotenv_io_error;
use crate::util::env::handle_dotenv_not_found;
use crate::util::progress_bar::ProgressBar;
use crate::util::progress_bar::ProgressBarStyle;
use crate::util::progress_bar::ProgressMessagePrompt;

/// A URL that can be designated as the base for relative URLs.
///
/// After creation, this URL may be used to get the key for a
/// module in the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandaloneRelativeFileBaseUrl<'a> {
  WindowsSystemRoot,
  Path(&'a Url),
}

impl StandaloneRelativeFileBaseUrl<'_> {
  /// Gets the module map key of the provided specifier.
  ///
  /// * Descendant file specifiers will be made relative to the base.
  /// * Non-descendant file specifiers will stay as-is (absolute).
  /// * Non-file specifiers will stay as-is.
  pub fn specifier_key<'b>(&self, target: &'b Url) -> Cow<'b, str> {
    if target.scheme() != "file" {
      return Cow::Borrowed(target.as_str());
    }
    let base = match self {
      Self::Path(base) => base,
      Self::WindowsSystemRoot => return Cow::Borrowed(target.path()),
    };

    match base.make_relative(target) {
      Some(relative) => {
        // This is not a great scenario to have because it means that the
        // specifier is outside the vfs and could cause the binary to act
        // strangely. If you encounter this, the fix is to add more paths
        // to the vfs builder by calling `add_possible_min_root_dir`.
        debug_assert!(
          !relative.starts_with("../"),
          "{} -> {} ({})",
          base.as_str(),
          target.as_str(),
          relative,
        );
        Cow::Owned(relative)
      }
      None => Cow::Borrowed(target.as_str()),
    }
  }
}

struct VfsCjsAnalysisSourceProvider<'a> {
  vfs: &'a VfsBuilder,
  sources: &'a HashMap<Url, OffsetWithLength>,
  fallback: Option<&'a dyn CjsAnalysisSourceProvider>,
}

impl CjsAnalysisSourceProvider for VfsCjsAnalysisSourceProvider<'_> {
  fn load_source<'a>(&'a self, specifier: &Url) -> Option<Cow<'a, str>> {
    if let Some(offset) = self.sources.get(specifier).copied()
      && let Some(bytes) = self.vfs.file_bytes(offset)
      && let Ok(source) = std::str::from_utf8(bytes)
    {
      return Some(Cow::Borrowed(source));
    }
    self
      .fallback
      .and_then(|provider| provider.load_source(specifier))
  }
}

// `deno compile` is a trusted local build step, so sources absent from the
// graph and VFS may be read directly to preserve compile-time CJS analysis.
struct FileSystemCjsAnalysisSourceProvider<'a> {
  sys: &'a CliSys,
}

impl CjsAnalysisSourceProvider for FileSystemCjsAnalysisSourceProvider<'_> {
  fn load_source<'a>(&'a self, specifier: &Url) -> Option<Cow<'a, str>> {
    let path = deno_path_util::url_to_file_path(specifier).ok()?;
    self
      .sys
      .fs_read_to_string_lossy(path)
      .ok()
      .map(|source| Cow::Owned(source.into_owned()))
  }
}

fn collect_vfs_cjs_analysis_sources(
  vfs: &VfsBuilder,
) -> HashMap<Url, OffsetWithLength> {
  let mut sources = HashMap::new();
  for (path, file) in vfs.iter_files() {
    let Ok(specifier) = deno_path_util::url_from_file_path(&path) else {
      continue;
    };
    sources.insert(specifier, file.offset);
  }
  sources
}

struct SpecifierStore<'a> {
  data: IndexMap<&'a Url, SpecifierId>,
}

impl<'a> SpecifierStore<'a> {
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      data: IndexMap::with_capacity(capacity),
    }
  }

  pub fn get_or_add(&mut self, specifier: &'a Url) -> SpecifierId {
    let len = self.data.len();
    let entry = self.data.entry(specifier);
    match entry {
      indexmap::map::Entry::Occupied(occupied_entry) => *occupied_entry.get(),
      indexmap::map::Entry::Vacant(vacant_entry) => {
        let new_id = SpecifierId::new(len as u32);
        vacant_entry.insert(new_id);
        new_id
      }
    }
  }

  pub fn for_serialization(
    self,
    base_url: &StandaloneRelativeFileBaseUrl<'a>,
  ) -> SpecifierStoreForSerialization<'a> {
    SpecifierStoreForSerialization {
      data: self
        .data
        .into_iter()
        .map(|(specifier, id)| (base_url.specifier_key(specifier), id))
        .collect(),
    }
  }
}

struct SpecifierStoreForSerialization<'a> {
  data: Vec<(Cow<'a, str>, SpecifierId)>,
}

impl<'a> BytesAppendable<'a> for &'a SpecifierStoreForSerialization<'a> {
  fn append_to_builder<TBytes: capacity_builder::BytesType>(
    self,
    builder: &mut capacity_builder::BytesBuilder<'a, TBytes>,
  ) {
    builder.append_le(self.data.len() as u32);
    for (specifier_str, id) in &self.data {
      builder.append_le(specifier_str.len() as u32);
      builder.append(specifier_str.as_ref());
      builder.append(*id);
    }
  }
}

/// Given a canonical npm package folder (e.g.
/// `<.deno>/<id>/node_modules/@scope/name`), walk up to the enclosing
/// `node_modules/` directory. Embedding from there picks up sibling
/// symlinks the deno linker creates for direct dependencies, which the
/// canonical folder itself doesn't contain.
fn pkg_folder_node_modules_root(folder: &Path) -> Option<&Path> {
  let mut current = folder.parent()?;
  loop {
    if current.file_name() == Some(std::ffi::OsStr::new("node_modules")) {
      return Some(current);
    }
    current = current.parent()?;
  }
}

pub fn is_standalone_binary(exe_path: &Path) -> bool {
  let Ok(data) = std::fs::read(exe_path) else {
    return false;
  };
  libsui::utils::is_elf(&data)
    || libsui::utils::is_pe(&data)
    || libsui::utils::is_macho(&data)
}

/// Validate a user-provided `--app-name`. The name becomes a single directory
/// component under the platform's app data directory at runtime. Because a
/// binary can be cross-compiled, validate against the union of what every
/// target OS allows so the baked identity resolves to one unambiguous
/// directory component everywhere, rather than escaping the directory or
/// failing on the target's filesystem. Done here so the user gets a clear
/// compile-time error instead of a surprising (or unusable) store location.
fn validate_app_name(app_name: &str) -> Result<(), AnyError> {
  const RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6",
    "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9",
  ];

  // Windows reserved device names match case-insensitively against the portion
  // before the first `.` (so `nul`, `NUL`, and `nul.txt` all match).
  let stem = app_name.split('.').next().unwrap_or(app_name);
  let is_reserved_name =
    RESERVED_NAMES.iter().any(|n| stem.eq_ignore_ascii_case(n));

  let reason = if app_name.is_empty() {
    Some("must not be empty")
  } else if app_name == "." || app_name == ".." {
    Some("must not be `.` or `..`")
  } else if app_name
    // `/` and `\` are path separators; `<>:"|?*` are reserved on Windows;
    // control characters are rejected by the filesystem.
    .contains(|c: char| {
      matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
        || c.is_control()
    })
  {
    Some("must not contain path separators or any of `<>:\"|?*`")
  } else if app_name.ends_with('.')
    || app_name.ends_with(' ')
    || app_name.starts_with(' ')
  {
    // Windows silently strips trailing dots and spaces, which would change the
    // identity out from under the user; a leading space is an error-prone
    // directory name everywhere, so reject it too.
    Some("must not start or end with a space, or end with a `.`")
  } else if is_reserved_name {
    Some("must not be a reserved device name (e.g. `CON`, `NUL`, `COM1`)")
  } else {
    None
  };

  if let Some(reason) = reason {
    bail!("Invalid `--app-name` value {:?}: {}.", app_name, reason);
  }
  Ok(())
}

fn default_app_name(display_output_filename: &str, is_desktop: bool) -> String {
  if is_desktop {
    // A desktop build's compile output is an intermediate shared library. The
    // final app is packaged without this platform-specific extension, so keep
    // its baked identity (and default window title) in sync with that name.
    Path::new(display_output_filename)
      .file_stem()
      .and_then(|stem| stem.to_str())
      .unwrap_or(display_output_filename)
      .to_string()
  } else {
    display_output_filename
      .strip_suffix(".exe")
      .unwrap_or(display_output_filename)
      .to_string()
  }
}

/// Resolve the stable app identity baked into a compiled binary: an explicit
/// `--app-name`, otherwise the output file name (minus the executable extension
/// added by Deno). The derived default is held to the same rules as an explicit
/// flag, since it becomes a single directory component at runtime (possibly on
/// a different target OS when cross-compiling); otherwise an output name like
/// `aux` or one with a trailing dot would silently break persistent storage on
/// the target.
fn resolve_app_name(
  compile_flags: &CompileFlags,
  display_output_filename: &str,
  is_desktop: bool,
) -> Result<String, AnyError> {
  let app_name = compile_flags
    .app_name
    .clone()
    .unwrap_or_else(|| default_app_name(display_output_filename, is_desktop));
  validate_app_name(&app_name)?;
  Ok(app_name)
}

/// Resolve the desktop app's page origin from `desktop.app.origin`, validated
/// and normalized so the runtime can register the scheme and compare `Origin`
/// headers against it byte-for-byte. `None` when unset (the runtime then uses
/// [`deno_lib::standalone::app_origin::DEFAULT_APP_ORIGIN`]).
pub fn resolve_desktop_app_origin(
  cli_options: &CliOptions,
) -> Result<Option<String>, AnyError> {
  let desktop_config = cli_options.start_dir.to_desktop_config()?;
  let Some(origin) = desktop_config
    .app
    .as_ref()
    .and_then(|app| app.origin.as_deref())
  else {
    return Ok(None);
  };
  let parsed = deno_lib::standalone::app_origin::AppOrigin::parse(origin)
    .with_context(|| {
      format!("Invalid desktop.app.origin {origin:?} in deno.json")
    })?;
  Ok(Some(parsed.as_origin_string()))
}

/// Resolve the desktop app's identifier from `desktop.app.identifier`,
/// validated as a reverse-DNS id (it names the macOS bundle, the Linux
/// `.desktop` file and the per-app web data directory). `None` when unset.
pub fn resolve_desktop_app_identifier(
  cli_options: &CliOptions,
) -> Result<Option<String>, AnyError> {
  let desktop_config = cli_options.start_dir.to_desktop_config()?;
  let Some(identifier) = desktop_config
    .app
    .as_ref()
    .and_then(|app| app.identifier.as_deref())
  else {
    return Ok(None);
  };
  deno_lib::standalone::app_id::validate_app_identifier(identifier)
    .with_context(|| {
      format!("Invalid desktop.app.identifier {identifier:?} in deno.json")
    })?;
  Ok(Some(identifier.to_string()))
}

/// Resolve the desktop app's deep-link schemes from `desktop.app.deepLinks`,
/// normalized (trimmed, lower-cased, deduplicated) and validated the same way
/// `deno desktop` registers them with the OS. Empty when unset.
pub fn resolve_desktop_deep_links(
  cli_options: &CliOptions,
) -> Result<Vec<String>, AnyError> {
  let desktop_config = cli_options.start_dir.to_desktop_config()?;
  let Some(schemes) = desktop_config
    .app
    .as_ref()
    .and_then(|app| app.deep_links.as_deref())
  else {
    return Ok(Vec::new());
  };
  deno_lib::standalone::launch_args::normalize_deep_link_schemes(schemes)
    .map_err(|e| {
      deno_core::anyhow::anyhow!(
        "Invalid desktop.app.deepLinks in deno.json: {e}"
      )
    })
}

/// The desktop app's configured origin and identifier, resolved together
/// because a configured origin requires an identifier (see
/// [`check_desktop_app_identity`]).
pub struct DesktopAppIdentity {
  /// Normalized `desktop.app.origin`, see [`resolve_desktop_app_origin`].
  pub origin: Option<String>,
  /// Validated `desktop.app.identifier`, see
  /// [`resolve_desktop_app_identifier`].
  pub identifier: Option<String>,
}

/// Resolve `desktop.app.origin` and `desktop.app.identifier`, failing when
/// either is invalid or an origin is configured without an identifier.
pub fn resolve_desktop_app_identity(
  cli_options: &CliOptions,
) -> Result<DesktopAppIdentity, AnyError> {
  let origin = resolve_desktop_app_origin(cli_options)?;
  let identifier = resolve_desktop_app_identifier(cli_options)?;
  check_desktop_app_identity(origin.as_deref(), identifier.as_deref())?;
  Ok(DesktopAppIdentity { origin, identifier })
}

/// A configured `desktop.app.origin` requires `desktop.app.identifier`.
///
/// Web storage (`localStorage`, IndexedDB, cookies) is keyed by origin inside
/// the webview's profile, and the profile is per app only when the backend is
/// told the app's identifier. Without one, two apps configured with the same
/// origin would read each other's data (the webview's shared default store),
/// so this is a build error rather than a warning.
fn check_desktop_app_identity(
  origin: Option<&str>,
  identifier: Option<&str>,
) -> Result<(), AnyError> {
  if let Some(origin) = origin
    && identifier.is_none()
  {
    bail!(
      "desktop.app.origin {origin:?} requires desktop.app.identifier in \
       deno.json. Web storage is keyed by origin, so without a per-app \
       identifier two apps with the same origin could share it. Set a \
       reverse-DNS identifier, e.g. \"desktop\": {{ \"app\": {{ \"identifier\": \
       \"com.example.myapp\" }} }}."
    );
  }
  Ok(())
}

pub struct WriteBinOptions<'a> {
  pub writer: File,
  pub display_output_filename: &'a str,
  pub graph: &'a ModuleGraph,
  pub entrypoint: &'a ModuleSpecifier,
  pub include_paths: &'a [ModuleSpecifier],
  pub exclude_paths: Vec<PathBuf>,
  pub compile_flags: &'a CompileFlags,
}

pub struct DenoCompileBinaryWriter<'a> {
  cjs_module_export_analyzer: &'a CliCjsModuleExportAnalyzer,
  cjs_tracker: &'a CliCjsTracker,
  cli_options: &'a CliOptions,
  deno_dir: &'a DenoDir,
  emitter: &'a CliEmitter,
  file_fetcher: &'a CliFileFetcher,
  http_client_provider: &'a HttpClientProvider,
  npm_resolver: &'a CliNpmResolver,
  workspace_resolver: &'a WorkspaceResolver<CliSys>,
  npm_system_info: NpmSystemInfo,
  is_desktop: bool,
}

impl<'a> DenoCompileBinaryWriter<'a> {
  #[allow(clippy::too_many_arguments, reason = "construction")]
  pub fn new(
    cjs_module_export_analyzer: &'a CliCjsModuleExportAnalyzer,
    cjs_tracker: &'a CliCjsTracker,
    cli_options: &'a CliOptions,
    deno_dir: &'a DenoDir,
    emitter: &'a CliEmitter,
    file_fetcher: &'a CliFileFetcher,
    http_client_provider: &'a HttpClientProvider,
    npm_resolver: &'a CliNpmResolver,
    workspace_resolver: &'a WorkspaceResolver<CliSys>,
    npm_system_info: NpmSystemInfo,
    is_desktop: bool,
  ) -> Self {
    Self {
      cjs_module_export_analyzer,
      cjs_tracker,
      cli_options,
      deno_dir,
      emitter,
      file_fetcher,
      http_client_provider,
      npm_resolver,
      workspace_resolver,
      npm_system_info,
      is_desktop,
    }
  }

  /// deno.json `desktop.update.publicKey` for a desktop build, checked to be
  /// an ECDSA P-256 public key (a key that does not parse would make every
  /// update fail at runtime).
  fn resolve_update_public_key(&self) -> Result<Option<String>, AnyError> {
    if !self.is_desktop {
      return Ok(None);
    }
    update_public_key_from_config(
      self.cli_options.start_dir.to_desktop_config(),
    )
  }

  pub async fn write_bin(
    &self,
    options: WriteBinOptions<'_>,
  ) -> Result<(), AnyError> {
    // Select base binary based on target
    let mut original_binary =
      self.get_base_binary(options.compile_flags).await?;

    if options.compile_flags.no_terminal {
      let target = resolve_compile_target(options.compile_flags);
      if !target.contains("windows") {
        bail!(
          "The `--no-terminal` flag is only available when targeting Windows (current: {})",
          target,
        )
      }
      set_windows_binary_to_gui(&mut original_binary)
        .context("Setting windows binary to GUI.")?;
    }
    if options.compile_flags.icon.is_some() {
      let target = resolve_compile_target(options.compile_flags);
      // Desktop builds handle icons during app bundle packaging.
      if !target.contains("windows") && !self.is_desktop {
        bail!(
          "The `--icon` flag is only available when targeting Windows (current: {})",
          target,
        );
      }
    }
    // Validate the resolved app name (explicit `--app-name` or the default
    // derived from the output file name) up front, so an invalid name fails
    // before we do any work to write the binary. The returned name is discarded
    // here; the value actually baked into the metadata is resolved again at the
    // write site below.
    resolve_app_name(
      options.compile_flags,
      options.display_output_filename,
      self.is_desktop,
    )?;
    self.write_standalone_binary(options, original_binary).await
  }

  async fn get_base_binary(
    &self,
    compile_flags: &CompileFlags,
  ) -> Result<Vec<u8>, AnyError> {
    if self.is_desktop {
      return self.get_desktop_base_binary(compile_flags).await;
    }

    // Used for testing.
    //
    // Phase 2 of the 'min sized' deno compile RFC talks
    // about adding this as a flag.
    if let Some(path) = get_dev_binary_path() {
      if compile_flags.engine == JavaScriptEngine::QuickJs {
        log::warn!(
          "--engine quickjs is ignored when using the development denort at {}",
          path.to_string_lossy()
        );
      }
      log::debug!("Resolved denort: {}", path.to_string_lossy());
      return std::fs::read(&path).with_context(|| {
        format!("Could not find denort at '{}'", path.to_string_lossy())
      });
    }

    let target = resolve_compile_target(compile_flags);
    let binary_name =
      runtime_archive_name("denort", &compile_flags.engine, &target);

    // A denext build compiles onto its own release's denort (the runtime
    // patches, Deno.SQL, ...), never Deno's stock one of the same version.
    let denext_version = (compile_flags.engine == JavaScriptEngine::V8)
      .then(|| DENO_VERSION_INFO.denext())
      .flatten();
    let read_file = |path: &Path| -> Result<Vec<u8>, AnyError> {
      std::fs::read(path).with_context(|| format!("Reading {}", path.display()))
    };
    let download_directory = self.deno_dir.dl_folder_path();
    let archive_data = if let Some(version) = denext_version {
      // denext (DENEXT.md item 130): the release's own denort, checked
      // against the same release's SHA256SUMS before it is used or cached.
      let (binary_path_suffix, download_url) = denext_denort(version, &target);
      let binary_path = download_directory.join(&binary_path_suffix);
      log::debug!("Resolved denort: {}", binary_path.display());
      let asset = denext_denort_asset_name(version, &target);
      let sums_url = denext_release_sums_url(&download_url);
      let client = self.http_client_provider.get_or_create()?;
      let fetch =
        async |what: DenortFetch| -> Result<Option<Vec<u8>>, AnyError> {
          let url = match what {
            DenortFetch::Archive => download_url.as_str(),
            DenortFetch::Sums => sums_url.as_str(),
          };
          let progress_bars = ProgressBar::new(ProgressBarStyle::DownloadBars);
          let progress = progress_bars.update(url);
          let response = client
            .download_with_progress_and_retries(
              url.parse()?,
              &Default::default(),
              &progress,
            )
            .await
            .with_context(|| format!("Failed downloading '{url}'"))?;
          Ok(
            response
              .into_maybe_bytes()
              .with_context(|| format!("Failed downloading '{url}'"))?,
          )
        };
      load_verified_denext_denort(
        &binary_path,
        &DenextDenortSource {
          asset: &asset,
          archive_url: &download_url,
          sums_url: &sums_url,
        },
        fetch,
      )
      .await
      .context("Setting up base binary.")?
    } else {
      let binary_path_suffix = deno_land_suffix(
        DENO_VERSION_INFO.release_channel,
        DENO_VERSION_INFO.git_hash,
        &binary_name,
      );
      let download_url = format!("https://dl.deno.land/{binary_path_suffix}");
      let binary_path = download_directory.join(&binary_path_suffix);
      log::debug!("Resolved denort: {}", binary_path.display());
      if binary_path.exists() {
        read_file(&binary_path)?
      } else {
        self
          .download_base_binary(&binary_path, &download_url)
          .await
          .context("Setting up base binary.")?
      }
    };
    let temp_dir = tempfile::TempDir::new()?;
    let base_binary_path = archive::unpack_into_dir(archive::UnpackArgs {
      exe_name: if target.contains("windows") {
        "denort.exe"
      } else {
        "denort"
      },
      archive_name: &binary_name,
      archive_data: &archive_data,
      dest_path: temp_dir.path(),
    })?;
    let base_binary = read_file(&base_binary_path)?;
    drop(temp_dir); // delete the temp dir
    Ok(base_binary)
  }

  async fn get_desktop_base_binary(
    &self,
    compile_flags: &CompileFlags,
  ) -> Result<Vec<u8>, AnyError> {
    // For development: check DENORT_DESKTOP_BIN env var or look
    // for libdenort next to the deno executable.
    if let Some(path) = get_dev_desktop_binary_path() {
      if compile_flags.engine == JavaScriptEngine::QuickJs {
        log::warn!(
          "--engine quickjs is ignored when using the development libdenort at {}",
          path.to_string_lossy()
        );
      }
      log::debug!("Resolved libdenort: {}", path.to_string_lossy());
      return std::fs::read(&path).with_context(|| {
        format!("Could not find libdenort at '{}'", path.to_string_lossy())
      });
    }

    let target = resolve_compile_target(compile_flags);
    let lib_ext = if target.contains("darwin") {
      "dylib"
    } else if target.contains("windows") {
      "dll"
    } else {
      "so"
    };
    let lib_name = if target.contains("windows") {
      format!("denort.{lib_ext}")
    } else {
      format!("libdenort.{lib_ext}")
    };
    let binary_name =
      runtime_archive_name("libdenort", &compile_flags.engine, &target);

    let binary_path_suffix = deno_land_suffix(
      DENO_VERSION_INFO.release_channel,
      DENO_VERSION_INFO.git_hash,
      &binary_name,
    );

    let download_directory = self.deno_dir.dl_folder_path();
    let binary_path = download_directory.join(&binary_path_suffix);
    log::debug!("Resolved libdenort: {}", binary_path.display());

    let read_file = |path: &Path| -> Result<Vec<u8>, AnyError> {
      std::fs::read(path).with_context(|| format!("Reading {}", path.display()))
    };
    let archive_data = if binary_path.exists() {
      read_file(&binary_path)?
    } else {
      self
        .download_base_binary(
          &binary_path,
          &format!("https://dl.deno.land/{binary_path_suffix}"),
        )
        .await
        .context("Setting up desktop base binary.")?
    };
    let temp_dir = tempfile::TempDir::new()?;
    let base_binary_path = archive::unpack_into_dir(archive::UnpackArgs {
      exe_name: &lib_name,
      archive_name: &binary_name,
      archive_data: &archive_data,
      dest_path: temp_dir.path(),
    })?;
    let base_binary = read_file(&base_binary_path)?;
    drop(temp_dir);
    Ok(base_binary)
  }

  async fn download_base_binary(
    &self,
    output_path: &Path,
    download_url: &str,
  ) -> Result<Vec<u8>, AnyError> {
    let response = {
      let progress_bars = ProgressBar::new(ProgressBarStyle::DownloadBars);
      let progress = progress_bars.update(download_url);

      self
        .http_client_provider
        .get_or_create()?
        .download_with_progress_and_retries(
          download_url.parse()?,
          &Default::default(),
          &progress,
        )
        .await?
    };
    let bytes = response
      .into_bytes()
      .with_context(|| format!("Failed downloading '{}'", download_url))?;

    let create_dir_all = |dir: &Path| {
      std::fs::create_dir_all(dir)
        .with_context(|| format!("Creating {}", dir.display()))
    };
    create_dir_all(output_path.parent().unwrap())?;
    atomic_write_file_with_retries(
      &CliSys::default(),
      output_path,
      &bytes,
      CACHE_PERM,
    )
    .with_context(|| format!("Writing {}", output_path.display()))?;
    Ok(bytes)
  }

  /// This functions creates a standalone deno binary by appending a bundle
  /// and magic trailer to the currently executing binary.
  async fn write_standalone_binary(
    &self,
    options: WriteBinOptions<'_>,
    original_bin: Vec<u8>,
  ) -> Result<(), AnyError> {
    let WriteBinOptions {
      writer,
      display_output_filename,
      graph,
      entrypoint,
      include_paths,
      exclude_paths,
      compile_flags,
    } = options;
    let ca_data = match self.cli_options.ca_data() {
      Some(CaData::File(ca_file)) => Some(
        std::fs::read(ca_file).with_context(|| format!("Reading {ca_file}"))?,
      ),
      Some(CaData::Bytes(bytes)) => Some(bytes.clone()),
      None => None,
    };
    let mut vfs = VfsBuilder::new();
    for path in exclude_paths {
      vfs.add_exclude_path(path);
    }
    // Embed the workspace package.json files so the standalone binary's node
    // resolver can read their "exports" (and other) fields at runtime. Without
    // this, resolving a workspace member by its package name falls back to
    // legacy `index.js` resolution instead of honoring the package's exports.
    for pkg_json in self.cli_options.workspace().package_jsons() {
      vfs.add_path(&pkg_json.path)?;
    }
    let progress_bar = ProgressBar::new(ProgressBarStyle::ProgressBars);
    // With --bundle the JS graph is self-contained, so the whole npm tree
    // is intentionally left out of the binary. The exception is packages
    // that ship native (.node) addons: the package JS is still bundled, but
    // its `.node` file imports stay external (`external = ["*.node"]` in
    // compile.rs) so the addon loader resolves them against the embedded VFS
    // at runtime. For that to work the package's installed folder, plus the
    // closure of its dependencies, must be embedded in the VFS.
    let npm_snapshot = if compile_flags.bundle {
      self
        .fill_bundle_native_addon_vfs(&mut vfs, &progress_bar)
        .context("Embedding native addon packages.")?
    } else {
      match &self.npm_resolver {
        CliNpmResolver::Managed(managed) => {
          if graph.modules().any(|m| m.npm().is_some()) {
            let snapshot = managed.resolution().snapshot();
            // When the user opts in (or via the existing unstable lazy-caching
            // path), prune the resolution snapshot to packages reachable from
            // npm specifiers in the graph. Otherwise embed the full snapshot
            // so non-statically-analyzable dynamic imports keep working.
            let snapshot = if compile_flags.exclude_unused_npm
              || self.cli_options.unstable_npm_lazy_caching()
            {
              let reqs = graph
                .specifiers()
                .filter_map(|(s, _)| {
                  NpmPackageReqReference::from_specifier(s)
                    .ok()
                    .map(|req_ref| req_ref.into_inner().req)
                })
                .collect::<Vec<_>>();
              snapshot.subset(&reqs)
            } else {
              snapshot
            }
            .as_valid_serialized_for_system(&self.npm_system_info);
            if !snapshot.as_serialized().packages.is_empty() {
              self
                .fill_npm_vfs(&mut vfs, Some(&snapshot), &progress_bar)
                .context("Building npm vfs.")?;
              Some(snapshot)
            } else {
              None
            }
          } else {
            None
          }
        }
        CliNpmResolver::Byonm(_) => {
          self.fill_npm_vfs(&mut vfs, None, &progress_bar)?;
          None
        }
      }
    };
    for include_file in include_paths {
      let path = deno_path_util::url_to_file_path(include_file)?;
      vfs
        .add_path(&path)
        .with_context(|| format!("Including {}", path.display()))?;
    }
    let specifiers_count = graph.specifiers_count();
    let mut specifier_store = SpecifierStore::with_capacity(specifiers_count);
    let mut remote_modules_store =
      SpecifierDataStore::with_capacity(specifiers_count);
    let mut asset_module_urls = graph.asset_module_urls();
    let progress =
      progress_bar.update_with_prompt(ProgressMessagePrompt::Compile, "");
    progress.set_total_size(specifiers_count as u64);
    let mut modules_done: u64 = 0;
    let vfs_sources = collect_vfs_cjs_analysis_sources(&vfs);
    let sys = CliSys::default();
    let file_system_source_provider =
      FileSystemCjsAnalysisSourceProvider { sys: &sys };
    // todo(dsherret): transpile and analyze CJS in parallel
    for module in graph.modules() {
      if module.specifier().scheme() == "data" {
        continue; // don't store data urls as an entry as they're in the code
      }
      let mut maybe_source_map = None;
      let mut maybe_transpiled = None;
      let mut maybe_cjs_analysis = None;
      let (maybe_original_source, media_type) = match module {
        deno_graph::Module::Js(m) => {
          let specifier = &m.specifier;
          let original_bytes = match m.source.try_get_original_bytes() {
            Some(bytes) => bytes,
            None => self.load_asset_bypass_permissions(specifier).await?.source,
          };
          if self.cjs_tracker.is_maybe_cjs(specifier, m.media_type)? {
            if self.cjs_tracker.is_cjs_with_known_is_script(
              specifier,
              m.media_type,
              m.is_script,
            )? {
              let vfs_source_provider = VfsCjsAnalysisSourceProvider {
                vfs: &vfs,
                sources: &vfs_sources,
                fallback: Some(&file_system_source_provider),
              };
              let graph_source_provider = GraphCjsAnalysisSourceProvider::new(
                graph,
                Some(&vfs_source_provider),
              );
              let cjs_analysis = self
                .cjs_module_export_analyzer
                .analyze_all_exports(
                  module.specifier(),
                  Some(Cow::Borrowed(m.source.text.as_ref())),
                  Some(&graph_source_provider),
                )
                .await?;
              maybe_cjs_analysis = Some(match cjs_analysis {
                ResolvedCjsAnalysis::Esm(_) => CjsExportAnalysisEntry::Esm,
                ResolvedCjsAnalysis::Cjs(exports) => {
                  CjsExportAnalysisEntry::Cjs(
                    exports.into_iter().collect::<Vec<_>>(),
                  )
                }
              });
            } else {
              maybe_cjs_analysis = Some(CjsExportAnalysisEntry::Esm);
            }
          }
          if m.media_type.is_emittable() {
            let module_kind = match maybe_cjs_analysis.as_ref() {
              Some(CjsExportAnalysisEntry::Cjs(_)) => ModuleKind::Cjs,
              _ => ModuleKind::Esm,
            };
            let (source, source_map) =
              self.emitter.emit_source_for_deno_compile(
                &m.specifier,
                m.media_type,
                module_kind,
                &m.source.text,
              )?;
            if source != m.source.text.as_ref() {
              maybe_source_map = Some(source_map.into_bytes());
              maybe_transpiled = Some(source.into_bytes());
            }
          }
          (Some(original_bytes), m.media_type)
        }
        deno_graph::Module::Json(m) => {
          let original_bytes = match m.source.try_get_original_bytes() {
            Some(bytes) => bytes,
            None => {
              self
                .load_asset_bypass_permissions(&m.specifier)
                .await?
                .source
            }
          };
          (Some(original_bytes), m.media_type)
        }
        deno_graph::Module::Wasm(m) => {
          (Some(m.source.clone()), MediaType::Wasm)
        }
        deno_graph::Module::Npm(_)
        | deno_graph::Module::Node(_)
        | deno_graph::Module::External(_) => (None, MediaType::Unknown),
      };
      if let Some(original_source) = maybe_original_source {
        asset_module_urls.swap_remove(module.specifier());
        let maybe_cjs_export_analysis = maybe_cjs_analysis
          .as_ref()
          .map(bincode::serialize)
          .transpose()?;
        if module.specifier().scheme() == "file" {
          let file_path = deno_path_util::url_to_file_path(module.specifier())?;
          vfs
            .add_file_with_data(
              &file_path,
              deno_lib::standalone::virtual_fs::AddFileDataOptions {
                data: original_source.to_vec(),
                maybe_transpiled,
                maybe_source_map,
                maybe_cjs_export_analysis,
                mtime: file_path
                  .metadata()
                  .ok()
                  .and_then(|m| m.modified().ok()),
              },
            )
            .with_context(|| {
              format!("Failed adding '{}'", file_path.display())
            })?;
        } else {
          let specifier_id = specifier_store.get_or_add(module.specifier());
          remote_modules_store.add(
            specifier_id,
            RemoteModuleEntry {
              media_type,
              is_valid_utf8: is_valid_utf8(&original_source),
              data: Cow::Owned(original_source.to_vec()),
              maybe_transpiled: maybe_transpiled.map(Cow::Owned),
              maybe_source_map: maybe_source_map.map(Cow::Owned),
              maybe_cjs_export_analysis: maybe_cjs_export_analysis
                .map(Cow::Owned),
            },
          );
        }
      }
      modules_done += 1;
      progress.set_position(modules_done);
    }
    drop(progress);

    for url in asset_module_urls {
      if graph.try_get(url).is_err() {
        // skip because there was an error loading this module
        continue;
      }
      match url.scheme() {
        "file" => {
          let file_path = deno_path_util::url_to_file_path(url)?;
          vfs.add_path(&file_path)?;
        }
        "http" | "https" => {
          let specifier_id = specifier_store.get_or_add(url);
          if !remote_modules_store.contains(specifier_id) {
            // it's ok to bypass permissions here because we verified the module
            // loaded successfully in the graph
            let file = self.load_asset_bypass_permissions(url).await?;
            remote_modules_store.add(
              specifier_id,
              RemoteModuleEntry {
                media_type: MediaType::from_specifier_and_headers(
                  &file.url,
                  file.maybe_headers.as_ref(),
                ),
                is_valid_utf8: is_valid_utf8(&file.source),
                data: Cow::Owned(file.source.to_vec()),
                maybe_cjs_export_analysis: None,
                maybe_source_map: None,
                maybe_transpiled: None,
              },
            );
          }
        }
        _ => {}
      }
    }

    let mut redirects_store =
      SpecifierDataStore::with_capacity(graph.redirects.len());
    for (from, to) in &graph.redirects {
      redirects_store.add(
        specifier_store.get_or_add(from),
        specifier_store.get_or_add(to),
      );
    }

    if let Some(import_map) = self.workspace_resolver.maybe_import_map()
      && let Ok(file_path) = url_to_file_path(import_map.base_url())
      && let Some(import_map_parent_dir) = file_path.parent()
    {
      // tell the vfs about the import map's parent directory in case it
      // falls outside what the root of where the VFS will be based
      vfs.add_possible_min_root_dir(import_map_parent_dir);
    }
    if let Some(node_modules_dir) = self.npm_resolver.root_node_modules_path() {
      // ensure the vfs doesn't go below the node_modules directory's parent
      if let Some(parent) = node_modules_dir.parent() {
        vfs.add_possible_min_root_dir(parent);
      }
    }

    // do CJS export analysis on all the files in the VFS
    // todo(dsherret): analyze cjs in parallel
    let vfs_source_provider = VfsCjsAnalysisSourceProvider {
      vfs: &vfs,
      sources: &vfs_sources,
      fallback: Some(&file_system_source_provider),
    };
    let graph_source_provider =
      GraphCjsAnalysisSourceProvider::new(graph, Some(&vfs_source_provider));
    let mut to_add = Vec::new();
    for (file_path, file) in vfs.iter_files() {
      if file.cjs_export_analysis_offset.is_some() {
        continue; // already analyzed
      }
      let specifier = deno_path_util::url_from_file_path(&file_path)?;
      let media_type = MediaType::from_specifier(&specifier);
      // Only script-flavored files can carry CJS exports. Extensions answer
      // this for everything except extensionless files (`MediaType::Unknown`),
      // which may be real modules (an npm `"main"` with no extension — see
      // test-module-main-extension-lookup); those are disambiguated by content
      // below rather than skipped outright.
      if !matches!(
        media_type,
        MediaType::JavaScript
          | MediaType::Mjs
          | MediaType::Cjs
          | MediaType::Jsx
          | MediaType::TypeScript
          | MediaType::Mts
          | MediaType::Cts
          | MediaType::Tsx
          | MediaType::Dts
          | MediaType::Dmts
          | MediaType::Dcts
          | MediaType::Unknown
      ) {
        continue;
      }
      if self.cjs_tracker.is_maybe_cjs(&specifier, media_type)? {
        // Strict UTF-8 (not `from_utf8_lossy`): binary assets (images,
        // fonts, …) that resolve to `Unknown` are skipped rather than
        // mangled into garbage that panics swc. Extensionless *text*
        // modules still flow through.
        let Some(bytes) = vfs.file_bytes(file.offset) else {
          continue;
        };
        let Ok(source) = std::str::from_utf8(bytes) else {
          continue;
        };
        let cjs_analysis_result = self
          .cjs_module_export_analyzer
          .analyze_all_exports(
            &specifier,
            Some(source.into()),
            Some(&graph_source_provider),
          )
          .await;
        let analysis = match cjs_analysis_result {
          Ok(ResolvedCjsAnalysis::Esm(_)) => CjsExportAnalysisEntry::Esm,
          Ok(ResolvedCjsAnalysis::Cjs(exports)) => {
            CjsExportAnalysisEntry::Cjs(exports.into_iter().collect::<Vec<_>>())
          }
          Err(err) => {
            log::debug!(
              "Had cjs export analysis error for '{}': {}",
              specifier,
              err
            );
            CjsExportAnalysisEntry::Error(err.to_string())
          }
        };
        to_add.push((file_path, bincode::serialize(&analysis)?));
      }
    }
    for (file_path, analysis) in to_add {
      vfs.add_cjs_export_analysis(&file_path, analysis);
    }

    let vfs = self.build_vfs_consolidating_global_npm_cache(vfs);

    let root_dir_url = match &vfs.root_path {
      WindowsSystemRootablePath::Path(dir) => {
        Some(url_from_directory_path(dir)?)
      }
      WindowsSystemRootablePath::WindowSystemRoot => None,
    };
    let root_dir_url = match &root_dir_url {
      Some(url) => StandaloneRelativeFileBaseUrl::Path(url),
      None => StandaloneRelativeFileBaseUrl::WindowsSystemRoot,
    };

    let code_cache_key = if self.cli_options.code_cache_enabled() {
      let mut hasher = FastInsecureHasher::new_deno_versioned();
      for module in graph.modules() {
        if let Some(source) = module.source() {
          hasher
            .write(root_dir_url.specifier_key(module.specifier()).as_bytes());
          hasher.write(source.as_bytes());
        }
      }
      Some(hasher.finish())
    } else {
      None
    };

    let node_modules = match &self.npm_resolver {
      CliNpmResolver::Managed(_) => {
        npm_snapshot.as_ref().map(|_| NodeModules::Managed {
          node_modules_dir: self.npm_resolver.root_node_modules_path().map(
            |path| {
              root_dir_url
                .specifier_key(
                  &ModuleSpecifier::from_directory_path(path).unwrap(),
                )
                .into_owned()
            },
          ),
        })
      }
      CliNpmResolver::Byonm(resolver) => Some(NodeModules::Byonm {
        root_node_modules_dir: resolver.root_node_modules_path().map(
          |node_modules_dir| {
            root_dir_url
              .specifier_key(
                &ModuleSpecifier::from_directory_path(node_modules_dir)
                  .unwrap(),
              )
              .into_owned()
          },
        ),
      }),
    };

    let env_vars_from_env_file = {
      let mut aggregated_env_vars = IndexMap::new();
      for env_file_name in self.cli_options.env_file_names().rev() {
        match deno_dotenv::find_path_and_content(
          &CliSys::default(),
          self.cli_options.initial_cwd(),
          env_file_name,
        ) {
          Ok(Some((env_file_path, content))) => {
            match get_file_env_vars(&content) {
              Ok(env_vars) => {
                aggregated_env_vars.extend(env_vars);
                log::info!(
                  "{} Environment variables from the file \"{}\" were embedded in the generated executable file",
                  crate::colors::yellow("Warning"),
                  env_file_path.display()
                );
              }
              Err(e) => {
                handle_dotenv_error(
                  &e,
                  &env_file_path,
                  self.cli_options.log_level(),
                );
              }
            };
          }
          Ok(None) => {
            handle_dotenv_not_found(
              env_file_name,
              self.cli_options.log_level(),
            );
          }
          Err(e) => {
            handle_dotenv_io_error(&e, self.cli_options.log_level());
          }
        };
      }
      aggregated_env_vars
    };

    output_vfs(&vfs, display_output_filename);

    let preload_modules = self
      .cli_options
      .preload_modules()?
      .into_iter()
      .map(|s| root_dir_url.specifier_key(&s).into_owned())
      .collect::<Vec<_>>();

    let require_modules = self
      .cli_options
      .require_modules()?
      .into_iter()
      .map(|s| root_dir_url.specifier_key(&s).into_owned())
      .collect::<Vec<_>>();

    // The page origin and identifier are desktop-only concerns: `deno
    // compile` binaries have no webview. Unlike the release URLs below, a
    // malformed value is an error rather than a silent fallback — the app
    // would otherwise start at an origin, or keep its web storage in a place,
    // the developer did not configure.
    let mut desktop_app_identity = if self.is_desktop {
      Some(resolve_desktop_app_identity(self.cli_options)?)
    } else {
      None
    };
    // The deep-link schemes let the runtime tell a link in its launch
    // arguments from any other argument. Desktop-only, like the origin.
    let desktop_deep_links = if self.is_desktop {
      Some(resolve_desktop_deep_links(self.cli_options)?)
    } else {
      None
    };
    let metadata = Metadata {
      argv: compile_flags.args.clone(),
      seed: self.cli_options.seed(),
      max_memory: self.cli_options.max_memory(),
      max_cpu_time: self.cli_options.max_cpu_time(),
      max_time: self.cli_options.max_time(),
      code_cache_key,
      location: self.cli_options.location_flag().clone(),
      permissions: self.cli_options.permissions_options()?,
      v8_flags: construct_v8_flags(
        &get_default_v8_flags(),
        self.cli_options.v8_flags(),
        vec![],
      ),
      unsafely_ignore_certificate_errors: self
        .cli_options
        .unsafely_ignore_certificate_errors()
        .clone(),
      log_level: self.cli_options.log_level(),
      ca_stores: self.cli_options.ca_stores().clone(),
      ca_data,
      env_vars_from_env_file,
      entrypoint_key: root_dir_url.specifier_key(entrypoint).into_owned(),
      preload_modules,
      require_modules,
      workspace_resolver: SerializedWorkspaceResolver {
        import_map: self.workspace_resolver.maybe_import_map().map(|i| {
          SerializedWorkspaceResolverImportMap {
            specifier: if i.base_url().scheme() == "file" {
              root_dir_url.specifier_key(i.base_url()).into_owned()
            } else {
              // just make a remote url local
              "deno.json".to_string()
            },
            json: i.to_json(),
          }
        }),
        jsr_pkgs: self
          .workspace_resolver
          .jsr_packages()
          .iter()
          .map(|pkg| SerializedResolverWorkspaceJsrPackage {
            relative_base: root_dir_url.specifier_key(&pkg.base).into_owned(),
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            exports: pkg.exports.clone(),
          })
          .collect(),
        package_jsons: self
          .workspace_resolver
          .package_jsons()
          .map(|pkg_json| {
            (
              root_dir_url
                .specifier_key(&pkg_json.specifier())
                .into_owned(),
              serde_json::to_value(pkg_json).unwrap(),
            )
          })
          .collect(),
        pkg_json_resolution: self.workspace_resolver.pkg_json_dep_resolution(),
        catalogs: self.workspace_resolver.catalogs().clone(),
      },
      node_modules,
      unstable_config: UnstableConfig {
        legacy_flag_enabled: false,
        detect_cjs: self.cli_options.unstable_detect_cjs(),
        features: self
          .cli_options
          .unstable_features()
          .into_iter()
          .map(|s| s.to_string())
          .collect(),
        lazy_dynamic_imports: self.cli_options.unstable_lazy_dynamic_imports(),
        npm_lazy_caching: self.cli_options.unstable_npm_lazy_caching(),
        raw_imports: self.cli_options.unstable_raw_imports(),
        sloppy_imports: self.cli_options.unstable_sloppy_imports(),
      },
      otel_config: self.cli_options.otel_config(),
      vfs_case_sensitivity: vfs.case_sensitivity,
      self_extracting: if compile_flags.self_extracting {
        let mut hasher = FastInsecureHasher::new_deno_versioned();
        for file in &vfs.files {
          hasher.write_u64(file.len() as u64);
          hasher.write(file);
        }
        Some(format!("{:016x}", hasher.finish()))
      } else {
        None
      },
      // Bake in a stable app identity so origin-bound storage (default
      // `Deno.openKv()`, `localStorage`, `caches`) persists to a per-app
      // directory at runtime. Prefer an explicit `--app-name`, otherwise derive
      // it from the output file name (minus any executable extension Deno adds).
      // Resolving here keeps the identity stable even if the binary is later
      // renamed. The name is already validated in `write_bin` (via
      // `resolve_app_name`).
      app_name: Some(resolve_app_name(
        compile_flags,
        display_output_filename,
        self.is_desktop,
      )?),
      app_version: self
        .cli_options
        .workspace()
        .root_deno_json()
        .and_then(|c| c.json.version.clone()),
      error_reporting_url: self
        .cli_options
        .start_dir
        .to_desktop_config()
        .ok()
        .and_then(|c| c.error_reporting.as_ref()?.url.clone()),
      release_base_url: self
        .cli_options
        .start_dir
        .to_desktop_config()
        .ok()
        .and_then(|c| c.release.as_ref()?.base_url.clone()),
      update_public_key: self.resolve_update_public_key()?,
      app_origin: desktop_app_identity
        .as_mut()
        .and_then(|identity| identity.origin.take()),
      // The identifier reaches the runtime so it can hand it to the webview
      // backend itself when the launcher did not (a directly executed
      // binary, or a Windows `<app>.exe`, which has no launcher).
      app_identifier: desktop_app_identity
        .as_mut()
        .and_then(|identity| identity.identifier.take()),
      app_deep_links: desktop_deep_links,
      desktop_dev: self.is_desktop && self.cli_options.is_desktop_dev_build(),
      initial_window: self
        .cli_options
        .start_dir
        .to_desktop_config()
        .ok()
        .and_then(|c| c.initial_window.as_ref())
        .map(|w| InitialWindowConfig {
          width: w.width,
          height: w.height,
          frameless: w.frameless,
          no_activate: w.no_activate,
          transparent_titlebar: w.transparent_titlebar,
          transparent: w.transparent,
          show_on_first_load: w.show_on_first_load,
        }),
    };

    let (data_section_bytes, section_sizes) = serialize_binary_data_section(
      &metadata,
      npm_snapshot.map(|s| s.into_serialized()),
      &specifier_store.for_serialization(&root_dir_url),
      &redirects_store,
      &remote_modules_store,
      &vfs,
    )
    .context("Serializing binary data section.")?;

    log::info!(
      "\n{} {}",
      crate::colors::bold("Files:"),
      crate::util::display::human_size(section_sizes.vfs as f64)
    );
    log::info!(
      "{} {}",
      crate::colors::bold("Metadata:"),
      crate::util::display::human_size(section_sizes.metadata as f64)
    );
    log::info!(
      "{} {}\n",
      crate::colors::bold("Remote modules:"),
      crate::util::display::human_size(section_sizes.remote_modules as f64)
    );

    write_binary_bytes(writer, original_bin, data_section_bytes, compile_flags)
      .context("Writing binary bytes")
  }

  async fn load_asset_bypass_permissions(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<
    deno_cache_dir::file_fetcher::File,
    deno_resolver::file_fetcher::FetchError,
  > {
    self
      .file_fetcher
      .fetch_with_options(
        specifier,
        FetchPermissionsOptionRef::AllowAll,
        FetchOptions {
          local: FetchLocalOptions {
            include_mtime: false,
          },
          maybe_auth: None,
          maybe_accept: None,
          maybe_cache_setting: Some(
            &deno_cache_dir::file_fetcher::CacheSetting::Use,
          ),
        },
      )
      .await
  }

  /// Decide what to embed for `deno compile --bundle`. The bundle is
  /// always shipped; this controls the npm portion. We need it when
  /// either the CJS-from-ESM wrapper pointed at on-disk paths during
  /// rewriting, or the resolved tree has a native (`.node`) addon — in
  /// both cases the compiled binary will do node-module resolution at
  /// runtime. Pure-ESM bundles with no native addons skip this and ship
  /// nothing npm-related.
  ///
  /// When embedding is needed, we ship only the packages actually
  /// reached: the rewriter recorded every absolute path it pointed at,
  /// and we map each path back to its owning npm package and walk that
  /// closure. The full resolution snapshot still goes in the metadata
  /// so denort can resolve packages by name at runtime.
  fn fill_bundle_native_addon_vfs(
    &self,
    builder: &mut VfsBuilder,
    progress_bar: &ProgressBar,
  ) -> Result<Option<ValidSerializedNpmResolutionSnapshot>, AnyError> {
    let needs_for_cjs_wrapper =
      self.cli_options.compile_bundle_embed_node_modules();
    let referenced_paths = self.cli_options.compile_bundle_referenced_paths();
    // For BYONM the addon scan walks the workspace `node_modules` trees, so
    // it needs the workspace root (managed npm ignores it).
    let workspace_root = self
      .cli_options
      .workspace()
      .root_dir_url()
      .to_file_path()
      .ok();
    let needs_for_native_addons = !needs_for_cjs_wrapper
      && !super::native_addons::find_native_addon_packages(
        self.npm_resolver,
        &self.npm_system_info,
        workspace_root.as_deref(),
      )?
      .is_empty();
    if !needs_for_cjs_wrapper && !needs_for_native_addons {
      return Ok(None);
    }

    match self.npm_resolver {
      CliNpmResolver::Managed(managed) => {
        let snapshot = managed
          .resolution()
          .snapshot()
          .as_valid_serialized_for_system(&self.npm_system_info);
        if snapshot.as_serialized().packages.is_empty() {
          return Ok(None);
        }
        // `collect_bundle_required_packages` only returns `None` for BYONM,
        // which is handled by the `CliNpmResolver::Byonm` arm below, so a
        // managed resolver always yields `Some` here.
        let Some(needed_ids) =
          super::native_addons::collect_bundle_required_packages(
            self.npm_resolver,
            &self.npm_system_info,
            referenced_paths,
          )?
        else {
          unreachable!(
            "collect_bundle_required_packages returns None only for BYONM"
          );
        };
        let progress =
          progress_bar.update_with_prompt(ProgressMessagePrompt::Compile, "");
        progress.set_total_size(needed_ids.len() as u64);
        // Dedup the set of `<deno-cache>/<id>/node_modules/` directories we
        // add: a single id's node_modules dir contains the canonical package
        // folder plus sibling symlinks to its direct deps. Going one level up
        // from the canonical folder picks both up so node-module resolution at
        // runtime can follow the symlink chain (e.g. the NAPI-RS
        // platform-specific sibling package).
        let mut embedded_roots: std::collections::HashSet<PathBuf> =
          std::collections::HashSet::new();
        let mut done: u64 = 0;
        for id in &needed_ids {
          if let Ok(folder) = managed.resolve_pkg_folder_from_pkg_id(id)
            && folder.exists()
          {
            let root_to_add =
              pkg_folder_node_modules_root(&folder).unwrap_or(folder.as_path());
            if embedded_roots.insert(root_to_add.to_path_buf()) {
              builder.add_dir_recursive(root_to_add).with_context(|| {
                format!("Embedding npm package at '{}'", root_to_add.display())
              })?;
            }
          }
          done += 1;
          progress.set_position(done);
        }
        Ok(Some(snapshot))
      }
      CliNpmResolver::Byonm(_) => {
        self.fill_npm_vfs(builder, None, progress_bar)?;
        Ok(None)
      }
    }
  }

  fn fill_npm_vfs(
    &self,
    builder: &mut VfsBuilder,
    snapshot: Option<&ValidSerializedNpmResolutionSnapshot>,
    progress_bar: &ProgressBar,
  ) -> Result<(), AnyError> {
    fn maybe_warn_different_system(system_info: &NpmSystemInfo) {
      if system_info != &NpmSystemInfo::default() {
        log::warn!(
          "{} The node_modules directory may be incompatible with the target system.",
          crate::colors::yellow("Warning")
        );
      }
    }

    match &self.npm_resolver {
      CliNpmResolver::Managed(npm_resolver) => {
        if let Some(node_modules_path) = npm_resolver.root_node_modules_path() {
          maybe_warn_different_system(&self.npm_system_info);
          let _progress =
            progress_bar.update_with_prompt(ProgressMessagePrompt::Compile, "");
          builder.add_dir_recursive(node_modules_path)?;
          Ok(())
        } else {
          let snapshot = snapshot.unwrap();
          // we'll flatten to remove any custom registries later
          let mut packages =
            snapshot.as_serialized().packages.iter().collect::<Vec<_>>();
          packages.sort_by(|a, b| a.id.cmp(&b.id)); // determinism
          let current_system = NpmSystemInfo::default();
          let progress =
            progress_bar.update_with_prompt(ProgressMessagePrompt::Compile, "");
          progress.set_total_size(packages.len() as u64);
          let mut packages_done: u64 = 0;
          for package in packages {
            let folder =
              npm_resolver.resolve_pkg_folder_from_pkg_id(&package.id)?;
            if !package.system.matches_system(&current_system)
              && !folder.exists()
            {
              log::warn!(
                "{} Ignoring 'npm:{}' because it was not present on the current system.",
                crate::colors::yellow("Warning"),
                package.id
              );
            } else {
              builder.add_dir_recursive(&folder)?;
            }
            packages_done += 1;
            progress.set_position(packages_done);
          }
          drop(progress);
          Ok(())
        }
      }
      CliNpmResolver::Byonm(_) => {
        maybe_warn_different_system(&self.npm_system_info);
        let _progress =
          progress_bar.update_with_prompt(ProgressMessagePrompt::Compile, "");
        // traverse and add all the node_modules directories in the workspace
        let mut pending_dirs = VecDeque::new();
        pending_dirs.push_back(
          self
            .cli_options
            .workspace()
            .root_dir_url()
            .to_file_path()
            .unwrap(),
        );
        while let Some(pending_dir) = pending_dirs.pop_front() {
          let Ok(entries) = fs::read_dir(&pending_dir) else {
            // Don't bother surfacing this error as it might be an error
            // like "access denied". In this case, just skip over it.
            continue;
          };
          let mut entries = entries.filter_map(|e| e.ok()).collect::<Vec<_>>();
          entries.sort_by_cached_key(|entry| entry.file_name()); // determinism
          for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
              continue;
            }
            if path.ends_with("node_modules") {
              builder.add_dir_recursive(&path)?;
            } else {
              pending_dirs.push_back(path);
            }
          }
        }
        Ok(())
      }
    }
  }

  fn build_vfs_consolidating_global_npm_cache(
    &self,
    mut vfs: VfsBuilder,
  ) -> BuiltVfs {
    match &self.npm_resolver {
      CliNpmResolver::Managed(npm_resolver) => {
        if npm_resolver.root_node_modules_path().is_some() {
          return vfs.build();
        }

        let global_cache_root_path = npm_resolver.global_cache_root_path();

        // Flatten all the registries folders into a single ".deno_compile_node_modules/localhost" folder
        // that will be used by denort when loading the npm cache. This avoids us exposing
        // the user's private registry information and means we don't have to bother
        // serializing all the different registry config into the binary.
        //
        // A registry url may include a sub-path (e.g.
        // `http://mirrors.example.com/npm/`), in which case the on-disk cache
        // layout is `<global_cache>/<host>/<sub>/<pkg>/...` rather than
        // `<global_cache>/<host>/<pkg>/...`. Walk to each known registry's
        // package root before flattening so packages always end up directly
        // under `localhost/`.
        let known_registries_dirnames: Vec<String> =
          npm_resolver.known_registries_dirnames().to_vec();
        let mut localhost_entries: IndexMap<String, VfsEntry> = IndexMap::new();
        let mut registry_top_segments: HashSet<String> = HashSet::new();
        for registry_dirname in &known_registries_dirnames {
          if let Some(first) = registry_dirname.split('/').next()
            && !first.is_empty()
          {
            registry_top_segments.insert(first.to_string());
          }
          let registry_path = global_cache_root_path.join(registry_dirname);
          let Some(registry_dir) = vfs.get_dir_mut(&registry_path) else {
            continue;
          };
          for entry in registry_dir.entries.take_inner() {
            log::debug!("Flattening {} into node_modules", entry.name());
            if let Some(existing) =
              localhost_entries.insert(entry.name().to_string(), entry)
            {
              panic!(
                "Unhandled scenario where a duplicate entry was found: {:?}",
                existing
              );
            }
          }
        }

        let Some(root_dir) = vfs.get_dir_mut(global_cache_root_path) else {
          return vfs.build();
        };

        root_dir.name = DENO_COMPILE_GLOBAL_NODE_MODULES_DIR_NAME.to_string();
        let mut new_entries = Vec::with_capacity(root_dir.entries.len());
        for entry in root_dir.entries.take_inner() {
          match &entry {
            VfsEntry::Dir(dir) if registry_top_segments.contains(&dir.name) => {
              // The packages under this registry host dir have already been
              // flattened into `localhost_entries`. Drop the (now empty)
              // intermediate directory tree so it isn't embedded twice.
            }
            _ => {
              new_entries.push(entry);
            }
          }
        }
        new_entries.push(VfsEntry::Dir(VirtualDirectory {
          name: "localhost".to_string(),
          entries: VirtualDirectoryEntries::new(
            localhost_entries.into_iter().map(|(_, v)| v).collect(),
          ),
        }));
        root_dir.entries = VirtualDirectoryEntries::new(new_entries);

        // it's better to not expose the user's cache directory, so take it out
        // of there
        let case_sensitivity = vfs.case_sensitivity();
        let parent = global_cache_root_path.parent().unwrap();
        let parent_dir = vfs.get_dir_mut(parent).unwrap();
        let index = parent_dir
          .entries
          .binary_search(
            DENO_COMPILE_GLOBAL_NODE_MODULES_DIR_NAME,
            case_sensitivity,
          )
          .unwrap();
        let npm_global_cache_dir_entry = parent_dir.entries.remove(index);

        // go up from the ancestors removing empty directories...
        // this is not as optimized as it could be
        let mut last_name =
          Cow::Borrowed(DENO_COMPILE_GLOBAL_NODE_MODULES_DIR_NAME);
        for ancestor in
          parent.ancestors().map(Some).chain(std::iter::once(None))
        {
          let dir = if let Some(ancestor) = ancestor {
            vfs.get_dir_mut(ancestor).unwrap()
          } else if cfg!(windows) {
            vfs.get_system_root_dir_mut()
          } else {
            break;
          };
          if let Ok(index) =
            dir.entries.binary_search(&last_name, case_sensitivity)
          {
            dir.entries.remove(index);
          }
          last_name = Cow::Owned(dir.name.clone());
          if !dir.entries.is_empty() {
            break;
          }
        }

        // now build the vfs and add the global cache dir entry there
        let mut built_vfs = vfs.build();
        built_vfs
          .entries
          .insert(npm_global_cache_dir_entry, case_sensitivity);
        built_vfs
      }
      CliNpmResolver::Byonm(_) => vfs.build(),
    }
  }
}

#[allow(clippy::too_many_arguments, reason = "private code")]
fn write_binary_bytes(
  mut file_writer: File,
  original_bin: Vec<u8>,
  data_section_bytes: Vec<u8>,
  compile_flags: &CompileFlags,
) -> Result<(), AnyError> {
  let target = resolve_compile_target(compile_flags);
  if target.contains("linux") {
    libsui::Elf::new(&original_bin).append(
      "d3n0l4nd",
      &data_section_bytes,
      &mut file_writer,
    )?;
  } else if target.contains("windows") {
    let mut pe = libsui::PortableExecutable::from(&original_bin)?;
    if let Some(icon) = compile_flags.icon.as_ref() {
      let icon = std::fs::read(icon)?;
      pe = pe.set_icon(&icon)?;
    }

    pe.write_resource("d3n0l4nd", data_section_bytes)?
      .build(&mut file_writer)?;
  } else if target.contains("darwin") {
    libsui::Macho::from(original_bin)?
      .write_section("d3n0l4nd", data_section_bytes)?
      .build_and_sign(&mut file_writer)?;
  }
  Ok(())
}

struct BinaryDataSectionSizes {
  metadata: usize,
  remote_modules: usize,
  vfs: usize,
}

/// Binary format:
/// * d3n0l4nd
/// * <metadata_len><metadata>
/// * <npm_snapshot_len><npm_snapshot>
/// * <specifiers>
/// * <redirects>
/// * <remote_modules>
/// * <vfs_headers_len><vfs_headers>
/// * <vfs_file_data_len><vfs_file_data>
/// * d3n0l4nd
#[allow(clippy::too_many_arguments, reason = "private code")]
fn serialize_binary_data_section(
  metadata: &Metadata,
  npm_snapshot: Option<SerializedNpmResolutionSnapshot>,
  specifiers: &SpecifierStoreForSerialization,
  redirects: &SpecifierDataStore<SpecifierId>,
  remote_modules: &SpecifierDataStore<RemoteModuleEntry<'_>>,
  vfs: &BuiltVfs,
) -> Result<(Vec<u8>, BinaryDataSectionSizes), AnyError> {
  let metadata = serde_json::to_string(metadata)?;
  let npm_snapshot =
    npm_snapshot.map(serialize_npm_snapshot).unwrap_or_default();
  let serialized_vfs = serde_json::to_string(&vfs.entries)?;

  let remote_modules_len = Cell::new(0);
  let metadata_len = Cell::new(0);
  let vfs_len = Cell::new(0);

  let bytes = capacity_builder::BytesBuilder::build(|builder| {
    builder.append(MAGIC_BYTES);
    // 1. Metadata
    {
      builder.append_le(metadata.len() as u64);
      builder.append(&metadata);
    }
    // 2. Npm snapshot
    {
      builder.append_le(npm_snapshot.len() as u64);
      builder.append(&npm_snapshot);
    }
    metadata_len.set(builder.len());
    // 3. Specifiers
    builder.append(specifiers);
    // 4. Redirects
    redirects.serialize(builder);
    // 5. Remote modules
    remote_modules.serialize(builder);
    remote_modules_len.set(builder.len() - metadata_len.get());
    // 6. VFS
    {
      builder.append_le(serialized_vfs.len() as u64);
      builder.append(&serialized_vfs);
      let vfs_bytes_len = vfs.files.iter().map(|f| f.len() as u64).sum::<u64>();
      builder.append_le(vfs_bytes_len);
      for file in &vfs.files {
        builder.append(file);
      }
    }
    vfs_len.set(builder.len() - remote_modules_len.get());

    // write the magic bytes at the end so we can use it
    // to make sure we've deserialized correctly
    builder.append(MAGIC_BYTES);
  })?;

  Ok((
    bytes,
    BinaryDataSectionSizes {
      metadata: metadata_len.get(),
      remote_modules: remote_modules_len.get(),
      vfs: vfs_len.get(),
    },
  ))
}

fn serialize_npm_snapshot(
  mut snapshot: SerializedNpmResolutionSnapshot,
) -> Vec<u8> {
  fn append_string(bytes: &mut Vec<u8>, string: &str) {
    let len = string.len() as u32;
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(string.as_bytes());
  }

  snapshot.packages.sort_by(|a, b| a.id.cmp(&b.id)); // determinism
  let ids_to_stored_ids = snapshot
    .packages
    .iter()
    .enumerate()
    .map(|(i, pkg)| (&pkg.id, i as u32))
    .collect::<HashMap<_, _>>();

  let mut root_packages: Vec<_> = snapshot.root_packages.iter().collect();
  root_packages.sort();
  let mut bytes = Vec::new();

  bytes.extend_from_slice(&(snapshot.packages.len() as u32).to_le_bytes());
  for pkg in &snapshot.packages {
    append_string(&mut bytes, &pkg.id.as_serialized());
  }

  bytes.extend_from_slice(&(root_packages.len() as u32).to_le_bytes());
  for (req, id) in root_packages {
    append_string(&mut bytes, &req.to_string());
    let id = ids_to_stored_ids.get(&id).unwrap();
    bytes.extend_from_slice(&id.to_le_bytes());
  }

  for pkg in &snapshot.packages {
    let deps_len = pkg.dependencies.len() as u32;
    bytes.extend_from_slice(&deps_len.to_le_bytes());
    let mut deps: Vec<_> = pkg.dependencies.iter().collect();
    deps.sort();
    for (req, id) in deps {
      append_string(&mut bytes, req);
      let id = ids_to_stored_ids.get(&id).unwrap();
      bytes.extend_from_slice(&id.to_le_bytes());
    }
  }

  bytes
}

fn get_denort_path(deno_exe: PathBuf) -> Option<OsString> {
  let mut denort = deno_exe;
  denort.set_file_name(if cfg!(windows) {
    "denort.exe"
  } else {
    "denort"
  });
  denort.exists().then(|| denort.into_os_string())
}

/// Where a denext release keeps its `denort`: the asset
/// `deno-denort-<version>-<target>.zip` (a zip holding `denort` /
/// `denort.exe`) of the GitHub release `denext-runtime-v<version>`, under
/// DENEXT_DENORT_BASE_URL (default: the Brainwires/deno releases). Returns
/// the cache path suffix under the DENO_DIR `dl` folder and the URL.
fn denext_denort(version: &str, target: &str) -> (String, String) {
  let asset = denext_denort_asset_name(version, target);
  let base = env::var("DENEXT_DENORT_BASE_URL")
    .ok()
    .filter(|b| !b.is_empty())
    .unwrap_or_else(|| DENEXT_DENORT_BASE_URL.to_string());
  (
    format!("denext/v{version}/{asset}"),
    format!(
      "{}/denext-runtime-v{version}/{asset}",
      base.trim_end_matches('/')
    ),
  )
}

const DENEXT_DENORT_BASE_URL: &str =
  "https://github.com/Brainwires/deno/releases/download";

/// An upstream runtime archive's path on dl.deno.land (and below the
/// download cache). A release names the upstream Deno version this fork is
/// built from ([`UPSTREAM_DENO_VERSION`]), never the fork's own `3.0.0-rc.N`:
/// that is no upstream release, and an upstream release of the same number
/// would be a different runtime.
fn deno_land_suffix(
  release_channel: ReleaseChannel,
  git_hash: &str,
  binary_name: &str,
) -> String {
  match release_channel {
    ReleaseChannel::Canary => format!("canary/{git_hash}/{binary_name}"),
    _ => format!("release/v{UPSTREAM_DENO_VERSION}/{binary_name}"),
  }
}

fn denext_denort_asset_name(version: &str, target: &str) -> String {
  format!("deno-denort-{version}-{target}.zip")
}

/// The `SHA256SUMS` asset of the release that `asset_url` (one of its
/// assets) belongs to.
fn denext_release_sums_url(asset_url: &str) -> String {
  match asset_url.rsplit_once('/') {
    Some((release, _)) => format!("{release}/SHA256SUMS"),
    None => "SHA256SUMS".to_string(),
  }
}

/// What [`load_verified_denext_denort`] asks its fetcher for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenortFetch {
  /// The `deno-denort-<version>-<target>.zip` asset.
  Archive,
  /// The release's `SHA256SUMS`.
  Sums,
}

/// A denext release's `denort` asset and where it and the release's
/// `SHA256SUMS` are downloaded from (the URLs are for error messages).
struct DenextDenortSource<'a> {
  asset: &'a str,
  archive_url: &'a str,
  sums_url: &'a str,
}

fn sha256_hex(data: &[u8]) -> String {
  use sha2::Digest;
  faster_hex::hex_string(&sha2::Sha256::digest(data))
}

/// The path beside a cached archive that records the SHA-256 it was
/// verified with.
fn verified_hash_path(archive_path: &Path) -> PathBuf {
  let mut name = archive_path.file_name().unwrap_or_default().to_os_string();
  name.push(".sha256");
  archive_path.with_file_name(name)
}

/// `asset`'s SHA-256 in a `sha256sum` listing (`<hex>  <name>`, or
/// `<hex> *<name>` in binary mode). `None` when the listing has no entry for
/// it; an error when it has two that disagree or a malformed hash for it.
fn sha256sums_entry(
  sums: &str,
  asset: &str,
) -> Result<Option<String>, AnyError> {
  let mut found: Option<String> = None;
  for line in sums.lines() {
    let Some((hash, name)) = line.trim_end().split_once(char::is_whitespace)
    else {
      continue;
    };
    let name = name.trim_start();
    let name = name.strip_prefix('*').unwrap_or(name);
    if name != asset {
      continue;
    }
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
      bail!("SHA256SUMS has a malformed hash for {asset}: '{hash}'");
    }
    let hash = hash.to_ascii_lowercase();
    match &found {
      Some(previous) if *previous != hash => {
        bail!(
          "SHA256SUMS lists {asset} twice with different hashes ({previous} and {hash})"
        )
      }
      _ => found = Some(hash),
    }
  }
  Ok(found)
}

/// A cached archive whose recorded hash (see [`verified_hash_path`]) still
/// matches its bytes. Anything else (no record, a different hash, an
/// unreadable file) is `None`: the cache is re-verified, never trusted.
fn read_verified_cache(archive_path: &Path) -> Option<Vec<u8>> {
  let recorded =
    std::fs::read_to_string(verified_hash_path(archive_path)).ok()?;
  let recorded = recorded.split_whitespace().next()?.to_ascii_lowercase();
  let bytes = std::fs::read(archive_path).ok()?;
  (sha256_hex(&bytes) == recorded).then_some(bytes)
}

/// denext (DENEXT.md item 130): a denext release's `denort` archive, verified
/// against the SHA-256 the same release's `SHA256SUMS` lists for it. A
/// missing `SHA256SUMS`, a missing entry or a mismatch is an error (fail
/// closed); nothing unverified is cached. A cached archive is used only
/// while its bytes still hash to the SHA-256 recorded beside it when it was
/// verified; otherwise it is checked against `SHA256SUMS` again (and
/// downloaded again if it does not match).
async fn load_verified_denext_denort(
  cache_path: &Path,
  source: &DenextDenortSource<'_>,
  fetch: impl AsyncFn(DenortFetch) -> Result<Option<Vec<u8>>, AnyError>,
) -> Result<Vec<u8>, AnyError> {
  let DenextDenortSource {
    asset,
    archive_url,
    sums_url,
  } = *source;
  if let Some(bytes) = read_verified_cache(cache_path) {
    return Ok(bytes);
  }

  let Some(sums) = fetch(DenortFetch::Sums).await? else {
    bail!(
      "The denext release has no SHA256SUMS ({sums_url} was not found), so \
       {asset} cannot be verified. Refusing to use it."
    );
  };
  let sums = String::from_utf8(sums)
    .with_context(|| format!("{sums_url} is not UTF-8 text"))?;
  let Some(expected) = sha256sums_entry(&sums, asset)? else {
    bail!(
      "{sums_url} has no entry for {asset}, so it cannot be verified. \
       Refusing to use it."
    );
  };

  let cached = std::fs::read(cache_path)
    .ok()
    .filter(|bytes| sha256_hex(bytes) == expected);
  let archive = match cached {
    Some(bytes) => bytes,
    None => match fetch(DenortFetch::Archive).await? {
      Some(bytes) => bytes,
      None => bail!("{archive_url} was not found"),
    },
  };
  let actual = sha256_hex(&archive);
  if actual != expected {
    bail!(
      "{asset} from {archive_url} does not match the release's SHA256SUMS: \
       expected SHA-256 {expected}, got {actual}. Refusing to use it."
    );
  }

  let sys = CliSys::default();
  if let Some(dir) = cache_path.parent() {
    std::fs::create_dir_all(dir)
      .with_context(|| format!("Creating {}", dir.display()))?;
  }
  atomic_write_file_with_retries(&sys, cache_path, &archive, CACHE_PERM)
    .with_context(|| format!("Writing {}", cache_path.display()))?;
  let hash_path = verified_hash_path(cache_path);
  atomic_write_file_with_retries(
    &sys,
    &hash_path,
    format!("{actual}  {asset}\n").as_bytes(),
    CACHE_PERM,
  )
  .with_context(|| format!("Writing {}", hash_path.display()))?;
  Ok(archive)
}

fn runtime_archive_name(
  prefix: &str,
  engine: &JavaScriptEngine,
  target: &str,
) -> String {
  format!("{prefix}{}-{target}.zip", engine.artifact_suffix())
}

fn get_dev_binary_path() -> Option<OsString> {
  env::var_os("DENORT_BIN").or_else(|| {
    env::current_exe().ok().and_then(|exec_path| {
      if exec_path
        .components()
        .any(|component| component == Component::Normal("target".as_ref()))
      {
        get_denort_path(exec_path)
      } else {
        None
      }
    })
  })
}

fn get_libdenort_path(deno_exe: PathBuf) -> Option<OsString> {
  let mut libdenort = deno_exe;
  if cfg!(target_os = "macos") {
    libdenort.set_file_name("libdenort.dylib");
  } else if cfg!(windows) {
    libdenort.set_file_name("denort.dll");
  } else {
    libdenort.set_file_name("libdenort.so");
  }
  libdenort.exists().then(|| libdenort.into_os_string())
}

fn get_dev_desktop_binary_path() -> Option<OsString> {
  env::var_os("DENORT_DESKTOP_BIN").or_else(|| {
    env::current_exe().ok().and_then(|exec_path| {
      if exec_path
        .components()
        .any(|component| component == Component::Normal("target".as_ref()))
      {
        // Prefer release libdenort (optimized) over debug.
        let target_dir = exec_path.parent().and_then(|p| p.parent());
        target_dir
          .and_then(|d| {
            get_libdenort_path(d.join("release").join("libdenort.dylib"))
          })
          .or_else(|| get_libdenort_path(exec_path.clone()))
      } else {
        None
      }
    })
  })
}

/// This function returns the environment variables specified
/// in the passed environment file.
fn get_file_env_vars(
  content: &str,
) -> Result<IndexMap<String, String>, deno_dotenv::ParseError> {
  let mut file_env_vars = IndexMap::new();
  for item in deno_dotenv::from_content_sanitized_iter_with_substitution(
    &CliSys::default(),
    content,
  )? {
    let Ok((key, val)) = item else {
      continue; // this failure will be warned about on load
    };
    file_env_vars.insert(key, val);
  }
  Ok(file_env_vars)
}

/// This function sets the subsystem field in the PE header to 2 (GUI subsystem)
/// For more information about the PE header: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
fn set_windows_binary_to_gui(bin: &mut [u8]) -> Result<(), AnyError> {
  // Get the PE header offset located in an i32 found at offset 60
  // See: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#ms-dos-stub-image-only
  let start_pe = u32::from_le_bytes((bin[60..64]).try_into()?);

  // Get image type (PE32 or PE32+) indicates whether the binary is 32 or 64 bit
  // The used offset and size values can be found here:
  // https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#optional-header-image-only
  let start_32 = start_pe as usize + 28;
  let magic_32 =
    u16::from_le_bytes(bin[(start_32)..(start_32 + 2)].try_into()?);

  let start_64 = start_pe as usize + 24;
  let magic_64 =
    u16::from_le_bytes(bin[(start_64)..(start_64 + 2)].try_into()?);

  // Take the standard fields size for the current architecture (32 or 64 bit)
  // This is the ofset for the Windows-Specific fields
  let standard_fields_size = if magic_32 == 0x10b {
    28
  } else if magic_64 == 0x20b {
    24
  } else {
    bail!("Could not find a matching magic field in the PE header")
  };

  // Set the subsystem field (offset 68) to 2 (GUI subsystem)
  // For all possible options, see: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format#optional-header-windows-specific-fields-image-only
  let subsystem_offset = 68;
  let subsystem_start =
    start_pe as usize + standard_fields_size + subsystem_offset;
  let subsystem: u16 = 2;
  bin[(subsystem_start)..(subsystem_start + 2)]
    .copy_from_slice(&subsystem.to_le_bytes());
  Ok(())
}

/// The update public key a desktop build bakes in: `desktop.update.publicKey`
/// trimmed, after checking that it parses. A desktop config that does not
/// parse is an error, never "no key": dropping the key silently would ship an
/// app whose updater is quietly off.
fn update_public_key_from_config<E: std::fmt::Display>(
  config: Result<&deno_config::deno_json::DesktopConfig, E>,
) -> Result<Option<String>, AnyError> {
  let config = config.map_err(|e| {
    deno_core::anyhow::anyhow!(
      "invalid desktop config, so desktop.update.publicKey can't be read: {e}"
    )
  })?;
  let Some(key) = config.update.as_ref().and_then(|u| u.public_key.clone())
  else {
    return Ok(None);
  };
  deno_runtime::ops::desktop_update::manifest::parse_public_key(&key).map_err(
    |e| deno_core::anyhow::anyhow!("invalid desktop.update.publicKey: {e}"),
  )?;
  Ok(Some(key.trim().to_string()))
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::path::Path;

  use deno_core::error::AnyError;

  use super::DenextDenortSource;
  use super::DenortFetch;
  use super::check_desktop_app_identity;
  use super::default_app_name;
  use super::denext_denort;
  use super::denext_denort_asset_name;
  use super::denext_release_sums_url;
  use super::deno_land_suffix;
  use super::load_verified_denext_denort;
  use super::runtime_archive_name;
  use super::sha256_hex;
  use super::sha256sums_entry;
  use super::update_public_key_from_config;
  use crate::args::JavaScriptEngine;

  #[test]
  fn deno_land_downloads_name_the_upstream_release() {
    use deno_lib::shared::ReleaseChannel;
    use deno_lib::version::DENO_VERSION;
    use deno_lib::version::UPSTREAM_DENO_VERSION;
    let suffix =
      deno_land_suffix(ReleaseChannel::Stable, "abc1234", "denort-x.zip");
    assert_eq!(
      suffix,
      format!("release/v{UPSTREAM_DENO_VERSION}/denort-x.zip")
    );
    if DENO_VERSION != UPSTREAM_DENO_VERSION {
      assert!(!suffix.contains(DENO_VERSION), "{suffix}");
    }
    assert_eq!(
      deno_land_suffix(ReleaseChannel::Rc, "abc1234", "denort-x.zip"),
      suffix
    );
    assert_eq!(
      deno_land_suffix(ReleaseChannel::Canary, "abc1234", "denort-x.zip"),
      "canary/abc1234/denort-x.zip"
    );
  }

  #[test]
  fn denext_denort_is_the_release_asset() {
    assert_eq!(
      denext_denort_asset_name("3.0.0-rc.1", "aarch64-pc-windows-msvc"),
      "deno-denort-3.0.0-rc.1-aarch64-pc-windows-msvc.zip"
    );
    if std::env::var_os("DENEXT_DENORT_BASE_URL").is_none() {
      assert_eq!(
        denext_denort("3.0.0-rc.1", "x86_64-unknown-linux-gnu"),
        (
          "denext/v3.0.0-rc.1/deno-denort-3.0.0-rc.1-x86_64-unknown-linux-gnu.zip"
            .to_string(),
          "https://github.com/Brainwires/deno/releases/download/denext-runtime-v3.0.0-rc.1/deno-denort-3.0.0-rc.1-x86_64-unknown-linux-gnu.zip"
            .to_string()
        )
      );
    }
    assert_eq!(
      denext_denort_asset_name("2.9.7-denext.20", "x86_64-pc-windows-msvc"),
      "deno-denort-2.9.7-denext.20-x86_64-pc-windows-msvc.zip"
    );
    if std::env::var_os("DENEXT_DENORT_BASE_URL").is_none() {
      assert_eq!(
        denext_denort("2.9.7-denext.20", "aarch64-apple-darwin"),
        (
          "denext/v2.9.7-denext.20/deno-denort-2.9.7-denext.20-aarch64-apple-darwin.zip"
            .to_string(),
          "https://github.com/Brainwires/deno/releases/download/denext-runtime-v2.9.7-denext.20/deno-denort-2.9.7-denext.20-aarch64-apple-darwin.zip"
            .to_string()
        )
      );
    }
  }

  const DENORT_ASSET: &str =
    "deno-denort-3.0.0-rc.7-x86_64-unknown-linux-gnu.zip";
  const DENORT_ARCHIVE_URL: &str = "https://example.test/denext-runtime-v3.0.0-rc.7/deno-denort-3.0.0-rc.7-x86_64-unknown-linux-gnu.zip";
  const DENORT_SUMS_URL: &str =
    "https://example.test/denext-runtime-v3.0.0-rc.7/SHA256SUMS";

  fn denort_source() -> DenextDenortSource<'static> {
    DenextDenortSource {
      asset: DENORT_ASSET,
      archive_url: DENORT_ARCHIVE_URL,
      sums_url: DENORT_SUMS_URL,
    }
  }

  /// A release serving `archive` and `sums` (`None`: a 404), counting the
  /// downloads of each.
  struct FakeRelease {
    archive: Vec<u8>,
    sums: Option<String>,
    archive_fetches: Cell<usize>,
    sums_fetches: Cell<usize>,
  }

  impl FakeRelease {
    fn new(archive: &[u8], sums: Option<String>) -> Self {
      Self {
        archive: archive.to_vec(),
        sums,
        archive_fetches: Cell::new(0),
        sums_fetches: Cell::new(0),
      }
    }

    async fn load(&self, cache: &Path) -> Result<Vec<u8>, AnyError> {
      load_verified_denext_denort(cache, &denort_source(), async |what| {
        match what {
          DenortFetch::Archive => {
            self.archive_fetches.set(self.archive_fetches.get() + 1);
            Ok(Some(self.archive.clone()))
          }
          DenortFetch::Sums => {
            self.sums_fetches.set(self.sums_fetches.get() + 1);
            Ok(self.sums.clone().map(String::into_bytes))
          }
        }
      })
      .await
    }
  }

  fn sums_listing(entries: &[(&str, &[u8])]) -> String {
    entries
      .iter()
      .map(|(name, bytes)| format!("{}  {name}\n", sha256_hex(bytes)))
      .collect()
  }

  #[test]
  fn release_sums_url_is_beside_the_asset() {
    assert_eq!(denext_release_sums_url(DENORT_ARCHIVE_URL), DENORT_SUMS_URL);
  }

  #[test]
  fn sha256sums_entry_reads_the_listing() {
    let hash = "AB".repeat(32);
    let sums = format!(
      "{}  deno-cli.zip\n{hash} *{DENORT_ASSET}\n",
      "cd".repeat(32)
    );
    assert_eq!(
      sha256sums_entry(&sums, DENORT_ASSET).unwrap(),
      Some("ab".repeat(32))
    );
    assert_eq!(sha256sums_entry(&sums, "other.zip").unwrap(), None);
    // A prefix of a listed name is not an entry.
    assert_eq!(
      sha256sums_entry(&sums, "deno-denort-3.0.0-rc.7").unwrap(),
      None
    );
    let conflicting = format!(
      "{hash}  {DENORT_ASSET}\n{}  {DENORT_ASSET}\n",
      "cd".repeat(32)
    );
    let err = sha256sums_entry(&conflicting, DENORT_ASSET)
      .unwrap_err()
      .to_string();
    assert!(err.contains("twice"), "{err}");
    let malformed = format!("xyz  {DENORT_ASSET}\n");
    assert!(sha256sums_entry(&malformed, DENORT_ASSET).is_err());
  }

  #[tokio::test]
  async fn denext_denort_with_a_good_hash_is_used_and_cached() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join("denext/v3.0.0-rc.7").join(DENORT_ASSET);
    let archive = b"the real denort zip";
    let release = FakeRelease::new(
      archive,
      Some(sums_listing(&[
        ("deno-cli-3.0.0-rc.7-x86_64-unknown-linux-gnu.zip", b"cli"),
        (DENORT_ASSET, archive),
      ])),
    );
    assert_eq!(release.load(&cache).await.unwrap(), archive);
    assert_eq!(std::fs::read(&cache).unwrap(), archive);
    let recorded =
      std::fs::read_to_string(super::verified_hash_path(&cache)).unwrap();
    assert!(recorded.starts_with(&sha256_hex(archive)), "{recorded}");

    // The verified cache is used again without any download.
    assert_eq!(release.load(&cache).await.unwrap(), archive);
    assert_eq!(release.archive_fetches.get(), 1);
    assert_eq!(release.sums_fetches.get(), 1);
  }

  #[tokio::test]
  async fn denext_denort_tampered_archive_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join(DENORT_ASSET);
    let release = FakeRelease::new(
      b"a swapped denort zip",
      Some(sums_listing(&[(DENORT_ASSET, b"the real denort zip")])),
    );
    let err = format!("{:#}", release.load(&cache).await.unwrap_err());
    assert!(err.contains(DENORT_ASSET), "{err}");
    assert!(err.contains(&sha256_hex(b"the real denort zip")), "{err}");
    assert!(err.contains(&sha256_hex(b"a swapped denort zip")), "{err}");
    assert!(err.contains("does not match"), "{err}");
    // Nothing unverified is cached.
    assert!(!cache.exists());
    assert!(!super::verified_hash_path(&cache).exists());
  }

  #[tokio::test]
  async fn denext_denort_missing_sums_entry_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join(DENORT_ASSET);
    let release = FakeRelease::new(
      b"the real denort zip",
      Some(sums_listing(&[(
        "deno-cli-3.0.0-rc.7-x86_64-unknown-linux-gnu.zip",
        b"cli",
      )])),
    );
    let err = format!("{:#}", release.load(&cache).await.unwrap_err());
    assert!(err.contains("has no entry for"), "{err}");
    assert!(err.contains(DENORT_ASSET), "{err}");
    assert_eq!(release.archive_fetches.get(), 0);
    assert!(!cache.exists());
  }

  #[tokio::test]
  async fn denext_denort_missing_sums_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join(DENORT_ASSET);
    let release = FakeRelease::new(b"the real denort zip", None);
    let err = format!("{:#}", release.load(&cache).await.unwrap_err());
    assert!(err.contains("no SHA256SUMS"), "{err}");
    assert!(err.contains(DENORT_SUMS_URL), "{err}");
    assert!(err.contains(DENORT_ASSET), "{err}");
    assert_eq!(release.archive_fetches.get(), 0);
    assert!(!cache.exists());
  }

  #[tokio::test]
  async fn denext_denort_cache_is_not_trusted_blindly() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join(DENORT_ASSET);
    let archive = b"the real denort zip";
    let sums = sums_listing(&[(DENORT_ASSET, archive)]);

    // A cached archive with no recorded hash (an older CLI's cache) that
    // does not match SHA256SUMS is replaced with a verified download.
    std::fs::write(&cache, b"a swapped denort zip").unwrap();
    let release = FakeRelease::new(archive, Some(sums.clone()));
    assert_eq!(release.load(&cache).await.unwrap(), archive);
    assert_eq!(release.archive_fetches.get(), 1);
    assert_eq!(std::fs::read(&cache).unwrap(), archive);

    // A verified cache changed afterwards no longer matches its record: it
    // is checked again, and with the release serving a swapped archive too,
    // refused.
    std::fs::write(&cache, b"a swapped denort zip").unwrap();
    let swapped = FakeRelease::new(b"a swapped denort zip", Some(sums.clone()));
    let err = format!("{:#}", swapped.load(&cache).await.unwrap_err());
    assert!(err.contains("does not match"), "{err}");
    assert_eq!(swapped.sums_fetches.get(), 1);

    // A cached archive with no record that matches SHA256SUMS is used
    // without downloading it again, and recorded.
    std::fs::write(&cache, archive).unwrap();
    std::fs::remove_file(super::verified_hash_path(&cache)).unwrap();
    let release = FakeRelease::new(b"unused", Some(sums));
    assert_eq!(release.load(&cache).await.unwrap(), archive);
    assert_eq!(release.archive_fetches.get(), 0);
    assert!(super::verified_hash_path(&cache).exists());
  }

  #[test]
  fn desktop_app_origin_requires_an_identifier() {
    // No origin: the identifier stays optional (the CLI synthesizes one).
    check_desktop_app_identity(None, None).unwrap();
    check_desktop_app_identity(None, Some("com.acme.app")).unwrap();
    check_desktop_app_identity(Some("acme://app"), Some("com.acme.app"))
      .unwrap();
    // A configured origin without an identifier is a build error that names
    // both keys and how to fix it.
    let err = check_desktop_app_identity(Some("acme://app"), None)
      .unwrap_err()
      .to_string();
    assert!(err.contains("desktop.app.origin \"acme://app\""), "{err}");
    assert!(err.contains("requires desktop.app.identifier"), "{err}");
    assert!(
      err.contains("\"identifier\": \"com.example.myapp\""),
      "{err}"
    );
  }

  #[test]
  fn runtime_archive_names_include_engine_suffix() {
    let target = "aarch64-apple-darwin";
    assert_eq!(
      runtime_archive_name("denort", &JavaScriptEngine::V8, target),
      "denort-aarch64-apple-darwin.zip"
    );
    assert_eq!(
      runtime_archive_name("denort", &JavaScriptEngine::QuickJs, target),
      "denort-quickjs-aarch64-apple-darwin.zip"
    );
    assert_eq!(
      runtime_archive_name("libdenort", &JavaScriptEngine::QuickJs, target),
      "libdenort-quickjs-aarch64-apple-darwin.zip"
    );
  }

  #[test]
  fn default_app_name_strips_only_deno_added_extensions() {
    assert_eq!(default_app_name("speedgraph.exe", false), "speedgraph");
    assert_eq!(default_app_name("speedgraph.so", false), "speedgraph.so");

    assert_eq!(default_app_name("speedgraph.dll", true), "speedgraph");
    assert_eq!(default_app_name("speedgraph.dylib", true), "speedgraph");
    assert_eq!(default_app_name("speedgraph.so", true), "speedgraph");
    assert_eq!(default_app_name("speed.graph.so", true), "speed.graph");
  }

  #[test]
  fn update_public_key_comes_from_a_valid_config_only() {
    use deno_config::deno_json::DesktopConfig;
    use deno_config::deno_json::DesktopUpdateConfig;
    const KEY: &str = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEu5luQoCWV/y9l8enmIN8mS+FDQRqxgQXJMYMm63fzCJeBOEIXLPtHA10rb+x74DWIOlqf1C4ao4xBJb+vnyQsw==";
    let with_key = |key: &str| DesktopConfig {
      update: Some(DesktopUpdateConfig {
        public_key: Some(key.to_string()),
      }),
      ..Default::default()
    };
    // No update section, or no key: no key, not an error.
    assert_eq!(
      update_public_key_from_config::<String>(Ok(&DesktopConfig::default()))
        .unwrap(),
      None
    );
    assert_eq!(
      update_public_key_from_config::<String>(Ok(&DesktopConfig {
        update: Some(DesktopUpdateConfig { public_key: None }),
        ..Default::default()
      }))
      .unwrap(),
      None
    );
    // A valid key is trimmed; a PEM is accepted as written.
    assert_eq!(
      update_public_key_from_config::<String>(Ok(&with_key(&format!(
        "  {KEY}\n"
      ))))
      .unwrap()
      .as_deref(),
      Some(KEY)
    );
    let pem = format!(
      "-----BEGIN PUBLIC KEY-----\n{}\n{}\n-----END PUBLIC KEY-----",
      &KEY[..64],
      &KEY[64..]
    );
    assert_eq!(
      update_public_key_from_config::<String>(Ok(&with_key(&pem)))
        .unwrap()
        .as_deref(),
      Some(pem.as_str())
    );
    // A key that does not parse fails the build.
    let err = update_public_key_from_config::<String>(Ok(&with_key("nope")))
      .unwrap_err()
      .to_string();
    assert!(err.contains("invalid desktop.update.publicKey"), "{err}");
    // A desktop config that does not parse fails the build too: the key is
    // never dropped silently (an app whose updater is quietly off).
    let err = update_public_key_from_config::<String>(Err(
      "desktop.update: invalid type".to_string(),
    ))
    .unwrap_err()
    .to_string();
    assert!(err.contains("invalid desktop config"), "{err}");
    assert!(err.contains("desktop.update: invalid type"), "{err}");
  }
}
