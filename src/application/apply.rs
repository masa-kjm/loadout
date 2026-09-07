//! Non-dry-run coordination for the implemented single file-link actions.

use std::fmt;

use crate::domain::actual::TargetObservation;
use crate::domain::hashes::{CanonicalHashError, desired_hash};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, Plan, PlannedAction};
use crate::executor::file_link::{
    CreateLinkExecutionError, FileLinkExecutor, ForgetMissingExecutionError,
    RelocateLinkExecutionError, RemoveLinkExecutionError, ReplaceLinkExecutionError,
};
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::planner::file_link::plan;
use crate::resolver::ResolvedApplyInput;
use crate::state::operation::ActionStatus;
use crate::state::operation::RecordedAction;
use crate::state::repository::{LockedStateRepository, StateRepository, StateRepositoryError};

/// Coordinates a confirmed non-dry-run apply for one home and state directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApplyCoordinator {
    home_directory: ResolvedPath,
    state_repository: StateRepository,
    #[cfg(test)]
    force_capability_failure: bool,
}

impl ApplyCoordinator {
    /// Binds resolved machine paths without inspecting targets or writing state.
    pub(crate) fn new(home_directory: ResolvedPath, state_directory: ResolvedPath) -> Self {
        Self {
            home_directory,
            state_repository: StateRepository::new(state_directory),
            #[cfg(test)]
            force_capability_failure: false,
        }
    }

    /// Executes one Slice 4 create plan from fresh state.
    ///
    /// The caller supplies the resolver's canonical Desired and source proofs.
    /// This coordinator neither parses declarations nor chooses a resource action: it asks the pure planner for a fresh plan and invokes only the selected `create_link` action. It invokes `confirm` only after successful preflight and before it writes an operation record.
    pub(crate) fn apply_create_link<F>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
    ) -> Result<CreateLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
    {
        self.apply_with_hooks(resolved, confirm, |_| {})
    }

    /// Executes one Slice 5 stale-resource plan from fresh state.
    ///
    /// The planner decides whether the stale record requires a link-entry removal or a state-only forget. This coordinator only preflights and executes that selected single action.
    pub(crate) fn apply_stale_link<F>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
    ) -> Result<StaleLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
    {
        self.apply_stale_with_hooks(resolved, confirm, |_| {})
    }

    /// Executes one same-target, source-changing managed replacement.
    pub(crate) fn apply_replace_link<F>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
    ) -> Result<ReplaceLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
    {
        self.apply_replace_link_with_hooks(resolved, confirm, |_| {})
    }

    fn apply_replace_link_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<ReplaceLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut LockedStateRepository),
    {
        let result = self.apply_single_action(resolved, confirm, after_running, |plan| {
            let action = require_single_replace_action(plan)?.clone();
            let source = resolved
                .verified_sources()
                .get(action.resource_id())
                .ok_or_else(|| ApplyError::MissingVerifiedSource {
                    resource_id: action.resource_id().clone(),
                })?
                .clone();
            let executor = FileLinkExecutor::new(self.home_directory.as_ref())
                .map_err(ApplyError::InitialInspection)?;
            executor
                .preflight_replace(&action, &source)
                .map_err(ApplyError::ReplacePreflight)?;
            Ok((
                action,
                move |action: &PlannedAction, recorded: RecordedAction| {
                    classify_execution(
                        executor.execute_replace(action, &recorded, &source),
                        classify_replace_execution_error,
                    )
                },
            ))
        })?;
        Ok(match result {
            SingleActionApplyResult::Applied { resource_id, .. } => {
                ReplaceLinkApplyResult::Applied { resource_id }
            }
            SingleActionApplyResult::Blocked { plan } => ReplaceLinkApplyResult::Blocked { plan },
            SingleActionApplyResult::Declined { plan } => ReplaceLinkApplyResult::Declined { plan },
            SingleActionApplyResult::Failed { error } => ReplaceLinkApplyResult::Failed { error },
            SingleActionApplyResult::Uncertain { error } => {
                ReplaceLinkApplyResult::Uncertain { error }
            }
        })
    }

    /// Executes an internal managed identity handoff at one target.
    pub(crate) fn apply_replace_ownership<F>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
    ) -> Result<ReplaceOwnershipApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
    {
        self.apply_replace_ownership_with_hooks(resolved, confirm, |_| {})
    }

    /// Executes one managed file-link relocation selected by the planner.
    pub(crate) fn apply_relocate_link<F>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
    ) -> Result<RelocateLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
    {
        let result = self.apply_single_action(
            resolved,
            confirm,
            |_| {},
            |plan| {
                let action = require_single_relocate_action(plan)?.clone();
                let source = resolved
                    .verified_sources()
                    .get(action.resource_id())
                    .ok_or_else(|| ApplyError::MissingVerifiedSource {
                        resource_id: action.resource_id().clone(),
                    })?
                    .clone();
                let executor = FileLinkExecutor::new(self.home_directory.as_ref())
                    .map_err(ApplyError::InitialInspection)?;
                executor
                    .preflight_relocate(&action, &source)
                    .map_err(ApplyError::RelocatePreflight)?;
                Ok((
                    action,
                    move |action: &PlannedAction, recorded: RecordedAction| {
                        classify_execution(
                            executor.execute_relocate(action, &recorded, &source),
                            classify_relocate_execution_error,
                        )
                    },
                ))
            },
        )?;
        Ok(match result {
            SingleActionApplyResult::Applied { resource_id, .. } => {
                RelocateLinkApplyResult::Applied { resource_id }
            }
            SingleActionApplyResult::Blocked { plan } => RelocateLinkApplyResult::Blocked { plan },
            SingleActionApplyResult::Declined { plan } => {
                RelocateLinkApplyResult::Declined { plan }
            }
            SingleActionApplyResult::Failed { error } => RelocateLinkApplyResult::Failed { error },
            SingleActionApplyResult::Uncertain { error } => {
                RelocateLinkApplyResult::Uncertain { error }
            }
        })
    }

    fn apply_replace_ownership_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<ReplaceOwnershipApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut LockedStateRepository),
    {
        let result = self.apply_single_action(resolved, confirm, after_running, |plan| {
            let action = require_single_replace_ownership_action(plan)?.clone();
            let source = resolved
                .verified_sources()
                .get(action.resource_id())
                .ok_or_else(|| ApplyError::MissingVerifiedSource {
                    resource_id: action.resource_id().clone(),
                })?
                .clone();
            let executor = FileLinkExecutor::new(self.home_directory.as_ref())
                .map_err(ApplyError::InitialInspection)?;
            let changed_source = action.preconditions()[0] != action.postconditions()[0];
            if changed_source {
                executor
                    .preflight_replace(&action, &source)
                    .map_err(ApplyError::ReplacePreflight)?;
            } else {
                executor
                    .preflight_same_source_ownership_handoff(&action, &source)
                    .map_err(ApplyError::ReplacePreflight)?;
            }
            Ok((
                action,
                move |action: &PlannedAction, recorded: RecordedAction| {
                    if recorded.replacement_facts().is_some() {
                        classify_execution(
                            executor.execute_replace(action, &recorded, &source),
                            classify_replace_execution_error,
                        )
                    } else {
                        classify_execution(
                            executor
                                .execute_same_source_ownership_handoff(action, &recorded, &source),
                            classify_replace_execution_error,
                        )
                    }
                },
            ))
        })?;
        Ok(match result {
            SingleActionApplyResult::Applied { resource_id, .. } => {
                ReplaceOwnershipApplyResult::Applied { resource_id }
            }
            SingleActionApplyResult::Blocked { plan } => {
                ReplaceOwnershipApplyResult::Blocked { plan }
            }
            SingleActionApplyResult::Declined { plan } => {
                ReplaceOwnershipApplyResult::Declined { plan }
            }
            SingleActionApplyResult::Failed { error } => {
                ReplaceOwnershipApplyResult::Failed { error }
            }
            SingleActionApplyResult::Uncertain { error } => {
                ReplaceOwnershipApplyResult::Uncertain { error }
            }
        })
    }

    fn apply_stale_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<StaleLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut LockedStateRepository),
    {
        let result = self.apply_single_action(resolved, confirm, after_running, |plan| {
            let action = require_single_stale_action(plan)?.clone();
            let executor = FileLinkExecutor::new(self.home_directory.as_ref())
                .map_err(ApplyError::InitialInspection)?;
            preflight_stale_action(&executor, &action).map_err(ApplyError::StalePreflight)?;
            Ok((action, move |action: &PlannedAction, _: RecordedAction| {
                let result = match action.kind() {
                    ActionKind::RemoveLink => executor
                        .execute_remove(action)
                        .map_err(StaleLinkExecutionError::Remove),
                    ActionKind::ForgetMissing => executor
                        .execute_forget_missing(action)
                        .map_err(StaleLinkExecutionError::ForgetMissing),
                    kind => Err(StaleLinkExecutionError::UnsupportedAction { kind }),
                };
                classify_execution(result, classify_stale_execution_error)
            }))
        })?;
        Ok(match result {
            SingleActionApplyResult::Applied { resource_id, kind } => {
                StaleLinkApplyResult::Applied { resource_id, kind }
            }
            SingleActionApplyResult::Blocked { plan } => StaleLinkApplyResult::Blocked { plan },
            SingleActionApplyResult::Declined { plan } => StaleLinkApplyResult::Declined { plan },
            SingleActionApplyResult::Failed { error } => StaleLinkApplyResult::Failed { error },
            SingleActionApplyResult::Uncertain { error } => {
                StaleLinkApplyResult::Uncertain { error }
            }
        })
    }

    fn apply_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<CreateLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut LockedStateRepository),
    {
        let result = self.apply_single_action(resolved, confirm, after_running, |plan| {
            let action = require_single_create_action(plan)?.clone();
            let source = resolved
                .verified_sources()
                .get(action.resource_id())
                .ok_or_else(|| ApplyError::MissingVerifiedSource {
                    resource_id: action.resource_id().clone(),
                })?
                .clone();
            let executor = FileLinkExecutor::new(self.home_directory.as_ref())
                .map_err(ApplyError::InitialInspection)?;
            #[cfg(test)]
            let executor = if self.force_capability_failure {
                executor.with_forced_capability_failure_for_test()
            } else {
                executor
            };
            executor
                .preflight_create(&action, &source)
                .map_err(ApplyError::Preflight)?;
            Ok((action, move |action: &PlannedAction, _: RecordedAction| {
                classify_execution(
                    executor.execute_create(action, &source),
                    classify_execution_error,
                )
            }))
        })?;
        Ok(match result {
            SingleActionApplyResult::Applied { resource_id, .. } => {
                CreateLinkApplyResult::Applied { resource_id }
            }
            SingleActionApplyResult::Blocked { plan } => CreateLinkApplyResult::Blocked { plan },
            SingleActionApplyResult::Declined { plan } => CreateLinkApplyResult::Declined { plan },
            SingleActionApplyResult::Failed { error } => CreateLinkApplyResult::Failed { error },
            SingleActionApplyResult::Uncertain { error } => {
                CreateLinkApplyResult::Uncertain { error }
            }
        })
    }

    /// Owns the shared lifecycle while retaining each entry point's action restriction.
    /// `prepare` binds and preflights a planner-selected action; the returned execution runs only after `running` is durable and must repeat safety checks.
    /// Neither callback receives the repository or advances durable state.
    fn apply_single_action<E, X>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: impl FnOnce(&Plan) -> bool,
        after_running: impl FnOnce(&mut LockedStateRepository),
        prepare: impl FnOnce(&Plan) -> Result<(PlannedAction, X), ApplyError>,
    ) -> Result<SingleActionApplyResult<E>, ApplyError>
    where
        X: for<'a> FnOnce(&'a PlannedAction, RecordedAction) -> ExecutionOutcome<E>,
    {
        // Locking precedes every state-for-execution read and target inspection.
        let mut locked = self
            .state_repository
            .acquire_exclusive()
            .map_err(ApplyError::State)?;
        if locked.state().active_operation().is_some() {
            // Until recovery is implemented, leave unfinished records untouched.
            return Err(ApplyError::RecoveryRequired);
        }
        let desired = resolved.desired();
        let inspector = FileLinkInspector::new(self.home_directory.as_ref())
            .map_err(ApplyError::InitialInspection)?;
        let actual = inspector
            .inspect(desired, locked.state().known())
            .map_err(ApplyError::InitialInspection)?;
        let plan = plan(desired, locked.state().known(), &actual);
        if !plan.is_executable() {
            return Ok(SingleActionApplyResult::Blocked { plan });
        }

        let (action, execute) = prepare(&plan)?;
        locked
            .preflight_writable()
            .map_err(ApplyError::StatePreflight)?;
        let desired_hash = desired_hash(desired).map_err(ApplyError::DesiredHash)?;
        if !confirm(&plan) {
            return Ok(SingleActionApplyResult::Declined { plan });
        }

        let action_id = locked
            .begin_operation(desired_hash, &action)
            .map_err(ApplyError::State)?;
        locked.mark_running(&action_id).map_err(ApplyError::State)?;
        after_running(&mut locked);

        let recorded = locked
            .state()
            .active_operation()
            .and_then(|operation| operation.action(&action_id))
            .expect("begin_operation records the action before marking it running");
        let result = match execute(&action, recorded.clone()) {
            ExecutionOutcome::Succeeded => {
                locked
                    .commit_succeeded(&action_id)
                    .map_err(ApplyError::State)?;
                SingleActionApplyResult::Applied {
                    resource_id: action.resource_id().clone(),
                    kind: action.kind(),
                }
            }
            ExecutionOutcome::Failed(error) => {
                locked
                    .mark_without_known(&action_id, ActionStatus::Failed)
                    .map_err(ApplyError::State)?;
                SingleActionApplyResult::Failed { error }
            }
            ExecutionOutcome::Uncertain(error) => {
                locked
                    .mark_without_known(&action_id, ActionStatus::Uncertain)
                    .map_err(ApplyError::State)?;
                return Ok(SingleActionApplyResult::Uncertain { error });
            }
        };
        locked
            .close_finished_operation()
            .map_err(ApplyError::State)?;
        Ok(result)
    }

    #[cfg(test)]
    fn apply_create_link_with_after_running<H>(
        &self,
        resolved: &ResolvedApplyInput,
        after_running: H,
    ) -> Result<CreateLinkApplyResult, ApplyError>
    where
        H: FnOnce(&mut LockedStateRepository),
    {
        self.apply_with_hooks(resolved, |_| true, after_running)
    }

    #[cfg(test)]
    fn apply_stale_link_with_after_running<H>(
        &self,
        resolved: &ResolvedApplyInput,
        after_running: H,
    ) -> Result<StaleLinkApplyResult, ApplyError>
    where
        H: FnOnce(&mut LockedStateRepository),
    {
        self.apply_stale_with_hooks(resolved, |_| true, after_running)
    }

    #[cfg(test)]
    fn apply_replace_ownership_with_after_running<H>(
        &self,
        resolved: &ResolvedApplyInput,
        after_running: H,
    ) -> Result<ReplaceOwnershipApplyResult, ApplyError>
    where
        H: FnOnce(&mut LockedStateRepository),
    {
        self.apply_replace_ownership_with_hooks(resolved, |_| true, after_running)
    }

    #[cfg(test)]
    fn fail_next_state_write_preflight(&mut self) {
        self.state_repository.fail_next_state_write_preflight();
    }

    #[cfg(test)]
    fn force_capability_failure_for_test(&mut self) {
        self.force_capability_failure = true;
    }
}

fn require_single_create_action(plan: &Plan) -> Result<&PlannedAction, ApplyError> {
    let [action] = plan.actions() else {
        return Err(ApplyError::SliceFourRequiresSingleCreateAction {
            action_kinds: plan.actions().iter().map(PlannedAction::kind).collect(),
        });
    };
    if action.kind() != ActionKind::CreateLink {
        return Err(ApplyError::SliceFourRequiresSingleCreateAction {
            action_kinds: vec![action.kind()],
        });
    }
    Ok(action)
}

fn require_single_stale_action(plan: &Plan) -> Result<&PlannedAction, ApplyError> {
    let [action] = plan.actions() else {
        return Err(ApplyError::SliceFiveRequiresSingleStaleAction {
            action_kinds: plan.actions().iter().map(PlannedAction::kind).collect(),
        });
    };
    if !matches!(
        action.kind(),
        ActionKind::RemoveLink | ActionKind::ForgetMissing
    ) {
        return Err(ApplyError::SliceFiveRequiresSingleStaleAction {
            action_kinds: vec![action.kind()],
        });
    }
    Ok(action)
}

fn require_single_replace_action(plan: &Plan) -> Result<&PlannedAction, ApplyError> {
    let [action] = plan.actions() else {
        return Err(ApplyError::SliceSixRequiresSingleReplaceAction {
            action_kinds: plan.actions().iter().map(PlannedAction::kind).collect(),
        });
    };
    if action.kind() != ActionKind::ReplaceLink {
        return Err(ApplyError::SliceSixRequiresSingleReplaceAction {
            action_kinds: vec![action.kind()],
        });
    }
    Ok(action)
}

fn require_single_replace_ownership_action(plan: &Plan) -> Result<&PlannedAction, ApplyError> {
    let [action] = plan.actions() else {
        return Err(ApplyError::SliceSixRequiresSingleReplaceOwnershipAction {
            action_kinds: plan.actions().iter().map(PlannedAction::kind).collect(),
        });
    };
    if action.kind() != ActionKind::ReplaceOwnership {
        return Err(ApplyError::SliceSixRequiresSingleReplaceOwnershipAction {
            action_kinds: vec![action.kind()],
        });
    }
    Ok(action)
}

fn require_single_relocate_action(plan: &Plan) -> Result<&PlannedAction, ApplyError> {
    let [action] = plan.actions() else {
        return Err(ApplyError::SliceSixRequiresSingleRelocateAction {
            action_kinds: plan.actions().iter().map(PlannedAction::kind).collect(),
        });
    };
    if action.kind() != ActionKind::RelocateLink {
        return Err(ApplyError::SliceSixRequiresSingleRelocateAction {
            action_kinds: vec![action.kind()],
        });
    }
    Ok(action)
}

fn preflight_stale_action(
    executor: &FileLinkExecutor,
    action: &PlannedAction,
) -> Result<(), StaleLinkExecutionError> {
    match action.kind() {
        ActionKind::RemoveLink => executor
            .preflight_remove(action)
            .map_err(StaleLinkExecutionError::Remove),
        ActionKind::ForgetMissing => executor
            .preflight_forget_missing(action)
            .map_err(StaleLinkExecutionError::ForgetMissing),
        kind => Err(StaleLinkExecutionError::UnsupportedAction { kind }),
    }
}

/// Shared progress outcomes keep each entry point's execution error type intact.
enum SingleActionApplyResult<E> {
    Applied {
        resource_id: FullyQualifiedResourceId,
        kind: ActionKind,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: E,
    },
    Uncertain {
        error: E,
    },
}

enum ExecutionOutcome<E> {
    Succeeded,
    Failed(E),
    Uncertain(E),
}

fn classify_execution<E>(
    result: Result<(), E>,
    classify_error: impl FnOnce(&E) -> ExecutionClassification,
) -> ExecutionOutcome<E> {
    match result {
        Ok(()) => ExecutionOutcome::Succeeded,
        Err(error) => match classify_error(&error) {
            // A verified post-condition may prove success despite an OS error.
            ExecutionClassification::Succeeded => ExecutionOutcome::Succeeded,
            ExecutionClassification::Failed => ExecutionOutcome::Failed(error),
            ExecutionClassification::Uncertain => ExecutionOutcome::Uncertain(error),
        },
    }
}

fn classify_stale_execution_error(error: &StaleLinkExecutionError) -> ExecutionClassification {
    match error {
        StaleLinkExecutionError::Remove(error) => classify_remove_execution_error(error),
        StaleLinkExecutionError::ForgetMissing(error) => {
            classify_forget_missing_execution_error(error)
        }
        StaleLinkExecutionError::UnsupportedAction { .. } => ExecutionClassification::Uncertain,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionClassification {
    Succeeded,
    Failed,
    Uncertain,
}

fn classify_execution_error(error: &CreateLinkExecutionError) -> ExecutionClassification {
    match error {
        CreateLinkExecutionError::CreateAttemptFailed { aftermath, .. }
        | CreateLinkExecutionError::PostconditionNotMet {
            observation: aftermath,
            ..
        } => classify_observation(aftermath),
        // A failed immediate recheck did not attempt the action. It must not turn an externally created matching link into managed state.
        CreateLinkExecutionError::PreconditionNoLongerHolds { .. }
        | CreateLinkExecutionError::UnsupportedAction { .. }
        | CreateLinkExecutionError::InvalidCreateConditions
        | CreateLinkExecutionError::SourceRecheck(_)
        | CreateLinkExecutionError::SourceDoesNotMatchAction { .. }
        | CreateLinkExecutionError::TargetInspection(_)
        | CreateLinkExecutionError::PlatformCapability { .. }
        | CreateLinkExecutionError::CreateAftermathUnproven { .. }
        | CreateLinkExecutionError::PostconditionInspection(_) => {
            ExecutionClassification::Uncertain
        }
    }
}

fn classify_replace_execution_error(error: &ReplaceLinkExecutionError) -> ExecutionClassification {
    match error {
        ReplaceLinkExecutionError::MutationAttempt { aftermath, .. }
        | ReplaceLinkExecutionError::Aftermath { aftermath }
            if aftermath.postcondition_holds() =>
        {
            ExecutionClassification::Succeeded
        }
        ReplaceLinkExecutionError::MutationAttempt { aftermath, .. }
        | ReplaceLinkExecutionError::Aftermath { aftermath }
            if aftermath.precondition_holds() =>
        {
            ExecutionClassification::Failed
        }
        ReplaceLinkExecutionError::PreconditionNoLongerHolds { .. } => {
            ExecutionClassification::Uncertain
        }
        _ => ExecutionClassification::Uncertain,
    }
}

fn classify_relocate_execution_error(
    error: &RelocateLinkExecutionError,
) -> ExecutionClassification {
    match error {
        RelocateLinkExecutionError::PreconditionNoLongerHolds { .. } => {
            ExecutionClassification::Uncertain
        }
        RelocateLinkExecutionError::Aftermath {
            old_observation: TargetObservation::Missing,
            new_observation: TargetObservation::ExpectedLink { .. },
        } => ExecutionClassification::Succeeded,
        RelocateLinkExecutionError::Aftermath {
            old_observation: TargetObservation::ExpectedLink { .. },
            new_observation: TargetObservation::Missing,
        } => ExecutionClassification::Failed,
        _ => ExecutionClassification::Uncertain,
    }
}

fn classify_observation(observation: &TargetObservation) -> ExecutionClassification {
    match observation {
        TargetObservation::ExpectedLink { .. } => ExecutionClassification::Succeeded,
        TargetObservation::Missing => ExecutionClassification::Failed,
        TargetObservation::MatchingUnmanagedLink { .. }
        | TargetObservation::OtherLink { .. }
        | TargetObservation::OtherEntry { .. }
        | TargetObservation::UnsafePath { .. } => ExecutionClassification::Uncertain,
    }
}

fn classify_remove_execution_error(error: &RemoveLinkExecutionError) -> ExecutionClassification {
    match error {
        RemoveLinkExecutionError::RemoveAttemptFailed { aftermath, .. }
        | RemoveLinkExecutionError::PostconditionNotMet {
            observation: aftermath,
            ..
        } => match aftermath {
            TargetObservation::Missing => ExecutionClassification::Succeeded,
            TargetObservation::ExpectedLink { .. } => ExecutionClassification::Failed,
            TargetObservation::MatchingUnmanagedLink { .. }
            | TargetObservation::OtherLink { .. }
            | TargetObservation::OtherEntry { .. }
            | TargetObservation::UnsafePath { .. } => ExecutionClassification::Uncertain,
        },
        // The executor did not attempt a removal after these errors. Keep the historical fact unchanged and let a later fresh plan classify any externally changed target instead of adopting that new state here.
        RemoveLinkExecutionError::PreconditionNoLongerHolds { .. } => {
            ExecutionClassification::Failed
        }
        RemoveLinkExecutionError::UnsupportedAction { .. }
        | RemoveLinkExecutionError::InvalidRemoveConditions
        | RemoveLinkExecutionError::TargetInspection(_)
        | RemoveLinkExecutionError::PlatformCapability { .. }
        | RemoveLinkExecutionError::RemoveAftermathUnproven { .. }
        | RemoveLinkExecutionError::PostconditionInspection(_) => {
            ExecutionClassification::Uncertain
        }
    }
}

fn classify_forget_missing_execution_error(
    error: &ForgetMissingExecutionError,
) -> ExecutionClassification {
    match error {
        // A target appeared after planning. This action never mutated it and must retain Known state; a future fresh plan will report its current conflict rather than deleting it.
        ForgetMissingExecutionError::PostconditionNotMet { .. } => ExecutionClassification::Failed,
        ForgetMissingExecutionError::UnsupportedAction { .. }
        | ForgetMissingExecutionError::InvalidForgetMissingConditions
        | ForgetMissingExecutionError::TargetInspection(_) => ExecutionClassification::Uncertain,
    }
}

/// The complete visible outcome of this internal create-only coordinator.
#[derive(Debug)]
pub(crate) enum CreateLinkApplyResult {
    Applied {
        resource_id: FullyQualifiedResourceId,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: CreateLinkExecutionError,
    },
    Uncertain {
        error: CreateLinkExecutionError,
    },
}

/// The complete outcome of Slice 6's single managed replacement coordinator.
#[derive(Debug)]
pub(crate) enum ReplaceLinkApplyResult {
    Applied {
        resource_id: FullyQualifiedResourceId,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: ReplaceLinkExecutionError,
    },
    Uncertain {
        error: ReplaceLinkExecutionError,
    },
}

#[derive(Debug)]
pub(crate) enum ReplaceOwnershipApplyResult {
    Applied {
        resource_id: FullyQualifiedResourceId,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: ReplaceLinkExecutionError,
    },
    Uncertain {
        error: ReplaceLinkExecutionError,
    },
}

#[derive(Debug)]
pub(crate) enum RelocateLinkApplyResult {
    Applied {
        resource_id: FullyQualifiedResourceId,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: RelocateLinkExecutionError,
    },
    Uncertain {
        error: RelocateLinkExecutionError,
    },
}

/// The complete visible outcome of Slice 5's single stale-resource coordinator.
#[derive(Debug)]
pub(crate) enum StaleLinkApplyResult {
    Applied {
        resource_id: FullyQualifiedResourceId,
        kind: ActionKind,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
    Failed {
        error: StaleLinkExecutionError,
    },
    Uncertain {
        error: StaleLinkExecutionError,
    },
}

/// The execution error retained by one stale-resource action outcome.
#[derive(Debug)]
pub(crate) enum StaleLinkExecutionError {
    Remove(RemoveLinkExecutionError),
    ForgetMissing(ForgetMissingExecutionError),
    UnsupportedAction { kind: ActionKind },
}

impl fmt::Display for StaleLinkExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Remove(error) => error.fmt(formatter),
            Self::ForgetMissing(error) => error.fmt(formatter),
            Self::UnsupportedAction { kind } => {
                write!(
                    formatter,
                    "stale-resource executor cannot execute action kind {kind:?}"
                )
            }
        }
    }
}

impl std::error::Error for StaleLinkExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Remove(error) => Some(error),
            Self::ForgetMissing(error) => Some(error),
            Self::UnsupportedAction { .. } => None,
        }
    }
}

/// The reason an apply attempt could not reach an executable create action.
#[derive(Debug)]
pub(crate) enum ApplyError {
    State(StateRepositoryError),
    StatePreflight(StateRepositoryError),
    RecoveryRequired,
    InitialInspection(TargetInspectionError),
    MissingVerifiedSource {
        resource_id: FullyQualifiedResourceId,
    },
    SliceFourRequiresSingleCreateAction {
        action_kinds: Vec<ActionKind>,
    },
    Preflight(CreateLinkExecutionError),
    SliceSixRequiresSingleReplaceAction {
        action_kinds: Vec<ActionKind>,
    },
    ReplacePreflight(ReplaceLinkExecutionError),
    SliceSixRequiresSingleReplaceOwnershipAction {
        action_kinds: Vec<ActionKind>,
    },
    SliceSixRequiresSingleRelocateAction {
        action_kinds: Vec<ActionKind>,
    },
    RelocatePreflight(RelocateLinkExecutionError),
    SliceFiveRequiresSingleStaleAction {
        action_kinds: Vec<ActionKind>,
    },
    StalePreflight(StaleLinkExecutionError),
    DesiredHash(CanonicalHashError),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => error.fmt(formatter),
            Self::StatePreflight(error) => error.fmt(formatter),
            Self::RecoveryRequired => formatter.write_str(
                "an active operation must be recovered before a fresh plan can be created",
            ),
            Self::InitialInspection(error) => error.fmt(formatter),
            Self::MissingVerifiedSource { resource_id } => {
                write!(
                    formatter,
                    "no verified source is available for {resource_id}"
                )
            }
            Self::SliceFourRequiresSingleCreateAction { action_kinds } => write!(
                formatter,
                "Slice 4 apply supports exactly one create_link action, not {action_kinds:?}"
            ),
            Self::Preflight(error) => error.fmt(formatter),
            Self::SliceSixRequiresSingleReplaceAction { action_kinds } => write!(
                formatter,
                "Slice 6 apply supports exactly one replace_link action, not {action_kinds:?}"
            ),
            Self::ReplacePreflight(error) => error.fmt(formatter),
            Self::SliceSixRequiresSingleReplaceOwnershipAction { action_kinds } => write!(
                formatter,
                "Slice 6 apply supports exactly one replace_ownership action, not {action_kinds:?}"
            ),
            Self::SliceSixRequiresSingleRelocateAction { action_kinds } => write!(
                formatter,
                "Slice 6 apply supports exactly one relocate_link action, not {action_kinds:?}"
            ),
            Self::RelocatePreflight(error) => error.fmt(formatter),
            Self::SliceFiveRequiresSingleStaleAction { action_kinds } => write!(
                formatter,
                "Slice 5 apply supports exactly one remove_link or forget_missing action, not {action_kinds:?}"
            ),
            Self::StalePreflight(error) => error.fmt(formatter),
            Self::DesiredHash(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::StatePreflight(error) => Some(error),
            Self::InitialInspection(error) => Some(error),
            Self::Preflight(error) => Some(error),
            Self::ReplacePreflight(error) => Some(error),
            Self::RelocatePreflight(error) => Some(error),
            Self::StalePreflight(error) => Some(error),
            Self::DesiredHash(error) => Some(error),
            Self::RecoveryRequired
            | Self::MissingVerifiedSource { .. }
            | Self::SliceFourRequiresSingleCreateAction { .. }
            | Self::SliceFiveRequiresSingleStaleAction { .. } => None,
            Self::SliceSixRequiresSingleReplaceAction { .. }
            | Self::SliceSixRequiresSingleReplaceOwnershipAction { .. }
            | Self::SliceSixRequiresSingleRelocateAction { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::declaration::environment_config::EnvironmentConfig;
    use crate::domain::desired::ResolvedDesired;
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::hashes::desired_hash;
    use crate::domain::ids::ProfileId;
    use crate::domain::paths::SourceRelativePath;
    use crate::inspection::source::{VerifiedSource, resolve_store_root, verify_regular_source};
    use crate::resolver::{ResolverContext, resolve_for_apply};
    use crate::state::operation::ActionStatus;
    #[cfg(unix)]
    use crate::state::repository::{CommitError, CommitStage};

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
                "loadout-apply-coordinator-test-{}-{timestamp}-{unique_id}",
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

        fn coordinator(&self) -> ApplyCoordinator {
            ApplyCoordinator::new(
                ResolvedPath::new(self.path("home")).unwrap(),
                ResolvedPath::new(self.path("state")).unwrap(),
            )
        }

        fn repository(&self) -> StateRepository {
            StateRepository::new(ResolvedPath::new(self.path("state")).unwrap())
        }

        fn desired(&self) -> ResolvedDesired {
            ResolvedDesired::new(
                ProfileId::parse("workstation").unwrap(),
                [ResolvedFileLink::new(
                    FullyQualifiedResourceId::parse("base/git-config").unwrap(),
                    self.verified_source().path().clone(),
                    ResolvedPath::new(self.path("home/.gitconfig")).unwrap(),
                )
                .unwrap()],
            )
            .unwrap()
        }

        fn verified_source(&self) -> VerifiedSource {
            let root = resolve_store_root(&self.path("store")).unwrap();
            verify_regular_source(&root, &SourceRelativePath::parse("git/config").unwrap()).unwrap()
        }

        fn verified_sources(&self) -> BTreeMap<FullyQualifiedResourceId, VerifiedSource> {
            BTreeMap::from([(
                FullyQualifiedResourceId::parse("base/git-config").unwrap(),
                self.verified_source(),
            )])
        }

        fn input(&self) -> ResolvedApplyInput {
            ResolvedApplyInput::new_for_test(self.desired(), self.verified_sources())
        }

        fn replacement_input(&self) -> ResolvedApplyInput {
            let root = resolve_store_root(&self.path("store")).unwrap();
            let replacement = verify_regular_source(
                &root,
                &SourceRelativePath::parse("git/replacement").unwrap(),
            )
            .unwrap();
            let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
            let desired = ResolvedDesired::new(
                ProfileId::parse("workstation").unwrap(),
                [ResolvedFileLink::new(
                    resource_id.clone(),
                    replacement.path().clone(),
                    ResolvedPath::new(self.path("home/.gitconfig")).unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
            ResolvedApplyInput::new_for_test(desired, BTreeMap::from([(resource_id, replacement)]))
        }

        fn ownership_input(&self, resource_id: &str, source_relative: &str) -> ResolvedApplyInput {
            let root = resolve_store_root(&self.path("store")).unwrap();
            let source =
                verify_regular_source(&root, &SourceRelativePath::parse(source_relative).unwrap())
                    .unwrap();
            let resource_id = FullyQualifiedResourceId::parse(resource_id).unwrap();
            let desired = ResolvedDesired::new(
                ProfileId::parse("workstation").unwrap(),
                [ResolvedFileLink::new(
                    resource_id.clone(),
                    source.path().clone(),
                    ResolvedPath::new(self.path("home/.gitconfig")).unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
            ResolvedApplyInput::new_for_test(desired, BTreeMap::from([(resource_id, source)]))
        }

        fn relocation_input(&self) -> ResolvedApplyInput {
            let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
            let source = self.verified_source();
            let desired = ResolvedDesired::new(
                ProfileId::parse("workstation").unwrap(),
                [ResolvedFileLink::new(
                    resource_id.clone(),
                    source.path().clone(),
                    ResolvedPath::new(self.path("home/.config/gitconfig")).unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
            ResolvedApplyInput::new_for_test(desired, BTreeMap::from([(resource_id, source)]))
        }

        fn stale_input(&self) -> ResolvedApplyInput {
            ResolvedApplyInput::new_for_test(
                ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), []).unwrap(),
                BTreeMap::new(),
            )
        }

        fn resolved_input(&self) -> ResolvedApplyInput {
            self.write("config/loadout.yaml", "schema_version: 1\n");
            self.write("config/environment.yaml", "schema_version: 1\n");
            self.write(
                "profiles/workstation.yaml",
                "schema_version: 1\nid: workstation\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/.gitconfig\n      operation: link\n",
            );
            let context = ResolverContext::new(
                self.path("home"),
                self.path("config/loadout.yaml"),
                self.path("config/environment.yaml"),
                self.path("state"),
            )
            .unwrap();
            let environment = EnvironmentConfig::parse(
                "schema_version: 1\ndefault_profile: workstation\nprofile_discovery:\n  paths:\n    - ../profiles\nstores:\n  dotfiles:\n    type: local\n    path: ../store\n",
            )
            .unwrap();

            resolve_for_apply(&context, &environment, None).unwrap()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn shared_coordination_keeps_both_entry_points_restricted_to_one_action() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source contents\n");
        let resources = ["first", "second"].map(|name| {
            ResolvedFileLink::new(
                FullyQualifiedResourceId::parse(&format!("base/{name}")).unwrap(),
                ResolvedPath::new(workspace.path("store/git/config")).unwrap(),
                ResolvedPath::new(workspace.path(&format!("home/{name}"))).unwrap(),
            )
            .unwrap()
        });
        for resources in [Vec::new(), resources.to_vec()] {
            let expected_kinds = vec![ActionKind::CreateLink; resources.len()];
            let resolved = ResolvedApplyInput::new_for_test(
                ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), resources).unwrap(),
                BTreeMap::new(),
            );
            let coordinator = workspace.coordinator();
            assert!(matches!(
                coordinator.apply_create_link(&resolved, |_| panic!("unsupported plan")),
                Err(ApplyError::SliceFourRequiresSingleCreateAction { action_kinds })
                    if action_kinds == expected_kinds
            ));
            assert!(matches!(
                coordinator.apply_stale_link(&resolved, |_| panic!("unsupported plan")),
                Err(ApplyError::SliceFiveRequiresSingleStaleAction { action_kinds })
                    if action_kinds == expected_kinds
            ));
            assert!(!workspace.path("state/state.json").exists());
            assert_eq!(fs::read_dir(workspace.path("home")).unwrap().count(), 0);
            assert_eq!(
                fs::read_to_string(workspace.path("store/git/config")).unwrap(),
                "source contents\n"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn stale_preflight_and_declined_confirmation_preserve_the_complete_state() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source contents\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::remove_file(workspace.path("home/.gitconfig")).unwrap();
        let resolved = workspace.stale_input();
        let before = fs::read(workspace.path("state/state.json")).unwrap();

        // The create entry point must not start accepting stale actions through the shared path.
        assert!(matches!(
            workspace.coordinator().apply_create_link(&resolved, |_| panic!("wrong action kind")),
            Err(ApplyError::SliceFourRequiresSingleCreateAction { action_kinds })
                if action_kinds == [ActionKind::ForgetMissing]
        ));

        let mut coordinator = workspace.coordinator();
        coordinator.fail_next_state_write_preflight();
        assert!(matches!(
            coordinator.apply_stale_link(&resolved, |_| panic!("preflight must finish first")),
            Err(ApplyError::StatePreflight(_))
        ));
        assert_eq!(
            fs::read(workspace.path("state/state.json")).unwrap(),
            before
        );

        let result = workspace
            .coordinator()
            .apply_stale_link(&resolved, |plan| {
                assert_eq!(plan.actions()[0].kind(), ActionKind::ForgetMissing);
                assert_eq!(
                    fs::read(workspace.path("state/state.json")).unwrap(),
                    before
                );
                assert!(matches!(
                    workspace.repository().acquire_exclusive(),
                    Err(StateRepositoryError::LockContended { .. })
                ));
                false
            })
            .unwrap();
        assert!(matches!(result, StaleLinkApplyResult::Declined { .. }));
        assert_eq!(
            fs::read(workspace.path("state/state.json")).unwrap(),
            before
        );
        assert!(!workspace.path("home/.gitconfig").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "source contents\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_success_with_a_failed_commit_retains_running_and_prior_known() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source contents\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::remove_file(workspace.path("home/.gitconfig")).unwrap();
        let repository = workspace.repository();
        let before = repository.load().unwrap();

        let error = workspace
            .coordinator()
            .apply_stale_link_with_after_running(&workspace.stale_input(), |locked| {
                let durable = repository.load().unwrap();
                assert_eq!(durable.known(), before.known());
                let (_, action) = durable
                    .active_operation()
                    .unwrap()
                    .actions()
                    .next()
                    .unwrap();
                assert_eq!(action.status(), ActionStatus::Running);
                locked.fail_next_commit_at(CommitStage::CreateTemporary);
            })
            .unwrap_err();
        assert!(matches!(
            error,
            ApplyError::State(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::CreateTemporary
            }))
        ));
        let after = repository.load().unwrap();
        assert_eq!(after.known(), before.known());
        let (_, action) = after.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Running);
        assert!(!workspace.path("home/.gitconfig").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "source contents\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replace_link_blocks_before_confirmation_without_touching_either_entry() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.replacement_input();
        let error = workspace
            .coordinator()
            .apply_replace_link(&replacement, |_| {
                panic!("an unbound replacement must not request confirmation")
            })
            .unwrap_err();
        assert!(matches!(error, ApplyError::ReplacePreflight(_)));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        let known = state
            .known()
            .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
            .unwrap();
        assert_eq!(
            known.link_target().as_path().as_ref(),
            workspace.path("store/git/config")
        );
    }

    #[cfg(unix)]
    #[test]
    fn same_source_ownership_handoff_records_no_temporary_and_commits_both_identities() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/config");
        let repository = workspace.repository();
        let target = workspace.path("home/.gitconfig");
        let before = fs::read_link(&target).unwrap();

        let result = workspace
            .coordinator()
            .apply_replace_ownership_with_after_running(&replacement, |locked| {
                let _ = locked;
                let state = repository.load().unwrap();
                let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
                assert_eq!(action.status(), ActionStatus::Running);
                assert_eq!(action.kind(), ActionKind::ReplaceOwnership);
                assert!(action.replacement_facts().is_none());
                assert_eq!(
                    action.replaced_resource_id().unwrap().as_str(),
                    "base/git-config"
                );
                assert_eq!(action.resource_id().as_str(), "base/git-config-renamed");
            })
            .unwrap();

        assert!(matches!(
            result,
            ReplaceOwnershipApplyResult::Applied { .. }
        ));
        assert_eq!(fs::read_link(&target).unwrap(), before);
        let state = repository.load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_none()
        );
        let known = state
            .known()
            .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
            .unwrap();
        assert_eq!(
            known.link_target().as_path().as_ref(),
            workspace.path("store/git/config")
        );
    }

    #[cfg(unix)]
    #[test]
    fn same_source_ownership_handoff_retains_an_uncertain_operation_when_recheck_loses_old_link() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/config");
        let target = workspace.path("home/.gitconfig");

        let result = workspace
            .coordinator()
            .apply_replace_ownership_with_after_running(&replacement, |_| {
                fs::remove_file(&target).unwrap();
                fs::write(&target, "unmanaged replacement\n").unwrap();
            })
            .unwrap();

        assert!(matches!(
            result,
            ReplaceOwnershipApplyResult::Uncertain {
                error: ReplaceLinkExecutionError::PreconditionNoLongerHolds { .. }
            }
        ));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "unmanaged replacement\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_none()
        );
        let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
    }

    #[cfg(unix)]
    #[test]
    fn changed_source_ownership_handoff_blocks_without_recording_or_replacing() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/replacement");
        let error = workspace
            .coordinator()
            .apply_replace_ownership(&replacement, |_| {
                panic!("an unbound replacement must not request confirmation")
            })
            .unwrap_err();
        assert!(matches!(error, ApplyError::ReplacePreflight(_)));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn changed_source_ownership_handoff_does_not_run_after_replacement_preflight_fails() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/replacement");
        let error = workspace
            .coordinator()
            .apply_replace_ownership_with_after_running(&replacement, |locked| {
                let _ = locked;
                panic!("preflight must reject before recording running")
            })
            .unwrap_err();

        assert!(matches!(error, ApplyError::ReplacePreflight(_)));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_none()
        );
        assert!(state.active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_handoff_rejects_an_unmanaged_target_without_recording_an_operation() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace.write("store/git/other", "unmanaged source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        fs::remove_file(&target).unwrap();
        let other = workspace.path("store/git/other");
        symlink(&other, &target).unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/config");

        let result = workspace
            .coordinator()
            .apply_replace_ownership(&replacement, |_| {
                panic!("an unmanaged target must not request confirmation")
            })
            .unwrap();

        assert!(matches!(
            result,
            ReplaceOwnershipApplyResult::Blocked { .. }
        ));
        assert_eq!(fs::read_link(&target).unwrap(), other);
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn relocation_blocks_before_confirmation_when_expected_entry_removal_is_unavailable() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let relocation = workspace.relocation_input();

        let error = workspace
            .coordinator()
            .apply_relocate_link(&relocation, |_| {
                panic!("unsupported relocation must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::RelocatePreflight(RelocateLinkExecutionError::RemoveCapability(
                RemoveLinkExecutionError::PlatformCapability { .. }
            ))
        ));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert!(!workspace.path("home/.config/gitconfig").exists());
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn coordinator_locks_plans_preflights_records_executes_and_commits_create() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.resolved_input();

        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();

        assert!(matches!(
            result,
            CreateLinkApplyResult::Applied { ref resource_id }
                if resource_id.as_str() == "workstation/git-config"
        ));
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
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        let known = state
            .known()
            .get(&FullyQualifiedResourceId::parse("workstation/git-config").unwrap())
            .unwrap();
        assert_eq!(known.target_path().as_ref(), target);
        assert_eq!(
            known.source_path().as_ref(),
            workspace.path("store/git/config")
        );
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[test]
    fn blocked_plan_does_not_create_an_operation_record_or_replace_the_target() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        workspace.write("home/.gitconfig", "user-owned contents\n");
        let resolved = workspace.input();

        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| {
                panic!("blocked plans must not request confirmation")
            })
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Blocked { .. }));
        assert_eq!(
            fs::read_to_string(workspace.path("home/.gitconfig")).unwrap(),
            "user-owned contents\n"
        );
        assert!(!workspace.path("state/state.json").exists());
    }

    #[test]
    fn preflight_failure_leaves_target_and_operation_record_absent() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        fs::remove_file(workspace.path("store/git/config")).unwrap();
        fs::create_dir(workspace.path("store/git/config")).unwrap();

        let error = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| {
                panic!("preflight failures must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::Preflight(CreateLinkExecutionError::SourceRecheck(
                crate::inspection::source::SourceVerificationError::SourceNotRegular { .. }
            ))
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert!(!workspace.path("state/state.json").exists());
        assert!(
            fs::metadata(workspace.path("store/git/config"))
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn state_write_preflight_failure_skips_confirmation_and_leaves_no_operation_record() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let mut coordinator = workspace.coordinator();
        coordinator.fail_next_state_write_preflight();

        let error = coordinator
            .apply_create_link(&resolved, |_| {
                panic!("state write preflight failures must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::StatePreflight(StateRepositoryError::StateWritePreflight { .. })
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert!(!workspace.path("state/state.json").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[test]
    fn capability_preflight_failure_skips_confirmation_and_leaves_no_operation_record() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_failure_for_test();

        let error = coordinator
            .apply_create_link(&resolved, |_| {
                panic!("capability preflight failures must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::Preflight(CreateLinkExecutionError::PlatformCapability { .. })
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert!(!workspace.path("state/state.json").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_is_preflight_blocked_until_no_follow_parent_traversal_is_available() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();

        let error = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| {
                panic!("a Windows capability failure must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::Preflight(CreateLinkExecutionError::PlatformCapability { .. })
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert!(!workspace.path("state/state.json").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[test]
    fn declined_confirmation_follows_preflight_and_leaves_no_operation_record() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();

        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |plan| {
                assert!(plan.is_executable());
                assert_eq!(plan.actions().len(), 1);
                false
            })
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Declined { .. }));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert!(!workspace.path("state/state.json").exists());
    }

    #[test]
    fn running_is_durable_before_executor_recheck_and_an_uncertain_result_is_retained() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let repository = workspace.repository();
        let target = workspace.path("home/.gitconfig");

        let result = workspace
            .coordinator()
            .apply_create_link_with_after_running(&resolved, |_| {
                let state = repository.load().unwrap();
                let operation = state.active_operation().unwrap();
                let (_, action) = operation.actions().next().unwrap();
                assert_eq!(action.status(), ActionStatus::Running);
                assert!(state.known().resources().next().is_none());
                fs::write(&target, "appeared after planning\n").unwrap();
            })
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_to_string(workspace.path("home/.gitconfig")).unwrap(),
            "appeared after planning\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        let operation = state.active_operation().unwrap();
        let (_, action) = operation.actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn successful_create_with_a_failed_state_commit_retains_the_link_and_running_operation() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let target = workspace.path("home/.gitconfig");

        let error = workspace
            .coordinator()
            .apply_create_link_with_after_running(&resolved, |locked| {
                locked.fail_next_commit_at(CommitStage::CreateTemporary);
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::State(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::CreateTemporary
            }))
        ));
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&target).unwrap(),
            resolved.desired().resources()[0]
                .link_target()
                .as_path()
                .as_ref()
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        let operation = state.active_operation().unwrap();
        let (_, action) = operation.actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Running);
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_owned_link_removal_fails_preflight_without_an_operation_or_target_mutation() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let stale = workspace.stale_input();

        let error = workspace
            .coordinator()
            .apply_stale_link(&stale, |plan| {
                assert_eq!(plan.actions()[0].kind(), ActionKind::RemoveLink);
                panic!("an unsupported removal capability must not request confirmation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::StalePreflight(StaleLinkExecutionError::Remove(
                RemoveLinkExecutionError::PlatformCapability { .. }
            ))
        ));
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
        assert!(
            fs::symlink_metadata(workspace.path("home"))
                .unwrap()
                .is_dir()
        );
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_some());
        assert!(state.active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn stale_missing_target_forgets_only_its_known_record() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        fs::remove_file(workspace.path("home/.gitconfig")).unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link(&stale, |plan| {
                assert_eq!(plan.actions()[0].kind(), ActionKind::ForgetMissing);
                true
            })
            .unwrap();

        assert!(matches!(
            result,
            StaleLinkApplyResult::Applied {
                kind: ActionKind::ForgetMissing,
                ..
            }
        ));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "[user]\nname = Example\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert!(state.active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn forget_missing_rechecks_after_running_and_retains_known_when_a_target_appears() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        fs::remove_file(&target).unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link_with_after_running(&stale, |_| {
                fs::write(&target, "appeared after planning\n").unwrap();
            })
            .unwrap();

        assert!(matches!(result, StaleLinkApplyResult::Failed { .. }));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "appeared after planning\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_unmanaged_replacement_blocks_without_changing_target_or_known_state() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace.write("store/git/other", "unmanaged source\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        fs::remove_file(&target).unwrap();
        let other = workspace.path("store/git/other");
        symlink(&other, &target).unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link(&stale, |_| {
                panic!("a blocked ownership conflict must not request confirmation")
            })
            .unwrap();

        assert!(matches!(result, StaleLinkApplyResult::Blocked { .. }));
        assert_eq!(fs::read_link(&target).unwrap(), other);
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "owned source\n"
        );
        assert!(
            fs::symlink_metadata(workspace.path("home"))
                .unwrap()
                .is_dir()
        );
        assert!(
            workspace
                .repository()
                .load()
                .unwrap()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_unsafe_parent_blocks_without_touching_the_outside_target_or_known_state() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        fs::rename(workspace.path("home"), workspace.path("former-home")).unwrap();
        fs::create_dir(workspace.path("outside")).unwrap();
        workspace.write("outside/.gitconfig", "outside contents\n");
        symlink(workspace.path("outside"), workspace.path("home")).unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link(&stale, |_| {
                panic!("an unsafe parent must not request confirmation")
            })
            .unwrap();

        assert!(matches!(result, StaleLinkApplyResult::Blocked { .. }));
        assert_eq!(
            fs::read_to_string(workspace.path("outside/.gitconfig")).unwrap(),
            "outside contents\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "owned source\n"
        );
        assert!(
            workspace
                .repository()
                .load()
                .unwrap()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_remove_capability_failure_does_not_invoke_the_after_running_hook() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let stale = workspace.stale_input();

        let error = workspace
            .coordinator()
            .apply_stale_link_with_after_running(&stale, |locked| {
                let _ = locked;
                panic!("preflight must reject before creating an operation")
            })
            .unwrap_err();

        assert!(matches!(
            error,
            ApplyError::StalePreflight(StaleLinkExecutionError::Remove(
                RemoveLinkExecutionError::PlatformCapability { .. }
            ))
        ));
        let target = workspace.path("home/.gitconfig");
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(state.active_operation().is_none());
    }

    #[test]
    fn running_operation_blocks_a_fresh_plan_without_changing_the_target() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        drop(locked);
        workspace.write("home/.gitconfig", "leave untouched\n");

        let error = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap_err();

        assert!(matches!(error, ApplyError::RecoveryRequired));
        assert_eq!(
            fs::read_to_string(workspace.path("home/.gitconfig")).unwrap(),
            "leave untouched\n"
        );
        let state = workspace.repository().load().unwrap();
        let operation = state.active_operation().unwrap();
        let (_, action) = operation.actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Running);
    }
}
