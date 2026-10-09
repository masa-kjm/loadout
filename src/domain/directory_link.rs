//! Resolved directory-link definitions shared by Desired and Known state.

use std::fmt;

use crate::domain::file_link::LinkTarget;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;

/// A canonical desired directory-link definition with no declaration-level syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedDirectoryLink {
    resource_id: FullyQualifiedResourceId,
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    link_target: LinkTarget,
}

impl ResolvedDirectoryLink {
    /// Creates a resolved definition whose link target is the absolute source path.
    pub(crate) fn new(
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
    ) -> Result<Self, ResolvedDirectoryLinkError> {
        if source_path == target_path {
            return Err(ResolvedDirectoryLinkError::SourceEqualsTarget);
        }

        Ok(Self {
            resource_id,
            link_target: LinkTarget::new(source_path.clone()),
            source_path,
            target_path,
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
    pub(crate) fn link_target(&self) -> &LinkTarget {
        &self.link_target
    }
}

/// The reason a resolved directory-link definition is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedDirectoryLinkError {
    SourceEqualsTarget,
}

impl fmt::Display for ResolvedDirectoryLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a directory-link source and target must not be the same path")
    }
}

impl std::error::Error for ResolvedDirectoryLinkError {}
