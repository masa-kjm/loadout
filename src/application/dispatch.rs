//! Dispatch exactly one planner-selected action across the resource boundary.

use super::apply::{ApplyError, StaleLinkExecutionError};
use crate::domain::desired::ResolvedResource;
use crate::domain::ids::FullyQualifiedResourceId;
use crate::domain::known::KnownResource;
use crate::domain::plan::{ActionKind, PlannedResourceAction};
use crate::executor::file_copy::{
    CopyPreflightError, CopyToLinkHandoffExecutionError, CreateCopyExecutionError,
    FileCopyExecutor, LinkToCopyHandoffExecutionError, RelocateCopyExecutionError,
    RemoveCopyExecutionError, ReplaceCopyExecutionError,
};
use crate::executor::file_link::{
    CreateLinkExecutionError, FileLinkExecutor, RelocateLinkExecutionError,
    ReplaceLinkExecutionError,
};
use crate::inspection::source::VerifiedSource;
use crate::resolver::ResolvedApplyInput;
use crate::state::operation::RecordedAction;

#[derive(Debug)]
pub(crate) enum ResourceExecutionError {
    CreateLink(CreateLinkExecutionError),
    ReplaceLink(ReplaceLinkExecutionError),
    RelocateLink(RelocateLinkExecutionError),
    StaleLink(StaleLinkExecutionError),
    CreateCopy(CreateCopyExecutionError),
    ReplaceCopy(ReplaceCopyExecutionError),
    RelocateCopy(RelocateCopyExecutionError),
    RemoveCopy(RemoveCopyExecutionError),
    CopyStateOnly(CopyPreflightError),
    LinkToCopyHandoff(LinkToCopyHandoffExecutionError),
    CopyToLinkHandoff(CopyToLinkHandoffExecutionError),
    InvalidEffectHandoff,
}

impl std::fmt::Display for ResourceExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateLink(error) => error.fmt(formatter),
            Self::ReplaceLink(error) => error.fmt(formatter),
            Self::RelocateLink(error) => error.fmt(formatter),
            Self::StaleLink(error) => error.fmt(formatter),
            Self::CreateCopy(error) => error.fmt(formatter),
            Self::ReplaceCopy(error) => error.fmt(formatter),
            Self::RelocateCopy(error) => error.fmt(formatter),
            Self::RemoveCopy(error) => error.fmt(formatter),
            Self::CopyStateOnly(error) => error.fmt(formatter),
            Self::LinkToCopyHandoff(error) => error.fmt(formatter),
            Self::CopyToLinkHandoff(error) => error.fmt(formatter),
            Self::InvalidEffectHandoff => {
                formatter.write_str("effect handoff does not have a supported link/copy pair")
            }
        }
    }
}

impl std::error::Error for ResourceExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateLink(error) => Some(error),
            Self::ReplaceLink(error) => Some(error),
            Self::RelocateLink(error) => Some(error),
            Self::StaleLink(error) => Some(error),
            Self::CreateCopy(error) => Some(error),
            Self::ReplaceCopy(error) => Some(error),
            Self::RelocateCopy(error) => Some(error),
            Self::RemoveCopy(error) => Some(error),
            Self::CopyStateOnly(error) => Some(error),
            Self::LinkToCopyHandoff(error) => Some(error),
            Self::CopyToLinkHandoff(error) => Some(error),
            Self::InvalidEffectHandoff => None,
        }
    }
}

fn source<'a>(
    resolved: &'a ResolvedApplyInput,
    resource_id: &FullyQualifiedResourceId,
) -> Result<&'a VerifiedSource, ApplyError> {
    resolved
        .verified_sources()
        .get(resource_id)
        .ok_or_else(|| ApplyError::MissingVerifiedSource {
            resource_id: resource_id.clone(),
        })
}

pub(super) fn preflight(
    link_executor: &FileLinkExecutor,
    copy_executor: &FileCopyExecutor,
    resource_action: &PlannedResourceAction,
    resolved: &ResolvedApplyInput,
) -> Result<(), ApplyError> {
    match resource_action {
        PlannedResourceAction::FileCopy(_) | PlannedResourceAction::ReplaceEffect(_) => {
            copy_executor
                .preflight(
                    resource_action,
                    resolved
                        .verified_sources()
                        .get(resource_action.resource_id()),
                )
                .map_err(ApplyError::CopyPreflight)
        }
        PlannedResourceAction::FileLink(action) => match action.kind() {
            ActionKind::CreateLink => link_executor
                .preflight_create(action, source(resolved, action.resource_id())?)
                .map_err(ApplyError::Preflight),
            ActionKind::ReplaceLink => link_executor
                .preflight_replace(action, source(resolved, action.resource_id())?)
                .map_err(ApplyError::ReplacePreflight),
            ActionKind::ReplaceOwnership if action.preconditions() == action.postconditions() => {
                link_executor
                    .preflight_same_source_ownership_handoff(
                        action,
                        source(resolved, action.resource_id())?,
                    )
                    .map_err(ApplyError::ReplacePreflight)
            }
            ActionKind::ReplaceOwnership => link_executor
                .preflight_replace(action, source(resolved, action.resource_id())?)
                .map_err(ApplyError::ReplacePreflight),
            ActionKind::RelocateLink => link_executor
                .preflight_relocate(action, source(resolved, action.resource_id())?)
                .map_err(ApplyError::RelocatePreflight),
            ActionKind::RemoveLink => link_executor.preflight_remove(action).map_err(|error| {
                ApplyError::StalePreflight(StaleLinkExecutionError::Remove(error))
            }),
            ActionKind::ForgetMissing => {
                link_executor
                    .preflight_forget_missing(action)
                    .map_err(|error| {
                        ApplyError::StalePreflight(StaleLinkExecutionError::ForgetMissing(error))
                    })
            }
            ActionKind::Noop => link_executor
                .preflight_noop(action, source(resolved, action.resource_id())?)
                .map_err(ApplyError::ReplacePreflight),
            ActionKind::CreateCopy
            | ActionKind::ReplaceCopy
            | ActionKind::RelocateCopy
            | ActionKind::RemoveCopy
            | ActionKind::ReplaceEffect => {
                Err(ApplyError::CopyPreflight(CopyPreflightError::WrongAction))
            }
        },
    }
}

pub(super) fn execute(
    link_executor: &FileLinkExecutor,
    copy_executor: &FileCopyExecutor,
    resource_action: &PlannedResourceAction,
    recorded: &RecordedAction,
    resolved: &ResolvedApplyInput,
) -> Result<(), ResourceExecutionError> {
    match resource_action {
        PlannedResourceAction::FileLink(action) => match action.kind() {
            ActionKind::CreateLink => link_executor
                .execute_create(
                    action,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::CreateLink),
            ActionKind::ReplaceLink => link_executor
                .execute_replace(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::ReplaceLink),
            ActionKind::ReplaceOwnership if action.preconditions() == action.postconditions() => {
                link_executor
                    .execute_same_source_ownership_handoff(
                        action,
                        recorded,
                        source(resolved, action.resource_id())
                            .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                    )
                    .map_err(ResourceExecutionError::ReplaceLink)
            }
            ActionKind::ReplaceOwnership => link_executor
                .execute_replace(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::ReplaceLink),
            ActionKind::RelocateLink => link_executor
                .execute_relocate(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::RelocateLink),
            ActionKind::RemoveLink => link_executor.execute_remove(action).map_err(|error| {
                ResourceExecutionError::StaleLink(StaleLinkExecutionError::Remove(error))
            }),
            ActionKind::ForgetMissing => {
                link_executor
                    .execute_forget_missing(action)
                    .map_err(|error| {
                        ResourceExecutionError::StaleLink(StaleLinkExecutionError::ForgetMissing(
                            error,
                        ))
                    })
            }
            ActionKind::Noop => unreachable!("report-only action cannot be recorded for execution"),
            _ => unreachable!("file-link payload cannot carry a copy action"),
        },
        PlannedResourceAction::FileCopy(action) => match action.kind() {
            ActionKind::CreateCopy => copy_executor
                .execute_create(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::CreateCopy),
            ActionKind::ReplaceCopy => copy_executor
                .execute_replace(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::ReplaceCopy),
            ActionKind::RelocateCopy => copy_executor
                .execute_relocate(
                    action,
                    recorded,
                    source(resolved, action.resource_id())
                        .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                )
                .map_err(ResourceExecutionError::RelocateCopy),
            ActionKind::RemoveCopy => copy_executor
                .execute_remove(action)
                .map_err(ResourceExecutionError::RemoveCopy),
            ActionKind::ForgetMissing => copy_executor
                .preflight(resource_action, None)
                .map_err(ResourceExecutionError::CopyStateOnly),
            ActionKind::Noop => unreachable!("report-only action cannot be recorded for execution"),
            _ => unreachable!("file-copy payload cannot carry a link action"),
        },
        PlannedResourceAction::ReplaceEffect(action) => {
            match (action.old_effect(), action.final_effect()) {
                (KnownResource::FileLink(_), ResolvedResource::FileCopy(_)) => copy_executor
                    .execute_link_to_copy_handoff(
                        action,
                        recorded,
                        source(resolved, action.resource_id())
                            .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                    )
                    .map_err(ResourceExecutionError::LinkToCopyHandoff),
                (KnownResource::FileCopy(_), ResolvedResource::FileLink(_)) => copy_executor
                    .execute_copy_to_link_handoff(
                        action,
                        recorded,
                        source(resolved, action.resource_id())
                            .map_err(|_| ResourceExecutionError::InvalidEffectHandoff)?,
                    )
                    .map_err(ResourceExecutionError::CopyToLinkHandoff),
                _ => Err(ResourceExecutionError::InvalidEffectHandoff),
            }
        }
    }
}
