//! Typed operation progress recorded before and after resource execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::domain::file_copy::ContentFingerprint;
use crate::domain::file_link::LinkTarget;
use crate::domain::hashes::DesiredHash;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::KnownResource;
use crate::domain::known::{KnownFileLink, KnownFileLinkError};
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{
    ActionKind, PlannedAction, PlannedEffectHandoff, PlannedFileCopyAction, TargetCondition,
};

/// An opaque operation identifier stored in `active_operation`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct OperationId(String);

impl OperationId {
    /// Validates an opaque operation identifier loaded from durable state.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, OperationRecordError> {
        let value = value.into();
        if value.is_empty() {
            return Err(OperationRecordError::EmptyOperationId);
        }
        Ok(Self(value))
    }

    /// Returns the stored opaque identifier.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque action identifier scoped to one operation record.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ActionId(String);

impl ActionId {
    /// Validates an opaque action identifier loaded from durable state.
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, OperationRecordError> {
        let value = value.into();
        if value.is_empty() {
            return Err(OperationRecordError::EmptyActionId);
        }
        Ok(Self(value))
    }

    /// Returns the stored opaque identifier.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The durable status of one planned action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Uncertain,
}

impl ActionStatus {
    /// Whether this status permits closing the operation record.
    pub(crate) fn closes_operation(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Skipped)
    }
}

/// The exact Known-state transition eligible after a recorded post-condition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecordedKnownStateUpdate {
    Upsert(KnownFileLink),
    UpsertCopy(crate::domain::known::KnownFileCopy),
    RemoveExpected(KnownFileLink),
    RemoveMissing {
        resource_id: FullyQualifiedResourceId,
    },
    ReplaceIdentity {
        old_resource: KnownFileLink,
        new_resource: KnownFileLink,
    },
}

/// One action whose recorded facts are sufficient for future recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedAction {
    facts: ActionFacts,
    status: ActionStatus,
}

/// Complete persisted facts required to reconstruct one copy action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistedCopyActionFacts {
    pub(crate) kind: ActionKind,
    pub(crate) resource_id: FullyQualifiedResourceId,
    pub(crate) source_path: ResolvedPath,
    pub(crate) target_path: ResolvedPath,
    pub(crate) content_fingerprint: ContentFingerprint,
    pub(crate) temporary_path: ResolvedPath,
    pub(crate) old_effect: Option<KnownResource>,
    pub(crate) final_effect: KnownResource,
    pub(crate) precondition: TargetCondition,
    pub(crate) postcondition: TargetCondition,
    pub(crate) status: ActionStatus,
}

/// Complete persisted facts required to reconstruct one managed effect handoff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistedEffectHandoffFacts {
    pub(crate) resource_id: FullyQualifiedResourceId,
    pub(crate) old_effect: KnownResource,
    pub(crate) final_effect: KnownResource,
    pub(crate) temporary_path: ResolvedPath,
    pub(crate) precondition: TargetCondition,
    pub(crate) postcondition: TargetCondition,
    pub(crate) status: ActionStatus,
}

/// Only the facts required by an implemented action are representable.
/// Preconditions and post-conditions are derived from these facts rather than stored independently where they could contradict the action kind.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ActionFacts {
    CreateLink {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        link_target: LinkTarget,
    },
    RemoveLink {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        link_target: LinkTarget,
    },
    ForgetMissing {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
    },
    ReplaceLink {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        old_link_target: LinkTarget,
        new_link_target: LinkTarget,
        temporary_path: ResolvedPath,
    },
    ReplaceOwnership {
        old_resource_id: FullyQualifiedResourceId,
        new_resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        old_link_target: LinkTarget,
        new_link_target: LinkTarget,
        temporary_path: Option<ResolvedPath>,
    },
    RelocateLink {
        resource_id: FullyQualifiedResourceId,
        old_target_path: ResolvedPath,
        new_target_path: ResolvedPath,
        old_link_target: LinkTarget,
        new_link_target: LinkTarget,
    },
    Copy {
        kind: ActionKind,
        resource_id: FullyQualifiedResourceId,
        source_path: ResolvedPath,
        target_path: ResolvedPath,
        content_fingerprint: ContentFingerprint,
        temporary_path: ResolvedPath,
        old_effect: Box<Option<KnownResource>>,
        final_effect: Box<KnownResource>,
        precondition: TargetCondition,
        postcondition: TargetCondition,
    },
    ReplaceEffect {
        resource_id: FullyQualifiedResourceId,
        old_effect: Box<KnownResource>,
        final_effect: Box<KnownResource>,
        temporary_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
    },
}

impl RecordedAction {
    /// Builds a record for one planner-selected action supported by the current vertical slice.
    pub(crate) fn from_action(action: &PlannedAction) -> Result<Self, OperationRecordError> {
        if matches!(
            action.kind(),
            ActionKind::ReplaceLink | ActionKind::RelocateLink
        ) {
            return Err(OperationRecordError::ReplacementTemporaryRequired);
        }
        let preconditions = action.preconditions();
        let postconditions = action.postconditions();
        let [precondition] = preconditions.as_slice() else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        let [postcondition] = postconditions.as_slice() else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        if precondition.target_path() != postcondition.target_path() {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        }

        Self::from_persisted(
            action.kind(),
            action.resource_id().clone(),
            precondition.target_path().clone(),
            precondition.clone(),
            postcondition.clone(),
            ActionStatus::Pending,
        )
    }

    /// Records a same-target replacement with its repository-allocated sibling.
    pub(crate) fn replace_link(
        action: &PlannedAction,
        temporary_path: ResolvedPath,
    ) -> Result<Self, OperationRecordError> {
        if action.kind() != ActionKind::ReplaceLink {
            return Err(OperationRecordError::UnsupportedActionKind {
                kind: action.kind(),
            });
        }
        let preconditions = action.preconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path,
                link_target: old_link_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        let postconditions = action.postconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path: post_target,
                link_target: new_link_target,
            },
        ] = postconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        if target_path != post_target
            || old_link_target == new_link_target
            || temporary_path == *target_path
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        }
        Ok(Self {
            facts: ActionFacts::ReplaceLink {
                resource_id: action.resource_id().clone(),
                target_path: target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
                temporary_path,
            },
            status: ActionStatus::Pending,
        })
    }

    pub(crate) fn replace_ownership(
        action: &PlannedAction,
        temporary_path: Option<ResolvedPath>,
    ) -> Result<Self, OperationRecordError> {
        if action.kind() != ActionKind::ReplaceOwnership {
            return Err(OperationRecordError::UnsupportedActionKind {
                kind: action.kind(),
            });
        }
        let old_resource_id = action
            .replaced_resource_id()
            .ok_or(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            })?
            .clone();
        let new_resource_id = action.resource_id().clone();
        let preconditions = action.preconditions();
        let postconditions = action.postconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path,
                link_target: old_link_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        let [
            TargetCondition::ExpectedLink {
                target_path: post_target,
                link_target: new_link_target,
            },
        ] = postconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        if target_path != post_target || old_resource_id == new_resource_id {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        }
        if (old_link_target == new_link_target) != temporary_path.is_none() {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        }
        if let Some(path) = &temporary_path {
            if path == target_path || path.as_ref().parent() != target_path.as_ref().parent() {
                return Err(OperationRecordError::InvalidActionConditions {
                    kind: action.kind(),
                });
            }
        }
        Ok(Self {
            facts: ActionFacts::ReplaceOwnership {
                old_resource_id,
                new_resource_id,
                target_path: target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
                temporary_path,
            },
            status: ActionStatus::Pending,
        })
    }

    pub(crate) fn relocate_link(action: &PlannedAction) -> Result<Self, OperationRecordError> {
        if action.kind() != ActionKind::RelocateLink {
            return Err(OperationRecordError::UnsupportedActionKind {
                kind: action.kind(),
            });
        }
        let preconditions = action.preconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path: old_target_path,
                link_target: old_link_target,
            },
            TargetCondition::Missing {
                target_path: pre_new_target_path,
            },
        ] = preconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        let postconditions = action.postconditions();
        let [
            TargetCondition::Missing {
                target_path: post_old_target_path,
            },
            TargetCondition::ExpectedLink {
                target_path: new_target_path,
                link_target: new_link_target,
            },
        ] = postconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        };
        if old_target_path == new_target_path
            || old_target_path != post_old_target_path
            || pre_new_target_path != new_target_path
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: action.kind(),
            });
        }
        Ok(Self {
            facts: ActionFacts::RelocateLink {
                resource_id: action.resource_id().clone(),
                old_target_path: old_target_path.clone(),
                new_target_path: new_target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
            },
            status: ActionStatus::Pending,
        })
    }

    /// Reconstructs a replacement record and its action-local sibling from strict persisted facts.
    pub(crate) fn from_persisted_replace_link(
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
        temporary_path: ResolvedPath,
        status: ActionStatus,
    ) -> Result<Self, OperationRecordError> {
        let action = PlannedAction::replace_link(
            crate::domain::file_link::ResolvedFileLink::new(
                resource_id.clone(),
                postcondition_link_target(&postcondition)?.as_path().clone(),
                target_path.clone(),
            )
            .map_err(|_| OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceLink,
            })?,
            KnownFileLink::new(
                resource_id,
                precondition_link_target(&precondition)?.as_path().clone(),
                target_path,
                precondition_link_target(&precondition)?.clone(),
            )
            .map_err(OperationRecordError::InvalidKnownFileLink)?,
        )
        .map_err(|_| OperationRecordError::InvalidActionConditions {
            kind: ActionKind::ReplaceLink,
        })?;
        if temporary_path.as_ref().parent()
            != action.preconditions()[0].target_path().as_ref().parent()
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceLink,
            });
        }
        let mut recorded = Self::replace_link(&action, temporary_path)?;
        recorded.status = status;
        Ok(recorded)
    }

    pub(crate) fn from_persisted_replace_ownership(
        old_resource_id: FullyQualifiedResourceId,
        new_resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
        temporary_path: Option<ResolvedPath>,
        status: ActionStatus,
    ) -> Result<Self, OperationRecordError> {
        let old_link_target = precondition_link_target(&precondition)?.clone();
        let new_link_target = postcondition_link_target(&postcondition)?.clone();
        if old_resource_id == new_resource_id
            || precondition.target_path() != &target_path
            || postcondition.target_path() != &target_path
            || (old_link_target == new_link_target) != temporary_path.is_none()
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceOwnership,
            });
        }
        if let Some(path) = &temporary_path {
            if path == &target_path || path.as_ref().parent() != target_path.as_ref().parent() {
                return Err(OperationRecordError::InvalidActionConditions {
                    kind: ActionKind::ReplaceOwnership,
                });
            }
        }
        Ok(Self {
            facts: ActionFacts::ReplaceOwnership {
                old_resource_id,
                new_resource_id,
                target_path,
                old_link_target,
                new_link_target,
                temporary_path,
            },
            status,
        })
    }

    pub(crate) fn from_persisted_relocate_link(
        resource_id: FullyQualifiedResourceId,
        old_target_path: ResolvedPath,
        new_target_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
        status: ActionStatus,
    ) -> Result<Self, OperationRecordError> {
        let old_link_target = precondition_link_target(&precondition)?.clone();
        let new_link_target = postcondition_link_target(&postcondition)?.clone();
        if old_target_path == new_target_path
            || precondition.target_path() != &old_target_path
            || postcondition.target_path() != &new_target_path
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::RelocateLink,
            });
        }
        Ok(Self {
            facts: ActionFacts::RelocateLink {
                resource_id,
                old_target_path,
                new_target_path,
                old_link_target,
                new_link_target,
            },
            status,
        })
    }

    /// Reconstructs a persisted action after validating its recovery facts.
    pub(crate) fn from_persisted(
        kind: ActionKind,
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
        status: ActionStatus,
    ) -> Result<Self, OperationRecordError> {
        if !matches!(
            kind,
            ActionKind::CreateLink | ActionKind::RemoveLink | ActionKind::ForgetMissing
        ) {
            return Err(OperationRecordError::UnsupportedActionKind { kind });
        }
        if precondition.target_path() != &target_path || postcondition.target_path() != &target_path
        {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        }
        let facts = match (kind, precondition, postcondition) {
            (
                ActionKind::CreateLink,
                TargetCondition::Missing { .. },
                TargetCondition::ExpectedLink { link_target, .. },
            ) => ActionFacts::CreateLink {
                resource_id,
                target_path,
                link_target,
            },
            (
                ActionKind::RemoveLink,
                TargetCondition::ExpectedLink { link_target, .. },
                TargetCondition::Missing { .. },
            ) => ActionFacts::RemoveLink {
                resource_id,
                target_path,
                link_target,
            },
            (
                ActionKind::ForgetMissing,
                TargetCondition::Missing { .. },
                TargetCondition::Missing { .. },
            ) => ActionFacts::ForgetMissing {
                resource_id,
                target_path,
            },
            _ => return Err(OperationRecordError::InvalidActionConditions { kind }),
        };
        Ok(Self { facts, status })
    }

    /// Reconstructs a copy action from its complete typed content-ownership facts.
    pub(crate) fn from_persisted_copy(
        facts: PersistedCopyActionFacts,
    ) -> Result<Self, OperationRecordError> {
        let PersistedCopyActionFacts {
            kind,
            resource_id,
            source_path,
            target_path,
            content_fingerprint,
            temporary_path,
            old_effect,
            final_effect,
            precondition,
            postcondition,
            status,
        } = facts;
        if !matches!(
            kind,
            ActionKind::CreateCopy | ActionKind::ReplaceCopy | ActionKind::RelocateCopy
        ) {
            return Err(OperationRecordError::UnsupportedActionKind { kind });
        }
        if postcondition.target_path() != &target_path {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        }
        if temporary_path == target_path
            || temporary_path.as_ref().parent() != target_path.as_ref().parent()
        {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        }
        let KnownResource::FileCopy(final_copy) = &final_effect else {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        };
        if final_copy.resource_id() != &resource_id
            || final_copy.source_path() != &source_path
            || final_copy.target_path() != &target_path
            || final_copy.content_fingerprint() != &content_fingerprint
        {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        }
        if !matches!(postcondition, TargetCondition::ExpectedCopy { content_fingerprint: ref post_fingerprint, .. } if post_fingerprint == &content_fingerprint)
        {
            return Err(OperationRecordError::InvalidActionConditions { kind });
        }
        match kind {
            ActionKind::CreateCopy
                if old_effect.is_none()
                    && matches!(precondition, TargetCondition::Missing { .. }) => {}
            ActionKind::ReplaceCopy | ActionKind::RelocateCopy => {
                let Some(KnownResource::FileCopy(old_copy)) = old_effect.as_ref() else {
                    return Err(OperationRecordError::InvalidActionConditions { kind });
                };
                if !matches!(precondition, TargetCondition::ExpectedCopy { ref target_path, ref content_fingerprint } if target_path == old_copy.target_path() && content_fingerprint == old_copy.content_fingerprint())
                    || (kind == ActionKind::ReplaceCopy && old_copy.target_path() != &target_path)
                    || (kind == ActionKind::RelocateCopy && old_copy.target_path() == &target_path)
                {
                    return Err(OperationRecordError::InvalidActionConditions { kind });
                }
            }
            _ => return Err(OperationRecordError::InvalidActionConditions { kind }),
        }
        Ok(Self {
            facts: ActionFacts::Copy {
                kind,
                resource_id,
                source_path,
                target_path,
                content_fingerprint,
                temporary_path,
                old_effect: Box::new(old_effect),
                final_effect: Box::new(final_effect),
                precondition,
                postcondition,
            },
            status,
        })
    }

    /// Records a copy materialization selected by the pure planner using its repository-allocated temporary sibling.
    pub(crate) fn copy(
        action: &PlannedFileCopyAction,
        temporary_path: ResolvedPath,
    ) -> Result<Self, OperationRecordError> {
        let kind = action.kind();
        if !matches!(
            kind,
            ActionKind::CreateCopy | ActionKind::ReplaceCopy | ActionKind::RelocateCopy
        ) {
            return Err(OperationRecordError::UnsupportedActionKind { kind });
        }
        let desired = action
            .desired()
            .ok_or(OperationRecordError::InvalidActionConditions { kind })?;
        let old_effect = action.previous().cloned().map(KnownResource::from);
        let preconditions = action.preconditions();
        let postconditions = action.postconditions();
        let precondition = match kind {
            ActionKind::RelocateCopy => preconditions.first().cloned(),
            _ => preconditions.into_iter().next(),
        }
        .ok_or(OperationRecordError::InvalidActionConditions { kind })?;
        let postcondition = match kind {
            ActionKind::RelocateCopy => postconditions.get(1).cloned(),
            _ => postconditions.into_iter().next(),
        }
        .ok_or(OperationRecordError::InvalidActionConditions { kind })?;
        Self::from_persisted_copy(PersistedCopyActionFacts {
            kind,
            resource_id: action.resource_id().clone(),
            source_path: desired.source_path().clone(),
            target_path: desired.target_path().clone(),
            content_fingerprint: desired.source_content_fingerprint().clone(),
            temporary_path,
            old_effect,
            final_effect: KnownResource::from(crate::domain::known::KnownFileCopy::from_resolved(
                desired,
            )),
            precondition,
            postcondition,
            status: ActionStatus::Pending,
        })
    }

    /// Reconstructs a managed link/copy handoff from complete old and final effects.
    pub(crate) fn from_persisted_replace_effect(
        facts: PersistedEffectHandoffFacts,
    ) -> Result<Self, OperationRecordError> {
        if facts.old_effect.resource_id() != &facts.resource_id
            || facts.final_effect.resource_id() != &facts.resource_id
            || facts.old_effect.target_path() != facts.final_effect.target_path()
            || facts.temporary_path == *facts.final_effect.target_path()
            || facts.temporary_path.as_ref().parent()
                != facts.final_effect.target_path().as_ref().parent()
            || !condition_matches_effect(&facts.precondition, &facts.old_effect)
            || !condition_matches_effect(&facts.postcondition, &facts.final_effect)
        {
            return Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceEffect,
            });
        }
        Ok(Self {
            facts: ActionFacts::ReplaceEffect {
                resource_id: facts.resource_id,
                old_effect: Box::new(facts.old_effect),
                final_effect: Box::new(facts.final_effect),
                temporary_path: facts.temporary_path,
                precondition: facts.precondition,
                postcondition: facts.postcondition,
            },
            status: facts.status,
        })
    }

    /// Records a managed cross-effect handoff using its repository-allocated temporary sibling.
    pub(crate) fn replace_effect(
        action: &PlannedEffectHandoff,
        temporary_path: ResolvedPath,
    ) -> Result<Self, OperationRecordError> {
        let final_effect = match action.final_effect() {
            crate::domain::desired::ResolvedResource::FileLink(resource) => {
                KnownResource::from(crate::domain::known::KnownFileLink::from_resolved(resource))
            }
            crate::domain::desired::ResolvedResource::FileCopy(resource) => {
                KnownResource::from(crate::domain::known::KnownFileCopy::from_resolved(resource))
            }
        };
        let precondition = condition_for_known_effect(action.old_effect());
        let postcondition = condition_for_known_effect(&final_effect);
        Self::from_persisted_replace_effect(PersistedEffectHandoffFacts {
            resource_id: action.resource_id().clone(),
            old_effect: action.old_effect().clone(),
            final_effect,
            temporary_path,
            precondition,
            postcondition,
            status: ActionStatus::Pending,
        })
    }

    /// The planned action kind represented by this persisted record.
    pub(crate) fn kind(&self) -> ActionKind {
        match &self.facts {
            ActionFacts::CreateLink { .. } => ActionKind::CreateLink,
            ActionFacts::RemoveLink { .. } => ActionKind::RemoveLink,
            ActionFacts::ForgetMissing { .. } => ActionKind::ForgetMissing,
            ActionFacts::ReplaceLink { .. } => ActionKind::ReplaceLink,
            ActionFacts::ReplaceOwnership { .. } => ActionKind::ReplaceOwnership,
            ActionFacts::RelocateLink { .. } => ActionKind::RelocateLink,
            ActionFacts::Copy { kind, .. } => *kind,
            ActionFacts::ReplaceEffect { .. } => ActionKind::ReplaceEffect,
        }
    }

    /// The stable resource identity affected by this action.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match &self.facts {
            ActionFacts::CreateLink { resource_id, .. }
            | ActionFacts::RemoveLink { resource_id, .. }
            | ActionFacts::ForgetMissing { resource_id, .. } => resource_id,
            ActionFacts::ReplaceLink { resource_id, .. } => resource_id,
            ActionFacts::ReplaceOwnership {
                new_resource_id, ..
            } => new_resource_id,
            ActionFacts::RelocateLink { resource_id, .. } => resource_id,
            ActionFacts::Copy { resource_id, .. } => resource_id,
            ActionFacts::ReplaceEffect { resource_id, .. } => resource_id,
        }
    }

    /// The resolved target governed by the action's predicates.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match &self.facts {
            ActionFacts::CreateLink { target_path, .. }
            | ActionFacts::RemoveLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => target_path,
            ActionFacts::ReplaceLink { target_path, .. } => target_path,
            ActionFacts::ReplaceOwnership { target_path, .. } => target_path,
            ActionFacts::RelocateLink {
                new_target_path, ..
            } => new_target_path,
            ActionFacts::Copy { target_path, .. } => target_path,
            ActionFacts::ReplaceEffect { final_effect, .. } => final_effect.target_path(),
        }
    }

    /// The exact recorded condition that held before mutation began.
    pub(crate) fn precondition(&self) -> TargetCondition {
        match &self.facts {
            ActionFacts::RemoveLink {
                target_path,
                link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: link_target.clone(),
            },
            ActionFacts::ReplaceLink {
                target_path,
                old_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: old_link_target.clone(),
            },
            ActionFacts::ReplaceOwnership {
                target_path,
                old_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: old_link_target.clone(),
            },
            ActionFacts::CreateLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => TargetCondition::Missing {
                target_path: target_path.clone(),
            },
            ActionFacts::RelocateLink {
                old_target_path,
                old_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: old_target_path.clone(),
                link_target: old_link_target.clone(),
            },
            ActionFacts::Copy { precondition, .. } => precondition.clone(),
            ActionFacts::ReplaceEffect { precondition, .. } => precondition.clone(),
        }
    }

    /// The exact condition required before Known state may change.
    pub(crate) fn postcondition(&self) -> TargetCondition {
        match &self.facts {
            ActionFacts::CreateLink {
                target_path,
                link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: link_target.clone(),
            },
            ActionFacts::ReplaceLink {
                target_path,
                new_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: new_link_target.clone(),
            },
            ActionFacts::ReplaceOwnership {
                target_path,
                new_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: target_path.clone(),
                link_target: new_link_target.clone(),
            },
            ActionFacts::RemoveLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => TargetCondition::Missing {
                target_path: target_path.clone(),
            },
            ActionFacts::RelocateLink {
                new_target_path,
                new_link_target,
                ..
            } => TargetCondition::ExpectedLink {
                target_path: new_target_path.clone(),
                link_target: new_link_target.clone(),
            },
            ActionFacts::Copy { postcondition, .. } => postcondition.clone(),
            ActionFacts::ReplaceEffect { postcondition, .. } => postcondition.clone(),
        }
    }

    /// The durable progress status.
    pub(crate) fn status(&self) -> ActionStatus {
        self.status
    }

    /// Reconstructs the exact Known-state transition that must be atomic with `succeeded`.
    pub(crate) fn known_state_update_after_success(
        &self,
    ) -> Result<RecordedKnownStateUpdate, OperationRecordError> {
        match &self.facts {
            ActionFacts::CreateLink {
                resource_id,
                target_path,
                link_target,
            } => KnownFileLink::new(
                resource_id.clone(),
                link_target.as_path().clone(),
                target_path.clone(),
                link_target.clone(),
            )
            .map(RecordedKnownStateUpdate::Upsert)
            .map_err(OperationRecordError::InvalidKnownFileLink),
            ActionFacts::RemoveLink {
                resource_id,
                target_path,
                link_target,
            } => KnownFileLink::new(
                resource_id.clone(),
                link_target.as_path().clone(),
                target_path.clone(),
                link_target.clone(),
            )
            .map(RecordedKnownStateUpdate::RemoveExpected)
            .map_err(OperationRecordError::InvalidKnownFileLink),
            ActionFacts::ForgetMissing { resource_id, .. } => {
                Ok(RecordedKnownStateUpdate::RemoveMissing {
                    resource_id: resource_id.clone(),
                })
            }
            ActionFacts::ReplaceLink {
                resource_id,
                target_path,
                new_link_target,
                ..
            } => KnownFileLink::new(
                resource_id.clone(),
                new_link_target.as_path().clone(),
                target_path.clone(),
                new_link_target.clone(),
            )
            .map(RecordedKnownStateUpdate::Upsert)
            .map_err(OperationRecordError::InvalidKnownFileLink),
            ActionFacts::ReplaceOwnership {
                old_resource_id,
                new_resource_id,
                target_path,
                old_link_target,
                new_link_target,
                ..
            } => {
                let old_resource = KnownFileLink::new(
                    old_resource_id.clone(),
                    old_link_target.as_path().clone(),
                    target_path.clone(),
                    old_link_target.clone(),
                )
                .map_err(OperationRecordError::InvalidKnownFileLink)?;
                let new_resource = KnownFileLink::new(
                    new_resource_id.clone(),
                    new_link_target.as_path().clone(),
                    target_path.clone(),
                    new_link_target.clone(),
                )
                .map_err(OperationRecordError::InvalidKnownFileLink)?;
                Ok(RecordedKnownStateUpdate::ReplaceIdentity {
                    old_resource,
                    new_resource,
                })
            }
            ActionFacts::RelocateLink {
                resource_id,
                new_target_path,
                new_link_target,
                ..
            } => KnownFileLink::new(
                resource_id.clone(),
                new_link_target.as_path().clone(),
                new_target_path.clone(),
                new_link_target.clone(),
            )
            .map(RecordedKnownStateUpdate::Upsert)
            .map_err(OperationRecordError::InvalidKnownFileLink),
            ActionFacts::Copy {
                resource_id,
                source_path,
                target_path,
                content_fingerprint,
                ..
            } => crate::domain::known::KnownFileCopy::new(
                resource_id.clone(),
                source_path.clone(),
                target_path.clone(),
                content_fingerprint.clone(),
            )
            .map(RecordedKnownStateUpdate::UpsertCopy)
            .map_err(OperationRecordError::InvalidKnownFileCopy),
            ActionFacts::ReplaceEffect { final_effect, .. } => match final_effect.as_ref() {
                KnownResource::FileLink(resource) => {
                    Ok(RecordedKnownStateUpdate::Upsert(resource.clone()))
                }
                KnownResource::FileCopy(resource) => {
                    Ok(RecordedKnownStateUpdate::UpsertCopy(resource.clone()))
                }
            },
        }
    }

    /// Replacement-specific facts are intentionally inaccessible for other actions.
    pub(crate) fn replacement_facts(&self) -> Option<ReplacementFacts> {
        match &self.facts {
            ActionFacts::ReplaceLink {
                target_path,
                old_link_target,
                new_link_target,
                temporary_path,
                ..
            } => Some(ReplacementFacts {
                target_path: target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
                temporary_path: temporary_path.clone(),
            }),
            ActionFacts::ReplaceOwnership {
                target_path,
                old_link_target,
                new_link_target,
                temporary_path: Some(temporary_path),
                ..
            } => Some(ReplacementFacts {
                target_path: target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
                temporary_path: temporary_path.clone(),
            }),
            _ => None,
        }
    }

    /// Copy-specific ownership facts retained for typed persistence and later recovery.
    pub(crate) fn copy_facts(&self) -> Option<CopyFacts> {
        match &self.facts {
            ActionFacts::Copy {
                source_path,
                target_path,
                content_fingerprint,
                temporary_path,
                old_effect,
                final_effect,
                ..
            } => Some(CopyFacts {
                source_path: source_path.clone(),
                target_path: target_path.clone(),
                content_fingerprint: content_fingerprint.clone(),
                temporary_path: temporary_path.clone(),
                old_effect: (**old_effect).clone(),
                final_effect: (**final_effect).clone(),
            }),
            _ => None,
        }
    }

    pub(crate) fn effect_handoff_facts(&self) -> Option<EffectHandoffFacts> {
        match &self.facts {
            ActionFacts::ReplaceEffect {
                old_effect,
                final_effect,
                temporary_path,
                ..
            } => Some(EffectHandoffFacts {
                old_effect: (**old_effect).clone(),
                final_effect: (**final_effect).clone(),
                temporary_path: temporary_path.clone(),
            }),
            _ => None,
        }
    }

    /// Returns the exact action-local temporary retained for a replacement or copy materialization.
    pub(crate) fn temporary_path(&self) -> Option<&ResolvedPath> {
        match &self.facts {
            ActionFacts::ReplaceLink { temporary_path, .. }
            | ActionFacts::Copy { temporary_path, .. }
            | ActionFacts::ReplaceEffect { temporary_path, .. } => Some(temporary_path),
            ActionFacts::ReplaceOwnership {
                temporary_path: Some(temporary_path),
                ..
            } => Some(temporary_path),
            ActionFacts::CreateLink { .. }
            | ActionFacts::RemoveLink { .. }
            | ActionFacts::ForgetMissing { .. }
            | ActionFacts::ReplaceOwnership {
                temporary_path: None,
                ..
            }
            | ActionFacts::RelocateLink { .. } => None,
        }
    }

    pub(crate) fn replaced_resource_id(&self) -> Option<&FullyQualifiedResourceId> {
        match &self.facts {
            ActionFacts::ReplaceOwnership {
                old_resource_id, ..
            } => Some(old_resource_id),
            _ => None,
        }
    }

    /// Relocation-specific facts are inaccessible to other action kinds.
    pub(crate) fn relocation_facts(&self) -> Option<RelocationFacts> {
        match &self.facts {
            ActionFacts::RelocateLink {
                old_target_path,
                new_target_path,
                old_link_target,
                new_link_target,
                ..
            } => Some(RelocationFacts {
                old_target_path: old_target_path.clone(),
                new_target_path: new_target_path.clone(),
                old_link_target: old_link_target.clone(),
                new_link_target: new_link_target.clone(),
            }),
            _ => None,
        }
    }

    fn touched_target_paths(&self) -> Vec<&ResolvedPath> {
        match &self.facts {
            ActionFacts::RelocateLink {
                old_target_path,
                new_target_path,
                ..
            } => vec![old_target_path, new_target_path],
            ActionFacts::Copy {
                target_path,
                old_effect,
                kind: ActionKind::RelocateCopy,
                ..
            } => match old_effect.as_ref() {
                Some(KnownResource::FileCopy(old_copy)) => {
                    vec![old_copy.target_path(), target_path]
                }
                _ => vec![target_path],
            },
            ActionFacts::Copy { target_path, .. } => vec![target_path],
            ActionFacts::ReplaceEffect { final_effect, .. } => vec![final_effect.target_path()],
            _ => vec![self.target_path()],
        }
    }

    fn mark_running(&mut self) -> Result<(), OperationRecordError> {
        if self.status != ActionStatus::Pending {
            return Err(OperationRecordError::InvalidStatusTransition {
                from: self.status,
                to: ActionStatus::Running,
            });
        }
        self.status = ActionStatus::Running;
        Ok(())
    }

    fn mark_without_known(&mut self, status: ActionStatus) -> Result<(), OperationRecordError> {
        let permitted = match status {
            ActionStatus::Failed | ActionStatus::Uncertain => {
                matches!(self.status, ActionStatus::Running | ActionStatus::Uncertain)
            }
            ActionStatus::Skipped => self.status == ActionStatus::Pending,
            ActionStatus::Pending | ActionStatus::Running | ActionStatus::Succeeded => false,
        };
        if !permitted {
            return Err(OperationRecordError::InvalidStatusTransition {
                from: self.status,
                to: status,
            });
        }
        self.status = status;
        Ok(())
    }

    fn mark_succeeded(&mut self) -> Result<RecordedKnownStateUpdate, OperationRecordError> {
        if !matches!(self.status, ActionStatus::Running | ActionStatus::Uncertain) {
            return Err(OperationRecordError::InvalidStatusTransition {
                from: self.status,
                to: ActionStatus::Succeeded,
            });
        }
        let update = self.known_state_update_after_success()?;
        self.status = ActionStatus::Succeeded;
        Ok(update)
    }
}

/// The facts an executor and recovery observer need for a recorded replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplacementFacts {
    target_path: ResolvedPath,
    old_link_target: LinkTarget,
    new_link_target: LinkTarget,
    temporary_path: ResolvedPath,
}

/// The final copy ownership facts selected by a typed copy action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CopyFacts {
    source_path: ResolvedPath,
    target_path: ResolvedPath,
    content_fingerprint: ContentFingerprint,
    temporary_path: ResolvedPath,
    old_effect: Option<KnownResource>,
    final_effect: KnownResource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EffectHandoffFacts {
    old_effect: KnownResource,
    final_effect: KnownResource,
    temporary_path: ResolvedPath,
}

impl EffectHandoffFacts {
    pub(crate) fn old_effect(&self) -> &KnownResource {
        &self.old_effect
    }
    pub(crate) fn final_effect(&self) -> &KnownResource {
        &self.final_effect
    }
    pub(crate) fn temporary_path(&self) -> &ResolvedPath {
        &self.temporary_path
    }
}

impl CopyFacts {
    pub(crate) fn source_path(&self) -> &ResolvedPath {
        &self.source_path
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn content_fingerprint(&self) -> &ContentFingerprint {
        &self.content_fingerprint
    }
    pub(crate) fn temporary_path(&self) -> &ResolvedPath {
        &self.temporary_path
    }
    pub(crate) fn old_effect(&self) -> Option<&KnownResource> {
        self.old_effect.as_ref()
    }
    pub(crate) fn final_effect(&self) -> &KnownResource {
        &self.final_effect
    }
}

fn precondition_link_target(
    condition: &TargetCondition,
) -> Result<&LinkTarget, OperationRecordError> {
    match condition {
        TargetCondition::ExpectedLink { link_target, .. } => Ok(link_target),
        TargetCondition::Missing { .. } | TargetCondition::ExpectedCopy { .. } => {
            Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceLink,
            })
        }
    }
}

fn condition_matches_effect(condition: &TargetCondition, effect: &KnownResource) -> bool {
    match (condition, effect) {
        (
            TargetCondition::ExpectedLink {
                target_path,
                link_target,
            },
            KnownResource::FileLink(effect),
        ) => target_path == effect.target_path() && link_target == effect.link_target(),
        (
            TargetCondition::ExpectedCopy {
                target_path,
                content_fingerprint,
            },
            KnownResource::FileCopy(effect),
        ) => {
            target_path == effect.target_path()
                && content_fingerprint == effect.content_fingerprint()
        }
        _ => false,
    }
}

fn condition_for_known_effect(effect: &KnownResource) -> TargetCondition {
    match effect {
        KnownResource::FileLink(resource) => TargetCondition::ExpectedLink {
            target_path: resource.target_path().clone(),
            link_target: resource.link_target().clone(),
        },
        KnownResource::FileCopy(resource) => TargetCondition::ExpectedCopy {
            target_path: resource.target_path().clone(),
            content_fingerprint: resource.content_fingerprint().clone(),
        },
    }
}

fn postcondition_link_target(
    condition: &TargetCondition,
) -> Result<&LinkTarget, OperationRecordError> {
    precondition_link_target(condition)
}

impl ReplacementFacts {
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn old_link_target(&self) -> &LinkTarget {
        &self.old_link_target
    }
    pub(crate) fn new_link_target(&self) -> &LinkTarget {
        &self.new_link_target
    }
    pub(crate) fn temporary_path(&self) -> &ResolvedPath {
        &self.temporary_path
    }
}

/// The facts an executor and recovery observer need for a recorded relocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelocationFacts {
    old_target_path: ResolvedPath,
    new_target_path: ResolvedPath,
    old_link_target: LinkTarget,
    new_link_target: LinkTarget,
}

impl RelocationFacts {
    pub(crate) fn new_for_executor(
        old_target_path: ResolvedPath,
        new_target_path: ResolvedPath,
        old_link_target: LinkTarget,
        new_link_target: LinkTarget,
    ) -> Self {
        Self {
            old_target_path,
            new_target_path,
            old_link_target,
            new_link_target,
        }
    }

    pub(crate) fn old_target_path(&self) -> &ResolvedPath {
        &self.old_target_path
    }

    pub(crate) fn new_target_path(&self) -> &ResolvedPath {
        &self.new_target_path
    }

    pub(crate) fn old_link_target(&self) -> &LinkTarget {
        &self.old_link_target
    }

    pub(crate) fn new_link_target(&self) -> &LinkTarget {
        &self.new_link_target
    }
}

/// The complete active operation, including every action's durable progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationRecord {
    id: OperationId,
    desired_hash: DesiredHash,
    actions: BTreeMap<ActionId, RecordedAction>,
}

impl OperationRecord {
    /// Creates a new single-action operation for an action supported by the current vertical slice.
    pub(crate) fn new_single_action(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        let action_id = ActionId::parse("a1")?;
        let recorded_action = RecordedAction::from_action(action)?;
        let record = Self::from_actions(id, desired_hash, [(action_id.clone(), recorded_action)])?;
        Ok((record, action_id))
    }

    /// Creates a replacement record after the repository has chosen its action-local sibling name.
    pub(crate) fn new_replace_link(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
        temporary_path: ResolvedPath,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        let action_id = ActionId::parse("a1")?;
        let recorded_action = RecordedAction::replace_link(action, temporary_path)?;
        let record = Self::from_actions(id, desired_hash, [(action_id.clone(), recorded_action)])?;
        Ok((record, action_id))
    }

    pub(crate) fn new_replace_ownership(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
        temporary_path: Option<ResolvedPath>,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        let action_id = ActionId::parse("a1")?;
        let recorded_action = RecordedAction::replace_ownership(action, temporary_path)?;
        let record = Self::from_actions(id, desired_hash, [(action_id.clone(), recorded_action)])?;
        Ok((record, action_id))
    }

    pub(crate) fn new_relocate_link(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        let action_id = ActionId::parse("a1")?;
        let recorded_action = RecordedAction::relocate_link(action)?;
        let record = Self::from_actions(id, desired_hash, [(action_id.clone(), recorded_action)])?;
        Ok((record, action_id))
    }

    /// Compatibility constructor for Slice 4's create-only tests and caller.
    pub(crate) fn new_create_link(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        Self::new_single_action(id, desired_hash, action)
    }

    /// Reconstructs an active operation from validated persisted actions.
    pub(crate) fn from_actions(
        id: OperationId,
        desired_hash: DesiredHash,
        actions: impl IntoIterator<Item = (ActionId, RecordedAction)>,
    ) -> Result<Self, OperationRecordError> {
        let actions = actions.into_iter().collect::<BTreeMap<_, _>>();
        if actions.is_empty() {
            return Err(OperationRecordError::NoActions);
        }

        let mut resource_ids = BTreeSet::new();
        let mut target_paths = BTreeSet::new();
        for action in actions.values() {
            if !resource_ids.insert(action.resource_id().clone()) {
                return Err(OperationRecordError::DuplicateResourceId {
                    resource_id: action.resource_id().clone(),
                });
            }
            for target_path in action.touched_target_paths() {
                if !target_paths.insert(target_path.clone()) {
                    return Err(OperationRecordError::DuplicateTargetPath {
                        target_path: target_path.clone(),
                    });
                }
            }
        }

        // Temporary siblings are action-local and may never alias another recorded target or another replacement's temporary entry.
        for action in actions.values() {
            if let Some(facts) = action.replacement_facts() {
                if !target_paths.insert(facts.temporary_path().clone()) {
                    return Err(OperationRecordError::DuplicateTargetPath {
                        target_path: facts.temporary_path().clone(),
                    });
                }
            }
        }

        Ok(Self {
            id,
            desired_hash,
            actions,
        })
    }

    /// The opaque operation identifier.
    pub(crate) fn id(&self) -> &OperationId {
        &self.id
    }

    /// The canonical Desired hash that produced the original plan.
    pub(crate) fn desired_hash(&self) -> &DesiredHash {
        &self.desired_hash
    }

    /// Actions in stable opaque-ID order.
    pub(crate) fn actions(&self) -> impl ExactSizeIterator<Item = (&ActionId, &RecordedAction)> {
        self.actions.iter()
    }

    /// Looks up one recorded action.
    pub(crate) fn action(&self, action_id: &ActionId) -> Option<&RecordedAction> {
        self.actions.get(action_id)
    }

    /// Transitions a persisted action from `pending` to `running` before mutation.
    pub(crate) fn mark_running(
        &mut self,
        action_id: &ActionId,
    ) -> Result<(), OperationRecordError> {
        self.action_mut(action_id)?.mark_running()
    }

    /// Records a conclusive failure or uncertainty without changing Known state.
    pub(crate) fn mark_without_known(
        &mut self,
        action_id: &ActionId,
        status: ActionStatus,
    ) -> Result<(), OperationRecordError> {
        self.action_mut(action_id)?.mark_without_known(status)
    }

    /// Marks a verified action successful and returns its atomic Known-state update.
    pub(crate) fn mark_succeeded(
        &mut self,
        action_id: &ActionId,
    ) -> Result<RecordedKnownStateUpdate, OperationRecordError> {
        self.action_mut(action_id)?.mark_succeeded()
    }

    /// Compatibility transition for Slice 4's create-only caller.
    pub(crate) fn mark_create_succeeded(
        &mut self,
        action_id: &ActionId,
    ) -> Result<KnownFileLink, OperationRecordError> {
        match self.mark_succeeded(action_id)? {
            RecordedKnownStateUpdate::Upsert(known) => Ok(known),
            RecordedKnownStateUpdate::UpsertCopy(_)
            | RecordedKnownStateUpdate::RemoveExpected(_)
            | RecordedKnownStateUpdate::RemoveMissing { .. }
            | RecordedKnownStateUpdate::ReplaceIdentity { .. } => {
                Err(OperationRecordError::UnsupportedActionKind {
                    kind: self
                        .action(action_id)
                        .expect("a completed action must remain recorded")
                        .kind(),
                })
            }
        }
    }

    /// Whether every action has a closeable final status.
    pub(crate) fn can_close(&self) -> bool {
        self.actions
            .values()
            .all(|action| action.status.closes_operation())
    }

    fn action_mut(
        &mut self,
        action_id: &ActionId,
    ) -> Result<&mut RecordedAction, OperationRecordError> {
        self.actions
            .get_mut(action_id)
            .ok_or_else(|| OperationRecordError::UnknownActionId {
                action_id: action_id.clone(),
            })
    }
}

/// The reason an operation record cannot safely represent recovery facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationRecordError {
    ReplacementTemporaryRequired,
    EmptyOperationId,
    EmptyActionId,
    NoActions,
    UnsupportedActionKind {
        kind: ActionKind,
    },
    InvalidActionConditions {
        kind: ActionKind,
    },
    DuplicateResourceId {
        resource_id: FullyQualifiedResourceId,
    },
    DuplicateTargetPath {
        target_path: ResolvedPath,
    },
    UnknownActionId {
        action_id: ActionId,
    },
    InvalidStatusTransition {
        from: ActionStatus,
        to: ActionStatus,
    },
    InvalidKnownFileLink(KnownFileLinkError),
    InvalidKnownFileCopy(crate::domain::known::KnownFileCopyError),
}

impl fmt::Display for OperationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReplacementTemporaryRequired => formatter.write_str(
                "a replacement action requires a repository-allocated temporary sibling",
            ),
            Self::EmptyOperationId => formatter.write_str("operation ID must not be empty"),
            Self::EmptyActionId => formatter.write_str("action ID must not be empty"),
            Self::NoActions => formatter.write_str("an operation record must contain an action"),
            Self::UnsupportedActionKind { kind } => {
                write!(formatter, "this slice cannot record action kind {kind:?}")
            }
            Self::InvalidActionConditions { kind } => write!(
                formatter,
                "a {kind:?} record has invalid target preconditions or postconditions"
            ),
            Self::DuplicateResourceId { resource_id } => {
                write!(
                    formatter,
                    "operation records resource {resource_id} more than once"
                )
            }
            Self::DuplicateTargetPath { target_path } => {
                write!(
                    formatter,
                    "operation records target {target_path} more than once"
                )
            }
            Self::UnknownActionId { action_id } => {
                write!(
                    formatter,
                    "operation does not contain action {}",
                    action_id.as_str()
                )
            }
            Self::InvalidStatusTransition { from, to } => {
                write!(
                    formatter,
                    "invalid action-status transition: {from:?} -> {to:?}"
                )
            }
            Self::InvalidKnownFileLink(error) => error.fmt(formatter),
            Self::InvalidKnownFileCopy(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for OperationRecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidKnownFileLink(error) => Some(error),
            Self::InvalidKnownFileCopy(error) => Some(error),
            Self::EmptyOperationId
            | Self::ReplacementTemporaryRequired
            | Self::EmptyActionId
            | Self::NoActions
            | Self::UnsupportedActionKind { .. }
            | Self::InvalidActionConditions { .. }
            | Self::DuplicateResourceId { .. }
            | Self::DuplicateTargetPath { .. }
            | Self::UnknownActionId { .. }
            | Self::InvalidStatusTransition { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::desired::ResolvedResource;
    use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::paths::ResolvedPath;

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-operation-test")
                .join(name),
        )
        .unwrap()
    }

    fn create_action() -> PlannedAction {
        PlannedAction::create_link(
            ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/git").unwrap(),
                path("store/git/config"),
                path("home/.gitconfig"),
            )
            .unwrap(),
        )
    }

    fn desired_hash() -> DesiredHash {
        DesiredHash::parse(format!("sha256:{}", "a".repeat(64))).unwrap()
    }

    fn fingerprint() -> ContentFingerprint {
        ContentFingerprint::parse(format!("sha256:{}", "b".repeat(64))).unwrap()
    }

    #[test]
    fn replace_copy_requires_the_exact_old_copy_effect() {
        let resource_id = FullyQualifiedResourceId::parse("base/git").unwrap();
        let source = path("store/git/config");
        let target = path("home/.gitconfig");
        let old = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            path("store/git/old"),
            target.clone(),
            fingerprint(),
        )
        .unwrap();
        let final_copy = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            source.clone(),
            target.clone(),
            fingerprint(),
        )
        .unwrap();
        let facts = PersistedCopyActionFacts {
            kind: ActionKind::ReplaceCopy,
            resource_id,
            source_path: source,
            target_path: target.clone(),
            content_fingerprint: fingerprint(),
            temporary_path: path("home/.loadout-copy-a1"),
            old_effect: Some(old.into()),
            final_effect: final_copy.into(),
            precondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: fingerprint(),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: target,
                content_fingerprint: fingerprint(),
            },
            status: ActionStatus::Pending,
        };
        assert!(RecordedAction::from_persisted_copy(facts.clone()).is_ok());

        assert!(matches!(
            RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
                old_effect: None,
                ..facts
            }),
            Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceCopy
            })
        ));
    }

    #[test]
    fn relocate_copy_requires_distinct_old_and_new_owned_targets() {
        let resource_id = FullyQualifiedResourceId::parse("base/git").unwrap();
        let old_target = path("home/.gitconfig");
        let new_target = path("home/.config/git/config");
        let old = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            path("store/git/old"),
            old_target.clone(),
            fingerprint(),
        )
        .unwrap();
        let final_copy = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            path("store/git/config"),
            new_target.clone(),
            fingerprint(),
        )
        .unwrap();
        let facts = PersistedCopyActionFacts {
            kind: ActionKind::RelocateCopy,
            resource_id,
            source_path: path("store/git/config"),
            target_path: new_target.clone(),
            content_fingerprint: fingerprint(),
            temporary_path: path("home/.config/git/.loadout-copy-a1"),
            old_effect: Some(old.clone().into()),
            final_effect: final_copy.into(),
            precondition: TargetCondition::ExpectedCopy {
                target_path: old_target,
                content_fingerprint: fingerprint(),
            },
            postcondition: TargetCondition::ExpectedCopy {
                target_path: new_target,
                content_fingerprint: fingerprint(),
            },
            status: ActionStatus::Pending,
        };
        let action = RecordedAction::from_persisted_copy(facts.clone()).unwrap();
        assert_eq!(action.kind(), ActionKind::RelocateCopy);
        assert_eq!(
            action.copy_facts().unwrap().old_effect(),
            Some(&KnownResource::from(old.clone()))
        );

        assert!(matches!(
            RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
                target_path: old.target_path().clone(),
                ..facts
            }),
            Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::RelocateCopy
            })
        ));
    }

    #[test]
    fn replace_effect_accepts_a_copy_to_link_handoff_with_typed_effects() {
        let resource_id = FullyQualifiedResourceId::parse("base/git").unwrap();
        let target = path("home/.gitconfig");
        let old = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            path("store/git/config"),
            target.clone(),
            fingerprint(),
        )
        .unwrap();
        let final_link = KnownFileLink::new(
            resource_id.clone(),
            path("store/git/next"),
            target.clone(),
            LinkTarget::new(path("store/git/next")),
        )
        .unwrap();
        let action = RecordedAction::from_persisted_replace_effect(PersistedEffectHandoffFacts {
            resource_id,
            old_effect: old.into(),
            final_effect: final_link.into(),
            temporary_path: path("home/.loadout-effect-a1"),
            precondition: TargetCondition::ExpectedCopy {
                target_path: target.clone(),
                content_fingerprint: fingerprint(),
            },
            postcondition: TargetCondition::ExpectedLink {
                target_path: target,
                link_target: LinkTarget::new(path("store/git/next")),
            },
            status: ActionStatus::Pending,
        })
        .unwrap();
        assert_eq!(action.kind(), ActionKind::ReplaceEffect);
        assert!(matches!(
            action.known_state_update_after_success(),
            Ok(RecordedKnownStateUpdate::Upsert(_))
        ));
    }

    #[test]
    fn replace_effect_requires_a_temporary_sibling_of_its_target() {
        let resource_id = FullyQualifiedResourceId::parse("base/git").unwrap();
        let target = path("home/.gitconfig");
        let old = crate::domain::known::KnownFileCopy::new(
            resource_id.clone(),
            path("store/git/config"),
            target.clone(),
            fingerprint(),
        )
        .unwrap();
        let final_link = KnownFileLink::new(
            resource_id.clone(),
            path("store/git/next"),
            target.clone(),
            LinkTarget::new(path("store/git/next")),
        )
        .unwrap();

        assert!(matches!(
            RecordedAction::from_persisted_replace_effect(PersistedEffectHandoffFacts {
                resource_id,
                old_effect: old.into(),
                final_effect: final_link.into(),
                temporary_path: path("home/other/.loadout-effect-a1"),
                precondition: TargetCondition::ExpectedCopy {
                    target_path: target.clone(),
                    content_fingerprint: fingerprint(),
                },
                postcondition: TargetCondition::ExpectedLink {
                    target_path: target,
                    link_target: LinkTarget::new(path("store/git/next")),
                },
                status: ActionStatus::Pending,
            }),
            Err(OperationRecordError::InvalidActionConditions {
                kind: ActionKind::ReplaceEffect
            })
        ));
    }

    #[test]
    fn planner_copy_and_handoff_actions_become_pending_typed_records() {
        let resource_id = FullyQualifiedResourceId::parse("base/git").unwrap();
        let source = path("store/git/config");
        let target = path("home/.gitconfig");
        let copy = ResolvedFileCopy::new(
            resource_id.clone(),
            source.clone(),
            target.clone(),
            fingerprint(),
        )
        .unwrap();
        let create = PlannedFileCopyAction::Create {
            desired: copy.clone(),
        };
        let recorded_copy = RecordedAction::copy(&create, path("home/.loadout-copy-a1")).unwrap();
        assert_eq!(recorded_copy.kind(), ActionKind::CreateCopy);
        assert_eq!(recorded_copy.status(), ActionStatus::Pending);
        assert_eq!(
            recorded_copy.copy_facts().unwrap().final_effect(),
            &KnownResource::from(crate::domain::known::KnownFileCopy::from_resolved(&copy))
        );

        let final_link = ResolvedFileLink::new(resource_id, source, target).unwrap();
        let handoff = PlannedEffectHandoff::new(
            KnownResource::from(crate::domain::known::KnownFileCopy::from_resolved(&copy)),
            ResolvedResource::from(final_link.clone()),
        )
        .unwrap();
        let recorded_handoff =
            RecordedAction::replace_effect(&handoff, path("home/.loadout-effect-a1")).unwrap();
        assert_eq!(recorded_handoff.kind(), ActionKind::ReplaceEffect);
        assert_eq!(recorded_handoff.status(), ActionStatus::Pending);
        assert_eq!(
            recorded_handoff
                .effect_handoff_facts()
                .unwrap()
                .final_effect(),
            &KnownResource::from(KnownFileLink::from_resolved(&final_link))
        );
    }

    #[test]
    fn persisted_conditions_must_describe_the_recorded_target() {
        let action = RecordedAction::from_action(&create_action()).unwrap();
        for (precondition, postcondition) in [
            (
                TargetCondition::Missing {
                    target_path: path("home/other"),
                },
                action.postcondition(),
            ),
            (
                action.precondition(),
                TargetCondition::ExpectedLink {
                    target_path: path("home/other"),
                    link_target: LinkTarget::new(path("store/git/config")),
                },
            ),
        ] {
            assert!(matches!(
                RecordedAction::from_persisted(
                    action.kind(),
                    action.resource_id().clone(),
                    action.target_path().clone(),
                    precondition,
                    postcondition,
                    ActionStatus::Pending,
                ),
                Err(OperationRecordError::InvalidActionConditions {
                    kind: ActionKind::CreateLink,
                })
            ));
        }
    }

    #[test]
    fn create_record_requires_running_before_succeeded_and_derives_its_known_fact() {
        let (mut operation, action_id) = OperationRecord::new_create_link(
            OperationId::parse("op-1").unwrap(),
            desired_hash(),
            &create_action(),
        )
        .unwrap();

        assert!(matches!(
            operation.mark_create_succeeded(&action_id),
            Err(OperationRecordError::InvalidStatusTransition {
                from: ActionStatus::Pending,
                to: ActionStatus::Succeeded,
            })
        ));
        operation.mark_running(&action_id).unwrap();
        let known = operation.mark_create_succeeded(&action_id).unwrap();

        assert_eq!(known.resource_id().as_str(), "base/git");
        assert_eq!(known.source_path(), known.link_target().as_path());
        assert!(operation.can_close());
    }

    #[test]
    fn uncertain_actions_keep_the_operation_open() {
        let (mut operation, action_id) = OperationRecord::new_create_link(
            OperationId::parse("op-1").unwrap(),
            desired_hash(),
            &create_action(),
        )
        .unwrap();

        operation.mark_running(&action_id).unwrap();
        operation
            .mark_without_known(&action_id, ActionStatus::Uncertain)
            .unwrap();

        assert_eq!(
            operation.action(&action_id).unwrap().status(),
            ActionStatus::Uncertain
        );
        assert!(!operation.can_close());
    }
}
