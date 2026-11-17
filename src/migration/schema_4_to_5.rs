use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};

use crate::declaration::profile::{
    FileKind, FileOperation, ProfileDeclaration, ValueOrReference, parse_kind, parse_operation,
};
use crate::domain::ids::{ProfileId, ResourceId};
use crate::state::repository::StateDecodeError;
use crate::state::validate_current_state_bytes;

const LEGACY_SCHEMA_VERSION: u64 = 4;
const TARGET_SCHEMA_VERSION: u64 = 5;

pub(super) fn validate_schema_four_profile(source: &str) -> Result<(), MigrationError> {
    profile_facts_continuous(source).map(|_| ())
}
/// Validates schema-4 declarations for the continuous route without widening the old CLI.
pub(super) fn profile_facts_continuous(
    source: &str,
) -> Result<(ProfileId, Vec<ProfileId>), MigrationError> {
    let document: YamlValue = serde_yaml::from_str(source).map_err(MigrationError::Profile)?;
    let mapping = document
        .as_mapping()
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    require_yaml_keys(
        mapping,
        &[
            "schema_version",
            "id",
            "parameters",
            "includes",
            "resources",
        ],
    )?;
    let version = mapping
        .get(YamlValue::String("schema_version".to_owned()))
        .and_then(YamlValue::as_u64)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    if version != LEGACY_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedSchemaVersion {
            document: "profile",
            actual: version,
        });
    }
    if let Some(includes) = mapping.get(YamlValue::String("includes".to_owned())) {
        for include in includes
            .as_sequence()
            .ok_or(MigrationError::InvalidLegacyProfile)?
        {
            require_yaml_keys(
                include
                    .as_mapping()
                    .ok_or(MigrationError::InvalidLegacyProfile)?,
                &["id", "as", "with"],
            )?;
        }
    }
    let resources = mapping
        .get(YamlValue::String("resources".to_owned()))
        .and_then(YamlValue::as_mapping)
        .ok_or(MigrationError::InvalidLegacyProfile)?;
    for resource in resources.values() {
        require_yaml_keys(
            resource
                .as_mapping()
                .ok_or(MigrationError::InvalidLegacyProfile)?,
            &["type", "properties"],
        )?;
    }
    let candidate = replace_root_schema_version(source, '4', '5')?;
    let parsed = ProfileDeclaration::parse(&candidate).map_err(MigrationError::Profile)?;
    let id = ProfileId::parse(parsed.id().to_owned())
        .map_err(|_| MigrationError::InvalidLegacyProfile)?;
    for parameter in parsed.parameters().keys() {
        ProfileId::parse(parameter.to_owned()).map_err(|_| MigrationError::InvalidLegacyProfile)?;
    }
    for (resource_id, resource) in parsed.resources() {
        ResourceId::parse(resource_id.to_owned())
            .map_err(|_| MigrationError::InvalidLegacyProfile)?;
        let properties = resource.properties();
        let kind = match properties.kind() {
            ValueOrReference::Value(value) => value.as_string().and_then(parse_kind),
            ValueOrReference::Reference(_) => None,
        };
        let operation = match properties.operation() {
            ValueOrReference::Value(value) => value.as_string().and_then(parse_operation),
            ValueOrReference::Reference(_) => None,
        };
        if matches!(properties.kind(), ValueOrReference::Value(_)) && kind.is_none()
            || matches!(properties.operation(), ValueOrReference::Value(_)) && operation.is_none()
            || kind == Some(FileKind::Directory) && operation == Some(FileOperation::Copy)
        {
            return Err(MigrationError::InvalidLegacyProfile);
        }
    }
    let includes = parsed
        .includes()
        .iter()
        .map(|include| {
            ProfileId::parse(include.id().to_owned())
                .map_err(|_| MigrationError::InvalidLegacyProfile)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((id, includes))
}

/// Converts a schema-4 document for a fully validated continuous inventory.
pub(super) fn convert_profile_continuous(source: &str) -> Result<String, MigrationError> {
    profile_facts_continuous(source)?;
    replace_root_schema_version(source, '4', '5')
}

/// Converts a complete inactive schema-4 state document after preserving its facts byte-for-byte.
pub(super) fn convert_state(source: &[u8]) -> Result<Vec<u8>, MigrationError> {
    let legacy: LegacyStateDocument =
        serde_json::from_slice(source).map_err(MigrationError::State)?;
    if legacy.schema_version != LEGACY_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedSchemaVersion {
            document: "state",
            actual: legacy.schema_version,
        });
    }
    if legacy.active_operation.is_some() {
        return Err(MigrationError::ActiveOperationPresent);
    }

    let candidate = serde_json::to_vec(&serde_json::json!({
        "schema_version": TARGET_SCHEMA_VERSION,
        "resources": legacy.resources,
        "applied_lifecycle_topology": {
            "current": [],
            "historical": [],
        },
        "active_operation": null,
    }))
    .map_err(MigrationError::State)?;
    validate_current_state_bytes(&candidate).map_err(MigrationError::InvalidConvertedState)?;
    Ok(candidate)
}

/// Validates a schema-4 state through its strict legacy DTO and current-schema candidate.
pub(super) fn validate_schema_four_state(source: &[u8]) -> Result<(), MigrationError> {
    convert_state(source).map(|_| ())
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
/// These DTOs are deliberately strict even though conversion retains their values verbatim:
/// accepting an unknown nested field would make a later schema-4 publication silently carry
/// an unreviewed legacy fact across the schema boundary.
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

fn require_yaml_keys(mapping: &YamlMapping, allowed: &[&str]) -> Result<(), MigrationError> {
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    if mapping
        .keys()
        .any(|key| key.as_str().is_none_or(|key| !allowed.contains(key)))
    {
        return Err(MigrationError::InvalidLegacyProfile);
    }
    Ok(())
}

pub(super) fn replace_root_schema_version(
    source: &str,
    from: char,
    to: char,
) -> Result<String, MigrationError> {
    let mut result = String::with_capacity(source.len());
    let mut replaced = false;

    for line in source.split_inclusive('\n') {
        if !replaced {
            if let Some(value_start) = root_schema_version_value_start(line)
                .or_else(|| flow_root_schema_version_value_start(line))
            {
                let value = &line[value_start..];
                if let Some(suffix) = value.strip_prefix(from) {
                    if suffix.is_empty()
                        || suffix.chars().next().is_some_and(char::is_whitespace)
                        || suffix.starts_with('#')
                        || suffix.starts_with(',')
                        || suffix.starts_with('}')
                    {
                        result.push_str(&line[..value_start]);
                        result.push(to);
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
        return Err(MigrationError::InvalidLegacyProfile);
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

/// A migration-only failure that leaves original documents authoritative.
#[derive(Debug)]
pub(crate) enum MigrationError {
    Profile(serde_yaml::Error),
    State(serde_json::Error),
    UnsupportedSchemaVersion { document: &'static str, actual: u64 },
    InvalidLegacyProfile,
    ActiveOperationPresent,
    InvalidConvertedState(StateDecodeError),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => {
                write!(formatter, "invalid profile migration document: {error}")
            }
            Self::State(error) => write!(formatter, "invalid state migration document: {error}"),
            Self::UnsupportedSchemaVersion { document, actual } => write!(
                formatter,
                "unsupported {document} schema_version {actual}; expected {LEGACY_SCHEMA_VERSION}"
            ),
            Self::InvalidLegacyProfile => {
                formatter.write_str("profile is not a complete schema-4 declaration")
            }
            Self::ActiveOperationPresent => {
                formatter.write_str("state migration requires no active operation")
            }
            Self::InvalidConvertedState(error) => write!(
                formatter,
                "schema-4 state is not losslessly convertible: {error}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

#[cfg(test)]
mod tests {
    use super::*;
    const PROFILE: &str = "schema_version: 4\nid: base\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source: { store: dotfiles, path: git/config }\n      target: /home/example/.gitconfig\n      operation: link\n";
    #[cfg(unix)]
    const STATE: &str = r#"{"schema_version":4,"resources":{"base/git-config":{"definition_hash":"sha256:1250e6bf4abdb529664c1444709aeae2ce814e8b97c3984e32d46d9c971b3a5e","effect":{"kind":"file_link","source_path":"/home/example/dotfiles/git/config","target_path":"/home/example/.gitconfig","link_target":"/home/example/dotfiles/git/config"}}},"active_operation":null}"#;
    #[cfg(windows)]
    const STATE: &str = r#"{"schema_version":4,"resources":{"base/git-config":{"definition_hash":"sha256:553a59aab51ec70ffcec1e6c4dd3ebb4059bb3a257cd2cc3ccd5ef5908849348","effect":{"kind":"file_link","source_path":"C:\\loadout\\dotfiles\\git\\config","target_path":"C:\\loadout\\.gitconfig","link_target":"C:\\loadout\\dotfiles\\git\\config"}}},"active_operation":null}"#;

    #[test]
    fn profile_conversion_only_admits_complete_schema_four_documents() {
        let converted = convert_profile_continuous(PROFILE).unwrap();
        assert!(converted.contains("schema_version: 5"));
        assert_eq!(
            converted,
            PROFILE.replacen("schema_version: 4", "schema_version: 5", 1)
        );
        assert!(ProfileDeclaration::parse(&converted).is_ok());
        assert!(
            convert_profile_continuous(&PROFILE.replace("schema_version: 4", "schema_version: 2"))
                .is_err()
        );
        assert!(
            convert_profile_continuous(&PROFILE.replace("id: base", "stages: {}\nid: base"))
                .is_err()
        );
    }

    #[test]
    fn profile_conversion_rejects_unsupported_legacy_resource_effects() {
        for (label, profile) in [
            (
                "unsupported-kind",
                PROFILE.replace("kind: file", "kind: device"),
            ),
            (
                "unsupported-operation",
                PROFILE.replace("operation: link", "operation: synchronize"),
            ),
            (
                "directory-copy",
                PROFILE
                    .replace("kind: file", "kind: directory")
                    .replace("operation: link", "operation: copy"),
            ),
        ] {
            assert!(
                convert_profile_continuous(&profile).is_err(),
                "{label} must reject"
            );
        }
    }

    #[test]
    fn profile_conversion_preserves_comments_and_mapping_order() {
        let source =
            "# kept\nid: base\nresources: {}\nschema_version: 4 # only this value changes\n";

        assert_eq!(
            convert_profile_continuous(source).unwrap(),
            "# kept\nid: base\nresources: {}\nschema_version: 5 # only this value changes\n"
        );
    }

    #[test]
    fn profile_conversion_preserves_quoted_key_and_pre_colon_whitespace() {
        for source in [
            PROFILE.replacen("schema_version: 4", "\"schema_version\": 4", 1),
            PROFILE.replacen("schema_version: 4", "schema_version \t: 4", 1),
            PROFILE.replacen("schema_version: 4", "'schema_version' : 4 # kept", 1),
        ] {
            let converted = convert_profile_continuous(&source).unwrap();
            assert_eq!(converted.replace('5', "4"), source);
            assert!(ProfileDeclaration::parse(&converted).is_ok());
        }
    }

    #[test]
    fn profile_conversion_supports_a_valid_flow_style_root_mapping() {
        let source = "{ schema_version: 4, id: base, resources: {} }\n";
        assert_eq!(
            convert_profile_continuous(source).unwrap(),
            "{ schema_version: 5, id: base, resources: {} }\n"
        );
    }

    #[test]
    fn state_conversion_preserves_resource_facts_and_rejects_active_operations() {
        let source = STATE.to_owned();
        let converted = convert_state(source.as_bytes()).unwrap();
        let value: JsonValue = serde_json::from_slice(&converted).unwrap();
        let original: JsonValue = serde_json::from_str(&source).unwrap();
        assert_eq!(value["schema_version"], TARGET_SCHEMA_VERSION);
        assert_eq!(value["resources"], original["resources"]);
        assert_eq!(
            value["resources"]["base/git-config"]["definition_hash"],
            original["resources"]["base/git-config"]["definition_hash"]
        );
        assert!(matches!(
            convert_state(source.replace("null", "{}").as_bytes()),
            Err(MigrationError::ActiveOperationPresent)
        ));
    }

    #[test]
    fn state_conversion_rejects_unknown_legacy_fields_at_every_retained_level() {
        for (label, source) in [
            (
                "root",
                STATE.replace("\"resources\":", "\"unexpected\":true,\"resources\":"),
            ),
            (
                "resource",
                STATE.replace("\"effect\":{", "\"unexpected\":true,\"effect\":{"),
            ),
            (
                "effect",
                STATE.replace(
                    "\"kind\":\"file_link\",",
                    "\"kind\":\"file_link\",\"unexpected\":true,",
                ),
            ),
        ] {
            assert!(
                convert_state(source.as_bytes()).is_err(),
                "{label} must reject"
            );
        }
    }
}
