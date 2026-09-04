//! Immediate safety rechecks and execution for planned file-link actions.

use std::fmt;
use std::io;
use std::path::Path;

use crate::domain::actual::TargetObservation;
use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;
use crate::domain::plan::{ActionKind, PlannedAction, TargetCondition};
use crate::filesystem::{
    create_file_symbolic_link_no_replace, ensure_file_symbolic_link_creation_supported,
};
use crate::inspection::file_link::{FileLinkInspector, TargetInspectionError};
use crate::inspection::source::{SourceVerificationError, VerifiedSource};

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
        if let Err(source) = create_file_symbolic_link_no_replace(
            self.inspector.canonical_home(),
            &physical_target_path,
            &link_target,
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
                    inspection,
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

    /// Performs the non-mutating checks required before an operation record is created. `execute_create` repeats these checks immediately before its mutation, so this preflight result is never treated as authorization to skip the executor recheck.
    pub(crate) fn preflight_create(
        &self,
        action: &PlannedAction,
        source: &VerifiedSource,
    ) -> Result<(), CreateLinkExecutionError> {
        let (target_path, _) = self.recheck_create(action, source)?;
        let physical_target_path = self
            .inspector
            .physical_target_path_for_execution(&target_path)
            .map_err(CreateLinkExecutionError::TargetInspection)?;
        self.ensure_create_capability(&target_path, &physical_target_path)
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
        inspection: TargetInspectionError,
    },
    PostconditionInspection(TargetInspectionError),
    PostconditionNotMet {
        target_path: ResolvedPath,
        observation: TargetObservation,
    },
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
}
