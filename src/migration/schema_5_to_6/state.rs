//! Private schema-5 state decoding and the adjacent 5-to-6 conversion.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::validate_current_state_bytes;

const LEGACY_SCHEMA_VERSION: u32 = 5;
const TARGET_SCHEMA_VERSION: u32 = 6;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyState {
    schema_version: u32,
    resources: BTreeMap<String, Value>,
    applied_lifecycle_topology: LegacyTopology,
    active_operation: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyTopology {
    current: Vec<Value>,
    historical: Vec<Value>,
}

/// Validates an inactive schema-5 state, including its existing topology.
pub(crate) fn validate_schema_five_state(source: &[u8]) -> Result<(), StateMigrationError> {
    let legacy = parse(source)?;
    validate_candidate(&legacy)
}

/// Converts only an inactive schema-5 state with no durable ordering facts.
pub(crate) fn convert_state(source: &[u8]) -> Result<Vec<u8>, StateMigrationError> {
    let mut legacy = parse(source)?;
    if !legacy.applied_lifecycle_topology.current.is_empty() {
        return Err(StateMigrationError::NonemptyTopology("current"));
    }
    if !legacy.applied_lifecycle_topology.historical.is_empty() {
        return Err(StateMigrationError::NonemptyTopology("historical"));
    }
    legacy.schema_version = TARGET_SCHEMA_VERSION;
    let candidate = serde_json::to_vec(&legacy).map_err(StateMigrationError::Json)?;
    validate_current_state_bytes(&candidate)
        .map_err(|error| StateMigrationError::InvalidState(error.to_string()))?;
    Ok(candidate)
}

fn parse(source: &[u8]) -> Result<LegacyState, StateMigrationError> {
    let legacy: LegacyState = serde_json::from_slice(source).map_err(StateMigrationError::Json)?;
    if legacy.schema_version != LEGACY_SCHEMA_VERSION {
        return Err(StateMigrationError::UnsupportedVersion(
            legacy.schema_version,
        ));
    }
    if legacy.active_operation.is_some() {
        return Err(StateMigrationError::ActiveOperationPresent);
    }
    Ok(legacy)
}

fn validate_candidate(legacy: &LegacyState) -> Result<(), StateMigrationError> {
    let mut candidate: Value = serde_json::to_value(legacy).map_err(StateMigrationError::Json)?;
    candidate["schema_version"] = Value::from(TARGET_SCHEMA_VERSION);
    let candidate = serde_json::to_vec(&candidate).map_err(StateMigrationError::Json)?;
    validate_current_state_bytes(&candidate)
        .map_err(|error| StateMigrationError::InvalidState(error.to_string()))
}

#[derive(Debug)]
pub(crate) enum StateMigrationError {
    Json(serde_json::Error),
    UnsupportedVersion(u32),
    ActiveOperationPresent,
    NonemptyTopology(&'static str),
    InvalidState(String),
}

impl fmt::Display for StateMigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid schema-5 state: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported state schema_version {version}; expected 5"
                )
            }
            Self::ActiveOperationPresent => {
                formatter.write_str("state migration requires no active operation")
            }
            Self::NonemptyTopology(array) => write!(
                formatter,
                "state migration from schema 5 to 6 requires empty applied_lifecycle_topology.{array}; use a schema-5 Loadout binary to complete recorded-order stale removal until both applied_lifecycle_topology.current and applied_lifecycle_topology.historical are empty, then retry migration"
            ),
            Self::InvalidState(error) => write!(
                formatter,
                "schema-5 state is not losslessly convertible: {error}"
            ),
        }
    }
}

impl std::error::Error for StateMigrationError {}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = r#"{"schema_version":5,"resources":{},"applied_lifecycle_topology":{"current":[],"historical":[]},"active_operation":null}"#;
    #[cfg(unix)]
    const WITH_KNOWN: &str = r#"{"schema_version":5,"resources":{"base/git-config":{"definition_hash":"sha256:1250e6bf4abdb529664c1444709aeae2ce814e8b97c3984e32d46d9c971b3a5e","effect":{"kind":"file_link","source_path":"/home/example/dotfiles/git/config","target_path":"/home/example/.gitconfig","link_target":"/home/example/dotfiles/git/config"}}},"applied_lifecycle_topology":{"current":[],"historical":[]},"active_operation":null}"#;
    #[cfg(windows)]
    const WITH_KNOWN: &str = r#"{"schema_version":5,"resources":{"base/git-config":{"definition_hash":"sha256:553a59aab51ec70ffcec1e6c4dd3ebb4059bb3a257cd2cc3ccd5ef5908849348","effect":{"kind":"file_link","source_path":"C:\\loadout\\dotfiles\\git\\config","target_path":"C:\\loadout\\.gitconfig","link_target":"C:\\loadout\\dotfiles\\git\\config"}}},"applied_lifecycle_topology":{"current":[],"historical":[]},"active_operation":null}"#;

    #[test]
    fn empty_topology_converts_without_inventing_facts() {
        let converted = convert_state(WITH_KNOWN.as_bytes()).unwrap();
        let state: Value = serde_json::from_slice(&converted).unwrap();
        let original: Value = serde_json::from_str(WITH_KNOWN).unwrap();
        assert_eq!(state["schema_version"], 6);
        assert_eq!(state["resources"], original["resources"]);
        assert_eq!(
            state["applied_lifecycle_topology"]["current"],
            serde_json::json!([])
        );
        assert_eq!(
            state["applied_lifecycle_topology"]["historical"],
            serde_json::json!([])
        );
    }

    #[test]
    fn both_topology_arrays_and_active_operation_block_conversion() {
        for (source, expected) in [
            (
                EMPTY.replace("\"current\":[]", "\"current\":[{}]"),
                "applied_lifecycle_topology.current",
            ),
            (
                EMPTY.replace("\"historical\":[]", "\"historical\":[{}]"),
                "applied_lifecycle_topology.historical",
            ),
            (
                EMPTY.replace("\"active_operation\":null", "\"active_operation\":{}"),
                "active operation",
            ),
        ] {
            assert!(
                convert_state(source.as_bytes())
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
        for array in ["current", "historical"] {
            let source = EMPTY.replace(&format!("\"{array}\":[]"), &format!("\"{array}\":[{{}}]"));
            let error = convert_state(source.as_bytes()).unwrap_err().to_string();
            assert!(error.contains("schema-5 Loadout binary"), "{error}");
            assert!(error.contains("both applied_lifecycle_topology.current and applied_lifecycle_topology.historical are empty"), "{error}");
            assert!(error.contains("then retry migration"), "{error}");
        }
    }

    #[test]
    fn strict_decoding_rejects_unknown_or_invalid_known_facts() {
        for source in [
            EMPTY.replace("\"resources\":{}", "\"resources\":{},\"extra\":true"),
            EMPTY.replace("\"current\":[]", "\"current\":[],\"extra\":true"),
            EMPTY.replace("\"resources\":{}", "\"resources\":{\"bad\":{}}"),
        ] {
            assert!(convert_state(source.as_bytes()).is_err());
        }
    }

    #[test]
    fn composed_older_state_routes_apply_the_same_topology_gate() {
        let mut four: Value = serde_json::from_str(WITH_KNOWN).unwrap();
        four["schema_version"] = Value::from(4);
        four.as_object_mut()
            .unwrap()
            .remove("applied_lifecycle_topology");
        let four = serde_json::to_string(&four).unwrap();
        let five = super::super::super::schema_4_to_5::convert_state(four.as_bytes()).unwrap();
        let six = convert_state(&five).unwrap();
        let result: Value = serde_json::from_slice(&six).unwrap();
        let original: Value = serde_json::from_str(WITH_KNOWN).unwrap();
        assert_eq!(result["resources"], original["resources"]);

        let mut three: Value = serde_json::from_str(&four).unwrap();
        three["schema_version"] = Value::from(3);
        let three = serde_json::to_string(&three).unwrap();
        let four = super::super::super::schema_3_to_4::convert_state(three.as_bytes()).unwrap();
        let five = super::super::super::schema_4_to_5::convert_state(&four).unwrap();
        assert!(convert_state(&five).is_ok());
    }
}
