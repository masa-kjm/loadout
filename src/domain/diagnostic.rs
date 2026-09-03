//! Structured blocking diagnostics emitted by the pure planner.

use crate::domain::actual::TargetObservation;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::ActionKind;

/// A structured condition that makes a plan non-executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Diagnostic {
    TargetCollision {
        target_path: ResolvedPath,
        resource_ids: Vec<FullyQualifiedResourceId>,
    },
    UnexpectedTarget {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    MissingActualObservation {
        target_path: ResolvedPath,
    },
    UnsupportedPlatform {
        resource_id: FullyQualifiedResourceId,
        action_kind: ActionKind,
    },
    IdentityHandoffPrecondition {
        old_resource_id: FullyQualifiedResourceId,
        new_resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
}

impl Diagnostic {
    /// Every v0.2 planner diagnostic currently represented here blocks mutation.
    pub(crate) fn is_blocking(&self) -> bool {
        true
    }
}
