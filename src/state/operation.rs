//! Typed operation progress recorded before and after resource execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::domain::file_link::LinkTarget;
use crate::domain::hashes::DesiredHash;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{KnownFileLink, KnownFileLinkError};
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, PlannedAction, TargetCondition};

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
    RemoveExpected(KnownFileLink),
    RemoveMissing {
        resource_id: FullyQualifiedResourceId,
    },
}

/// One action whose recorded facts are sufficient for future recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedAction {
    facts: ActionFacts,
    status: ActionStatus,
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
}

impl RecordedAction {
    /// Builds a record for one planner-selected action supported by the current vertical slice.
    pub(crate) fn from_action(action: &PlannedAction) -> Result<Self, OperationRecordError> {
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

    /// The planned action kind represented by this persisted record.
    pub(crate) fn kind(&self) -> ActionKind {
        match &self.facts {
            ActionFacts::CreateLink { .. } => ActionKind::CreateLink,
            ActionFacts::RemoveLink { .. } => ActionKind::RemoveLink,
            ActionFacts::ForgetMissing { .. } => ActionKind::ForgetMissing,
        }
    }

    /// The stable resource identity affected by this action.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match &self.facts {
            ActionFacts::CreateLink { resource_id, .. }
            | ActionFacts::RemoveLink { resource_id, .. }
            | ActionFacts::ForgetMissing { resource_id, .. } => resource_id,
        }
    }

    /// The resolved target governed by the action's predicates.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match &self.facts {
            ActionFacts::CreateLink { target_path, .. }
            | ActionFacts::RemoveLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => target_path,
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
            ActionFacts::CreateLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => TargetCondition::Missing {
                target_path: target_path.clone(),
            },
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
            ActionFacts::RemoveLink { target_path, .. }
            | ActionFacts::ForgetMissing { target_path, .. } => TargetCondition::Missing {
                target_path: target_path.clone(),
            },
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
            ActionStatus::Failed | ActionStatus::Uncertain => self.status == ActionStatus::Running,
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
        if self.status != ActionStatus::Running {
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
            if !target_paths.insert(action.target_path().clone()) {
                return Err(OperationRecordError::DuplicateTargetPath {
                    target_path: action.target_path().clone(),
                });
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
            RecordedKnownStateUpdate::RemoveExpected(_)
            | RecordedKnownStateUpdate::RemoveMissing { .. } => {
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
}

impl fmt::Display for OperationRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        }
    }
}

impl std::error::Error for OperationRecordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidKnownFileLink(error) => Some(error),
            Self::EmptyOperationId
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
