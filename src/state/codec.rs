//! Strict JSON representations and conversions for the durable state schema.
//! This module performs no I/O and does not advance operation or Known state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::directory_link::ResolvedDirectoryLink;
use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::hashes::{
    CanonicalHashError, DefinitionHash, DesiredHash, HashParseError, definition_hash,
    directory_link_definition_hash,
};
use crate::domain::ids::{FullyQualifiedResourceId, FullyQualifiedResourceIdError};
use crate::domain::known::{
    KnownDirectoryLink, KnownDirectoryLinkError, KnownFileCopy, KnownFileCopyError, KnownFileLink,
    KnownFileLinkError, KnownResource, KnownState, KnownStateError,
};
use crate::domain::paths::{ResolvedPath, ResolvedPathError};
use crate::domain::plan::{ActionKind, TargetCondition};
use crate::state::operation::{
    ActionId, ActionStatus, OperationId, OperationRecord, OperationRecordError,
    PersistedCopyActionFacts, PersistedCopyRemovalFacts, PersistedEffectHandoffFacts,
    RecordedAction,
};
use crate::state::repository::{CommitError, PersistedState};

const STATE_SCHEMA_VERSION: u32 = 3;

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
    effect: PersistedKnownEffect,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PersistedKnownEffect {
    FileLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
    FileCopy {
        source_path: String,
        target_path: String,
        content_fingerprint: String,
    },
    DirectoryLink {
        source_path: String,
        target_path: String,
        link_target: String,
    },
}

impl PersistedKnownResource {
    fn from_known(resource: &KnownResource) -> Result<Self, CommitError> {
        match resource {
            KnownResource::FileLink(resource) => Ok(Self {
                definition_hash: definition_hash_for_known(resource)
                    .map_err(CommitError::Hash)?
                    .as_str()
                    .to_owned(),
                effect: PersistedKnownEffect::FileLink {
                    source_path: encode_path(resource.source_path())?,
                    target_path: encode_path(resource.target_path())?,
                    link_target: encode_path(resource.link_target().as_path())?,
                },
            }),
            KnownResource::FileCopy(resource) => Ok(Self {
                definition_hash: definition_hash_for_copy_known(resource)
                    .map_err(CommitError::Hash)?
                    .as_str()
                    .to_owned(),
                effect: PersistedKnownEffect::FileCopy {
                    source_path: encode_path(resource.source_path())?,
                    target_path: encode_path(resource.target_path())?,
                    content_fingerprint: resource.content_fingerprint().as_str().to_owned(),
                },
            }),
            KnownResource::DirectoryLink(resource) => Ok(Self {
                definition_hash: definition_hash_for_directory_known(resource)
                    .map_err(CommitError::Hash)?
                    .as_str()
                    .to_owned(),
                effect: PersistedKnownEffect::DirectoryLink {
                    source_path: encode_path(resource.source_path())?,
                    target_path: encode_path(resource.target_path())?,
                    link_target: encode_path(resource.link_target().as_path())?,
                },
            }),
        }
    }

    fn into_known(self, raw_resource_id: String) -> Result<KnownResource, StateDecodeError> {
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
        let known = self.effect.into_known(resource_id.clone())?;
        let actual_hash = definition_hash_for_known_resource(&known).map_err(|source| {
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

impl PersistedKnownEffect {
    fn from_known(resource: &KnownResource) -> Result<Self, CommitError> {
        match resource {
            KnownResource::FileLink(resource) => Ok(Self::FileLink {
                source_path: encode_path(resource.source_path())?,
                target_path: encode_path(resource.target_path())?,
                link_target: encode_path(resource.link_target().as_path())?,
            }),
            KnownResource::FileCopy(resource) => Ok(Self::FileCopy {
                source_path: encode_path(resource.source_path())?,
                target_path: encode_path(resource.target_path())?,
                content_fingerprint: resource.content_fingerprint().as_str().to_owned(),
            }),
            KnownResource::DirectoryLink(resource) => Ok(Self::DirectoryLink {
                source_path: encode_path(resource.source_path())?,
                target_path: encode_path(resource.target_path())?,
                link_target: encode_path(resource.link_target().as_path())?,
            }),
        }
    }

    fn into_known(
        self,
        resource_id: FullyQualifiedResourceId,
    ) -> Result<KnownResource, StateDecodeError> {
        let known = match self {
            PersistedKnownEffect::FileLink {
                source_path,
                target_path,
                link_target,
            } => PersistedFileLink {
                source_path,
                target_path,
                link_target,
            }
            .into_known(resource_id.clone())?
            .into(),
            PersistedKnownEffect::FileCopy {
                source_path,
                target_path,
                content_fingerprint,
            } => KnownFileCopy::new(
                resource_id.clone(),
                decode_path(source_path)?,
                decode_path(target_path)?,
                ContentFingerprint::parse(content_fingerprint).map_err(|source| {
                    StateDecodeError::InvalidContentFingerprint {
                        resource_id: resource_id.clone(),
                        source,
                    }
                })?,
            )
            .map_err(StateDecodeError::InvalidKnownFileCopy)?
            .into(),
            PersistedKnownEffect::DirectoryLink {
                source_path,
                target_path,
                link_target,
            } => KnownDirectoryLink::new(
                resource_id.clone(),
                decode_path(source_path)?,
                decode_path(target_path)?,
                LinkTarget::new(decode_path(link_target)?),
            )
            .map_err(StateDecodeError::InvalidKnownDirectoryLink)?
            .into(),
        };
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    superseded_resource_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_effect: Option<PersistedKnownEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    final_effect: Option<PersistedKnownEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_fingerprint: Option<String>,
    precondition: PersistedActionCondition,
    postcondition: PersistedActionCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temporary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temporary_staged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication_attempted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temporary: Option<PersistedDirectoryTemporary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new_resource_id: Option<String>,
    status: PersistedActionStatus,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum PersistedActionCondition {
    Legacy(PersistedTargetCondition),
    Directory(PersistedDirectoryCondition),
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(untagged)]
enum PersistedDirectoryCondition {
    Single(PersistedDirectoryPredicate),
    Relocate(PersistedDirectoryRelocateCondition),
    Final(PersistedDirectoryFinalCondition),
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PersistedDirectoryRelocateCondition {
    old: PersistedDirectoryPredicate,
    new: PersistedDirectoryPredicate,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PersistedDirectoryFinalCondition {
    target: PersistedDirectoryPredicate,
    temporary: PersistedDirectoryPredicate,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PersistedDirectoryTemporary {
    path: String,
    predicate: PersistedDirectoryPredicate,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PersistedDirectoryPredicate {
    kind: PersistedDirectoryPredicateKind,
    target_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link_target: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PersistedDirectoryPredicateKind {
    Missing,
    ExpectedDirectoryLink,
}

impl PersistedRecordedAction {
    fn into_directory_action(
        self,
        kind: ActionKind,
        resource_id: FullyQualifiedResourceId,
    ) -> Result<RecordedAction, StateDecodeError> {
        if self.target_path.is_some()
            || self.old_target_path.is_some()
            || self.source_path.is_some()
            || self.content_fingerprint.is_some()
            || self.temporary_path.is_some()
            || self.temporary_staged.is_some()
            || self.publication_attempted.is_some()
        {
            return Err(StateDecodeError::InvalidTargetCondition);
        }
        let PersistedActionCondition::Directory(declared_precondition) = self.precondition else {
            return Err(StateDecodeError::InvalidTargetCondition);
        };
        let PersistedActionCondition::Directory(declared_postcondition) = self.postcondition else {
            return Err(StateDecodeError::InvalidTargetCondition);
        };
        let old_resource_id = self
            .old_resource_id
            .map(|value| {
                FullyQualifiedResourceId::parse(&value)
                    .map_err(|source| StateDecodeError::InvalidResourceId { value, source })
            })
            .transpose()?;
        let superseded_resource_id = self
            .superseded_resource_id
            .map(|value| {
                FullyQualifiedResourceId::parse(&value)
                    .map_err(|source| StateDecodeError::InvalidResourceId { value, source })
            })
            .transpose()?;
        let old_effect_resource_id = if kind == ActionKind::ReplaceOwnership {
            old_resource_id
                .clone()
                .ok_or(StateDecodeError::InvalidTargetCondition)?
        } else {
            resource_id.clone()
        };
        let old_effect = self
            .old_effect
            .map(|effect| effect.into_known(old_effect_resource_id.clone()))
            .transpose()?
            .map(|effect| match effect {
                KnownResource::DirectoryLink(value) => Ok(value),
                _ => Err(StateDecodeError::InvalidTargetCondition),
            })
            .transpose()?;
        let final_effect = self
            .final_effect
            .map(|effect| effect.into_known(resource_id.clone()))
            .transpose()?
            .map(|effect| match effect {
                KnownResource::DirectoryLink(value) => Ok(value),
                _ => Err(StateDecodeError::InvalidTargetCondition),
            })
            .transpose()?;
        let temporary_path = self
            .temporary
            .as_ref()
            .map(|temporary| decode_path(temporary.path.clone()))
            .transpose()?;
        if self
            .new_resource_id
            .as_deref()
            .map(FullyQualifiedResourceId::parse)
            .transpose()
            .map_err(|source| StateDecodeError::InvalidResourceId {
                value: self.new_resource_id.clone().unwrap_or_default(),
                source,
            })?
            .as_ref()
            .is_some_and(|new_resource_id| new_resource_id != &resource_id)
        {
            return Err(StateDecodeError::InvalidTargetCondition);
        }
        let (precondition, postcondition) = directory_simple_conditions(
            kind,
            old_effect.as_ref(),
            final_effect.as_ref(),
            temporary_path.as_ref(),
        )?;
        let action = RecordedAction::from_persisted_directory_link(
            kind,
            resource_id,
            old_effect,
            final_effect,
            match kind {
                ActionKind::CreateDirectoryLink => superseded_resource_id,
                ActionKind::ReplaceOwnership => old_resource_id,
                _ => {
                    if old_resource_id.is_some() || superseded_resource_id.is_some() {
                        return Err(StateDecodeError::InvalidTargetCondition);
                    }
                    None
                }
            },
            temporary_path,
            precondition,
            postcondition,
            self.status.into_status(),
        )
        .map_err(StateDecodeError::InvalidOperation)?;
        let (expected_precondition, expected_postcondition, expected_temporary) =
            directory_conditions_for(&action)
                .map_err(|_| StateDecodeError::InvalidTargetCondition)?;
        if declared_precondition != expected_precondition
            || declared_postcondition != expected_postcondition
            || self.temporary != expected_temporary
            || (action.kind() == ActionKind::ReplaceOwnership && self.new_resource_id.is_none())
            || (action.kind() != ActionKind::ReplaceOwnership && self.new_resource_id.is_some())
        {
            return Err(StateDecodeError::InvalidTargetCondition);
        }
        Ok(action)
    }

    fn from_directory_action(
        action: &RecordedAction,
        kind: PersistedActionKind,
    ) -> Result<Self, CommitError> {
        let facts = action.directory_link_facts().expect("checked above");
        let (precondition, postcondition, temporary) = directory_conditions_for(action)?;
        let old_effect = facts
            .old_effect()
            .map(|effect| PersistedKnownEffect::from_known(&effect.clone().into()))
            .transpose()?;
        let final_effect = facts
            .final_effect()
            .map(|effect| PersistedKnownEffect::from_known(&effect.clone().into()))
            .transpose()?;
        let source_changing_handoff =
            action.kind() == ActionKind::ReplaceOwnership && facts.temporary_path().is_some();
        Ok(Self {
            kind,
            resource_id: action.resource_id().as_str().to_owned(),
            old_resource_id: if action.kind() == ActionKind::ReplaceOwnership {
                facts
                    .superseded_resource_id()
                    .map(|id| id.as_str().to_owned())
            } else {
                None
            },
            superseded_resource_id: if action.kind() == ActionKind::CreateDirectoryLink {
                facts
                    .superseded_resource_id()
                    .map(|id| id.as_str().to_owned())
            } else {
                None
            },
            old_target_path: None,
            old_effect,
            final_effect,
            target_path: None,
            source_path: None,
            content_fingerprint: None,
            precondition: PersistedActionCondition::Directory(precondition),
            postcondition: PersistedActionCondition::Directory(postcondition),
            temporary_path: None,
            temporary_staged: None,
            publication_attempted: None,
            temporary,
            new_resource_id: source_changing_handoff
                .then(|| action.resource_id().as_str().to_owned())
                .or_else(|| {
                    (action.kind() == ActionKind::ReplaceOwnership)
                        .then(|| action.resource_id().as_str().to_owned())
                }),
            status: PersistedActionStatus::from_status(action.status()),
        })
    }

    fn from_action(action: &RecordedAction) -> Result<Self, CommitError> {
        let kind = match action.kind() {
            ActionKind::CreateLink => PersistedActionKind::CreateLink,
            ActionKind::ReplaceLink => PersistedActionKind::ReplaceLink,
            ActionKind::ReplaceOwnership => PersistedActionKind::ReplaceOwnership,
            ActionKind::RelocateLink => PersistedActionKind::RelocateLink,
            ActionKind::RemoveLink => PersistedActionKind::RemoveLink,
            ActionKind::ForgetMissing => PersistedActionKind::ForgetMissing,
            ActionKind::CreateCopy => PersistedActionKind::CreateCopy,
            ActionKind::ReplaceCopy => PersistedActionKind::ReplaceCopy,
            ActionKind::RelocateCopy => PersistedActionKind::RelocateCopy,
            ActionKind::RemoveCopy => PersistedActionKind::RemoveCopy,
            ActionKind::ReplaceEffect => PersistedActionKind::ReplaceEffect,
            ActionKind::CreateDirectoryLink => PersistedActionKind::CreateDirectoryLink,
            ActionKind::ReplaceDirectoryLink => PersistedActionKind::ReplaceDirectoryLink,
            ActionKind::RelocateDirectoryLink => PersistedActionKind::RelocateDirectoryLink,
            ActionKind::RemoveDirectoryLink => PersistedActionKind::RemoveDirectoryLink,
            kind => return Err(CommitError::UnsupportedOperationAction { kind }),
        };
        if action.directory_link_facts().is_some() {
            return Self::from_directory_action(action, kind);
        }
        Ok(Self {
            kind,
            resource_id: action.resource_id().as_str().to_owned(),
            old_resource_id: action
                .replaced_resource_id()
                .map(|id| id.as_str().to_owned()),
            superseded_resource_id: None,
            old_target_path: action
                .relocation_facts()
                .map(|facts| encode_path(facts.old_target_path()))
                .or_else(|| {
                    action
                        .copy_facts()
                        .and_then(|facts| {
                            (action.kind() == ActionKind::RelocateCopy).then(|| {
                                facts.old_effect().and_then(|effect| match effect {
                                    KnownResource::FileCopy(copy) => {
                                        Some(encode_path(copy.target_path()))
                                    }
                                    KnownResource::FileLink(_) => None,
                                    KnownResource::DirectoryLink(_) => None,
                                })
                            })
                        })
                        .flatten()
                })
                .or_else(|| {
                    action.directory_link_facts().and_then(|facts| {
                        (action.kind() == ActionKind::RelocateDirectoryLink)
                            .then(|| {
                                facts
                                    .old_effect()
                                    .map(|effect| encode_path(effect.target_path()))
                            })
                            .flatten()
                    })
                })
                .transpose()?,
            old_effect: action
                .copy_facts()
                .and_then(|facts| facts.old_effect().cloned())
                .or_else(|| {
                    action
                        .effect_handoff_facts()
                        .map(|facts| facts.old_effect().clone())
                })
                .or_else(|| {
                    action
                        .directory_link_facts()
                        .and_then(|facts| facts.old_effect().cloned().map(KnownResource::from))
                })
                .map(|effect| PersistedKnownEffect::from_known(&effect))
                .transpose()?,
            final_effect: action
                .copy_facts()
                .map(|facts| PersistedKnownEffect::from_known(facts.final_effect()))
                .or_else(|| {
                    action
                        .effect_handoff_facts()
                        .map(|facts| PersistedKnownEffect::from_known(facts.final_effect()))
                })
                .or_else(|| {
                    action
                        .directory_link_facts()
                        .and_then(|facts| facts.final_effect().cloned())
                        .map(|effect| PersistedKnownEffect::from_known(&effect.into()))
                })
                .transpose()?,
            target_path: Some(encode_path(action.target_path())?),
            source_path: action
                .copy_facts()
                .map(|facts| encode_path(facts.source_path()))
                .or_else(|| {
                    action
                        .copy_removal_facts()
                        .map(|facts| encode_path(facts.source_path()))
                })
                .transpose()?,
            content_fingerprint: action
                .copy_facts()
                .map(|facts| facts.content_fingerprint().as_str().to_owned())
                .or_else(|| {
                    action
                        .copy_removal_facts()
                        .map(|facts| facts.content_fingerprint().as_str().to_owned())
                }),
            precondition: PersistedActionCondition::Legacy(
                PersistedTargetCondition::from_condition(&action.precondition())?,
            ),
            postcondition: PersistedActionCondition::Legacy(
                PersistedTargetCondition::from_condition(&action.postcondition())?,
            ),
            temporary_path: action
                .replacement_facts()
                .map(|facts| encode_path(facts.temporary_path()))
                .or_else(|| {
                    action
                        .copy_facts()
                        .map(|facts| encode_path(facts.temporary_path()))
                })
                .or_else(|| {
                    action
                        .effect_handoff_facts()
                        .and_then(|facts| facts.temporary_path().map(encode_path))
                })
                .or_else(|| {
                    action
                        .directory_link_facts()
                        .and_then(|facts| facts.temporary_path().map(encode_path))
                })
                .transpose()?,
            temporary_staged: action.temporary_staged(),
            publication_attempted: action.publication_attempted(),
            temporary: None,
            new_resource_id: None,
            status: PersistedActionStatus::from_status(action.status()),
        })
    }

    fn into_action(self) -> Result<RecordedAction, StateDecodeError> {
        let resource_id = FullyQualifiedResourceId::parse(&self.resource_id).map_err(|source| {
            StateDecodeError::InvalidResourceId {
                value: self.resource_id.clone(),
                source,
            }
        })?;
        let kind = match self.kind {
            PersistedActionKind::CreateLink => ActionKind::CreateLink,
            PersistedActionKind::ReplaceLink => ActionKind::ReplaceLink,
            PersistedActionKind::ReplaceOwnership => ActionKind::ReplaceOwnership,
            PersistedActionKind::RelocateLink => ActionKind::RelocateLink,
            PersistedActionKind::RemoveLink => ActionKind::RemoveLink,
            PersistedActionKind::ForgetMissing => ActionKind::ForgetMissing,
            PersistedActionKind::CreateCopy => ActionKind::CreateCopy,
            PersistedActionKind::ReplaceCopy => ActionKind::ReplaceCopy,
            PersistedActionKind::RelocateCopy => ActionKind::RelocateCopy,
            PersistedActionKind::RemoveCopy => ActionKind::RemoveCopy,
            PersistedActionKind::ReplaceEffect => ActionKind::ReplaceEffect,
            PersistedActionKind::CreateDirectoryLink => ActionKind::CreateDirectoryLink,
            PersistedActionKind::ReplaceDirectoryLink => ActionKind::ReplaceDirectoryLink,
            PersistedActionKind::RelocateDirectoryLink => ActionKind::RelocateDirectoryLink,
            PersistedActionKind::RemoveDirectoryLink => ActionKind::RemoveDirectoryLink,
        };
        let is_directory = matches!(
            kind,
            ActionKind::CreateDirectoryLink
                | ActionKind::ReplaceDirectoryLink
                | ActionKind::RelocateDirectoryLink
                | ActionKind::RemoveDirectoryLink
        ) || (kind == ActionKind::ForgetMissing
            && self.old_effect.as_ref().is_some_and(|effect| {
                matches!(effect, PersistedKnownEffect::DirectoryLink { .. })
            }))
            || (kind == ActionKind::ReplaceOwnership
                && self.old_effect.as_ref().is_some_and(|effect| {
                    matches!(effect, PersistedKnownEffect::DirectoryLink { .. })
                }));
        if is_directory {
            return self.into_directory_action(kind, resource_id);
        }
        let target_path = decode_path(
            self.target_path
                .ok_or(StateDecodeError::InvalidTargetCondition)?,
        )?;
        let old_target_path = self.old_target_path.map(decode_path).transpose()?;
        let precondition_target = old_target_path.as_ref().unwrap_or(&target_path);
        let PersistedActionCondition::Legacy(precondition) = self.precondition else {
            return Err(StateDecodeError::InvalidTargetCondition);
        };
        let PersistedActionCondition::Legacy(postcondition) = self.postcondition else {
            return Err(StateDecodeError::InvalidTargetCondition);
        };
        let precondition = precondition.into_condition(precondition_target)?;
        let postcondition = postcondition.into_condition(&target_path)?;
        let status = self.status.into_status();
        let old_resource_id = self
            .old_resource_id
            .map(|value| {
                FullyQualifiedResourceId::parse(&value)
                    .map_err(|source| StateDecodeError::InvalidResourceId { value, source })
            })
            .transpose()?;
        let superseded_resource_id = self
            .superseded_resource_id
            .map(|value| {
                FullyQualifiedResourceId::parse(&value)
                    .map_err(|source| StateDecodeError::InvalidResourceId { value, source })
            })
            .transpose()?;
        if superseded_resource_id.is_some() {
            return Err(StateDecodeError::InvalidTargetCondition);
        }
        if kind == ActionKind::ReplaceEffect {
            let old_effect = self
                .old_effect
                .ok_or(StateDecodeError::InvalidTargetCondition)?
                .into_known(resource_id.clone())?;
            let final_effect = self
                .final_effect
                .ok_or(StateDecodeError::InvalidTargetCondition)?
                .into_known(resource_id.clone())?;
            return RecordedAction::from_persisted_replace_effect(PersistedEffectHandoffFacts {
                resource_id,
                old_effect,
                final_effect,
                temporary_path: self.temporary_path.map(decode_path).transpose()?,
                precondition,
                postcondition,
                temporary_staged: self.temporary_staged,
                publication_attempted: self
                    .publication_attempted
                    .ok_or(StateDecodeError::InvalidTargetCondition)?,
                status,
            })
            .map_err(StateDecodeError::InvalidOperation);
        }
        if matches!(
            kind,
            ActionKind::CreateCopy | ActionKind::ReplaceCopy | ActionKind::RelocateCopy
        ) {
            let source_path = self
                .source_path
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            let content_fingerprint = self
                .content_fingerprint
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            if old_resource_id.is_some()
                || (old_target_path.is_some() && kind != ActionKind::RelocateCopy)
            {
                return Err(StateDecodeError::InvalidTargetCondition);
            }
            let temporary_path = self
                .temporary_path
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            let temporary_staged = self
                .temporary_staged
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            let publication_attempted = self
                .publication_attempted
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            let old_effect = self
                .old_effect
                .map(|effect| effect.into_known(resource_id.clone()))
                .transpose()?;
            let final_effect = self
                .final_effect
                .ok_or(StateDecodeError::InvalidTargetCondition)?
                .into_known(resource_id.clone())?;
            return RecordedAction::from_persisted_copy(PersistedCopyActionFacts {
                kind,
                resource_id,
                source_path: decode_path(source_path)?,
                target_path,
                content_fingerprint: ContentFingerprint::parse(content_fingerprint)
                    .map_err(|_| StateDecodeError::InvalidTargetCondition)?,
                temporary_path: decode_path(temporary_path)?,
                old_effect,
                final_effect,
                precondition,
                postcondition,
                temporary_staged,
                publication_attempted,
                status,
            })
            .map_err(StateDecodeError::InvalidOperation);
        }
        if matches!(kind, ActionKind::RemoveCopy | ActionKind::ForgetMissing)
            && (self.source_path.is_some() || self.content_fingerprint.is_some())
        {
            if old_resource_id.is_some()
                || old_target_path.is_some()
                || self.temporary_path.is_some()
                || self.old_effect.is_some()
                || self.final_effect.is_some()
            {
                return Err(StateDecodeError::InvalidTargetCondition);
            }
            let source_path = self
                .source_path
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            let content_fingerprint = self
                .content_fingerprint
                .ok_or(StateDecodeError::InvalidTargetCondition)?;
            return RecordedAction::from_persisted_copy_removal(PersistedCopyRemovalFacts {
                kind,
                resource_id,
                source_path: decode_path(source_path)?,
                target_path,
                content_fingerprint: ContentFingerprint::parse(content_fingerprint)
                    .map_err(|_| StateDecodeError::InvalidTargetCondition)?,
                precondition,
                postcondition,
                status,
            })
            .map_err(StateDecodeError::InvalidOperation);
        }
        match (kind, old_resource_id, old_target_path, self.temporary_path) {
            (ActionKind::ReplaceLink, None, None, Some(temporary_path)) => {
                RecordedAction::from_persisted_replace_link(
                    resource_id,
                    target_path,
                    precondition,
                    postcondition,
                    decode_path(temporary_path)?,
                    status,
                )
            }
            (ActionKind::ReplaceLink, None, None, None) => {
                Err(OperationRecordError::ReplacementTemporaryRequired)
            }
            (ActionKind::ReplaceOwnership, Some(old_resource_id), None, temporary_path) => {
                RecordedAction::from_persisted_replace_ownership(
                    old_resource_id,
                    resource_id,
                    target_path,
                    precondition,
                    postcondition,
                    temporary_path.map(decode_path).transpose()?,
                    status,
                )
            }
            (ActionKind::ReplaceOwnership, None, None, _) => {
                Err(OperationRecordError::InvalidActionConditions { kind })
            }
            (ActionKind::RelocateLink, None, Some(old_target_path), None) => {
                RecordedAction::from_persisted_relocate_link(
                    resource_id,
                    old_target_path,
                    target_path,
                    precondition,
                    postcondition,
                    status,
                )
            }
            (_, None, None, None) => RecordedAction::from_persisted(
                kind,
                resource_id,
                target_path,
                precondition,
                postcondition,
                status,
            ),
            (_, _, _, Some(_)) | (_, Some(_), _, None) | (_, _, Some(_), None) => {
                Err(OperationRecordError::InvalidActionConditions { kind })
            }
        }
        .map_err(StateDecodeError::InvalidOperation)
    }
}

fn directory_conditions_for(
    action: &RecordedAction,
) -> Result<
    (
        PersistedDirectoryCondition,
        PersistedDirectoryCondition,
        Option<PersistedDirectoryTemporary>,
    ),
    CommitError,
> {
    let facts = action
        .directory_link_facts()
        .expect("directory action required");
    let predicate =
        |condition: &TargetCondition| -> Result<PersistedDirectoryPredicate, CommitError> {
            match condition {
                TargetCondition::Missing { target_path } => Ok(PersistedDirectoryPredicate {
                    kind: PersistedDirectoryPredicateKind::Missing,
                    target_path: encode_path(target_path)?,
                    link_target: None,
                }),
                TargetCondition::ExpectedDirectoryLink {
                    target_path,
                    link_target,
                } => Ok(PersistedDirectoryPredicate {
                    kind: PersistedDirectoryPredicateKind::ExpectedDirectoryLink,
                    target_path: encode_path(target_path)?,
                    link_target: Some(encode_path(link_target.as_path())?),
                }),
                _ => unreachable!("directory facts only derive directory predicates"),
            }
        };
    let missing = |path: &ResolvedPath| {
        predicate(&TargetCondition::Missing {
            target_path: path.clone(),
        })
    };
    let expected = |effect: &KnownDirectoryLink| {
        predicate(&TargetCondition::ExpectedDirectoryLink {
            target_path: effect.target_path().clone(),
            link_target: effect.link_target().clone(),
        })
    };
    let (precondition, postcondition, temporary) = match action.kind() {
        ActionKind::CreateDirectoryLink => (
            PersistedDirectoryCondition::Single(predicate(&action.precondition())?),
            PersistedDirectoryCondition::Single(predicate(&action.postcondition())?),
            None,
        ),
        ActionKind::RemoveDirectoryLink | ActionKind::ForgetMissing => (
            PersistedDirectoryCondition::Single(predicate(&action.precondition())?),
            PersistedDirectoryCondition::Single(predicate(&action.postcondition())?),
            None,
        ),
        ActionKind::RelocateDirectoryLink => {
            let old = facts
                .old_effect()
                .expect("validated relocation has old effect");
            let final_effect = facts
                .final_effect()
                .expect("validated relocation has final effect");
            (
                PersistedDirectoryCondition::Relocate(PersistedDirectoryRelocateCondition {
                    old: expected(old)?,
                    new: missing(final_effect.target_path())?,
                }),
                PersistedDirectoryCondition::Relocate(PersistedDirectoryRelocateCondition {
                    old: missing(old.target_path())?,
                    new: expected(final_effect)?,
                }),
                None,
            )
        }
        ActionKind::ReplaceDirectoryLink | ActionKind::ReplaceOwnership => {
            let old = facts
                .old_effect()
                .expect("validated replacement has old effect");
            let final_effect = facts
                .final_effect()
                .expect("validated replacement has final effect");
            if let Some(path) = facts.temporary_path() {
                let temporary_expected = predicate(&TargetCondition::ExpectedDirectoryLink {
                    target_path: path.clone(),
                    link_target: final_effect.link_target().clone(),
                })?;
                (
                    PersistedDirectoryCondition::Single(expected(old)?),
                    PersistedDirectoryCondition::Final(PersistedDirectoryFinalCondition {
                        target: expected(final_effect)?,
                        temporary: missing(path)?,
                    }),
                    Some(PersistedDirectoryTemporary {
                        path: encode_path(path)?,
                        predicate: temporary_expected,
                    }),
                )
            } else {
                (
                    PersistedDirectoryCondition::Single(expected(old)?),
                    PersistedDirectoryCondition::Single(expected(final_effect)?),
                    None,
                )
            }
        }
        _ => unreachable!("validated directory facts have a directory action kind"),
    };
    Ok((precondition, postcondition, temporary))
}

fn directory_simple_conditions(
    kind: ActionKind,
    old_effect: Option<&KnownDirectoryLink>,
    final_effect: Option<&KnownDirectoryLink>,
    _temporary_path: Option<&ResolvedPath>,
) -> Result<(TargetCondition, TargetCondition), StateDecodeError> {
    let expected = |effect: &KnownDirectoryLink| TargetCondition::ExpectedDirectoryLink {
        target_path: effect.target_path().clone(),
        link_target: effect.link_target().clone(),
    };
    let missing = |path: &ResolvedPath| TargetCondition::Missing {
        target_path: path.clone(),
    };
    match kind {
        ActionKind::CreateDirectoryLink => {
            let effect = final_effect.ok_or(StateDecodeError::InvalidTargetCondition)?;
            Ok((missing(effect.target_path()), expected(effect)))
        }
        ActionKind::RemoveDirectoryLink => {
            let effect = old_effect.ok_or(StateDecodeError::InvalidTargetCondition)?;
            Ok((expected(effect), missing(effect.target_path())))
        }
        ActionKind::ForgetMissing => {
            let effect = old_effect.ok_or(StateDecodeError::InvalidTargetCondition)?;
            Ok((missing(effect.target_path()), missing(effect.target_path())))
        }
        ActionKind::RelocateDirectoryLink
        | ActionKind::ReplaceDirectoryLink
        | ActionKind::ReplaceOwnership => {
            let old = old_effect.ok_or(StateDecodeError::InvalidTargetCondition)?;
            let final_effect = final_effect.ok_or(StateDecodeError::InvalidTargetCondition)?;
            Ok((expected(old), expected(final_effect)))
        }
        _ => Err(StateDecodeError::InvalidTargetCondition),
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PersistedActionKind {
    CreateLink,
    ReplaceLink,
    ReplaceOwnership,
    RelocateLink,
    RemoveLink,
    ForgetMissing,
    CreateCopy,
    ReplaceCopy,
    RelocateCopy,
    RemoveCopy,
    ReplaceEffect,
    CreateDirectoryLink,
    ReplaceDirectoryLink,
    RelocateDirectoryLink,
    RemoveDirectoryLink,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_fingerprint: Option<String>,
}

impl PersistedTargetCondition {
    fn from_condition(condition: &TargetCondition) -> Result<Self, CommitError> {
        match condition {
            TargetCondition::Missing { .. } => Ok(Self {
                target: PersistedTargetKind::Missing,
                link_target: None,
                content_fingerprint: None,
            }),
            TargetCondition::ExpectedLink { link_target, .. } => Ok(Self {
                target: PersistedTargetKind::ExpectedLink,
                link_target: Some(encode_path(link_target.as_path())?),
                content_fingerprint: None,
            }),
            TargetCondition::ExpectedCopy {
                content_fingerprint,
                ..
            } => Ok(Self {
                target: PersistedTargetKind::ExpectedCopy,
                link_target: None,
                content_fingerprint: Some(content_fingerprint.as_str().to_owned()),
            }),
            TargetCondition::ExpectedDirectoryLink { link_target, .. } => Ok(Self {
                target: PersistedTargetKind::ExpectedDirectoryLink,
                link_target: Some(encode_path(link_target.as_path())?),
                content_fingerprint: None,
            }),
        }
    }

    fn into_condition(
        self,
        target_path: &ResolvedPath,
    ) -> Result<TargetCondition, StateDecodeError> {
        match (self.target, self.link_target, self.content_fingerprint) {
            (PersistedTargetKind::Missing, None, None) => Ok(TargetCondition::Missing {
                target_path: target_path.clone(),
            }),
            (PersistedTargetKind::Missing, _, _) => Err(StateDecodeError::InvalidTargetCondition),
            (PersistedTargetKind::ExpectedLink, Some(link_target), None) => {
                Ok(TargetCondition::ExpectedLink {
                    target_path: target_path.clone(),
                    link_target: LinkTarget::new(decode_path(link_target)?),
                })
            }
            (PersistedTargetKind::ExpectedLink, _, _) => {
                Err(StateDecodeError::InvalidTargetCondition)
            }
            (PersistedTargetKind::ExpectedCopy, None, Some(content_fingerprint)) => {
                Ok(TargetCondition::ExpectedCopy {
                    target_path: target_path.clone(),
                    content_fingerprint: ContentFingerprint::parse(content_fingerprint)
                        .map_err(|_| StateDecodeError::InvalidTargetCondition)?,
                })
            }
            (PersistedTargetKind::ExpectedCopy, _, _) => {
                Err(StateDecodeError::InvalidTargetCondition)
            }
            (PersistedTargetKind::ExpectedDirectoryLink, Some(link_target), None) => {
                Ok(TargetCondition::ExpectedDirectoryLink {
                    target_path: target_path.clone(),
                    link_target: LinkTarget::new(decode_path(link_target)?),
                })
            }
            (PersistedTargetKind::ExpectedDirectoryLink, _, _) => {
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
    ExpectedCopy,
    ExpectedDirectoryLink,
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

fn definition_hash_for_copy_known(
    resource: &KnownFileCopy,
) -> Result<DefinitionHash, CanonicalHashError> {
    let resolved = ResolvedFileCopy::new(
        resource.resource_id().clone(),
        resource.source_path().clone(),
        resource.target_path().clone(),
        resource.content_fingerprint().clone(),
    )
    .expect("KnownFileCopy already rejects equal source and target paths");
    crate::domain::hashes::file_copy_definition_hash(&resolved)
}

fn definition_hash_for_directory_known(
    resource: &KnownDirectoryLink,
) -> Result<DefinitionHash, CanonicalHashError> {
    let resolved = ResolvedDirectoryLink::new(
        resource.resource_id().clone(),
        resource.source_path().clone(),
        resource.target_path().clone(),
    )
    .expect("KnownDirectoryLink already rejects equal source and target paths");
    directory_link_definition_hash(&resolved)
}

fn definition_hash_for_known_resource(
    resource: &KnownResource,
) -> Result<DefinitionHash, CanonicalHashError> {
    match resource {
        KnownResource::FileLink(resource) => definition_hash_for_known(resource),
        KnownResource::FileCopy(resource) => definition_hash_for_copy_known(resource),
        KnownResource::DirectoryLink(resource) => definition_hash_for_directory_known(resource),
    }
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
    InvalidKnownFileCopy(KnownFileCopyError),
    InvalidKnownDirectoryLink(KnownDirectoryLinkError),
    InvalidContentFingerprint {
        resource_id: FullyQualifiedResourceId,
        source: crate::domain::file_copy::ContentFingerprintError,
    },
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
            Self::InvalidKnownFileCopy(error) => error.fmt(formatter),
            Self::InvalidKnownDirectoryLink(error) => error.fmt(formatter),
            Self::InvalidContentFingerprint {
                resource_id,
                source,
            } => write!(
                formatter,
                "invalid content fingerprint for {resource_id}: {source}"
            ),
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
            Self::InvalidKnownFileCopy(error) => Some(error),
            Self::InvalidKnownDirectoryLink(error) => Some(error),
            Self::InvalidContentFingerprint { source, .. } => Some(source),
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

    #[test]
    fn directory_replacement_round_trips_its_temporary_and_exact_predicates() {
        let root = std::env::temp_dir().join("loadout-directory-replacement-codec");
        let old_source = root.join("store/old");
        let new_source = ResolvedPath::new(root.join("store/new")).unwrap();
        let target = root.join("home/.config/nvim");
        let temporary = ResolvedPath::new(root.join("home/.config/.nvim.loadout-tmp")).unwrap();
        let old = ResolvedDirectoryLink::new(
            FullyQualifiedResourceId::parse("base/nvim").unwrap(),
            ResolvedPath::new(old_source.clone()).unwrap(),
            ResolvedPath::new(target.clone()).unwrap(),
        )
        .unwrap();
        let document = json!({
            "schema_version": 3,
            "resources": { "base/nvim": { "definition_hash": directory_link_definition_hash(&old).unwrap().as_str(), "effect": { "kind": "directory_link", "source_path": old.source_path().as_ref(), "target_path": old.target_path().as_ref(), "link_target": old.source_path().as_ref() } } },
            "active_operation": { "id": "op-directory", "desired_hash": format!("sha256:{}", "a".repeat(64)), "actions": { "a1": {
                "kind": "replace_directory_link", "resource_id": "base/nvim",
                "old_effect": { "kind": "directory_link", "source_path": old.source_path().as_ref(), "target_path": old.target_path().as_ref(), "link_target": old.source_path().as_ref() },
                "final_effect": { "kind": "directory_link", "source_path": new_source.as_ref(), "target_path": old.target_path().as_ref(), "link_target": new_source.as_ref() },
                "precondition": { "kind": "expected_directory_link", "target_path": old.target_path().as_ref(), "link_target": old.source_path().as_ref() },
                "postcondition": {
                    "target": { "kind": "expected_directory_link", "target_path": old.target_path().as_ref(), "link_target": new_source.as_ref() },
                    "temporary": { "kind": "missing", "target_path": temporary.as_ref() }
                },
                "temporary": { "path": temporary.as_ref(), "predicate": { "kind": "expected_directory_link", "target_path": temporary.as_ref(), "link_target": new_source.as_ref() } },
                "status": "running"
            } } }
        });
        let decoded: StateDocument = serde_json::from_value(document).unwrap();
        let state = decoded.into_state().unwrap();
        let mut encoded = serde_json::to_value(StateDocument::from_state(&state).unwrap()).unwrap();
        assert_eq!(
            encoded["active_operation"]["actions"]["a1"]["temporary"]["path"],
            temporary.as_ref().to_string_lossy().as_ref()
        );
        assert_eq!(
            encoded["active_operation"]["actions"]["a1"]["precondition"]["kind"],
            "expected_directory_link"
        );
        let mut extra_postcondition_field = encoded.clone();
        extra_postcondition_field["active_operation"]["actions"]["a1"]["postcondition"]["unexpected"] =
            json!(true);
        assert!(serde_json::from_value::<StateDocument>(extra_postcondition_field).is_err());
        encoded["active_operation"]["actions"]["a1"]["temporary"]["predicate"]["link_target"] =
            json!(root.join("store/unexpected"));
        let corrupt: StateDocument = serde_json::from_value(encoded).unwrap();
        assert!(matches!(
            corrupt.into_state(),
            Err(StateDecodeError::InvalidTargetCondition)
        ));
    }

    #[test]
    fn directory_relocation_uses_only_the_exact_old_new_predicate_objects() {
        let root = std::env::temp_dir().join("loadout-directory-relocation-codec");
        let old = KnownDirectoryLink::from_resolved(
            &ResolvedDirectoryLink::new(
                FullyQualifiedResourceId::parse("base/nvim").unwrap(),
                ResolvedPath::new(root.join("store/nvim")).unwrap(),
                ResolvedPath::new(root.join("home/.config/nvim")).unwrap(),
            )
            .unwrap(),
        );
        let final_effect = KnownDirectoryLink::from_resolved(
            &ResolvedDirectoryLink::new(
                FullyQualifiedResourceId::parse("base/nvim").unwrap(),
                ResolvedPath::new(root.join("store/nvim")).unwrap(),
                ResolvedPath::new(root.join("home/.config/nvim-next")).unwrap(),
            )
            .unwrap(),
        );
        let action = RecordedAction::from_persisted_directory_link(
            ActionKind::RelocateDirectoryLink,
            old.resource_id().clone(),
            Some(old.clone()),
            Some(final_effect.clone()),
            None,
            None,
            TargetCondition::ExpectedDirectoryLink {
                target_path: old.target_path().clone(),
                link_target: old.link_target().clone(),
            },
            TargetCondition::ExpectedDirectoryLink {
                target_path: final_effect.target_path().clone(),
                link_target: final_effect.link_target().clone(),
            },
            ActionStatus::Pending,
        )
        .unwrap();
        let operation = OperationRecord::from_actions(
            OperationId::parse("directory-relocation").unwrap(),
            DesiredHash::parse(format!("sha256:{}", "a".repeat(64))).unwrap(),
            [(ActionId::parse("a1").unwrap(), action)],
        )
        .unwrap();
        let state =
            PersistedState::from_parts(KnownState::new([old]).unwrap(), Some(operation)).unwrap();
        let encoded = serde_json::to_value(StateDocument::from_state(&state).unwrap()).unwrap();
        let action = &encoded["active_operation"]["actions"]["a1"];
        assert!(action.get("target_path").is_none());
        assert!(action.get("directory_predicates").is_none());
        assert_eq!(action["precondition"].as_object().unwrap().len(), 2);
        assert_eq!(
            action["precondition"]["old"]["kind"],
            "expected_directory_link"
        );
        assert_eq!(action["precondition"]["new"]["kind"], "missing");
        assert_eq!(action["postcondition"]["old"]["kind"], "missing");
        assert_eq!(
            action["postcondition"]["new"]["kind"],
            "expected_directory_link"
        );
        let mut corrupt = encoded;
        corrupt["active_operation"]["actions"]["a1"]["precondition"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<StateDocument>(corrupt).is_err());
    }

    #[test]
    fn directory_ownership_handoffs_restore_the_old_effect_under_its_old_identity() {
        let root = std::env::temp_dir().join("loadout-directory-handoff-codec");
        for (name, old_source, final_source, temporary) in [
            ("same", "store/nvim", "store/nvim", None),
            (
                "changed",
                "store/nvim-old",
                "store/nvim-new",
                Some("home/.config/.nvim.loadout-tmp"),
            ),
        ] {
            let target = ResolvedPath::new(root.join("home/.config/nvim")).unwrap();
            let old = KnownDirectoryLink::from_resolved(
                &ResolvedDirectoryLink::new(
                    FullyQualifiedResourceId::parse("base/nvim-old").unwrap(),
                    ResolvedPath::new(root.join(old_source)).unwrap(),
                    target.clone(),
                )
                .unwrap(),
            );
            let final_effect = KnownDirectoryLink::from_resolved(
                &ResolvedDirectoryLink::new(
                    FullyQualifiedResourceId::parse("base/nvim-new").unwrap(),
                    ResolvedPath::new(root.join(final_source)).unwrap(),
                    target,
                )
                .unwrap(),
            );
            let action = RecordedAction::from_persisted_directory_link(
                ActionKind::ReplaceOwnership,
                final_effect.resource_id().clone(),
                Some(old.clone()),
                Some(final_effect.clone()),
                Some(old.resource_id().clone()),
                temporary.map(|path| ResolvedPath::new(root.join(path)).unwrap()),
                TargetCondition::ExpectedDirectoryLink {
                    target_path: old.target_path().clone(),
                    link_target: old.link_target().clone(),
                },
                TargetCondition::ExpectedDirectoryLink {
                    target_path: final_effect.target_path().clone(),
                    link_target: final_effect.link_target().clone(),
                },
                ActionStatus::Running,
            )
            .unwrap();
            let operation = OperationRecord::from_actions(
                OperationId::parse(format!("directory-handoff-{name}")).unwrap(),
                DesiredHash::parse(format!("sha256:{}", "a".repeat(64))).unwrap(),
                [(ActionId::parse("a1").unwrap(), action)],
            )
            .unwrap();
            let state = PersistedState::from_parts(
                KnownState::new([old.clone()]).unwrap(),
                Some(operation),
            )
            .unwrap();
            let decoded = StateDocument::from_state(&state)
                .unwrap()
                .into_state()
                .unwrap();
            let facts = decoded
                .active_operation()
                .unwrap()
                .action(&ActionId::parse("a1").unwrap())
                .unwrap()
                .directory_link_facts()
                .unwrap();
            assert_eq!(
                facts.old_effect().unwrap().resource_id(),
                old.resource_id(),
                "{name}"
            );
            assert_eq!(
                facts.final_effect().unwrap().resource_id(),
                final_effect.resource_id(),
                "{name}"
            );
        }
    }

    // Construct the existing wire format directly, independently of the DTO encoder.
    // Paths are platform-local values only; these tests never access the filesystem.
    fn document_fixture(kind: &str, status: &str) -> Value {
        let root = std::env::temp_dir().join("loadout-codec-fixture");
        let source = root.join("store").join("config");
        let target = root.join("home").join(".config");
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
                "effect": {
                    "kind": "file_link",
                    "source_path": source,
                    "target_path": target,
                    "link_target": source,
                },
            });
        }
        json!({
            "schema_version": 3,
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
    fn directory_link_known_effect_round_trips_with_its_typed_hash() {
        let root = std::env::temp_dir().join("loadout-directory-link-codec");
        let resolved = ResolvedDirectoryLink::new(
            FullyQualifiedResourceId::parse("base/nvim").unwrap(),
            ResolvedPath::new(root.join("store/nvim")).unwrap(),
            ResolvedPath::new(root.join("home/.config/nvim")).unwrap(),
        )
        .unwrap();
        let known = KnownDirectoryLink::from_resolved(&resolved);
        let state = PersistedState::from_parts(KnownState::new([known]).unwrap(), None).unwrap();

        let document = StateDocument::from_state(&state).unwrap();
        let encoded = serde_json::to_value(&document).unwrap();
        assert_eq!(encoded["schema_version"], 3);
        assert_eq!(
            encoded["resources"]["base/nvim"]["effect"]["kind"],
            "directory_link"
        );
        assert_eq!(document.into_state().unwrap(), state);
    }

    #[test]
    fn active_relocate_copy_round_trips_its_old_and_new_target_predicates() {
        let root = std::env::temp_dir().join("loadout-codec-relocate-copy-operation");
        let resource_id = FullyQualifiedResourceId::parse("base/config").unwrap();
        let old_target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let new_target = ResolvedPath::new(root.join("home/.config/config")).unwrap();
        let old_source = ResolvedPath::new(root.join("store/old-config")).unwrap();
        let new_source = ResolvedPath::new(root.join("store/new-config")).unwrap();
        let old_fingerprint =
            ContentFingerprint::parse(format!("sha256:{}", "b".repeat(64))).unwrap();
        let new_fingerprint =
            ContentFingerprint::parse(format!("sha256:{}", "c".repeat(64))).unwrap();
        let old_copy = KnownFileCopy::new(
            resource_id.clone(),
            old_source.clone(),
            old_target.clone(),
            old_fingerprint.clone(),
        )
        .unwrap();
        let definition_hash = definition_hash_for_copy_known(&old_copy).unwrap();
        let document = json!({
            "schema_version": 3,
            "resources": {"base/config": {
                "definition_hash": definition_hash.as_str(),
                "effect": {
                    "kind": "file_copy",
                    "source_path": old_source.as_ref(),
                    "target_path": old_target.as_ref(),
                    "content_fingerprint": old_fingerprint.as_str(),
                },
            }},
            "active_operation": {
                "id": "op-relocate-copy",
                "desired_hash": format!("sha256:{}", "a".repeat(64)),
                "actions": {"a1": {
                    "kind": "relocate_copy",
                    "resource_id": "base/config",
                    "old_target_path": old_target.as_ref(),
                    "target_path": new_target.as_ref(),
                    "source_path": new_source.as_ref(),
                    "content_fingerprint": new_fingerprint.as_str(),
                    "temporary_path": root.join("home/.config/.loadout-copy-a1"),
                    "old_effect": {
                        "kind": "file_copy",
                        "source_path": old_source.as_ref(),
                        "target_path": old_target.as_ref(),
                        "content_fingerprint": old_fingerprint.as_str(),
                    },
                    "final_effect": {
                        "kind": "file_copy",
                        "source_path": new_source.as_ref(),
                        "target_path": new_target.as_ref(),
                        "content_fingerprint": new_fingerprint.as_str(),
                    },
                    "precondition": {
                        "target": "expected_copy",
                        "content_fingerprint": old_fingerprint.as_str(),
                    },
                    "postcondition": {
                        "target": "expected_copy",
                        "content_fingerprint": new_fingerprint.as_str(),
                    },
                    "temporary_staged": false,
                    "publication_attempted": false,
                    "status": "running",
                }},
            },
        });

        let state = serde_json::from_value::<StateDocument>(document)
            .unwrap()
            .into_state()
            .unwrap();
        let encoded = serde_json::to_value(StateDocument::from_state(&state).unwrap()).unwrap();
        assert_eq!(
            encoded["active_operation"]["actions"]["a1"]["old_target_path"],
            json!(old_target.as_ref())
        );
        let round_tripped = serde_json::from_value::<StateDocument>(encoded)
            .unwrap()
            .into_state()
            .unwrap();
        let action = round_tripped
            .active_operation()
            .unwrap()
            .action(&ActionId::parse("a1").unwrap())
            .unwrap();
        assert_eq!(action.kind(), ActionKind::RelocateCopy);
        assert_eq!(
            action.precondition(),
            TargetCondition::ExpectedCopy {
                target_path: old_target,
                content_fingerprint: old_fingerprint,
            }
        );
        assert_eq!(
            action.postcondition(),
            TargetCondition::ExpectedCopy {
                target_path: new_target,
                content_fingerprint: new_fingerprint,
            }
        );
    }

    #[test]
    fn active_create_copy_decodes_as_typed_operation_facts_and_round_trips() {
        let root = std::env::temp_dir().join("loadout-codec-copy-operation");
        let source = root.join("store").join("config");
        let target = root.join("home").join(".config");
        let fingerprint = format!("sha256:{}", "b".repeat(64));
        let document = json!({
            "schema_version": 3,
            "resources": {},
            "active_operation": {
                "id": "op-copy",
                "desired_hash": format!("sha256:{}", "a".repeat(64)),
                "actions": {
                    "a1": {
                        "kind": "create_copy",
                        "resource_id": "base/config",
                        "target_path": target,
                        "source_path": source,
                        "content_fingerprint": fingerprint,
                        "temporary_path": root.join("home").join(".loadout-copy-a1"),
                        "final_effect": {
                            "kind": "file_copy",
                            "source_path": root.join("store").join("config"),
                            "target_path": root.join("home").join(".config"),
                            "content_fingerprint": format!("sha256:{}", "b".repeat(64))
                        },
                        "precondition": { "target": "missing" },
                        "postcondition": {
                            "target": "expected_copy",
                            "content_fingerprint": format!("sha256:{}", "b".repeat(64))
                        },
                        "temporary_staged": false,
                        "publication_attempted": false,
                        "status": "running"
                    }
                }
            }
        });

        let decoded: StateDocument = serde_json::from_value(document).unwrap();
        let state = decoded.into_state().unwrap();
        let action = state
            .active_operation()
            .unwrap()
            .action(&ActionId::parse("a1").unwrap())
            .unwrap();
        assert_eq!(action.kind(), ActionKind::CreateCopy);
        assert!(matches!(
            action.postcondition(),
            TargetCondition::ExpectedCopy { .. }
        ));

        let encoded = StateDocument::from_state(&state).unwrap();
        let value = serde_json::to_value(encoded).unwrap();
        assert_eq!(
            value["active_operation"]["actions"]["a1"]["kind"],
            "create_copy"
        );
    }

    #[test]
    fn active_copy_without_its_recorded_temporary_is_rejected() {
        let root = std::env::temp_dir().join("loadout-codec-copy-operation-missing-temp");
        let document = json!({
            "schema_version": 3,
            "resources": {},
            "active_operation": {
                "id": "op-copy",
                "desired_hash": format!("sha256:{}", "a".repeat(64)),
                "actions": { "a1": {
                    "kind": "create_copy", "resource_id": "base/config",
                    "target_path": root.join("home/.config"),
                    "source_path": root.join("store/config"),
                    "content_fingerprint": format!("sha256:{}", "b".repeat(64)),
                    "final_effect": {
                        "kind": "file_copy",
                        "source_path": root.join("store/config"),
                        "target_path": root.join("home/.config"),
                        "content_fingerprint": format!("sha256:{}", "b".repeat(64))
                    },
                    "precondition": { "target": "missing" },
                    "postcondition": { "target": "expected_copy", "content_fingerprint": format!("sha256:{}", "b".repeat(64)) },
                    "status": "pending"
                }}
            }
        });
        let decoded: StateDocument = serde_json::from_value(document).unwrap();
        assert!(matches!(
            decoded.into_state(),
            Err(StateDecodeError::InvalidTargetCondition)
        ));
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
    fn file_copy_known_effect_round_trips_and_rejects_a_noncanonical_fingerprint() {
        let root = std::env::temp_dir().join("loadout-copy-codec-fixture");
        let source = ResolvedPath::new(root.join("store/config")).unwrap();
        let target = ResolvedPath::new(root.join("home/.config")).unwrap();
        let fingerprint = ContentFingerprint::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
        let copy = KnownFileCopy::new(
            FullyQualifiedResourceId::parse("base/config").unwrap(),
            source.clone(),
            target.clone(),
            fingerprint.clone(),
        )
        .unwrap();
        let definition = definition_hash_for_copy_known(&copy).unwrap();
        let fixture = json!({
            "schema_version": 3,
            "resources": {"base/config": {
                "definition_hash": definition.as_str(),
                "effect": {"kind": "file_copy", "source_path": source.as_ref(), "target_path": target.as_ref(), "content_fingerprint": fingerprint.as_str()}
            }},
            "active_operation": null
        });

        let state = serde_json::from_value::<StateDocument>(fixture.clone())
            .unwrap()
            .into_state()
            .unwrap();
        assert!(matches!(
            state
                .known()
                .get_variant(&FullyQualifiedResourceId::parse("base/config").unwrap()),
            Some(KnownResource::FileCopy(_))
        ));
        assert_eq!(
            serde_json::to_value(StateDocument::from_state(&state).unwrap()).unwrap(),
            fixture
        );

        let mut invalid = fixture;
        invalid["resources"]["base/config"]["effect"]["content_fingerprint"] = json!("sha256:ABC");
        assert!(matches!(
            serde_json::from_value::<StateDocument>(invalid)
                .unwrap()
                .into_state(),
            Err(StateDecodeError::InvalidContentFingerprint { .. })
        ));
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_state_object_level() {
        for pointer in [
            "",
            "/resources/base~1config",
            "/resources/base~1config/effect",
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

    #[cfg(windows)]
    #[test]
    fn windows_state_paths_require_the_normal_v1_representation() {
        assert_eq!(
            decode_path("C:/Users/Example/file".to_owned())
                .unwrap()
                .as_path(),
            std::path::Path::new(r"C:\Users\Example\file")
        );
        for value in [
            r"\\?\C:\Users\Example\file",
            r"\\?\UNC\server\share\file",
            r"\\.\C:\Users\Example\file",
            r"C:\Users\CON",
            "C:\\Users\\Example\\file\u{001F}",
        ] {
            assert!(
                matches!(
                    decode_path(value.to_owned()),
                    Err(StateDecodeError::InvalidPath { .. })
                ),
                "{value:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_nonconforming_v1_known_and_active_operation_paths_are_rejected() {
        let mut known_fixture = document_fixture("create_link", "succeeded");
        known_fixture["resources"]["base/config"]["effect"]["target_path"] =
            json!(r"\\?\C:\Users\Example\target");
        let known_document = serde_json::from_value::<StateDocument>(known_fixture).unwrap();
        assert!(matches!(
            known_document.into_state(),
            Err(StateDecodeError::InvalidPath { .. })
        ));

        let mut active_fixture = document_fixture("create_link", "running");
        active_fixture["active_operation"]["actions"]["a1"]["target_path"] =
            json!(r"\\.\C:\Users\Example\target");
        let active_document = serde_json::from_value::<StateDocument>(active_fixture).unwrap();
        assert!(matches!(
            active_document.into_state(),
            Err(StateDecodeError::InvalidPath { .. })
        ));

        let mut control_character_fixture = document_fixture("create_link", "succeeded");
        control_character_fixture["resources"]["base/config"]["effect"]["target_path"] =
            json!("C:\\Users\\Example\\target\u{0001}");
        let control_character_document =
            serde_json::from_value::<StateDocument>(control_character_fixture).unwrap();
        assert!(matches!(
            control_character_document.into_state(),
            Err(StateDecodeError::InvalidPath { .. })
        ));
    }
}
