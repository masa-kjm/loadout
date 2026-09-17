//! Strict raw DTO for the machine-local runtime configuration.

use serde::{Deserialize, Serialize};

use crate::declaration::schema::{SchemaVersionV1, deserialize_optional_string};

/// Machine-local `loadout.yaml` before path binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeConfig {
    #[allow(dead_code)]
    schema_version: SchemaVersionV1,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    config_path: Option<String>,
}

impl RuntimeConfig {
    /// Parses only the version-1 runtime configuration schema.
    ///
    /// Relative and home-relative paths intentionally remain raw declaration syntax until the resolver binds them against the runtime config file.
    pub(crate) fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// The optional raw portable environment configuration path.
    pub(crate) fn config_path(&self) -> Option<&str> {
        self.config_path.as_deref()
    }

    /// Renders a complete version-1 runtime configuration selecting an absolute portable path.
    pub(crate) fn selected_config(path: &str) -> Result<String, serde_yaml::Error> {
        #[derive(Serialize)]
        struct SelectedConfig<'a> {
            schema_version: u8,
            config_path: &'a str,
        }

        serde_yaml::to_string(&SelectedConfig {
            schema_version: 1,
            config_path: path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_accepts_version_one_and_preserves_raw_path_syntax() {
        let config = RuntimeConfig::parse(
            "schema_version: 1\nconfig_path: ~/dotfiles/loadout/config.yaml\n",
        )
        .unwrap();

        assert_eq!(config.config_path(), Some("~/dotfiles/loadout/config.yaml"));
        assert_eq!(
            RuntimeConfig::parse("schema_version: 1\n")
                .unwrap()
                .config_path(),
            None
        );
    }

    #[test]
    fn runtime_config_rejects_missing_or_unsupported_version_and_unknown_fields() {
        for (fixture, yaml) in [
            ("missing schema version", "config_path: config.yaml\n"),
            ("unsupported schema version", "schema_version: 2\n"),
            (
                "unknown top-level field",
                "schema_version: 1\nprofiles: {}\n",
            ),
            (
                "explicit null path",
                "schema_version: 1\nconfig_path: null\n",
            ),
        ] {
            assert!(RuntimeConfig::parse(yaml).is_err(), "{fixture} must reject");
        }
    }

    #[test]
    fn selected_config_is_a_complete_parseable_version_one_document() {
        let yaml = RuntimeConfig::selected_config("/absolute/config.yaml").unwrap();
        assert_eq!(
            RuntimeConfig::parse(&yaml).unwrap().config_path(),
            Some("/absolute/config.yaml")
        );
    }
}
