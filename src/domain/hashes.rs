//! Canonical JCS and SHA-256 hashes for resolved v0.2 file-link definitions.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::domain::desired::ResolvedDesired;
use crate::domain::file_link::ResolvedFileLink;

const SHA256_PREFIX: &str = "sha256:";
const RESOLVED_FILE_LINK_FORMAT: &str = "loadout.resolved-file-link.v1";
const RESOLVED_DESIRED_FORMAT: &str = "loadout.resolved-desired.v1";

/// A SHA-256 hash of one canonical resolved file-link definition.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DefinitionHash(String);

impl DefinitionHash {
    /// Parses the persisted `sha256:<lowercase-hex>` representation.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, HashParseError> {
        let value = value.into();
        validate_hash(&value)?;
        Ok(Self(value))
    }

    /// Returns the persisted hash representation.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DefinitionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A SHA-256 hash of the complete canonical Resolved Desired set.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DesiredHash(String);

impl DesiredHash {
    /// Parses the persisted `sha256:<lowercase-hex>` representation.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, HashParseError> {
        let value = value.into();
        validate_hash(&value)?;
        Ok(Self(value))
    }

    /// Returns the persisted hash representation.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DesiredHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Produces the definition hash from the exact v0.2 canonical representation.
pub(crate) fn definition_hash(
    resource: &ResolvedFileLink,
) -> Result<DefinitionHash, CanonicalHashError> {
    let definition = CanonicalFileLink::from_resolved(resource)?;
    Ok(DefinitionHash(sha256(&canonical_json(&definition))))
}

/// Produces the desired-set hash from canonically ordered resolved resources.
pub(crate) fn desired_hash(desired: &ResolvedDesired) -> Result<DesiredHash, CanonicalHashError> {
    let mut resources = desired
        .resources()
        .iter()
        .map(CanonicalDesiredResource::from_resolved)
        .collect::<Result<Vec<_>, _>>()?;
    resources.sort_by(|left, right| left.resource_id.cmp(&right.resource_id));

    let desired = CanonicalDesired {
        format: RESOLVED_DESIRED_FORMAT,
        resources,
    };
    Ok(DesiredHash(sha256(&canonical_json(&desired))))
}

fn canonical_json(value: &impl Serialize) -> Vec<u8> {
    // Every v0.2 hash input below consists only of validated UTF-8 strings and static ASCII literals. Serializing to an in-memory vector cannot fail for that schema, so a failure would indicate an implementation defect.
    serde_json_canonicalizer::to_vec(value)
        .expect("fixed resolved hash schema must serialize as RFC 8785 JCS")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{SHA256_PREFIX}{:x}", Sha256::digest(bytes))
}

fn validate_hash(value: &str) -> Result<(), HashParseError> {
    let Some(hex) = value.strip_prefix(SHA256_PREFIX) else {
        return Err(HashParseError::MissingSha256Prefix);
    };
    if hex.len() != 64 {
        return Err(HashParseError::WrongLength);
    }
    if hex
        .bytes()
        .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(HashParseError::NotLowercaseHex);
    }

    Ok(())
}

#[derive(Serialize)]
struct CanonicalFileLink {
    #[serde(rename = "type")]
    resource_type: &'static str,
    target_path: String,
    operation: &'static str,
    kind: &'static str,
    source_path: String,
    format: &'static str,
}

impl CanonicalFileLink {
    fn from_resolved(resource: &ResolvedFileLink) -> Result<Self, CanonicalHashError> {
        Ok(Self {
            resource_type: "file",
            target_path: resolved_path_utf8(resource.target_path())?,
            operation: "link",
            kind: "file",
            source_path: resolved_path_utf8(resource.source_path())?,
            format: RESOLVED_FILE_LINK_FORMAT,
        })
    }
}

#[derive(Serialize)]
struct CanonicalDesired {
    format: &'static str,
    resources: Vec<CanonicalDesiredResource>,
}

#[derive(Serialize)]
struct CanonicalDesiredResource {
    definition: CanonicalFileLink,
    resource_id: String,
}

impl CanonicalDesiredResource {
    fn from_resolved(resource: &ResolvedFileLink) -> Result<Self, CanonicalHashError> {
        Ok(Self {
            definition: CanonicalFileLink::from_resolved(resource)?,
            resource_id: resource.resource_id().as_str().to_owned(),
        })
    }
}

fn resolved_path_utf8(
    path: &crate::domain::paths::ResolvedPath,
) -> Result<String, CanonicalHashError> {
    path.as_path()
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| CanonicalHashError::NonUnicodePath {
            path: path.as_path().to_path_buf(),
        })
}

/// The reason a resolved value cannot be rendered as the fixed JCS hash input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CanonicalHashError {
    NonUnicodePath { path: PathBuf },
}

impl fmt::Display for CanonicalHashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUnicodePath { path } => write!(
                formatter,
                "resolved path cannot be represented in the UTF-8 canonical hash input: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CanonicalHashError {}

/// The reason a persisted hash string is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HashParseError {
    MissingSha256Prefix,
    WrongLength,
    NotLowercaseHex,
}

impl fmt::Display for HashParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSha256Prefix => {
                formatter.write_str("hash must begin with the sha256: prefix")
            }
            Self::WrongLength => formatter.write_str("sha256 hash must have exactly 64 hex digits"),
            Self::NotLowercaseHex => {
                formatter.write_str("sha256 hash must use lowercase hexadecimal digits")
            }
        }
    }
}

impl std::error::Error for HashParseError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::domain::desired::ResolvedDesired;
    #[cfg(unix)]
    use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
    #[cfg(unix)]
    use crate::domain::paths::ResolvedPath;

    #[cfg(unix)]
    fn file_link(resource_id: &str) -> ResolvedFileLink {
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(resource_id).unwrap(),
            ResolvedPath::new("/home/example/dotfiles/git/config").unwrap(),
            ResolvedPath::new("/home/example/.gitconfig").unwrap(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn exact_definition_hash_fixture_uses_the_v0_2_canonical_value() {
        let resource = file_link("base/git-config");

        assert_eq!(
            canonical_json(&CanonicalFileLink::from_resolved(&resource).unwrap()),
            br#"{"format":"loadout.resolved-file-link.v1","kind":"file","operation":"link","source_path":"/home/example/dotfiles/git/config","target_path":"/home/example/.gitconfig","type":"file"}"#
        );
        assert_eq!(
            definition_hash(&resource).unwrap().as_str(),
            "sha256:1250e6bf4abdb529664c1444709aeae2ce814e8b97c3984e32d46d9c971b3a5e"
        );
    }

    #[cfg(unix)]
    #[test]
    fn exact_desired_hash_fixture_uses_sorted_resource_identity() {
        let desired = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            vec![file_link("base/git-config")],
        )
        .unwrap();

        assert_eq!(
            desired_hash(&desired).unwrap().as_str(),
            "sha256:2f2e1e550b61eb6fd9916996456d1f3c1321df8f6e3c568d4711c358ffa86e54"
        );
    }

    #[cfg(unix)]
    #[test]
    fn equal_definitions_with_different_resource_ids_have_equal_definition_and_distinct_desired_hashes()
     {
        let base_resource = file_link("base/git-config");
        let work_resource = file_link("workstation/git-config");
        let base_desired = ResolvedDesired::new(
            ProfileId::parse("base").unwrap(),
            vec![base_resource.clone()],
        )
        .unwrap();
        let work_desired = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            vec![work_resource.clone()],
        )
        .unwrap();

        assert_eq!(
            definition_hash(&base_resource).unwrap(),
            definition_hash(&work_resource).unwrap()
        );
        assert_ne!(
            desired_hash(&base_desired).unwrap(),
            desired_hash(&work_desired).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn desired_hash_is_independent_of_resolved_resource_input_order() {
        let first = file_link("base/git-config");
        let second = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/zsh").unwrap(),
            ResolvedPath::new("/home/example/dotfiles/zsh/.zshrc").unwrap(),
            ResolvedPath::new("/home/example/.zshrc").unwrap(),
        )
        .unwrap();
        let ordered = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            vec![first.clone(), second.clone()],
        )
        .unwrap();
        let reversed = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            vec![second, first],
        )
        .unwrap();

        assert_eq!(
            desired_hash(&ordered).unwrap(),
            desired_hash(&reversed).unwrap()
        );
    }

    #[test]
    fn persisted_hashes_require_sha256_lowercase_hex_format() {
        let valid = format!("sha256:{}", "a".repeat(64));

        assert_eq!(
            DefinitionHash::parse(valid.clone()).unwrap().as_str(),
            valid
        );
        assert_eq!(DesiredHash::parse(valid).unwrap().as_str().len(), 71);
        assert_eq!(
            DefinitionHash::parse("sha512:abcd").unwrap_err(),
            HashParseError::MissingSha256Prefix
        );
        assert_eq!(
            DefinitionHash::parse(format!("sha256:{}", "a".repeat(63))).unwrap_err(),
            HashParseError::WrongLength
        );
        assert_eq!(
            DefinitionHash::parse(format!("sha256:{}", "A".repeat(64))).unwrap_err(),
            HashParseError::NotLowercaseHex
        );
    }
}
