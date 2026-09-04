//! Non-dry-run coordination for Slice 4's single `create_link` path.

use std::fmt;

use crate::domain::actual::TargetObservation;
use crate::domain::hashes::{CanonicalHashError, desired_hash};
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, Plan, PlannedAction};
use crate::executor::file_link::{
    CreateLinkExecutionError, FileLinkExecutor, ForgetMissingExecutionError,
    RemoveLinkExecutionError,
};
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::planner::file_link::plan;
use crate::resolver::ResolvedApplyInput;
use crate::state::operation::ActionStatus;
use crate::state::repository::{StateRepository, StateRepositoryError};

/// Coordinates a confirmed non-dry-run Slice 4 apply for one home and state directory.
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

    fn apply_stale_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<StaleLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut crate::state::repository::LockedStateRepository),
    {
        let desired = resolved.desired();
        let mut locked = self
            .state_repository
            .acquire_exclusive()
            .map_err(ApplyError::State)?;
        if locked.state().active_operation().is_some() {
            return Err(ApplyError::RecoveryRequired);
        }

        let inspector = FileLinkInspector::new(self.home_directory.as_ref())
            .map_err(ApplyError::InitialInspection)?;
        let actual = inspector
            .inspect(desired, locked.state().known())
            .map_err(ApplyError::InitialInspection)?;
        let plan = plan(desired, locked.state().known(), &actual);
        if !plan.is_executable() {
            return Ok(StaleLinkApplyResult::Blocked { plan });
        }

        let action = require_single_stale_action(&plan)?.clone();
        let executor = FileLinkExecutor::new(self.home_directory.as_ref())
            .map_err(ApplyError::InitialInspection)?;
        preflight_stale_action(&executor, &action).map_err(ApplyError::StalePreflight)?;
        locked
            .preflight_writable()
            .map_err(ApplyError::StatePreflight)?;
        let desired_hash = desired_hash(desired).map_err(ApplyError::DesiredHash)?;
        if !confirm(&plan) {
            return Ok(StaleLinkApplyResult::Declined { plan });
        }

        let action_id = locked
            .begin_operation(desired_hash, &action)
            .map_err(ApplyError::State)?;
        locked.mark_running(&action_id).map_err(ApplyError::State)?;
        after_running(&mut locked);

        match action.kind() {
            ActionKind::RemoveLink => match executor.execute_remove(&action) {
                Ok(()) => commit_stale_success(&mut locked, &action_id, &action),
                Err(error) => match classify_remove_execution_error(&error) {
                    StaleExecutionClassification::Succeeded => {
                        commit_stale_success(&mut locked, &action_id, &action)
                    }
                    StaleExecutionClassification::Failed => close_stale_failure(
                        &mut locked,
                        &action_id,
                        StaleLinkExecutionError::Remove(error),
                    ),
                    StaleExecutionClassification::Uncertain => {
                        locked
                            .mark_without_known(&action_id, ActionStatus::Uncertain)
                            .map_err(ApplyError::State)?;
                        Ok(StaleLinkApplyResult::Uncertain {
                            error: StaleLinkExecutionError::Remove(error),
                        })
                    }
                },
            },
            ActionKind::ForgetMissing => match executor.execute_forget_missing(&action) {
                Ok(()) => commit_stale_success(&mut locked, &action_id, &action),
                Err(error) => match classify_forget_missing_execution_error(&error) {
                    StaleExecutionClassification::Failed => close_stale_failure(
                        &mut locked,
                        &action_id,
                        StaleLinkExecutionError::ForgetMissing(error),
                    ),
                    StaleExecutionClassification::Uncertain => {
                        locked
                            .mark_without_known(&action_id, ActionStatus::Uncertain)
                            .map_err(ApplyError::State)?;
                        Ok(StaleLinkApplyResult::Uncertain {
                            error: StaleLinkExecutionError::ForgetMissing(error),
                        })
                    }
                    StaleExecutionClassification::Succeeded => unreachable!(
                        "a state-only stale action reports success directly from its missing recheck"
                    ),
                },
            },
            kind => Err(ApplyError::SliceFiveRequiresSingleStaleAction {
                action_kinds: vec![kind],
            }),
        }
    }

    fn apply_with_hooks<F, H>(
        &self,
        resolved: &ResolvedApplyInput,
        confirm: F,
        after_running: H,
    ) -> Result<CreateLinkApplyResult, ApplyError>
    where
        F: FnOnce(&Plan) -> bool,
        H: FnOnce(&mut crate::state::repository::LockedStateRepository),
    {
        let desired = resolved.desired();
        let verified_sources = resolved.verified_sources();

        // Locking precedes every state-for-execution read and target inspection.
        let mut locked = self
            .state_repository
            .acquire_exclusive()
            .map_err(ApplyError::State)?;
        if locked.state().active_operation().is_some() {
            // Slice 7 will reconcile these records from their recorded facts.
            // Until then, continuing could create a fresh plan over an unknown mutation result, so leave the record untouched and block safely.
            return Err(ApplyError::RecoveryRequired);
        }

        let inspector = FileLinkInspector::new(self.home_directory.as_ref())
            .map_err(ApplyError::InitialInspection)?;
        let actual = inspector
            .inspect(desired, locked.state().known())
            .map_err(ApplyError::InitialInspection)?;
        let plan = plan(desired, locked.state().known(), &actual);
        if !plan.is_executable() {
            return Ok(CreateLinkApplyResult::Blocked { plan });
        }

        let action = require_single_create_action(&plan)?.clone();
        let source = verified_sources.get(action.resource_id()).ok_or_else(|| {
            ApplyError::MissingVerifiedSource {
                resource_id: action.resource_id().clone(),
            }
        })?;
        let executor = FileLinkExecutor::new(self.home_directory.as_ref())
            .map_err(ApplyError::InitialInspection)?;
        #[cfg(test)]
        let executor = if self.force_capability_failure {
            executor.with_forced_capability_failure_for_test()
        } else {
            executor
        };

        // Preflight remains non-mutating. The executor repeats this immediately before mutation, so this cannot authorize a skipped recheck.
        executor
            .preflight_create(&action, source)
            .map_err(ApplyError::Preflight)?;
        locked
            .preflight_writable()
            .map_err(ApplyError::StatePreflight)?;
        let desired_hash = desired_hash(desired).map_err(ApplyError::DesiredHash)?;
        if !confirm(&plan) {
            return Ok(CreateLinkApplyResult::Declined { plan });
        }

        let action_id = locked
            .begin_create_operation(desired_hash, &action)
            .map_err(ApplyError::State)?;
        locked.mark_running(&action_id).map_err(ApplyError::State)?;
        after_running(&mut locked);

        match executor.execute_create(&action, source) {
            Ok(()) => {
                locked
                    .commit_create_succeeded(&action_id)
                    .map_err(ApplyError::State)?;
                locked
                    .close_finished_operation()
                    .map_err(ApplyError::State)?;
                Ok(CreateLinkApplyResult::Applied {
                    resource_id: action.resource_id().clone(),
                })
            }
            Err(error) => match classify_execution_error(&error) {
                ExecutionClassification::Succeeded => {
                    // A no-follow postcondition can prove success even when an operating-system create call reported an error.
                    locked
                        .commit_create_succeeded(&action_id)
                        .map_err(ApplyError::State)?;
                    locked
                        .close_finished_operation()
                        .map_err(ApplyError::State)?;
                    Ok(CreateLinkApplyResult::Applied {
                        resource_id: action.resource_id().clone(),
                    })
                }
                ExecutionClassification::Failed => {
                    locked
                        .mark_without_known(&action_id, ActionStatus::Failed)
                        .map_err(ApplyError::State)?;
                    locked
                        .close_finished_operation()
                        .map_err(ApplyError::State)?;
                    Ok(CreateLinkApplyResult::Failed { error })
                }
                ExecutionClassification::Uncertain => {
                    locked
                        .mark_without_known(&action_id, ActionStatus::Uncertain)
                        .map_err(ApplyError::State)?;
                    Ok(CreateLinkApplyResult::Uncertain { error })
                }
            },
        }
    }

    #[cfg(test)]
    fn apply_create_link_with_after_running<H>(
        &self,
        resolved: &ResolvedApplyInput,
        after_running: H,
    ) -> Result<CreateLinkApplyResult, ApplyError>
    where
        H: FnOnce(&mut crate::state::repository::LockedStateRepository),
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
        H: FnOnce(&mut crate::state::repository::LockedStateRepository),
    {
        self.apply_stale_with_hooks(resolved, |_| true, after_running)
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

fn commit_stale_success(
    locked: &mut crate::state::repository::LockedStateRepository,
    action_id: &crate::state::operation::ActionId,
    action: &PlannedAction,
) -> Result<StaleLinkApplyResult, ApplyError> {
    locked
        .commit_succeeded(action_id)
        .map_err(ApplyError::State)?;
    locked
        .close_finished_operation()
        .map_err(ApplyError::State)?;
    Ok(StaleLinkApplyResult::Applied {
        resource_id: action.resource_id().clone(),
        kind: action.kind(),
    })
}

fn close_stale_failure(
    locked: &mut crate::state::repository::LockedStateRepository,
    action_id: &crate::state::operation::ActionId,
    error: StaleLinkExecutionError,
) -> Result<StaleLinkApplyResult, ApplyError> {
    locked
        .mark_without_known(action_id, ActionStatus::Failed)
        .map_err(ApplyError::State)?;
    locked
        .close_finished_operation()
        .map_err(ApplyError::State)?;
    Ok(StaleLinkApplyResult::Failed { error })
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
        // A failed immediate recheck did not attempt the action. It must not
        // turn an externally created matching link into managed state.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StaleExecutionClassification {
    Succeeded,
    Failed,
    Uncertain,
}

fn classify_remove_execution_error(
    error: &RemoveLinkExecutionError,
) -> StaleExecutionClassification {
    match error {
        RemoveLinkExecutionError::RemoveAttemptFailed { aftermath, .. }
        | RemoveLinkExecutionError::PostconditionNotMet {
            observation: aftermath,
            ..
        } => match aftermath {
            TargetObservation::Missing => StaleExecutionClassification::Succeeded,
            TargetObservation::ExpectedLink { .. } => StaleExecutionClassification::Failed,
            TargetObservation::MatchingUnmanagedLink { .. }
            | TargetObservation::OtherLink { .. }
            | TargetObservation::OtherEntry { .. }
            | TargetObservation::UnsafePath { .. } => StaleExecutionClassification::Uncertain,
        },
        // The executor did not attempt a removal after these errors. Keep the historical fact unchanged and let a later fresh plan classify any externally changed target instead of adopting that new state here.
        RemoveLinkExecutionError::PreconditionNoLongerHolds { .. } => {
            StaleExecutionClassification::Failed
        }
        RemoveLinkExecutionError::UnsupportedAction { .. }
        | RemoveLinkExecutionError::InvalidRemoveConditions
        | RemoveLinkExecutionError::TargetInspection(_)
        | RemoveLinkExecutionError::PlatformCapability { .. }
        | RemoveLinkExecutionError::RemoveAftermathUnproven { .. }
        | RemoveLinkExecutionError::PostconditionInspection(_) => {
            StaleExecutionClassification::Uncertain
        }
    }
}

fn classify_forget_missing_execution_error(
    error: &ForgetMissingExecutionError,
) -> StaleExecutionClassification {
    match error {
        // A target appeared after planning. This action never mutated it and must retain Known state; a future fresh plan will report its current conflict rather than deleting it.
        ForgetMissingExecutionError::PostconditionNotMet { .. } => {
            StaleExecutionClassification::Failed
        }
        ForgetMissingExecutionError::UnsupportedAction { .. }
        | ForgetMissingExecutionError::InvalidForgetMissingConditions
        | ForgetMissingExecutionError::TargetInspection(_) => {
            StaleExecutionClassification::Uncertain
        }
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
            Self::StalePreflight(error) => Some(error),
            Self::DesiredHash(error) => Some(error),
            Self::RecoveryRequired
            | Self::MissingVerifiedSource { .. }
            | Self::SliceFourRequiresSingleCreateAction { .. }
            | Self::SliceFiveRequiresSingleStaleAction { .. } => None,
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
