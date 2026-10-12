//! Unix execution for already-recorded DirectoryLink actions.

use crate::domain::actual::TargetObservation;
use crate::domain::plan::PlannedDirectoryLinkAction;
use crate::filesystem::ExecutionTarget;
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::inspection::source::VerifiedDirectorySource;
use crate::state::operation::RecordedAction;
use std::{io, path::Path};

#[derive(Debug)]
pub(crate) enum DirectoryLinkExecutionError {
    Inspection(TargetInspectionError),
    Io(io::Error),
    InvalidRecord,
}
impl std::fmt::Display for DirectoryLinkExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inspection(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
            Self::InvalidRecord => f.write_str("invalid recorded directory-link action"),
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
            PlannedDirectoryLinkAction::Replace {
                desired: _,
                previous,
            }
            | PlannedDirectoryLinkAction::ReplaceOwnership {
                desired: _,
                previous,
            } => {
                let source = source.ok_or(DirectoryLinkExecutionError::InvalidRecord)?;
                source
                    .reverify()
                    .map_err(|e| DirectoryLinkExecutionError::Io(io::Error::other(e)))?;
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
                if matches!(
                    self.inspector
                        .inspect_target_for_expected_directory_link(
                            previous.target_path(),
                            previous.link_target()
                        )
                        .map_err(DirectoryLinkExecutionError::Inspection)?
                        .observation(),
                    TargetObservation::Missing
                ) {
                    Ok(())
                } else {
                    Err(DirectoryLinkExecutionError::InvalidRecord)
                }
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
                    .map_err(|e| DirectoryLinkExecutionError::Io(io::Error::other(e)))?;
                let context = self.context(desired.target_path())?;
                context
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|c| c.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())
            }
            PlannedDirectoryLinkAction::Remove { previous } => {
                let context = self.context(previous.target_path())?;
                context
                    .prepare_remove(previous.link_target())
                    .and_then(|c| c.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                if matches!(
                    self.inspector
                        .inspect_target_for_expected_directory_link(
                            previous.target_path(),
                            previous.link_target()
                        )
                        .map_err(DirectoryLinkExecutionError::Inspection)?
                        .observation(),
                    TargetObservation::Missing
                ) {
                    Ok(())
                } else {
                    Err(DirectoryLinkExecutionError::InvalidRecord)
                }
            }
            PlannedDirectoryLinkAction::ForgetMissing { previous } => {
                self.missing(previous.target_path(), previous.link_target())
            }
            PlannedDirectoryLinkAction::Relocate { desired, previous } => {
                let source = source
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?
                    .reverify()
                    .map_err(|e| DirectoryLinkExecutionError::Io(io::Error::other(e)))?;
                let new = self.context(desired.target_path())?;
                new.prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|c| c.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())?;
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforeRelocateRemovalRecheck,
                )
                .map_err(DirectoryLinkExecutionError::Io)?;
                let old = self.context(previous.target_path())?;
                old.prepare_remove(previous.link_target())
                    .and_then(|c| c.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforePostObservation,
                )
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
                    .map_err(|e| DirectoryLinkExecutionError::Io(io::Error::other(e)))?;
                let temporary = recorded
                    .directory_link_facts()
                    .and_then(|f| f.temporary_path().cloned())
                    .ok_or(DirectoryLinkExecutionError::InvalidRecord)?;
                let temporary_context = self.context(&temporary)?;
                temporary_context
                    .prepare_directory_create(source.physical_root(), desired.link_target())
                    .and_then(|c| c.attempt_temporary())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforeReplacementRenameRecheck,
                )
                .map_err(DirectoryLinkExecutionError::Io)?;
                let context = self.context(previous.target_path())?;
                context
                    .prepare_directory_replace(
                        &temporary,
                        previous.link_target(),
                        desired.link_target(),
                        source.physical_root(),
                    )
                    .and_then(|c| c.attempt())
                    .map_err(DirectoryLinkExecutionError::Io)?;
                #[cfg(test)]
                crate::test_support::execution_boundary(
                    crate::test_support::ExecutionBoundary::BeforePostObservation,
                )
                .map_err(DirectoryLinkExecutionError::Io)?;
                self.expected(desired.target_path(), desired.link_target())?;
                self.missing(&temporary, desired.link_target())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::directory_link::ResolvedDirectoryLink;
    use crate::domain::ids::FullyQualifiedResourceId;
    use crate::domain::known::KnownDirectoryLink;
    use crate::domain::paths::{ResolvedPath, SourceRelativePath};
    use crate::inspection::source::{resolve_store_root, verify_directory_source_for_execution};
    use crate::state::operation::ActionStatus;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct Workspace {
        root: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-directory-link-executor-{}-{}",
                std::process::id(),
                NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir_all(root.join("home/.config")).unwrap();
            fs::create_dir_all(root.join("store/old")).unwrap();
            fs::create_dir_all(root.join("store/new")).unwrap();
            Self { root }
        }

        fn path(&self, value: &str) -> ResolvedPath {
            ResolvedPath::new(self.root.join(value)).unwrap()
        }

        fn source(&self, value: &str) -> VerifiedDirectorySource {
            let root = resolve_store_root(self.root.join("store").as_path()).unwrap();
            verify_directory_source_for_execution(&root, &SourceRelativePath::parse(value).unwrap())
                .unwrap()
        }

        fn desired(&self, id: &str, source: &str, target: &str) -> ResolvedDirectoryLink {
            ResolvedDirectoryLink::new(
                FullyQualifiedResourceId::parse(id).unwrap(),
                self.path(source),
                self.path(target),
            )
            .unwrap()
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn create_remove_and_same_source_handoff_preserve_directory_referents() {
        let workspace = Workspace::new();
        let desired = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        let create = PlannedDirectoryLinkAction::Create {
            desired: desired.clone(),
            superseded: None,
        };
        let record = RecordedAction::directory_link(&create, None).unwrap();
        let executor = DirectoryLinkExecutor::new(workspace.path("home").as_ref()).unwrap();
        executor
            .execute(&create, &record, Some(&workspace.source("old")))
            .unwrap();
        assert_eq!(
            fs::read_link(workspace.path("home/.config/nvim").as_ref()).unwrap(),
            workspace.path("store/old").as_ref()
        );

        let known = KnownDirectoryLink::from_resolved(&desired);
        let handoff_desired = workspace.desired("base/nvim-next", "store/old", "home/.config/nvim");
        let handoff = PlannedDirectoryLinkAction::ReplaceOwnership {
            desired: handoff_desired,
            previous: known.clone(),
        };
        let handoff_record = RecordedAction::directory_link(&handoff, None).unwrap();
        executor.execute(&handoff, &handoff_record, None).unwrap();

        let remove = PlannedDirectoryLinkAction::Remove {
            previous: KnownDirectoryLink::from_resolved(&desired),
        };
        let remove_record = RecordedAction::directory_link(&remove, None).unwrap();
        executor.execute(&remove, &remove_record, None).unwrap();
        assert!(fs::symlink_metadata(workspace.path("home/.config/nvim").as_ref()).is_err());
        assert!(workspace.path("store/old").as_ref().is_dir());
    }

    #[test]
    fn relocate_verifies_old_missing_and_new_expected() {
        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim-next");
        let action = PlannedDirectoryLinkAction::Relocate {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let record = RecordedAction::directory_link(&action, None).unwrap();
        DirectoryLinkExecutor::new(workspace.path("home").as_ref())
            .unwrap()
            .execute(&action, &record, Some(&workspace.source("new")))
            .unwrap();
        assert!(fs::symlink_metadata(previous.target_path().as_ref()).is_err());
        assert_eq!(
            fs::read_link(desired.target_path().as_ref()).unwrap(),
            desired.link_target().as_path().as_ref()
        );
    }

    #[test]
    fn replacement_consumes_only_its_recorded_temporary() {
        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim");
        let action = PlannedDirectoryLinkAction::Replace {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let temporary = workspace.path("home/.config/.nvim.loadout-tmp");
        let record = RecordedAction::directory_link(&action, Some(temporary.clone())).unwrap();
        DirectoryLinkExecutor::new(workspace.path("home").as_ref())
            .unwrap()
            .execute(&action, &record, Some(&workspace.source("new")))
            .unwrap();
        assert!(fs::symlink_metadata(temporary.as_ref()).is_err());
        assert_eq!(
            fs::read_link(desired.target_path().as_ref()).unwrap(),
            desired.link_target().as_path().as_ref()
        );
        assert_eq!(record.status(), ActionStatus::Pending);
    }

    #[test]
    fn relocate_rejects_an_old_target_reintroduced_before_complete_postcondition() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim-next");
        let action = PlannedDirectoryLinkAction::Relocate {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let record = RecordedAction::directory_link(&action, None).unwrap();
        let old_target = previous.target_path().clone();
        let old_source = previous.link_target().clone();
        let _hook = on_execution_boundary(ExecutionBoundary::BeforePostObservation, move || {
            symlink(old_source.as_path(), old_target.as_ref())
        });
        assert!(
            DirectoryLinkExecutor::new(workspace.path("home").as_ref())
                .unwrap()
                .execute(&action, &record, Some(&workspace.source("new")))
                .is_err()
        );
        assert!(
            fs::symlink_metadata(previous.target_path().as_ref())
                .unwrap()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(desired.target_path().as_ref())
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn relocate_recheck_rejection_does_not_remove_the_substituted_old_target() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim-next");
        let action = PlannedDirectoryLinkAction::Relocate {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let record = RecordedAction::directory_link(&action, None).unwrap();
        let old_target = previous.target_path().clone();
        let other = workspace.path("store/new");
        let _hook =
            on_execution_boundary(ExecutionBoundary::BeforeRelocateRemovalRecheck, move || {
                fs::remove_file(old_target.as_ref())?;
                symlink(other.as_ref(), old_target.as_ref())
            });
        assert!(
            DirectoryLinkExecutor::new(workspace.path("home").as_ref())
                .unwrap()
                .execute(&action, &record, Some(&workspace.source("new")))
                .is_err()
        );
        assert_eq!(
            fs::read_link(previous.target_path().as_ref()).unwrap(),
            workspace.path("store/new").as_ref()
        );
        assert_eq!(
            fs::read_link(desired.target_path().as_ref()).unwrap(),
            desired.link_target().as_path().as_ref()
        );
    }

    #[test]
    fn replacement_rejects_a_temporary_reintroduced_before_complete_postcondition() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim");
        let action = PlannedDirectoryLinkAction::Replace {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let temporary = workspace.path("home/.config/.nvim.loadout-tmp");
        let record = RecordedAction::directory_link(&action, Some(temporary.clone())).unwrap();
        let injected_temporary = temporary.clone();
        let other = workspace.path("store/old");
        let _hook = on_execution_boundary(ExecutionBoundary::BeforePostObservation, move || {
            symlink(other.as_ref(), injected_temporary.as_ref())
        });
        assert!(
            DirectoryLinkExecutor::new(workspace.path("home").as_ref())
                .unwrap()
                .execute(&action, &record, Some(&workspace.source("new")))
                .is_err()
        );
        assert!(
            fs::symlink_metadata(temporary.as_ref())
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(desired.target_path().as_ref()).unwrap(),
            desired.link_target().as_path().as_ref()
        );
    }

    #[test]
    fn forget_missing_rechecks_that_the_target_is_still_missing() {
        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        let action = PlannedDirectoryLinkAction::ForgetMissing {
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let record = RecordedAction::directory_link(&action, None).unwrap();
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        assert!(
            DirectoryLinkExecutor::new(workspace.path("home").as_ref())
                .unwrap()
                .execute(&action, &record, None)
                .is_err()
        );
        assert!(
            fs::symlink_metadata(previous.target_path().as_ref())
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn replacement_interruption_after_temporary_creation_preserves_old_effect_and_temporary() {
        use crate::test_support::{ExecutionBoundary, on_execution_boundary};

        let workspace = Workspace::new();
        let previous = workspace.desired("base/nvim", "store/old", "home/.config/nvim");
        symlink(
            previous.link_target().as_path(),
            previous.target_path().as_ref(),
        )
        .unwrap();
        let desired = workspace.desired("base/nvim", "store/new", "home/.config/nvim");
        let action = PlannedDirectoryLinkAction::Replace {
            desired: desired.clone(),
            previous: KnownDirectoryLink::from_resolved(&previous),
        };
        let temporary = workspace.path("home/.config/.nvim.loadout-tmp");
        let record = RecordedAction::directory_link(&action, Some(temporary.clone())).unwrap();
        let _hook = on_execution_boundary(ExecutionBoundary::AfterTemporaryCreation, || {
            Err(io::Error::other("injected interruption"))
        });
        assert!(
            DirectoryLinkExecutor::new(workspace.path("home").as_ref())
                .unwrap()
                .execute(&action, &record, Some(&workspace.source("new")))
                .is_err()
        );
        assert_eq!(
            fs::read_link(previous.target_path().as_ref()).unwrap(),
            previous.link_target().as_path().as_ref()
        );
        assert_eq!(
            fs::read_link(temporary.as_ref()).unwrap(),
            desired.link_target().as_path().as_ref()
        );
    }
}
