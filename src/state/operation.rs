//! Typed operation progress recorded before and after resource execution.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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

/// One action whose recorded facts are sufficient for future recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedAction {
    kind: ActionKind,
    resource_id: FullyQualifiedResourceId,
    target_path: ResolvedPath,
    precondition: TargetCondition,
    postcondition: TargetCondition,
    status: ActionStatus,
}

impl RecordedAction {
    /// Builds the Slice 4 record for one planner-selected `create_link` action.
    pub(crate) fn from_create_link(action: &PlannedAction) -> Result<Self, OperationRecordError> {
        if action.kind() != ActionKind::CreateLink {
            return Err(OperationRecordError::UnsupportedActionKind {
                kind: action.kind(),
            });
        }

        let preconditions = action.preconditions();
        let postconditions = action.postconditions();
        let [
            TargetCondition::Missing {
                target_path: pre_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidCreateConditions);
        };
        let [
            TargetCondition::ExpectedLink {
                target_path: post_target,
                link_target: _,
            },
        ] = postconditions.as_slice()
        else {
            return Err(OperationRecordError::InvalidCreateConditions);
        };
        if pre_target != post_target {
            return Err(OperationRecordError::InvalidCreateConditions);
        }

        Ok(Self {
            kind: ActionKind::CreateLink,
            resource_id: action.resource_id().clone(),
            target_path: pre_target.clone(),
            precondition: preconditions
                .into_iter()
                .next()
                .expect("a checked create precondition must exist"),
            postcondition: postconditions
                .into_iter()
                .next()
                .expect("a checked create postcondition must exist"),
            status: ActionStatus::Pending,
        })
    }

    /// Reconstructs a persisted `create_link` record after validating its facts.
    pub(crate) fn from_persisted_create_link(
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        precondition: TargetCondition,
        postcondition: TargetCondition,
        status: ActionStatus,
    ) -> Result<Self, OperationRecordError> {
        let record = Self {
            kind: ActionKind::CreateLink,
            resource_id,
            target_path,
            precondition,
            postcondition,
            status,
        };
        record.validate_create_conditions()?;
        Ok(record)
    }

    /// The planned action kind represented by this persisted record.
    pub(crate) fn kind(&self) -> ActionKind {
        self.kind
    }

    /// The stable resource identity affected by this action.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        &self.resource_id
    }

    /// The resolved target governed by the action's predicates.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }

    /// The exact recorded condition that held before mutation began.
    pub(crate) fn precondition(&self) -> &TargetCondition {
        &self.precondition
    }

    /// The exact condition required before Known state may change.
    pub(crate) fn postcondition(&self) -> &TargetCondition {
        &self.postcondition
    }

    /// The durable progress status.
    pub(crate) fn status(&self) -> ActionStatus {
        self.status
    }

    /// Reconstructs the Known fact that a successful create action must have committed atomically with its `succeeded` status.
    pub(crate) fn known_after_success(&self) -> Result<KnownFileLink, OperationRecordError> {
        self.known_after_create_success()
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

    fn mark_create_succeeded(&mut self) -> Result<KnownFileLink, OperationRecordError> {
        if self.status != ActionStatus::Running {
            return Err(OperationRecordError::InvalidStatusTransition {
                from: self.status,
                to: ActionStatus::Succeeded,
            });
        }
        let known = self.known_after_create_success()?;
        self.status = ActionStatus::Succeeded;
        Ok(known)
    }

    fn known_after_create_success(&self) -> Result<KnownFileLink, OperationRecordError> {
        self.validate_create_conditions()?;
        let TargetCondition::ExpectedLink { link_target, .. } = &self.postcondition else {
            return Err(OperationRecordError::InvalidCreateConditions);
        };
        KnownFileLink::new(
            self.resource_id.clone(),
            link_target.as_path().clone(),
            self.target_path.clone(),
            link_target.clone(),
        )
        .map_err(OperationRecordError::InvalidKnownFileLink)
    }

    fn validate_create_conditions(&self) -> Result<(), OperationRecordError> {
        if self.kind != ActionKind::CreateLink {
            return Err(OperationRecordError::UnsupportedActionKind { kind: self.kind });
        }
        let TargetCondition::Missing {
            target_path: pre_target,
        } = &self.precondition
        else {
            return Err(OperationRecordError::InvalidCreateConditions);
        };
        let TargetCondition::ExpectedLink {
            target_path: post_target,
            link_target: _,
        } = &self.postcondition
        else {
            return Err(OperationRecordError::InvalidCreateConditions);
        };
        if pre_target != &self.target_path || post_target != &self.target_path {
            return Err(OperationRecordError::InvalidCreateConditions);
        }
        Ok(())
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
    /// Creates a new single-action operation for Slice 4's `create_link` path.
    pub(crate) fn new_create_link(
        id: OperationId,
        desired_hash: DesiredHash,
        action: &PlannedAction,
    ) -> Result<(Self, ActionId), OperationRecordError> {
        let action_id = ActionId::parse("a1")?;
        let recorded_action = RecordedAction::from_create_link(action)?;
        let record = Self::from_actions(id, desired_hash, [(action_id.clone(), recorded_action)])?;
        Ok((record, action_id))
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
            action.validate_create_conditions()?;
            if !resource_ids.insert(action.resource_id.clone()) {
                return Err(OperationRecordError::DuplicateResourceId {
                    resource_id: action.resource_id.clone(),
                });
            }
            if !target_paths.insert(action.target_path.clone()) {
                return Err(OperationRecordError::DuplicateTargetPath {
                    target_path: action.target_path.clone(),
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

    /// Marks a `create_link` action successful and returns its verified Known fact.
    pub(crate) fn mark_create_succeeded(
        &mut self,
        action_id: &ActionId,
    ) -> Result<KnownFileLink, OperationRecordError> {
        self.action_mut(action_id)?.mark_create_succeeded()
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
    InvalidCreateConditions,
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
                write!(formatter, "Slice 4 cannot record action kind {kind:?}")
            }
            Self::InvalidCreateConditions => formatter.write_str(
                "a create_link record requires one missing precondition and one expected-link postcondition for the same target",
            ),
            Self::DuplicateResourceId { resource_id } => {
                write!(formatter, "operation records resource {resource_id} more than once")
            }
            Self::DuplicateTargetPath { target_path } => {
                write!(formatter, "operation records target {target_path} more than once")
            }
            Self::UnknownActionId { action_id } => {
                write!(formatter, "operation does not contain action {}", action_id.as_str())
            }
            Self::InvalidStatusTransition { from, to } => {
                write!(formatter, "invalid action-status transition: {from:?} -> {to:?}")
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
            | Self::InvalidCreateConditions
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
