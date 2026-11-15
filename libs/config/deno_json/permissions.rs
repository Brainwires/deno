// Copyright 2018-2026 the Deno authors. MIT license.

use indexmap::IndexMap;
use serde::Deserialize;
use url::Url;

use super::UndefinedPermissionError;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PermissionConfigValue {
  All,
  Some(Vec<String>),
  None,
}

impl<'de> serde::Deserialize<'de> for PermissionConfigValue {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: serde::Deserializer<'de>,
  {
    struct Visitor;
    impl<'d> serde::de::Visitor<'d> for Visitor {
      type Value = PermissionConfigValue;

      fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "either an array or bool")
      }

      fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
      where
        E: serde::de::Error,
      {
        if v {
          Ok(PermissionConfigValue::All)
        } else {
          Ok(PermissionConfigValue::None)
        }
      }

      fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
      where
        A: serde::de::SeqAccess<'d>,
      {
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(8));
        while let Some(element) = seq.next_element::<String>()? {
          out.push(element);
        }

        if out.is_empty() {
          Ok(PermissionConfigValue::None)
        } else {
          Ok(PermissionConfigValue::Some(out))
        }
      }

      fn visit_unit<E>(self) -> Result<Self::Value, E>
      where
        E: serde::de::Error,
      {
        Ok(PermissionConfigValue::None)
      }
    }
    deserializer.deserialize_any(Visitor)
  }
}

#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct AllowDenyPermissionConfig {
  pub allow: Option<PermissionConfigValue>,
  pub deny: Option<PermissionConfigValue>,
}

impl AllowDenyPermissionConfig {
  pub fn is_none(&self) -> bool {
    self.allow.is_none() && self.deny.is_none()
  }
}

#[derive(Deserialize, Default, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct AllowDenyIgnorePermissionConfig {
  pub allow: Option<PermissionConfigValue>,
  pub deny: Option<PermissionConfigValue>,
  pub ignore: Option<PermissionConfigValue>,
}

impl AllowDenyIgnorePermissionConfig {
  pub fn is_none(&self) -> bool {
    self.allow.is_none() && self.deny.is_none() && self.ignore.is_none()
  }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum AllowDenyPermissionConfigValue {
  Boolean(bool),
  AllowList(Vec<String>),
  Object(AllowDenyPermissionConfig),
}

fn deserialize_allow_deny<'de, D: serde::Deserializer<'de>>(
  de: D,
) -> Result<AllowDenyPermissionConfig, D::Error> {
  AllowDenyPermissionConfigValue::deserialize(de).map(|value| match value {
    AllowDenyPermissionConfigValue::Boolean(b) => AllowDenyPermissionConfig {
      allow: Some(if b {
        PermissionConfigValue::All
      } else {
        PermissionConfigValue::None
      }),
      deny: None,
    },
    AllowDenyPermissionConfigValue::AllowList(allow) => {
      AllowDenyPermissionConfig {
        allow: Some(if allow.is_empty() {
          PermissionConfigValue::None
        } else {
          PermissionConfigValue::Some(allow)
        }),
        deny: None,
      }
    }
    AllowDenyPermissionConfigValue::Object(obj) => obj,
  })
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum AllowDenyIgnorePermissionConfigValue {
  Boolean(bool),
  AllowList(Vec<String>),
  Object(AllowDenyIgnorePermissionConfig),
}

fn deserialize_allow_deny_ignore<'de, D: serde::Deserializer<'de>>(
  de: D,
) -> Result<AllowDenyIgnorePermissionConfig, D::Error> {
  AllowDenyIgnorePermissionConfigValue::deserialize(de).map(|value| match value
  {
    AllowDenyIgnorePermissionConfigValue::Boolean(b) => {
      AllowDenyIgnorePermissionConfig {
        allow: Some(if b {
          PermissionConfigValue::All
        } else {
          PermissionConfigValue::None
        }),
        deny: None,
        ignore: None,
      }
    }
    AllowDenyIgnorePermissionConfigValue::AllowList(allow) => {
      AllowDenyIgnorePermissionConfig {
        allow: Some(if allow.is_empty() {
          PermissionConfigValue::None
        } else {
          PermissionConfigValue::Some(allow)
        }),
        deny: None,
        ignore: None,
      }
    }
    AllowDenyIgnorePermissionConfigValue::Object(obj) => obj,
  })
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Hash)]
#[serde(untagged)]
pub enum PermissionNameOrObject {
  Name(String),
  Object(Box<PermissionsObject>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionsObjectWithBase {
  pub base: Url,
  pub permissions: PermissionsObject,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Default, Hash)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsObject {
  #[serde(default)]
  pub all: Option<bool>,
  #[serde(default, deserialize_with = "deserialize_allow_deny_ignore")]
  pub read: AllowDenyIgnorePermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub write: AllowDenyPermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub import: AllowDenyPermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny_ignore")]
  pub env: AllowDenyIgnorePermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub net: AllowDenyPermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub run: AllowDenyPermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub ffi: AllowDenyPermissionConfig,
  #[serde(default, deserialize_with = "deserialize_allow_deny")]
  pub sys: AllowDenyPermissionConfig,
}

impl PermissionsObject {
  /// Returns true if the permissions object is empty (no permissions are set).
  pub fn is_empty(&self) -> bool {
    self.all.is_none()
      && self.read.is_none()
      && self.write.is_none()
      && self.import.is_none()
      && self.env.is_none()
      && self.net.is_none()
      && self.run.is_none()
      && self.ffi.is_none()
      && self.sys.is_none()
  }
}

/// denext: the reserved `kernelSandbox` key of deno.json `permissions` (not
/// a permission set): the project's kernel-sandbox mode for every run, below
/// `--kernel-sandbox` (DENEXT.md item 420).
pub const KERNEL_SANDBOX_KEY: &str = "kernelSandbox";

/// The values `permissions.kernelSandbox` accepts.
pub const KERNEL_SANDBOX_CONFIG_VALUES: &[&str] =
  &["off", "warn", "auto", "enforce", "broker"];

#[derive(Clone, Debug, Default)]
pub struct PermissionsConfig {
  pub sets: IndexMap<String, PermissionsObjectWithBase>,
  /// denext: `permissions.kernelSandbox`, one of
  /// [`KERNEL_SANDBOX_CONFIG_VALUES`] (checked when parsed).
  pub kernel_sandbox: Option<String>,
}

impl PermissionsConfig {
  pub fn parse(
    mut value: serde_json::Value,
    base: &Url,
  ) -> Result<Self, serde_json::Error> {
    let kernel_sandbox = match value
      .as_object_mut()
      .and_then(|map| map.remove(KERNEL_SANDBOX_KEY))
    {
      None => None,
      Some(serde_json::Value::String(mode))
        if KERNEL_SANDBOX_CONFIG_VALUES.contains(&mode.as_str()) =>
      {
        Some(mode)
      }
      Some(other) => {
        return Err(<serde_json::Error as serde::de::Error>::custom(format!(
          "invalid value {other} for \"permissions.{KERNEL_SANDBOX_KEY}\": expected one of \"{}\"",
          KERNEL_SANDBOX_CONFIG_VALUES.join("\", \"")
        )));
      }
    };
    let sets: IndexMap<String, PermissionsObject> =
      serde_json::from_value(value)?;
    Ok(Self {
      kernel_sandbox,
      sets: sets
        .into_iter()
        .map(|(k, permissions)| {
          (
            k,
            PermissionsObjectWithBase {
              base: base.clone(),
              permissions,
            },
          )
        })
        .collect(),
    })
  }

  pub fn get(
    &self,
    name: &str,
  ) -> Result<&PermissionsObjectWithBase, UndefinedPermissionError> {
    match self.sets.get(name) {
      Some(value) => Ok(value),
      None => Err(UndefinedPermissionError(name.to_string())),
    }
  }

  pub fn merge(self, member: Self) -> Self {
    let mut sets = self.sets;
    // a member's own mode wins, as its sets do
    let kernel_sandbox = member.kernel_sandbox.or(self.kernel_sandbox);

    for (key, value) in member.sets {
      // When the same key exists in the root and the member, we overwrite
      // with the member instead of merging because we don't want someone looking
      // at a member config file and not realizing the permissions are extended
      // in the root. In the future, we may add an explicit "extends" concept in
      // permissions in order to support this scenario.
      sets.insert(key, value);
    }

    Self {
      sets,
      kernel_sandbox,
    }
  }
}

#[cfg(test)]
mod test {
  use pretty_assertions::assert_eq;
  use serde_json::json;

  use super::*;

  #[test]
  fn kernel_sandbox_key() {
    let base = Url::parse("file:///project/deno.json").unwrap();
    let config = PermissionsConfig::parse(
      json!({ "kernelSandbox": "warn", "default": { "read": true } }),
      &base,
    )
    .unwrap();
    assert_eq!(config.kernel_sandbox.as_deref(), Some("warn"));
    // the reserved key is not a permission set
    assert_eq!(config.sets.keys().collect::<Vec<_>>(), vec!["default"]);
    for value in KERNEL_SANDBOX_CONFIG_VALUES {
      let config =
        PermissionsConfig::parse(json!({ "kernelSandbox": value }), &base)
          .unwrap();
      assert_eq!(config.kernel_sandbox.as_deref(), Some(*value));
    }
    for bad in [json!("on"), json!(true), json!(["warn"])] {
      let err =
        PermissionsConfig::parse(json!({ "kernelSandbox": bad }), &base)
          .unwrap_err();
      assert!(
        err.to_string().contains("permissions.kernelSandbox"),
        "{err}"
      );
    }
    assert_eq!(
      PermissionsConfig::parse(json!({}), &base)
        .unwrap()
        .kernel_sandbox,
      None
    );
  }

  #[test]
  fn kernel_sandbox_member_wins() {
    let base = Url::parse("file:///project/deno.json").unwrap();
    let root =
      PermissionsConfig::parse(json!({ "kernelSandbox": "enforce" }), &base)
        .unwrap();
    let member =
      PermissionsConfig::parse(json!({ "kernelSandbox": "warn" }), &base)
        .unwrap();
    assert_eq!(
      root.clone().merge(member).kernel_sandbox.as_deref(),
      Some("warn")
    );
    assert_eq!(
      root
        .merge(PermissionsConfig::default())
        .kernel_sandbox
        .as_deref(),
      Some("enforce")
    );
  }

  #[test]
  fn deserialize() {
    assert_eq!(
      serde_json::from_value::<PermissionsObject>(json!({
        "all": true,
        "read": true,
        "write": true,
        "import": true,
        "env": true,
        "net": true,
        "run": true,
        "ffi": true,
        "sys": false,
      }))
      .unwrap(),
      PermissionsObject {
        all: Some(true),
        read: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
          ignore: None,
        },
        write: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
        },
        import: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
        },
        env: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
          ignore: None,
        },
        net: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
        },
        run: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
        },
        ffi: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: None,
        },
        sys: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::None),
          deny: None,
        }
      }
    );

    assert_eq!(
      serde_json::from_value::<PermissionsObject>(json!({
        "read": ["test"],
        "write": ["test"],
        "import": ["test"],
        "env": ["test"],
        "net": ["test"],
        "run": ["test"],
        "ffi": ["test"],
        "sys": ["test"],
      }))
      .unwrap(),
      PermissionsObject {
        all: None,
        read: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
          ignore: None,
        },
        write: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        },
        import: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        },
        env: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
          ignore: None,
        },
        net: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        },
        run: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        },
        ffi: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        },
        sys: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: None,
        }
      }
    );

    assert_eq!(
      serde_json::from_value::<PermissionsObject>(json!({
        "read": {
          "allow": ["test"],
          "deny": ["test-deny"],
          "ignore": ["test-ignore"],
        },
        "write": [],
        "sys": {
          "allow": []
        }
      }))
      .unwrap(),
      PermissionsObject {
        all: None,
        read: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::Some(vec!["test".to_string()])),
          deny: Some(PermissionConfigValue::Some(vec![
            "test-deny".to_string()
          ])),
          ignore: Some(PermissionConfigValue::Some(vec![
            "test-ignore".to_string()
          ]))
        },
        write: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::None),
          deny: None
        },
        sys: AllowDenyPermissionConfig {
          allow: Some(PermissionConfigValue::None),
          deny: None
        },
        ..Default::default()
      }
    );

    assert_eq!(
      serde_json::from_value::<PermissionsObject>(json!({
        "read": {
          "allow": true,
          "deny": ["test-deny"],
          "ignore": ["test-ignore"],
        },
      }))
      .unwrap(),
      PermissionsObject {
        all: None,
        read: AllowDenyIgnorePermissionConfig {
          allow: Some(PermissionConfigValue::All),
          deny: Some(PermissionConfigValue::Some(vec![
            "test-deny".to_string()
          ])),
          ignore: Some(PermissionConfigValue::Some(vec![
            "test-ignore".to_string()
          ])),
        },
        ..Default::default()
      }
    );
  }
}
