//! Dispatch exactly one planner-selected action across the resource boundary.

use super::apply::{ApplyError, StaleLinkExecutionError};
use crate::domain::plan::{ActionKind, PlannedAction};
use crate::executor::file_link::FileLinkExecutor;
use crate::inspection::source::VerifiedSource;
use crate::resolver::ResolvedApplyInput;
use crate::state::operation::RecordedAction;

fn source<'a>(
    resolved: &'a ResolvedApplyInput,
    action: &PlannedAction,
) -> Result<&'a VerifiedSource, ApplyError> {
    resolved
        .verified_sources()
        .get(action.resource_id())
        .ok_or_else(|| ApplyError::MissingVerifiedSource {
            resource_id: action.resource_id().clone(),
        })
}

pub(super) fn preflight(
    executor: &FileLinkExecutor,
    action: &PlannedAction,
    resolved: &ResolvedApplyInput,
) -> Result<(), ApplyError> {
    match action.kind() {
        ActionKind::CreateLink => executor
            .preflight_create(action, source(resolved, action)?)
            .map_err(ApplyError::Preflight),
        ActionKind::ReplaceLink => executor
            .preflight_replace(action, source(resolved, action)?)
            .map_err(ApplyError::ReplacePreflight),
        ActionKind::ReplaceOwnership if action.preconditions() == action.postconditions() => {
            executor
                .preflight_same_source_ownership_handoff(action, source(resolved, action)?)
                .map_err(ApplyError::ReplacePreflight)
        }
        ActionKind::ReplaceOwnership => executor
            .preflight_replace(action, source(resolved, action)?)
            .map_err(ApplyError::ReplacePreflight),
        ActionKind::RelocateLink => executor
            .preflight_relocate(action, source(resolved, action)?)
            .map_err(ApplyError::RelocatePreflight),
        ActionKind::RemoveLink => executor
            .preflight_remove(action)
            .map_err(|error| ApplyError::StalePreflight(StaleLinkExecutionError::Remove(error))),
        ActionKind::ForgetMissing => executor.preflight_forget_missing(action).map_err(|error| {
            ApplyError::StalePreflight(StaleLinkExecutionError::ForgetMissing(error))
        }),
        ActionKind::Noop => executor
            .preflight_noop(action, source(resolved, action)?)
            .map_err(ApplyError::ReplacePreflight),
        kind @ (ActionKind::CreateCopy
        | ActionKind::ReplaceCopy
        | ActionKind::RelocateCopy
        | ActionKind::RemoveCopy
        | ActionKind::ReplaceEffect) => Err(ApplyError::StalePreflight(
            StaleLinkExecutionError::UnsupportedAction { kind },
        )),
    }
}

pub(super) fn execute(
    executor: &FileLinkExecutor,
    action: &PlannedAction,
    recorded: &RecordedAction,
    resolved: &ResolvedApplyInput,
) -> Result<(), ApplyError> {
    match action.kind() {
        ActionKind::CreateLink => executor
            .execute_create(action, source(resolved, action)?)
            .map_err(ApplyError::Preflight),
        ActionKind::ReplaceLink => executor
            .execute_replace(action, recorded, source(resolved, action)?)
            .map_err(ApplyError::ReplacePreflight),
        ActionKind::ReplaceOwnership if action.preconditions() == action.postconditions() => {
            executor
                .execute_same_source_ownership_handoff(action, recorded, source(resolved, action)?)
                .map_err(ApplyError::ReplacePreflight)
        }
        ActionKind::ReplaceOwnership => executor
            .execute_replace(action, recorded, source(resolved, action)?)
            .map_err(ApplyError::ReplacePreflight),
        ActionKind::RelocateLink => executor
            .execute_relocate(action, recorded, source(resolved, action)?)
            .map_err(ApplyError::RelocatePreflight),
        ActionKind::RemoveLink => executor
            .execute_remove(action)
            .map_err(|error| ApplyError::StalePreflight(StaleLinkExecutionError::Remove(error))),
        ActionKind::ForgetMissing => executor.execute_forget_missing(action).map_err(|error| {
            ApplyError::StalePreflight(StaleLinkExecutionError::ForgetMissing(error))
        }),
        ActionKind::Noop => unreachable!("report-only action cannot be recorded for execution"),
        kind @ (ActionKind::CreateCopy
        | ActionKind::ReplaceCopy
        | ActionKind::RelocateCopy
        | ActionKind::RemoveCopy
        | ActionKind::ReplaceEffect) => Err(ApplyError::StalePreflight(
            StaleLinkExecutionError::UnsupportedAction { kind },
        )),
    }
}
