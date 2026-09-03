//! Immutable executor-ready action plans derived by the pure planner.

use std::collections::BTreeMap;
use std::fmt;

use crate::domain::diagnostic::Diagnostic;
use crate::domain::file_link::{LinkTarget, ResolvedFileLink};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::KnownFileLink;
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
    Noop,
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
}

impl TargetCondition {
    /// The target path governed by this condition.
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        match self {
            Self::Missing { target_path } | Self::ExpectedLink { target_path, .. } => target_path,
        }
    }
}

/// The exact Known-state transition that becomes eligible after verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KnownStateUpdate {
    Upsert {
        resource: KnownFileLink,
    },
    Remove {
        resource_id: FullyQualifiedResourceId,
    },
    ReplaceIdentity {
        old_resource_id: FullyQualifiedResourceId,
        new_resource: KnownFileLink,
    },
}

/// One complete action; its payload is private so every crate caller must use a validated constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedAction {
    inner: PlannedActionInner,
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
    diagnostics: Vec<Diagnostic>,
}

impl Plan {
    /// Builds a plan while rejecting multiple actions that touch the same target.
    pub(crate) fn new(
        actions: impl IntoIterator<Item = PlannedAction>,
        diagnostics: impl IntoIterator<Item = Diagnostic>,
    ) -> Result<Self, PlanError> {
        let actions = actions.into_iter().collect::<Vec<_>>();
        let mut claimed_targets = BTreeMap::new();

        for action in &actions {
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

        Ok(Self {
            actions,
            diagnostics: diagnostics.into_iter().collect(),
        })
    }

    /// Returns executor-ready actions without exposing mutable access.
    pub(crate) fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    /// Returns structured diagnostics without exposing mutable access.
    pub(crate) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether apply may execute this plan after preflight and confirmation.
    pub(crate) fn is_executable(&self) -> bool {
        self.diagnostics
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
}
