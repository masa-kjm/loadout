//! File-link transition-table implementation.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::actual::{ActualState, TargetObservation};
use crate::domain::desired::ResolvedDesired;
use crate::domain::diagnostic::Diagnostic;
use crate::domain::file_link::ResolvedFileLink;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{KnownFileLink, KnownState};
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{Plan, PlannedAction};
use crate::planner::ordering::sort_actions;

/// Produces a complete deterministic Plan from resolved Desired, Known, and Actual inputs.
///
/// This is intentionally a pure function. Every input is a typed domain value;
/// it neither observes nor mutates the filesystem or durable state.
pub(crate) fn plan(desired: &ResolvedDesired, known: &KnownState, actual: &ActualState) -> Plan {
    let mut actions = Vec::new();
    let mut diagnostics = Vec::new();
    let blocked_targets = desired_target_collisions(desired, &mut diagnostics);
    let desired_ids = desired
        .resources()
        .iter()
        .map(|resource| resource.resource_id().clone())
        .collect::<BTreeSet<_>>();
    let desired_targets = desired
        .resources()
        .iter()
        .map(|resource| resource.target_path().clone())
        .collect::<BTreeSet<_>>();
    let mut handed_off_known_ids = BTreeSet::new();

    for resource in desired.resources() {
        if blocked_targets.contains(resource.target_path()) {
            continue;
        }

        match known.get(resource.resource_id()) {
            Some(previous) => {
                plan_existing_identity(resource, previous, actual, &mut actions, &mut diagnostics)
            }
            None => plan_new_identity(
                resource,
                known,
                actual,
                &desired_ids,
                &mut handed_off_known_ids,
                &mut actions,
                &mut diagnostics,
            ),
        }
    }

    for previous in known.resources() {
        if desired_ids.contains(previous.resource_id())
            || handed_off_known_ids.contains(previous.resource_id())
            || blocked_targets.contains(previous.target_path())
            // A Desired resource already governs this target. If its planning decision blocked, do not turn the stale record at the same target into an independently executable removal.
            || desired_targets.contains(previous.target_path())
        {
            continue;
        }

        plan_stale_identity(previous, actual, &mut actions, &mut diagnostics);
    }

    sort_actions(&mut actions);
    Plan::new(actions, diagnostics).expect("planner must not emit competing target actions")
}

fn desired_target_collisions(
    desired: &ResolvedDesired,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeSet<ResolvedPath> {
    let mut claimants = BTreeMap::<ResolvedPath, Vec<FullyQualifiedResourceId>>::new();
    for resource in desired.resources() {
        claimants
            .entry(resource.target_path().clone())
            .or_default()
            .push(resource.resource_id().clone());
    }

    let mut blocked_targets = BTreeSet::new();
    for (target_path, mut resource_ids) in claimants {
        if resource_ids.len() > 1 {
            resource_ids.sort();
            blocked_targets.insert(target_path.clone());
            diagnostics.push(Diagnostic::TargetCollision {
                target_path,
                resource_ids,
            });
        }
    }

    blocked_targets
}

fn plan_existing_identity(
    desired: &ResolvedFileLink,
    previous: &KnownFileLink,
    actual: &ActualState,
    actions: &mut Vec<PlannedAction>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if desired.target_path() == previous.target_path() {
        let Some(observation) = observation_at(actual, desired.target_path(), diagnostics) else {
            return;
        };

        match observation {
            TargetObservation::ExpectedLink { link_target }
                if link_target == previous.link_target() =>
            {
                if desired.link_target() == previous.link_target() {
                    actions.push(
                        PlannedAction::noop(desired.clone(), previous.clone())
                            .expect("matching Desired and Known definitions must noop"),
                    );
                } else {
                    actions.push(
                        PlannedAction::replace_link(desired.clone(), previous.clone())
                            .expect("same identity, same target, and changed source must replace"),
                    );
                }
            }
            TargetObservation::Missing => actions.push(PlannedAction::create_link(desired.clone())),
            observation => diagnostics.push(Diagnostic::UnexpectedTarget {
                resource_id: desired.resource_id().clone(),
                target_path: desired.target_path().clone(),
                observation: observation.clone(),
            }),
        }
        return;
    }

    let old_observation = observation_at(actual, previous.target_path(), diagnostics);
    let new_observation = observation_at(actual, desired.target_path(), diagnostics);
    let old_is_expected = matches!(
        old_observation,
        Some(TargetObservation::ExpectedLink { link_target }) if link_target == previous.link_target()
    );
    let new_is_missing = matches!(new_observation, Some(TargetObservation::Missing));

    if old_is_expected && new_is_missing {
        actions.push(
            PlannedAction::relocate_link(desired.clone(), previous.clone())
                .expect("same identity with a changed target must relocate"),
        );
        return;
    }

    if !old_is_expected {
        if let Some(observation) = old_observation {
            diagnostics.push(Diagnostic::UnexpectedTarget {
                resource_id: desired.resource_id().clone(),
                target_path: previous.target_path().clone(),
                observation: observation.clone(),
            });
        }
    }
    if !new_is_missing {
        if let Some(observation) = new_observation {
            diagnostics.push(Diagnostic::UnexpectedTarget {
                resource_id: desired.resource_id().clone(),
                target_path: desired.target_path().clone(),
                observation: observation.clone(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_new_identity(
    desired: &ResolvedFileLink,
    known: &KnownState,
    actual: &ActualState,
    desired_ids: &BTreeSet<FullyQualifiedResourceId>,
    handed_off_known_ids: &mut BTreeSet<FullyQualifiedResourceId>,
    actions: &mut Vec<PlannedAction>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let stale_at_target = known.resources().find(|previous| {
        previous.target_path() == desired.target_path()
            && !desired_ids.contains(previous.resource_id())
    });

    if let Some(previous) = stale_at_target {
        handed_off_known_ids.insert(previous.resource_id().clone());
        let Some(observation) = observation_at(actual, previous.target_path(), diagnostics) else {
            return;
        };

        match observation {
            TargetObservation::ExpectedLink { link_target }
                if link_target == previous.link_target() =>
            {
                actions.push(
                    PlannedAction::replace_ownership(desired.clone(), previous.clone()).expect(
                        "a distinct desired identity at a stale Known target must hand off ownership",
                    ),
                );
            }
            observation => diagnostics.push(Diagnostic::IdentityHandoffPrecondition {
                old_resource_id: previous.resource_id().clone(),
                new_resource_id: desired.resource_id().clone(),
                target_path: desired.target_path().clone(),
                observation: observation.clone(),
            }),
        }
        return;
    }

    let Some(observation) = observation_at(actual, desired.target_path(), diagnostics) else {
        return;
    };
    match observation {
        TargetObservation::Missing => actions.push(PlannedAction::create_link(desired.clone())),
        observation => diagnostics.push(Diagnostic::UnexpectedTarget {
            resource_id: desired.resource_id().clone(),
            target_path: desired.target_path().clone(),
            observation: observation.clone(),
        }),
    }
}

fn plan_stale_identity(
    previous: &KnownFileLink,
    actual: &ActualState,
    actions: &mut Vec<PlannedAction>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(observation) = observation_at(actual, previous.target_path(), diagnostics) else {
        return;
    };

    match observation {
        TargetObservation::ExpectedLink { link_target }
            if link_target == previous.link_target() =>
        {
            actions.push(PlannedAction::remove_link(previous.clone()));
        }
        TargetObservation::Missing => actions.push(PlannedAction::forget_missing(previous.clone())),
        observation => diagnostics.push(Diagnostic::UnexpectedTarget {
            resource_id: previous.resource_id().clone(),
            target_path: previous.target_path().clone(),
            observation: observation.clone(),
        }),
    }
}

fn observation_at<'a>(
    actual: &'a ActualState,
    target_path: &ResolvedPath,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a TargetObservation> {
    if let Some(observation) = actual.get(target_path) {
        return Some(observation.observation());
    }

    diagnostics.push(Diagnostic::MissingActualObservation {
        target_path: target_path.clone(),
    });
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::actual::{ActualFileLink, OtherEntryKind, ParentSafety};
    use crate::domain::desired::ResolvedDesired;
    use crate::domain::file_link::LinkTarget;
    use crate::domain::ids::ProfileId;
    use crate::domain::plan::{ActionKind, ActionReason, KnownStateUpdate, TargetCondition};

    fn path(name: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-planner-transition")
                .join(name),
        )
        .unwrap()
    }

    fn resource(id: &str, source: &str, target: &str) -> ResolvedFileLink {
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            path(source),
            path(target),
        )
        .unwrap()
    }

    fn desired(resources: Vec<ResolvedFileLink>) -> ResolvedDesired {
        ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), resources).unwrap()
    }

    fn known(resource: &ResolvedFileLink) -> KnownState {
        KnownState::new([KnownFileLink::from_resolved(resource)]).unwrap()
    }

    fn actual(observations: Vec<(&ResolvedPath, TargetObservation)>) -> ActualState {
        ActualState::new(observations.into_iter().map(|(target_path, observation)| {
            ActualFileLink::new(target_path.clone(), observation).unwrap()
        }))
        .unwrap()
    }

    fn expected(resource: &ResolvedFileLink) -> TargetObservation {
        TargetObservation::ExpectedLink {
            link_target: resource.link_target().clone(),
        }
    }

    fn unexpected_observations(resource: &ResolvedFileLink) -> Vec<TargetObservation> {
        vec![
            TargetObservation::MatchingUnmanagedLink {
                link_target: resource.link_target().clone(),
            },
            TargetObservation::OtherLink {
                link_target: LinkTarget::new(path("store/other")),
            },
            TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            },
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Symlink,
            },
        ]
    }

    fn assert_single_action(plan: Plan, kind: ActionKind, reason: ActionReason) {
        assert!(
            plan.is_executable(),
            "expected an executable plan: {plan:?}"
        );
        assert!(plan.diagnostics().is_empty());
        assert_eq!(plan.actions().len(), 1);
        assert_eq!(plan.actions()[0].kind(), kind);
        assert_eq!(plan.actions()[0].reason(), reason);
    }

    fn assert_replace_ownership_plan(
        previous: ResolvedFileLink,
        desired_resource: ResolvedFileLink,
    ) {
        let previous_known = KnownFileLink::from_resolved(&previous);
        let transition = plan(
            &desired(vec![desired_resource.clone()]),
            &KnownState::new([previous_known.clone()]).unwrap(),
            &actual(vec![(previous.target_path(), expected(&previous))]),
        );

        assert!(transition.is_executable());
        assert!(transition.diagnostics().is_empty());
        assert_eq!(transition.actions().len(), 1);

        let action = &transition.actions()[0];
        assert_eq!(action.kind(), ActionKind::ReplaceOwnership);
        assert_eq!(action.reason(), ActionReason::ManagedIdentityHandoff);
        assert_eq!(action.replaced_resource_id(), Some(previous.resource_id()));
        assert_eq!(action.resource_id(), desired_resource.resource_id());
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
                target_path: desired_resource.target_path().clone(),
                link_target: desired_resource.link_target().clone(),
            }]
        );
        assert_eq!(
            action.known_state_update(),
            Some(KnownStateUpdate::ReplaceIdentity {
                old_resource_id: previous.resource_id().clone(),
                new_resource: KnownFileLink::from_resolved(&desired_resource),
            })
        );
    }

    fn assert_blocked(plan: Plan) {
        assert!(!plan.is_executable(), "expected a blocked plan: {plan:?}");
        assert!(
            plan.actions().is_empty(),
            "blocked target must have no action"
        );
        assert!(!plan.diagnostics().is_empty());
    }

    #[test]
    fn present_new_identity_with_missing_target_creates_link() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![resource.clone()]),
                &KnownState::empty(),
                &actual(vec![(resource.target_path(), TargetObservation::Missing)]),
            ),
            ActionKind::CreateLink,
            ActionReason::TargetMissing,
        );
    }

    #[test]
    fn present_new_identity_rejects_every_unmanaged_or_unexpected_target_kind() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        for observation in unexpected_observations(&resource) {
            let transition = plan(
                &desired(vec![resource.clone()]),
                &KnownState::empty(),
                &actual(vec![(resource.target_path(), observation)]),
            );

            assert_blocked(transition);
        }
    }

    #[test]
    fn unchanged_definition_noops_when_its_expected_link_is_present() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![resource.clone()]),
                &known(&resource),
                &actual(vec![(resource.target_path(), expected(&resource))]),
            ),
            ActionKind::Noop,
            ActionReason::AlreadySatisfied,
        );
    }

    #[test]
    fn unchanged_definition_recreates_missing_target() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![resource.clone()]),
                &known(&resource),
                &actual(vec![(resource.target_path(), TargetObservation::Missing)]),
            ),
            ActionKind::CreateLink,
            ActionReason::TargetMissing,
        );
    }

    #[test]
    fn unchanged_definition_rejects_every_unexpected_target_kind() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        for observation in unexpected_observations(&resource) {
            assert_blocked(plan(
                &desired(vec![resource.clone()]),
                &known(&resource),
                &actual(vec![(resource.target_path(), observation)]),
            ));
        }
    }

    #[test]
    fn source_change_with_same_target_replaces_expected_link() {
        let previous = resource("base/git", "store/git/old", "home/.gitconfig");
        let desired_resource = resource("base/git", "store/git/new", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![desired_resource.clone()]),
                &known(&previous),
                &actual(vec![(previous.target_path(), expected(&previous))]),
            ),
            ActionKind::ReplaceLink,
            ActionReason::SourceChanged,
        );
    }

    #[test]
    fn source_change_with_same_target_recreates_missing_target() {
        let previous = resource("base/git", "store/git/old", "home/.gitconfig");
        let desired_resource = resource("base/git", "store/git/new", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![desired_resource.clone()]),
                &known(&previous),
                &actual(vec![(previous.target_path(), TargetObservation::Missing)]),
            ),
            ActionKind::CreateLink,
            ActionReason::TargetMissing,
        );
    }

    #[test]
    fn source_change_with_same_target_rejects_every_unexpected_target_kind() {
        let previous = resource("base/git", "store/git/old", "home/.gitconfig");
        let desired_resource = resource("base/git", "store/git/new", "home/.gitconfig");

        for observation in unexpected_observations(&desired_resource) {
            assert_blocked(plan(
                &desired(vec![desired_resource.clone()]),
                &known(&previous),
                &actual(vec![(previous.target_path(), observation)]),
            ));
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum RelocationObservation {
        Expected,
        Missing,
        MatchingUnmanaged,
        OtherLink,
        OtherEntry,
        UnsafePath,
    }

    impl RelocationObservation {
        fn all() -> [Self; 6] {
            [
                Self::Expected,
                Self::Missing,
                Self::MatchingUnmanaged,
                Self::OtherLink,
                Self::OtherEntry,
                Self::UnsafePath,
            ]
        }

        fn actual(self, expected_link_target: &LinkTarget) -> TargetObservation {
            match self {
                Self::Expected => TargetObservation::ExpectedLink {
                    link_target: expected_link_target.clone(),
                },
                Self::Missing => TargetObservation::Missing,
                Self::MatchingUnmanaged => TargetObservation::MatchingUnmanagedLink {
                    link_target: expected_link_target.clone(),
                },
                Self::OtherLink => TargetObservation::OtherLink {
                    link_target: LinkTarget::new(path("store/other")),
                },
                Self::OtherEntry => TargetObservation::OtherEntry {
                    kind: OtherEntryKind::RegularFile,
                },
                Self::UnsafePath => TargetObservation::UnsafePath {
                    parent_safety: ParentSafety::Symlink,
                },
            }
        }
    }

    #[test]
    fn target_change_relocates_only_when_old_is_expected_and_new_is_missing() {
        let previous = resource("base/git", "store/git/old", "home/.gitconfig");
        let desired_resource = resource("base/git", "store/git/new", "home/.config/git/config");

        for old_kind in RelocationObservation::all() {
            for new_kind in RelocationObservation::all() {
                let fixture = format!("old={old_kind:?}, new={new_kind:?}");
                let transition = plan(
                    &desired(vec![desired_resource.clone()]),
                    &known(&previous),
                    &actual(vec![
                        (
                            previous.target_path(),
                            old_kind.actual(previous.link_target()),
                        ),
                        (
                            desired_resource.target_path(),
                            new_kind.actual(desired_resource.link_target()),
                        ),
                    ]),
                );

                if matches!(old_kind, RelocationObservation::Expected)
                    && matches!(new_kind, RelocationObservation::Missing)
                {
                    assert_single_action(
                        transition,
                        ActionKind::RelocateLink,
                        ActionReason::TargetChanged,
                    );
                } else {
                    assert!(
                        !transition.is_executable(),
                        "{fixture} must block target relocation"
                    );
                    assert!(transition.actions().is_empty(), "{fixture} must not replan");
                    let diagnostic_targets = transition
                        .diagnostics()
                        .iter()
                        .map(|diagnostic| match diagnostic {
                            Diagnostic::UnexpectedTarget { target_path, .. } => target_path,
                            unexpected => {
                                panic!("{fixture} produced unexpected diagnostic: {unexpected:?}")
                            }
                        })
                        .collect::<Vec<_>>();
                    let mut expected_diagnostic_targets = Vec::new();
                    if !matches!(old_kind, RelocationObservation::Expected) {
                        expected_diagnostic_targets.push(previous.target_path());
                    }
                    if !matches!(new_kind, RelocationObservation::Missing) {
                        expected_diagnostic_targets.push(desired_resource.target_path());
                    }
                    assert_eq!(
                        diagnostic_targets, expected_diagnostic_targets,
                        "{fixture} must report only failed relocation observations"
                    );
                }
            }
        }
    }

    #[test]
    fn stale_expected_link_is_removed_and_stale_missing_target_is_forgotten() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        assert_single_action(
            plan(
                &desired(vec![]),
                &known(&resource),
                &actual(vec![(resource.target_path(), expected(&resource))]),
            ),
            ActionKind::RemoveLink,
            ActionReason::StaleResource,
        );
        assert_single_action(
            plan(
                &desired(vec![]),
                &known(&resource),
                &actual(vec![(resource.target_path(), TargetObservation::Missing)]),
            ),
            ActionKind::ForgetMissing,
            ActionReason::StaleResourceTargetMissing,
        );
    }

    #[test]
    fn stale_identity_rejects_every_unexpected_target_kind() {
        let resource = resource("base/git", "store/git/config", "home/.gitconfig");

        for observation in unexpected_observations(&resource) {
            assert_blocked(plan(
                &desired(vec![]),
                &known(&resource),
                &actual(vec![(resource.target_path(), observation)]),
            ));
        }
    }

    #[test]
    fn ownership_handoff_with_the_same_link_target_records_a_state_only_transition() {
        let old_resource = resource("base/git", "store/git/config", "home/.gitconfig");
        let new_resource = resource("workstation/git", "store/git/config", "home/.gitconfig");

        assert_eq!(old_resource.link_target(), new_resource.link_target());
        assert_replace_ownership_plan(old_resource, new_resource);
    }

    #[test]
    fn ownership_handoff_with_a_changed_link_target_records_replace_conditions() {
        let old_resource = resource("base/git", "store/git/old", "home/.gitconfig");
        let new_resource = resource("workstation/git", "store/git/new", "home/.gitconfig");

        assert_ne!(old_resource.link_target(), new_resource.link_target());
        assert_replace_ownership_plan(old_resource, new_resource);
    }

    #[test]
    fn relocation_into_a_stale_known_target_leaves_that_conflicted_target_action_free() {
        let previous = resource("base/git", "store/git/old", "home/.gitconfig");
        let desired_resource = resource("base/git", "store/git/new", "home/.config/git/config");
        let stale_at_new_target = resource("zeta/git", "store/zeta/git", "home/.config/git/config");
        let known = KnownState::new([
            KnownFileLink::from_resolved(&previous),
            KnownFileLink::from_resolved(&stale_at_new_target),
        ])
        .unwrap();

        let transition = plan(
            &desired(vec![desired_resource.clone()]),
            &known,
            &actual(vec![
                (previous.target_path(), expected(&previous)),
                (
                    desired_resource.target_path(),
                    expected(&stale_at_new_target),
                ),
            ]),
        );

        assert!(!transition.is_executable());
        assert!(transition.actions().is_empty());
        assert_eq!(
            transition.diagnostics(),
            [Diagnostic::UnexpectedTarget {
                resource_id: desired_resource.resource_id().clone(),
                target_path: desired_resource.target_path().clone(),
                observation: expected(&stale_at_new_target),
            }]
        );
    }

    #[test]
    fn target_collision_is_blocking_and_has_no_action_for_the_conflicted_target() {
        let first = resource("base/git", "store/git/config", "home/.gitconfig");
        let second = resource("workstation/git", "store/git/other", "home/.gitconfig");
        let independent = resource("zeta/zsh", "store/zshrc", "home/.zshrc");
        let collided = ResolvedDesired::new_unchecked_for_test(
            ProfileId::parse("workstation").unwrap(),
            vec![first.clone(), second, independent.clone()],
        );

        let transition = plan(
            &collided,
            &KnownState::empty(),
            &actual(vec![
                (first.target_path(), TargetObservation::Missing),
                (independent.target_path(), TargetObservation::Missing),
            ]),
        );

        assert!(!transition.is_executable());
        assert_eq!(transition.diagnostics().len(), 1);
        assert_eq!(transition.actions().len(), 1);
        assert_eq!(
            transition.actions()[0].resource_id(),
            independent.resource_id()
        );
    }

    #[test]
    fn planner_orders_complete_output_by_phase_then_resource_identity() {
        let create = resource("zeta/create", "store/create", "home/.create");
        let old_replace = resource("base/replace", "store/replace-old", "home/.replace");
        let new_replace = resource("base/replace", "store/replace-new", "home/.replace");
        let stale = resource("alpha/stale", "store/stale", "home/.stale");
        let known = KnownState::new([
            KnownFileLink::from_resolved(&old_replace),
            KnownFileLink::from_resolved(&stale),
        ])
        .unwrap();
        let transition = plan(
            &desired(vec![new_replace.clone(), create.clone()]),
            &known,
            &actual(vec![
                (create.target_path(), TargetObservation::Missing),
                (old_replace.target_path(), expected(&old_replace)),
                (stale.target_path(), expected(&stale)),
            ]),
        );

        assert!(transition.is_executable());
        assert_eq!(
            transition
                .actions()
                .iter()
                .map(|action| (action.kind(), action.resource_id().as_str()))
                .collect::<Vec<_>>(),
            [
                (ActionKind::CreateLink, "zeta/create"),
                (ActionKind::ReplaceLink, "base/replace"),
                (ActionKind::RemoveLink, "alpha/stale"),
            ]
        );
    }

    #[test]
    fn planner_output_is_deterministic_when_equivalent_inputs_arrive_in_different_orders() {
        let first = resource("base/git", "store/git/config", "home/.gitconfig");
        let second = resource("zeta/zsh", "store/zshrc", "home/.zshrc");
        let expected_inputs = (
            desired(vec![first.clone(), second.clone()]),
            KnownState::empty(),
            actual(vec![
                (first.target_path(), TargetObservation::Missing),
                (second.target_path(), TargetObservation::Missing),
            ]),
        );
        let reordered_inputs = (
            desired(vec![second.clone(), first.clone()]),
            KnownState::empty(),
            actual(vec![
                (second.target_path(), TargetObservation::Missing),
                (first.target_path(), TargetObservation::Missing),
            ]),
        );

        let expected_plan = plan(&expected_inputs.0, &expected_inputs.1, &expected_inputs.2);
        let reordered_plan = plan(
            &reordered_inputs.0,
            &reordered_inputs.1,
            &reordered_inputs.2,
        );

        assert_eq!(expected_plan, reordered_plan);
        assert_eq!(
            expected_plan
                .actions()
                .iter()
                .map(|action| action.resource_id().as_str())
                .collect::<Vec<_>>(),
            ["base/git", "zeta/zsh"]
        );
    }
}
