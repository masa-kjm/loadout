//! Private schema-3-to-4 document route.
//!
//! This module validates and converts documents in memory.
//! Publication belongs to the facade's state and profile responsibilities.

#![allow(dead_code)] // The continuous-route facade is added in the next stage.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};

use crate::domain::ids::{ProfileId, ResourceId};

const LEGACY_SCHEMA_VERSION: u64 = 3;
const TARGET_SCHEMA_VERSION: u64 = 4;

/// Converts a complete schema-3 profile, changing only its root version value.
/// The facade must validate cross-profile references before publication.
pub(super) fn convert_profile(source: &str) -> Result<String, Schema3To4Error> {
    validate_legacy_profile_source(source)?;
    let candidate = replace_root_schema_version(source)?;
    super::schema_4_to_5::validate_schema_four_profile(&candidate)
        .map_err(|error| Schema3To4Error::InvalidConvertedProfile(error.to_string()))?;
    Ok(candidate)
}

/// Converts an inactive schema-3 state while retaining every Known fact.
/// The facade must prove default-instance identity against the full profile inventory before publication.
pub(super) fn convert_state(source: &[u8]) -> Result<Vec<u8>, Schema3To4Error> {
    let legacy: LegacyStateDocument =
        serde_json::from_slice(source).map_err(Schema3To4Error::State)?;
    if legacy.schema_version != LEGACY_SCHEMA_VERSION {
        return Err(Schema3To4Error::UnsupportedSchemaVersion {
            document: "state",
            actual: legacy.schema_version,
        });
    }
    if legacy.active_operation.is_some() {
        return Err(Schema3To4Error::ActiveOperationPresent);
    }
    let candidate = serde_json::to_vec(&serde_json::json!({
        "schema_version": TARGET_SCHEMA_VERSION,
        "resources": legacy.resources,
        "active_operation": null,
    }))
    .map_err(Schema3To4Error::State)?;
    super::schema_4_to_5::validate_schema_four_state(&candidate)
        .map_err(|error| Schema3To4Error::InvalidConvertedState(error.to_string()))?;
    Ok(candidate)
}

struct LegacyProfileFacts {
    id: ProfileId,
    includes: Vec<ProfileId>,
}

fn validate_legacy_profile_source(source: &str) -> Result<LegacyProfileFacts, Schema3To4Error> {
    let document: YamlValue = serde_yaml::from_str(source).map_err(Schema3To4Error::Profile)?;
    validate_legacy_profile(&document)?;
    let mapping = document
        .as_mapping()
        .expect("legacy profile validation requires a mapping");
    let id = ProfileId::parse(require_yaml_string(mapping, "id")?.to_owned())
        .map_err(|_| Schema3To4Error::InvalidLegacyProfile)?;
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .expect("legacy profile validation requires resources");
    for resource_id in resources.keys() {
        ResourceId::parse(
            resource_id
                .as_str()
                .expect("legacy validation requires string resource IDs")
                .to_owned(),
        )
        .map_err(|_| Schema3To4Error::InvalidLegacyProfile)?;
    }
    let no_includes = Vec::new();
    let includes = match mapping.get(YamlValue::String("includes".to_owned())) {
        Some(value) => value
            .as_sequence()
            .ok_or(Schema3To4Error::InvalidLegacyProfile)?,
        None => &no_includes,
    }
    .iter()
    .map(|include| {
        let include = include
            .as_mapping()
            .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
        ProfileId::parse(require_yaml_string(include, "id")?.to_owned())
            .map_err(|_| Schema3To4Error::InvalidLegacyProfile)
    })
    .collect::<Result<Vec<_>, _>>()?;
    Ok(LegacyProfileFacts { id, includes })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStateDocument {
    schema_version: u64,
    resources: BTreeMap<String, LegacyKnownResource>,
    active_operation: Option<JsonValue>,
}

/// The schema-3 state surface that migration can preserve without interpretation.
///
/// Strict DTOs prevent unreviewed nested legacy facts from crossing the schema boundary.
#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyKnownResource {
    definition_hash: String,
    effect: LegacyKnownEffect,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum LegacyKnownEffect {
    FileLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
    FileCopy {
        source_path: String,
        target_path: String,
        content_fingerprint: String,
    },
    DirectoryLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
}

fn validate_legacy_profile(document: &YamlValue) -> Result<(), Schema3To4Error> {
    let mapping = document
        .as_mapping()
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    require_yaml_keys(mapping, &["schema_version", "id", "includes", "resources"])?;
    let version = mapping
        .get(YamlValue::String("schema_version".to_owned()))
        .and_then(YamlValue::as_u64)
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    if version != LEGACY_SCHEMA_VERSION {
        return Err(Schema3To4Error::UnsupportedSchemaVersion {
            document: "profile",
            actual: version,
        });
    }
    require_yaml_string(mapping, "id")?;
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    for (resource_id, resource) in resources {
        if resource_id.as_str().is_none() {
            return Err(Schema3To4Error::InvalidLegacyProfile);
        }
        validate_legacy_resource(resource)?;
    }
    if let Some(includes) = mapping.get(YamlValue::String("includes".to_owned())) {
        for include in includes
            .as_sequence()
            .ok_or(Schema3To4Error::InvalidLegacyProfile)?
        {
            let include = include
                .as_mapping()
                .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
            require_yaml_keys(include, &["id"])?;
            require_yaml_string(include, "id")?;
        }
    }
    Ok(())
}

fn validate_legacy_resource(value: &YamlValue) -> Result<(), Schema3To4Error> {
    let resource = value
        .as_mapping()
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    require_yaml_keys(resource, &["type", "properties"])?;
    if require_yaml_string(resource, "type")? != "file" {
        return Err(Schema3To4Error::InvalidLegacyProfile);
    }
    let properties = resource
        .get(YamlValue::String("properties".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    require_yaml_keys(properties, &["kind", "source", "target", "operation"])?;
    let kind = require_yaml_string(properties, "kind")?;
    let operation = require_yaml_string(properties, "operation")?;
    require_yaml_string(properties, "target")?;
    if !matches!(kind, "file" | "directory")
        || !matches!(operation, "link" | "copy")
        || (kind == "directory" && operation == "copy")
    {
        return Err(Schema3To4Error::InvalidLegacyProfile);
    }
    let source = properties
        .get(YamlValue::String("source".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(Schema3To4Error::InvalidLegacyProfile)?;
    require_yaml_keys(source, &["store", "path"])?;
    require_yaml_string(source, "store")?;
    require_yaml_string(source, "path")?;
    Ok(())
}

fn require_yaml_keys(mapping: &YamlMapping, allowed: &[&str]) -> Result<(), Schema3To4Error> {
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    if mapping
        .keys()
        .any(|key| key.as_str().is_none_or(|key| !allowed.contains(key)))
    {
        return Err(Schema3To4Error::InvalidLegacyProfile);
    }
    Ok(())
}

fn require_yaml_string<'a>(
    mapping: &'a YamlMapping,
    key: &str,
) -> Result<&'a str, Schema3To4Error> {
    mapping
        .get(YamlValue::String(key.to_owned()))
        .and_then(YamlValue::as_str)
        .ok_or(Schema3To4Error::InvalidLegacyProfile)
}

/// Replaces the required root schema value without reserializing the authoring document.
///
/// Only the root schema value may change; comments, mapping order, and quoting remain intact.
fn replace_root_schema_version(source: &str) -> Result<String, Schema3To4Error> {
    let mut result = String::with_capacity(source.len());
    let mut replaced = false;

    for line in source.split_inclusive('\n') {
        if !replaced {
            if let Some(value_start) = root_schema_version_value_start(line)
                .or_else(|| flow_root_schema_version_value_start(line))
            {
                let value = &line[value_start..];
                if let Some(suffix) = value.strip_prefix('3') {
                    if suffix.is_empty()
                        || suffix.chars().next().is_some_and(char::is_whitespace)
                        || suffix.starts_with('#')
                        || suffix.starts_with(',')
                        || suffix.starts_with('}')
                    {
                        result.push_str(&line[..value_start]);
                        result.push('4');
                        result.push_str(suffix);
                        replaced = true;
                        continue;
                    }
                }
            }
        }
        result.push_str(line);
    }

    if !replaced {
        return Err(Schema3To4Error::InvalidLegacyProfile);
    }
    Ok(result)
}

/// Returns the value offset for the supported top-level YAML spellings of `schema_version`.
/// The surrounding text is retained exactly so profile migration changes only the numeric value.
fn root_schema_version_value_start(line: &str) -> Option<usize> {
    let without_newline = line.strip_suffix('\n').unwrap_or(line);
    let leading = without_newline.len() - without_newline.trim_start_matches([' ', '\t']).len();
    let rest = &without_newline[leading..];
    let key_length = if rest.starts_with("schema_version") {
        "schema_version".len()
    } else if rest.starts_with("\"schema_version\"") {
        "\"schema_version\"".len()
    } else if rest.starts_with("'schema_version'") {
        "'schema_version'".len()
    } else {
        return None;
    };
    let after_key = &rest[key_length..];
    let spaces = after_key.len() - after_key.trim_start_matches([' ', '\t']).len();
    let after_spaces = &after_key[spaces..];
    if !after_spaces.starts_with(':') {
        return None;
    }
    let after_colon = &after_spaces[1..];
    let value_spaces = after_colon.len() - after_colon.trim_start_matches([' ', '\t']).len();
    Some(leading + key_length + spaces + 1 + value_spaces)
}

fn flow_root_schema_version_value_start(line: &str) -> Option<usize> {
    let mut offset = 0;
    let bytes = line.as_bytes();
    while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
        offset += 1;
    }
    if bytes.get(offset) != Some(&b'{') {
        return None;
    }
    offset += 1;
    loop {
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            offset += 1;
        }
        if bytes.get(offset) == Some(&b'}') || offset >= bytes.len() {
            return None;
        }
        let key_start = offset;
        let key_end = if matches!(bytes.get(offset), Some(b'\'' | b'"')) {
            let quote = bytes[offset];
            offset += 1;
            let content_start = offset;
            while bytes.get(offset) != Some(&quote) {
                offset += 1;
            }
            let content_end = offset;
            offset += 1;
            if &line[content_start..content_end] == "schema_version" {
                content_end
            } else {
                key_start
            }
        } else {
            while bytes
                .get(offset)
                .is_some_and(|byte| *byte != b':' && !byte.is_ascii_whitespace())
            {
                offset += 1;
            }
            offset
        };
        let key_is_schema = if key_end == key_start {
            false
        } else if matches!(bytes.get(key_start), Some(b'\'' | b'"')) {
            let quote = bytes[key_start];
            line[key_start + 1..]
                .find(quote as char)
                .is_some_and(|length| {
                    &line[key_start + 1..key_start + 1 + length] == "schema_version"
                })
        } else {
            &line[key_start..key_end] == "schema_version"
        };
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if bytes.get(offset) != Some(&b':') {
            return None;
        }
        offset += 1;
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset += 1;
        }
        if key_is_schema {
            return Some(offset);
        }
        let mut nested = 0_u32;
        let mut quote = None;
        while let Some(byte) = bytes.get(offset) {
            if let Some(expected) = quote {
                if *byte == expected {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(*byte),
                    b'{' | b'[' => nested += 1,
                    b'}' | b']' if nested > 0 => nested -= 1,
                    b',' if nested == 0 => break,
                    b'}' if nested == 0 => return None,
                    _ => {}
                }
            }
            offset += 1;
        }
    }
}

#[derive(Debug)]
pub(super) enum Schema3To4Error {
    Profile(serde_yaml::Error),
    State(serde_json::Error),
    UnsupportedSchemaVersion { document: &'static str, actual: u64 },
    InvalidLegacyProfile,
    ActiveOperationPresent,
    InvalidConvertedProfile(String),
    InvalidConvertedState(String),
}

impl fmt::Display for Schema3To4Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => write!(formatter, "invalid schema-3 profile: {error}"),
            Self::State(error) => write!(formatter, "invalid schema-3 state: {error}"),
            Self::UnsupportedSchemaVersion { document, actual } => write!(
                formatter,
                "unsupported {document} schema_version {actual}; expected {LEGACY_SCHEMA_VERSION}"
            ),
            Self::InvalidLegacyProfile => {
                formatter.write_str("profile is not a complete schema-3 declaration")
            }
            Self::ActiveOperationPresent => {
                formatter.write_str("state migration requires no active operation")
            }
            Self::InvalidConvertedProfile(error) => {
                write!(formatter, "schema-4 profile validation failed: {error}")
            }
            Self::InvalidConvertedState(error) => {
                write!(formatter, "schema-4 state validation failed: {error}")
            }
        }
    }
}

impl std::error::Error for Schema3To4Error {}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "schema_version: 3\nid: base\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source: { store: dotfiles, path: git/config }\n      target: /home/example/.gitconfig\n      operation: link\n";
    #[cfg(unix)]
    const STATE: &str = r#"{"schema_version":3,"resources":{"base/git-config":{"definition_hash":"sha256:1250e6bf4abdb529664c1444709aeae2ce814e8b97c3984e32d46d9c971b3a5e","effect":{"kind":"file_link","source_path":"/home/example/dotfiles/git/config","target_path":"/home/example/.gitconfig","link_target":"/home/example/dotfiles/git/config"}}},"active_operation":null}"#;
    #[cfg(windows)]
    const STATE: &str = r#"{"schema_version":3,"resources":{"base/git-config":{"definition_hash":"sha256:553a59aab51ec70ffcec1e6c4dd3ebb4059bb3a257cd2cc3ccd5ef5908849348","effect":{"kind":"file_link","source_path":"C:\\loadout\\dotfiles\\git\\config","target_path":"C:\\loadout\\.gitconfig","link_target":"C:\\loadout\\dotfiles\\git\\config"}}},"active_operation":null}"#;

    #[test]
    fn profile_conversion_changes_only_the_root_version() {
        let source =
            "# kept\nid: base\nresources: {}\nschema_version: 3 # only this value changes\n";
        assert_eq!(
            convert_profile(source).unwrap(),
            source.replacen("schema_version: 3", "schema_version: 4", 1)
        );
        assert_eq!(
            convert_profile(PROFILE).unwrap(),
            PROFILE.replacen("schema_version: 3", "schema_version: 4", 1)
        );
        assert_eq!(
            convert_profile("{ schema_version: 3, id: base, resources: {} }\n").unwrap(),
            "{ schema_version: 4, id: base, resources: {} }\n"
        );
    }

    #[test]
    fn profile_conversion_rejects_schema_four_features_and_invalid_sources() {
        for source in [
            PROFILE.replace("schema_version: 3", "schema_version: 4"),
            PROFILE.replace("id: base", "parameters: {}\nid: base"),
            PROFILE.replace("kind: file", "kind: device"),
            PROFILE.replace("operation: link", "operation: synchronize"),
            PROFILE
                .replace("kind: file", "kind: directory")
                .replace("operation: link", "operation: copy"),
            PROFILE.replace("id: base", "id: INVALID"),
        ] {
            assert!(convert_profile(&source).is_err(), "{source}");
        }
    }

    #[test]
    fn state_conversion_preserves_known_facts_and_rejects_invalid_input() {
        let converted = convert_state(STATE.as_bytes()).unwrap();
        let before: JsonValue = serde_json::from_str(STATE).unwrap();
        let after: JsonValue = serde_json::from_slice(&converted).unwrap();
        assert_eq!(after["schema_version"], TARGET_SCHEMA_VERSION);
        assert_eq!(after["resources"], before["resources"]);
        assert_eq!(after["active_operation"], JsonValue::Null);

        for source in [
            STATE.replace("\"schema_version\":3", "\"schema_version\":4"),
            STATE.replace("\"active_operation\":null", "\"active_operation\":{}"),
            STATE.replace("\"resources\":", "\"unknown\":true,\"resources\":"),
            STATE.replace("\"effect\":{", "\"unknown\":true,\"effect\":{"),
            STATE.replace(
                "\"kind\":\"file_link\",",
                "\"kind\":\"file_link\",\"unknown\":true,",
            ),
            STATE.replace(
                "\"definition_hash\":\"sha256:",
                "\"definition_hash\":\"bad:",
            ),
        ] {
            assert!(convert_state(source.as_bytes()).is_err(), "{source}");
        }
    }

    #[test]
    fn route_outputs_can_continue_to_schema_five_without_changing_known_facts() {
        let profile_four = convert_profile(PROFILE).unwrap();
        let profile_five = super::super::schema_4_to_5::convert_profile(&profile_four).unwrap();
        assert_eq!(
            profile_five,
            PROFILE.replacen("schema_version: 3", "schema_version: 5", 1)
        );

        let state_four = convert_state(STATE.as_bytes()).unwrap();
        let state_five = super::super::schema_4_to_5::convert_state(&state_four).unwrap();
        let original: JsonValue = serde_json::from_str(STATE).unwrap();
        let final_state: JsonValue = serde_json::from_slice(&state_five).unwrap();
        assert_eq!(final_state["schema_version"], 5);
        assert_eq!(final_state["resources"], original["resources"]);
        assert_eq!(
            final_state["applied_lifecycle_topology"]["current"],
            serde_json::json!([])
        );
        assert_eq!(
            final_state["applied_lifecycle_topology"]["historical"],
            serde_json::json!([])
        );
    }
}
