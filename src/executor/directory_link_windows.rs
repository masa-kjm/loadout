//! Windows execution for already-recorded DirectoryLink actions.

use std::{io, path::Path};

use crate::domain::actual::TargetObservation;
use crate::domain::plan::PlannedDirectoryLinkAction;
use crate::filesystem::ExecutionTarget;
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::inspection::source::VerifiedDirectorySource;
use crate::state::operation::RecordedAction;

#[derive(Debug)]
pub(crate) enum DirectoryLinkExecutionError {
    Inspection(TargetInspectionError),
    Io(io::Error),
    InvalidRecord,
}

impl std::fmt::Display for DirectoryLinkExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inspection(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::InvalidRecord => formatter.write_str("invalid recorded directory-link action"),
        }
    }
}
impl std::error::Error for DirectoryLinkExecutionError {}

pub(crate) struct DirectoryLinkExecutor {
    inspector: FileLinkInspector,
}

impl DirectoryLinkExecutor {
    pub(crate) fn new(home: &Path) -> Result<Self, TargetInspectionError> {
        Ok(Self {
            inspector: FileLinkInspector::new(home)?,
        })
    }

    fn context(
        &self,
        path: &crate::domain::paths::ResolvedPath,
    ) -> Result<ExecutionTarget, DirectoryLinkExecutionError> {
        let physical = self
            .inspector
            .physical_target_path_for_execution(path)
            .map_err(DirectoryLinkExecutionError::Inspection)?;
        ExecutionTarget::open_with_declared_root(
            self.inspector.canonical_home(),
            self.inspector.declared_home(),
            &physical,
        )
        .map_err(DirectoryLinkExecutionError::Io)
    }

    fn expected(
        &self,
        path: &crate::domain::paths::ResolvedPath,
        link: &crate::domain::file_link::LinkTarget,
    ) -> Result<(), DirectoryLinkExecutionError> {
        if matches!(
            self.inspector
                .inspect_target_for_expected_directory_link(path, link)
                .map_err(DirectoryLinkExecutionError::Inspection)?
                .observation(),
            TargetObservation::ExpectedLink { .. }
        ) {
            Ok(())
        } else {
            Err(DirectoryLinkExecutionError::InvalidRecord)
        }
    }

    fn missing(
        &self,
        path: &crate::domain::paths::ResolvedPath,
        link: &crate::domain::file_link::LinkTarget,
    ) -> Result<(), DirectoryLinkExecutionError> {
        if matches!(
            self.inspector
                .inspect_target_for_expected_directory_link(path, link)
                .map_err(DirectoryLinkExecutionError::Inspection)?
                .observation(),
            TargetObservation::Missing
        ) {
            Ok(())
        } else {
            Err(DirectoryLinkExecutionError::InvalidRecord)
        }
    }

    pub(crate) fn preflight(
        &self,
        action: &PlannedDirectoryLinkAction,
        source: Option<&VerifiedDirectorySource>,
    ) -> Result<(), DirectoryLinkExecutionError> {
        match action {
            PlannedDirectoryLinkAction::Create { desired, .. } => {
                let source = source.ok_or(DirectoryLinkExecutionError::InvalidRecord)?;
                self.context(desired.target_path())?
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .map(|_| ())
                    .map_err(DirectoryLinkExecutionError::Io)
            }
            PlannedDirectoryLinkAction::Replace { previous, .. }
            | PlannedDirectoryLinkAction::ReplaceOwnership { previous, .. } => {
                if requires_replacement_capability(action) {
                    source
                        .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                        .reverify()
                        .map_err(|error| {
                            DirectoryLinkExecutionError::Io(io::Error::other(error))
                        })?;
                    self.context(previous.target_path())?
                        .ensure_directory_link_replacement_capability()
                        .map_err(DirectoryLinkExecutionError::Io)?;
                }
                self.expected(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::Relocate { desired, previous } => {
                let source = source.ok_or(DirectoryLinkExecutionError::InvalidRecord)?;
                let _ = self
                    .context(desired.target_path())?
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::Remove { previous } => {
                self.expected(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::ForgetMissing { previous } => {
                self.missing(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::Noop { previous, .. } => {
                source
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                    .reverify()
                    .map_err(|error| DirectoryLinkExecutionError::Io(io::Error::other(error)))?;
                self.expected(previous.target_path(), previous.link_target())
            }
        }
    }

    pub(crate) fn execute(
        &self,
        action: &PlannedDirectoryLinkAction,
        recorded: &RecordedAction,
        source: Option<&VerifiedDirectorySource>,
    ) -> Result<(), DirectoryLinkExecutionError> {
        match action {
            PlannedDirectoryLinkAction::Create { desired, .. } => {
                let source = source
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                    .reverify()
                    .map_err(|error| DirectoryLinkExecutionError::Io(io::Error::other(error)))?;
                self.context(desired.target_path())?
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|checked| checked.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())
            }
            PlannedDirectoryLinkAction::Remove { previous } => {
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforeDirectoryRemoval,
                )
                .map_err(DirectoryLinkExecutionError::Io)?;
                self.context(previous.target_path())?
                    .prepare_directory_remove(previous.link_target())
                    .and_then(|checked| checked.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.missing(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::ForgetMissing { previous } => {
                self.missing(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::Relocate { desired, previous } => {
                let source = source
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                    .reverify()
                    .map_err(|error| DirectoryLinkExecutionError::Io(io::Error::other(error)))?;
                self.context(desired.target_path())?
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|checked| checked.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())?;
                self.context(previous.target_path())?
                    .prepare_directory_remove(previous.link_target())
                    .and_then(|checked| checked.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.missing(previous.target_path(), previous.link_target())?;
                self.expected(desired.target_path(), desired.link_target())
            }
            PlannedDirectoryLinkAction::Replace { desired, previous }
            | PlannedDirectoryLinkAction::ReplaceOwnership { desired, previous } => {
                if desired.link_target() == previous.link_target() {
                    return self.expected(previous.target_path(), previous.link_target());
                }
                let source = source
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                    .reverify()
                    .map_err(|error| DirectoryLinkExecutionError::Io(io::Error::other(error)))?;
                let temporary = recorded
                    .directory_link_facts()
                    .and_then(|facts| facts.temporary_path().cloned())
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?;
                self.context(&temporary)?
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|checked| checked.attempt_temporary())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforeReplacementRenameRecheck,
                )
                .map_err(DirectoryLinkExecutionError::Io)?;
                self.context(previous.target_path())?
                    .prepare_directory_replace(
                        &temporary,
                        previous.link_target(),
                        desired.link_target(),
                        source.physical_root(),
                    )
                    .and_then(|checked| checked.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())?;
                self.missing(&temporary, desired.link_target())
            }
            PlannedDirectoryLinkAction::Noop { .. } => {
                unreachable!("report-only action cannot be recorded for execution")
            }
        }
    }
}

fn requires_replacement_capability(action: &PlannedDirectoryLinkAction) -> bool {
    match action {
        PlannedDirectoryLinkAction::Replace { .. } => true,
        PlannedDirectoryLinkAction::ReplaceOwnership { desired, previous } => {
            desired.link_target() != previous.link_target()
        }
        PlannedDirectoryLinkAction::Create { .. }
        | PlannedDirectoryLinkAction::Relocate { .. }
        | PlannedDirectoryLinkAction::Remove { .. }
        | PlannedDirectoryLinkAction::ForgetMissing { .. }
        | PlannedDirectoryLinkAction::Noop { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::DirectoryLinkExecutor;
    use super::requires_replacement_capability;
    use crate::domain::directory_link::ResolvedDirectoryLink;
    use crate::domain::file_link::LinkTarget;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::KnownDirectoryLink;
    use crate::domain::paths::ResolvedPath;
    use crate::domain::plan::PlannedDirectoryLinkAction;
    use crate::state::operation::RecordedAction;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Workspace {
        root: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-directory-link-windows-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(root.join("home/.config")).unwrap();
            fs::create_dir_all(root.join("store/source")).unwrap();
            Self { root }
        }

        fn path(&self, value: &str) -> ResolvedPath {
            ResolvedPath::new(self.root.join(value)).unwrap()
        }

        fn desired(&self, id: &str) -> ResolvedDirectoryLink {
            ResolvedDirectoryLink::new(
                FullyQualifiedResourceId::parse(id).unwrap(),
                self.path("store/source"),
                self.path("home/.config/link"),
            )
            .unwrap()
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn desired(resource_id: &str, source: &str) -> ResolvedDirectoryLink {
        ResolvedDirectoryLink::new(
            FullyQualifiedResourceId::parse(resource_id).unwrap(),
            ResolvedPath::new(format!("C:\\loadout-test\\{source}")).unwrap(),
            ResolvedPath::new("C:\\loadout-test\\target").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn state_only_ownership_handoff_does_not_require_replacement_capability() {
        let previous_desired = desired("base/old", "source");
        let previous = KnownDirectoryLink::new(
            previous_desired.resource_id().clone(),
            previous_desired.source_path().clone(),
            previous_desired.target_path().clone(),
            LinkTarget::new(previous_desired.source_path().clone()),
        )
        .unwrap();
        let state_only = PlannedDirectoryLinkAction::ReplaceOwnership {
            desired: desired("base/new", "source"),
            previous: previous.clone(),
        };
        let source_changing = PlannedDirectoryLinkAction::ReplaceOwnership {
            desired: desired("base/new", "replacement"),
            previous,
        };

        assert!(!requires_replacement_capability(&state_only));
        assert!(requires_replacement_capability(&source_changing));
    }

    #[test]
    fn state_only_ownership_handoff_does_not_require_a_verified_source() {
        let workspace = Workspace::new();
        let previous_desired = workspace.desired("base/old");
        std::os::windows::fs::symlink_dir(
            previous_desired.link_target().as_path(),
            previous_desired.target_path().as_path(),
        )
        .unwrap();
        let previous = KnownDirectoryLink::from_resolved(&previous_desired);
        let action = PlannedDirectoryLinkAction::ReplaceOwnership {
            desired: workspace.desired("base/new"),
            previous,
        };
        let record = RecordedAction::directory_link(&action, None).unwrap();
        let executor = DirectoryLinkExecutor::new(workspace.path("home").as_ref()).unwrap();

        executor.preflight(&action, None).unwrap();
        executor.execute(&action, &record, None).unwrap();

        assert_eq!(
            fs::read_link(workspace.path("home/.config/link").as_ref()).unwrap(),
            workspace.path("store/source").as_ref(),
        );
    }
}
