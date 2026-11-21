//! Structured blocking diagnostics emitted by the pure planner.

use crate::domain::actual::{CopyTargetObservation, TargetObservation};
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
    UnexpectedCopyTarget {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        observation: CopyTargetObservation,
    },
    MissingActualObservation {
        target_path: ResolvedPath,
    },
    UnsupportedPlatform {
        resource_id: FullyQualifiedResourceId,
        action_kind: ActionKind,
    },
    OrderingBlocked {
        resource_id: FullyQualifiedResourceId,
        reason: OrderingBlockReason,
    },
    UnsupportedResourceEffect {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        effect: &'static str,
    },
    IdentityHandoffPrecondition {
        old_resource_id: FullyQualifiedResourceId,
        new_resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    RecordedOrderConflict {
        old_before: FullyQualifiedResourceId,
        old_after: FullyQualifiedResourceId,
        old_sources: Vec<String>,
        proposed_before: FullyQualifiedResourceId,
        proposed_after: FullyQualifiedResourceId,
        proposed_sources: Vec<String>,
        rebuild_set: Vec<FullyQualifiedResourceId>,
    },
}

/// The ordering condition that prevented a safe operation graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OrderingBlockReason {
    UnsupportedActionRoles,
    UnprovedPredecessor,
    UnprovedRecordedPredecessor,
    UnprovedTopologyPublication,
    Cycle,
}

impl Diagnostic {
    /// Every v0.2 planner diagnostic currently represented here blocks mutation.
    pub(crate) fn is_blocking(&self) -> bool {
        true
    }
}
