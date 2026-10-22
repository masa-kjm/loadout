//! Strict raw DTO for one discovered schema-4 profile declaration.

use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_yaml::Value;

use crate::declaration::schema::ProfileSchemaVersionV4;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileDeclaration {
    #[allow(dead_code)]
    schema_version: ProfileSchemaVersionV4,
    id: String,
    #[serde(default)]
    parameters: BTreeMap<String, ParameterType>,
    #[serde(default)]
    includes: Vec<IncludeDeclaration>,
    resources: BTreeMap<String, FileResourceDeclaration>,
}

impl ProfileDeclaration {
    pub(crate) fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn parameters(&self) -> &BTreeMap<String, ParameterType> {
        &self.parameters
    }
    pub(crate) fn includes(&self) -> &[IncludeDeclaration] {
        &self.includes
    }
    pub(crate) fn resources(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &FileResourceDeclaration)> {
        self.resources
            .iter()
            .map(|(id, resource)| (id.as_str(), resource))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum ParameterType {
    #[serde(rename = "string")]
    String,
    #[serde(rename = "integer")]
    Integer,
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "list")]
    List,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParameterValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    List(Vec<ParameterScalar>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ParameterScalar {
    String(String),
    Integer(i64),
    Boolean(bool),
}

impl ParameterValue {
    pub(crate) fn parse(value: Value) -> Result<Self, String> {
        match value {
            Value::String(value) => Ok(Self::String(value)),
            Value::Number(value) => value
                .as_i64()
                .map(Self::Integer)
                .ok_or_else(|| "must be an integer".to_owned()),
            Value::Bool(value) => Ok(Self::Boolean(value)),
            Value::Sequence(values) => values
                .into_iter()
                .map(ParameterScalar::parse)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::List),
            Value::Tagged(_) => Err("must not use a YAML tag".to_owned()),
            Value::Null => Err("must not be null".to_owned()),
            Value::Mapping(_) => Err("must be a scalar, list, or whole-value reference".to_owned()),
        }
    }
    pub(crate) fn matches(&self, parameter_type: ParameterType) -> bool {
        matches!(
            (self, parameter_type),
            (Self::String(_), ParameterType::String)
                | (Self::Integer(_), ParameterType::Integer)
                | (Self::Boolean(_), ParameterType::Boolean)
                | (Self::List(_), ParameterType::List)
        )
    }
    pub(crate) fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

impl ParameterScalar {
    fn parse(value: Value) -> Result<Self, String> {
        match value {
            Value::String(value) => Ok(Self::String(value)),
            Value::Number(value) => value
                .as_i64()
                .map(Self::Integer)
                .ok_or_else(|| "list elements must be integers, not floats".to_owned()),
            Value::Bool(value) => Ok(Self::Boolean(value)),
            Value::Tagged(_) => Err("list elements must not use YAML tags".to_owned()),
            _ => Err(
                "list elements must be non-recursive string, integer, or boolean scalars"
                    .to_owned(),
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ValueOrReference {
    Value(ParameterValue),
    Reference(String),
}

impl<'de> Deserialize<'de> for ValueOrReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if let Value::String(value) = &value {
            if value.contains("{ parameter:") {
                return Err(D::Error::custom(
                    "parameter references must occupy the complete YAML value",
                ));
            }
        }
        if let Value::Mapping(mapping) = &value {
            if mapping.len() == 1 {
                let key = Value::String("parameter".to_owned());
                if let Some(Value::String(name)) = mapping.get(&key) {
                    return Ok(Self::Reference(name.clone()));
                }
            }
            return Err(D::Error::custom(
                "reference must be exactly { parameter: name }",
            ));
        }
        ParameterValue::parse(value)
            .map(Self::Value)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct IncludeDeclaration {
    id: String,
    #[serde(default, rename = "as")]
    as_name: Option<String>,
    #[serde(default, rename = "with")]
    bindings: BTreeMap<String, ValueOrReference>,
}
impl IncludeDeclaration {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn as_name(&self) -> Option<&str> {
        self.as_name.as_deref()
    }
    pub(crate) fn bindings(&self) -> &BTreeMap<String, ValueOrReference> {
        &self.bindings
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileResourceDeclaration {
    #[serde(rename = "type")]
    resource_type: FileResourceType,
    properties: FileLinkProperties,
}
impl FileResourceDeclaration {
    pub(crate) fn properties(&self) -> &FileLinkProperties {
        &self.properties
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum FileResourceType {
    #[serde(rename = "file")]
    File,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileLinkProperties {
    kind: ValueOrReference,
    source: SourceDeclaration,
    target: ValueOrReference,
    operation: ValueOrReference,
}
impl FileLinkProperties {
    pub(crate) fn source(&self) -> &SourceDeclaration {
        &self.source
    }
    pub(crate) fn target(&self) -> &ValueOrReference {
        &self.target
    }
    pub(crate) fn operation(&self) -> &ValueOrReference {
        &self.operation
    }
    pub(crate) fn kind(&self) -> &ValueOrReference {
        &self.kind
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDeclaration {
    store: String,
    path: String,
}
impl SourceDeclaration {
    pub(crate) fn store(&self) -> &str {
        &self.store
    }
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileKind {
    File,
    Directory,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileOperation {
    Link,
    Copy,
}
pub(crate) fn parse_kind(value: &str) -> Option<FileKind> {
    match value {
        "file" => Some(FileKind::File),
        "directory" => Some(FileKind::Directory),
        _ => None,
    }
}
pub(crate) fn parse_operation(value: &str) -> Option<FileOperation> {
    match value {
        "link" => Some(FileOperation::Link),
        "copy" => Some(FileOperation::Copy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_schema_four_parameters_and_references_parse() {
        let profile = ProfileDeclaration::parse("schema_version: 4\nid: base\nparameters: { name: string, count: integer, enabled: boolean, values: list }\nincludes: [{ id: child, as: child, with: { name: { parameter: name } } }]\nresources: {}\n").unwrap();
        assert_eq!(profile.parameters().len(), 4);
        assert!(matches!(
            profile.includes()[0].bindings()["name"],
            ValueOrReference::Reference(_)
        ));
    }
    #[test]
    fn rejects_invalid_reference_and_recursive_list() {
        for yaml in [
            "schema_version: 4\nid: base\nresources: { x: { type: file, properties: { kind: { parameter: x, extra: y }, source: { store: s, path: p }, target: /x, operation: link } } }\n",
            "schema_version: 4\nid: base\nresources: {}\nincludes: [{ id: x, with: { p: [a, [b]] } }]\n",
        ] {
            assert!(ProfileDeclaration::parse(yaml).is_err());
        }
    }

    #[test]
    fn parameter_values_accept_each_closed_type_and_preserve_list_order() {
        let values = [
            (
                "string",
                "plain",
                ParameterValue::String("plain".to_owned()),
            ),
            ("integer", "42", ParameterValue::Integer(42)),
            ("boolean", "true", ParameterValue::Boolean(true)),
            ("empty list", "[]", ParameterValue::List(Vec::new())),
            (
                "ordered scalar list",
                "[first, 2, false]",
                ParameterValue::List(vec![
                    ParameterScalar::String("first".to_owned()),
                    ParameterScalar::Integer(2),
                    ParameterScalar::Boolean(false),
                ]),
            ),
        ];

        for (name, yaml, expected) in values {
            let value = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(ParameterValue::parse(value).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn parameter_value_and_reference_grammar_reject_invalid_yaml_forms() {
        for (name, yaml) in [
            ("null", "null"),
            ("float", "1.5"),
            ("tagged scalar", "!custom value"),
            ("mapping lookalike", "{ value: plain }"),
            ("nested list", "[plain, [nested]]"),
            ("tagged list element", "[!custom value]"),
        ] {
            let value = serde_yaml::from_str(yaml).unwrap();
            assert!(ParameterValue::parse(value).is_err(), "{name} must reject");
        }

        for (name, yaml) in [
            ("partial string reference", "'prefix { parameter: name }'"),
            (
                "reference with extra field",
                "{ parameter: name, extra: value }",
            ),
            ("reference with non-string name", "{ parameter: 1 }"),
            ("reference in list element", "[{ parameter: name }]"),
        ] {
            assert!(
                serde_yaml::from_str::<ValueOrReference>(yaml).is_err(),
                "{name} must reject"
            );
        }
    }

    #[test]
    fn profile_declaration_rejects_unknown_types_and_null_property_values() {
        for (name, yaml) in [
            (
                "unknown parameter type",
                "schema_version: 4\nid: base\nparameters: { value: path }\nresources: {}\n",
            ),
            (
                "null property",
                "schema_version: 4\nid: base\nresources: { x: { type: file, properties: { kind: null, source: { store: s, path: p }, target: /x, operation: link } } }\n",
            ),
        ] {
            assert!(
                ProfileDeclaration::parse(yaml).is_err(),
                "{name} must reject"
            );
        }
    }
}
