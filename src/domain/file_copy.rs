//! Resolved file-copy definitions and byte-content ownership facts.

use std::fmt;

use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;

/// A verified SHA-256 fingerprint of exact regular-file bytes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ContentFingerprint(String);

impl ContentFingerprint {
    /// Validates the persisted `sha256:<lowercase-hex>` representation.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, ContentFingerprintError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ContentFingerprintError::MissingPrefix);
        };
        if hex.len() != 64 {
            return Err(ContentFingerprintError::WrongLength);
        }
        if hex
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        {
            return Err(ContentFingerprintError::NotLowercaseHex);
        }
        Ok(Self(value))
    }

    /// Returns the canonical persisted representation.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The reason a content fingerprint is not canonical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContentFingerprintError {
    MissingPrefix,
    WrongLength,
    NotLowercaseHex,
}

impl fmt::Display for ContentFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingPrefix => "content fingerprint is missing the sha256 prefix",
            Self::WrongLength => "content fingerprint must contain 64 hexadecimal characters",
            Self::NotLowercaseHex => {
                "content fingerprint must use lowercase hexadecimal characters"
            }
        })
    }
}

impl std::error::Error for ContentFingerprintError {}

/// A canonical desired file-copy definition with no declaration-level syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFileCopy {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    source_content_fingerprint: ContentFingerprint,
}

impl ResolvedFileCopy {
    /// Creates a resolved copy after source-byte inspection has established its fingerprint.
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
        source_content_fingerprint: ContentFingerprint,
    ) -> Result<Self, ResolvedFileCopyError> {
        if source_path == target_path {
            return Err(ResolvedFileCopyError::SourceEqualsTarget);
        }
        Ok(Self {
            resource_id,
            source_path,
            target_path,
            source_content_fingerprint,
        })
    }

    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn source_content_fingerprint(&self) -> &ContentFingerprint {
        &self.source_content_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedFileCopyError {
    SourceEqualsTarget,
}

impl fmt::Display for ResolvedFileCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a file-copy source and target must not be the same path")
    }
}

impl std::error::Error for ResolvedFileCopyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_rejects_noncanonical_values() {
        assert!(
            ContentFingerprint::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_ok()
        );
        assert!(
            ContentFingerprint::parse(
                "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            )
            .is_err()
        );
    }
}
