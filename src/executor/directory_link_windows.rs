//! DirectoryLink execution remains unavailable on Windows until the dedicated backend is enabled.

use std::path::Path;

use crate::domain::plan::PlannedDirectoryLinkAction;
use crate::inspection::file_link::TargetInspectionError;
use crate::inspection::source::VerifiedDirectorySource;
use crate::state::operation::RecordedAction;

#[derive(Debug)]
pub(crate) enum DirectoryLinkExecutionError {
    Unsupported,
}

impl std::fmt::Display for DirectoryLinkExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DirectoryLink execution is not enabled on Windows")
    }
}

impl std::error::Error for DirectoryLinkExecutionError {}

pub(crate) struct DirectoryLinkExecutor;

impl DirectoryLinkExecutor {
    pub(crate) fn new(_: &Path) -> Result<Self, TargetInspectionError> {
        Ok(Self)
    }

    pub(crate) fn preflight(
        &self,
        _: &PlannedDirectoryLinkAction,
        _: Option<&VerifiedDirectorySource>,
    ) -> Result<(), DirectoryLinkExecutionError> {
        Err(DirectoryLinkExecutionError::Unsupported)
    }

    pub(crate) fn execute(
        &self,
        _: &PlannedDirectoryLinkAction,
        _: &RecordedAction,
        _: Option<&VerifiedDirectorySource>,
    ) -> Result<(), DirectoryLinkExecutionError> {
        Err(DirectoryLinkExecutionError::Unsupported)
    }
}
