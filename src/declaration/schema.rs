//! Shared strict schema-version decoding for portable declarations.

use serde::Deserialize;
use serde::de::Error as _;

const DECLARATION_SCHEMA_VERSION_V1: u32 = 1;
const V0_3_ENVIRONMENT_SCHEMA_VERSION: u32 = 2;

/// The version retained by runtime and profile declarations in v0.3.0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchemaVersionV1;

impl<'de> Deserialize<'de> for SchemaVersionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == DECLARATION_SCHEMA_VERSION_V1 {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "unsupported schema_version {version}; expected {DECLARATION_SCHEMA_VERSION_V1}"
            )))
        }
    }
}

/// The supported portable environment configuration schema version for v0.3.0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EnvironmentSchemaVersionV2;

impl<'de> Deserialize<'de> for EnvironmentSchemaVersionV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == V0_3_ENVIRONMENT_SCHEMA_VERSION {
            Ok(Self)
        } else {
            Err(D::Error::custom(format!(
                "unsupported schema_version {version}; expected {V0_3_ENVIRONMENT_SCHEMA_VERSION}"
            )))
        }
    }
}

/// Decodes an optional declaration string while rejecting explicit YAML null.
///
/// An optional field may be omitted, but when present the applicable schema requires a string value rather than a null placeholder.
pub(crate) fn deserialize_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match serde_yaml::Value::deserialize(deserializer)? {
        serde_yaml::Value::String(value) => Ok(Some(value)),
        serde_yaml::Value::Null => Err(D::Error::custom(
            "optional field must be a string when present",
        )),
        _ => Err(D::Error::custom(
            "optional field must be a string when present",
        )),
    }
}
