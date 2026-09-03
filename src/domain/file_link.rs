//! Resolved file-link definitions shared by Desired and Known state.

use std::fmt;

use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;

/// The exact normalized, absolute target stored in a file symbolic link.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct LinkTarget(ResolvedPath);

impl LinkTarget {
    /// Wraps an already resolved path for use as an absolute symbolic-link target.
    pub(crate) fn new(path: ResolvedPath) -> Self {
        Self(path)
    }

    /// Returns the resolved path represented by this link target.
    pub(crate) fn as_path(&self) -> &ResolvedPath {
        &self.0
    }
}

impl fmt::Display for LinkTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A canonical desired file-link definition with no declaration-level syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedFileLink {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    link_target: LinkTarget,
}

impl ResolvedFileLink {
    /// Creates a resolved definition whose link target is the absolute source path.
    ///
    /// This constructor establishes only path-value invariants. Source existence, physical containment, and regular-file verification belong to the source inspection boundary.
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
    ) -> Result<Self, ResolvedFileLinkError> {
        if source_path == target_path {
            return Err(ResolvedFileLinkError::SourceEqualsTarget);
        }

        Ok(Self {
            resource_id,
            link_target: LinkTarget::new(source_path.clone()),
            source_path,
            target_path,
        })
    }

    /// Returns the stable desired resource identity.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }

    /// Returns the resolved source path.
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }

    /// Returns the resolved materialization target path.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }

    /// Returns the exact absolute link target that must be materialized.
    pub(crate) fn link_target(&self) -> &LinkTarget {
        &self.link_target
    }
}

/// The reason a resolved file-link definition is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedFileLinkError {
    SourceEqualsTarget,
}

impl fmt::Display for ResolvedFileLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceEqualsTarget => {
                formatter.write_str("a file-link source and target must not be the same path")
            }
        }
    }
}

impl std::error::Error for ResolvedFileLinkError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::FullyQualifiedResourceId;

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-domain-file-link")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn resolved_file_link_derives_an_absolute_link_target_from_the_source() {
        let source = path("store/git/config");
        let target = path("home/.gitconfig");
        let file_link = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/git-config").unwrap(),
            source.clone(),
            target.clone(),
        )
        .unwrap();

        assert_eq!(file_link.source_path(), &source);
        assert_eq!(file_link.target_path(), &target);
        assert_eq!(file_link.link_target().as_path(), &source);
    }

    #[test]
    fn resolved_file_link_rejects_a_target_that_would_overwrite_its_source() {
        let source = path("store/git/config");

        assert_eq!(
            ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git-config").unwrap(),
                source.clone(),
                source,
            )
            .unwrap_err(),
            ResolvedFileLinkError::SourceEqualsTarget
        );
    }
}
