//! Whole-Plan application coordination and recorded-fact recovery.

use std::fmt;

use crate::domain::actual::TargetObservation;
use crate::domain::file_link::LinkTarget;
use crate::domain::hashes::{CanonicalHashError, desired_hash};
use crate::domain::ids::FullyQualifiedResourceId;
#[cfg(test)]
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{
    ActionKind, Plan, PlannedAction, PlannedResourceAction, TargetCondition,
};
use crate::executor::file_copy::{CopyPreflightError, FileCopyExecutor};
use crate::executor::file_link::{
    CreateLinkExecutionError, FileLinkExecutor, ForgetMissingExecutionError,
    RelocateLinkExecutionError, RemoveLinkExecutionError, ReplaceLinkExecutionError,
};
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::planner::plan;
#[cfg(test)]
use crate::resolver::ResolvedApplyInput;
use crate::state::operation::{ActionId, ActionStatus, RecordedAction};
use crate::state::repository::{
    CommitFailureEffect, LockedStateRepository, OperationOutcome, StateRepository,
    StateRepositoryError,
};

/// Failure stage is independent of error type (notably recovery vs execution uncertainty).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplyStage {
    LockAndState,
    Recovery,
    Resolution,
    Planning,
    Preflight,
    OperationCreation,
    Execution,
    Closure,
}

#[derive(Debug)]
pub(crate) enum ApplyFailureCause {
    Lifecycle(ApplyError),
    Input(super::queries::QueryError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApplyAction {
    pub(crate) action_id: Option<ActionId>,
    pub(crate) resource_id: FullyQualifiedResourceId,
    pub(crate) kind: ActionKind,
}

#[derive(Debug)]
pub(crate) struct ApplyFailure {
    pub(crate) stage: ApplyStage,
    pub(crate) cause: ApplyFailureCause,
    pub(crate) affected_action: Option<ApplyAction>,
    /// None means locking/loading failed, not that no operation exists.
    pub(crate) operation: Option<OperationOutcome>,
    /// Qualifies the recorded statuses; replacement alone is not durable success.
    pub(crate) commit_failure: Option<CommitFailureEffect>,
    /// Only actions whose success commit returned successfully.
    pub(crate) committed: Vec<FullyQualifiedResourceId>,
    pub(crate) plan: Option<Plan>,
}

#[derive(Debug)]
pub(crate) enum ApplyReport {
    Applied {
        plan: Plan,
        committed: Vec<FullyQualifiedResourceId>,
    },
    Blocked {
        plan: Plan,
    },
    Declined {
        plan: Plan,
    },
}

/// Accepts declaration selection; no portable declaration is read before recovery.
pub(crate) fn apply_request(
    request: &super::queries::DeclarationRequest,
    confirm: impl FnOnce(&Plan) -> bool,
) -> Result<ApplyReport, Box<ApplyFailure>> {
    apply_request_with_hooks(request, confirm, |_, _| {})
}

fn apply_request_with_hooks(
    request: &super::queries::DeclarationRequest,
    confirm: impl FnOnce(&Plan) -> bool,
    after_running: impl FnMut(usize, &mut LockedStateRepository),
) -> Result<ApplyReport, Box<ApplyFailure>> {
    let mut stage = ApplyStage::LockAndState;
    let mut visible_plan = None;
    let mut committed = Vec::new();
    let mut affected_action = None;
    let mut locked_session = None;
    let result = (|| -> Result<ApplyReport, ApplyFailureCause> {
        let lifecycle = ApplyFailureCause::Lifecycle;
        let state_error = |error| lifecycle(ApplyError::State(error));
        let repository = StateRepository::new(request.context.state_directory().clone());
        locked_session = Some(repository.acquire_exclusive().map_err(state_error)?);
        let locked = locked_session
            .as_mut()
            .expect("exclusive session was acquired");
        stage = ApplyStage::Recovery;
        if reconcile_active_operation_with_action(
            locked,
            request.context.home_directory().as_ref(),
            &mut affected_action,
        )
        .map_err(lifecycle)?
        {
            return Err(lifecycle(ApplyError::RecoveryRequired));
        }
        stage = ApplyStage::Resolution;
        let resolved =
            super::queries::resolve_request(request).map_err(ApplyFailureCause::Input)?;
        stage = ApplyStage::Planning;
        let inspector = FileLinkInspector::new(request.context.home_directory().as_ref())
            .map_err(|error| lifecycle(ApplyError::InitialInspection(error)))?;
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .map_err(|error| lifecycle(ApplyError::InitialInspection(error)))?;
        let fresh_plan = plan(resolved.desired(), locked.state().known(), &actual);
        visible_plan = Some(fresh_plan.clone());
        if !fresh_plan.is_executable() {
            return Ok(ApplyReport::Blocked { plan: fresh_plan });
        }
        stage = ApplyStage::Preflight;
        let executor = FileLinkExecutor::new(request.context.home_directory().as_ref())
            .map_err(|error| lifecycle(ApplyError::InitialInspection(error)))?;
        let copy_executor = FileCopyExecutor::new(request.context.home_directory().as_ref())
            .map_err(|error| lifecycle(ApplyError::InitialInspection(error)))?;
        for action in fresh_plan.resource_actions() {
            affected_action = Some(ApplyAction {
                action_id: None,
                resource_id: action.resource_id().clone(),
                kind: action.kind(),
            });
            super::dispatch::preflight(&executor, &copy_executor, action, &resolved)
                .map_err(lifecycle)?;
        }
        affected_action = None;
        locked
            .preflight_writable()
            .map_err(|error| lifecycle(ApplyError::StatePreflight(error)))?;
        let hash = desired_hash(resolved.desired())
            .map_err(|error| lifecycle(ApplyError::DesiredHash(error)))?;
        if !confirm(&fresh_plan) {
            return Ok(ApplyReport::Declined { plan: fresh_plan });
        }
        // Noop is a report, without an executor phase or a Known transition.
        let resource_actions = fresh_plan
            .resource_actions()
            .iter()
            .filter(|action| action.kind() != ActionKind::Noop)
            .cloned()
            .collect::<Vec<_>>();
        if !resource_actions.is_empty() {
            stage = ApplyStage::OperationCreation;
            let ids = locked
                .begin_resource_actions(hash, &resource_actions)
                .map_err(state_error)?;
            stage = ApplyStage::Execution;
            execute_resource_actions(
                locked,
                request.context.home_directory().as_ref(),
                &resource_actions,
                &ids,
                &mut committed,
                &mut affected_action,
                after_running,
                |action, recorded| {
                    super::dispatch::execute(&executor, &copy_executor, action, recorded, &resolved)
                },
            )
            .map_err(lifecycle)?;
            stage = ApplyStage::Closure;
            locked.close_finished_operation().map_err(state_error)?;
        }
        Ok(ApplyReport::Applied {
            plan: fresh_plan,
            committed: committed.clone(),
        })
    })();
    result.map_err(|cause| {
        // Capture repository-owned facts while the exclusive session still lives.
        let operation = locked_session
            .as_ref()
            .map(LockedStateRepository::operation_outcome);
        let commit_failure = match &cause {
            ApplyFailureCause::Lifecycle(ApplyError::State(StateRepositoryError::Commit(
                error,
            ))) => Some(error.effect()),
            _ => None,
        };
        Box::new(ApplyFailure {
            stage,
            cause,
            affected_action,
            operation,
            commit_failure,
            committed,
            plan: visible_plan,
        })
    })
}

/// Dry run deliberately shares only the read-only planning path.
pub(crate) fn dry_run(
    request: &super::queries::DeclarationRequest,
) -> Result<super::queries::PlanReport, super::queries::QueryError> {
    super::queries::plan_request(request)
}

#[allow(clippy::too_many_arguments)] // The shared coordinator owns the complete recorded-action lifecycle boundary.
fn execute_resource_actions(
    locked: &mut LockedStateRepository,
    home_directory: &std::path::Path,
    actions: &[PlannedResourceAction],
    ids: &[crate::state::operation::ActionId],
    committed: &mut Vec<FullyQualifiedResourceId>,
    affected_action: &mut Option<ApplyAction>,
    mut after_running: impl FnMut(usize, &mut LockedStateRepository),
    mut execute: impl FnMut(
        &PlannedResourceAction,
        &RecordedAction,
    ) -> Result<(), super::dispatch::ResourceExecutionError>,
) -> Result<(), ApplyError> {
    for (index, (action, id)) in actions.iter().zip(ids).enumerate() {
        *affected_action = Some(ApplyAction {
            action_id: Some(id.clone()),
            resource_id: action.resource_id().clone(),
            kind: action.kind(),
        });
        locked.mark_running(id).map_err(ApplyError::State)?;
        after_running(index, locked);
        let recorded = locked
            .state()
            .active_operation()
            .and_then(|op| op.action(id))
            .expect("complete operation was persisted before execution")
            .clone();
        let execution = execute(action, &recorded);
        let classification = match &execution {
            Ok(()) => ExecutionClassification::Succeeded,
            Err(error) => classify_resource_execution_error(home_directory, &recorded, error),
        };
        match classification {
            ExecutionClassification::Succeeded => {
                // A commit error stops immediately: the durable record, not an in-memory guess, determines recovery on the next invocation.
                locked.commit_succeeded(id).map_err(ApplyError::State)?;
                committed.push(action.resource_id().clone());
            }
            ExecutionClassification::Failed | ExecutionClassification::Uncertain => {
                let status = if classification == ExecutionClassification::Failed {
                    ActionStatus::Failed
                } else {
                    ActionStatus::Uncertain
                };
                locked
                    .mark_without_known(id, status)
                    .map_err(ApplyError::State)?;
                let failed_action = affected_action.clone();
                for (pending, action) in ids[index + 1..].iter().zip(&actions[index + 1..]) {
                    *affected_action = Some(ApplyAction {
                        action_id: Some(pending.clone()),
                        resource_id: action.resource_id().clone(),
                        kind: action.kind(),
                    });
                    locked
                        .mark_without_known(pending, ActionStatus::Skipped)
                        .map_err(ApplyError::State)?;
                }
                *affected_action = failed_action;
                if classification == ExecutionClassification::Failed {
                    locked
                        .close_finished_operation()
                        .map_err(ApplyError::State)?;
                }
                return execution.map_err(ApplyError::ResourceExecution);
            }
        }
    }
    *affected_action = None;
    Ok(())
}

/// Coordinates a confirmed non-dry-run apply for one home and state directory.
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApplyCoordinator {
    home_directory: ResolvedPath,
    state_repository: StateRepository,
    #[cfg(test)]
    force_capability_failure: bool,
    #[cfg(test)]
    force_capability_success: bool,
}

#[cfg(test)]
impl ApplyCoordinator {
    /// Binds resolved machine paths without inspecting targets or writing state.
    pub(crate) fn new(home_directory: ResolvedPath, state_directory: ResolvedPath) -> Self {
        Self {
            home_directory,
            state_repository: StateRepository::new(state_directory),
            #[cfg(test)]
            force_capability_failure: false,
            #[cfg(test)]
            force_capability_success: false,
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
            let executor = self.test_executor(
                FileLinkExecutor::new(self.home_directory.as_ref())
                    .map_err(ApplyError::InitialInspection)?,
            );
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
                let executor = self.test_executor(
                    FileLinkExecutor::new(self.home_directory.as_ref())
                        .map_err(ApplyError::InitialInspection)?,
                );
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
            let executor = self.test_executor(
                FileLinkExecutor::new(self.home_directory.as_ref())
                    .map_err(ApplyError::InitialInspection)?,
            );
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
            let executor = self.test_executor(
                FileLinkExecutor::new(self.home_directory.as_ref())
                    .map_err(ApplyError::InitialInspection)?,
            );
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
            let executor = self.test_executor(
                FileLinkExecutor::new(self.home_directory.as_ref())
                    .map_err(ApplyError::InitialInspection)?,
            );
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
        if reconcile_active_operation(&mut locked, self.home_directory.as_ref())? {
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
        assert!(!self.force_capability_success);
        self.force_capability_failure = true;
    }

    fn force_capability_success_for_test(&mut self) {
        assert!(!self.force_capability_failure);
        self.force_capability_success = true;
    }

    fn test_executor(&self, executor: FileLinkExecutor) -> FileLinkExecutor {
        if self.force_capability_failure {
            executor.with_forced_capability_failure_for_test()
        } else if self.force_capability_success {
            executor.with_forced_capability_success_for_test()
        } else {
            executor
        }
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

fn classify_resource_execution_error(
    home_directory: &std::path::Path,
    recorded: &RecordedAction,
    error: &super::dispatch::ResourceExecutionError,
) -> ExecutionClassification {
    match error {
        super::dispatch::ResourceExecutionError::CreateLink(error) => {
            classify_execution_error(error)
        }
        super::dispatch::ResourceExecutionError::ReplaceLink(error) => {
            classify_replace_execution_error(error)
        }
        super::dispatch::ResourceExecutionError::RelocateLink(error) => {
            classify_relocate_execution_error(error)
        }
        super::dispatch::ResourceExecutionError::StaleLink(error) => {
            classify_stale_execution_error(error)
        }
        super::dispatch::ResourceExecutionError::CreateCopy(_)
        | super::dispatch::ResourceExecutionError::ReplaceCopy(_)
        | super::dispatch::ResourceExecutionError::RelocateCopy(_)
        | super::dispatch::ResourceExecutionError::RemoveCopy(_)
        | super::dispatch::ResourceExecutionError::CopyStateOnly(_)
        | super::dispatch::ResourceExecutionError::LinkToCopyHandoff(_)
        | super::dispatch::ResourceExecutionError::CopyToLinkHandoff(_) => {
            FileLinkInspector::new(home_directory)
                .and_then(|inspector| recovery_decision(&inspector, recorded))
                .map(recovery_decision_as_execution_classification)
                .unwrap_or(ExecutionClassification::Uncertain)
        }
        super::dispatch::ResourceExecutionError::InvalidEffectHandoff => {
            ExecutionClassification::Uncertain
        }
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

fn recovery_decision_as_execution_classification(
    decision: RecoveryDecision,
) -> ExecutionClassification {
    match decision {
        RecoveryDecision::Succeeded => ExecutionClassification::Succeeded,
        RecoveryDecision::Failed => ExecutionClassification::Failed,
        RecoveryDecision::Uncertain => ExecutionClassification::Uncertain,
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
        // Matching bytes after a failed create do not establish who created the link. In particular EEXIST must never turn an external link into
        // Known state, even when it appeared after the final recheck.
        CreateLinkExecutionError::CreateAttemptFailed {
            aftermath: TargetObservation::ExpectedLink { .. },
            ..
        } => ExecutionClassification::Uncertain,
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
    CopyPreflight(CopyPreflightError),
    ResourceExecution(super::dispatch::ResourceExecutionError),
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
            Self::CopyPreflight(error) => error.fmt(formatter),
            Self::ResourceExecution(error) => error.fmt(formatter),
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
            Self::CopyPreflight(error) => Some(error),
            Self::ResourceExecution(error) => Some(error),
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

/// Reconciles recorded facts only; it never executes an old plan or mutates a declared target except for the expressly authorized exact replacement-temporary cleanup. `true` means an uncertain record remains open.
fn reconcile_active_operation(
    locked: &mut LockedStateRepository,
    home_directory: &std::path::Path,
) -> Result<bool, ApplyError> {
    reconcile_active_operation_with_action(locked, home_directory, &mut None)
}

fn reconcile_active_operation_with_action(
    locked: &mut LockedStateRepository,
    home_directory: &std::path::Path,
    affected_action: &mut Option<ApplyAction>,
) -> Result<bool, ApplyError> {
    reconcile_active_operation_with_action_and_cleanup(
        locked,
        home_directory,
        affected_action,
        || FileLinkExecutor::new(home_directory).ok(),
    )
}

/// Reconciles recorded facts with an internal cleanup executor factory.
/// The factory exists only to keep production capability gating and test-only retained-token evidence separate.
fn reconcile_active_operation_with_action_and_cleanup<F>(
    locked: &mut LockedStateRepository,
    home_directory: &std::path::Path,
    affected_action: &mut Option<ApplyAction>,
    cleanup_executor: F,
) -> Result<bool, ApplyError>
where
    F: Fn() -> Option<FileLinkExecutor>,
{
    #[cfg(test)]
    crate::test_support::assert_mutation_allowed();
    let Some(operation) = locked.state().active_operation() else {
        return Ok(false);
    };
    let actions = operation
        .actions()
        .map(|(action_id, action)| (action_id.clone(), action.clone()))
        .collect::<Vec<_>>();
    let inspector = actions
        .iter()
        .any(|(_, action)| {
            matches!(
                action.status(),
                ActionStatus::Running | ActionStatus::Uncertain
            )
        })
        .then(|| FileLinkInspector::new(home_directory))
        .transpose()
        .ok()
        .flatten();

    for (action_id, action) in actions {
        if !action.status().closes_operation() {
            *affected_action = Some(ApplyAction {
                action_id: Some(action_id.clone()),
                resource_id: action.resource_id().clone(),
                kind: action.kind(),
            });
        }
        match action.status() {
            ActionStatus::Pending => locked
                .mark_without_known(&action_id, ActionStatus::Skipped)
                .map_err(ApplyError::State)?,
            ActionStatus::Running | ActionStatus::Uncertain => {
                let mut decision = inspector
                    .as_ref()
                    .and_then(|inspector| recovery_decision(inspector, &action).ok())
                    .unwrap_or(RecoveryDecision::Uncertain);
                if decision == RecoveryDecision::Uncertain
                    && action.temporary_path().is_some()
                    && inspector.as_ref().is_some_and(|inspector| {
                        condition_holds(inspector, &action.precondition()).unwrap_or(false)
                    })
                {
                    // Cleanup is permitted only when the old recorded effect still holds; a final or unprovable effect must retain its temporary and remain uncertain.
                    let cleaned = if action.replacement_facts().is_some() {
                        cleanup_executor().is_some_and(|executor| {
                            executor
                                .cleanup_recorded_replacement_temporary(&action)
                                .is_ok()
                        })
                    } else {
                        FileCopyExecutor::new(home_directory).is_ok_and(|executor| {
                            executor.cleanup_recorded_temporary(&action).is_ok()
                        })
                    };
                    decision = if cleaned {
                        inspector
                            .as_ref()
                            .and_then(|inspector| recovery_decision(inspector, &action).ok())
                            .unwrap_or(RecoveryDecision::Uncertain)
                    } else {
                        RecoveryDecision::Uncertain
                    };
                }
                match decision {
                    RecoveryDecision::Succeeded => locked
                        .commit_succeeded(&action_id)
                        .map_err(ApplyError::State)?,
                    RecoveryDecision::Failed => locked
                        .mark_without_known(&action_id, ActionStatus::Failed)
                        .map_err(ApplyError::State)?,
                    RecoveryDecision::Uncertain if action.status() == ActionStatus::Running => {
                        locked
                            .mark_without_known(&action_id, ActionStatus::Uncertain)
                            .map_err(ApplyError::State)?
                    }
                    RecoveryDecision::Uncertain => {}
                }
            }
            ActionStatus::Succeeded | ActionStatus::Failed | ActionStatus::Skipped => {}
        }
    }

    if locked
        .state()
        .active_operation()
        .is_some_and(|operation| operation.can_close())
    {
        *affected_action = None;
        locked
            .close_finished_operation()
            .map_err(ApplyError::State)?;
        *affected_action = None;
        Ok(false)
    } else {
        *affected_action = locked.state().active_operation().and_then(|operation| {
            operation
                .actions()
                .find(|(_, action)| action.status() == ActionStatus::Uncertain)
                .map(|(id, action)| ApplyAction {
                    action_id: Some(id.clone()),
                    resource_id: action.resource_id().clone(),
                    kind: action.kind(),
                })
        });
        Ok(true)
    }
}

#[cfg(test)]
fn reconcile_active_operation_with_retained_token_capability_for_test(
    locked: &mut LockedStateRepository,
    home_directory: &std::path::Path,
) -> Result<bool, ApplyError> {
    reconcile_active_operation_with_action_and_cleanup(locked, home_directory, &mut None, || {
        FileLinkExecutor::new(home_directory)
            .ok()
            .map(FileLinkExecutor::with_forced_capability_success_for_test)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryDecision {
    Succeeded,
    Failed,
    Uncertain,
}

fn recovery_decision(
    inspector: &FileLinkInspector,
    action: &RecordedAction,
) -> Result<RecoveryDecision, TargetInspectionError> {
    let temporary_missing = action
        .temporary_path()
        .map(|temporary_path| {
            condition_holds(
                inspector,
                &TargetCondition::Missing {
                    target_path: temporary_path.clone(),
                },
            )
        })
        .transpose()?
        .unwrap_or(true);
    if matches!(
        action.kind(),
        ActionKind::CreateLink | ActionKind::CreateCopy
    ) {
        // A matching effect does not prove that this create created it.
        if condition_holds(inspector, &action.precondition())? && temporary_missing {
            return Ok(RecoveryDecision::Failed);
        }
        return Ok(RecoveryDecision::Uncertain);
    }

    if action.kind() == ActionKind::RelocateLink {
        let facts = action
            .relocation_facts()
            .expect("validated relocation record has facts");
        let old_missing = condition_holds(
            inspector,
            &TargetCondition::Missing {
                target_path: facts.old_target_path().clone(),
            },
        )?;
        let new_expected = condition_holds(
            inspector,
            &TargetCondition::ExpectedLink {
                target_path: facts.new_target_path().clone(),
                link_target: facts.new_link_target().clone(),
            },
        )?;
        if old_missing && new_expected && temporary_missing {
            return Ok(RecoveryDecision::Succeeded);
        }
        let old_expected = condition_holds(
            inspector,
            &TargetCondition::ExpectedLink {
                target_path: facts.old_target_path().clone(),
                link_target: facts.old_link_target().clone(),
            },
        )?;
        let new_missing = condition_holds(
            inspector,
            &TargetCondition::Missing {
                target_path: facts.new_target_path().clone(),
            },
        )?;
        return Ok(if old_expected && new_missing && temporary_missing {
            RecoveryDecision::Failed
        } else {
            RecoveryDecision::Uncertain
        });
    }

    if action.kind() == ActionKind::RelocateCopy {
        let facts = action
            .copy_facts()
            .expect("validated copy relocation record has facts");
        let Some(crate::domain::known::KnownResource::FileCopy(old_copy)) = facts.old_effect()
        else {
            return Ok(RecoveryDecision::Uncertain);
        };
        let old_missing = condition_holds(
            inspector,
            &TargetCondition::Missing {
                target_path: old_copy.target_path().clone(),
            },
        )?;
        let new_expected = condition_holds(inspector, &action.postcondition())?;
        if old_missing && new_expected && temporary_missing {
            return Ok(RecoveryDecision::Succeeded);
        }
        let old_expected = condition_holds(
            inspector,
            &TargetCondition::ExpectedCopy {
                target_path: old_copy.target_path().clone(),
                content_fingerprint: old_copy.content_fingerprint().clone(),
            },
        )?;
        let new_missing = condition_holds(
            inspector,
            &TargetCondition::Missing {
                target_path: facts.target_path().clone(),
            },
        )?;
        return Ok(if old_expected && new_missing && temporary_missing {
            RecoveryDecision::Failed
        } else {
            RecoveryDecision::Uncertain
        });
    }

    if action.replacement_facts().is_some() {
        let postcondition = condition_holds(inspector, &action.postcondition())?;
        if postcondition && temporary_missing {
            return Ok(RecoveryDecision::Succeeded);
        }
        let precondition = condition_holds(inspector, &action.precondition())?;
        return Ok(if precondition && temporary_missing {
            RecoveryDecision::Failed
        } else {
            RecoveryDecision::Uncertain
        });
    }

    // Same-source ownership handoff has identical predicates: a matching link proves its state-only postcondition rather than a failed mutation.
    if condition_holds(inspector, &action.postcondition())? && temporary_missing {
        return Ok(RecoveryDecision::Succeeded);
    }
    if condition_holds(inspector, &action.precondition())? && temporary_missing {
        return Ok(RecoveryDecision::Failed);
    }
    Ok(RecoveryDecision::Uncertain)
}

fn condition_holds(
    inspector: &FileLinkInspector,
    condition: &TargetCondition,
) -> Result<bool, TargetInspectionError> {
    match condition {
        TargetCondition::ExpectedCopy {
            target_path,
            content_fingerprint,
        } => Ok(matches!(
            inspector
                .inspect_target_for_expected_copy(target_path, content_fingerprint)?
                .observation(),
            crate::domain::actual::CopyTargetObservation::ExpectedCopy { .. }
        )),
        TargetCondition::ExpectedLink { .. } | TargetCondition::Missing { .. } => {
            let expected = match condition {
                TargetCondition::ExpectedLink { link_target, .. } => link_target.clone(),
                TargetCondition::Missing { target_path } => LinkTarget::new(target_path.clone()),
                TargetCondition::ExpectedCopy { .. } => unreachable!("copy handled above"),
            };
            let actual =
                inspector.inspect_target_for_expected_link(condition.target_path(), &expected)?;
            Ok(match condition {
                TargetCondition::ExpectedLink { .. } => {
                    matches!(actual.observation(), TargetObservation::ExpectedLink { .. })
                }
                TargetCondition::Missing { .. } => {
                    matches!(actual.observation(), TargetObservation::Missing)
                }
                TargetCondition::ExpectedCopy { .. } => unreachable!("copy handled above"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    #[cfg(windows)]
    use std::fs::File;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(windows)]
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::declaration::environment_config::EnvironmentConfig;
    use crate::domain::desired::{ResolvedDesired, ResolvedResource};
    use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::hashes::desired_hash;
    use crate::domain::ids::ProfileId;
    use crate::domain::known::{KnownFileCopy, KnownResource};
    use crate::domain::paths::SourceRelativePath;
    use crate::domain::plan::{PlannedEffectHandoff, PlannedFileCopyAction};
    use crate::inspection::source::{VerifiedSource, resolve_store_root, verify_regular_source};
    use crate::resolver::{ResolverContext, resolve_for_apply};
    use crate::state::operation::ActionStatus;
    #[cfg(unix)]
    use crate::state::repository::{CommitError, CommitStage};
    use sha2::{Digest, Sha256};

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
            let root = ResolvedPath::from_platform_canonicalized(fs::canonicalize(root).unwrap())
                .unwrap()
                .into_path_buf();
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

        fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
            fn visit(path: &std::path::Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
                let metadata = fs::symlink_metadata(path).unwrap();
                if metadata.file_type().is_symlink() {
                    snapshot.insert(
                        path.to_owned(),
                        format!("link:{:?}", fs::read_link(path).unwrap()).into_bytes(),
                    );
                } else if metadata.is_dir() {
                    snapshot.insert(path.to_owned(), b"directory".to_vec());
                    for entry in fs::read_dir(path).unwrap() {
                        visit(&entry.unwrap().path(), snapshot);
                    }
                } else {
                    snapshot.insert(path.to_owned(), fs::read(path).unwrap());
                }
            }
            let mut snapshot = BTreeMap::new();
            visit(&self.root, &mut snapshot);
            snapshot
        }

        fn request(&self, resources: &[(&str, &str)]) -> super::super::queries::DeclarationRequest {
            self.write("config/environment.yaml", "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [../profiles]\nstores:\n  dotfiles:\n    type: local\n    properties:\n      path: ../store\n");
            let mut profile = String::from("schema_version: 2\nid: base\nresources:");
            if resources.is_empty() {
                profile.push_str(" {}\n");
            } else {
                profile.push('\n');
            }
            for (id, target) in resources {
                profile.push_str(&format!("  {id}:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/{target}\n"));
            }
            self.write("profiles/base.yaml", &profile);
            super::super::queries::DeclarationRequest {
                context: ResolverContext::new(
                    self.path("home"),
                    self.path("config/loadout.yaml"),
                    self.path("config/environment.yaml"),
                    self.path("state"),
                )
                .unwrap(),
                root: None,
            }
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
                ResolvedDesired::new(
                    ProfileId::parse("workstation").unwrap(),
                    Vec::<ResolvedFileLink>::new(),
                )
                .unwrap(),
                BTreeMap::new(),
            )
        }

        fn resolved_input(&self) -> ResolvedApplyInput {
            self.write("config/loadout.yaml", "schema_version: 1\n");
            self.write("config/environment.yaml", "schema_version: 1\n");
            self.write(
                "profiles/workstation.yaml",
                "schema_version: 2\nid: workstation\nresources:\n  git-config:\n    type: file\n    properties:\n      kind: file\n      source:\n        store: dotfiles\n        path: git/config\n      target: ~/.gitconfig\n      operation: link\n",
            );
            let context = ResolverContext::new(
                self.path("home"),
                self.path("config/loadout.yaml"),
                self.path("config/environment.yaml"),
                self.path("state"),
            )
            .unwrap();
            let environment = EnvironmentConfig::parse(
                "schema_version: 2\ndefault_profile: workstation\nprofile_discovery:\n  paths:\n    - ../profiles\nstores:\n  dotfiles:\n    type: local\n    properties:\n      path: ../store\n",
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

    fn copy_fingerprint(contents: &[u8]) -> ContentFingerprint {
        let mut hasher = Sha256::new();
        hasher.update(contents);
        ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize())).unwrap()
    }

    fn copy_desired(
        workspace: &TestWorkspace,
        resource_id: FullyQualifiedResourceId,
        source_relative: &str,
        target: ResolvedPath,
        contents: &[u8],
    ) -> ResolvedFileCopy {
        workspace.write(source_relative, std::str::from_utf8(contents).unwrap());
        let store = resolve_store_root(&workspace.path("store")).unwrap();
        let source = verify_regular_source(
            &store,
            &SourceRelativePath::parse(source_relative.strip_prefix("store/").unwrap()).unwrap(),
        )
        .unwrap();
        ResolvedFileCopy::new(
            resource_id,
            source.path().clone(),
            target,
            copy_fingerprint(contents),
        )
        .unwrap()
    }

    fn copy_desired_set(desired: ResolvedFileCopy) -> ResolvedDesired {
        ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), [desired]).unwrap()
    }

    fn begin_running_copy_action(
        locked: &mut LockedStateRepository,
        desired: &ResolvedDesired,
        action: PlannedFileCopyAction,
    ) -> ActionId {
        let action = PlannedResourceAction::FileCopy(action);
        let action_id = locked
            .begin_resource_actions(desired_hash(desired).unwrap(), &[action])
            .unwrap()[0]
            .clone();
        locked.mark_running(&action_id).unwrap();
        action_id
    }

    fn commit_known_copy(workspace: &TestWorkspace, desired: ResolvedFileCopy) -> KnownFileCopy {
        let desired_set = copy_desired_set(desired.clone());
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        let action_id = begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Create {
                desired: desired.clone(),
            },
        );
        fs::write(
            desired.target_path(),
            fs::read(desired.source_path()).unwrap(),
        )
        .unwrap();
        locked.commit_succeeded(&action_id).unwrap();
        locked.close_finished_operation().unwrap();
        KnownFileCopy::from_resolved(&desired)
    }

    #[cfg(unix)]
    fn commit_known_link(workspace: &TestWorkspace) -> KnownResource {
        workspace.write("store/git/config", "old link\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        workspace
            .repository()
            .load()
            .unwrap()
            .known()
            .get_variant(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
            .unwrap()
            .clone()
    }

    fn begin_running_effect_handoff(
        locked: &mut LockedStateRepository,
        old_effect: KnownResource,
        final_effect: ResolvedResource,
    ) -> ResolvedPath {
        let desired = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            [final_effect.clone()],
        )
        .unwrap();
        let action = PlannedResourceAction::ReplaceEffect(
            PlannedEffectHandoff::new(old_effect, final_effect).unwrap(),
        );
        let action_id = locked
            .begin_resource_actions(desired_hash(&desired).unwrap(), &[action])
            .unwrap()[0]
            .clone();
        let temporary = locked
            .state()
            .active_operation()
            .unwrap()
            .action(&action_id)
            .unwrap()
            .temporary_path()
            .unwrap()
            .clone();
        locked.mark_running(&action_id).unwrap();
        temporary
    }

    #[cfg(windows)]
    fn begin_running_action(
        workspace: &TestWorkspace,
        resolved: &ResolvedApplyInput,
    ) -> (StateRepository, PlannedAction, RecordedAction) {
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        let recorded = locked
            .state()
            .active_operation()
            .unwrap()
            .action(&action_id)
            .unwrap()
            .clone();
        drop(locked);
        (repository, action, recorded)
    }

    #[cfg(windows)]
    fn retained_token_executor(workspace: &TestWorkspace) -> FileLinkExecutor {
        FileLinkExecutor::new(workspace.path("home").as_path())
            .unwrap()
            .with_forced_capability_success_for_test()
    }

    #[cfg(windows)]
    fn hold_link_without_delete_sharing(path: PathBuf) -> std::io::Result<File> {
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };

        let mut name = path.as_os_str().encode_wide().collect::<Vec<_>>();
        name.push(0);
        // SAFETY: `name` is a NUL-terminated path buffer and all other arguments are valid for a synchronous handle open.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateFileW returned a newly owned file handle.
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }

    #[cfg(unix)]
    fn assert_relocation_recheck_failure_retains_old_known(workspace: &TestWorkspace) {
        let state = workspace.repository().load().unwrap();
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    fn assert_replacement_recheck_failure_retains_known(workspace: &TestWorkspace) {
        let state = workspace.repository().load().unwrap();
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    fn recorded_replacement_temporary(workspace: &TestWorkspace) -> ResolvedPath {
        workspace
            .repository()
            .load()
            .unwrap()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .replacement_facts()
            .unwrap()
            .temporary_path()
            .clone()
    }

    #[cfg(unix)]
    #[test]
    fn whole_plan_records_every_action_then_executes_in_plan_order_under_one_lock() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resources = (0..12)
            .rev()
            .map(|n| (format!("r{n:02}"), format!("target{n:02}")))
            .collect::<Vec<_>>();
        let refs = resources
            .iter()
            .map(|(id, target)| (id.as_str(), target.as_str()))
            .collect::<Vec<_>>();
        let request = workspace.request(&refs);
        let mut observed = Vec::new();
        let report = apply_request_with_hooks(
            &request,
            |plan| {
                assert_eq!(plan.actions().len(), 12);
                assert!(
                    workspace
                        .repository()
                        .load()
                        .unwrap()
                        .active_operation()
                        .is_none()
                );
                assert!(matches!(
                    workspace.repository().acquire_exclusive(),
                    Err(StateRepositoryError::LockContended { .. })
                ));
                true
            },
            |index, locked| {
                let persisted = workspace.repository().load().unwrap();
                let operation = persisted.active_operation().unwrap();
                assert_eq!(operation.actions().len(), 12);
                assert_eq!(persisted.known().resources().len(), index);
                let running = operation
                    .actions()
                    .find(|(_, action)| action.status() == ActionStatus::Running)
                    .unwrap()
                    .1;
                observed.push(running.resource_id().to_string());
                assert_eq!(locked.state(), &persisted);
            },
        )
        .unwrap();
        let ApplyReport::Applied { committed, .. } = report else {
            panic!("apply failed")
        };
        assert_eq!(
            observed,
            (0..12).map(|n| format!("base/r{n:02}")).collect::<Vec<_>>()
        );
        assert_eq!(
            committed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            observed
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        for n in 0..12 {
            assert_eq!(
                fs::read_link(workspace.path(&format!("home/target{n:02}"))).unwrap(),
                workspace.path("store/git/config")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn later_recheck_failure_retains_earlier_success_and_skips_every_remaining_action() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("c", "c"), ("b", "b"), ("a", "a")]);
        let failure = apply_request_with_hooks(
            &request,
            |_| true,
            |index, _| {
                if index == 1 {
                    workspace.write("home/b", "unmanaged");
                }
            },
        )
        .unwrap_err();
        assert_eq!(failure.stage, ApplyStage::Execution);
        assert_eq!(
            failure
                .committed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["base/a"]
        );
        assert_eq!(
            fs::read_to_string(workspace.path("home/b")).unwrap(),
            "unmanaged"
        );
        assert!(!workspace.path("home/c").exists());
        let state = workspace.repository().load().unwrap();
        assert_eq!(state.known().resources().len(), 1);
        assert_eq!(
            failure.operation,
            Some(OperationOutcome::Retained(
                state.active_operation().unwrap().clone()
            ))
        );
        assert!(failure.commit_failure.is_none());
        assert_eq!(
            failure
                .affected_action
                .as_ref()
                .unwrap()
                .resource_id
                .as_str(),
            "base/b"
        );
        let statuses = state
            .active_operation()
            .unwrap()
            .actions()
            .map(|(_, a)| (a.resource_id().to_string(), a.status()))
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                ("base/a".into(), ActionStatus::Succeeded),
                ("base/b".into(), ActionStatus::Uncertain),
                ("base/c".into(), ActionStatus::Skipped)
            ]
        );
        // Initial recovery uncertainty is a different stage and prevents even reading current YAML.
        workspace.write("config/environment.yaml", "invalid");
        let next = apply_request(&request, |_| {
            panic!("no confirmation after uncertain recovery")
        })
        .unwrap_err();
        assert_eq!(next.stage, ApplyStage::Recovery);
        assert_eq!(next.operation, failure.operation);
        assert_eq!(next.affected_action, failure.affected_action);
        assert!(matches!(
            next.cause,
            ApplyFailureCause::Lifecycle(ApplyError::RecoveryRequired)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn multi_action_commit_failure_recovers_then_plans_current_declarations() {
        for fault in [
            CommitStage::CreateTemporary,
            CommitStage::WriteTemporary,
            CommitStage::FlushTemporary,
            CommitStage::ReopenAndValidate,
            CommitStage::ReplaceState,
            CommitStage::FlushDirectory,
        ] {
            let workspace = TestWorkspace::new();
            workspace.write("store/git/config", "source\n");
            let request = workspace.request(&[("a", "a"), ("b", "b"), ("c", "c")]);
            let failure = apply_request_with_hooks(
                &request,
                |_| true,
                |index, locked| {
                    if index == 1 {
                        locked.fail_next_commit_at(fault);
                    }
                },
            )
            .unwrap_err();
            assert_eq!(failure.stage, ApplyStage::Execution);
            assert_eq!(failure.committed.len(), 1);
            assert_eq!(
                fs::read_link(workspace.path("home/b")).unwrap(),
                workspace.path("store/git/config")
            );
            assert!(!workspace.path("home/c").exists());
            let state = workspace.repository().load().unwrap();
            assert!(state.active_operation().is_some());
            assert_eq!(
                failure.operation,
                Some(OperationOutcome::Retained(
                    state.active_operation().unwrap().clone()
                ))
            );
            let affected = failure.affected_action.as_ref().unwrap();
            assert_eq!(affected.resource_id.as_str(), "base/b");
            assert_eq!(affected.kind, ActionKind::CreateLink);
            let recorded = state
                .active_operation()
                .unwrap()
                .action(affected.action_id.as_ref().unwrap())
                .unwrap();
            assert_eq!(
                recorded.status(),
                if fault == CommitStage::FlushDirectory {
                    ActionStatus::Succeeded
                } else {
                    ActionStatus::Running
                }
            );
            assert_eq!(
                failure.commit_failure,
                Some(if fault == CommitStage::FlushDirectory {
                    CommitFailureEffect::ReplacedDurabilityUnconfirmed
                } else {
                    CommitFailureEffect::PreviousStateRetained
                })
            );

            assert_eq!(
                state.known().resources().len(),
                if fault == CommitStage::FlushDirectory {
                    2
                } else {
                    1
                }
            );
            // Recovery happens despite invalid current input, then resolution fails.
            workspace.write("config/environment.yaml", "invalid");
            let failure =
                apply_request(&request, |_| panic!("invalid input cannot confirm")).unwrap_err();
            if fault != CommitStage::FlushDirectory {
                assert_eq!(failure.stage, ApplyStage::Recovery);
                assert_eq!(
                    failure
                        .affected_action
                        .as_ref()
                        .unwrap()
                        .resource_id
                        .as_str(),
                    "base/b"
                );
                let Some(OperationOutcome::Retained(operation)) = &failure.operation else {
                    panic!("unfinished create must remain retained")
                };
                assert_eq!(
                    operation
                        .actions()
                        .map(|(_, action)| action.status())
                        .collect::<Vec<_>>(),
                    [
                        ActionStatus::Succeeded,
                        ActionStatus::Uncertain,
                        ActionStatus::Skipped
                    ]
                );
                let recovered = workspace.repository().load().unwrap();
                assert_eq!(recovered.active_operation(), Some(operation));
                assert_eq!(recovered.known().resources().len(), 1);
                continue;
            }
            assert_eq!(failure.stage, ApplyStage::Resolution);
            assert!(failure.affected_action.is_none());
            assert!(failure.commit_failure.is_none());
            let Some(OperationOutcome::Closed(operation)) = &failure.operation else {
                panic!("recovered operation must be reported closed")
            };
            assert!(operation.can_close());
            assert_eq!(
                operation
                    .actions()
                    .map(|(_, action)| action.status())
                    .collect::<Vec<_>>(),
                [
                    ActionStatus::Succeeded,
                    ActionStatus::Succeeded,
                    ActionStatus::Skipped
                ]
            );
            let recovered = workspace.repository().load().unwrap();
            assert!(recovered.active_operation().is_none());
            assert_eq!(recovered.known().resources().len(), 2);
            // Changed declarations must determine the next plan, never old pending c.
            let request = workspace.request(&[("a", "a"), ("b", "b"), ("d", "d")]);
            let report = apply_request(&request, |plan| {
                assert_eq!(
                    plan.actions()
                        .iter()
                        .filter(|a| a.kind() == ActionKind::CreateLink)
                        .map(|a| a.resource_id().to_string())
                        .collect::<Vec<_>>(),
                    ["base/d"]
                );
                true
            })
            .unwrap();
            assert!(matches!(report, ApplyReport::Applied { .. }));
            assert!(!workspace.path("home/c").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn skipped_progress_commit_failure_identifies_the_unwritten_action() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("a", "a"), ("b", "b"), ("c", "c")]);
        let failure = apply_request_with_hooks(
            &request,
            |_| true,
            |index, locked| {
                if index == 1 {
                    workspace.write("home/b", "unmanaged");
                    locked.fail_commit_after(1, CommitStage::ReplaceState);
                }
            },
        )
        .unwrap_err();
        assert_eq!(failure.stage, ApplyStage::Execution);
        assert_eq!(
            failure
                .affected_action
                .as_ref()
                .unwrap()
                .resource_id
                .as_str(),
            "base/c"
        );
        assert_eq!(
            failure.commit_failure,
            Some(CommitFailureEffect::PreviousStateRetained)
        );
        let Some(OperationOutcome::Retained(operation)) = failure.operation else {
            panic!("operation must be retained")
        };
        assert_eq!(
            operation
                .actions()
                .map(|(_, action)| action.status())
                .collect::<Vec<_>>(),
            [
                ActionStatus::Succeeded,
                ActionStatus::Uncertain,
                ActionStatus::Pending
            ]
        );
        assert_eq!(
            workspace.repository().load().unwrap().active_operation(),
            Some(&operation)
        );
    }

    #[cfg(unix)]
    #[test]
    fn closure_commit_failure_reports_retained_or_closed_record_with_durability() {
        for fault in [CommitStage::ReplaceState, CommitStage::FlushDirectory] {
            let workspace = TestWorkspace::new();
            workspace.write("store/git/config", "source\n");
            let request = workspace.request(&[("a", "a")]);
            let failure = apply_request_with_hooks(
                &request,
                |_| true,
                |_, locked| {
                    locked.fail_commit_after(1, fault);
                },
            )
            .unwrap_err();
            assert_eq!(failure.stage, ApplyStage::Closure);
            assert!(failure.affected_action.is_none());
            assert_eq!(failure.committed.len(), 1);
            let state = workspace.repository().load().unwrap();
            let operation = match failure.operation.as_ref().unwrap() {
                OperationOutcome::Retained(operation) => {
                    assert_eq!(fault, CommitStage::ReplaceState);
                    assert_eq!(state.active_operation(), Some(operation));
                    assert_eq!(
                        failure.commit_failure,
                        Some(CommitFailureEffect::PreviousStateRetained)
                    );
                    operation
                }
                OperationOutcome::Closed(operation) => {
                    assert_eq!(fault, CommitStage::FlushDirectory);
                    assert!(state.active_operation().is_none());
                    assert_eq!(
                        failure.commit_failure,
                        Some(CommitFailureEffect::ReplacedDurabilityUnconfirmed)
                    );
                    operation
                }
                OperationOutcome::Absent => panic!("the operation began"),
            };
            assert_eq!(
                operation.actions().next().unwrap().1.status(),
                ActionStatus::Succeeded
            );
            let saved = failure.operation.clone();
            // A later invocation may change the repository after the lock is released.
            apply_request(&request, |_| true).unwrap();
            assert_eq!(failure.operation, saved);
        }
    }

    #[cfg(unix)]
    #[test]
    fn empty_noop_and_mixed_plans_confirm_but_record_only_required_transitions() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let empty = workspace.request(&[]);
        let report = apply_request(&empty, |plan| {
            assert!(plan.actions().is_empty());
            true
        })
        .unwrap();
        assert!(matches!(report, ApplyReport::Applied { committed, .. } if committed.is_empty()));
        assert!(!workspace.path("state/state.json").exists());
        let request = workspace.request(&[("a", "a")]);
        apply_request(&request, |_| true).unwrap();
        let before = fs::read(workspace.path("state/state.json")).unwrap();
        assert!(matches!(
            apply_request(&request, |_| false).unwrap(),
            ApplyReport::Declined { .. }
        ));
        let report = apply_request(&request, |plan| {
            assert_eq!(plan.actions()[0].kind(), ActionKind::Noop);
            true
        })
        .unwrap();
        assert!(matches!(report, ApplyReport::Applied { committed, .. } if committed.is_empty()));
        assert_eq!(
            fs::read(workspace.path("state/state.json")).unwrap(),
            before
        );
        let request = workspace.request(&[("a", "a"), ("b", "b")]);
        apply_request_with_hooks(
            &request,
            |_| true,
            |_, locked| {
                let op = locked.state().active_operation().unwrap();
                assert_eq!(op.actions().len(), 1);
                assert_eq!(
                    op.actions().next().unwrap().1.resource_id().as_str(),
                    "base/b"
                );
            },
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn enabled_removal_and_creation_commit_in_deterministic_phases() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let initial = workspace.request(&[("stale", "stale")]);
        apply_request(&initial, |_| true).unwrap();
        let request = workspace.request(&[("new", "new")]);
        let report = apply_request(&request, |plan| {
            assert_eq!(plan.actions()[0].kind(), ActionKind::CreateLink);
            assert_eq!(plan.actions()[1].kind(), ActionKind::RemoveLink);
            true
        })
        .unwrap();
        assert!(matches!(report, ApplyReport::Applied { committed, .. } if committed.len() == 2));
        assert_eq!(
            fs::read_link(workspace.path("home/new")).unwrap(),
            workspace.path("store/git/config")
        );
        assert!(!workspace.path("home/stale").exists());
    }

    #[cfg(unix)]
    #[test]
    fn phase_two_handoff_then_phase_three_forget_are_state_only_and_sequential() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let initial = workspace.request(&[("old", "shared"), ("stale", "stale")]);
        apply_request(&initial, |_| true).unwrap();
        fs::remove_file(workspace.path("home/stale")).unwrap();
        let request = workspace.request(&[("new", "shared"), ("z", "z")]);
        let mut sequence = Vec::new();
        apply_request_with_hooks(
            &request,
            |_| true,
            |_, locked| {
                let op = locked.state().active_operation().unwrap();
                assert!(op.actions().all(|(_, a)| a.replacement_facts().is_none()));
                let running = op
                    .actions()
                    .find(|(_, a)| a.status() == ActionStatus::Running)
                    .unwrap()
                    .1;
                sequence.push(running.kind());
            },
        )
        .unwrap();
        assert_eq!(
            sequence,
            [
                ActionKind::CreateLink,
                ActionKind::ReplaceOwnership,
                ActionKind::ForgetMissing
            ]
        );
        let state = workspace.repository().load().unwrap();
        assert_eq!(
            state
                .known()
                .resources()
                .map(|r| r.resource_id().as_str())
                .collect::<Vec<_>>(),
            ["base/new", "base/z"]
        );
        assert_eq!(
            fs::read_link(workspace.path("home/shared")).unwrap(),
            workspace.path("store/git/config")
        );
    }

    #[cfg(unix)]
    #[test]
    fn failure_in_forget_phase_stops_later_forgets_and_closes_definite_failure() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let initial = workspace.request(&[("old", "shared"), ("stale-a", "a"), ("stale-b", "b")]);
        apply_request(&initial, |_| true).unwrap();
        fs::remove_file(workspace.path("home/a")).unwrap();
        fs::remove_file(workspace.path("home/b")).unwrap();
        let request = workspace.request(&[("new", "shared"), ("z", "z")]);
        let mut started = Vec::new();
        let failure = apply_request_with_hooks(
            &request,
            |_| true,
            |index, locked| {
                started.push(index);
                if index == 2 {
                    workspace.write("home/a", "unmanaged");
                }
                assert!(locked.state().active_operation().is_some());
            },
        )
        .unwrap_err();
        assert_eq!(failure.stage, ApplyStage::Execution);
        assert_eq!(
            failure
                .affected_action
                .as_ref()
                .unwrap()
                .resource_id
                .as_str(),
            "base/stale-a"
        );
        assert!(failure.commit_failure.is_none());
        let Some(OperationOutcome::Closed(operation)) = &failure.operation else {
            panic!("definite failure must report the closed record")
        };
        assert_eq!(
            operation
                .actions()
                .map(|(_, action)| action.status())
                .collect::<Vec<_>>(),
            [
                ActionStatus::Succeeded,
                ActionStatus::Succeeded,
                ActionStatus::Failed,
                ActionStatus::Skipped
            ]
        );
        assert_eq!(started, [0, 1, 2]);
        assert_eq!(
            failure
                .committed
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["base/z", "base/new"]
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert_eq!(
            state
                .known()
                .resources()
                .map(|r| r.resource_id().as_str())
                .collect::<Vec<_>>(),
            ["base/new", "base/stale-a", "base/stale-b", "base/z"]
        );
        assert_eq!(
            fs::read_to_string(workspace.path("home/a")).unwrap(),
            "unmanaged"
        );
        assert!(!workspace.path("home/b").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sequential_executor_boundary_keeps_relocation_contiguous() {
        use std::os::unix::fs::symlink;
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let initial = workspace.request(&[("move", "old"), ("stale", "stale")]);
        apply_request(&initial, |_| true).unwrap();
        fs::remove_file(workspace.path("home/stale")).unwrap();
        let request = workspace.request(&[("move", "new")]);
        let resolved = super::super::queries::resolve_request(&request).unwrap();
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let plan = plan(resolved.desired(), locked.state().known(), &actual);
        assert_eq!(
            plan.resource_actions()
                .iter()
                .map(PlannedResourceAction::kind)
                .collect::<Vec<_>>(),
            [ActionKind::RelocateLink, ActionKind::ForgetMissing]
        );
        let ids = locked
            .begin_resource_actions(
                desired_hash(resolved.desired()).unwrap(),
                plan.resource_actions(),
            )
            .unwrap();
        let mut committed = Vec::new();
        // Controlled executor substitute proves coordinator sequencing; it makes
        // no claim about a supported expected-entry deletion platform primitive.
        execute_resource_actions(
            &mut locked,
            workspace.path("home").as_path(),
            plan.resource_actions(),
            &ids,
            &mut committed,
            &mut None,
            |_, _| {},
            |action, recorded| {
                match action.kind() {
                    ActionKind::RelocateLink => {
                        let facts = recorded.relocation_facts().unwrap();
                        symlink(
                            facts.new_link_target().as_path().as_ref(),
                            facts.new_target_path().as_ref(),
                        )
                        .unwrap();
                        assert!(condition_holds(&inspector, &action.postconditions()[1]).unwrap());
                        let state = workspace.repository().load().unwrap();
                        assert_eq!(
                            state
                                .active_operation()
                                .unwrap()
                                .action(&ids[1])
                                .unwrap()
                                .status(),
                            ActionStatus::Pending
                        );
                        assert_eq!(state.known().resources().len(), 2);
                        fs::remove_file(facts.old_target_path().as_ref()).unwrap();
                        assert!(
                            action
                                .postconditions()
                                .iter()
                                .all(|condition| condition_holds(&inspector, condition).unwrap())
                        );
                    }
                    ActionKind::ForgetMissing => {
                        assert!(!workspace.path("home/old").exists());
                        assert_eq!(
                            fs::read_link(workspace.path("home/new")).unwrap(),
                            workspace.path("store/git/config")
                        );
                        assert_eq!(
                            workspace
                                .repository()
                                .load()
                                .unwrap()
                                .active_operation()
                                .unwrap()
                                .action(&ids[0])
                                .unwrap()
                                .status(),
                            ActionStatus::Succeeded
                        );
                        FileLinkExecutor::new(workspace.path("home").as_path())
                            .unwrap()
                            .execute_forget_missing(match action {
                                PlannedResourceAction::FileLink(action) => action,
                                _ => panic!("unexpected non-link action"),
                            })
                            .unwrap();
                    }
                    _ => panic!("unexpected action"),
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(committed.len(), 2);
        locked.close_finished_operation().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dry_run_leaves_active_operation_untouched_even_while_exclusive_lock_is_held() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("a", "a"), ("b", "b")]);
        let resolved = super::super::queries::resolve_request(&request).unwrap();
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        let actions = resolved
            .desired()
            .resources()
            .iter()
            .cloned()
            .map(PlannedAction::create_link)
            .collect::<Vec<_>>();
        let ids = locked
            .begin_actions(desired_hash(resolved.desired()).unwrap(), &actions)
            .unwrap();
        locked.mark_running(&ids[0]).unwrap();
        let before = fs::read(workspace.path("state/state.json")).unwrap();
        let entries = fs::read_dir(workspace.path("state")).unwrap().count();
        let snapshot = workspace.snapshot();
        let _read_only = crate::test_support::forbid_mutation();
        let report = dry_run(&request).unwrap();
        assert_eq!(workspace.snapshot(), snapshot);
        assert_eq!(report.plan.actions().len(), 2);
        assert!(report.plan.is_executable());
        assert_eq!(
            fs::read(workspace.path("state/state.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read_dir(workspace.path("state")).unwrap().count(),
            entries
        );
        assert_eq!(fs::read_dir(workspace.path("home")).unwrap().count(), 0);
    }

    #[test]
    fn lock_and_invalid_state_fail_before_resolution_or_home_observation() {
        let workspace = TestWorkspace::new();
        let request = workspace.request(&[]);
        fs::remove_dir(workspace.path("home")).unwrap();
        workspace.write("config/environment.yaml", "invalid");
        let locked = workspace.repository().acquire_exclusive().unwrap();
        let failure = apply_request(&request, |_| panic!("no confirmation")).unwrap_err();
        assert_eq!(failure.stage, ApplyStage::LockAndState);
        assert!(failure.operation.is_none());
        assert!(failure.affected_action.is_none());
        assert!(failure.commit_failure.is_none());
        assert!(matches!(
            failure.cause,
            ApplyFailureCause::Lifecycle(ApplyError::State(
                StateRepositoryError::LockContended { .. }
            ))
        ));
        drop(locked);
        workspace.write("state/state.json", "invalid");
        let failure = apply_request(&request, |_| panic!("no confirmation")).unwrap_err();
        assert_eq!(failure.stage, ApplyStage::LockAndState);
        assert!(failure.operation.is_none());
        assert!(failure.affected_action.is_none());
        assert!(failure.commit_failure.is_none());
        assert_eq!(
            fs::read_to_string(workspace.path("state/state.json")).unwrap(),
            "invalid"
        );
    }

    #[test]
    fn dry_run_with_absent_state_has_no_write_or_lock_boundary_calls() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("a", "a")]);
        let before = workspace.snapshot();
        let _read_only = crate::test_support::forbid_mutation();
        let report = dry_run(&request).unwrap();
        assert!(report.plan.is_executable());
        assert_eq!(workspace.snapshot(), before);
        assert!(!workspace.path("state").exists());
    }

    #[cfg(unix)]
    #[test]
    fn diff_observes_known_targets_without_reading_missing_configuration_or_sources() {
        use std::os::unix::fs::symlink;
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        fs::create_dir(workspace.path("home/parent")).unwrap();
        let request = workspace.request(&[
            ("expected", "expected"),
            ("missing", "missing"),
            ("wrong", "wrong"),
            ("file", "file"),
            ("unsafe", "parent/unsafe"),
        ]);
        apply_request(&request, |_| true).unwrap();
        fs::remove_file(workspace.path("home/missing")).unwrap();
        fs::remove_file(workspace.path("home/wrong")).unwrap();
        symlink(workspace.path("other"), workspace.path("home/wrong")).unwrap();
        fs::remove_file(workspace.path("home/file")).unwrap();
        workspace.write("home/file", "unmanaged");
        fs::rename(
            workspace.path("home/parent"),
            workspace.path("moved-parent"),
        )
        .unwrap();
        fs::remove_file(workspace.path("store/git/config")).unwrap();
        fs::remove_file(workspace.path("config/environment.yaml")).unwrap();
        fs::remove_file(workspace.path("profiles/base.yaml")).unwrap();
        let before = workspace.snapshot();
        let _read_only = crate::test_support::forbid_mutation();
        let report =
            super::super::queries::diff(request.context.home_directory(), &workspace.repository())
                .unwrap();
        let observations = report
            .resources
            .iter()
            .filter_map(|(id, actual)| match actual {
                crate::domain::actual::ActualResource::FileLink(actual) => {
                    Some((id.as_str(), actual.observation()))
                }
                crate::domain::actual::ActualResource::FileCopy(_) => None,
            })
            .collect::<BTreeMap<_, _>>();
        assert!(matches!(
            observations["base/expected"],
            TargetObservation::ExpectedLink { .. }
        ));
        assert!(matches!(
            observations["base/missing"],
            TargetObservation::Missing
        ));
        assert!(matches!(
            observations["base/wrong"],
            TargetObservation::OtherLink { .. }
        ));
        assert!(matches!(
            observations["base/file"],
            TargetObservation::OtherEntry { .. }
        ));
        assert!(matches!(
            observations["base/unsafe"],
            TargetObservation::UnsafePath { .. }
        ));
        assert_eq!(workspace.snapshot(), before);
    }

    #[cfg(unix)]
    #[test]
    fn enabled_replacement_actions_allow_an_earlier_create() {
        for kind in [ActionKind::ReplaceLink, ActionKind::ReplaceOwnership] {
            let workspace = TestWorkspace::new();
            workspace.write("store/git/config", "old\n");
            workspace.write("store/git/replacement", "new\n");
            let initial = workspace.request(&[("old", "old")]);
            apply_request(&initial, |_| true).unwrap();
            let request = match kind {
                ActionKind::ReplaceLink => {
                    workspace.request(&[("old", "old"), ("create", "create")])
                }
                ActionKind::ReplaceOwnership => {
                    workspace.request(&[("renamed", "old"), ("create", "create")])
                }
                _ => unreachable!(),
            };
            let yaml = fs::read_to_string(workspace.path("profiles/base.yaml"))
                .unwrap()
                .replace("path: git/config", "path: git/replacement");
            workspace.write("profiles/base.yaml", &yaml);
            let report = apply_request(&request, |plan| {
                assert!(plan.actions().iter().any(|action| action.kind() == kind));
                true
            })
            .unwrap();
            assert!(matches!(report, ApplyReport::Applied { .. }));
        }
    }

    #[test]
    fn validate_all_reports_each_root_and_retains_independent_resolution_errors() {
        use super::super::queries::*;
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("a", "a")]);
        workspace.write(
            "profiles/other.yaml",
            "schema_version: 2\nid: other\nincludes: [{id: missing}]\nresources: {}\n",
        );
        let before = workspace.snapshot();
        let _read_only = crate::test_support::forbid_mutation();
        let _no_targets = crate::test_support::forbid_target_inspection();
        let report = validate(&ValidationRequest {
            context: request.context.clone(),
            selection: ValidationSelection::All,
        })
        .unwrap();
        assert_eq!(report.profiles.len(), 2);
        assert_eq!(report.profiles[0].0.as_str(), "base");
        assert!(report.profiles[0].1.is_ok());
        assert_eq!(report.profiles[1].0.as_str(), "other");
        assert!(report.profiles[1].1.is_err());
        let selected = validate(&ValidationRequest {
            context: request.context.clone(),
            selection: ValidationSelection::Root(None),
        })
        .unwrap();
        assert_eq!(selected.profiles.len(), 1);
        assert_eq!(workspace.snapshot(), before);
        fs::remove_file(workspace.path("profiles/base.yaml")).unwrap();
        fs::remove_file(workspace.path("profiles/other.yaml")).unwrap();
        fs::rename(workspace.path("store"), workspace.path("unavailable-store")).unwrap();
        let before = workspace.snapshot();
        assert!(
            validate(&ValidationRequest {
                context: request.context,
                selection: ValidationSelection::All,
            })
            .is_err()
        );
        assert_eq!(workspace.snapshot(), before);
    }

    #[test]
    fn queries_validate_without_target_or_state_access_and_plan_without_locking() {
        use super::super::queries::*;
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let request = workspace.request(&[("a", "missing-parent/target")]);
        // A state path that cannot be used as a directory must be irrelevant to validation.
        workspace.write("state", "not a directory");
        let no_targets = crate::test_support::forbid_target_inspection();
        let _read_only = crate::test_support::forbid_mutation();
        let report = validate(&ValidationRequest {
            context: request.context.clone(),
            selection: ValidationSelection::All,
        })
        .unwrap();
        assert_eq!(report.profiles.len(), 1);
        assert!(report.profiles[0].1.is_ok());
        drop(no_targets);
        fs::remove_file(workspace.path("state")).unwrap();
        let report = plan_request(&request).unwrap();
        assert!(!report.plan.is_executable());
        assert!(!workspace.path("state").exists());
        assert!(!workspace.path("home/missing-parent").exists());
    }

    #[test]
    fn diff_with_absent_state_needs_neither_home_nor_declarations() {
        let workspace = TestWorkspace::new();
        let home = ResolvedPath::new(workspace.path("absent-home")).unwrap();
        let report = super::super::queries::diff(&home, &workspace.repository()).unwrap();
        assert!(report.resources.is_empty());
        assert!(report.active_operation.is_none());
        assert!(!workspace.path("state").exists());
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
    fn replace_link_replaces_the_managed_link_and_commits_known() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.replacement_input();
        let result = workspace
            .coordinator()
            .apply_replace_link(&replacement, |_| true)
            .unwrap();
        assert!(matches!(result, ReplaceLinkApplyResult::Applied { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/replacement")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        let known = state
            .known()
            .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
            .unwrap();
        assert_eq!(
            known.link_target().as_path().as_ref(),
            workspace.path("store/git/replacement")
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_a_source_change_before_the_final_rename_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace.write("store/git/other", "substituted source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let source = workspace.path("store/git/replacement");
        let other = workspace.path("store/git/other");
        let _hook = on_execution_boundary(
            ExecutionBoundary::BeforeReplacementRenameRecheck,
            move || {
                fs::remove_file(&source)?;
                symlink(other, source)
            },
        );

        let result = workspace
            .coordinator()
            .apply_replace_link(&workspace.replacement_input(), |_| true)
            .unwrap();

        assert!(matches!(result, ReplaceLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert_eq!(
            fs::read_link(recorded_replacement_temporary(&workspace).as_path()).unwrap(),
            workspace.path("store/git/replacement")
        );
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_a_target_change_before_the_final_rename_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace.write("store/git/other", "substituted target\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        let other = workspace.path("store/git/other");
        let _hook = on_execution_boundary(
            ExecutionBoundary::BeforeReplacementRenameRecheck,
            move || {
                fs::remove_file(&target)?;
                symlink(other, target)
            },
        );

        let result = workspace
            .coordinator()
            .apply_replace_link(&workspace.replacement_input(), |_| true)
            .unwrap();

        assert!(matches!(result, ReplaceLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/other")
        );
        assert_replacement_recheck_failure_retains_known(&workspace);
        assert_eq!(
            fs::read_link(recorded_replacement_temporary(&workspace).as_path()).unwrap(),
            workspace.path("store/git/replacement")
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_a_recorded_temporary_change_before_the_final_rename_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace.write("store/git/other", "substituted temporary\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let repository = workspace.repository();
        let other = workspace.path("store/git/other");
        let _hook = on_execution_boundary(
            ExecutionBoundary::BeforeReplacementRenameRecheck,
            move || {
                let temporary = repository
                    .load()
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .active_operation()
                    .unwrap()
                    .actions()
                    .next()
                    .unwrap()
                    .1
                    .replacement_facts()
                    .unwrap()
                    .temporary_path()
                    .clone();
                fs::remove_file(temporary.as_path())?;
                symlink(other, temporary.as_path())
            },
        );

        let result = workspace
            .coordinator()
            .apply_replace_link(&workspace.replacement_input(), |_| true)
            .unwrap();

        assert!(matches!(result, ReplaceLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert_replacement_recheck_failure_retains_known(&workspace);
        assert_eq!(
            fs::read_link(recorded_replacement_temporary(&workspace).as_path()).unwrap(),
            workspace.path("store/git/other")
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_rejects_a_parent_association_change_before_the_final_rename_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let home = workspace.path("home");
        let detached = workspace.path("detached-home");
        let _hook = on_execution_boundary(
            ExecutionBoundary::BeforeReplacementRenameRecheck,
            move || {
                fs::rename(&home, &detached)?;
                fs::create_dir(&home)
            },
        );

        let result = workspace
            .coordinator()
            .apply_replace_link(&workspace.replacement_input(), |_| true)
            .unwrap();

        assert!(matches!(result, ReplaceLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("detached-home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert!(fs::symlink_metadata(workspace.path("home/.gitconfig")).is_err());
        assert_replacement_recheck_failure_retains_known(&workspace);
        let temporary = recorded_replacement_temporary(&workspace);
        let detached_temporary = workspace
            .path("detached-home")
            .join(temporary.as_path().file_name().unwrap());
        assert_eq!(
            fs::read_link(detached_temporary).unwrap(),
            workspace.path("store/git/replacement")
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
    fn changed_source_ownership_handoff_replaces_and_commits_both_identities() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/replacement");
        let result = workspace
            .coordinator()
            .apply_replace_ownership(&replacement, |_| true)
            .unwrap();
        assert!(matches!(
            result,
            ReplaceOwnershipApplyResult::Applied { .. }
        ));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/replacement")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_none()
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn changed_source_ownership_handoff_records_running_before_replacement() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old source\n");
        workspace.write("store/git/replacement", "new source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let replacement = workspace.ownership_input("base/git-config-renamed", "git/replacement");
        let result = workspace
            .coordinator()
            .apply_replace_ownership_with_after_running(&replacement, |locked| {
                assert_eq!(
                    locked
                        .state()
                        .active_operation()
                        .unwrap()
                        .actions()
                        .next()
                        .unwrap()
                        .1
                        .status(),
                    ActionStatus::Running
                );
            })
            .unwrap();
        assert!(matches!(
            result,
            ReplaceOwnershipApplyResult::Applied { .. }
        ));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/replacement")
        );
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_none()
        );
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_some()
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
    fn relocation_creates_the_new_link_removes_the_old_link_and_commits_known() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let relocation = workspace.relocation_input();

        let result = workspace
            .coordinator()
            .apply_relocate_link(&relocation, |plan| {
                assert_eq!(plan.actions()[0].kind(), ActionKind::RelocateLink);
                true
            })
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Applied { .. }));
        assert!(!workspace.path("home/.gitconfig").exists());
        assert_eq!(
            fs::read_link(workspace.path("home/.config/gitconfig")).unwrap(),
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
    }

    #[cfg(unix)]
    #[test]
    fn relocate_keeps_the_old_link_when_the_new_link_changes_before_final_removal_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let new_target = workspace.path("home/.config/gitconfig");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::remove_file(&new_target)?;
                fs::write(&new_target, "substituted unmanaged target")
            });

        let result = workspace
            .coordinator()
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert_eq!(
            fs::read_to_string(workspace.path("home/.config/gitconfig")).unwrap(),
            "substituted unmanaged target"
        );
        assert_relocation_recheck_failure_retains_old_known(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn relocate_keeps_the_old_link_when_the_old_link_changes_before_final_removal_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace.write("store/git/other", "unmanaged source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let old_target = workspace.path("home/.gitconfig");
        let other = workspace.path("store/git/other");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::remove_file(&old_target)?;
                symlink(other, old_target)
            });

        let result = workspace
            .coordinator()
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/other")
        );
        assert_relocation_recheck_failure_retains_old_known(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn relocate_keeps_the_old_link_when_the_source_changes_before_final_removal_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace.write("store/git/other", "substituted source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let source = workspace.path("store/git/config");
        let other = workspace.path("store/git/other");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::remove_file(&source)?;
                symlink(other, source)
            });

        let result = workspace
            .coordinator()
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert_relocation_recheck_failure_retains_old_known(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn relocate_keeps_the_old_link_when_the_new_parent_changes_before_final_removal_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let parent = workspace.path("home/.config");
        let detached = workspace.path("home/detached-config");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::rename(&parent, &detached)?;
                fs::create_dir(&parent)
            });

        let result = workspace
            .coordinator()
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Uncertain { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert!(
            fs::symlink_metadata(workspace.path("home/detached-config/gitconfig"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_relocation_recheck_failure_retains_old_known(&workspace);
    }

    #[cfg(unix)]
    #[test]
    fn relocate_keeps_the_old_link_when_the_old_parent_changes_before_final_removal_recheck() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let home = workspace.path("home");
        let detached = workspace.path("detached-home");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::rename(&home, &detached)?;
                fs::create_dir(&home)
            });

        let result = workspace
            .coordinator()
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(result, RelocateLinkApplyResult::Uncertain { .. }));
        assert!(
            fs::symlink_metadata(workspace.path("detached-home/.gitconfig"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!workspace.path("home/.gitconfig").exists());
        assert_relocation_recheck_failure_retains_old_known(&workspace);
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

    // Requires the supported Unix create path; Windows proves preflight rejection separately.
    #[cfg(unix)]
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
    fn windows_create_preflight_uses_the_retained_parent_capability() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();

        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Applied { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        assert!(
            workspace
                .repository()
                .load()
                .unwrap()
                .active_operation()
                .is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_rejects_a_source_parent_reparse_point_before_mutation() {
        use std::os::windows::fs::symlink_dir;

        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let source_parent = workspace.path("store/git");
        let outside = workspace.path("outside-source");
        let target = workspace.path("home/.gitconfig");
        let moved_source_parent = outside.clone();
        let _hook = on_execution_boundary(ExecutionBoundary::BeforeFinalRecheck, move || {
            fs::rename(&source_parent, &moved_source_parent)?;
            symlink_dir(&moved_source_parent, &source_parent)
        });

        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        assert!(fs::symlink_metadata(target).is_err());
        assert_eq!(
            fs::read_to_string(outside.join("config")).unwrap(),
            "source\n"
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
    }

    #[cfg(windows)]
    #[test]
    fn windows_retained_token_create_commits_known_state_with_test_only_capability_override() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let resolved = workspace.input();
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();

        let result = coordinator.apply_create_link(&resolved, |_| true).unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Applied { .. }));
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
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_parent_moved_after_final_recheck_cannot_commit_handle_local_success() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let home = workspace.path("home");
        let detached = workspace.path("detached-home");
        let _hook = on_execution_boundary(ExecutionBoundary::AfterFinalRecheck, move || {
            fs::rename(&home, &detached)?;
            fs::create_dir(&home)
        });
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();

        let result = coordinator.apply_create_link(&resolved, |_| true).unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        assert!(fs::symlink_metadata(workspace.path("home/.gitconfig")).is_err());
        assert_eq!(
            fs::read_link(workspace.path("detached-home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_retained_token_replace_then_remove_commits_each_state_transition() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old\n");
        workspace.write("store/git/replacement", "new\n");
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();

        let replaced = coordinator
            .apply_replace_link(&workspace.replacement_input(), |_| true)
            .unwrap();
        assert!(matches!(replaced, ReplaceLinkApplyResult::Applied { .. }));
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/replacement")
        );
        let replaced_state = workspace.repository().load().unwrap();
        assert!(replaced_state.active_operation().is_none());
        assert_eq!(
            replaced_state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            workspace.path("store/git/replacement").as_path()
        );

        let removed = coordinator
            .apply_stale_link(&workspace.stale_input(), |_| true)
            .unwrap();
        assert!(matches!(
            removed,
            StaleLinkApplyResult::Applied {
                kind: ActionKind::RemoveLink,
                ..
            }
        ));
        assert!(fs::symlink_metadata(workspace.path("home/.gitconfig")).is_err());
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(state.known().resources().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_retained_token_relocate_commits_new_target_state() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();

        let relocated = coordinator
            .apply_relocate_link(&workspace.relocation_input(), |_| true)
            .unwrap();

        assert!(matches!(relocated, RelocateLinkApplyResult::Applied { .. }));
        assert!(fs::symlink_metadata(workspace.path("home/.gitconfig")).is_err());
        assert_eq!(
            fs::read_link(workspace.path("home/.config/gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert_eq!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .target_path()
                .as_ref(),
            workspace.path("home/.config/gitconfig").as_path()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_recovery_keeps_an_interrupted_retained_token_create_uncertain() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let (repository, action, _) = begin_running_action(&workspace, &resolved);
        let source = resolved
            .verified_sources()
            .get(action.resource_id())
            .unwrap();
        retained_token_executor(&workspace)
            .execute_create(&action, source)
            .unwrap();

        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, recorded) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(recorded.status(), ActionStatus::Uncertain);
        assert!(locked.state().known().resources().next().is_none());
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.path("store/git/config")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_recovery_commits_retained_token_replace_remove_and_relocate() {
        let replacement = TestWorkspace::new();
        replacement.write("store/git/config", "old\n");
        replacement.write("store/git/replacement", "new\n");
        let mut coordinator = replacement.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&replacement.input(), |_| true)
            .unwrap();
        let resolved = replacement.replacement_input();
        let (repository, action, recorded) = begin_running_action(&replacement, &resolved);
        let source = resolved
            .verified_sources()
            .get(action.resource_id())
            .unwrap();
        retained_token_executor(&replacement)
            .execute_replace(&action, &recorded, source)
            .unwrap();
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(
            !reconcile_active_operation_with_retained_token_capability_for_test(
                &mut locked,
                replacement.path("home").as_path(),
            )
            .unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            replacement.path("store/git/replacement").as_path()
        );

        let removal = TestWorkspace::new();
        removal.write("store/git/config", "source\n");
        let mut coordinator = removal.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&removal.input(), |_| true)
            .unwrap();
        let resolved = removal.stale_input();
        let (repository, action, _) = begin_running_action(&removal, &resolved);
        retained_token_executor(&removal)
            .execute_remove(&action)
            .unwrap();
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(!reconcile_active_operation(&mut locked, removal.path("home").as_path()).unwrap());
        assert!(locked.state().active_operation().is_none());
        assert!(locked.state().known().resources().next().is_none());

        let relocation = TestWorkspace::new();
        relocation.write("store/git/config", "source\n");
        fs::create_dir(relocation.path("home/.config")).unwrap();
        let mut coordinator = relocation.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&relocation.input(), |_| true)
            .unwrap();
        let resolved = relocation.relocation_input();
        let (repository, action, recorded) = begin_running_action(&relocation, &resolved);
        let source = resolved
            .verified_sources()
            .get(action.resource_id())
            .unwrap();
        retained_token_executor(&relocation)
            .execute_relocate(&action, &recorded, source)
            .unwrap();
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, relocation.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .target_path()
                .as_ref(),
            relocation.path("home/.config/gitconfig").as_path()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_remove_sharing_denial_after_running_preserves_target_and_known_state() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        let source = workspace.path("store/git/config");
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut holder = None;

        let result = coordinator
            .apply_stale_link_with_after_running(&workspace.stale_input(), |_| {
                let target = target.clone();
                holder = Some(std::thread::spawn(move || {
                    let _held = hold_link_without_delete_sharing(target)?;
                    ready_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok::<_, std::io::Error>(())
                }));
                ready_rx.recv().unwrap();
            })
            .unwrap();

        release_tx.send(()).unwrap();
        holder.unwrap().join().unwrap().unwrap();
        assert!(matches!(result, StaleLinkApplyResult::Failed { .. }));
        assert_eq!(fs::read_link(&target).unwrap(), source);
        let state = workspace.repository().load().unwrap();
        assert!(state.active_operation().is_none());
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_replace_sharing_denial_after_running_preserves_target_and_known_state() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old\n");
        workspace.write("store/git/replacement", "new\n");
        let mut coordinator = workspace.coordinator();
        coordinator.force_capability_success_for_test();
        coordinator
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let target = workspace.path("home/.gitconfig");
        let old_source = workspace.path("store/git/config");
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut holder = None;

        let result = coordinator
            .apply_replace_link_with_hooks(
                &workspace.replacement_input(),
                |_| true,
                |_| {
                    let target = target.clone();
                    holder = Some(std::thread::spawn(move || {
                        let _held = hold_link_without_delete_sharing(target)?;
                        ready_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok::<_, std::io::Error>(())
                    }));
                    ready_rx.recv().unwrap();
                },
            )
            .unwrap();

        release_tx.send(()).unwrap();
        holder.unwrap().join().unwrap().unwrap();
        assert!(matches!(result, ReplaceLinkApplyResult::Uncertain { .. }));
        assert_eq!(fs::read_link(&target).unwrap(), old_source);
        let state = workspace.repository().load().unwrap();
        let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        let temporary = action.replacement_facts().unwrap().temporary_path().clone();
        assert_eq!(
            fs::read_link(temporary.as_path()).unwrap(),
            workspace.path("store/git/replacement")
        );
        assert_eq!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            workspace.path("store/git/config").as_path()
        );
        drop(state);

        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(
            !reconcile_active_operation_with_retained_token_capability_for_test(
                &mut locked,
                workspace.path("home").as_path(),
            )
            .unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert!(fs::symlink_metadata(temporary.as_path()).is_err());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            workspace.path("store/git/config").as_path()
        );
    }

    // Requires the supported Unix create path; Windows proves preflight rejection separately.
    #[cfg(unix)]
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

    // Requires the supported Unix create path; Windows proves preflight rejection separately.
    #[cfg(unix)]
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
    fn native_create_final_recheck_does_not_adopt_a_matching_external_link() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let target = workspace.path("home/.gitconfig");
        let source = workspace.verified_source().path().clone();
        let _hook = on_execution_boundary(ExecutionBoundary::BeforeFinalRecheck, move || {
            std::os::unix::fs::symlink(source.as_ref(), target)
        });
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();
        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        assert!(
            fs::symlink_metadata(workspace.path("home/.gitconfig"))
                .unwrap()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_create_after_final_recheck_matching_external_link_is_not_adopted() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let target = workspace.path("home/.gitconfig");
        let source = workspace.verified_source().path().clone();
        let repository = workspace.repository();
        let _hook = on_execution_boundary(ExecutionBoundary::AfterFinalRecheck, move || {
            let state = repository.load().unwrap();
            assert!(state.known().resources().next().is_none());
            assert_eq!(
                state
                    .active_operation()
                    .unwrap()
                    .actions()
                    .next()
                    .unwrap()
                    .1
                    .status(),
                ActionStatus::Running
            );
            std::os::unix::fs::symlink(source.as_ref(), target)
        });
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();
        let CreateLinkApplyResult::Uncertain {
            error:
                CreateLinkExecutionError::CreateAttemptFailed {
                    source, aftermath, ..
                },
        } = result
        else {
            panic!("a failed create must not adopt the external matching link: {result:?}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(matches!(aftermath, TargetObservation::ExpectedLink { .. }));
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.verified_source().path().as_ref()
        );
        assert_eq!(
            fs::read_to_string(workspace.path("store/git/config")).unwrap(),
            "source"
        );
        let recovery_error = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| panic!("recovery must block planning"))
            .unwrap_err();
        assert!(matches!(recovery_error, ApplyError::RecoveryRequired));
        let state = workspace.repository().load().unwrap();
        let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert!(state.known().resources().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn native_create_error_after_effect_remains_uncertain_despite_matching_postcondition() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let repository = workspace.repository();
        let _hook = on_execution_boundary(ExecutionBoundary::AfterMutationAttempt, move || {
            let state = repository.load().unwrap();
            assert!(state.known().resources().next().is_none());
            assert_eq!(
                state
                    .active_operation()
                    .unwrap()
                    .actions()
                    .next()
                    .unwrap()
                    .1
                    .status(),
                ActionStatus::Running
            );
            Err(std::io::Error::other("injected error after real create"))
        });
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();
        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            workspace.verified_source().path().as_ref()
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_create_unavailable_postobservation_stays_uncertain_during_recovery() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let _hook = on_execution_boundary(ExecutionBoundary::BeforePostObservation, || {
            Err(std::io::Error::other("observation unavailable"))
        });
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();
        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        let repository = workspace.repository();
        let state = repository.load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(reconcile_active_operation(&mut locked, &workspace.path("home")).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(locked.state().known().resources().count(), 0);
        assert!(
            fs::symlink_metadata(workspace.path("home/.gitconfig"))
                .unwrap()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_create_parent_moved_after_recheck_cannot_commit_handle_local_success() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let home = workspace.path("home");
        let detached = workspace.path("detached");
        let _hook = on_execution_boundary(ExecutionBoundary::AfterFinalRecheck, move || {
            fs::rename(&home, &detached)?;
            fs::create_dir(&home)
        });
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();
        assert!(matches!(result, CreateLinkApplyResult::Uncertain { .. }));
        assert!(
            fs::symlink_metadata(workspace.path("detached/.gitconfig"))
                .unwrap()
                .is_symlink()
        );
        assert!(!workspace.path("home/.gitconfig").exists());
        let state = workspace.repository().load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Uncertain
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_create_before_commit_fault_stays_uncertain_during_recovery() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source");
        let resolved = workspace.input();
        let repository = workspace.repository();
        let target = workspace.path("home/.gitconfig");
        let mut hook = None;
        let result = workspace
            .coordinator()
            .apply_create_link_with_after_running(&resolved, |_| {
                hook = Some(on_execution_boundary(
                    ExecutionBoundary::BeforeCommit,
                    move || {
                        let state = repository.load().unwrap();
                        assert_eq!(
                            state
                                .active_operation()
                                .unwrap()
                                .actions()
                                .next()
                                .unwrap()
                                .1
                                .status(),
                            ActionStatus::Running
                        );
                        assert!(state.known().resources().next().is_none());
                        assert!(fs::symlink_metadata(target).unwrap().is_symlink());
                        Err(std::io::Error::other(
                            "injected before verified Known commit",
                        ))
                    },
                ));
            });
        assert!(matches!(
            result,
            Err(ApplyError::State(StateRepositoryError::Commit(_)))
        ));
        let repository = workspace.repository();
        let state = repository.load().unwrap();
        assert!(state.known().resources().next().is_none());
        assert_eq!(
            state
                .active_operation()
                .unwrap()
                .actions()
                .next()
                .unwrap()
                .1
                .status(),
            ActionStatus::Running
        );
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(reconcile_active_operation(&mut locked, &workspace.path("home")).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(locked.state().known().resources().count(), 0);
        drop(hook);
    }

    #[cfg(unix)]
    #[test]
    fn stale_owned_link_removal_removes_the_link_and_commits_known() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "[user]\nname = Example\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link(&stale, |plan| {
                assert_eq!(plan.actions()[0].kind(), ActionKind::RemoveLink);
                true
            })
            .unwrap();

        assert!(matches!(
            result,
            StaleLinkApplyResult::Applied {
                kind: ActionKind::RemoveLink,
                ..
            }
        ));
        let target = workspace.path("home/.gitconfig");
        assert!(!target.exists());
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
        assert!(state.known().resources().next().is_none());
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
    fn stale_remove_records_running_before_removing_the_link() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "owned source\n");
        let current = workspace.input();
        workspace
            .coordinator()
            .apply_create_link(&current, |_| true)
            .unwrap();
        let stale = workspace.stale_input();

        let result = workspace
            .coordinator()
            .apply_stale_link_with_after_running(&stale, |locked| {
                assert_eq!(
                    locked
                        .state()
                        .active_operation()
                        .unwrap()
                        .actions()
                        .next()
                        .unwrap()
                        .1
                        .status(),
                    ActionStatus::Running
                );
            })
            .unwrap();

        assert!(matches!(
            result,
            StaleLinkApplyResult::Applied {
                kind: ActionKind::RemoveLink,
                ..
            }
        ));
        let target = workspace.path("home/.gitconfig");
        assert!(!target.exists());
        let state = workspace.repository().load().unwrap();
        assert!(
            state
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_none()
        );
        assert!(state.active_operation().is_none());
    }

    #[test]
    fn recovery_marks_an_unprovable_running_operation_uncertain_before_blocking_a_fresh_plan() {
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
        assert_eq!(action.status(), ActionStatus::Uncertain);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_keeps_a_running_create_with_matching_link_uncertain() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        symlink(
            workspace.path("store/git/config"),
            workspace.path("home/.gitconfig"),
        )
        .unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert!(locked.state().known().resources().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_commits_a_same_source_handoff_without_mutating_its_target() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let resolved = workspace.ownership_input("base/git-config-renamed", "git/config");
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        let before = fs::read_link(workspace.path("home/.gitconfig")).unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert_eq!(
            fs::read_link(workspace.path("home/.gitconfig")).unwrap(),
            before
        );
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_none()
        );
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_some()
        );
        assert!(locked.state().active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_corrected_uncertain_operation_recovers_before_apply_creates_a_fresh_plan() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        let target = workspace.path("home/.gitconfig");
        fs::write(&target, "unmanaged interruption\n").unwrap();
        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        drop(locked);

        fs::remove_file(&target).unwrap();
        let result = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| true)
            .unwrap();

        assert!(matches!(result, CreateLinkApplyResult::Applied { .. }));
        assert_eq!(
            fs::read_link(&target).unwrap(),
            workspace.path("store/git/config")
        );
        let state = repository.load().unwrap();
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
    fn recovery_keeps_an_uncertain_create_with_matching_link_open() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        locked
            .mark_without_known(&action_id, ActionStatus::Uncertain)
            .unwrap();
        drop(locked);

        let persisted = repository.load().unwrap();
        let (_, action) = persisted
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(persisted.known().resources().len(), 0);

        symlink(
            workspace.path("store/git/config"),
            workspace.path("home/.gitconfig"),
        )
        .unwrap();
        let mut locked = repository.acquire_exclusive().unwrap();
        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        drop(locked);

        let persisted = repository.load().unwrap();
        let (_, action) = persisted
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert!(persisted.known().resources().next().is_none());
    }

    #[test]
    fn recovery_marks_running_actions_uncertain_when_the_home_cannot_be_inspected() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        drop(locked);
        fs::rename(workspace.path("home"), workspace.path("unavailable-home")).unwrap();

        let error = workspace
            .coordinator()
            .apply_create_link(&resolved, |_| panic!("recovery must block planning"))
            .unwrap_err();

        assert!(matches!(error, ApplyError::RecoveryRequired));
        let state = repository.load().unwrap();
        let (_, action) = state.active_operation().unwrap().actions().next().unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(state.known().resources().len(), 0);
    }

    #[test]
    fn recovery_skips_a_pending_action_without_mutating_known_state() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(locked.state().known().resources().len(), 0);
    }

    #[test]
    fn recovery_marks_a_running_create_failed_when_its_precondition_still_holds() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(locked.state().known().resources().len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_keeps_a_partial_relocation_uncertain() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let resolved = workspace.relocation_input();
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        symlink(
            workspace.path("store/git/config"),
            workspace.path("home/.config/gitconfig"),
        )
        .unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_removes_only_the_recorded_expected_replacement_temporary() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old\n");
        workspace.write("store/git/replacement", "new\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let resolved = workspace.replacement_input();
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        let temporary_path = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .replacement_facts()
            .unwrap()
            .temporary_path()
            .clone();
        locked.mark_running(&action_id).unwrap();
        symlink(
            workspace.path("store/git/replacement"),
            temporary_path.as_path(),
        )
        .unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert!(fs::symlink_metadata(temporary_path.as_path()).is_err());
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(locked.state().active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_keeps_a_replacement_with_an_unexpected_temporary_entry_uncertain() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old\n");
        workspace.write("store/git/replacement", "new\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let resolved = workspace.replacement_input();
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        let temporary_path = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .replacement_facts()
            .unwrap()
            .temporary_path()
            .clone();
        locked.mark_running(&action_id).unwrap();
        fs::write(temporary_path.as_path(), "unmanaged temporary entry\n").unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(
            fs::read_to_string(temporary_path.as_path()).unwrap(),
            "unmanaged temporary entry\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_marks_a_running_changed_source_ownership_handoff_failed() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "old\n");
        workspace.write("store/git/replacement", "new\n");
        workspace
            .coordinator()
            .apply_create_link(&workspace.input(), |_| true)
            .unwrap();
        let resolved = workspace.ownership_input("base/git-config-renamed", "git/replacement");
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(workspace.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        assert_eq!(action.kind(), ActionKind::ReplaceOwnership);
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config-renamed").unwrap())
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_commits_or_fails_remove_link_from_its_recorded_conditions() {
        let successful = TestWorkspace::new();
        successful.write("store/git/config", "source\n");
        successful
            .coordinator()
            .apply_create_link(&successful.input(), |_| true)
            .unwrap();
        let repository = successful.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(successful.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(successful.stale_input().desired(), locked.state().known())
            .unwrap();
        let action = plan(
            successful.stale_input().desired(),
            locked.state().known(),
            &actual,
        )
        .actions()[0]
            .clone();
        assert_eq!(action.kind(), ActionKind::RemoveLink);
        let action_id = locked
            .begin_operation(
                desired_hash(successful.stale_input().desired()).unwrap(),
                &action,
            )
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        fs::remove_file(successful.path("home/.gitconfig")).unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, successful.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(locked.state().known().resources().len(), 0);

        let failed = TestWorkspace::new();
        failed.write("store/git/config", "source\n");
        failed
            .coordinator()
            .apply_create_link(&failed.input(), |_| true)
            .unwrap();
        let repository = failed.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(failed.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(failed.stale_input().desired(), locked.state().known())
            .unwrap();
        let action = plan(
            failed.stale_input().desired(),
            locked.state().known(),
            &actual,
        )
        .actions()[0]
            .clone();
        let action_id = locked
            .begin_operation(
                desired_hash(failed.stale_input().desired()).unwrap(),
                &action,
            )
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        assert!(!reconcile_active_operation(&mut locked, failed.path("home").as_path()).unwrap());
        assert!(locked.state().active_operation().is_none());
        assert!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_commits_forget_missing_and_keeps_a_lost_precondition_uncertain() {
        use std::os::unix::fs::symlink;

        let successful = TestWorkspace::new();
        successful.write("store/git/config", "source\n");
        successful
            .coordinator()
            .apply_create_link(&successful.input(), |_| true)
            .unwrap();
        fs::remove_file(successful.path("home/.gitconfig")).unwrap();
        let repository = successful.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(successful.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(successful.stale_input().desired(), locked.state().known())
            .unwrap();
        let action = plan(
            successful.stale_input().desired(),
            locked.state().known(),
            &actual,
        )
        .actions()[0]
            .clone();
        assert_eq!(action.kind(), ActionKind::ForgetMissing);
        let action_id = locked
            .begin_operation(
                desired_hash(successful.stale_input().desired()).unwrap(),
                &action,
            )
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, successful.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(locked.state().known().resources().len(), 0);

        let interrupted = TestWorkspace::new();
        interrupted.write("store/git/config", "source\n");
        interrupted
            .coordinator()
            .apply_create_link(&interrupted.input(), |_| true)
            .unwrap();
        fs::remove_file(interrupted.path("home/.gitconfig")).unwrap();
        let repository = interrupted.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(interrupted.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(interrupted.stale_input().desired(), locked.state().known())
            .unwrap();
        let action = plan(
            interrupted.stale_input().desired(),
            locked.state().known(),
            &actual,
        )
        .actions()[0]
            .clone();
        let action_id = locked
            .begin_operation(
                desired_hash(interrupted.stale_input().desired()).unwrap(),
                &action,
            )
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        symlink(
            interrupted.path("store/git/config"),
            interrupted.path("home/.gitconfig"),
        )
        .unwrap();
        assert!(
            reconcile_active_operation(&mut locked, interrupted.path("home").as_path()).unwrap()
        );
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_commits_or_fails_replace_link_from_its_recorded_conditions() {
        use std::os::unix::fs::symlink;

        let successful = TestWorkspace::new();
        successful.write("store/git/config", "old\n");
        successful.write("store/git/replacement", "new\n");
        successful
            .coordinator()
            .apply_create_link(&successful.input(), |_| true)
            .unwrap();
        let resolved = successful.replacement_input();
        let repository = successful.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(successful.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        assert_eq!(action.kind(), ActionKind::ReplaceLink);
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        fs::remove_file(successful.path("home/.gitconfig")).unwrap();
        symlink(
            successful.path("store/git/replacement"),
            successful.path("home/.gitconfig"),
        )
        .unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, successful.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            successful.path("store/git/replacement").as_path()
        );

        let failed = TestWorkspace::new();
        failed.write("store/git/config", "old\n");
        failed.write("store/git/replacement", "new\n");
        failed
            .coordinator()
            .apply_create_link(&failed.input(), |_| true)
            .unwrap();
        let resolved = failed.replacement_input();
        let repository = failed.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(failed.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        assert!(!reconcile_active_operation(&mut locked, failed.path("home").as_path()).unwrap());
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .source_path()
                .as_ref(),
            failed.path("store/git/config").as_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_commits_or_fails_relocation_from_its_recorded_conditions() {
        use std::os::unix::fs::symlink;

        let successful = TestWorkspace::new();
        successful.write("store/git/config", "source\n");
        successful
            .coordinator()
            .apply_create_link(&successful.input(), |_| true)
            .unwrap();
        fs::create_dir(successful.path("home/.config")).unwrap();
        let resolved = successful.relocation_input();
        let repository = successful.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(successful.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        assert_eq!(action.kind(), ActionKind::RelocateLink);
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        fs::remove_file(successful.path("home/.gitconfig")).unwrap();
        symlink(
            successful.path("store/git/config"),
            successful.path("home/.config/gitconfig"),
        )
        .unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, successful.path("home").as_path()).unwrap()
        );
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .target_path()
                .as_ref(),
            successful.path("home/.config/gitconfig").as_path()
        );

        let failed = TestWorkspace::new();
        failed.write("store/git/config", "source\n");
        failed
            .coordinator()
            .apply_create_link(&failed.input(), |_| true)
            .unwrap();
        fs::create_dir(failed.path("home/.config")).unwrap();
        let resolved = failed.relocation_input();
        let repository = failed.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let inspector = FileLinkInspector::new(failed.path("home").as_path()).unwrap();
        let actual = inspector
            .inspect(resolved.desired(), locked.state().known())
            .unwrap();
        let action = plan(resolved.desired(), locked.state().known(), &actual).actions()[0].clone();
        let action_id = locked
            .begin_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        assert!(!reconcile_active_operation(&mut locked, failed.path("home").as_path()).unwrap());
        assert!(locked.state().active_operation().is_none());
        assert_eq!(
            locked
                .state()
                .known()
                .get(&FullyQualifiedResourceId::parse("base/git-config").unwrap())
                .unwrap()
                .target_path()
                .as_ref(),
            failed.path("home/.gitconfig").as_path()
        );
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_a_running_action_when_its_state_commit_fails() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "source\n");
        let resolved = workspace.input();
        let action = PlannedAction::create_link(resolved.desired().resources()[0].clone());
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_create_operation(desired_hash(resolved.desired()).unwrap(), &action)
            .unwrap();
        locked.mark_running(&action_id).unwrap();
        symlink(
            workspace.path("store/git/config"),
            workspace.path("home/.gitconfig"),
        )
        .unwrap();
        locked.fail_next_commit_at(CommitStage::CreateTemporary);

        let error =
            reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap_err();
        assert!(matches!(
            error,
            ApplyError::State(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::CreateTemporary
            }))
        ));
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Running);
        assert_eq!(locked.state().known().resources().len(), 0);
        drop(locked);
        let persisted = repository.load().unwrap();
        let (_, action) = persisted
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Running);
        assert_eq!(persisted.known().resources().len(), 0);
    }

    #[test]
    fn recovery_keeps_matching_final_copy_create_uncertain_without_known_update() {
        let workspace = TestWorkspace::new();
        workspace.write("store/git/config", "copy source\n");
        let source = workspace.verified_source();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let target = ResolvedPath::new(workspace.path("home/.gitconfig")).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"copy source\n");
        let fingerprint =
            ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize())).unwrap();
        let desired = ResolvedFileCopy::new(
            resource_id.clone(),
            source.path().clone(),
            target.clone(),
            fingerprint,
        )
        .unwrap();
        let desired_set =
            ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), [desired.clone()])
                .unwrap();
        let action = PlannedResourceAction::FileCopy(PlannedFileCopyAction::Create { desired });
        let repository = workspace.repository();
        let mut locked = repository.acquire_exclusive().unwrap();
        let action_id = locked
            .begin_resource_actions(desired_hash(&desired_set).unwrap(), &[action])
            .unwrap()[0]
            .clone();
        locked.mark_running(&action_id).unwrap();
        fs::write(&target, b"copy source\n").unwrap();
        locked
            .mark_without_known(&action_id, ActionStatus::Uncertain)
            .unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, recorded) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(recorded.status(), ActionStatus::Uncertain);
        assert!(locked.state().known().get_variant(&resource_id).is_none());
        assert!(fs::symlink_metadata(target).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_cleans_an_exact_copy_temporary_then_fails_and_keeps_old_known() {
        let workspace = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let target = ResolvedPath::new(workspace.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/old",
            target.clone(),
            b"old\n",
        );
        let previous = commit_known_copy(&workspace, old);
        let desired = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/new",
            target,
            b"new\n",
        );
        let desired_set = copy_desired_set(desired.clone());
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Replace {
                desired: desired.clone(),
                previous: previous.clone(),
            },
        );
        let temporary = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .temporary_path()
            .unwrap()
            .clone();
        fs::write(&temporary, b"new\n").unwrap();

        assert!(
            !reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap()
        );
        assert!(fs::symlink_metadata(temporary).is_err());
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(previous))
        );
        assert!(locked.state().active_operation().is_none());
    }

    #[test]
    fn recovery_keeps_copy_with_retained_or_unexpected_temporary_uncertain() {
        let workspace = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let target = ResolvedPath::new(workspace.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/old",
            target.clone(),
            b"old\n",
        );
        let previous = commit_known_copy(&workspace, old);
        let desired = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/new",
            target,
            b"new\n",
        );
        let desired_set = copy_desired_set(desired.clone());
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Replace {
                desired,
                previous: previous.clone(),
            },
        );
        let temporary = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .temporary_path()
            .unwrap()
            .clone();
        fs::write(&temporary, b"unmanaged temporary\n").unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, recorded) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(recorded.status(), ActionStatus::Uncertain);
        assert_eq!(fs::read(&temporary).unwrap(), b"unmanaged temporary\n");
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(previous))
        );

        let retained = TestWorkspace::new();
        let target = ResolvedPath::new(retained.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &retained,
            resource_id.clone(),
            "store/git/old",
            target.clone(),
            b"old\n",
        );
        let previous = commit_known_copy(&retained, old);
        let desired = copy_desired(
            &retained,
            resource_id.clone(),
            "store/git/new",
            target,
            b"new\n",
        );
        let desired_set = copy_desired_set(desired.clone());
        let mut locked = retained.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Replace {
                desired: desired.clone(),
                previous: previous.clone(),
            },
        );
        let temporary = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .1
            .temporary_path()
            .unwrap()
            .clone();
        fs::write(desired.target_path(), b"new\n").unwrap();
        fs::write(&temporary, b"new\n").unwrap();

        assert!(reconcile_active_operation(&mut locked, retained.path("home").as_path()).unwrap());
        let (_, recorded) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(recorded.status(), ActionStatus::Uncertain);
        assert_eq!(fs::read(&temporary).unwrap(), b"new\n");
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(previous))
        );
    }

    #[test]
    fn recovery_keeps_a_partial_copy_relocation_uncertain_with_old_known() {
        let workspace = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let old_target = ResolvedPath::new(workspace.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/old",
            old_target,
            b"old\n",
        );
        let previous = commit_known_copy(&workspace, old);
        fs::create_dir(workspace.path("home/.config")).unwrap();
        let desired = copy_desired(
            &workspace,
            resource_id.clone(),
            "store/git/new",
            ResolvedPath::new(workspace.path("home/.config/gitconfig")).unwrap(),
            b"new\n",
        );
        let desired_set = copy_desired_set(desired.clone());
        let mut locked = workspace.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Relocate {
                desired: desired.clone(),
                previous: previous.clone(),
            },
        );
        fs::write(desired.target_path(), b"new\n").unwrap();

        assert!(reconcile_active_operation(&mut locked, workspace.path("home").as_path()).unwrap());
        let (_, recorded) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(recorded.status(), ActionStatus::Uncertain);
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(previous))
        );
    }

    #[test]
    fn recovery_removes_copy_known_only_for_the_recorded_missing_postcondition() {
        let successful = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let target = ResolvedPath::new(successful.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &successful,
            resource_id.clone(),
            "store/git/old",
            target.clone(),
            b"old\n",
        );
        let previous = commit_known_copy(&successful, old.clone());
        let desired_set = copy_desired_set(old);
        let mut locked = successful.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Remove { previous },
        );
        fs::remove_file(target).unwrap();
        assert!(
            !reconcile_active_operation(&mut locked, successful.path("home").as_path()).unwrap()
        );
        assert!(locked.state().known().get_variant(&resource_id).is_none());

        let retained = TestWorkspace::new();
        let target = ResolvedPath::new(retained.path("home/.gitconfig")).unwrap();
        let old = copy_desired(
            &retained,
            resource_id.clone(),
            "store/git/old",
            target,
            b"old\n",
        );
        let previous = commit_known_copy(&retained, old.clone());
        let desired_set = copy_desired_set(old);
        let mut locked = retained.repository().acquire_exclusive().unwrap();
        begin_running_copy_action(
            &mut locked,
            &desired_set,
            PlannedFileCopyAction::Remove {
                previous: previous.clone(),
            },
        );
        assert!(!reconcile_active_operation(&mut locked, retained.path("home").as_path()).unwrap());
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(previous))
        );
        assert!(locked.state().active_operation().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_classifies_link_to_copy_handoff_aftermath_from_recorded_effects() {
        let succeeded = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let old = commit_known_link(&succeeded);
        let target = ResolvedPath::new(succeeded.path("home/.gitconfig")).unwrap();
        let final_copy = copy_desired(
            &succeeded,
            resource_id.clone(),
            "store/git/final-copy",
            target.clone(),
            b"final copy\n",
        );
        let mut locked = succeeded.repository().acquire_exclusive().unwrap();
        begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileCopy(final_copy.clone()),
        );
        fs::remove_file(&target).unwrap();
        fs::write(&target, b"final copy\n").unwrap();
        let action_id = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .0
            .clone();
        locked.fail_next_commit_at(CommitStage::CreateTemporary);
        assert!(matches!(
            locked.commit_succeeded(&action_id),
            Err(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::CreateTemporary
            }))
        ));
        assert!(
            !reconcile_active_operation(&mut locked, succeeded.path("home").as_path()).unwrap()
        );
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileCopy(KnownFileCopy::from_resolved(
                &final_copy
            )))
        );

        let failed = TestWorkspace::new();
        let old = commit_known_link(&failed);
        let target = ResolvedPath::new(failed.path("home/.gitconfig")).unwrap();
        let final_copy = copy_desired(
            &failed,
            resource_id.clone(),
            "store/git/final-copy",
            target,
            b"final copy\n",
        );
        let mut locked = failed.repository().acquire_exclusive().unwrap();
        let temporary = begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileCopy(final_copy),
        );
        fs::write(&temporary, b"final copy\n").unwrap();
        assert!(!reconcile_active_operation(&mut locked, failed.path("home").as_path()).unwrap());
        assert!(fs::symlink_metadata(temporary).is_err());
        assert_eq!(locked.state().known().get_variant(&resource_id), Some(&old));

        let uncertain = TestWorkspace::new();
        let old = commit_known_link(&uncertain);
        let target = ResolvedPath::new(uncertain.path("home/.gitconfig")).unwrap();
        let final_copy = copy_desired(
            &uncertain,
            resource_id.clone(),
            "store/git/final-copy",
            target,
            b"final copy\n",
        );
        let mut locked = uncertain.repository().acquire_exclusive().unwrap();
        let temporary = begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileCopy(final_copy),
        );
        fs::write(&temporary, b"substituted temporary\n").unwrap();
        assert!(reconcile_active_operation(&mut locked, uncertain.path("home").as_path()).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(fs::read(temporary).unwrap(), b"substituted temporary\n");
        assert_eq!(locked.state().known().get_variant(&resource_id), Some(&old));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_classifies_copy_to_link_handoff_aftermath_from_recorded_effects() {
        use std::os::unix::fs::symlink;

        let succeeded = TestWorkspace::new();
        let resource_id = FullyQualifiedResourceId::parse("base/git-config").unwrap();
        let target = ResolvedPath::new(succeeded.path("home/.gitconfig")).unwrap();
        let old_copy = copy_desired(
            &succeeded,
            resource_id.clone(),
            "store/git/old-copy",
            target.clone(),
            b"old copy\n",
        );
        let old = KnownResource::FileCopy(commit_known_copy(&succeeded, old_copy));
        succeeded.write("store/git/final-link", "final link\n");
        let final_link = ResolvedFileLink::new(
            resource_id.clone(),
            ResolvedPath::new(succeeded.path("store/git/final-link")).unwrap(),
            target.clone(),
        )
        .unwrap();
        let mut locked = succeeded.repository().acquire_exclusive().unwrap();
        begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileLink(final_link.clone()),
        );
        fs::remove_file(&target).unwrap();
        symlink(final_link.source_path(), &target).unwrap();
        let action_id = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap()
            .0
            .clone();
        locked.fail_next_commit_at(CommitStage::CreateTemporary);
        assert!(matches!(
            locked.commit_succeeded(&action_id),
            Err(StateRepositoryError::Commit(CommitError::Injected {
                stage: CommitStage::CreateTemporary
            }))
        ));
        assert!(
            !reconcile_active_operation(&mut locked, succeeded.path("home").as_path()).unwrap()
        );
        assert_eq!(
            locked.state().known().get_variant(&resource_id),
            Some(&KnownResource::FileLink(
                crate::domain::known::KnownFileLink::from_resolved(&final_link)
            ))
        );

        let failed = TestWorkspace::new();
        let target = ResolvedPath::new(failed.path("home/.gitconfig")).unwrap();
        let old_copy = copy_desired(
            &failed,
            resource_id.clone(),
            "store/git/old-copy",
            target.clone(),
            b"old copy\n",
        );
        let old = KnownResource::FileCopy(commit_known_copy(&failed, old_copy));
        failed.write("store/git/final-link", "final link\n");
        let final_link = ResolvedFileLink::new(
            resource_id.clone(),
            ResolvedPath::new(failed.path("store/git/final-link")).unwrap(),
            target,
        )
        .unwrap();
        let mut locked = failed.repository().acquire_exclusive().unwrap();
        let temporary = begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileLink(final_link.clone()),
        );
        symlink(final_link.source_path(), &temporary).unwrap();
        assert!(!reconcile_active_operation(&mut locked, failed.path("home").as_path()).unwrap());
        assert!(fs::symlink_metadata(temporary).is_err());
        assert_eq!(locked.state().known().get_variant(&resource_id), Some(&old));

        let uncertain = TestWorkspace::new();
        let target = ResolvedPath::new(uncertain.path("home/.gitconfig")).unwrap();
        let old_copy = copy_desired(
            &uncertain,
            resource_id.clone(),
            "store/git/old-copy",
            target.clone(),
            b"old copy\n",
        );
        let old = KnownResource::FileCopy(commit_known_copy(&uncertain, old_copy));
        uncertain.write("store/git/final-link", "final link\n");
        let final_link = ResolvedFileLink::new(
            resource_id.clone(),
            ResolvedPath::new(uncertain.path("store/git/final-link")).unwrap(),
            target,
        )
        .unwrap();
        let mut locked = uncertain.repository().acquire_exclusive().unwrap();
        let temporary = begin_running_effect_handoff(
            &mut locked,
            old.clone(),
            ResolvedResource::FileLink(final_link),
        );
        fs::write(&temporary, b"substituted temporary\n").unwrap();
        assert!(reconcile_active_operation(&mut locked, uncertain.path("home").as_path()).unwrap());
        let (_, action) = locked
            .state()
            .active_operation()
            .unwrap()
            .actions()
            .next()
            .unwrap();
        assert_eq!(action.status(), ActionStatus::Uncertain);
        assert_eq!(fs::read(temporary).unwrap(), b"substituted temporary\n");
        assert_eq!(locked.state().known().get_variant(&resource_id), Some(&old));
    }
}
