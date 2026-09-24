//! Immutable executor-ready action plans derived by the pure planner.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::desired::ResolvedResource;
use crate::domain::diagnostic::Diagnostic;
use crate::domain::file_copy::ResolvedFileCopy;
use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{KnownFileCopy, KnownFileLink, KnownResource};
use crate::domain::paths::ResolvedPath;

/// The file-link action chosen by the planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionKind {
    CreateLink,
    ReplaceLink,
    RelocateLink,
    ReplaceOwnership,
    RemoveLink,
    ForgetMissing,
    CreateCopy,
    ReplaceCopy,
    RelocateCopy,
    RemoveCopy,
    ReplaceEffect,
    Noop,
}

/// The closed file-copy action vocabulary reserved for the copy planner and executor slice.
///
/// These values are intentionally separate from `PlannedAction` until M3 can provide every required precondition, temporary, recovery, and executor implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlannedFileCopyAction {
    Create {
        desired: ResolvedFileCopy,
    },
    Replace {
        desired: ResolvedFileCopy,
        previous: crate::domain::known::KnownFileCopy,
    },
    Relocate {
        desired: ResolvedFileCopy,
        previous: crate::domain::known::KnownFileCopy,
    },
    Remove {
        previous: crate::domain::known::KnownFileCopy,
    },
    ForgetMissing {
        previous: crate::domain::known::KnownFileCopy,
    },
    Noop {
        desired: ResolvedFileCopy,
        previous: crate::domain::known::KnownFileCopy,
    },
}

impl PlannedFileCopyAction {
    /// Returns the closed action kind represented by this copy payload.
    pub(crate) fn kind(&self) -> ActionKind {
        match self {
            Self::Create { .. } => ActionKind::CreateCopy,
            Self::Replace { .. } => ActionKind::ReplaceCopy,
            Self::Relocate { .. } => ActionKind::RelocateCopy,
            Self::Remove { .. } => ActionKind::RemoveCopy,
            Self::ForgetMissing { .. } => ActionKind::ForgetMissing,
            Self::Noop { .. } => ActionKind::Noop,
        }
    }

    /// Returns the stable resource identity that participates in deterministic ordering.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match self {
            Self::Create { desired }
            | Self::Replace { desired, .. }
            | Self::Relocate { desired, .. }
            | Self::Noop { desired, .. } => desired.resource_id(),
            Self::Remove { previous } | Self::ForgetMissing { previous } => previous.resource_id(),
        }
    }

    /// Returns the desired copy definition for actions that materialize a final copy.
    pub(crate) fn desired(&self) -> Option<&ResolvedFileCopy> {
        match self {
            Self::Create { desired }
            | Self::Replace { desired, .. }
            | Self::Relocate { desired, .. }
            | Self::Noop { desired, .. } => Some(desired),
            Self::Remove { .. } | Self::ForgetMissing { .. } => None,
        }
    }

    /// Returns the prior owned copy fact for actions that require one.
    pub(crate) fn previous(&self) -> Option<&KnownFileCopy> {
        match self {
            Self::Replace { previous, .. }
            | Self::Relocate { previous, .. }
            | Self::Remove { previous }
            | Self::ForgetMissing { previous }
            | Self::Noop { previous, .. } => Some(previous),
            Self::Create { .. } => None,
        }
    }

    /// Returns the exact no-follow predicates the executor must recheck before mutation.
    pub(crate) fn preconditions(&self) -> Vec<TargetCondition> {
        match self {
            Self::Create { desired } => vec![missing(desired.target_path())],
            Self::Replace { previous, .. } | Self::Remove { previous } => {
                vec![expected_copy(previous)]
            }
            Self::Relocate { desired, previous } => {
                vec![expected_copy(previous), missing(desired.target_path())]
            }
            Self::ForgetMissing { previous } => vec![missing(previous.target_path())],
            Self::Noop { previous, .. } => vec![expected_copy(previous)],
        }
    }

    /// Returns the exact predicates required before Known state can be changed.
    pub(crate) fn postconditions(&self) -> Vec<TargetCondition> {
        match self {
            Self::Create { desired }
            | Self::Replace { desired, .. }
            | Self::Noop { desired, .. } => vec![expected_desired_copy(desired)],
            Self::Relocate { desired, previous } => {
                vec![
                    missing(previous.target_path()),
                    expected_desired_copy(desired),
                ]
            }
            Self::Remove { previous } | Self::ForgetMissing { previous } => {
                vec![missing(previous.target_path())]
            }
        }
    }

    /// Returns the typed state transition that becomes eligible after the postcondition.
    pub(crate) fn known_state_update(&self) -> Option<KnownStateUpdate> {
        match self {
            Self::Create { desired }
            | Self::Replace { desired, .. }
            | Self::Relocate { desired, .. } => Some(KnownStateUpdate::UpsertCopy {
                resource: KnownFileCopy::from_resolved(desired),
            }),
            Self::Remove { previous } | Self::ForgetMissing { previous } => {
                Some(KnownStateUpdate::Remove {
                    resource_id: previous.resource_id().clone(),
                })
            }
            Self::Noop { .. } => None,
        }
    }

    pub(crate) fn touched_targets(&self) -> Vec<&ResolvedPath> {
        match self {
            Self::Create { desired }
            | Self::Replace { desired, .. }
            | Self::Noop { desired, .. } => vec![desired.target_path()],
            Self::Relocate { desired, previous } => {
                vec![previous.target_path(), desired.target_path()]
            }
            Self::Remove { previous } | Self::ForgetMissing { previous } => {
                vec![previous.target_path()]
            }
        }
    }
}

/// The planner reason attached to a selected action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActionReason {
    TargetMissing,
    SourceChanged,
    TargetChanged,
    ManagedIdentityHandoff,
    StaleResource,
    StaleResourceTargetMissing,
    AlreadySatisfied,
}

/// One resolved target predicate required before or after an action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TargetCondition {
    Missing {
        target_path: ResolvedPath,
    },
    ExpectedLink {
        target_path: ResolvedPath,
        link_target: LinkTarget,
    },
    ExpectedCopy {
        target_path: ResolvedPath,
        content_fingerprint: crate::domain::file_copy::ContentFingerprint,
    },
}

impl TargetCondition {
    /// The target path governed by this condition.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match self {
            Self::Missing { target_path }
            | Self::ExpectedLink { target_path, .. }
            | Self::ExpectedCopy { target_path, .. } => target_path,
        }
    }
}

/// The exact Known-state transition that becomes eligible after verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KnownStateUpdate {
    Upsert {
        resource: KnownFileLink,
    },
    UpsertCopy {
        resource: KnownFileCopy,
    },
    Remove {
        resource_id: FullyQualifiedResourceId,
    },
    ReplaceIdentity {
        old_resource_id: FullyQualifiedResourceId,
        new_resource: KnownFileLink,
    },
}

/// The closed Known-state transition selected for any resource action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResourceKnownStateUpdate {
    Upsert {
        resource: KnownResource,
    },
    Remove {
        resource_id: FullyQualifiedResourceId,
    },
    ReplaceIdentity {
        old_resource_id: FullyQualifiedResourceId,
        new_resource: KnownResource,
    },
}

impl From<KnownStateUpdate> for ResourceKnownStateUpdate {
    fn from(update: KnownStateUpdate) -> Self {
        match update {
            KnownStateUpdate::Upsert { resource } => Self::Upsert {
                resource: resource.into(),
            },
            KnownStateUpdate::UpsertCopy { resource } => Self::Upsert {
                resource: resource.into(),
            },
            KnownStateUpdate::Remove { resource_id } => Self::Remove { resource_id },
            KnownStateUpdate::ReplaceIdentity {
                old_resource_id,
                new_resource,
            } => Self::ReplaceIdentity {
                old_resource_id,
                new_resource: new_resource.into(),
            },
        }
    }
}

/// One complete action; its payload is private so every crate caller must use a validated constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedAction {
    inner: PlannedActionInner,
}

/// A closed executor-plan member. The legacy link payload stays distinct while copy planning gains a typed path into the aggregate plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlannedResourceAction {
    FileLink(PlannedAction),
    FileCopy(PlannedFileCopyAction),
    ReplaceEffect(PlannedEffectHandoff),
}

/// A managed same-target handoff between the two closed file effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedEffectHandoff {
    old_effect: KnownResource,
    final_effect: ResolvedResource,
}

impl PlannedEffectHandoff {
    pub(crate) fn new(
        old_effect: KnownResource,
        final_effect: ResolvedResource,
    ) -> Result<Self, PlannedActionError> {
        if old_effect.target_path() != final_effect.target_path() {
            return Err(PlannedActionError::TargetPathMismatch);
        }
        if old_effect.resource_id() != final_effect.resource_id() {
            return Err(PlannedActionError::ResourceIdMismatch);
        }
        Ok(Self {
            old_effect,
            final_effect,
        })
    }
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        self.final_effect.resource_id()
    }
    /// Returns the complete old owned effect required by the executor precondition.
    pub(crate) fn old_effect(&self) -> &KnownResource {
        &self.old_effect
    }
    /// Returns the exact final resolved effect required by the executor postcondition.
    pub(crate) fn final_effect(&self) -> &ResolvedResource {
        &self.final_effect
    }
    pub(crate) fn touched_targets(&self) -> Vec<&ResolvedPath> {
        vec![self.final_effect.target_path()]
    }

    /// Returns the exact old managed effect required before the handoff can start.
    pub(crate) fn preconditions(&self) -> Vec<TargetCondition> {
        vec![condition_for_known_effect(&self.old_effect)]
    }

    /// Returns the exact final effect that must be verified before Known state changes.
    pub(crate) fn postconditions(&self) -> Vec<TargetCondition> {
        vec![condition_for_resolved_effect(&self.final_effect)]
    }

    /// Returns the final effect that replaces the old Known fact after verification.
    pub(crate) fn known_state_update(&self) -> ResourceKnownStateUpdate {
        ResourceKnownStateUpdate::Upsert {
            resource: known_from_resolved_effect(&self.final_effect),
        }
    }
}

impl PlannedResourceAction {
    pub(crate) fn kind(&self) -> ActionKind {
        match self {
            Self::FileLink(action) => action.kind(),
            Self::FileCopy(action) => action.kind(),
            Self::ReplaceEffect(_) => ActionKind::ReplaceEffect,
        }
    }

    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match self {
            Self::FileLink(action) => action.resource_id(),
            Self::FileCopy(action) => action.resource_id(),
            Self::ReplaceEffect(action) => action.resource_id(),
        }
    }

    pub(crate) fn touched_targets(&self) -> Vec<&ResolvedPath> {
        match self {
            Self::FileLink(action) => action.touched_targets(),
            Self::FileCopy(action) => action.touched_targets(),
            Self::ReplaceEffect(action) => action.touched_targets(),
        }
    }

    /// Returns the exact predicates the common coordinator must recheck before execution.
    pub(crate) fn preconditions(&self) -> Vec<TargetCondition> {
        match self {
            Self::FileLink(action) => action.preconditions(),
            Self::FileCopy(action) => action.preconditions(),
            Self::ReplaceEffect(action) => action.preconditions(),
        }
    }

    /// Returns the exact predicates required before the common coordinator commits Known state.
    pub(crate) fn postconditions(&self) -> Vec<TargetCondition> {
        match self {
            Self::FileLink(action) => action.postconditions(),
            Self::FileCopy(action) => action.postconditions(),
            Self::ReplaceEffect(action) => action.postconditions(),
        }
    }

    /// Returns the complete Known-state transition eligible after postcondition verification.
    pub(crate) fn known_state_update(&self) -> Option<ResourceKnownStateUpdate> {
        match self {
            Self::FileLink(action) => action.known_state_update().map(Into::into),
            Self::FileCopy(action) => action.known_state_update().map(Into::into),
            Self::ReplaceEffect(action) => Some(action.known_state_update()),
        }
    }

    /// Returns the stale identity replaced by a same-effect ownership handoff, if any.
    pub(crate) fn replaced_resource_id(&self) -> Option<&FullyQualifiedResourceId> {
        match self {
            Self::FileLink(action) => action.replaced_resource_id(),
            Self::FileCopy(_) | Self::ReplaceEffect(_) => None,
        }
    }
}

impl From<PlannedAction> for PlannedResourceAction {
    fn from(action: PlannedAction) -> Self {
        Self::FileLink(action)
    }
}

impl From<PlannedFileCopyAction> for PlannedResourceAction {
    fn from(action: PlannedFileCopyAction) -> Self {
        Self::FileCopy(action)
    }
}

impl From<PlannedEffectHandoff> for PlannedResourceAction {
    fn from(action: PlannedEffectHandoff) -> Self {
        Self::ReplaceEffect(action)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PlannedActionInner {
    CreateLink {
        desired: ResolvedFileLink,
    },
    ReplaceLink {
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    },
    RelocateLink {
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    },
    ReplaceOwnership {
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    },
    RemoveLink {
        previous: KnownFileLink,
    },
    ForgetMissing {
        previous: KnownFileLink,
    },
    Noop {
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    },
}

impl PlannedAction {
    /// Selects creation after a target has been observed missing.
    pub(crate) fn create_link(desired: ResolvedFileLink) -> Self {
        Self {
            inner: PlannedActionInner::CreateLink { desired },
        }
    }

    /// Selects a same-target replacement after an expected managed link was observed.
    pub(crate) fn replace_link(
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    ) -> Result<Self, PlannedActionError> {
        require_same_resource_id(&desired, &previous)?;
        require_same_target(&desired, &previous)?;
        if desired.link_target() == previous.link_target() {
            return Err(PlannedActionError::LinkTargetUnchanged);
        }

        Ok(Self {
            inner: PlannedActionInner::ReplaceLink { desired, previous },
        })
    }

    /// Selects relocation when one stable identity changes to another target.
    pub(crate) fn relocate_link(
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    ) -> Result<Self, PlannedActionError> {
        require_same_resource_id(&desired, &previous)?;
        if desired.target_path() == previous.target_path() {
            return Err(PlannedActionError::TargetPathUnchanged);
        }

        Ok(Self {
            inner: PlannedActionInner::RelocateLink { desired, previous },
        })
    }

    /// Selects an internal handoff from one managed identity to a distinct new identity.
    pub(crate) fn replace_ownership(
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    ) -> Result<Self, PlannedActionError> {
        if desired.resource_id() == previous.resource_id() {
            return Err(PlannedActionError::ResourceIdsEqual);
        }
        require_same_target(&desired, &previous)?;

        Ok(Self {
            inner: PlannedActionInner::ReplaceOwnership { desired, previous },
        })
    }

    /// Selects deletion of one stale link whose ownership was proven by Actual state.
    pub(crate) fn remove_link(previous: KnownFileLink) -> Self {
        Self {
            inner: PlannedActionInner::RemoveLink { previous },
        }
    }

    /// Selects removal of a stale Known fact with no filesystem mutation.
    pub(crate) fn forget_missing(previous: KnownFileLink) -> Self {
        Self {
            inner: PlannedActionInner::ForgetMissing { previous },
        }
    }

    /// Records that Desired and Known definitions already converge at the target.
    pub(crate) fn noop(
        desired: ResolvedFileLink,
        previous: KnownFileLink,
    ) -> Result<Self, PlannedActionError> {
        require_same_resource_id(&desired, &previous)?;
        require_same_target(&desired, &previous)?;
        if desired.source_path() != previous.source_path()
            || desired.link_target() != previous.link_target()
        {
            return Err(PlannedActionError::DefinitionChanged);
        }

        Ok(Self {
            inner: PlannedActionInner::Noop { desired, previous },
        })
    }

    /// Returns the planner-selected kind without exposing mutable action payloads.
    pub(crate) fn kind(&self) -> ActionKind {
        match &self.inner {
            PlannedActionInner::CreateLink { .. } => ActionKind::CreateLink,
            PlannedActionInner::ReplaceLink { .. } => ActionKind::ReplaceLink,
            PlannedActionInner::RelocateLink { .. } => ActionKind::RelocateLink,
            PlannedActionInner::ReplaceOwnership { .. } => ActionKind::ReplaceOwnership,
            PlannedActionInner::RemoveLink { .. } => ActionKind::RemoveLink,
            PlannedActionInner::ForgetMissing { .. } => ActionKind::ForgetMissing,
            PlannedActionInner::Noop { .. } => ActionKind::Noop,
        }
    }

    /// Returns the transition-table reason implied by this action.
    pub(crate) fn reason(&self) -> ActionReason {
        match &self.inner {
            PlannedActionInner::CreateLink { .. } => ActionReason::TargetMissing,
            PlannedActionInner::ReplaceLink { .. } => ActionReason::SourceChanged,
            PlannedActionInner::RelocateLink { .. } => ActionReason::TargetChanged,
            PlannedActionInner::ReplaceOwnership { .. } => ActionReason::ManagedIdentityHandoff,
            PlannedActionInner::RemoveLink { .. } => ActionReason::StaleResource,
            PlannedActionInner::ForgetMissing { .. } => ActionReason::StaleResourceTargetMissing,
            PlannedActionInner::Noop { .. } => ActionReason::AlreadySatisfied,
        }
    }

    /// The fully qualified resource ID that provides the action's deterministic key.
    pub(crate) fn resource_id(&self) -> &FullyQualifiedResourceId {
        match &self.inner {
            PlannedActionInner::CreateLink { desired }
            | PlannedActionInner::ReplaceLink { desired, .. }
            | PlannedActionInner::RelocateLink { desired, .. }
            | PlannedActionInner::ReplaceOwnership { desired, .. }
            | PlannedActionInner::Noop { desired, .. } => desired.resource_id(),
            PlannedActionInner::RemoveLink { previous }
            | PlannedActionInner::ForgetMissing { previous } => previous.resource_id(),
        }
    }

    /// Returns the stale identity replaced by an ownership handoff, if any.
    pub(crate) fn replaced_resource_id(&self) -> Option<&FullyQualifiedResourceId> {
        match &self.inner {
            PlannedActionInner::ReplaceOwnership { previous, .. } => Some(previous.resource_id()),
            PlannedActionInner::CreateLink { .. }
            | PlannedActionInner::ReplaceLink { .. }
            | PlannedActionInner::RelocateLink { .. }
            | PlannedActionInner::RemoveLink { .. }
            | PlannedActionInner::ForgetMissing { .. }
            | PlannedActionInner::Noop { .. } => None,
        }
    }

    /// The exact no-follow predicates that must hold immediately before execution.
    pub(crate) fn preconditions(&self) -> Vec<TargetCondition> {
        match &self.inner {
            PlannedActionInner::CreateLink { desired } => vec![missing(desired.target_path())],
            PlannedActionInner::ReplaceLink { previous, .. }
            | PlannedActionInner::ReplaceOwnership { previous, .. }
            | PlannedActionInner::RemoveLink { previous } => vec![expected(previous)],
            PlannedActionInner::RelocateLink { desired, previous } => {
                vec![expected(previous), missing(desired.target_path())]
            }
            PlannedActionInner::ForgetMissing { previous } => vec![missing(previous.target_path())],
            PlannedActionInner::Noop { previous, .. } => vec![expected(previous)],
        }
    }

    /// The exact no-follow predicates that must hold before Known state may change.
    pub(crate) fn postconditions(&self) -> Vec<TargetCondition> {
        match &self.inner {
            PlannedActionInner::CreateLink { desired }
            | PlannedActionInner::ReplaceLink { desired, .. }
            | PlannedActionInner::ReplaceOwnership { desired, .. }
            | PlannedActionInner::Noop { desired, .. } => vec![expected_desired(desired)],
            PlannedActionInner::RelocateLink { desired, previous } => {
                vec![missing(previous.target_path()), expected_desired(desired)]
            }
            PlannedActionInner::RemoveLink { previous }
            | PlannedActionInner::ForgetMissing { previous } => {
                vec![missing(previous.target_path())]
            }
        }
    }

    /// The complete Known-state update eligible only after the post-condition holds.
    pub(crate) fn known_state_update(&self) -> Option<KnownStateUpdate> {
        match &self.inner {
            PlannedActionInner::CreateLink { desired }
            | PlannedActionInner::ReplaceLink { desired, .. }
            | PlannedActionInner::RelocateLink { desired, .. } => Some(KnownStateUpdate::Upsert {
                resource: KnownFileLink::from_resolved(desired),
            }),
            PlannedActionInner::ReplaceOwnership { desired, previous } => {
                Some(KnownStateUpdate::ReplaceIdentity {
                    old_resource_id: previous.resource_id().clone(),
                    new_resource: KnownFileLink::from_resolved(desired),
                })
            }
            PlannedActionInner::RemoveLink { previous }
            | PlannedActionInner::ForgetMissing { previous } => Some(KnownStateUpdate::Remove {
                resource_id: previous.resource_id().clone(),
            }),
            PlannedActionInner::Noop { .. } => None,
        }
    }

    fn touched_targets(&self) -> Vec<&ResolvedPath> {
        match &self.inner {
            PlannedActionInner::CreateLink { desired }
            | PlannedActionInner::ReplaceLink { desired, .. }
            | PlannedActionInner::ReplaceOwnership { desired, .. }
            | PlannedActionInner::Noop { desired, .. } => vec![desired.target_path()],
            PlannedActionInner::RelocateLink { desired, previous } => {
                vec![previous.target_path(), desired.target_path()]
            }
            PlannedActionInner::RemoveLink { previous }
            | PlannedActionInner::ForgetMissing { previous } => {
                vec![previous.target_path()]
            }
        }
    }
}

fn missing(target_path: &ResolvedPath) -> TargetCondition {
    TargetCondition::Missing {
        target_path: target_path.clone(),
    }
}

fn expected(known: &KnownFileLink) -> TargetCondition {
    TargetCondition::ExpectedLink {
        target_path: known.target_path().clone(),
        link_target: known.link_target().clone(),
    }
}

fn expected_desired(desired: &ResolvedFileLink) -> TargetCondition {
    TargetCondition::ExpectedLink {
        target_path: desired.target_path().clone(),
        link_target: desired.link_target().clone(),
    }
}

fn expected_copy(known: &KnownFileCopy) -> TargetCondition {
    TargetCondition::ExpectedCopy {
        target_path: known.target_path().clone(),
        content_fingerprint: known.content_fingerprint().clone(),
    }
}

fn expected_desired_copy(desired: &ResolvedFileCopy) -> TargetCondition {
    TargetCondition::ExpectedCopy {
        target_path: desired.target_path().clone(),
        content_fingerprint: desired.source_content_fingerprint().clone(),
    }
}

fn condition_for_known_effect(effect: &KnownResource) -> TargetCondition {
    match effect {
        KnownResource::FileLink(resource) => expected(resource),
        KnownResource::FileCopy(resource) => expected_copy(resource),
    }
}

fn condition_for_resolved_effect(effect: &ResolvedResource) -> TargetCondition {
    match effect {
        ResolvedResource::FileLink(resource) => expected_desired(resource),
        ResolvedResource::FileCopy(resource) => expected_desired_copy(resource),
    }
}

fn known_from_resolved_effect(effect: &ResolvedResource) -> KnownResource {
    match effect {
        ResolvedResource::FileLink(resource) => KnownFileLink::from_resolved(resource).into(),
        ResolvedResource::FileCopy(resource) => KnownFileCopy::from_resolved(resource).into(),
    }
}

fn require_same_resource_id(
    desired: &ResolvedFileLink,
    previous: &KnownFileLink,
) -> Result<(), PlannedActionError> {
    if desired.resource_id() == previous.resource_id() {
        Ok(())
    } else {
        Err(PlannedActionError::ResourceIdMismatch)
    }
}

fn require_same_target(
    desired: &ResolvedFileLink,
    previous: &KnownFileLink,
) -> Result<(), PlannedActionError> {
    if desired.target_path() == previous.target_path() {
        Ok(())
    } else {
        Err(PlannedActionError::TargetPathMismatch)
    }
}

/// The reason one action payload cannot represent its selected transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlannedActionError {
    ResourceIdMismatch,
    ResourceIdsEqual,
    TargetPathMismatch,
    TargetPathUnchanged,
    LinkTargetUnchanged,
    DefinitionChanged,
}

impl fmt::Display for PlannedActionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceIdMismatch => formatter.write_str(
                "this action requires Desired and Known records with the same resource ID",
            ),
            Self::ResourceIdsEqual => formatter
                .write_str("replace ownership requires distinct old and new resource identities"),
            Self::TargetPathMismatch => formatter.write_str(
                "this action requires Desired and Known records with the same target path",
            ),
            Self::TargetPathUnchanged => {
                formatter.write_str("relocate link requires different old and new target paths")
            }
            Self::LinkTargetUnchanged => {
                formatter.write_str("replace link requires a changed resolved link target")
            }
            Self::DefinitionChanged => {
                formatter.write_str("noop requires equal Desired and Known file-link definitions")
            }
        }
    }
}

impl std::error::Error for PlannedActionError {}

/// An immutable plan that is executable only when it has no blocking diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Plan {
    actions: Vec<PlannedAction>,
    resource_actions: Vec<PlannedResourceAction>,
    diagnostics: Vec<Diagnostic>,
}

impl Plan {
    /// Builds a plan while rejecting multiple actions that touch the same target.
    pub(crate) fn new(
        actions: impl IntoIterator<Item = PlannedAction>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Result<Self, PlanError> {
        let actions = actions.into_iter().collect::<Vec<_>>();
        Self::from_resource_actions(
            actions.iter().cloned().map(PlannedResourceAction::from),
            diagnostics,
        )
    }

    /// Builds a closed mixed-effect plan. Link-only orchestration continues to use [`Self::new`] until the executor owns every copy action.
    pub(crate) fn new_with_resource_actions(
        actions: impl IntoIterator<Item = PlannedResourceAction>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Result<Self, PlanError> {
        Self::from_resource_actions(actions, diagnostics)
    }

    fn from_resource_actions(
        actions: impl IntoIterator<Item = PlannedResourceAction>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Result<Self, PlanError> {
        let resource_actions = actions.into_iter().collect::<Vec<_>>();
        let mut claimed_targets = BTreeMap::new();

        for action in &resource_actions {
            for target_path in action.touched_targets() {
                if let Some(first_resource_id) =
                    claimed_targets.insert(target_path.clone(), action.resource_id().clone())
                {
                    return Err(PlanError::DuplicateActionTarget {
                        target_path: target_path.clone(),
                        first_resource_id,
                        duplicate_resource_id: action.resource_id().clone(),
                    });
                }
            }
        }

        let actions = resource_actions
            .iter()
            .filter_map(|action| match action {
                PlannedResourceAction::FileLink(action) => Some(action.clone()),
                PlannedResourceAction::FileCopy(_) | PlannedResourceAction::ReplaceEffect(_) => {
                    None
                }
            })
            .collect();

        Ok(Self {
            actions,
            resource_actions,
            diagnostics: diagnostics.into_iter().collect(),
        })
    }

    /// Returns executor-ready actions without exposing mutable access.
    pub(crate) fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    /// Returns every closed resource action, including copy actions not yet accepted by the link-only executor.
    pub(crate) fn resource_actions(&self) -> &[PlannedResourceAction] {
        &self.resource_actions
    }

    /// Returns structured diagnostics without exposing mutable access.
    pub(crate) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether apply may execute this plan after preflight and confirmation.
    pub(crate) fn is_executable(&self) -> bool {
        self.actions.len() == self.resource_actions.len()
            && self
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.is_blocking())
    }
}

/// The reason a Plan violates its executor-safety invariants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlanError {
    DuplicateActionTarget {
        target_path: ResolvedPath,
        first_resource_id: FullyQualifiedResourceId,
        duplicate_resource_id: FullyQualifiedResourceId,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateActionTarget {
                target_path,
                first_resource_id,
                duplicate_resource_id,
            } => write!(
                formatter,
                "plan target {target_path} is touched by both {first_resource_id} and {duplicate_resource_id}"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::actual::TargetObservation;
    use crate::domain::diagnostic::Diagnostic;
    use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(std::env::temp_dir().join("loadout-domain-plan").join(name)).unwrap()
    }

    fn desired(id: &str, source: &str, target: &str) -> ResolvedFileLink {
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            path(source),
            path(target),
        )
        .unwrap()
    }

    fn known(id: &str, source: &str, target: &str) -> KnownFileLink {
        KnownFileLink::from_resolved(&desired(id, source, target))
    }

    fn copy(id: &str, source: &str, target: &str) -> ResolvedFileCopy {
        ResolvedFileCopy::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            path(source),
            path(target),
            ContentFingerprint::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn replace_action_carries_exact_preconditions_postconditions_and_known_update() {
        let previous = known("base/git", "store/git/config", "home/.gitconfig");
        let desired = desired("base/git", "store/git/next", "home/.gitconfig");
        let action = PlannedAction::replace_link(desired.clone(), previous.clone()).unwrap();

        assert_eq!(action.kind(), ActionKind::ReplaceLink);
        assert_eq!(action.reason(), ActionReason::SourceChanged);
        assert_eq!(
            action.preconditions(),
            [TargetCondition::ExpectedLink {
                target_path: previous.target_path().clone(),
                link_target: previous.link_target().clone(),
            }]
        );
        assert_eq!(
            action.postconditions(),
            [TargetCondition::ExpectedLink {
                target_path: desired.target_path().clone(),
                link_target: desired.link_target().clone(),
            }]
        );
        assert_eq!(
            action.known_state_update(),
            Some(KnownStateUpdate::Upsert {
                resource: KnownFileLink::from_resolved(&desired),
            })
        );
    }

    #[test]
    fn copy_actions_carry_exact_predicates_and_typed_known_updates() {
        let previous =
            KnownFileCopy::from_resolved(&copy("base/git", "store/git/config", "home/.gitconfig"));
        let desired = ResolvedFileCopy::new(
            previous.resource_id().clone(),
            path("store/git/next-config"),
            previous.target_path().clone(),
            ContentFingerprint::parse(format!("sha256:{}", "b".repeat(64))).unwrap(),
        )
        .unwrap();
        let replace = PlannedFileCopyAction::Replace {
            desired: desired.clone(),
            previous: previous.clone(),
        };

        assert_eq!(
            replace.preconditions(),
            [TargetCondition::ExpectedCopy {
                target_path: previous.target_path().clone(),
                content_fingerprint: previous.content_fingerprint().clone(),
            }]
        );
        assert_eq!(
            replace.postconditions(),
            [TargetCondition::ExpectedCopy {
                target_path: desired.target_path().clone(),
                content_fingerprint: desired.source_content_fingerprint().clone(),
            }]
        );
        assert_eq!(
            replace.known_state_update(),
            Some(KnownStateUpdate::UpsertCopy {
                resource: KnownFileCopy::from_resolved(&desired),
            })
        );

        let relocate = PlannedFileCopyAction::Relocate {
            desired: ResolvedFileCopy::new(
                desired.resource_id().clone(),
                desired.source_path().clone(),
                path("home/.config/git/config"),
                desired.source_content_fingerprint().clone(),
            )
            .unwrap(),
            previous: previous.clone(),
        };
        assert!(matches!(
            relocate.preconditions().as_slice(),
            [
                TargetCondition::ExpectedCopy { .. },
                TargetCondition::Missing { .. }
            ]
        ));
        assert!(matches!(
            PlannedFileCopyAction::Remove { previous }
                .postconditions()
                .as_slice(),
            [TargetCondition::Missing { .. }]
        ));
    }

    #[test]
    fn resource_action_exposes_handoff_predicates_and_final_known_effect() {
        let old =
            KnownFileCopy::from_resolved(&copy("base/git", "store/git/config", "home/.gitconfig"));
        let final_link = desired("base/git", "store/git/next-config", "home/.gitconfig");
        let action = PlannedResourceAction::from(
            PlannedEffectHandoff::new(old.clone().into(), final_link.clone().into()).unwrap(),
        );

        assert_eq!(
            action.preconditions(),
            [TargetCondition::ExpectedCopy {
                target_path: old.target_path().clone(),
                content_fingerprint: old.content_fingerprint().clone(),
            }]
        );
        assert_eq!(
            action.postconditions(),
            [TargetCondition::ExpectedLink {
                target_path: final_link.target_path().clone(),
                link_target: final_link.link_target().clone(),
            }]
        );
        assert_eq!(
            action.known_state_update(),
            Some(ResourceKnownStateUpdate::Upsert {
                resource: KnownFileLink::from_resolved(&final_link).into(),
            })
        );
    }

    #[test]
    fn action_constructors_reject_payloads_that_do_not_match_their_transition() {
        let previous = known("base/git", "store/git/config", "home/.gitconfig");

        assert_eq!(
            PlannedAction::replace_link(
                desired("base/git", "store/git/config", "home/.gitconfig"),
                previous.clone(),
            )
            .unwrap_err(),
            PlannedActionError::LinkTargetUnchanged
        );
        assert_eq!(
            PlannedAction::relocate_link(
                desired("base/git", "store/git/config", "home/.gitconfig"),
                previous.clone(),
            )
            .unwrap_err(),
            PlannedActionError::TargetPathUnchanged
        );
        assert_eq!(
            PlannedAction::replace_ownership(
                desired("base/git", "store/git/config", "home/.gitconfig"),
                previous,
            )
            .unwrap_err(),
            PlannedActionError::ResourceIdsEqual
        );
    }

    #[test]
    fn plan_is_blocked_by_diagnostics_and_rejects_actions_with_a_shared_target() {
        let target = path("home/.gitconfig");
        let first =
            PlannedAction::create_link(desired("base/git", "store/git/config", "home/.gitconfig"));
        let second =
            PlannedAction::create_link(desired("base/zsh", "store/zshrc", "home/.gitconfig"));

        assert!(matches!(
            Plan::new([first, second], []),
            Err(PlanError::DuplicateActionTarget { .. })
        ));

        let diagnostic = Diagnostic::UnexpectedTarget {
            resource_id: FullyQualifiedResourceId::parse("base/git").unwrap(),
            target_path: target,
            observation: TargetObservation::OtherEntry {
                kind: crate::domain::actual::OtherEntryKind::RegularFile,
            },
        };
        let blocked = Plan::new([], [diagnostic]).unwrap();

        assert!(!blocked.is_executable());
        assert!(blocked.actions().is_empty());
        assert_eq!(blocked.diagnostics().len(), 1);
    }

    #[test]
    fn aggregate_plan_retains_a_typed_copy_action_without_exposing_it_as_a_link_action() {
        let copy_action = PlannedFileCopyAction::Create {
            desired: copy("base/git", "store/git/config", "home/.gitconfig"),
        };
        let plan = Plan::new_with_resource_actions([copy_action.into()], []).unwrap();

        assert!(plan.actions().is_empty());
        assert!(!plan.is_executable());
        assert!(matches!(
            plan.resource_actions(),
            [PlannedResourceAction::FileCopy(action)] if action.kind() == ActionKind::CreateCopy
        ));
    }
}
