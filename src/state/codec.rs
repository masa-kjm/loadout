//! Strict JSON representations and conversions for the durable state schema.
//! This module performs no I/O and does not advance operation or Known state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::hashes::{
    CanonicalHashError, DefinitionHash, DesiredHash, HashParseError, definition_hash,
};
use crate::domain::ids::{FullyQualifiedResourceId, FullyQualifiedResourceIdError};
use crate::domain::known::{KnownFileLink, KnownFileLinkError, KnownState, KnownStateError};
use crate::domain::paths::{ResolvedPath, ResolvedPathError};
use crate::domain::plan::{ActionKind, TargetCondition};
use crate::state::operation::{
    ActionId, ActionStatus, OperationId, OperationRecord, OperationRecordError, RecordedAction,
};
use crate::state::repository::{CommitError, PersistedState};

const STATE_SCHEMA_VERSION: u32 = 1;

/// A strict on-disk representation of `state.json`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StateDocument {
    schema_version: u32,
    resources: BTreeMap<String, PersistedKnownResource>,
    active_operation: Option<PersistedOperationRecord>,
}

impl StateDocument {
    pub(super) fn from_state(state: &PersistedState) -> Result<Self, CommitError> {
        let resources = state
            .known()
            .resources()
            .map(|resource| {
                Ok((
                    resource.resource_id().as_str().to_owned(),
                    PersistedKnownResource::from_known(resource)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CommitError>>()?;
        let active_operation = state
            .active_operation()
            .map(PersistedOperationRecord::from_operation)
            .transpose()?;
        Ok(Self {
            schema_version: STATE_SCHEMA_VERSION,
            resources,
            active_operation,
        })
    }

    pub(super) fn into_state(self) -> Result<PersistedState, StateDecodeError> {
        if self.schema_version != STATE_SCHEMA_VERSION {
            return Err(StateDecodeError::UnsupportedSchemaVersion {
                actual: self.schema_version,
            });
        }
        let resources = self
            .resources
            .into_iter()
            .map(|(resource_id, resource)| resource.into_known(resource_id))
            .collect::<Result<Vec<_>, _>>()?;
        let known = KnownState::new(resources).map_err(StateDecodeError::InvalidKnownState)?;
        let active_operation = self
            .active_operation
            .map(PersistedOperationRecord::into_operation)
            .transpose()?;
        PersistedState::from_parts(known, active_operation)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedKnownResource {
    definition_hash: String,
    file_link: PersistedFileLink,
}

impl PersistedKnownResource {
    fn from_known(resource: &KnownFileLink) -> Result<Self, CommitError> {
        let definition_hash = definition_hash_for_known(resource).map_err(CommitError::Hash)?;
        Ok(Self {
            definition_hash: definition_hash.as_str().to_owned(),
            file_link: PersistedFileLink::from_known(resource)?,
        })
    }

    fn into_known(self, raw_resource_id: String) -> Result<KnownFileLink, StateDecodeError> {
        let resource_id = FullyQualifiedResourceId::parse(&raw_resource_id).map_err(|source| {
            StateDecodeError::InvalidResourceId {
                value: raw_resource_id,
                source,
            }
        })?;
        let expected_hash = DefinitionHash::parse(self.definition_hash).map_err(|source| {
            StateDecodeError::InvalidDefinitionHash {
                resource_id: resource_id.clone(),
                source,
            }
        })?;
        let known = self.file_link.into_known(resource_id.clone())?;
        let actual_hash = definition_hash_for_known(&known).map_err(|source| {
            StateDecodeError::DefinitionHashEncoding {
                resource_id: resource_id.clone(),
                source,
            }
        })?;
        if expected_hash != actual_hash {
            return Err(StateDecodeError::DefinitionHashMismatch {
                resource_id,
                expected: expected_hash,
                actual: actual_hash,
            });
        }
        Ok(known)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileLink {
    source_path: String,
    target_path: String,
    link_target: String,
}

impl PersistedFileLink {
    fn from_known(resource: &KnownFileLink) -> Result<Self, CommitError> {
        Ok(Self {
            source_path: encode_path(resource.source_path())?,
            target_path: encode_path(resource.target_path())?,
            link_target: encode_path(resource.link_target().as_path())?,
        })
    }

    fn into_known(
        self,
        resource_id: FullyQualifiedResourceId,
    ) -> Result<KnownFileLink, StateDecodeError> {
        let source_path = decode_path(self.source_path)?;
        let target_path = decode_path(self.target_path)?;
        let link_target = LinkTarget::new(decode_path(self.link_target)?);
        KnownFileLink::new(resource_id, source_path, target_path, link_target)
            .map_err(StateDecodeError::InvalidKnownFileLink)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedOperationRecord {
    id: String,
    desired_hash: String,
    actions: BTreeMap<String, PersistedRecordedAction>,
}

impl PersistedOperationRecord {
    fn from_operation(operation: &OperationRecord) -> Result<Self, CommitError> {
        let actions = operation
            .actions()
            .map(|(action_id, action)| {
                Ok((
                    action_id.as_str().to_owned(),
                    PersistedRecordedAction::from_action(action)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CommitError>>()?;
        Ok(Self {
            id: operation.id().as_str().to_owned(),
            desired_hash: operation.desired_hash().as_str().to_owned(),
            actions,
        })
    }

    fn into_operation(self) -> Result<OperationRecord, StateDecodeError> {
        let id = OperationId::parse(self.id).map_err(StateDecodeError::InvalidOperation)?;
        let desired_hash =
            DesiredHash::parse(self.desired_hash).map_err(StateDecodeError::InvalidDesiredHash)?;
        let actions = self
            .actions
            .into_iter()
            .map(|(action_id, action)| {
                let action_id =
                    ActionId::parse(action_id).map_err(StateDecodeError::InvalidOperation)?;
                Ok((action_id, action.into_action()?))
            })
            .collect::<Result<Vec<_>, StateDecodeError>>()?;
        OperationRecord::from_actions(id, desired_hash, actions)
            .map_err(StateDecodeError::InvalidOperation)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedRecordedAction {
    kind: PersistedActionKind,
    resource_id: String,
    target_path: String,
    precondition: PersistedTargetCondition,
    postcondition: PersistedTargetCondition,
    status: PersistedActionStatus,
}

impl PersistedRecordedAction {
    fn from_action(action: &RecordedAction) -> Result<Self, CommitError> {
        let kind = match action.kind() {
            ActionKind::CreateLink => PersistedActionKind::CreateLink,
            ActionKind::RemoveLink => PersistedActionKind::RemoveLink,
            ActionKind::ForgetMissing => PersistedActionKind::ForgetMissing,
            kind => return Err(CommitError::UnsupportedOperationAction { kind }),
        };
        Ok(Self {
            kind,
            resource_id: action.resource_id().as_str().to_owned(),
            target_path: encode_path(action.target_path())?,
            precondition: PersistedTargetCondition::from_condition(&action.precondition())?,
            postcondition: PersistedTargetCondition::from_condition(&action.postcondition())?,
            status: PersistedActionStatus::from_status(action.status()),
        })
    }

    fn into_action(self) -> Result<RecordedAction, StateDecodeError> {
        let resource_id = FullyQualifiedResourceId::parse(&self.resource_id).map_err(|source| {
            StateDecodeError::InvalidResourceId {
                value: self.resource_id,
                source,
            }
        })?;
        let target_path = decode_path(self.target_path)?;
        let precondition = self.precondition.into_condition(&target_path)?;
        let postcondition = self.postcondition.into_condition(&target_path)?;
        let kind = match self.kind {
            PersistedActionKind::CreateLink => ActionKind::CreateLink,
            PersistedActionKind::RemoveLink => ActionKind::RemoveLink,
            PersistedActionKind::ForgetMissing => ActionKind::ForgetMissing,
        };
        RecordedAction::from_persisted(
            kind,
            resource_id,
            target_path,
            precondition,
            postcondition,
            self.status.into_status(),
        )
        .map_err(StateDecodeError::InvalidOperation)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PersistedActionKind {
    CreateLink,
    RemoveLink,
    ForgetMissing,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PersistedActionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Uncertain,
}

impl PersistedActionStatus {
    fn from_status(status: ActionStatus) -> Self {
        match status {
            ActionStatus::Pending => Self::Pending,
            ActionStatus::Running => Self::Running,
            ActionStatus::Succeeded => Self::Succeeded,
            ActionStatus::Failed => Self::Failed,
            ActionStatus::Skipped => Self::Skipped,
            ActionStatus::Uncertain => Self::Uncertain,
        }
    }

    fn into_status(self) -> ActionStatus {
        match self {
            Self::Pending => ActionStatus::Pending,
            Self::Running => ActionStatus::Running,
            Self::Succeeded => ActionStatus::Succeeded,
            Self::Failed => ActionStatus::Failed,
            Self::Skipped => ActionStatus::Skipped,
            Self::Uncertain => ActionStatus::Uncertain,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedTargetCondition {
    target: PersistedTargetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link_target: Option<String>,
}

impl PersistedTargetCondition {
    fn from_condition(condition: &TargetCondition) -> Result<Self, CommitError> {
        match condition {
            TargetCondition::Missing { .. } => Ok(Self {
                target: PersistedTargetKind::Missing,
                link_target: None,
            }),
            TargetCondition::ExpectedLink { link_target, .. } => Ok(Self {
                target: PersistedTargetKind::ExpectedLink,
                link_target: Some(encode_path(link_target.as_path())?),
            }),
        }
    }

    fn into_condition(
        self,
        target_path: &ResolvedPath,
    ) -> Result<TargetCondition, StateDecodeError> {
        match (self.target, self.link_target) {
            (PersistedTargetKind::Missing, None) => Ok(TargetCondition::Missing {
                target_path: target_path.clone(),
            }),
            (PersistedTargetKind::Missing, Some(_)) => {
                Err(StateDecodeError::InvalidTargetCondition)
            }
            (PersistedTargetKind::ExpectedLink, Some(link_target)) => {
                Ok(TargetCondition::ExpectedLink {
                    target_path: target_path.clone(),
                    link_target: LinkTarget::new(decode_path(link_target)?),
                })
            }
            (PersistedTargetKind::ExpectedLink, None) => {
                Err(StateDecodeError::InvalidTargetCondition)
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PersistedTargetKind {
    Missing,
    ExpectedLink,
}

fn definition_hash_for_known(
    resource: &KnownFileLink,
) -> Result<DefinitionHash, CanonicalHashError> {
    let resolved = ResolvedFileLink::new(
        resource.resource_id().clone(),
        resource.source_path().clone(),
        resource.target_path().clone(),
    )
    .expect("KnownFileLink already rejects equal source and target paths");
    definition_hash(&resolved)
}

fn encode_path(path: &ResolvedPath) -> Result<String, CommitError> {
    path.as_path()
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| CommitError::NonUnicodePath {
            path: path.as_path().to_path_buf(),
        })
}

fn decode_path(value: String) -> Result<ResolvedPath, StateDecodeError> {
    let path = PathBuf::from(&value);
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(StateDecodeError::NonNormalizedPath { value });
    }
    ResolvedPath::new(path).map_err(|source| StateDecodeError::InvalidPath { value, source })
}

/// The reason a syntactically valid state document violates its durable contract.
#[derive(Debug)]
pub(crate) enum StateDecodeError {
    UnsupportedSchemaVersion {
        actual: u32,
    },
    InvalidResourceId {
        value: String,
        source: FullyQualifiedResourceIdError,
    },
    InvalidDefinitionHash {
        resource_id: FullyQualifiedResourceId,
        source: HashParseError,
    },
    DefinitionHashEncoding {
        resource_id: FullyQualifiedResourceId,
        source: CanonicalHashError,
    },
    DefinitionHashMismatch {
        resource_id: FullyQualifiedResourceId,
        expected: DefinitionHash,
        actual: DefinitionHash,
    },
    NonNormalizedPath {
        value: String,
    },
    InvalidPath {
        value: String,
        source: ResolvedPathError,
    },
    InvalidKnownFileLink(KnownFileLinkError),
    InvalidKnownState(KnownStateError),
    InvalidDesiredHash(HashParseError),
    InvalidOperation(OperationRecordError),
    InvalidTargetCondition,
    SucceededActionKnownMismatch {
        action_id: String,
        resource_id: FullyQualifiedResourceId,
    },
    ActiveActionKnownMismatch {
        action_id: String,
        resource_id: FullyQualifiedResourceId,
    },
}

impl fmt::Display for StateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { actual } => write!(
                formatter,
                "unsupported state schema_version {actual}; expected {STATE_SCHEMA_VERSION}"
            ),
            Self::InvalidResourceId { value, source } => {
                write!(
                    formatter,
                    "invalid persisted resource ID {value:?}: {source}"
                )
            }
            Self::InvalidDefinitionHash {
                resource_id,
                source,
            } => write!(
                formatter,
                "invalid definition hash for persisted resource {resource_id}: {source}"
            ),
            Self::DefinitionHashEncoding {
                resource_id,
                source,
            } => write!(
                formatter,
                "cannot validate definition hash for persisted resource {resource_id}: {source}"
            ),
            Self::DefinitionHashMismatch {
                resource_id,
                expected,
                actual,
            } => write!(
                formatter,
                "definition hash mismatch for persisted resource {resource_id}: expected {expected}, calculated {actual}"
            ),
            Self::NonNormalizedPath { value } => {
                write!(formatter, "persisted path is not normalized: {value}")
            }
            Self::InvalidPath { value, source } => {
                write!(formatter, "invalid persisted path {value:?}: {source}")
            }
            Self::InvalidKnownFileLink(error) => error.fmt(formatter),
            Self::InvalidKnownState(error) => error.fmt(formatter),
            Self::InvalidDesiredHash(error) => error.fmt(formatter),
            Self::InvalidOperation(error) => error.fmt(formatter),
            Self::InvalidTargetCondition => {
                formatter.write_str("persisted target condition does not match its target kind")
            }
            Self::SucceededActionKnownMismatch {
                action_id,
                resource_id,
            } => write!(
                formatter,
                "succeeded action {action_id} lacks its atomically committed Known resource {resource_id}"
            ),
            Self::ActiveActionKnownMismatch {
                action_id,
                resource_id,
            } => write!(
                formatter,
                "active action {action_id} does not retain its required Known resource {resource_id}"
            ),
        }
    }
}

impl std::error::Error for StateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidResourceId { source, .. } => Some(source),
            Self::InvalidDefinitionHash { source, .. } => Some(source),
            Self::DefinitionHashEncoding { source, .. } => Some(source),
            Self::InvalidPath { source, .. } => Some(source),
            Self::InvalidKnownFileLink(error) => Some(error),
            Self::InvalidKnownState(error) => Some(error),
            Self::InvalidDesiredHash(error) => Some(error),
            Self::InvalidOperation(error) => Some(error),
            Self::UnsupportedSchemaVersion { .. }
            | Self::DefinitionHashMismatch { .. }
            | Self::NonNormalizedPath { .. }
            | Self::InvalidTargetCondition
            | Self::SucceededActionKnownMismatch { .. }
            | Self::ActiveActionKnownMismatch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    // Construct the existing wire format directly, independently of the DTO encoder.
    // Paths are platform-local values only; these tests never access the filesystem.
    fn document_fixture(kind: &str, status: &str) -> Value {
        let root = std::env::temp_dir().join("loadout-codec-fixture");
        let source = root.join("store/config");
        let target = root.join("home/.config");
        let resolved = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            ResolvedPath::new(source.clone()).unwrap(),
            ResolvedPath::new(target.clone()).unwrap(),
        )
        .unwrap();
        let missing = json!({"target": "missing"});
        let expected = json!({"target": "expected_link", "link_target": source});
        let (precondition, postcondition) = match kind {
            "create_link" => (missing, expected),
            "remove_link" => (expected, missing),
            "forget_missing" => (missing.clone(), missing),
            _ => panic!("fixture supports only implemented actions"),
        };
        let mut resources = json!({});
        if (kind == "create_link") == (status == "succeeded") {
            resources["base/config"] = json!({
                "definition_hash": definition_hash(&resolved).unwrap().as_str(),
                "file_link": {
                    "source_path": source,
                    "target_path": target,
                    "link_target": source,
                },
            });
        }
        json!({
            "schema_version": 1,
            "resources": resources,
            "active_operation": {
                "id": "op-fixture",
                "desired_hash": format!("sha256:{}", "a".repeat(64)),
                "actions": {
                    "a1": {
                        "kind": kind,
                        "resource_id": "base/config",
                        "target_path": target,
                        "precondition": precondition,
                        "postcondition": postcondition,
                        "status": status,
                    },
                },
            },
        })
    }

    #[test]
    fn existing_action_documents_preserve_their_complete_json_for_every_status() {
        for kind in ["create_link", "remove_link", "forget_missing"] {
            for status in [
                "pending",
                "running",
                "succeeded",
                "failed",
                "skipped",
                "uncertain",
            ] {
                let fixture = document_fixture(kind, status);
                let state = serde_json::from_value::<StateDocument>(fixture.clone())
                    .unwrap()
                    .into_state()
                    .unwrap();
                let encoded =
                    serde_json::to_value(StateDocument::from_state(&state).unwrap()).unwrap();
                assert_eq!(encoded, fixture, "{kind}/{status}");
            }
        }
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_state_object_level() {
        for pointer in [
            "",
            "/resources/base~1config",
            "/resources/base~1config/file_link",
            "/active_operation",
            "/active_operation/actions/a1",
            "/active_operation/actions/a1/precondition",
            "/active_operation/actions/a1/postcondition",
        ] {
            let mut fixture = document_fixture("remove_link", "running");
            fixture.pointer_mut(pointer).unwrap()["unexpected"] = json!(true);
            assert!(
                serde_json::from_value::<StateDocument>(fixture).is_err(),
                "unknown field at {pointer} must be rejected"
            );
        }
    }

    #[test]
    fn action_kind_and_condition_mismatches_are_rejected() {
        for kind in ["create_link", "remove_link", "forget_missing"] {
            for condition in ["precondition", "postcondition"] {
                let mut fixture = document_fixture(kind, "running");
                let action = &mut fixture["active_operation"]["actions"]["a1"];
                action[condition] = if action[condition]["target"] == "missing" {
                    json!({
                        "target": "expected_link",
                        "link_target": std::env::temp_dir().join("loadout-codec-fixture/store/config"),
                    })
                } else {
                    json!({"target": "missing"})
                };
                let document = serde_json::from_value::<StateDocument>(fixture).unwrap();
                assert!(
                    matches!(
                        document.into_state(),
                        Err(StateDecodeError::InvalidOperation(
                            OperationRecordError::InvalidActionConditions { .. }
                        ))
                    ),
                    "{kind}/{condition}"
                );
            }
        }
    }
}
