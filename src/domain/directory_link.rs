//! Resolved directory-link definitions shared by Desired and Known state.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::actual::{ParentSafety, TargetObservation};
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

/// A no-follow DirectoryLink target observation supplied to inspection and planning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActualDirectoryLink {
    target_path: ResolvedPath,
    observation: TargetObservation,
}

impl ActualDirectoryLink {
    /// Creates a target observation with coherent parent-safety information.
    pub(crate) fn new(
        target_path: ResolvedPath,
        observation: TargetObservation,
    ) -> Result<Self, ActualDirectoryLinkError> {
        if matches!(
            observation,
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Safe
            }
        ) {
            return Err(ActualDirectoryLinkError::UnsafePathMarkedSafe);
        }
        Ok(Self {
            target_path,
            observation,
        })
    }

    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }

    pub(crate) fn observation(&self) -> &TargetObservation {
        &self.observation
    }
}

/// The reason an Actual DirectoryLink observation is internally contradictory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActualDirectoryLinkError {
    UnsafePathMarkedSafe,
}

impl fmt::Display for ActualDirectoryLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unsafe target path must record an unsafe parent condition")
    }
}

impl std::error::Error for ActualDirectoryLinkError {}

/// Complete, unique no-follow DirectoryLink observations supplied to planning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ActualDirectoryLinkState(BTreeMap<ResolvedPath, ActualDirectoryLink>);

impl ActualDirectoryLinkState {
    pub(crate) fn new(
        values: impl IntoIterator<Item = ActualDirectoryLink>,
    ) -> Result<Self, ActualDirectoryLinkStateError> {
        let mut targets = BTreeMap::new();
        for value in values {
            let target_path = value.target_path.clone();
            if targets.insert(target_path.clone(), value).is_some() {
                return Err(ActualDirectoryLinkStateError::DuplicateTargetObservation {
                    target_path,
                });
            }
        }
        Ok(Self(targets))
    }

    pub(crate) fn get(&self, target: &ResolvedPath) -> Option<&TargetObservation> {
        self.0.get(target).map(ActualDirectoryLink::observation)
    }
}

/// The reason DirectoryLink Actual state cannot be supplied to the planner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ActualDirectoryLinkStateError {
    DuplicateTargetObservation { target_path: ResolvedPath },
}

impl fmt::Display for ActualDirectoryLinkStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTargetObservation { target_path } => {
                write!(
                    formatter,
                    "duplicate DirectoryLink Actual observation for {target_path}"
                )
            }
        }
    }
}

impl std::error::Error for ActualDirectoryLinkStateError {}
