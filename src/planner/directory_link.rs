//! Internal pure transition table for DirectoryLink before lifecycle enablement.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::actual::{ParentSafety, TargetObservation};
use crate::domain::desired::{ResolvedDesired, ResolvedResource};
use crate::domain::directory_link::ResolvedDirectoryLink;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::{KnownDirectoryLink, KnownResource, KnownState};
use crate::domain::paths::ResolvedPath;
use crate::planner::ordering::ActionPhase;

/// A no-follow DirectoryLink observation supplied to the pure transition table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryLinkActual {
    target_path: ResolvedPath,
    observation: TargetObservation,
}

impl DirectoryLinkActual {
    pub(crate) fn new(
        target_path: ResolvedPath,
        observation: TargetObservation,
    ) -> Result<Self, DirectoryLinkActualError> {
        if matches!(
            observation,
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Safe
            }
        ) {
            return Err(DirectoryLinkActualError::UnsafePathMarkedSafe);
        }
        Ok(Self {
            target_path,
            observation,
        })
    }
    pub(crate) fn target_path(&self) -> &ResolvedPath {
        &self.target_path
    }
    pub(crate) fn observation(&self) -> &TargetObservation {
        &self.observation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryLinkActualError {
    UnsafePathMarkedSafe,
}

/// The complete no-follow observations supplied to DirectoryLink planning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirectoryLinkActualState(BTreeMap<ResolvedPath, DirectoryLinkActual>);

impl DirectoryLinkActualState {
    pub(crate) fn new(
        values: impl IntoIterator<Item = DirectoryLinkActual>,
    ) -> Result<Self, DirectoryLinkActualStateError> {
        let mut targets = BTreeMap::new();
        for value in values {
            let target_path = value.target_path.clone();
            if targets.insert(target_path.clone(), value).is_some() {
                return Err(DirectoryLinkActualStateError::DuplicateTargetObservation {
                    target_path,
                });
            }
        }
        Ok(Self(targets))
    }
    fn get(&self, target: &ResolvedPath) -> Option<&TargetObservation> {
        self.0.get(target).map(DirectoryLinkActual::observation)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryLinkActualStateError {
    DuplicateTargetObservation { target_path: ResolvedPath },
}

/// A non-executor-ready DirectoryLink transition selected by the pure table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryLinkTransition {
    Create {
        desired: ResolvedDirectoryLink,
        superseded: Option<KnownDirectoryLink>,
    },
    Replace {
        desired: ResolvedDirectoryLink,
        previous: KnownDirectoryLink,
    },
    Relocate {
        desired: ResolvedDirectoryLink,
        previous: KnownDirectoryLink,
    },
    Remove(KnownDirectoryLink),
    ForgetMissing(KnownDirectoryLink),
    Noop {
        desired: ResolvedDirectoryLink,
        previous: KnownDirectoryLink,
    },
    ReplaceOwnership {
        desired: ResolvedDirectoryLink,
        previous: KnownDirectoryLink,
    },
}

/// A blocking result from the internal DirectoryLink transition table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryLinkDiagnostic {
    MissingActual(ResolvedPath),
    UnexpectedTarget {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    TargetCollision {
        target_path: ResolvedPath,
        resource_ids: Vec<FullyQualifiedResourceId>,
    },
    MixedEffect {
        resource_id: FullyQualifiedResourceId,
        target_path: ResolvedPath,
    },
}

/// Pure DirectoryLink transitions kept separate from executor-ready plans until Commit 8.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DirectoryLinkTransitionPlan {
    transitions: Vec<DirectoryLinkTransition>,
    diagnostics: Vec<DirectoryLinkDiagnostic>,
}

impl DirectoryLinkTransitionPlan {
    pub(crate) fn transitions(&self) -> &[DirectoryLinkTransition] {
        &self.transitions
    }
    pub(crate) fn diagnostics(&self) -> &[DirectoryLinkDiagnostic] {
        &self.diagnostics
    }
    pub(crate) fn is_executable(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Plans only DirectoryLink transitions and never yields executor-ready actions.
pub(crate) fn plan(
    desired: &ResolvedDesired,
    known: &KnownState,
    actual: &DirectoryLinkActualState,
) -> DirectoryLinkTransitionPlan {
    let desired_links = desired
        .variants()
        .iter()
        .filter_map(|value| match value {
            ResolvedResource::DirectoryLink(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let known_links = known
        .variants()
        .filter_map(|value| match value {
            KnownResource::DirectoryLink(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut result = DirectoryLinkTransitionPlan::default();
    let mut claims = BTreeMap::<ResolvedPath, Vec<FullyQualifiedResourceId>>::new();
    for value in &desired_links {
        claims
            .entry(value.target_path().clone())
            .or_default()
            .push(value.resource_id().clone());
    }
    let mut blocked = BTreeSet::new();
    for (target, ids) in claims {
        if ids.len() > 1 {
            blocked.insert(target.clone());
            result
                .diagnostics
                .push(DirectoryLinkDiagnostic::TargetCollision {
                    target_path: target,
                    resource_ids: ids,
                });
        }
    }
    let desired_ids = desired_links
        .iter()
        .map(|value| value.resource_id().clone())
        .collect::<BTreeSet<_>>();
    let mut mixed_effect_ids = BTreeSet::new();
    for value in &desired_links {
        if matches!(
            known.get_variant(value.resource_id()),
            Some(resource) if !matches!(resource, KnownResource::DirectoryLink(_))
        ) {
            mixed_effect_ids.insert(value.resource_id().clone());
            result
                .diagnostics
                .push(DirectoryLinkDiagnostic::MixedEffect {
                    resource_id: value.resource_id().clone(),
                    target_path: value.target_path().clone(),
                });
        }
    }
    for old in &known_links {
        if let Some(value) = desired.variants().iter().find(|value| {
            value.resource_id() == old.resource_id()
                && !matches!(value, ResolvedResource::DirectoryLink(_))
        }) {
            mixed_effect_ids.insert(old.resource_id().clone());
            result
                .diagnostics
                .push(DirectoryLinkDiagnostic::MixedEffect {
                    resource_id: value.resource_id().clone(),
                    target_path: old.target_path().clone(),
                });
        }
    }
    for value in desired_links {
        if blocked.contains(value.target_path()) || mixed_effect_ids.contains(value.resource_id()) {
            continue;
        }
        if known.variants().any(|old| {
            old.target_path() == value.target_path()
                && !matches!(old, KnownResource::DirectoryLink(_))
        }) {
            result
                .diagnostics
                .push(DirectoryLinkDiagnostic::MixedEffect {
                    resource_id: value.resource_id().clone(),
                    target_path: value.target_path().clone(),
                });
            continue;
        }
        match known_links
            .iter()
            .find(|old| old.resource_id() == value.resource_id())
        {
            Some(old) => existing(value, old, actual, &mut result),
            None => new_identity(value, &known_links, &desired_ids, actual, &mut result),
        }
    }
    for old in &known_links {
        if let Some(value) = desired.variants().iter().find(|value| {
            value.target_path() == old.target_path()
                && !matches!(value, ResolvedResource::DirectoryLink(_))
        }) {
            result
                .diagnostics
                .push(DirectoryLinkDiagnostic::MixedEffect {
                    resource_id: value.resource_id().clone(),
                    target_path: old.target_path().clone(),
                });
        }
    }
    for old in known_links {
        if mixed_effect_ids.contains(old.resource_id())
            || desired_ids.contains(old.resource_id())
            || desired
                .variants()
                .iter()
                .any(|value| value.target_path() == old.target_path())
        {
            continue;
        }
        match observe(actual, old.target_path(), &mut result) {
            Some(TargetObservation::ExpectedLink { link_target })
                if link_target == old.link_target() =>
            {
                result
                    .transitions
                    .push(DirectoryLinkTransition::Remove(old.clone()))
            }
            Some(TargetObservation::Missing) => result
                .transitions
                .push(DirectoryLinkTransition::ForgetMissing(old.clone())),
            Some(observation) => unexpected(
                old.resource_id(),
                old.target_path(),
                observation,
                &mut result,
            ),
            None => {}
        }
    }
    result.transitions.sort_by_key(transition_order);
    result
}

fn existing(
    value: &ResolvedDirectoryLink,
    old: &KnownDirectoryLink,
    actual: &DirectoryLinkActualState,
    result: &mut DirectoryLinkTransitionPlan,
) {
    if value.target_path() != old.target_path() {
        let old_observation = observe(actual, old.target_path(), result);
        let new_observation = observe(actual, value.target_path(), result);
        let old_ok = matches!(old_observation, Some(TargetObservation::ExpectedLink { link_target }) if link_target == old.link_target());
        let new_missing = matches!(new_observation, Some(TargetObservation::Missing));
        if old_ok && new_missing {
            result.transitions.push(DirectoryLinkTransition::Relocate {
                desired: value.clone(),
                previous: old.clone(),
            });
        }
        if !old_ok {
            if let Some(observation) = old_observation {
                unexpected(value.resource_id(), old.target_path(), observation, result);
            }
        }
        if !new_missing {
            if let Some(observation) = new_observation {
                unexpected(
                    value.resource_id(),
                    value.target_path(),
                    observation,
                    result,
                );
            }
        }
        return;
    }
    match observe(actual, value.target_path(), result) {
        Some(TargetObservation::ExpectedLink { link_target })
            if link_target == old.link_target() && value.link_target() == old.link_target() =>
        {
            result.transitions.push(DirectoryLinkTransition::Noop {
                desired: value.clone(),
                previous: old.clone(),
            })
        }
        Some(TargetObservation::ExpectedLink { link_target })
            if link_target == old.link_target() =>
        {
            result.transitions.push(DirectoryLinkTransition::Replace {
                desired: value.clone(),
                previous: old.clone(),
            })
        }
        Some(TargetObservation::Missing) => {
            result.transitions.push(DirectoryLinkTransition::Create {
                desired: value.clone(),
                superseded: None,
            })
        }
        Some(observation) => unexpected(
            value.resource_id(),
            value.target_path(),
            observation,
            result,
        ),
        None => {}
    }
}

fn new_identity(
    value: &ResolvedDirectoryLink,
    known: &[&KnownDirectoryLink],
    desired_ids: &BTreeSet<FullyQualifiedResourceId>,
    actual: &DirectoryLinkActualState,
    result: &mut DirectoryLinkTransitionPlan,
) {
    if let Some(old) = known.iter().find(|old| {
        old.target_path() == value.target_path() && !desired_ids.contains(old.resource_id())
    }) {
        match observe(actual, value.target_path(), result) {
            Some(TargetObservation::ExpectedLink { link_target })
                if link_target == old.link_target() =>
            {
                result
                    .transitions
                    .push(DirectoryLinkTransition::ReplaceOwnership {
                        desired: value.clone(),
                        previous: (*old).clone(),
                    })
            }
            Some(TargetObservation::Missing) => {
                result.transitions.push(DirectoryLinkTransition::Create {
                    desired: value.clone(),
                    superseded: Some((*old).clone()),
                })
            }
            Some(observation) => unexpected(
                value.resource_id(),
                value.target_path(),
                observation,
                result,
            ),
            None => {}
        }
    } else {
        match observe(actual, value.target_path(), result) {
            Some(TargetObservation::Missing) => {
                result.transitions.push(DirectoryLinkTransition::Create {
                    desired: value.clone(),
                    superseded: None,
                })
            }
            Some(observation) => unexpected(
                value.resource_id(),
                value.target_path(),
                observation,
                result,
            ),
            None => {}
        }
    }
}

fn observe<'a>(
    actual: &'a DirectoryLinkActualState,
    target: &ResolvedPath,
    result: &mut DirectoryLinkTransitionPlan,
) -> Option<&'a TargetObservation> {
    let value = actual.get(target);
    if value.is_none() {
        result
            .diagnostics
            .push(DirectoryLinkDiagnostic::MissingActual(target.clone()));
    }
    value
}
fn unexpected(
    id: &FullyQualifiedResourceId,
    target: &ResolvedPath,
    observation: &TargetObservation,
    result: &mut DirectoryLinkTransitionPlan,
) {
    result
        .diagnostics
        .push(DirectoryLinkDiagnostic::UnexpectedTarget {
            resource_id: id.clone(),
            target_path: target.clone(),
            observation: observation.clone(),
        });
}

fn transition_order(value: &DirectoryLinkTransition) -> (ActionPhase, String) {
    let (phase, resource_id) = match value {
        DirectoryLinkTransition::Create { desired, .. } => (
            ActionPhase::Create,
            desired.resource_id().as_str().to_owned(),
        ),
        DirectoryLinkTransition::Replace { desired, .. }
        | DirectoryLinkTransition::Relocate { desired, .. } => (
            ActionPhase::ReplaceOrRelocate,
            desired.resource_id().as_str().to_owned(),
        ),
        DirectoryLinkTransition::ReplaceOwnership { desired, previous } => (
            ActionPhase::ReplaceOrRelocate,
            format!("{}\0{}", previous.resource_id(), desired.resource_id()),
        ),
        DirectoryLinkTransition::Remove(previous)
        | DirectoryLinkTransition::ForgetMissing(previous) => (
            ActionPhase::RemoveOrForget,
            previous.resource_id().as_str().to_owned(),
        ),
        DirectoryLinkTransition::Noop { desired, .. } => {
            (ActionPhase::Noop, desired.resource_id().as_str().to_owned())
        }
    };
    (phase, resource_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::file_link::LinkTarget;
    use crate::domain::ids::ProfileId;

    fn path(value: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-directory-link-plan")
                .join(value),
        )
        .unwrap()
    }
    fn desired(id: &str, source: &str, target: &str) -> ResolvedDirectoryLink {
        ResolvedDirectoryLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            path(source),
            path(target),
        )
        .unwrap()
    }
    fn desired_set(values: Vec<ResolvedDirectoryLink>) -> ResolvedDesired {
        ResolvedDesired::new(ProfileId::parse("base").unwrap(), values).unwrap()
    }
    fn actual(values: Vec<(&ResolvedPath, TargetObservation)>) -> DirectoryLinkActualState {
        DirectoryLinkActualState::new(values.into_iter().map(|(path, observation)| {
            DirectoryLinkActual::new(path.clone(), observation).unwrap()
        }))
        .unwrap()
    }
    fn expected(value: &ResolvedDirectoryLink) -> TargetObservation {
        TargetObservation::ExpectedLink {
            link_target: value.link_target().clone(),
        }
    }

    #[test]
    fn transition_table_covers_create_replace_relocate_remove_forget_noop_and_handoff() {
        let old = desired("base/old", "store/old", "home/nvim");
        let changed = desired("base/old", "store/new", "home/nvim");
        let moved = desired("base/old", "store/old", "home/moved");
        let new = desired("base/new", "store/old", "home/nvim");
        let known = KnownState::new([KnownDirectoryLink::from_resolved(&old)]).unwrap();
        let cases = [
            (
                desired_set(vec![old.clone()]),
                actual(vec![(old.target_path(), expected(&old))]),
                "noop",
            ),
            (
                desired_set(vec![changed.clone()]),
                actual(vec![(old.target_path(), expected(&old))]),
                "replace",
            ),
            (
                desired_set(vec![moved.clone()]),
                actual(vec![
                    (old.target_path(), expected(&old)),
                    (moved.target_path(), TargetObservation::Missing),
                ]),
                "relocate",
            ),
            (
                desired_set(vec![new.clone()]),
                actual(vec![(old.target_path(), expected(&old))]),
                "handoff",
            ),
            (
                desired_set(vec![]),
                actual(vec![(old.target_path(), expected(&old))]),
                "remove",
            ),
            (
                desired_set(vec![]),
                actual(vec![(old.target_path(), TargetObservation::Missing)]),
                "forget",
            ),
        ];
        for (desired, actual, expected_kind) in cases {
            let plan = plan(&desired, &known, &actual);
            assert!(plan.is_executable(), "{expected_kind}: {plan:?}");
            assert_eq!(plan.transitions().len(), 1);
            let kind = match &plan.transitions()[0] {
                DirectoryLinkTransition::Create { .. } => "create",
                DirectoryLinkTransition::Replace { .. } => "replace",
                DirectoryLinkTransition::Relocate { .. } => "relocate",
                DirectoryLinkTransition::Remove(_) => "remove",
                DirectoryLinkTransition::ForgetMissing(_) => "forget",
                DirectoryLinkTransition::Noop { .. } => "noop",
                DirectoryLinkTransition::ReplaceOwnership { .. } => "handoff",
            };
            assert_eq!(kind, expected_kind);
        }
    }

    #[test]
    fn missing_new_identity_creates_and_unexpected_or_mixed_effect_blocks_without_transition() {
        let value = desired("base/nvim", "store/nvim", "home/nvim");
        let create = plan(
            &desired_set(vec![value.clone()]),
            &KnownState::empty(),
            &actual(vec![(value.target_path(), TargetObservation::Missing)]),
        );
        assert!(matches!(
            create.transitions(),
            [DirectoryLinkTransition::Create { .. }]
        ));
        let unexpected = plan(
            &desired_set(vec![value.clone()]),
            &KnownState::empty(),
            &actual(vec![(
                value.target_path(),
                TargetObservation::OtherLink {
                    link_target: LinkTarget::new(path("store/other")),
                },
            )]),
        );
        assert!(!unexpected.is_executable());
        assert!(unexpected.transitions().is_empty());
        let file = crate::domain::file_link::ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/file").unwrap(),
            path("store/file"),
            value.target_path().clone(),
        )
        .unwrap();
        let mixed = plan(
            &desired_set(vec![value]),
            &KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&file)]).unwrap(),
            &DirectoryLinkActualState::default(),
        );
        assert!(!mixed.is_executable());
        assert!(mixed.transitions().is_empty());
    }

    #[test]
    fn relocation_failure_and_missing_target_handoff_preserve_blocking_or_supersession_facts() {
        let old = desired("base/old", "store/old", "home/nvim");
        let moved = desired("base/old", "store/old", "home/moved");
        let known = KnownState::new([KnownDirectoryLink::from_resolved(&old)]).unwrap();
        let blocked = plan(
            &desired_set(vec![moved.clone()]),
            &known,
            &actual(vec![
                (old.target_path(), TargetObservation::Missing),
                (
                    moved.target_path(),
                    TargetObservation::OtherEntry {
                        kind: crate::domain::actual::OtherEntryKind::Directory,
                    },
                ),
            ]),
        );
        assert!(!blocked.is_executable());
        assert!(blocked.transitions().is_empty());
        assert_eq!(blocked.diagnostics().len(), 2);

        let replacement = desired("base/new", "store/old", "home/nvim");
        let handoff = plan(
            &desired_set(vec![replacement.clone()]),
            &known,
            &actual(vec![(old.target_path(), TargetObservation::Missing)]),
        );
        assert!(matches!(
            handoff.transitions(),
            [DirectoryLinkTransition::Create { desired, superseded: Some(previous) }]
                if desired == &replacement && previous.resource_id() == old.resource_id()
        ));
    }

    #[test]
    fn transitions_are_ordered_by_phase_then_resource_id() {
        let create = desired("base/z-create", "store/create", "home/create");
        let create_target = create.target_path().clone();
        let old = desired("base/a-old", "store/old", "home/old");
        let changed = desired("base/a-old", "store/new", "home/old");
        let known = KnownState::new([KnownDirectoryLink::from_resolved(&old)]).unwrap();
        let result = plan(
            &desired_set(vec![changed, create]),
            &known,
            &actual(vec![
                (old.target_path(), expected(&old)),
                (&create_target, TargetObservation::Missing),
            ]),
        );
        assert!(matches!(
            result.transitions()[0],
            DirectoryLinkTransition::Create { .. }
        ));
        assert!(matches!(
            result.transitions()[1],
            DirectoryLinkTransition::Replace { .. }
        ));
    }

    #[test]
    fn noop_and_handoff_use_the_existing_action_sort_key_rules() {
        let first_old = desired("old/alpha", "store/one", "home/one");
        let second_old = desired("old/zeta", "store/two", "home/two");
        let first_new = desired("new/zeta", "store/one", "home/one");
        let second_new = desired("new/alpha", "store/two", "home/two");
        let known = KnownState::new([
            KnownDirectoryLink::from_resolved(&first_old),
            KnownDirectoryLink::from_resolved(&second_old),
        ])
        .unwrap();
        let handoffs = plan(
            &desired_set(vec![first_new, second_new]),
            &known,
            &actual(vec![
                (first_old.target_path(), expected(&first_old)),
                (second_old.target_path(), expected(&second_old)),
            ]),
        );
        assert!(matches!(
            handoffs.transitions(),
            [DirectoryLinkTransition::ReplaceOwnership { previous, .. }, DirectoryLinkTransition::ReplaceOwnership { previous: next, .. }]
                if previous.resource_id().as_str() == "old/alpha" && next.resource_id().as_str() == "old/zeta"
        ));

        let noop = plan(
            &desired_set(vec![first_old.clone()]),
            &KnownState::new([KnownDirectoryLink::from_resolved(&first_old)]).unwrap(),
            &actual(vec![(first_old.target_path(), expected(&first_old))]),
        );
        assert_eq!(
            transition_order(&noop.transitions()[0]).0,
            ActionPhase::Noop
        );
    }

    #[test]
    fn mixed_effect_identity_changes_block_both_target_change_directions() {
        let directory = desired("base/item", "store/directory", "home/new");
        let file = crate::domain::file_link::ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("base/item").unwrap(),
            path("store/file"),
            path("home/old"),
        )
        .unwrap();
        let directory_from_file = plan(
            &desired_set(vec![directory.clone()]),
            &KnownState::new([crate::domain::known::KnownFileLink::from_resolved(&file)]).unwrap(),
            &actual(vec![(directory.target_path(), TargetObservation::Missing)]),
        );
        assert!(!directory_from_file.is_executable());
        assert!(directory_from_file.transitions().is_empty());

        let desired_file = ResolvedDesired::new(
            ProfileId::parse("base").unwrap(),
            [crate::domain::file_link::ResolvedFileLink::new(
                FullyQualifiedResourceId::parse("base/item").unwrap(),
                path("store/file-next"),
                path("home/next"),
            )
            .unwrap()],
        )
        .unwrap();
        let file_from_directory = plan(
            &desired_file,
            &KnownState::new([KnownDirectoryLink::from_resolved(&directory)]).unwrap(),
            &actual(vec![(directory.target_path(), expected(&directory))]),
        );
        assert!(!file_from_directory.is_executable());
        assert!(file_from_directory.transitions().is_empty());
    }

    #[test]
    fn actual_rejects_unsafe_safe_and_duplicate_target_observations() {
        let target = path("home/nvim");
        assert_eq!(
            DirectoryLinkActual::new(
                target.clone(),
                TargetObservation::UnsafePath {
                    parent_safety: ParentSafety::Safe,
                },
            )
            .unwrap_err(),
            DirectoryLinkActualError::UnsafePathMarkedSafe
        );
        let first = DirectoryLinkActual::new(target.clone(), TargetObservation::Missing).unwrap();
        let second = DirectoryLinkActual::new(target.clone(), TargetObservation::Missing).unwrap();
        assert_eq!(
            DirectoryLinkActualState::new([first, second]).unwrap_err(),
            DirectoryLinkActualStateError::DuplicateTargetObservation {
                target_path: target
            }
        );
    }
}
