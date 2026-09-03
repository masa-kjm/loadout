//! No-follow Actual observations supplied to the pure planner.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;

/// The no-follow safety classification for a target's parent path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParentSafety {
    Safe,
    Missing,
    NotDirectory,
    Symlink,
    Junction,
    ReparsePoint,
}

impl ParentSafety {
    /// Whether the parent path permits final-target observation or mutation.
    pub(crate) fn is_safe(self) -> bool {
        matches!(self, Self::Safe)
    }
}

/// A no-follow target entry that is not a supported file symbolic link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OtherEntryKind {
    RegularFile,
    Directory,
    Junction,
    ReparsePoint,
    Unsupported,
}

/// The planner-relevant classification of one target path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TargetObservation {
    Missing,
    ExpectedLink { link_target: LinkTarget },
    MatchingUnmanagedLink { link_target: LinkTarget },
    OtherLink { link_target: LinkTarget },
    OtherEntry { kind: OtherEntryKind },
    UnsafePath { parent_safety: ParentSafety },
}

impl TargetObservation {
    /// The parent safety represented by this observation.
    pub(crate) fn parent_safety(&self) -> ParentSafety {
        match self {
            Self::UnsafePath { parent_safety } => *parent_safety,
            Self::Missing
            | Self::ExpectedLink { .. }
            | Self::MatchingUnmanagedLink { .. }
            | Self::OtherLink { .. }
            | Self::OtherEntry { .. } => ParentSafety::Safe,
        }
    }
}

/// An observation of one resolved target and every relevant parent-path fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActualFileLink {
    target_path: ResolvedPath,
    observation: TargetObservation,
}

impl ActualFileLink {
    /// Creates a target observation with coherent parent-safety information.
    pub(crate) fn new(
        target_path: ResolvedPath,
        observation: TargetObservation,
    ) -> Result<Self, ActualFileLinkError> {
        if matches!(
            observation,
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Safe
            }
        ) {
            return Err(ActualFileLinkError::UnsafePathMarkedSafe);
        }

        Ok(Self {
            target_path,
            observation,
        })
    }

    /// Returns the observed target path.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }

    /// Returns the complete no-follow observation classification.
    pub(crate) fn observation(&self) -> &TargetObservation {
        &self.observation
    }
}

/// The reason an Actual observation is internally contradictory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActualFileLinkError {
    UnsafePathMarkedSafe,
}

impl fmt::Display for ActualFileLinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePathMarkedSafe => {
                formatter.write_str("an unsafe target path must record an unsafe parent condition")
            }
        }
    }
}

impl std::error::Error for ActualFileLinkError {}

/// The complete no-follow observations supplied to one planner invocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ActualState {
    targets: BTreeMap<ResolvedPath, ActualFileLink>,
}

impl ActualState {
    /// Builds a target-indexed Actual state without duplicate observations.
    pub(crate) fn new(
        observations: impl IntoIterator<Item = ActualFileLink>,
    ) -> Result<Self, ActualStateError> {
        let mut targets = BTreeMap::new();

        for observation in observations {
            let target_path = observation.target_path().clone();
            if targets.insert(target_path.clone(), observation).is_some() {
                return Err(ActualStateError::DuplicateTargetObservation { target_path });
            }
        }

        Ok(Self { targets })
    }

    /// Looks up the observation for one resolved target path.
    pub(crate) fn get(&self, target_path: &ResolvedPath) -> Option<&ActualFileLink> {
        self.targets.get(target_path)
    }

    /// Iterates over observations in deterministic resolved-path order.
    pub(crate) fn observations(&self) -> impl ExactSizeIterator<Item = &ActualFileLink> {
        self.targets.values()
    }
}

/// The reason Actual state cannot represent a coherent planner input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ActualStateError {
    DuplicateTargetObservation { target_path: ResolvedPath },
}

impl fmt::Display for ActualStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTargetObservation { target_path } => {
                write!(
                    formatter,
                    "duplicate Actual observation for target: {target_path}"
                )
            }
        }
    }
}

impl std::error::Error for ActualStateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-domain-actual")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn actual_observation_preserves_all_file_link_planner_categories() {
        let target = path("home/.gitconfig");
        let link_target = LinkTarget::new(path("store/git/config"));
        let observations = [
            TargetObservation::Missing,
            TargetObservation::ExpectedLink {
                link_target: link_target.clone(),
            },
            TargetObservation::MatchingUnmanagedLink {
                link_target: link_target.clone(),
            },
            TargetObservation::OtherLink { link_target },
            TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            },
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Symlink,
            },
        ];

        for observation in observations {
            let actual = ActualFileLink::new(target.clone(), observation.clone()).unwrap();
            assert_eq!(actual.observation(), &observation);
        }
    }

    #[test]
    fn actual_observation_rejects_an_unsafe_path_without_an_unsafe_parent() {
        assert_eq!(
            ActualFileLink::new(
                path("home/.gitconfig"),
                TargetObservation::UnsafePath {
                    parent_safety: ParentSafety::Safe,
                },
            )
            .unwrap_err(),
            ActualFileLinkError::UnsafePathMarkedSafe
        );
    }

    #[test]
    fn actual_state_rejects_duplicate_observations_for_one_target() {
        let target = path("home/.gitconfig");
        let duplicate = ActualState::new([
            ActualFileLink::new(target.clone(), TargetObservation::Missing).unwrap(),
            ActualFileLink::new(target, TargetObservation::Missing).unwrap(),
        ])
        .unwrap_err();

        assert!(matches!(
            duplicate,
            ActualStateError::DuplicateTargetObservation { .. }
        ));
    }
}
