//! Strict raw DTO for the portable environment configuration.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

use crate::declaration::schema::{SchemaVersionV1, deserialize_optional_string};

/// Portable `config.yaml` before identifier validation and path binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnvironmentConfig {
    #[allow(dead_code)]
    schema_version: SchemaVersionV1,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    default_profile: Option<String>,
    profile_discovery: ProfileDiscovery,
    stores: BTreeMap<String, LocalStore>,
}

impl EnvironmentConfig {
    /// Parses and structurally validates only the version-1 environment schema.
    ///
    /// ID grammar, path binding, store-directory verification, profile discovery, and default-profile existence are resolver responsibilities.
    pub(crate) fn parse(yaml: &str) -> Result<Self, EnvironmentConfigError> {
        let config = serde_yaml::from_str::<Self>(yaml).map_err(EnvironmentConfigError::Yaml)?;
        if config.profile_discovery.paths.is_empty() {
            return Err(EnvironmentConfigError::EmptyProfileDiscoveryPaths);
        }

        Ok(config)
    }

    /// The optional raw default profile ID.
    pub(crate) fn default_profile(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    /// Ordered raw profile discovery paths.
    pub(crate) fn profile_discovery_paths(&self) -> &[String] {
        &self.profile_discovery.paths
    }

    /// Finds a raw local-store declaration by its unvalidated declaration key.
    pub(crate) fn store(&self, store_id: &str) -> Option<&LocalStore> {
        self.stores.get(store_id)
    }

    /// Iterates over raw local stores in deterministic declaration-key order.
    pub(crate) fn stores(&self) -> impl ExactSizeIterator<Item = (&str, &LocalStore)> {
        self.stores
            .iter()
            .map(|(store_id, store)| (store_id.as_str(), store))
    }
}

/// Ordered profile discovery directories before path binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ProfileDiscovery {
    paths: Vec<String>,
}

/// The only v0.2 store declaration shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalStore {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    store_type: LocalStoreType,
    path: String,
}

impl LocalStore {
    /// The raw local-store root path before path binding and filesystem validation.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

/// The only supported environment store type in v0.2.0.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum LocalStoreType {
    #[serde(rename = "local")]
    Local,
}

/// The reason an environment configuration cannot cross the declaration boundary.
#[derive(Debug)]
pub(crate) enum EnvironmentConfigError {
    Yaml(serde_yaml::Error),
    EmptyProfileDiscoveryPaths,
}

impl fmt::Display for EnvironmentConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(error) => error.fmt(formatter),
            Self::EmptyProfileDiscoveryPaths => {
                formatter.write_str("profile_discovery.paths must contain at least one path")
            }
        }
    }
}

impl std::error::Error for EnvironmentConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Yaml(error) => Some(error),
            Self::EmptyProfileDiscoveryPaths => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_config_accepts_version_one_and_preserves_raw_declarations() {
        let config = EnvironmentConfig::parse(
            "schema_version: 1\ndefault_profile: workstation\nprofile_discovery:\n  paths:\n    - ./profiles\n    - ~/shared-profiles\nstores:\n  dotfiles:\n    type: local\n    path: .\n",
        )
        .unwrap();

        assert_eq!(config.default_profile(), Some("workstation"));
        assert_eq!(
            config.profile_discovery_paths(),
            ["./profiles", "~/shared-profiles"]
        );
        assert_eq!(config.store("dotfiles").unwrap().path(), ".");
        assert_eq!(
            config
                .stores()
                .map(|(store_id, _)| store_id)
                .collect::<Vec<_>>(),
            ["dotfiles"]
        );
        assert_eq!(
            EnvironmentConfig::parse(
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores: {}\n",
            )
            .unwrap()
            .default_profile(),
            None
        );
    }

    #[test]
    fn environment_config_rejects_schema_errors_at_every_object_level() {
        for (fixture, yaml) in [
            (
                "missing required schema version",
                "profile_discovery:\n  paths: [./profiles]\nstores: {}\n",
            ),
            (
                "unsupported schema version",
                "schema_version: 2\nprofile_discovery:\n  paths: [./profiles]\nstores: {}\n",
            ),
            (
                "missing profile discovery",
                "schema_version: 1\nstores: {}\n",
            ),
            (
                "missing stores",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\n",
            ),
            (
                "unknown top-level field",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores: {}\nresources: {}\n",
            ),
            (
                "missing profile discovery paths",
                "schema_version: 1\nprofile_discovery: {}\nstores: {}\n",
            ),
            (
                "unknown profile discovery field",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\n  recursive: true\nstores: {}\n",
            ),
            (
                "unknown store field",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores:\n  dotfiles:\n    type: local\n    path: .\n    writable: false\n",
            ),
            (
                "missing store type",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores:\n  dotfiles:\n    path: .\n",
            ),
            (
                "missing store path",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores:\n  dotfiles:\n    type: local\n",
            ),
            (
                "unsupported store type",
                "schema_version: 1\nprofile_discovery:\n  paths: [./profiles]\nstores:\n  dotfiles:\n    type: remote\n    path: .\n",
            ),
            (
                "explicit null default profile",
                "schema_version: 1\ndefault_profile: null\nprofile_discovery:\n  paths: [./profiles]\nstores: {}\n",
            ),
        ] {
            assert!(
                EnvironmentConfig::parse(yaml).is_err(),
                "{fixture} must reject"
            );
        }
    }

    #[test]
    fn environment_config_rejects_an_empty_profile_discovery_list() {
        let error = EnvironmentConfig::parse(
            "schema_version: 1\nprofile_discovery:\n  paths: []\nstores: {}\n",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            EnvironmentConfigError::EmptyProfileDiscoveryPaths
        ));
    }
}
