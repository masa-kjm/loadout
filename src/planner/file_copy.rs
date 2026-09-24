//! Pure file-copy transition classification.

use crate::domain::actual::{
    ActualResource, ActualState, CopyTargetObservation, TargetObservation,
};
use crate::domain::desired::{ResolvedDesired, ResolvedResource};
use crate::domain::diagnostic::Diagnostic;
use crate::domain::file_copy::ResolvedFileCopy;
use crate::domain::known::{KnownFileCopy, KnownResource, KnownState};
use crate::domain::plan::{Plan, PlannedEffectHandoff, PlannedFileCopyAction};

/// Plans only copy effects from typed Desired, Known, and Actual inputs without filesystem access.
pub(crate) fn plan(desired: &ResolvedDesired, known: &KnownState, actual: &ActualState) -> Plan {
    let mut actions = Vec::new();
    let mut diagnostics = Vec::new();
    for resource in desired.variants() {
        let ResolvedResource::FileCopy(resource) = resource else {
            continue;
        };
        match known.get_variant(resource.resource_id()) {
            Some(KnownResource::FileCopy(previous))
                if previous.target_path() != resource.target_path() =>
            {
                let old = copy_observation(actual, previous.target_path());
                let new = copy_observation(actual, resource.target_path());
                if matches!(&old, Some(CopyTargetObservation::ExpectedCopy { content_fingerprint }) if content_fingerprint == previous.content_fingerprint())
                    && matches!(&new, Some(CopyTargetObservation::Missing))
                {
                    actions.push(
                        PlannedFileCopyAction::Relocate {
                            desired: resource.clone(),
                            previous: previous.clone(),
                        }
                        .into(),
                    );
                } else {
                    conflict(resource, old.or(new), &mut diagnostics);
                }
            }
            Some(KnownResource::FileCopy(previous)) => {
                match copy_observation(actual, resource.target_path()) {
                    Some(CopyTargetObservation::ExpectedCopy {
                        content_fingerprint,
                    }) if content_fingerprint == previous.content_fingerprint().clone() => {
                        if resource.source_content_fingerprint() == previous.content_fingerprint() {
                            actions.push(
                                PlannedFileCopyAction::Noop {
                                    desired: resource.clone(),
                                    previous: previous.clone(),
                                }
                                .into(),
                            );
                        } else {
                            actions.push(
                                PlannedFileCopyAction::Replace {
                                    desired: resource.clone(),
                                    previous: previous.clone(),
                                }
                                .into(),
                            );
                        }
                    }
                    Some(CopyTargetObservation::Missing) => actions.push(
                        PlannedFileCopyAction::Create {
                            desired: resource.clone(),
                        }
                        .into(),
                    ),
                    observation => conflict(resource, observation, &mut diagnostics),
                }
            }
            Some(KnownResource::FileLink(previous)) => {
                match actual.get_variant(resource.target_path()) {
                    Some(ActualResource::FileLink(observation)) if matches!(observation.observation(), TargetObservation::ExpectedLink { link_target } if link_target == previous.link_target()) =>
                    {
                        actions.push(
                            PlannedEffectHandoff::new(
                                KnownResource::FileLink(previous.clone()),
                                ResolvedResource::FileCopy(resource.clone()),
                            )
                            .expect("same identity and target are prevalidated")
                            .into(),
                        );
                    }
                    Some(ActualResource::FileCopy(observation))
                        if matches!(observation.observation(), CopyTargetObservation::Missing) =>
                    {
                        actions.push(
                            PlannedFileCopyAction::Create {
                                desired: resource.clone(),
                            }
                            .into(),
                        );
                    }
                    _ => conflict(resource, None, &mut diagnostics),
                }
            }
            None => match copy_observation(actual, resource.target_path()) {
                Some(CopyTargetObservation::Missing) => actions.push(
                    PlannedFileCopyAction::Create {
                        desired: resource.clone(),
                    }
                    .into(),
                ),
                observation => conflict(resource, observation, &mut diagnostics),
            },
        }
    }
    for resource in desired.variants() {
        let ResolvedResource::FileLink(resource) = resource else {
            continue;
        };
        let Some(KnownResource::FileCopy(previous)) = known.get_variant(resource.resource_id())
        else {
            continue;
        };
        if previous.target_path() != resource.target_path() {
            diagnostics.push(Diagnostic::UnexpectedCopyTarget {
                resource_id: resource.resource_id().clone(),
                target_path: resource.target_path().clone(),
                observation: CopyTargetObservation::UnsafePath {
                    parent_safety: crate::domain::actual::ParentSafety::Missing,
                },
            });
            continue;
        }
        match actual.get_variant(resource.target_path()) {
            Some(ActualResource::FileCopy(observation)) if matches!(observation.observation(), CopyTargetObservation::ExpectedCopy { content_fingerprint } if content_fingerprint == previous.content_fingerprint()) =>
            {
                actions.push(
                    PlannedEffectHandoff::new(
                        KnownResource::FileCopy(previous.clone()),
                        ResolvedResource::FileLink(resource.clone()),
                    )
                    .expect("same identity and target are prevalidated")
                    .into(),
                );
            }
            _ => diagnostics.push(Diagnostic::UnexpectedCopyTarget {
                resource_id: resource.resource_id().clone(),
                target_path: resource.target_path().clone(),
                observation: CopyTargetObservation::UnsafePath {
                    parent_safety: crate::domain::actual::ParentSafety::Missing,
                },
            }),
        }
    }
    for resource in known.variants() {
        let KnownResource::FileCopy(previous) = resource else {
            continue;
        };
        if desired
            .variants()
            .iter()
            .any(|resource| resource.resource_id() == previous.resource_id())
        {
            continue;
        }
        match copy_observation(actual, previous.target_path()) {
            Some(CopyTargetObservation::ExpectedCopy {
                content_fingerprint,
            }) if content_fingerprint == previous.content_fingerprint().clone() => actions.push(
                PlannedFileCopyAction::Remove {
                    previous: previous.clone(),
                }
                .into(),
            ),
            Some(CopyTargetObservation::Missing) => actions.push(
                PlannedFileCopyAction::ForgetMissing {
                    previous: previous.clone(),
                }
                .into(),
            ),
            observation => conflict_known(previous, observation, &mut diagnostics),
        }
    }
    Plan::new_with_resource_actions(actions, diagnostics)
        .expect("copy planner emits no duplicate target actions")
}

fn copy_observation(
    actual: &ActualState,
    target: &crate::domain::paths::ResolvedPath,
) -> Option<CopyTargetObservation> {
    match actual.get_variant(target) {
        Some(ActualResource::FileCopy(actual)) => Some(actual.observation().clone()),
        _ => None,
    }
}

fn conflict(
    resource: &ResolvedFileCopy,
    observation: Option<CopyTargetObservation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::UnexpectedCopyTarget {
        resource_id: resource.resource_id().clone(),
        target_path: resource.target_path().clone(),
        observation: observation.unwrap_or(CopyTargetObservation::UnsafePath {
            parent_safety: crate::domain::actual::ParentSafety::Missing,
        }),
    });
}

fn conflict_known(
    resource: &KnownFileCopy,
    observation: Option<CopyTargetObservation>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.push(Diagnostic::UnexpectedCopyTarget {
        resource_id: resource.resource_id().clone(),
        target_path: resource.target_path().clone(),
        observation: observation.unwrap_or(CopyTargetObservation::UnsafePath {
            parent_safety: crate::domain::actual::ParentSafety::Missing,
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::actual::ActualFileCopy;
    use crate::domain::actual::ActualFileLink;
    use crate::domain::file_copy::ContentFingerprint;
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
    use crate::domain::paths::ResolvedPath;
    use crate::domain::plan::PlannedResourceAction;

    fn path(value: &str) -> ResolvedPath {
        ResolvedPath::new(
            std::env::temp_dir()
                .join("loadout-copy-planner")
                .join(value),
        )
        .unwrap()
    }
    fn fingerprint() -> ContentFingerprint {
        ContentFingerprint::parse(format!("sha256:{}", "a".repeat(64))).unwrap()
    }
    fn desired() -> ResolvedFileCopy {
        ResolvedFileCopy::new(
            FullyQualifiedResourceId::parse("base/git").unwrap(),
            path("store/config"),
            path("home/.config"),
            fingerprint(),
        )
        .unwrap()
    }

    fn known_copy(copy: &ResolvedFileCopy) -> KnownFileCopy {
        KnownFileCopy::from_resolved(copy)
    }

    #[test]
    fn selects_create_for_a_missing_copy_target() {
        let copy = desired();
        let desired =
            ResolvedDesired::new(ProfileId::parse("base").unwrap(), [copy.clone()]).unwrap();
        let actual = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::Missing,
        )
        .unwrap()])
        .unwrap();
        let plan = plan(&desired, &KnownState::empty(), &actual);
        assert!(matches!(
            plan.resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::Create { .. }
            )]
        ));
        assert!(plan.diagnostics().is_empty());
    }

    #[test]
    fn conflict_has_no_copy_mutation_action() {
        let copy = desired();
        let desired =
            ResolvedDesired::new(ProfileId::parse("base").unwrap(), [copy.clone()]).unwrap();
        let actual = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::OtherRegularFile {
                content_fingerprint: fingerprint(),
            },
        )
        .unwrap()])
        .unwrap();
        let plan = plan(&desired, &KnownState::empty(), &actual);
        assert!(plan.resource_actions().is_empty());
        assert!(!plan.is_executable());
    }

    #[test]
    fn selects_replace_effect_for_an_expected_managed_link() {
        let copy = desired();
        let link = ResolvedFileLink::new(
            copy.resource_id().clone(),
            path("store/old"),
            copy.target_path().clone(),
        )
        .unwrap();
        let known = KnownState::new([KnownResource::from(
            crate::domain::known::KnownFileLink::from_resolved(&link),
        )])
        .unwrap();
        let desired =
            ResolvedDesired::new(ProfileId::parse("base").unwrap(), [copy.clone()]).unwrap();
        let actual = ActualState::new([ActualFileLink::new(
            copy.target_path().clone(),
            TargetObservation::ExpectedLink {
                link_target: link.link_target().clone(),
            },
        )
        .unwrap()])
        .unwrap();
        let plan = plan(&desired, &known, &actual);
        assert!(matches!(
            plan.resource_actions(),
            [PlannedResourceAction::ReplaceEffect(_)]
        ));
    }

    #[test]
    fn selects_replace_effect_for_an_expected_managed_copy() {
        let copy = desired();
        let known = KnownState::new([KnownResource::from(
            crate::domain::known::KnownFileCopy::from_resolved(&copy),
        )])
        .unwrap();
        let link = ResolvedFileLink::new(
            copy.resource_id().clone(),
            path("store/link"),
            copy.target_path().clone(),
        )
        .unwrap();
        let desired =
            ResolvedDesired::new(ProfileId::parse("base").unwrap(), [link.clone()]).unwrap();
        let actual = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::ExpectedCopy {
                content_fingerprint: fingerprint(),
            },
        )
        .unwrap()])
        .unwrap();
        let plan = plan(&desired, &known, &actual);
        assert!(matches!(
            plan.resource_actions(),
            [PlannedResourceAction::ReplaceEffect(_)]
        ));
    }

    #[test]
    fn selects_noop_or_replace_from_the_source_fingerprint() {
        let copy = desired();
        let known = KnownState::new([KnownResource::from(known_copy(&copy))]).unwrap();
        let desired =
            ResolvedDesired::new(ProfileId::parse("base").unwrap(), [copy.clone()]).unwrap();
        let actual = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::ExpectedCopy {
                content_fingerprint: fingerprint(),
            },
        )
        .unwrap()])
        .unwrap();
        assert!(matches!(
            plan(&desired, &known, &actual).resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::Noop { .. }
            )]
        ));

        let changed = ResolvedFileCopy::new(
            copy.resource_id().clone(),
            copy.source_path().clone(),
            copy.target_path().clone(),
            ContentFingerprint::parse(format!("sha256:{}", "b".repeat(64))).unwrap(),
        )
        .unwrap();
        let desired = ResolvedDesired::new(ProfileId::parse("base").unwrap(), [changed]).unwrap();
        assert!(matches!(
            plan(&desired, &known, &actual).resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::Replace { .. }
            )]
        ));
    }

    #[test]
    fn selects_remove_or_forget_for_a_stale_copy() {
        let copy = desired();
        let known = KnownState::new([KnownResource::from(known_copy(&copy))]).unwrap();
        let desired = ResolvedDesired::new(
            ProfileId::parse("base").unwrap(),
            Vec::<ResolvedResource>::new(),
        )
        .unwrap();
        let expected = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::ExpectedCopy {
                content_fingerprint: fingerprint(),
            },
        )
        .unwrap()])
        .unwrap();
        assert!(matches!(
            plan(&desired, &known, &expected).resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::Remove { .. }
            )]
        ));
        let missing = ActualState::new([ActualFileCopy::new(
            copy.target_path().clone(),
            CopyTargetObservation::Missing,
        )
        .unwrap()])
        .unwrap();
        assert!(matches!(
            plan(&desired, &known, &missing).resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::ForgetMissing { .. }
            )]
        ));
    }

    #[test]
    fn selects_relocate_when_the_old_owned_copy_and_new_target_are_proven() {
        let old = desired();
        let new_target = path("home/.config-relocated");
        let relocated = ResolvedFileCopy::new(
            old.resource_id().clone(),
            old.source_path().clone(),
            new_target.clone(),
            old.source_content_fingerprint().clone(),
        )
        .unwrap();
        let desired = ResolvedDesired::new(ProfileId::parse("base").unwrap(), [relocated]).unwrap();
        let known = KnownState::new([KnownResource::from(known_copy(&old))]).unwrap();
        let actual = ActualState::new([
            ActualFileCopy::new(
                old.target_path().clone(),
                CopyTargetObservation::ExpectedCopy {
                    content_fingerprint: fingerprint(),
                },
            )
            .unwrap(),
            ActualFileCopy::new(new_target, CopyTargetObservation::Missing).unwrap(),
        ])
        .unwrap();

        assert!(matches!(
            plan(&desired, &known, &actual).resource_actions(),
            [PlannedResourceAction::FileCopy(
                PlannedFileCopyAction::Relocate { .. }
            )]
        ));
    }
}
