//! Immediate safety rechecks and execution for planned file-link actions.

use std::fmt;
use std::io;
use std::path::Path;

use crate::domain::actual::TargetObservation;
use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, PlannedAction, TargetCondition};
#[cfg(windows)]
use crate::filesystem::remove_expected_file_symbolic_link_entry;
#[cfg(windows)]
use crate::filesystem::{
    create_file_symbolic_link_no_replace, replace_file_symbolic_link_from_temporary,
};
use crate::filesystem::{
    ensure_file_symbolic_link_creation_supported, ensure_file_symbolic_link_removal_supported,
    ensure_file_symbolic_link_replacement_supported,
};
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::inspection::source::{SourceVerificationError, VerifiedSource};
use crate::state::operation::{RecordedAction, RelocationFacts};

/// Executes filesystem effects selected by the planner for one home directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileLinkExecutor {
    inspector: FileLinkInspector,
    #[cfg(test)]
    force_capability_failure: bool,
}

impl FileLinkExecutor {
    /// Creates an executor that rechecks targets below this user's home directory.
    pub(crate) fn new(home_directory: &Path) -> Result<Self, TargetInspectionError> {
        Ok(Self {
            inspector: FileLinkInspector::new(home_directory)?,
            #[cfg(test)]
            force_capability_failure: false,
        })
    }

    /// Rechecks and creates exactly one planned `create_link` action.
    ///
    /// This method never writes Known state or operation progress. Its caller is responsible for recording `running` before calling it and committing `succeeded` only after this method proves the post-condition.
    pub(crate) fn execute_create(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), CreateLinkExecutionError> {
        let (target_path, link_target) = self.recheck_create(action, source)?;

        let physical_target_path = self
            .inspector
            .physical_target_path_for_execution(&target_path)
            .map_err(CreateLinkExecutionError::TargetInspection)?;
        self.ensure_create_capability(&target_path, &physical_target_path)?;
        #[cfg(unix)]
        {
            use crate::filesystem::ExecutionTarget;
            let inspection_error = |source| TargetInspectionError::TargetMetadata {
                target_path: target_path.clone(),
                source,
            };
            let context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &physical_target_path,
            )
            .map_err(|error| CreateLinkExecutionError::TargetInspection(inspection_error(error)))?;
            // A rejected recheck is not an attempted create. In particular it
            // cannot adopt an externally installed matching link as succeeded.
            let checked = context
                .prepare_create(source.physical_root(), &link_target)
                .map_err(|error| {
                    CreateLinkExecutionError::TargetInspection(inspection_error(error))
                })?;
            let attempt = checked.attempt();
            let observation = context
                .observe(&link_target)
                .map_err(&inspection_error)
                .and_then(|observation| {
                    let declared = self
                        .inspector
                        .inspect_target_for_expected_link(&target_path, &link_target)?;
                    if declared.observation() != &observation {
                        return Err(inspection_error(io::Error::other(
                            "recorded-path and retained-parent observations disagree",
                        )));
                    }
                    context.check_association().map_err(&inspection_error)?;
                    Ok(observation)
                });
            if let Err(source) = attempt {
                return match observation {
                    Ok(aftermath) => Err(CreateLinkExecutionError::CreateAttemptFailed {
                        target_path,
                        source,
                        aftermath,
                    }),
                    Err(error) => Err(CreateLinkExecutionError::CreateAftermathUnproven {
                        inspection: Box::new(error),
                        target_path,
                        source,
                    }),
                };
            }
            let observation =
                observation.map_err(CreateLinkExecutionError::PostconditionInspection)?;
            match observation {
                TargetObservation::ExpectedLink { .. } => Ok(()),
                observation => Err(CreateLinkExecutionError::PostconditionNotMet {
                    target_path,
                    observation,
                }),
            }
        }
        #[cfg(windows)]
        {
            if let Err(source) = create_file_symbolic_link_no_replace(
                self.inspector.canonical_home(),
                &physical_target_path,
                &link_target,
                source.physical_root(),
            ) {
                return match self
                    .inspector
                    .inspect_target_for_expected_link(&target_path, &link_target)
                {
                    Ok(after) => Err(CreateLinkExecutionError::CreateAttemptFailed {
                        target_path,
                        source,
                        aftermath: after.observation().clone(),
                    }),
                    Err(inspection) => Err(CreateLinkExecutionError::CreateAftermathUnproven {
                        target_path,
                        source,
                        inspection: Box::new(inspection),
                    }),
                };
            }

            let after = self
                .inspector
                .inspect_target_for_expected_link(&target_path, &link_target)
                .map_err(CreateLinkExecutionError::PostconditionInspection)?;
            match after.observation() {
                TargetObservation::ExpectedLink {
                    link_target: observed,
                } if observed == &link_target => Ok(()),
                observation => Err(CreateLinkExecutionError::PostconditionNotMet {
                    target_path,
                    observation: observation.clone(),
                }),
            }
        }
    }

    /// Performs the non-mutating checks required before an operation record is created. `execute_create` repeats these checks immediately before its mutation, so this preflight result is never treated as authorization to skip the executor recheck.
    pub(crate) fn preflight_create(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), CreateLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        let (target_path, _) = self.recheck_create(action, source)?;
        let physical_target_path = self
            .inspector
            .physical_target_path_for_execution(&target_path)
            .map_err(CreateLinkExecutionError::TargetInspection)?;
        self.ensure_create_capability(&target_path, &physical_target_path)
    }

    /// Rechecks and atomically replaces one managed link using the sibling path already persisted in its `running` operation record.
    pub(crate) fn execute_replace(
        &self,
        action: &PlannedAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        let facts = recorded
            .replacement_facts()
            .ok_or(ReplaceLinkExecutionError::MissingRecordedFacts)?;
        if !matches!(
            action.kind(),
            ActionKind::ReplaceLink | ActionKind::ReplaceOwnership
        ) || action.resource_id() != recorded.resource_id()
        {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let reverified = source
            .reverify()
            .map_err(ReplaceLinkExecutionError::SourceRecheck)?;
        if reverified.path() != facts.new_link_target().as_path() {
            return Err(ReplaceLinkExecutionError::SourceDoesNotMatchAction {
                expected: facts.new_link_target().clone(),
                actual: reverified.path().clone(),
            });
        }
        let before = self
            .inspector
            .inspect_target_for_expected_link(facts.target_path(), facts.old_link_target())
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        if !matches!(before.observation(), TargetObservation::ExpectedLink { .. }) {
            return Err(ReplaceLinkExecutionError::PreconditionNoLongerHolds {
                target_path: facts.target_path().clone(),
                observation: before.observation().clone(),
            });
        }
        let temporary_before = self
            .inspector
            .inspect_target_for_expected_link(facts.temporary_path(), facts.new_link_target())
            .map_err(ReplaceLinkExecutionError::TemporaryInspection)?;
        if !matches!(temporary_before.observation(), TargetObservation::Missing) {
            return Err(ReplaceLinkExecutionError::TemporaryNotMissing {
                temporary_path: facts.temporary_path().clone(),
                observation: temporary_before.observation().clone(),
            });
        }
        let target = self
            .inspector
            .physical_target_path_for_execution(facts.target_path())
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(ReplaceLinkExecutionError::TemporaryInspection)?;
        self.ensure_replace_capability(facts.target_path(), &target)?;
        #[cfg(unix)]
        {
            use crate::filesystem::ExecutionTarget;
            let target_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &target,
            )
            .map_err(|source| {
                ReplaceLinkExecutionError::TargetInspection(execution_target_inspection(
                    facts.target_path(),
                    source,
                ))
            })?;
            let temporary_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &temporary,
            )
            .map_err(|source| {
                ReplaceLinkExecutionError::TemporaryInspection(execution_target_inspection(
                    facts.temporary_path(),
                    source,
                ))
            })?;
            let create = temporary_context
                .prepare_create(source.physical_root(), facts.new_link_target())
                .and_then(|checked| checked.attempt_temporary());
            if let Err(source) = create {
                return Err(self.replacement_context_attempt_aftermath(
                    &facts,
                    source,
                    ReplacementMutation::TemporaryCreate,
                    &target_context,
                    &temporary_context,
                ));
            }
            if !matches!(
                self.observe_execution_target(
                    &temporary_context,
                    facts.temporary_path(),
                    facts.new_link_target()
                )
                .map_err(ReplaceLinkExecutionError::TemporaryInspection)?,
                TargetObservation::ExpectedLink { .. }
            ) {
                return Err(self.replacement_context_aftermath_error(
                    &facts,
                    &target_context,
                    &temporary_context,
                )?);
            }
            #[cfg(test)]
            crate::test_support::execution_boundary(
                crate::test_support::ExecutionBoundary::BeforeReplacementRenameRecheck,
            )
            .map_err(|source| {
                ReplaceLinkExecutionError::TargetInspection(execution_target_inspection(
                    facts.target_path(),
                    source,
                ))
            })?;
            let rename = target_context
                .prepare_replace(
                    &temporary,
                    facts.old_link_target(),
                    facts.new_link_target(),
                    source.physical_root(),
                )
                .and_then(|checked| checked.attempt());
            if let Err(source) = rename {
                return Err(self.replacement_context_attempt_aftermath(
                    &facts,
                    source,
                    ReplacementMutation::Rename,
                    &target_context,
                    &temporary_context,
                ));
            }
            let aftermath =
                self.replacement_context_aftermath(&facts, &target_context, &temporary_context)?;
            if aftermath.postcondition_holds() {
                Ok(())
            } else {
                Err(ReplaceLinkExecutionError::Aftermath {
                    aftermath: Box::new(aftermath),
                })
            }
        }
        #[cfg(windows)]
        {
            if let Err(source) = create_file_symbolic_link_no_replace(
                self.inspector.canonical_home(),
                &temporary,
                facts.new_link_target(),
                source.physical_root(),
            ) {
                return Err(self.replacement_attempt_aftermath(
                    &facts,
                    source,
                    ReplacementMutation::TemporaryCreate,
                ));
            }
            if let Err(source) = replace_file_symbolic_link_from_temporary(
                self.inspector.canonical_home(),
                &target,
                &temporary,
            ) {
                return Err(self.replacement_attempt_aftermath(
                    &facts,
                    source,
                    ReplacementMutation::Rename,
                ));
            }
            let aftermath = self.replacement_aftermath(&facts)?;
            if aftermath.postcondition_holds() {
                Ok(())
            } else {
                Err(ReplaceLinkExecutionError::Aftermath {
                    aftermath: Box::new(aftermath),
                })
            }
        }
    }

    /// Rechecks the state-only same-source ownership handoff. It deliberately performs no target mutation and does not allocate a temporary sibling.
    pub(crate) fn execute_same_source_ownership_handoff(
        &self,
        action: &PlannedAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        if action.kind() != ActionKind::ReplaceOwnership || recorded.replacement_facts().is_some() {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let verified = source
            .reverify()
            .map_err(ReplaceLinkExecutionError::SourceRecheck)?;
        let preconditions = action.preconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path,
                link_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        let postconditions = action.postconditions();
        let [
            TargetCondition::ExpectedLink {
                link_target: new_link_target,
                ..
            },
        ] = postconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        if link_target != new_link_target || verified.path() != link_target.as_path() {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let actual = self
            .inspector
            .inspect_target_for_expected_link(target_path, link_target)
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        if matches!(actual.observation(), TargetObservation::ExpectedLink { .. }) {
            Ok(())
        } else {
            Err(ReplaceLinkExecutionError::PreconditionNoLongerHolds {
                target_path: target_path.clone(),
                observation: actual.observation().clone(),
            })
        }
    }

    pub(crate) fn preflight_same_source_ownership_handoff(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        if action.kind() != ActionKind::ReplaceOwnership {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        self.preflight_unchanged_link(action, source)
    }

    /// Revalidates an already satisfied action without requiring mutation capability.
    pub(crate) fn preflight_noop(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        if action.kind() != ActionKind::Noop {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        self.preflight_unchanged_link(action, source)
    }

    fn preflight_unchanged_link(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        let preconditions = action.preconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path,
                link_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        let postconditions = action.postconditions();
        let [
            TargetCondition::ExpectedLink {
                link_target: new_link_target,
                ..
            },
        ] = postconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        if link_target != new_link_target {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let verified = source
            .reverify()
            .map_err(ReplaceLinkExecutionError::SourceRecheck)?;
        if verified.path() != link_target.as_path() {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let actual = self
            .inspector
            .inspect_target_for_expected_link(target_path, link_target)
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        if matches!(actual.observation(), TargetObservation::ExpectedLink { .. }) {
            Ok(())
        } else {
            Err(ReplaceLinkExecutionError::PreconditionNoLongerHolds {
                target_path: target_path.clone(),
                observation: actual.observation().clone(),
            })
        }
    }

    /// Preflight validates every fact available before the temporary sibling is recorded.
    pub(crate) fn preflight_replace(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), ReplaceLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        if !matches!(
            action.kind(),
            ActionKind::ReplaceLink | ActionKind::ReplaceOwnership
        ) {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let preconditions = action.preconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path,
                link_target: old_link_target,
            },
        ] = preconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        let postconditions = action.postconditions();
        let [
            TargetCondition::ExpectedLink {
                target_path: post_target,
                link_target: new_link_target,
            },
        ] = postconditions.as_slice()
        else {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        };
        if target_path != post_target || old_link_target == new_link_target {
            return Err(ReplaceLinkExecutionError::InvalidReplaceConditions);
        }
        let verified = source
            .reverify()
            .map_err(ReplaceLinkExecutionError::SourceRecheck)?;
        if verified.path() != new_link_target.as_path() {
            return Err(ReplaceLinkExecutionError::SourceDoesNotMatchAction {
                expected: new_link_target.clone(),
                actual: verified.path().clone(),
            });
        }
        let before = self
            .inspector
            .inspect_target_for_expected_link(target_path, old_link_target)
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        if !matches!(before.observation(), TargetObservation::ExpectedLink { .. }) {
            return Err(ReplaceLinkExecutionError::PreconditionNoLongerHolds {
                target_path: target_path.clone(),
                observation: before.observation().clone(),
            });
        }
        let physical = self
            .inspector
            .physical_target_path_for_execution(target_path)
            .map_err(ReplaceLinkExecutionError::TargetInspection)?;
        self.ensure_replace_capability(target_path, &physical)
    }

    /// Rechecks a relocation before it is recorded. Both mutation capabilities must be available before creating the new entry, because a relocation must never knowingly leave two declared targets behind.
    pub(crate) fn preflight_relocate(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), RelocateLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        let facts = relocation_facts_from_action(action)?;
        self.recheck_relocation_preconditions(&facts, source)?;
        self.ensure_relocation_capabilities(&facts)
    }

    /// Creates and verifies the new link, then removes and verifies the old link as one contiguous action. Every attempted-mutation failure is classified from observations of both recorded targets.
    pub(crate) fn execute_relocate(
        &self,
        action: &PlannedAction,
        recorded: &RecordedAction,
        source: &VerifiedSource,
    ) -> Result<(), RelocateLinkExecutionError> {
        let facts = recorded
            .relocation_facts()
            .ok_or(RelocateLinkExecutionError::MissingRecordedFacts)?;
        if action.kind() != ActionKind::RelocateLink
            || action.resource_id() != recorded.resource_id()
            || relocation_facts_from_action(action)? != facts
        {
            return Err(RelocateLinkExecutionError::InvalidRelocateConditions);
        }
        self.recheck_relocation_preconditions(&facts, source)?;
        self.ensure_relocation_capabilities(&facts)?;

        let new_physical = self
            .inspector
            .physical_target_path_for_execution(facts.new_target_path())
            .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
        #[cfg(unix)]
        {
            use crate::filesystem::ExecutionTarget;
            let new_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &new_physical,
            )
            .map_err(|source| {
                RelocateLinkExecutionError::NewTargetInspection(execution_target_inspection(
                    facts.new_target_path(),
                    source,
                ))
            })?;
            let old_physical = self
                .inspector
                .physical_target_path_for_execution(facts.old_target_path())
                .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
            let old_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &old_physical,
            )
            .map_err(|source| {
                RelocateLinkExecutionError::OldTargetInspection(execution_target_inspection(
                    facts.old_target_path(),
                    source,
                ))
            })?;
            let create = new_context
                .prepare_create(source.physical_root(), facts.new_link_target())
                .and_then(|checked| checked.attempt());
            if create.is_err() {
                return Err(self.relocation_context_aftermath_error(
                    &facts,
                    &old_context,
                    &new_context,
                )?);
            }
            if !matches!(
                self.observe_execution_target(
                    &new_context,
                    facts.new_target_path(),
                    facts.new_link_target()
                )
                .map_err(RelocateLinkExecutionError::NewTargetInspection)?,
                TargetObservation::ExpectedLink { .. }
            ) {
                return Err(self.relocation_context_aftermath_error(
                    &facts,
                    &old_context,
                    &new_context,
                )?);
            }
            #[cfg(test)]
            crate::test_support::execution_boundary(
                crate::test_support::ExecutionBoundary::BeforeRelocateRemovalRecheck,
            )
            .map_err(|source| {
                RelocateLinkExecutionError::OldTargetInspection(execution_target_inspection(
                    facts.old_target_path(),
                    source,
                ))
            })?;
            let reverified = source
                .reverify()
                .map_err(RelocateLinkExecutionError::SourceRecheck)?;
            if reverified.path() != facts.new_link_target().as_path() {
                return Err(RelocateLinkExecutionError::SourceDoesNotMatchAction {
                    expected: facts.new_link_target().clone(),
                    actual: reverified.path().clone(),
                });
            }
            new_context
                .recheck_expected_link(facts.new_link_target())
                .map_err(|source| {
                    RelocateLinkExecutionError::NewTargetInspection(execution_target_inspection(
                        facts.new_target_path(),
                        source,
                    ))
                })?;
            // This is the final old-entry recheck. No action is interleaved between the complete relocation predicate recheck and unlinkat.
            let remove = old_context
                .prepare_remove(facts.old_link_target())
                .and_then(|checked| checked.attempt());
            if remove.is_err() {
                return Err(self.relocation_context_aftermath_error(
                    &facts,
                    &old_context,
                    &new_context,
                )?);
            }
            let aftermath =
                self.relocation_context_observations(&facts, &old_context, &new_context)?;
            if matches!(aftermath.0, TargetObservation::Missing)
                && matches!(aftermath.1, TargetObservation::ExpectedLink { .. })
            {
                Ok(())
            } else {
                Err(RelocateLinkExecutionError::Aftermath {
                    old_observation: aftermath.0,
                    new_observation: aftermath.1,
                })
            }
        }
        #[cfg(windows)]
        {
            if create_file_symbolic_link_no_replace(
                self.inspector.canonical_home(),
                &new_physical,
                facts.new_link_target(),
                source.physical_root(),
            )
            .is_err()
            {
                return Err(self.relocation_aftermath_error(&facts)?);
            }
            let new_after = self
                .inspector
                .inspect_target_for_expected_link(facts.new_target_path(), facts.new_link_target())
                .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
            if !matches!(
                new_after.observation(),
                TargetObservation::ExpectedLink { .. }
            ) {
                return Err(self.relocation_aftermath_error(&facts)?);
            }

            let old_after_create = self
                .inspector
                .inspect_target_for_expected_link(facts.old_target_path(), facts.old_link_target())
                .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
            if !matches!(
                old_after_create.observation(),
                TargetObservation::ExpectedLink { .. }
            ) {
                return Err(self.relocation_aftermath_error(&facts)?);
            }
            let old_physical = self
                .inspector
                .physical_target_path_for_execution(facts.old_target_path())
                .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
            if remove_expected_file_symbolic_link_entry(
                self.inspector.canonical_home(),
                &old_physical,
                facts.old_link_target(),
            )
            .is_err()
            {
                return Err(self.relocation_aftermath_error(&facts)?);
            }
            let aftermath = self.relocation_observations(&facts)?;
            if matches!(aftermath.0, TargetObservation::Missing)
                && matches!(aftermath.1, TargetObservation::ExpectedLink { .. })
            {
                Ok(())
            } else {
                Err(RelocateLinkExecutionError::Aftermath {
                    old_observation: aftermath.0,
                    new_observation: aftermath.1,
                })
            }
        }
    }

    /// Rechecks and removes exactly one planned `remove_link` action.
    ///
    /// This method removes only the final link entry. It never follows the referent, mutates a parent directory, writes Known state, or selects a replacement action after a failed ownership recheck.
    pub(crate) fn execute_remove(
        &self,
        action: &PlannedAction,
    ) -> Result<(), RemoveLinkExecutionError> {
        let (target_path, link_target) = self.recheck_remove(action)?;
        let physical_target_path = self
            .inspector
            .physical_target_path_for_execution(&target_path)
            .map_err(RemoveLinkExecutionError::TargetInspection)?;
        self.ensure_remove_capability(&target_path, &physical_target_path)?;

        #[cfg(unix)]
        {
            use crate::filesystem::ExecutionTarget;
            let context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &physical_target_path,
            )
            .map_err(|source| {
                RemoveLinkExecutionError::TargetInspection(execution_target_inspection(
                    &target_path,
                    source,
                ))
            })?;
            let attempt = context
                .prepare_remove(&link_target)
                .and_then(|checked| checked.attempt());
            let observation = self.observe_execution_target(&context, &target_path, &link_target);
            if let Err(source) = attempt {
                return match observation {
                    Ok(aftermath) => Err(RemoveLinkExecutionError::RemoveAttemptFailed {
                        target_path,
                        source,
                        aftermath,
                    }),
                    Err(inspection) => Err(RemoveLinkExecutionError::RemoveAftermathUnproven {
                        target_path,
                        source,
                        inspection: Box::new(inspection),
                    }),
                };
            }
            match observation.map_err(RemoveLinkExecutionError::PostconditionInspection)? {
                TargetObservation::Missing => Ok(()),
                observation => Err(RemoveLinkExecutionError::PostconditionNotMet {
                    target_path,
                    observation,
                }),
            }
        }

        #[cfg(windows)]
        {
            if let Err(source) = remove_expected_file_symbolic_link_entry(
                self.inspector.canonical_home(),
                &physical_target_path,
                &link_target,
            ) {
                return match self
                    .inspector
                    .inspect_target_for_expected_link(&target_path, &link_target)
                {
                    Ok(after) => Err(RemoveLinkExecutionError::RemoveAttemptFailed {
                        target_path,
                        source,
                        aftermath: after.observation().clone(),
                    }),
                    Err(inspection) => Err(RemoveLinkExecutionError::RemoveAftermathUnproven {
                        target_path,
                        source,
                        inspection: Box::new(inspection),
                    }),
                };
            }

            let after = self
                .inspector
                .inspect_target_for_expected_link(&target_path, &link_target)
                .map_err(RemoveLinkExecutionError::PostconditionInspection)?;
            match after.observation() {
                TargetObservation::Missing => Ok(()),
                observation => Err(RemoveLinkExecutionError::PostconditionNotMet {
                    target_path,
                    observation: observation.clone(),
                }),
            }
        }
    }

    /// Removes only an exact, action-local replacement temporary during recovery.
    ///
    /// Recovery never resumes a replacement: it may perform this limited cleanup only while the recorded old link still proves the replacement did not occur.
    pub(crate) fn cleanup_recorded_replacement_temporary(
        &self,
        recorded: &RecordedAction,
    ) -> Result<(), ReplacementTemporaryCleanupError> {
        let facts = recorded
            .replacement_facts()
            .ok_or(ReplacementTemporaryCleanupError::MissingRecordedFacts)?;
        let target = self
            .inspector
            .physical_target_path_for_execution(facts.target_path())
            .map_err(ReplacementTemporaryCleanupError::TargetInspection)?;
        let temporary = self
            .inspector
            .physical_target_path_for_execution(facts.temporary_path())
            .map_err(ReplacementTemporaryCleanupError::TemporaryInspection)?;
        self.ensure_remove_capability(facts.temporary_path(), &temporary)
            .map_err(ReplacementTemporaryCleanupError::RemoveCapability)?;

        #[cfg(unix)]
        {
            use crate::filesystem::ExecutionTarget;
            let target_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &target,
            )
            .map_err(|source| {
                ReplacementTemporaryCleanupError::TargetInspection(execution_target_inspection(
                    facts.target_path(),
                    source,
                ))
            })?;
            let temporary_context = ExecutionTarget::open_with_declared_root(
                self.inspector.canonical_home(),
                self.inspector.declared_home(),
                &temporary,
            )
            .map_err(|source| {
                ReplacementTemporaryCleanupError::TemporaryInspection(execution_target_inspection(
                    facts.temporary_path(),
                    source,
                ))
            })?;

            // A missing recorded temporary needs no cleanup. The following recovery
            // classification decides whether the action can be marked failed.
            if matches!(
                self.observe_execution_target(
                    &temporary_context,
                    facts.temporary_path(),
                    facts.new_link_target(),
                )
                .map_err(ReplacementTemporaryCleanupError::TemporaryInspection)?,
                TargetObservation::Missing
            ) {
                return Ok(());
            }

            // Do not clean up after a replacement that may already have happened.
            target_context
                .recheck_expected_link(facts.old_link_target())
                .map_err(|source| {
                    ReplacementTemporaryCleanupError::TargetInspection(execution_target_inspection(
                        facts.target_path(),
                        source,
                    ))
                })?;
            let attempt = temporary_context
                .prepare_remove(facts.new_link_target())
                .and_then(|checked| checked.attempt());
            let aftermath = self
                .observe_execution_target(
                    &temporary_context,
                    facts.temporary_path(),
                    facts.new_link_target(),
                )
                .map_err(ReplacementTemporaryCleanupError::TemporaryInspection)?;
            if matches!(aftermath, TargetObservation::Missing) {
                // As with a normal removal, an OS error does not outweigh a proven
                // recorded post-condition under the filesystem concurrency contract.
                return Ok(());
            }
            match attempt {
                Ok(()) => Err(ReplacementTemporaryCleanupError::PostconditionNotMet {
                    temporary_path: facts.temporary_path().clone(),
                    observation: aftermath,
                }),
                Err(source) => Err(ReplacementTemporaryCleanupError::RemoveAttemptFailed {
                    temporary_path: facts.temporary_path().clone(),
                    source,
                    aftermath,
                }),
            }
        }

        #[cfg(windows)]
        {
            let _ = target;
            Err(ReplacementTemporaryCleanupError::RemoveCapability(
                RemoveLinkExecutionError::PlatformCapability {
                    target_path: facts.temporary_path().clone(),
                    source: io::Error::other("replacement temporary cleanup is unavailable"),
                },
            ))
        }
    }

    #[cfg(unix)]
    fn observe_execution_target(
        &self,
        context: &crate::filesystem::ExecutionTarget,
        target_path: &ResolvedPath,
        link_target: &LinkTarget,
    ) -> Result<TargetObservation, TargetInspectionError> {
        let observation = context
            .observe(link_target)
            .map_err(|source| execution_target_inspection(target_path, source))?;
        let declared = self.inspect_target_for_execution(target_path, link_target)?;
        if declared != observation {
            return Err(execution_target_inspection(
                target_path,
                io::Error::other("recorded-path and retained-parent observations disagree"),
            ));
        }
        context
            .check_association()
            .map_err(|source| execution_target_inspection(target_path, source))?;
        Ok(observation)
    }

    #[cfg(unix)]
    fn inspect_target_for_execution(
        &self,
        target_path: &ResolvedPath,
        link_target: &LinkTarget,
    ) -> Result<TargetObservation, TargetInspectionError> {
        Ok(self
            .inspector
            .inspect_target_for_expected_link(target_path, link_target)?
            .observation()
            .clone())
    }

    #[cfg(unix)]
    fn replacement_context_aftermath(
        &self,
        facts: &crate::state::operation::ReplacementFacts,
        target: &crate::filesystem::ExecutionTarget,
        temporary: &crate::filesystem::ExecutionTarget,
    ) -> Result<ReplacementAftermath, ReplaceLinkExecutionError> {
        Ok(ReplacementAftermath {
            postcondition: self
                .observe_execution_target(target, facts.target_path(), facts.new_link_target())
                .map_err(ReplaceLinkExecutionError::PostconditionInspection)?,
            precondition: self
                .observe_execution_target(target, facts.target_path(), facts.old_link_target())
                .map_err(ReplaceLinkExecutionError::TargetInspection)?,
            temporary: self
                .observe_execution_target(
                    temporary,
                    facts.temporary_path(),
                    facts.new_link_target(),
                )
                .map_err(ReplaceLinkExecutionError::TemporaryInspection)?,
        })
    }

    #[cfg(unix)]
    fn replacement_context_aftermath_error(
        &self,
        facts: &crate::state::operation::ReplacementFacts,
        target: &crate::filesystem::ExecutionTarget,
        temporary: &crate::filesystem::ExecutionTarget,
    ) -> Result<ReplaceLinkExecutionError, ReplaceLinkExecutionError> {
        Ok(ReplaceLinkExecutionError::Aftermath {
            aftermath: Box::new(self.replacement_context_aftermath(facts, target, temporary)?),
        })
    }

    #[cfg(unix)]
    fn replacement_context_attempt_aftermath(
        &self,
        facts: &crate::state::operation::ReplacementFacts,
        source: io::Error,
        mutation: ReplacementMutation,
        target: &crate::filesystem::ExecutionTarget,
        temporary: &crate::filesystem::ExecutionTarget,
    ) -> ReplaceLinkExecutionError {
        match self.replacement_context_aftermath(facts, target, temporary) {
            Ok(aftermath) => ReplaceLinkExecutionError::MutationAttempt {
                source,
                aftermath: Box::new(aftermath),
            },
            Err(_) => ReplaceLinkExecutionError::MutationAftermathUnproven { mutation, source },
        }
    }

    #[cfg(unix)]
    fn relocation_context_observations(
        &self,
        facts: &RelocationFacts,
        old: &crate::filesystem::ExecutionTarget,
        new: &crate::filesystem::ExecutionTarget,
    ) -> Result<(TargetObservation, TargetObservation), RelocateLinkExecutionError> {
        let old = self
            .observe_execution_target(old, facts.old_target_path(), facts.old_link_target())
            .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
        let new = self
            .observe_execution_target(new, facts.new_target_path(), facts.new_link_target())
            .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
        Ok((old, new))
    }

    #[cfg(unix)]
    fn relocation_context_aftermath_error(
        &self,
        facts: &RelocationFacts,
        old: &crate::filesystem::ExecutionTarget,
        new: &crate::filesystem::ExecutionTarget,
    ) -> Result<RelocateLinkExecutionError, RelocateLinkExecutionError> {
        let (old_observation, new_observation) =
            self.relocation_context_observations(facts, old, new)?;
        Ok(RelocateLinkExecutionError::Aftermath {
            old_observation,
            new_observation,
        })
    }

    /// Performs the non-mutating checks required before a `remove_link` operation record is created. `execute_remove` repeats these checks immediately before the link-entry removal.
    pub(crate) fn preflight_remove(
        &self,
        action: &PlannedAction,
    ) -> Result<(), RemoveLinkExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        let (target_path, _) = self.recheck_remove(action)?;
        let physical_target_path = self
            .inspector
            .physical_target_path_for_execution(&target_path)
            .map_err(RemoveLinkExecutionError::TargetInspection)?;
        self.ensure_remove_capability(&target_path, &physical_target_path)
    }

    /// Rechecks the `forget_missing` post-condition before the state repository deletes only the stale Known record. This action has no filesystem mutation and does not inspect a source.
    pub(crate) fn execute_forget_missing(
        &self,
        action: &PlannedAction,
    ) -> Result<(), ForgetMissingExecutionError> {
        let target_path = forget_missing_conditions(action)?;
        let after = self
            .inspector
            .inspect_target_for_expected_link(&target_path, &LinkTarget::new(target_path.clone()))
            .map_err(ForgetMissingExecutionError::TargetInspection)?;
        match after.observation() {
            TargetObservation::Missing => Ok(()),
            observation => Err(ForgetMissingExecutionError::PostconditionNotMet {
                target_path,
                observation: observation.clone(),
            }),
        }
    }

    /// Rechecks the missing-target precondition during preflight without
    /// changing the filesystem or Known state.
    pub(crate) fn preflight_forget_missing(
        &self,
        action: &PlannedAction,
    ) -> Result<(), ForgetMissingExecutionError> {
        #[cfg(test)]
        crate::test_support::assert_mutation_allowed();
        self.execute_forget_missing(action)
    }

    fn ensure_create_capability(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
    ) -> Result<(), CreateLinkExecutionError> {
        let parent_path = physical_target_path
            .as_ref()
            .parent()
            .expect("a validated file-link target must have a parent");
        let parent_path = ResolvedPath::new(parent_path.to_path_buf())
            .expect("a validated file-link target parent must be resolved");

        #[cfg(test)]
        if self.force_capability_failure {
            return Err(CreateLinkExecutionError::PlatformCapability {
                target_path: target_path.clone(),
                source: io::Error::new(
                    io::ErrorKind::Unsupported,
                    "injected file-symbolic-link capability failure",
                ),
            });
        }

        ensure_file_symbolic_link_creation_supported(&parent_path).map_err(|source| {
            CreateLinkExecutionError::PlatformCapability {
                target_path: target_path.clone(),
                source,
            }
        })
    }

    fn ensure_remove_capability(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
    ) -> Result<(), RemoveLinkExecutionError> {
        let parent_path = physical_target_path
            .as_ref()
            .parent()
            .expect("a validated file-link target must have a parent");
        let parent_path = ResolvedPath::new(parent_path.to_path_buf())
            .expect("a validated file-link target parent must be resolved");

        ensure_file_symbolic_link_removal_supported(&parent_path).map_err(|source| {
            RemoveLinkExecutionError::PlatformCapability {
                target_path: target_path.clone(),
                source,
            }
        })
    }

    fn ensure_replace_capability(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
    ) -> Result<(), ReplaceLinkExecutionError> {
        let parent = physical_target_path
            .as_ref()
            .parent()
            .expect("validated target has a parent");
        let parent = ResolvedPath::new(parent.to_path_buf()).expect("validated parent is resolved");
        ensure_file_symbolic_link_replacement_supported(&parent).map_err(|source| {
            ReplaceLinkExecutionError::PlatformCapability {
                target_path: target_path.clone(),
                source,
            }
        })
    }

    fn ensure_relocation_capabilities(
        &self,
        facts: &RelocationFacts,
    ) -> Result<(), RelocateLinkExecutionError> {
        let new_physical = self
            .inspector
            .physical_target_path_for_execution(facts.new_target_path())
            .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
        self.ensure_create_capability(facts.new_target_path(), &new_physical)
            .map_err(RelocateLinkExecutionError::CreateCapability)?;
        let old_physical = self
            .inspector
            .physical_target_path_for_execution(facts.old_target_path())
            .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
        self.ensure_remove_capability(facts.old_target_path(), &old_physical)
            .map_err(RelocateLinkExecutionError::RemoveCapability)
    }

    fn recheck_relocation_preconditions(
        &self,
        facts: &RelocationFacts,
        source: &VerifiedSource,
    ) -> Result<(), RelocateLinkExecutionError> {
        let verified = source
            .reverify()
            .map_err(RelocateLinkExecutionError::SourceRecheck)?;
        if verified.path() != facts.new_link_target().as_path() {
            return Err(RelocateLinkExecutionError::SourceDoesNotMatchAction {
                expected: facts.new_link_target().clone(),
                actual: verified.path().clone(),
            });
        }
        let old = self
            .inspector
            .inspect_target_for_expected_link(facts.old_target_path(), facts.old_link_target())
            .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
        if !matches!(old.observation(), TargetObservation::ExpectedLink { .. }) {
            return Err(RelocateLinkExecutionError::PreconditionNoLongerHolds {
                target_path: facts.old_target_path().clone(),
                observation: old.observation().clone(),
            });
        }
        let new = self
            .inspector
            .inspect_target_for_expected_link(facts.new_target_path(), facts.new_link_target())
            .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
        if !matches!(new.observation(), TargetObservation::Missing) {
            return Err(RelocateLinkExecutionError::PreconditionNoLongerHolds {
                target_path: facts.new_target_path().clone(),
                observation: new.observation().clone(),
            });
        }
        Ok(())
    }

    fn relocation_observations(
        &self,
        facts: &RelocationFacts,
    ) -> Result<(TargetObservation, TargetObservation), RelocateLinkExecutionError> {
        let old = self
            .inspector
            .inspect_target_for_expected_link(facts.old_target_path(), facts.old_link_target())
            .map_err(RelocateLinkExecutionError::OldTargetInspection)?;
        let new = self
            .inspector
            .inspect_target_for_expected_link(facts.new_target_path(), facts.new_link_target())
            .map_err(RelocateLinkExecutionError::NewTargetInspection)?;
        Ok((old.observation().clone(), new.observation().clone()))
    }

    fn relocation_aftermath_error(
        &self,
        facts: &RelocationFacts,
    ) -> Result<RelocateLinkExecutionError, RelocateLinkExecutionError> {
        let (old_observation, new_observation) = self.relocation_observations(facts)?;
        Ok(RelocateLinkExecutionError::Aftermath {
            old_observation,
            new_observation,
        })
    }

    fn replacement_aftermath(
        &self,
        facts: &crate::state::operation::ReplacementFacts,
    ) -> Result<ReplacementAftermath, ReplaceLinkExecutionError> {
        let postcondition = self
            .inspector
            .inspect_target_for_expected_link(facts.target_path(), facts.new_link_target())
            .map_err(ReplaceLinkExecutionError::PostconditionInspection)?
            .observation()
            .clone();
        let precondition = self
            .inspector
            .inspect_target_for_expected_link(facts.target_path(), facts.old_link_target())
            .map_err(ReplaceLinkExecutionError::TargetInspection)?
            .observation()
            .clone();
        let temporary = self
            .inspector
            .inspect_target_for_expected_link(facts.temporary_path(), facts.new_link_target())
            .map_err(ReplaceLinkExecutionError::TemporaryInspection)?
            .observation()
            .clone();
        Ok(ReplacementAftermath {
            precondition,
            postcondition,
            temporary,
        })
    }

    fn replacement_attempt_aftermath(
        &self,
        facts: &crate::state::operation::ReplacementFacts,
        source: io::Error,
        mutation: ReplacementMutation,
    ) -> ReplaceLinkExecutionError {
        match self.replacement_aftermath(facts) {
            Ok(aftermath) => ReplaceLinkExecutionError::MutationAttempt {
                source,
                aftermath: Box::new(aftermath),
            },
            Err(_) => ReplaceLinkExecutionError::MutationAftermathUnproven { mutation, source },
        }
    }

    #[cfg(test)]
    pub(crate) fn with_forced_capability_failure_for_test(mut self) -> Self {
        self.force_capability_failure = true;
        self
    }

    fn recheck_create(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(ResolvedPath, LinkTarget), CreateLinkExecutionError> {
        let (target_path, link_target) = create_conditions(action)?;
        let reverified_source = source
            .reverify()
            .map_err(CreateLinkExecutionError::SourceRecheck)?;
        if reverified_source.path() != link_target.as_path() {
            return Err(CreateLinkExecutionError::SourceDoesNotMatchAction {
                expected: link_target,
                actual: reverified_source.path().clone(),
            });
        }

        let before = self
            .inspector
            .inspect_target_for_expected_link(&target_path, &link_target)
            .map_err(CreateLinkExecutionError::TargetInspection)?;
        if !matches!(before.observation(), TargetObservation::Missing) {
            return Err(CreateLinkExecutionError::PreconditionNoLongerHolds {
                target_path,
                observation: before.observation().clone(),
            });
        }
        Ok((before.target_path().clone(), link_target))
    }

    fn recheck_remove(
        &self,
        action: &PlannedAction,
    ) -> Result<(ResolvedPath, LinkTarget), RemoveLinkExecutionError> {
        let (target_path, link_target) = remove_conditions(action)?;
        let before = self
            .inspector
            .inspect_target_for_expected_link(&target_path, &link_target)
            .map_err(RemoveLinkExecutionError::TargetInspection)?;
        if !matches!(before.observation(), TargetObservation::ExpectedLink { .. }) {
            return Err(RemoveLinkExecutionError::PreconditionNoLongerHolds {
                target_path,
                observation: before.observation().clone(),
            });
        }
        Ok((before.target_path().clone(), link_target))
    }
}

fn create_conditions(
    action: &PlannedAction,
) -> Result<(ResolvedPath, LinkTarget), CreateLinkExecutionError> {
    if action.kind() != ActionKind::CreateLink {
        return Err(CreateLinkExecutionError::UnsupportedAction {
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
        return Err(CreateLinkExecutionError::InvalidCreateConditions);
    };
    let [
        TargetCondition::ExpectedLink {
            target_path: post_target,
            link_target,
        },
    ] = postconditions.as_slice()
    else {
        return Err(CreateLinkExecutionError::InvalidCreateConditions);
    };
    if pre_target != post_target {
        return Err(CreateLinkExecutionError::InvalidCreateConditions);
    }
    Ok((pre_target.clone(), link_target.clone()))
}

#[cfg(unix)]
fn execution_target_inspection(
    target_path: &ResolvedPath,
    source: io::Error,
) -> TargetInspectionError {
    TargetInspectionError::TargetMetadata {
        target_path: target_path.clone(),
        source,
    }
}

fn relocation_facts_from_action(
    action: &PlannedAction,
) -> Result<RelocationFacts, RelocateLinkExecutionError> {
    if action.kind() != ActionKind::RelocateLink {
        return Err(RelocateLinkExecutionError::InvalidRelocateConditions);
    }
    let preconditions = action.preconditions();
    let [
        TargetCondition::ExpectedLink {
            target_path: old_target_path,
            link_target: old_link_target,
        },
        TargetCondition::Missing {
            target_path: new_precondition_target,
        },
    ] = preconditions.as_slice()
    else {
        return Err(RelocateLinkExecutionError::InvalidRelocateConditions);
    };
    let postconditions = action.postconditions();
    let [
        TargetCondition::Missing {
            target_path: old_postcondition_target,
        },
        TargetCondition::ExpectedLink {
            target_path: new_target_path,
            link_target: new_link_target,
        },
    ] = postconditions.as_slice()
    else {
        return Err(RelocateLinkExecutionError::InvalidRelocateConditions);
    };
    if old_target_path == new_target_path
        || old_target_path != old_postcondition_target
        || new_precondition_target != new_target_path
    {
        return Err(RelocateLinkExecutionError::InvalidRelocateConditions);
    }
    Ok(RelocationFacts::new_for_executor(
        old_target_path.clone(),
        new_target_path.clone(),
        old_link_target.clone(),
        new_link_target.clone(),
    ))
}

fn remove_conditions(
    action: &PlannedAction,
) -> Result<(ResolvedPath, LinkTarget), RemoveLinkExecutionError> {
    if action.kind() != ActionKind::RemoveLink {
        return Err(RemoveLinkExecutionError::UnsupportedAction {
            kind: action.kind(),
        });
    }
    let preconditions = action.preconditions();
    let postconditions = action.postconditions();
    let [
        TargetCondition::ExpectedLink {
            target_path: pre_target,
            link_target,
        },
    ] = preconditions.as_slice()
    else {
        return Err(RemoveLinkExecutionError::InvalidRemoveConditions);
    };
    let [
        TargetCondition::Missing {
            target_path: post_target,
        },
    ] = postconditions.as_slice()
    else {
        return Err(RemoveLinkExecutionError::InvalidRemoveConditions);
    };
    if pre_target != post_target {
        return Err(RemoveLinkExecutionError::InvalidRemoveConditions);
    }
    Ok((pre_target.clone(), link_target.clone()))
}

fn forget_missing_conditions(
    action: &PlannedAction,
) -> Result<ResolvedPath, ForgetMissingExecutionError> {
    if action.kind() != ActionKind::ForgetMissing {
        return Err(ForgetMissingExecutionError::UnsupportedAction {
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
        return Err(ForgetMissingExecutionError::InvalidForgetMissingConditions);
    };
    let [
        TargetCondition::Missing {
            target_path: post_target,
        },
    ] = postconditions.as_slice()
    else {
        return Err(ForgetMissingExecutionError::InvalidForgetMissingConditions);
    };
    if pre_target != post_target {
        return Err(ForgetMissingExecutionError::InvalidForgetMissingConditions);
    }
    Ok(pre_target.clone())
}

/// The reason a planned create action could not be safely completed and proven.
#[derive(Debug)]
pub(crate) enum CreateLinkExecutionError {
    UnsupportedAction {
        kind: ActionKind,
    },
    InvalidCreateConditions,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction {
        expected: LinkTarget,
        actual: ResolvedPath,
    },
    TargetInspection(TargetInspectionError),
    PlatformCapability {
        target_path: ResolvedPath,
        source: io::Error,
    },
    PreconditionNoLongerHolds {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    CreateAttemptFailed {
        target_path: ResolvedPath,
        source: io::Error,
        aftermath: TargetObservation,
    },
    CreateAftermathUnproven {
        target_path: ResolvedPath,
        source: io::Error,
        inspection: Box<TargetInspectionError>,
    },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
}

/// The reason a recorded replacement could not be completed and proven.
#[derive(Debug)]
pub(crate) enum ReplaceLinkExecutionError {
    MissingRecordedFacts,
    InvalidReplaceConditions,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction {
        expected: LinkTarget,
        actual: ResolvedPath,
    },
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    PlatformCapability {
        target_path: ResolvedPath,
        source: io::Error,
    },
    PreconditionNoLongerHolds {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    TemporaryNotMissing {
        temporary_path: ResolvedPath,
        observation: TargetObservation,
    },
    MutationAttempt {
        source: io::Error,
        aftermath: Box<ReplacementAftermath>,
    },
    MutationAftermathUnproven {
        mutation: ReplacementMutation,
        source: io::Error,
    },
    PostconditionInspection(TargetInspectionError),
    Aftermath {
        aftermath: Box<ReplacementAftermath>,
    },
}

/// The reason recovery could not safely clean up a recorded replacement temporary.
#[derive(Debug)]
pub(crate) enum ReplacementTemporaryCleanupError {
    MissingRecordedFacts,
    TargetInspection(TargetInspectionError),
    TemporaryInspection(TargetInspectionError),
    RemoveCapability(RemoveLinkExecutionError),
    RemoveAttemptFailed {
        temporary_path: ResolvedPath,
        source: io::Error,
        aftermath: TargetObservation,
    },
    PostconditionNotMet {
        temporary_path: ResolvedPath,
        observation: TargetObservation,
    },
}

#[derive(Debug)]
pub(crate) enum ReplacementMutation {
    TemporaryCreate,
    Rename,
}

#[derive(Debug)]
pub(crate) struct ReplacementAftermath {
    precondition: TargetObservation,
    postcondition: TargetObservation,
    temporary: TargetObservation,
}

impl ReplacementAftermath {
    pub(crate) fn postcondition_holds(&self) -> bool {
        matches!(self.postcondition, TargetObservation::ExpectedLink { .. })
            && matches!(self.temporary, TargetObservation::Missing)
    }

    pub(crate) fn precondition_holds(&self) -> bool {
        matches!(self.precondition, TargetObservation::ExpectedLink { .. })
            && matches!(self.temporary, TargetObservation::Missing)
    }
}

/// The reason a two-target relocation could not be completed and proven.
#[derive(Debug)]
pub(crate) enum RelocateLinkExecutionError {
    MissingRecordedFacts,
    InvalidRelocateConditions,
    SourceRecheck(SourceVerificationError),
    SourceDoesNotMatchAction {
        expected: LinkTarget,
        actual: ResolvedPath,
    },
    OldTargetInspection(TargetInspectionError),
    NewTargetInspection(TargetInspectionError),
    CreateCapability(CreateLinkExecutionError),
    RemoveCapability(RemoveLinkExecutionError),
    PreconditionNoLongerHolds {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    Aftermath {
        old_observation: TargetObservation,
        new_observation: TargetObservation,
    },
}

impl fmt::Display for RelocateLinkExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRecordedFacts => {
                formatter.write_str("relocation action has no recorded facts")
            }
            Self::InvalidRelocateConditions => formatter.write_str(
                "relocate action requires an expected old target and a missing distinct new target",
            ),
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction { expected, actual } => write!(
                formatter,
                "verified source {actual:?} does not match relocation target {expected:?}"
            ),
            Self::OldTargetInspection(error) | Self::NewTargetInspection(error) => {
                error.fmt(formatter)
            }
            Self::CreateCapability(error) => error.fmt(formatter),
            Self::RemoveCapability(error) => error.fmt(formatter),
            Self::PreconditionNoLongerHolds {
                target_path,
                observation,
            } => write!(
                formatter,
                "relocation precondition no longer holds at {target_path:?}: {observation:?}"
            ),
            Self::Aftermath {
                old_observation,
                new_observation,
            } => write!(
                formatter,
                "relocation aftermath is old={old_observation:?}, new={new_observation:?}"
            ),
        }
    }
}

impl std::error::Error for RelocateLinkExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRecheck(error) => Some(error),
            Self::OldTargetInspection(error) | Self::NewTargetInspection(error) => Some(error),
            Self::CreateCapability(error) => Some(error),
            Self::RemoveCapability(error) => Some(error),
            Self::MissingRecordedFacts
            | Self::InvalidRelocateConditions
            | Self::SourceDoesNotMatchAction { .. }
            | Self::PreconditionNoLongerHolds { .. }
            | Self::Aftermath { .. } => None,
        }
    }
}

impl fmt::Display for ReplaceLinkExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRecordedFacts => {
                f.write_str("replacement action has no recorded temporary sibling")
            }
            Self::InvalidReplaceConditions => {
                f.write_str("replace action requires changed sources at one target")
            }
            Self::SourceRecheck(error) => error.fmt(f),
            Self::SourceDoesNotMatchAction { expected, actual } => write!(
                f,
                "replacement source changed: expected {expected}, found {actual}"
            ),
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => error.fmt(f),
            Self::PlatformCapability {
                target_path,
                source,
            } => write!(
                f,
                "file symbolic-link replacement is unsupported at {target_path}: {source}"
            ),
            Self::PreconditionNoLongerHolds {
                target_path,
                observation,
            } => write!(
                f,
                "replacement precondition no longer holds at {target_path}: {observation:?}"
            ),
            Self::TemporaryNotMissing {
                temporary_path,
                observation,
            } => write!(
                f,
                "replacement temporary path is not missing at {temporary_path}: {observation:?}"
            ),
            Self::MutationAttempt { source, aftermath } => write!(
                f,
                "replacement mutation returned {source}; aftermath is precondition={:?}, postcondition={:?}, temporary={:?}",
                aftermath.precondition, aftermath.postcondition, aftermath.temporary,
            ),
            Self::MutationAftermathUnproven { mutation, source } => write!(
                f,
                "replacement {mutation:?} returned {source} and its aftermath could not be proven"
            ),
            Self::Aftermath { aftermath } => write!(
                f,
                "replacement aftermath is precondition={:?}, postcondition={:?}, temporary={:?}",
                aftermath.precondition, aftermath.postcondition, aftermath.temporary,
            ),
        }
    }
}

impl std::error::Error for ReplaceLinkExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRecheck(error) => Some(error),
            Self::TargetInspection(error)
            | Self::TemporaryInspection(error)
            | Self::PostconditionInspection(error) => Some(error),
            Self::PlatformCapability { source, .. }
            | Self::MutationAttempt { source, .. }
            | Self::MutationAftermathUnproven { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The reason a planned link-entry removal could not be safely completed and proven.
#[derive(Debug)]
pub(crate) enum RemoveLinkExecutionError {
    UnsupportedAction {
        kind: ActionKind,
    },
    InvalidRemoveConditions,
    TargetInspection(TargetInspectionError),
    PlatformCapability {
        target_path: ResolvedPath,
        source: io::Error,
    },
    PreconditionNoLongerHolds {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
    RemoveAttemptFailed {
        target_path: ResolvedPath,
        source: io::Error,
        aftermath: TargetObservation,
    },
    RemoveAftermathUnproven {
        target_path: ResolvedPath,
        source: io::Error,
        inspection: Box<TargetInspectionError>,
    },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
}

impl fmt::Display for RemoveLinkExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction { kind } => {
                write!(formatter, "remove executor cannot execute action kind {kind:?}")
            }
            Self::InvalidRemoveConditions => formatter.write_str(
                "a remove action requires one expected-link precondition and one missing post-condition for the same target",
            ),
            Self::TargetInspection(error) | Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::PlatformCapability {
                target_path,
                source,
            } => write!(
                formatter,
                "file symbolic-link removal is unsupported at {target_path}: {source}"
            ),
            Self::PreconditionNoLongerHolds {
                target_path,
                observation,
            } => write!(
                formatter,
                "remove precondition no longer holds at {target_path}: {observation:?}"
            ),
            Self::RemoveAttemptFailed {
                target_path,
                source,
                aftermath,
            } => write!(
                formatter,
                "cannot remove file link at {target_path}: {source}; no-follow aftermath: {aftermath:?}"
            ),
            Self::RemoveAftermathUnproven {
                target_path,
                source,
                inspection,
            } => write!(
                formatter,
                "cannot remove file link at {target_path}: {source}; aftermath cannot be proven: {inspection}"
            ),
            Self::PostconditionNotMet {
                target_path,
                observation,
            } => write!(
                formatter,
                "remove post-condition does not hold at {target_path}: {observation:?}"
            ),
        }
    }
}

impl std::error::Error for RemoveLinkExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TargetInspection(error) | Self::PostconditionInspection(error) => Some(error),
            Self::PlatformCapability { source, .. }
            | Self::RemoveAttemptFailed { source, .. }
            | Self::RemoveAftermathUnproven { source, .. } => Some(source),
            Self::UnsupportedAction { .. }
            | Self::InvalidRemoveConditions
            | Self::PreconditionNoLongerHolds { .. }
            | Self::PostconditionNotMet { .. } => None,
        }
    }
}

/// The reason a stale-Known-only action could not reprove a missing target.
#[derive(Debug)]
pub(crate) enum ForgetMissingExecutionError {
    UnsupportedAction {
        kind: ActionKind,
    },
    InvalidForgetMissingConditions,
    TargetInspection(TargetInspectionError),
    PostconditionNotMet {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
}

impl fmt::Display for ForgetMissingExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction { kind } => {
                write!(
                    formatter,
                    "forget-missing executor cannot execute action kind {kind:?}"
                )
            }
            Self::InvalidForgetMissingConditions => formatter.write_str(
                "a forget-missing action requires matching missing precondition and post-condition",
            ),
            Self::TargetInspection(error) => error.fmt(formatter),
            Self::PostconditionNotMet {
                target_path,
                observation,
            } => write!(
                formatter,
                "forget-missing post-condition does not hold at {target_path}: {observation:?}"
            ),
        }
    }
}

impl std::error::Error for ForgetMissingExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TargetInspection(error) => Some(error),
            Self::UnsupportedAction { .. }
            | Self::InvalidForgetMissingConditions
            | Self::PostconditionNotMet { .. } => None,
        }
    }
}

impl fmt::Display for CreateLinkExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAction { kind } => {
                write!(formatter, "create executor cannot execute action kind {kind:?}")
            }
            Self::InvalidCreateConditions => formatter.write_str(
                "a create action requires one missing precondition and one expected-link post-condition for the same target",
            ),
            Self::SourceRecheck(error) => error.fmt(formatter),
            Self::SourceDoesNotMatchAction { expected, actual } => write!(
                formatter,
                "reverified source {actual} does not match planned link target {expected}"
            ),
            Self::TargetInspection(error) => error.fmt(formatter),
            Self::PlatformCapability {
                target_path,
                source,
            } => write!(
                formatter,
                "file symbolic-link creation is unsupported at {target_path}: {source}"
            ),
            Self::PreconditionNoLongerHolds {
                target_path,
                observation,
            } => write!(
                formatter,
                "create precondition no longer holds at {target_path}: {observation:?}"
            ),
            Self::CreateAttemptFailed {
                target_path,
                source,
                aftermath,
            } => write!(
                formatter,
                "cannot create file link at {target_path}: {source}; no-follow aftermath: {aftermath:?}"
            ),
            Self::CreateAftermathUnproven {
                target_path,
                source,
                inspection,
            } => write!(
                formatter,
                "cannot create file link at {target_path}: {source}; aftermath cannot be proven: {inspection}"
            ),
            Self::PostconditionInspection(error) => error.fmt(formatter),
            Self::PostconditionNotMet {
                target_path,
                observation,
            } => write!(
                formatter,
                "create post-condition does not hold at {target_path}: {observation:?}"
            ),
        }
    }
}

impl std::error::Error for CreateLinkExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRecheck(error) => Some(error),
            Self::TargetInspection(error) | Self::PostconditionInspection(error) => Some(error),
            Self::PlatformCapability { source, .. } => Some(source),
            Self::CreateAttemptFailed { source, .. }
            | Self::CreateAftermathUnproven { source, .. } => Some(source),
            Self::UnsupportedAction { .. }
            | Self::InvalidCreateConditions
            | Self::SourceDoesNotMatchAction { .. }
            | Self::PreconditionNoLongerHolds { .. }
            | Self::PostconditionNotMet { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::KnownFileLink;
    use crate::domain::paths::{ResolvedPath, SourceRelativePath};
    use crate::inspection::source::{resolve_store_root, verify_regular_source};

    static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(0);

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let unique_id = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "loadout-create-executor-test-{}-{timestamp}-{unique_id}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir(root.join("home")).unwrap();
            fs::create_dir(root.join("store")).unwrap();
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn verified_source(&self) -> VerifiedSource {
            let root = resolve_store_root(&self.path("store")).unwrap();
            verify_regular_source(&root, &SourceRelativePath::parse("git/config").unwrap()).unwrap()
        }

        fn create_action(&self) -> PlannedAction {
            PlannedAction::create_link(
                ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse("base/git-config").unwrap(),
                    self.verified_source().path().clone(),
                    ResolvedPath::new(self.path("home/.gitconfig")).unwrap(),
                )
                .unwrap(),
            )
        }

        fn remove_action(&self) -> PlannedAction {
            PlannedAction::remove_link(KnownFileLink::from_resolved(
                &ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse("base/git-config").unwrap(),
                    ResolvedPath::new(self.path("store/git/config")).unwrap(),
                    ResolvedPath::new(self.path("home/.gitconfig")).unwrap(),
                )
                .unwrap(),
            ))
        }

        fn executor(&self) -> FileLinkExecutor {
            FileLinkExecutor::new(&self.path("home")).unwrap()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_executor_materializes_and_proves_the_absolute_expected_link() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let source = workspace.verified_source();
        let action = workspace.create_action();

        workspace
            .executor()
            .execute_create(&action, &source)
            .unwrap();

        let target = workspace.path("home/.gitconfig");
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&target).unwrap(),
            workspace.path("store/git/config")
        );
    }

    #[test]
    fn target_that_appears_after_planning_is_not_replaced_or_reinterpreted() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let source = workspace.verified_source();
        let action = workspace.create_action();
        workspace.write("home/.gitconfig", "user-owned contents\n");

        let error = workspace
            .executor()
            .execute_create(&action, &source)
            .unwrap_err();

        assert!(matches!(
            error,
            CreateLinkExecutionError::PreconditionNoLongerHolds {
                observation: TargetObservation::OtherEntry { .. },
                ..
            }
        ));
        let target = workspace.path("home/.gitconfig");
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "user-owned contents\n"
        );
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn source_that_fails_its_immediate_recheck_leaves_the_target_missing() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let source = workspace.verified_source();
        let action = workspace.create_action();
        fs::remove_file(workspace.path("store/git/config")).unwrap();
        fs::create_dir(workspace.path("store/git/config")).unwrap();

        let error = workspace
            .executor()
            .execute_create(&action, &source)
            .unwrap_err();

        assert!(matches!(
            error,
            CreateLinkExecutionError::SourceRecheck(
                SourceVerificationError::SourceNotRegular { .. }
            )
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_home_replaced_by_a_symlink_is_rejected_without_writing_through_it() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let source = workspace.verified_source();
        let action = workspace.create_action();
        let executor = workspace.executor();
        fs::rename(workspace.path("home"), workspace.path("former-home")).unwrap();
        fs::create_dir(workspace.path("outside")).unwrap();
        symlink(workspace.path("outside"), workspace.path("home")).unwrap();

        let error = executor.execute_create(&action, &source).unwrap_err();

        assert!(matches!(
            error,
            CreateLinkExecutionError::PreconditionNoLongerHolds {
                observation: TargetObservation::UnsafePath {
                    parent_safety: crate::domain::actual::ParentSafety::Symlink,
                },
                ..
            }
        ));
        assert!(!workspace.path("outside/.gitconfig").exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_executor_removes_only_the_link_entry_and_preserves_its_referent() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let action = workspace.remove_action();
        let source = workspace.path("store/git/config");
        let target = workspace.path("home/.gitconfig");
        symlink(&source, &target).unwrap();

        workspace.executor().execute_remove(&action).unwrap();

        assert!(!target.exists());
        assert_eq!(
            fs::read_to_string(&source).unwrap(),
            "[user]\nname = Example\n"
        );
        assert!(
            fs::symlink_metadata(workspace.path("home"))
                .unwrap()
                .is_dir()
        );
    }

    #[cfg(unix)]
    #[test]
    fn remove_executor_rejects_wrong_or_non_link_targets_without_mutation() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace.write("store/git/other", "other source\n");
        let action = workspace.remove_action();
        let target = workspace.path("home/.gitconfig");
        let other = workspace.path("store/git/other");
        symlink(&other, &target).unwrap();

        let error = workspace.executor().execute_remove(&action).unwrap_err();
        assert!(matches!(
            error,
            RemoveLinkExecutionError::PreconditionNoLongerHolds {
                observation: TargetObservation::OtherLink { .. },
                ..
            }
        ));
        assert_eq!(fs::read_link(&target).unwrap(), other);
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "owned source\n"
        );

        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        let error = workspace.executor().execute_remove(&action).unwrap_err();
        assert!(matches!(
            error,
            RemoveLinkExecutionError::PreconditionNoLongerHolds {
                observation: TargetObservation::OtherEntry { .. },
                ..
            }
        ));
        assert!(fs::symlink_metadata(&target).unwrap().is_dir());
        assert!(
            fs::symlink_metadata(workspace.path("home"))
                .unwrap()
                .is_dir()
        );
    }

    #[cfg(unix)]
    #[test]
    fn remove_executor_rejects_a_symlinked_parent_without_touching_the_outside_tree() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        let action = workspace.remove_action();
        let executor = workspace.executor();
        fs::rename(workspace.path("home"), workspace.path("former-home")).unwrap();
        fs::create_dir(workspace.path("outside")).unwrap();
        workspace.write("outside/.gitconfig", "outside contents\n");
        symlink(workspace.path("outside"), workspace.path("home")).unwrap();

        let error = executor.execute_remove(&action).unwrap_err();

        assert!(matches!(
            error,
            RemoveLinkExecutionError::PreconditionNoLongerHolds {
                observation: TargetObservation::UnsafePath { .. },
                ..
            }
        ));
        assert_eq!(
            fs::read_to_string(workspace.path("outside/.gitconfig")).unwrap(),
            "outside contents\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "owned source\n"
        );
    }
}
