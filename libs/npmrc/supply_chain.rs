// Copyright 2018-2026 the Deno authors. MIT license.

//! npm supply-chain gates (denext): the malicious-version gate
//! (`advisory-check`), the low-download gate on `deno add`
//! (`low-download-threshold`, `allowed-unpopular-packages`), confined
//! lifecycle scripts (`jail-scripts`) and `paranoid`, which turns all of them
//! on and makes each one fail closed.
//!
//! Each source (`.npmrc`, `npm_config_*` environment variables, deno.json's
//! `supplyChain`) yields a [`SupplyChainSettings`] with only what it sets;
//! [`resolve_supply_chain`] combines them into the [`SupplyChainConfig`] every
//! consumer reads, so no consumer has to know about `paranoid`.

use sys_traits::EnvVar;

/// The default `low-download-threshold`: weekly downloads below which
/// `deno add` asks before adding a package.
pub const DEFAULT_LOW_DOWNLOAD_THRESHOLD: u64 = 1000;

/// `advisory-check`: whether package versions listed as malicious (OSV
/// `MAL-*` advisories) are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AdvisoryCheck {
  /// No advisory queries.
  Off,
  /// A malicious version is refused; when the advisory service can't be
  /// reached, a warning is printed and the install continues (the default).
  #[default]
  On,
  /// Like `On`, but an unreachable advisory service refuses the install.
  Required,
}

impl AdvisoryCheck {
  pub fn parse(value: &str) -> Option<Self> {
    match value.trim() {
      "off" | "false" => Some(Self::Off),
      "on" | "true" => Some(Self::On),
      "required" => Some(Self::Required),
      _ => None,
    }
  }

  pub fn as_str(&self) -> &'static str {
    match self {
      Self::Off => "off",
      Self::On => "on",
      Self::Required => "required",
    }
  }
}

/// The supply-chain settings one source sets. `None` means "not set here".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SupplyChainSettings {
  pub paranoid: Option<bool>,
  pub jail_scripts: Option<bool>,
  pub advisory_check: Option<AdvisoryCheck>,
  pub low_download_threshold: Option<u64>,
  /// Package names or `@scope/*` / `prefix*` globs exempt from the
  /// low-download gate.
  pub allowed_unpopular_packages: Vec<String>,
  /// `trust-policy=off` written explicitly (a weaker value than `paranoid`
  /// requires).
  pub trust_policy_off: bool,
}

/// npm's boolean config values as text.
fn parse_bool(value: &str) -> Option<bool> {
  match value.trim() {
    "true" | "1" => Some(true),
    "false" | "0" | "" => Some(false),
    _ => None,
  }
}

pub(crate) fn parse_list(value: &str) -> Vec<String> {
  value
    .split(',')
    .map(|s| s.trim())
    .filter(|s| !s.is_empty())
    .map(|s| s.to_string())
    .collect()
}

impl SupplyChainSettings {
  /// Applies one `.npmrc` / `npm_config_*` key. Returns whether the key is a
  /// supply-chain key.
  pub(crate) fn apply_key(&mut self, key: &str, value: &str) -> bool {
    match key {
      "paranoid" => {
        if let Some(value) = parse_bool(value) {
          self.paranoid = Some(value);
        }
      }
      "jail-scripts" => {
        if let Some(value) = parse_bool(value) {
          self.jail_scripts = Some(value);
        }
      }
      "advisory-check" => {
        if let Some(value) = AdvisoryCheck::parse(value) {
          self.advisory_check = Some(value);
        }
      }
      "low-download-threshold" => {
        if let Ok(value) = value.trim().parse::<u64>() {
          self.low_download_threshold = Some(value);
        }
      }
      "allowed-unpopular-packages" => {
        for name in parse_list(value) {
          if !self.allowed_unpopular_packages.contains(&name) {
            self.allowed_unpopular_packages.push(name);
          }
        }
      }
      _ => return false,
    }
    true
  }

  /// The settings npm's `npm_config_<key>` (and `NPM_CONFIG_<KEY>`)
  /// environment variables set.
  pub fn from_env(sys: &impl EnvVar) -> Self {
    let mut settings = Self::default();
    for key in [
      "paranoid",
      "jail-scripts",
      "advisory-check",
      "low-download-threshold",
      "allowed-unpopular-packages",
    ] {
      let snake = key.replace('-', "_");
      for name in [
        format!("npm_config_{snake}"),
        format!("NPM_CONFIG_{}", snake.to_ascii_uppercase()),
      ] {
        if let Ok(value) = sys.env_var(&name) {
          settings.apply_key(key, &value);
          break;
        }
      }
    }
    if let Ok(value) = sys
      .env_var("npm_config_trust_policy")
      .or_else(|_| sys.env_var("NPM_CONFIG_TRUST_POLICY"))
      && value.trim() == "off"
    {
      settings.trust_policy_off = true;
    }
    settings
  }

  /// Combines two sources: where both set a member, the stricter value wins,
  /// so one file can't quietly loosen what another one sets.
  pub fn merge_strictest(mut self, other: Self) -> Self {
    fn max_opt<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
      match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
      }
    }
    self.paranoid = max_opt(self.paranoid, other.paranoid);
    self.jail_scripts = max_opt(self.jail_scripts, other.jail_scripts);
    self.advisory_check = max_opt(self.advisory_check, other.advisory_check);
    // a threshold of 0 disables the gate: any other value is stricter
    self.low_download_threshold =
      match (self.low_download_threshold, other.low_download_threshold) {
        (Some(0), Some(b)) => Some(b),
        (Some(a), Some(0)) => Some(a),
        (a, b) => max_opt(a, b),
      };
    for name in other.allowed_unpopular_packages {
      if !self.allowed_unpopular_packages.contains(&name) {
        self.allowed_unpopular_packages.push(name);
      }
    }
    self.trust_policy_off |= other.trust_policy_off;
    self
  }
}

/// The supply-chain gates as every consumer applies them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupplyChainConfig {
  /// `paranoid`: every gate is on and fails closed.
  pub paranoid: bool,
  /// Approved lifecycle scripts run confined (`jail-scripts`).
  pub jail_scripts: bool,
  pub advisory_check: AdvisoryCheck,
  /// Weekly downloads below which `deno add` asks; `0` disables the gate.
  pub low_download_threshold: u64,
  /// Whether an unknown download count (the downloads API can't be reached
  /// or has no count) refuses the add instead of letting it through.
  pub low_downloads_fail_closed: bool,
  pub allowed_unpopular_packages: Vec<String>,
  /// The settings the `.npmrc` files set, kept so the workspace can resolve
  /// them again together with the environment and deno.json.
  pub npmrc_settings: SupplyChainSettings,
}

impl Default for SupplyChainConfig {
  fn default() -> Self {
    Self {
      paranoid: false,
      jail_scripts: false,
      advisory_check: AdvisoryCheck::On,
      low_download_threshold: DEFAULT_LOW_DOWNLOAD_THRESHOLD,
      low_downloads_fail_closed: false,
      allowed_unpopular_packages: Vec::new(),
      npmrc_settings: Default::default(),
    }
  }
}

impl SupplyChainConfig {
  /// Whether `name` is exempt from the low-download gate by
  /// `allowed-unpopular-packages` (a name, an `npm:` name, `@scope/*` or a
  /// `prefix*` glob).
  pub fn is_allowed_unpopular(&self, name: &str) -> bool {
    self
      .allowed_unpopular_packages
      .iter()
      .any(|pattern| package_name_matches(pattern, name))
  }
}

/// Matches an npm package name against a name or a trailing-`*` glob.
pub fn package_name_matches(pattern: &str, name: &str) -> bool {
  let pattern = pattern.strip_prefix("npm:").unwrap_or(pattern);
  match pattern.strip_suffix('*') {
    Some(prefix) => name.starts_with(prefix),
    None => pattern == name,
  }
}

/// A member set explicitly to something weaker than `paranoid` requires.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error(
  "`paranoid` is on, but {setting} in {source_name} weakens it. Remove {setting} (paranoid sets {required}), or turn paranoid off."
)]
pub struct SupplyChainConfigError {
  pub setting: String,
  pub source_name: String,
  pub required: &'static str,
}

/// Resolves the settings of every source (each with a name for errors) into
/// the gates' configuration.
///
/// `paranoid` overrides the members: jail-scripts on, `advisory-check`
/// required, unknown download counts refuse, and the low-download gate can't
/// be turned off. A member another source sets explicitly to a weaker value is
/// an error rather than silently overridden, so a config can't look strict
/// while being loose (`allowed-unpopular-packages` stays honored: it names
/// packages, it doesn't loosen the gate).
pub fn resolve_supply_chain(
  sources: &[(&str, &SupplyChainSettings)],
  npmrc_settings: SupplyChainSettings,
) -> Result<SupplyChainConfig, SupplyChainConfigError> {
  let merged = sources
    .iter()
    .fold(SupplyChainSettings::default(), |acc, (_, s)| {
      acc.merge_strictest((*s).clone())
    });
  let paranoid = merged.paranoid.unwrap_or(false);
  if paranoid {
    for (source_name, settings) in sources {
      let conflict =
        |setting: String, required: &'static str| SupplyChainConfigError {
          setting,
          source_name: source_name.to_string(),
          required,
        };
      if settings.jail_scripts == Some(false) {
        return Err(conflict("jail-scripts=false".into(), "jail-scripts=true"));
      }
      if let Some(check) = settings.advisory_check
        && check != AdvisoryCheck::Required
      {
        return Err(conflict(
          format!("advisory-check={}", check.as_str()),
          "advisory-check=required",
        ));
      }
      if settings.low_download_threshold == Some(0) {
        return Err(conflict(
          "low-download-threshold=0".into(),
          "a non-zero threshold",
        ));
      }
      if settings.trust_policy_off {
        return Err(conflict(
          "trust-policy=off".into(),
          "trust-policy=no-downgrade",
        ));
      }
    }
  }
  Ok(SupplyChainConfig {
    paranoid,
    jail_scripts: paranoid || merged.jail_scripts.unwrap_or(false),
    advisory_check: if paranoid {
      AdvisoryCheck::Required
    } else {
      merged.advisory_check.unwrap_or_default()
    },
    low_download_threshold: merged
      .low_download_threshold
      .unwrap_or(DEFAULT_LOW_DOWNLOAD_THRESHOLD),
    low_downloads_fail_closed: paranoid,
    allowed_unpopular_packages: merged.allowed_unpopular_packages,
    npmrc_settings,
  })
}

#[cfg(test)]
mod test {
  use pretty_assertions::assert_eq;
  use sys_traits::EnvSetVar;
  use sys_traits::impls::InMemorySys;

  use super::*;

  fn settings(pairs: &[(&str, &str)]) -> SupplyChainSettings {
    let mut s = SupplyChainSettings::default();
    for (k, v) in pairs {
      assert!(s.apply_key(k, v), "{k}");
    }
    s
  }

  #[test]
  fn defaults() {
    let config = resolve_supply_chain(&[], Default::default()).unwrap();
    assert_eq!(config, SupplyChainConfig::default());
    assert!(!config.jail_scripts);
    assert_eq!(config.advisory_check, AdvisoryCheck::On);
    assert_eq!(config.low_download_threshold, 1000);
    assert!(!config.low_downloads_fail_closed);
  }

  #[test]
  fn paranoid_turns_every_member_on() {
    let s = settings(&[("paranoid", "true")]);
    let config = resolve_supply_chain(&[(".npmrc", &s)], s.clone()).unwrap();
    assert!(config.paranoid);
    assert!(config.jail_scripts);
    assert_eq!(config.advisory_check, AdvisoryCheck::Required);
    assert!(config.low_downloads_fail_closed);
    assert_eq!(config.low_download_threshold, 1000);
  }

  #[test]
  fn paranoid_keeps_stricter_members() {
    let s = settings(&[
      ("paranoid", "true"),
      ("low-download-threshold", "50000"),
      ("advisory-check", "required"),
      ("jail-scripts", "true"),
    ]);
    let config = resolve_supply_chain(&[(".npmrc", &s)], s.clone()).unwrap();
    assert_eq!(config.low_download_threshold, 50000);
  }

  #[test]
  fn paranoid_refuses_weaker_members_from_any_source() {
    let paranoid = settings(&[("paranoid", "true")]);
    for (key, value) in [
      ("jail-scripts", "false"),
      ("advisory-check", "off"),
      ("advisory-check", "on"),
      ("low-download-threshold", "0"),
    ] {
      let weak = settings(&[(key, value)]);
      let err = resolve_supply_chain(
        &[("environment", &paranoid), ("deno.json", &weak)],
        Default::default(),
      )
      .unwrap_err();
      assert_eq!(err.source_name, "deno.json");
      assert_eq!(err.setting, format!("{key}={value}"));
    }
    let trust_off = SupplyChainSettings {
      trust_policy_off: true,
      ..Default::default()
    };
    let err = resolve_supply_chain(
      &[(".npmrc", &trust_off), ("deno.json", &paranoid)],
      Default::default(),
    )
    .unwrap_err();
    assert_eq!(err.setting, "trust-policy=off");
    // allowed-unpopular-packages names packages; it stays honored
    let names = settings(&[("allowed-unpopular-packages", "a, @b/*")]);
    let config = resolve_supply_chain(
      &[("deno.json", &paranoid), (".npmrc", &names)],
      Default::default(),
    )
    .unwrap();
    assert!(config.is_allowed_unpopular("a"));
    assert!(config.is_allowed_unpopular("@b/c"));
    assert!(!config.is_allowed_unpopular("ab"));
  }

  #[test]
  fn paranoid_can_not_be_turned_off_by_another_source() {
    let on = settings(&[("paranoid", "true")]);
    let off = settings(&[("paranoid", "false")]);
    let config = resolve_supply_chain(
      &[("environment", &on), ("deno.json", &off)],
      Default::default(),
    )
    .unwrap();
    assert!(config.paranoid);
  }

  #[test]
  fn strictest_value_wins_without_paranoid() {
    let a = settings(&[("advisory-check", "off"), ("jail-scripts", "false")]);
    let b = settings(&[
      ("advisory-check", "required"),
      ("jail-scripts", "true"),
      ("low-download-threshold", "0"),
    ]);
    let c = settings(&[("low-download-threshold", "10")]);
    let config = resolve_supply_chain(
      &[("a", &a), ("b", &b), ("c", &c)],
      Default::default(),
    )
    .unwrap();
    assert_eq!(config.advisory_check, AdvisoryCheck::Required);
    assert!(config.jail_scripts);
    assert_eq!(config.low_download_threshold, 10);
    // only off: off
    let config =
      resolve_supply_chain(&[("a", &a)], Default::default()).unwrap();
    assert_eq!(config.advisory_check, AdvisoryCheck::Off);
    assert!(!config.jail_scripts);
  }

  #[test]
  fn env_settings() {
    let sys = InMemorySys::default();
    sys.env_set_var("npm_config_paranoid", "true");
    sys.env_set_var("NPM_CONFIG_ADVISORY_CHECK", "required");
    sys.env_set_var("npm_config_allowed_unpopular_packages", "x,y");
    sys.env_set_var("npm_config_low_download_threshold", "nonsense");
    let s = SupplyChainSettings::from_env(&sys);
    assert_eq!(s.paranoid, Some(true));
    assert_eq!(s.advisory_check, Some(AdvisoryCheck::Required));
    assert_eq!(s.allowed_unpopular_packages, vec!["x", "y"]);
    assert_eq!(s.low_download_threshold, None);
  }

  #[test]
  fn name_patterns() {
    assert!(package_name_matches("npm:left-pad", "left-pad"));
    assert!(package_name_matches("@org/*", "@org/a"));
    assert!(!package_name_matches("@org/*", "@orgx/a"));
    assert!(package_name_matches("lodash*", "lodash.merge"));
    assert!(!package_name_matches("lodash", "lodash.merge"));
  }
}
