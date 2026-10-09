//! Strict raw DTO for one discovered profile declaration.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::declaration::schema::ProfileSchemaVersionV3;

/// One profile file before identifier validation, include composition, and path binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileDeclaration {
    #[allow(dead_code)]
    schema_version: ProfileSchemaVersionV3,
    id: String,
    #[serde(default)]
    includes: Vec<IncludeDeclaration>,
    resources: BTreeMap<String, FileResourceDeclaration>,
}

impl ProfileDeclaration {
    /// Parses only the strict version-2 profile declaration schema.
    pub(crate) fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// The raw profile ID before identifier validation.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Includes in their declaration order.
    pub(crate) fn includes(&self) -> &[IncludeDeclaration] {
        &self.includes
    }

    /// Resources keyed by their raw declaration ID.
    pub(crate) fn resources(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &FileResourceDeclaration)> {
        self.resources
            .iter()
            .map(|(resource_id, resource)| (resource_id.as_str(), resource))
    }
}

/// One profile include before identifier validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct IncludeDeclaration {
    id: String,
}

impl IncludeDeclaration {
    /// The raw included profile ID.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}

/// The only v0.2.0 resource declaration shape.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileResourceDeclaration {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    resource_type: FileResourceType,
    properties: FileLinkProperties,
}

impl FileResourceDeclaration {
    /// File-link properties before source and target resolution.
    pub(crate) fn properties(&self) -> &FileLinkProperties {
        &self.properties
    }
}

/// The only supported profile resource type in v0.2.0.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
enum FileResourceType {
    #[serde(rename = "file")]
    File,
}

/// Raw properties of the only v0.2.0 file-link resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileLinkProperties {
    #[allow(dead_code)]
    kind: FileKind,
    source: SourceDeclaration,
    target: String,
    operation: FileOperation,
}

impl<'de> Deserialize<'de> for FileLinkProperties {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            kind: FileKind,
            source: SourceDeclaration,
            target: String,
            operation: FileOperation,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.kind == FileKind::Directory && raw.operation == FileOperation::Copy {
            return Err(serde::de::Error::custom(
                "directory resources support only operation: link",
            ));
        }
        Ok(Self {
            kind: raw.kind,
            source: raw.source,
            target: raw.target,
            operation: raw.operation,
        })
    }
}

impl FileLinkProperties {
    /// The raw source declaration before store lookup and path verification.
    pub(crate) fn source(&self) -> &SourceDeclaration {
        &self.source
    }

    /// The raw target declaration before home binding.
    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    /// The declared closed file materialization effect.
    pub(crate) fn operation(&self) -> FileOperation {
        self.operation
    }

    pub(crate) fn kind(&self) -> FileKind {
        self.kind
    }
}

/// Raw local-store source syntax before resolver validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDeclaration {
    store: String,
    path: String,
}

impl SourceDeclaration {
    /// The raw store ID.
    pub(crate) fn store(&self) -> &str {
        &self.store
    }

    /// The raw slash-separated source path.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}

/// The only permitted file-link kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum FileKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
}

/// The only permitted file-link operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum FileOperation {
    #[serde(rename = "link")]
    Link,
    #[serde(rename = "copy")]
    Copy,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_PROFILE: &str = "schema_version: 3\nid: workstation\nincludes:\n  - id: base\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/.gitconfig\n      operation: link\n";

    #[test]
    fn profile_declaration_preserves_raw_profile_resource_and_path_syntax() {
        let profile = ProfileDeclaration::parse(VALID_PROFILE).unwrap();

        assert_eq!(profile.id(), "workstation");
        assert_eq!(profile.includes()[0].id(), "base");
        let (resource_id, resource) = profile.resources().next().unwrap();
        assert_eq!(resource_id, "git-config");
        assert_eq!(resource.properties().source().store(), "dotfiles");
        assert_eq!(resource.properties().source().path(), "git/config");
        assert_eq!(resource.properties().target(), "~/.gitconfig");
    }

    #[test]
    fn profile_declaration_rejects_schema_errors_at_every_object_level() {
        for (fixture, yaml) in [
            ("missing schema version", "id: workstation\nresources: {}\n"),
            (
                "unsupported schema version",
                "schema_version: 2\nid: workstation\nresources: {}\n",
            ),
            (
                "unknown top-level field",
                "schema_version: 3\nid: workstation\nresources: {}\nname: workstation\n",
            ),
            (
                "unknown include field",
                "schema_version: 3\nid: workstation\nincludes:\n  - id: base\n    path: profiles/base.yaml\nresources: {}\n",
            ),
            (
                "unsupported resource type",
                "schema_version: 3\nid: workstation\nresources:\n  install:\n    type: task\n    properties: {}\n",
            ),
            (
                "unknown resource field",
                "schema_version: 3\nid: workstation\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/.gitconfig\n      operation: link\n    optional: true\n",
            ),
            (
                "unknown properties field",
                "schema_version: 3\nid: workstation\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/.gitconfig\n      operation: link\n      mode: 0600\n",
            ),
            (
                "unknown source field",
                "schema_version: 3\nid: workstation\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n        revision: main\n      target: ~/.gitconfig\n      operation: link\n",
            ),
        ] {
            assert!(
                ProfileDeclaration::parse(yaml).is_err(),
                "{fixture} must reject"
            );
        }
    }

    #[test]
    fn profile_declaration_defaults_includes_to_empty() {
        let profile =
            ProfileDeclaration::parse("schema_version: 3\nid: workstation\nresources: {}\n")
                .unwrap();

        assert!(profile.includes().is_empty());
    }

    #[test]
    fn profile_declaration_accepts_directory_link_and_rejects_directory_copy() {
        let directory_link = "schema_version: 3\nid: workstation\nresources:\n  config:\n    type: file\n    properties:\n      kind: directory\n      source: { store: dotfiles, path: nvim }\n      target: ~/.config/nvim\n      operation: link\n";
        assert!(ProfileDeclaration::parse(directory_link).is_ok());

        let directory_copy = directory_link.replace("operation: link", "operation: copy");
        assert!(ProfileDeclaration::parse(&directory_copy).is_err());
    }
}
