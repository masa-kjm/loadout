//! No-follow target observation for resolved file-link resources.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use crate::domain::actual::{
    ActualFileCopy, ActualFileLink, ActualState, CopyTargetObservation, OtherEntryKind,
    ParentSafety, TargetObservation,
};
use crate::domain::desired::ResolvedDesired;
use crate::domain::file_copy::ContentFingerprint;
use crate::domain::file_link::LinkTarget;
use crate::domain::known::KnownState;
use crate::domain::paths::{ResolvedPath, ResolvedPathError};
use crate::filesystem::{
    NoFollowEntryKind, classify_nofollow_entry, normalize_observed_absolute_path,
};
use sha2::{Digest, Sha256};

/// A read-only observer rooted at the current user's canonical home directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileLinkInspector {
    declared_home: ResolvedPath,
    canonical_home: ResolvedPath,
}

impl FileLinkInspector {
    /// Establishes the canonical home directory used for physical target containment.
    pub(crate) fn new(home_directory: &Path) -> Result<Self, TargetInspectionError> {
        #[cfg(test)]
        crate::test_support::assert_target_inspection_allowed();
        let declared_home = ResolvedPath::new(home_directory.to_path_buf())
            .map_err(TargetInspectionError::InvalidDeclaredHome)?;
        let physical_home = fs::canonicalize(home_directory).map_err(|source| {
            TargetInspectionError::HomeDirectoryIo {
                path: home_directory.to_path_buf(),
                source,
            }
        })?;
        let metadata = fs::metadata(&physical_home).map_err(|source| {
            TargetInspectionError::HomeDirectoryIo {
                path: physical_home.clone(),
                source,
            }
        })?;
        if !metadata.is_dir() {
            return Err(TargetInspectionError::HomeDirectoryNotDirectory {
                path: physical_home,
            });
        }

        let canonical_home = ResolvedPath::from_platform_canonicalized(physical_home)
            .map_err(TargetInspectionError::InvalidCanonicalHome)?;
        Ok(Self {
            declared_home,
            canonical_home,
        })
    }

    /// Observes every target named by Desired or Known state exactly once.
    ///
    /// This performs no mutation and returns only planner-facing domain observations.
    pub(crate) fn inspect(
        &self,
        desired: &ResolvedDesired,
        known: &KnownState,
    ) -> Result<ActualState, TargetInspectionError> {
        let mut expectations = BTreeMap::<ResolvedPath, TargetExpectations>::new();

        for resource in desired.resources() {
            expectations
                .entry(resource.target_path().clone())
                .or_default()
                .desired_link_target = Some(resource.link_target().clone());
        }
        for resource in known.file_links() {
            expectations
                .entry(resource.target_path().clone())
                .or_default()
                .known_link_target = Some(resource.link_target().clone());
        }

        // A Known copy owns a byte fingerprint and determines the observation representation during a link/copy handoff at the same target.
        // Observing Desired links first would lose that ownership proof.
        let known_copy_targets = known
            .variants()
            .filter_map(|resource| match resource {
                crate::domain::known::KnownResource::FileCopy(resource) => Some(resource),
                crate::domain::known::KnownResource::FileLink(_) => None,
            })
            .collect::<Vec<_>>();
        let mut observations = known_copy_targets
            .iter()
            .map(|resource| {
                self.inspect_target_for_copy(
                    resource.target_path(),
                    Some(resource.content_fingerprint()),
                )
                .map(Into::into)
            })
            .collect::<Result<Vec<crate::domain::actual::ActualResource>, _>>()?;

        observations.extend(
            expectations
                .into_iter()
                .filter(|(target_path, _)| {
                    !known_copy_targets
                        .iter()
                        .any(|resource| resource.target_path() == target_path)
                })
                .map(|(target_path, expectations)| {
                    self.inspect_target(target_path, expectations)
                        .map(Into::into)
                })
                .collect::<Result<Vec<crate::domain::actual::ActualResource>, _>>()?,
        );
        for resource in desired.variants() {
            let crate::domain::desired::ResolvedResource::FileCopy(resource) = resource else {
                continue;
            };
            if observations
                .iter()
                .any(|observation| observation.target_path() == resource.target_path())
            {
                continue;
            }
            observations.push(
                self.inspect_target_for_copy(
                    resource.target_path(),
                    match known.get_variant(resource.resource_id()) {
                        Some(crate::domain::known::KnownResource::FileCopy(known)) => {
                            Some(known.content_fingerprint())
                        }
                        _ => None,
                    },
                )?
                .into(),
            );
        }
        for resource in known.variants() {
            let crate::domain::known::KnownResource::FileCopy(resource) = resource else {
                continue;
            };
            if observations
                .iter()
                .any(|observation| observation.target_path() == resource.target_path())
            {
                continue;
            }
            observations.push(
                self.inspect_target_for_copy(
                    resource.target_path(),
                    Some(resource.content_fingerprint()),
                )?
                .into(),
            );
        }
        ActualState::new(observations).map_err(TargetInspectionError::InvalidActualState)
    }

    /// Rechecks one target against the exact link target required by an executor post-condition. This does not confer ownership or mutate.
    pub(crate) fn inspect_target_for_expected_link(
        &self,
        target_path: &ResolvedPath,
        expected_link_target: &LinkTarget,
    ) -> Result<ActualFileLink, TargetInspectionError> {
        self.inspect_target(
            target_path.clone(),
            TargetExpectations {
                desired_link_target: None,
                known_link_target: Some(expected_link_target.clone()),
            },
        )
    }

    /// Rechecks one target as a regular file owned by an expected byte fingerprint.
    pub(crate) fn inspect_target_for_expected_copy(
        &self,
        target_path: &ResolvedPath,
        expected_fingerprint: &ContentFingerprint,
    ) -> Result<ActualFileCopy, TargetInspectionError> {
        self.inspect_target_for_copy(target_path, Some(expected_fingerprint))
    }

    fn inspect_target_for_copy(
        &self,
        target_path: &ResolvedPath,
        expected_fingerprint: Option<&ContentFingerprint>,
    ) -> Result<ActualFileCopy, TargetInspectionError> {
        let Some(physical_target_path) = self.physical_target_path(target_path)? else {
            return ActualFileCopy::new(
                target_path.clone(),
                CopyTargetObservation::UnsafePath {
                    parent_safety: ParentSafety::OutsideHome,
                },
            )
            .map_err(TargetInspectionError::InvalidActualFileCopy);
        };
        let parent_safety = self.inspect_parent_safety(target_path, &physical_target_path)?;
        let observation = if !parent_safety.is_safe() {
            CopyTargetObservation::UnsafePath { parent_safety }
        } else {
            self.inspect_final_copy(target_path, &physical_target_path, expected_fingerprint)?
        };
        ActualFileCopy::new(target_path.clone(), observation)
            .map_err(TargetInspectionError::InvalidActualFileCopy)
    }

    /// Observes one declared Desired target without treating a matching link as owned.
    pub(crate) fn inspect_target_for_desired_link(
        &self,
        target_path: &ResolvedPath,
        desired_link_target: &LinkTarget,
    ) -> Result<ActualFileLink, TargetInspectionError> {
        self.inspect_target(
            target_path.clone(),
            TargetExpectations {
                desired_link_target: Some(desired_link_target.clone()),
                known_link_target: None,
            },
        )
    }

    /// Resolves the physical target name anchored at the canonical home used by this inspector. The executor passes this to the filesystem boundary only after the no-follow recheck has established a safe parent path.
    pub(crate) fn physical_target_path_for_execution(
        &self,
        target_path: &ResolvedPath,
    ) -> Result<ResolvedPath, TargetInspectionError> {
        self.physical_target_path(target_path)?.ok_or_else(|| {
            TargetInspectionError::InvalidTargetPath {
                target_path: target_path.clone(),
            }
        })
    }

    /// The declared home spelling whose association execution must also retain.
    pub(crate) fn declared_home(&self) -> &ResolvedPath {
        &self.declared_home
    }

    /// The canonical home root used to anchor a no-follow filesystem mutation.
    pub(crate) fn canonical_home(&self) -> &ResolvedPath {
        &self.canonical_home
    }

    fn inspect_target(
        &self,
        target_path: ResolvedPath,
        expectations: TargetExpectations,
    ) -> Result<ActualFileLink, TargetInspectionError> {
        #[cfg(test)]
        crate::test_support::assert_target_inspection_allowed();
        let observation = if let Some(physical_target_path) =
            self.physical_target_path(&target_path)?
        {
            let parent_safety = self.inspect_parent_safety(&target_path, &physical_target_path)?;
            if !parent_safety.is_safe() {
                TargetObservation::UnsafePath { parent_safety }
            } else {
                self.inspect_final_target(&target_path, &physical_target_path, &expectations)?
            }
        } else {
            TargetObservation::UnsafePath {
                parent_safety: ParentSafety::OutsideHome,
            }
        };

        ActualFileLink::new(target_path, observation)
            .map_err(TargetInspectionError::InvalidActualFileLink)
    }

    fn physical_target_path(
        &self,
        target_path: &ResolvedPath,
    ) -> Result<Option<ResolvedPath>, TargetInspectionError> {
        if target_path
            .as_ref()
            .starts_with(self.canonical_home.as_ref())
        {
            return Ok(Some(target_path.clone()));
        }
        let Ok(relative_target) = target_path
            .as_ref()
            .strip_prefix(self.declared_home.as_ref())
        else {
            return Ok(None);
        };
        let physical_target = ResolvedPath::new(self.canonical_home.as_ref().join(relative_target))
            .map_err(|_| TargetInspectionError::InvalidTargetPath {
                target_path: target_path.clone(),
            })?;
        Ok(Some(physical_target))
    }

    fn inspect_parent_safety(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
    ) -> Result<ParentSafety, TargetInspectionError> {
        if !physical_target_path
            .as_ref()
            .starts_with(self.canonical_home.as_ref())
        {
            return Ok(ParentSafety::OutsideHome);
        }
        let target_parent = physical_target_path.as_ref().parent().ok_or_else(|| {
            TargetInspectionError::TargetHasNoParent {
                target_path: target_path.clone(),
            }
        })?;
        if !target_parent.starts_with(self.canonical_home.as_ref()) {
            return Ok(ParentSafety::OutsideHome);
        }
        let home_metadata = match fs::symlink_metadata(self.canonical_home.as_ref()) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(ParentSafety::Missing);
            }
            Err(source) => {
                return Err(TargetInspectionError::ParentMetadata {
                    target_path: target_path.clone(),
                    parent_path: self.canonical_home.as_ref().to_path_buf(),
                    source,
                });
            }
        };
        match classify_nofollow_entry(&home_metadata) {
            NoFollowEntryKind::Directory => {}
            NoFollowEntryKind::FileSymbolicLink => return Ok(ParentSafety::Symlink),
            NoFollowEntryKind::ReparsePoint => return Ok(ParentSafety::ReparsePoint),
            NoFollowEntryKind::RegularFile | NoFollowEntryKind::Unsupported => {
                return Ok(ParentSafety::NotDirectory);
            }
        }
        let relative_parent = target_parent
            .strip_prefix(self.canonical_home.as_ref())
            .expect("a checked path prefix must strip successfully");
        let mut current = self.canonical_home.as_ref().to_path_buf();

        for component in relative_parent.components() {
            let Component::Normal(component) = component else {
                return Err(TargetInspectionError::InvalidTargetPath {
                    target_path: target_path.clone(),
                });
            };
            current.push(component);
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return Ok(ParentSafety::Missing);
                }
                Err(source) => {
                    return Err(TargetInspectionError::ParentMetadata {
                        target_path: target_path.clone(),
                        parent_path: current,
                        source,
                    });
                }
            };

            match classify_nofollow_entry(&metadata) {
                NoFollowEntryKind::Directory => {}
                NoFollowEntryKind::FileSymbolicLink => return Ok(ParentSafety::Symlink),
                NoFollowEntryKind::ReparsePoint => return Ok(ParentSafety::ReparsePoint),
                NoFollowEntryKind::RegularFile | NoFollowEntryKind::Unsupported => {
                    return Ok(ParentSafety::NotDirectory);
                }
            }
        }

        Ok(ParentSafety::Safe)
    }

    fn inspect_final_target(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
        expectations: &TargetExpectations,
    ) -> Result<TargetObservation, TargetInspectionError> {
        let metadata = match fs::symlink_metadata(physical_target_path.as_ref()) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(TargetObservation::Missing);
            }
            Err(source) => {
                return Err(TargetInspectionError::TargetMetadata {
                    target_path: target_path.clone(),
                    source,
                });
            }
        };

        match classify_nofollow_entry(&metadata) {
            NoFollowEntryKind::FileSymbolicLink => {
                let link_target = read_normalized_link_target(target_path, physical_target_path)?;
                if expectations.known_link_target.as_ref() == Some(&link_target) {
                    Ok(TargetObservation::ExpectedLink { link_target })
                } else if expectations.desired_link_target.as_ref() == Some(&link_target) {
                    Ok(TargetObservation::MatchingUnmanagedLink { link_target })
                } else {
                    Ok(TargetObservation::OtherLink { link_target })
                }
            }
            NoFollowEntryKind::RegularFile => Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            }),
            NoFollowEntryKind::Directory => Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::Directory,
            }),
            NoFollowEntryKind::ReparsePoint => Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::ReparsePoint,
            }),
            NoFollowEntryKind::Unsupported => Ok(TargetObservation::OtherEntry {
                kind: OtherEntryKind::Unsupported,
            }),
        }
    }

    fn inspect_final_copy(
        &self,
        target_path: &ResolvedPath,
        physical_target_path: &ResolvedPath,
        expected_fingerprint: Option<&ContentFingerprint>,
    ) -> Result<CopyTargetObservation, TargetInspectionError> {
        let metadata = match fs::symlink_metadata(physical_target_path.as_ref()) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(CopyTargetObservation::Missing);
            }
            Err(source) => {
                return Err(TargetInspectionError::TargetMetadata {
                    target_path: target_path.clone(),
                    source,
                });
            }
        };
        match classify_nofollow_entry(&metadata) {
            NoFollowEntryKind::RegularFile => {
                let mut file =
                    open_copy_target_nofollow(physical_target_path.as_ref()).map_err(|source| {
                        TargetInspectionError::TargetContent {
                            target_path: target_path.clone(),
                            source,
                        }
                    })?;
                let mut hasher = Sha256::new();
                let mut buffer = [0_u8; 8192];
                loop {
                    let read = file.read(&mut buffer).map_err(|source| {
                        TargetInspectionError::TargetContent {
                            target_path: target_path.clone(),
                            source,
                        }
                    })?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                let fingerprint =
                    ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize()))
                        .expect("SHA-256 output is canonical");
                Ok(if expected_fingerprint == Some(&fingerprint) {
                    CopyTargetObservation::ExpectedCopy {
                        content_fingerprint: fingerprint,
                    }
                } else {
                    CopyTargetObservation::OtherRegularFile {
                        content_fingerprint: fingerprint,
                    }
                })
            }
            NoFollowEntryKind::Directory => Ok(CopyTargetObservation::OtherEntry {
                kind: OtherEntryKind::Directory,
            }),
            NoFollowEntryKind::ReparsePoint => Ok(CopyTargetObservation::OtherEntry {
                kind: OtherEntryKind::ReparsePoint,
            }),
            NoFollowEntryKind::FileSymbolicLink | NoFollowEntryKind::Unsupported => {
                Ok(CopyTargetObservation::OtherEntry {
                    kind: OtherEntryKind::Unsupported,
                })
            }
        }
    }
}

fn open_copy_target_nofollow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TargetExpectations {
    desired_link_target: Option<LinkTarget>,
    known_link_target: Option<LinkTarget>,
}

fn read_normalized_link_target(
    target_path: &ResolvedPath,
    physical_target_path: &ResolvedPath,
) -> Result<LinkTarget, TargetInspectionError> {
    let observed = fs::read_link(physical_target_path.as_ref()).map_err(|source| {
        TargetInspectionError::ReadLink {
            target_path: target_path.clone(),
            source,
        }
    })?;
    let absolute = if observed.is_absolute() {
        observed.clone()
    } else {
        physical_target_path
            .as_ref()
            .parent()
            .ok_or_else(|| TargetInspectionError::TargetHasNoParent {
                target_path: target_path.clone(),
            })?
            .join(&observed)
    };
    let normalized = normalize_observed_absolute_path(&absolute).map_err(|source| {
        TargetInspectionError::InvalidObservedLinkTarget {
            target_path: target_path.clone(),
            link_target: observed,
            source,
        }
    })?;

    Ok(LinkTarget::new(normalized))
}

/// The reason target observation could not produce a complete safe Actual state.
#[derive(Debug)]
pub(crate) enum TargetInspectionError {
    HomeDirectoryIo {
        path: PathBuf,
        source: io::Error,
    },
    HomeDirectoryNotDirectory {
        path: PathBuf,
    },
    InvalidDeclaredHome(ResolvedPathError),
    InvalidCanonicalHome(ResolvedPathError),
    TargetHasNoParent {
        target_path: ResolvedPath,
    },
    InvalidTargetPath {
        target_path: ResolvedPath,
    },
    ParentMetadata {
        target_path: ResolvedPath,
        parent_path: PathBuf,
        source: io::Error,
    },
    TargetMetadata {
        target_path: ResolvedPath,
        source: io::Error,
    },
    TargetContent {
        target_path: ResolvedPath,
        source: io::Error,
    },
    ReadLink {
        target_path: ResolvedPath,
        source: io::Error,
    },
    InvalidObservedLinkTarget {
        target_path: ResolvedPath,
        link_target: PathBuf,
        source: ResolvedPathError,
    },
    InvalidActualFileLink(crate::domain::actual::ActualFileLinkError),
    InvalidActualFileCopy(crate::domain::actual::ActualFileCopyError),
    InvalidActualState(crate::domain::actual::ActualStateError),
}

impl fmt::Display for TargetInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeDirectoryIo { path, source } => {
                write!(
                    formatter,
                    "cannot access home directory {}: {source}",
                    path.display()
                )
            }
            Self::HomeDirectoryNotDirectory { path } => {
                write!(
                    formatter,
                    "home directory is not a directory: {}",
                    path.display()
                )
            }
            Self::InvalidDeclaredHome(error) => error.fmt(formatter),
            Self::InvalidCanonicalHome(error) => error.fmt(formatter),
            Self::TargetHasNoParent { target_path } => {
                write!(formatter, "target path has no parent: {target_path}")
            }
            Self::InvalidTargetPath { target_path } => {
                write!(
                    formatter,
                    "target path cannot be inspected safely: {target_path}"
                )
            }
            Self::ParentMetadata {
                target_path,
                parent_path,
                source,
            } => write!(
                formatter,
                "cannot inspect parent {} for target {target_path}: {source}",
                parent_path.display()
            ),
            Self::TargetMetadata {
                target_path,
                source,
            } => write!(formatter, "cannot inspect target {target_path}: {source}"),
            Self::TargetContent {
                target_path,
                source,
            } => write!(
                formatter,
                "cannot fingerprint target {target_path}: {source}"
            ),
            Self::ReadLink {
                target_path,
                source,
            } => write!(
                formatter,
                "cannot read symbolic link {target_path}: {source}"
            ),
            Self::InvalidObservedLinkTarget {
                target_path,
                link_target,
                source,
            } => write!(
                formatter,
                "cannot normalize link target {} at {target_path}: {source}",
                link_target.display()
            ),
            Self::InvalidActualFileLink(error) => error.fmt(formatter),
            Self::InvalidActualFileCopy(error) => error.fmt(formatter),
            Self::InvalidActualState(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TargetInspectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HomeDirectoryIo { source, .. }
            | Self::ParentMetadata { source, .. }
            | Self::TargetMetadata { source, .. }
            | Self::TargetContent { source, .. }
            | Self::ReadLink { source, .. } => Some(source),
            Self::InvalidDeclaredHome(error)
            | Self::InvalidCanonicalHome(error)
            | Self::InvalidObservedLinkTarget { source: error, .. } => Some(error),
            Self::InvalidActualFileLink(error) => Some(error),
            Self::InvalidActualFileCopy(error) => Some(error),
            Self::InvalidActualState(error) => Some(error),
            Self::HomeDirectoryNotDirectory { .. }
            | Self::TargetHasNoParent { .. }
            | Self::InvalidTargetPath { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::domain::desired::ResolvedDesired;
    use crate::domain::file_copy::{ContentFingerprint, ResolvedFileCopy};
    use crate::domain::file_link::ResolvedFileLink;
    use crate::domain::ids::{FullyQualifiedResourceId, ProfileId};
    #[cfg(any(unix, windows))]
    use crate::domain::known::KnownFileLink;
    use crate::domain::known::{KnownFileCopy, KnownState};

    static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    unsafe extern "C" {
        fn mkfifo(pathname: *const std::ffi::c_char, mode: u32) -> std::ffi::c_int;
    }

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
                "loadout-file-link-inspection-test-{}-{timestamp}-{unique_id}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            for directory in ["home", "store", "config", "state"] {
                fs::create_dir(root.join(directory)).unwrap();
            }

            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn create_dir(&self, relative: &str) {
            fs::create_dir_all(self.path(relative)).unwrap();
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn inspector(&self) -> FileLinkInspector {
            FileLinkInspector::new(&self.path("home")).unwrap()
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn resource(
        workspace: &TestWorkspace,
        id: &str,
        source_relative_path: &str,
        target_relative_path: &str,
    ) -> ResolvedFileLink {
        workspace.write(
            &format!("store/{source_relative_path}"),
            "source contents\n",
        );
        resource_at(
            workspace,
            id,
            source_relative_path,
            ResolvedPath::new(workspace.path("home").join(target_relative_path)).unwrap(),
        )
    }

    fn resource_at(
        workspace: &TestWorkspace,
        id: &str,
        source_relative_path: &str,
        target_path: ResolvedPath,
    ) -> ResolvedFileLink {
        ResolvedFileLink::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            ResolvedPath::new(workspace.path("store").join(source_relative_path)).unwrap(),
            target_path,
        )
        .unwrap()
    }

    fn desired(resources: impl IntoIterator<Item = ResolvedFileLink>) -> ResolvedDesired {
        ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), resources).unwrap()
    }

    fn fingerprint(contents: &[u8]) -> ContentFingerprint {
        let mut hasher = Sha256::new();
        hasher.update(contents);
        ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize())).unwrap()
    }

    fn copy_resource(
        workspace: &TestWorkspace,
        id: &str,
        source_relative_path: &str,
        target_relative_path: &str,
        contents: &[u8],
    ) -> ResolvedFileCopy {
        let source = workspace.path("store").join(source_relative_path);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, contents).unwrap();
        ResolvedFileCopy::new(
            FullyQualifiedResourceId::parse(id).unwrap(),
            ResolvedPath::new(source).unwrap(),
            ResolvedPath::new(workspace.path("home").join(target_relative_path)).unwrap(),
            fingerprint(contents),
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "mkfifo must create the disposable test entry");
    }

    #[test]
    fn inspector_classifies_missing_regular_and_directory_targets_without_mutation() {
        let workspace = TestWorkspace::new();
        let missing = resource(&workspace, "workstation/missing", "missing", "missing");
        let regular = resource(&workspace, "workstation/regular", "regular", "regular");
        let directory = resource(
            &workspace,
            "workstation/directory",
            "directory",
            "directory",
        );
        workspace.write("home/regular", "unmanaged\n");
        workspace.create_dir("home/directory");

        let actual = workspace
            .inspector()
            .inspect(
                &desired([missing.clone(), regular.clone(), directory.clone()]),
                &KnownState::empty(),
            )
            .unwrap();

        assert_eq!(
            actual.get(missing.target_path()).unwrap().observation(),
            &TargetObservation::Missing
        );
        assert_eq!(
            actual.get(regular.target_path()).unwrap().observation(),
            &TargetObservation::OtherEntry {
                kind: OtherEntryKind::RegularFile,
            }
        );
        assert_eq!(
            actual.get(directory.target_path()).unwrap().observation(),
            &TargetObservation::OtherEntry {
                kind: OtherEntryKind::Directory,
            }
        );
        assert!(
            !workspace.path("home/missing").exists(),
            "inspection must not create a missing target"
        );
    }

    #[test]
    fn inspector_classifies_copy_targets_by_known_byte_fingerprint_without_adoption() {
        let workspace = TestWorkspace::new();
        let expected = copy_resource(
            &workspace,
            "workstation/expected-copy",
            "expected-copy-source",
            "expected-copy",
            b"expected copy bytes\n",
        );
        let other = copy_resource(
            &workspace,
            "workstation/other-copy",
            "other-copy-source",
            "other-copy",
            b"desired bytes\n",
        );
        let missing = copy_resource(
            &workspace,
            "workstation/missing-copy",
            "missing-copy-source",
            "missing-copy",
            b"missing desired bytes\n",
        );
        fs::write(expected.target_path().as_ref(), b"expected copy bytes\n").unwrap();
        fs::write(other.target_path().as_ref(), b"desired bytes\n").unwrap();
        let known = KnownState::new([KnownFileCopy::from_resolved(&expected)]).unwrap();
        let desired = ResolvedDesired::new(
            ProfileId::parse("workstation").unwrap(),
            [expected.clone(), other.clone(), missing.clone()],
        )
        .unwrap();

        let actual = workspace.inspector().inspect(&desired, &known).unwrap();

        assert!(matches!(
            actual.get_variant(expected.target_path()),
            Some(crate::domain::actual::ActualResource::FileCopy(
                ActualFileCopy { .. }
            ))
        ));
        assert_eq!(
            actual
                .get_variant(expected.target_path())
                .and_then(|value| match value {
                    crate::domain::actual::ActualResource::FileCopy(value) =>
                        Some(value.observation()),
                    _ => None,
                }),
            Some(&CopyTargetObservation::ExpectedCopy {
                content_fingerprint: expected.source_content_fingerprint().clone(),
            })
        );
        assert_eq!(
            actual
                .get_variant(other.target_path())
                .and_then(|value| match value {
                    crate::domain::actual::ActualResource::FileCopy(value) =>
                        Some(value.observation()),
                    _ => None,
                }),
            Some(&CopyTargetObservation::OtherRegularFile {
                content_fingerprint: other.source_content_fingerprint().clone(),
            }),
            "matching desired bytes without Known state are unmanaged"
        );
        assert_eq!(
            actual
                .get_variant(missing.target_path())
                .and_then(|value| match value {
                    crate::domain::actual::ActualResource::FileCopy(value) =>
                        Some(value.observation()),
                    _ => None,
                }),
            Some(&CopyTargetObservation::Missing)
        );
    }

    #[test]
    fn known_copy_observation_wins_over_desired_link_during_effect_handoff() {
        let workspace = TestWorkspace::new();
        let copy = copy_resource(
            &workspace,
            "workstation/item",
            "copy-source",
            "item",
            b"owned copy bytes\n",
        );
        fs::write(copy.target_path().as_ref(), b"owned copy bytes\n").unwrap();
        workspace.write("store/link-source", "link source bytes\n");
        let link = ResolvedFileLink::new(
            copy.resource_id().clone(),
            ResolvedPath::new(workspace.path("store/link-source")).unwrap(),
            copy.target_path().clone(),
        )
        .unwrap();
        let known = KnownState::new([KnownFileCopy::from_resolved(&copy)]).unwrap();
        let desired =
            ResolvedDesired::new(ProfileId::parse("workstation").unwrap(), [link.clone()]).unwrap();

        let actual = workspace.inspector().inspect(&desired, &known).unwrap();
        assert!(matches!(
            actual.get_variant(link.target_path()),
            Some(crate::domain::actual::ActualResource::FileCopy(actual))
                if matches!(actual.observation(), CopyTargetObservation::ExpectedCopy { content_fingerprint }
                    if content_fingerprint == copy.source_content_fingerprint())
        ));

        let plan = crate::planner::plan(&desired, &known, &actual);
        assert!(matches!(
            plan.resource_actions(),
            [crate::domain::plan::PlannedResourceAction::ReplaceEffect(_)]
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inspector_distinguishes_expected_unmanaged_and_other_links_without_following_them() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.create_dir("home/config");
        let expected = resource(
            &workspace,
            "workstation/expected",
            "expected-source",
            "config/expected",
        );
        let unmanaged = resource(
            &workspace,
            "workstation/unmanaged",
            "unmanaged-source",
            "config/unmanaged",
        );
        let other = resource(
            &workspace,
            "workstation/other",
            "other-source",
            "config/other",
        );
        let changed = resource(
            &workspace,
            "workstation/changed",
            "changed-current-source",
            "config/changed",
        );
        workspace.write("store/changed-old-source", "old source contents\n");
        let old_changed = ResolvedFileLink::new(
            FullyQualifiedResourceId::parse("workstation/changed").unwrap(),
            ResolvedPath::new(workspace.path("store/changed-old-source")).unwrap(),
            changed.target_path().clone(),
        )
        .unwrap();
        symlink(
            expected.link_target().as_path().as_ref(),
            expected.target_path().as_ref(),
        )
        .unwrap();
        symlink(
            unmanaged.link_target().as_path().as_ref(),
            unmanaged.target_path().as_ref(),
        )
        .unwrap();
        symlink("../unfollowed-other-target", other.target_path().as_ref()).unwrap();
        symlink(
            changed.link_target().as_path().as_ref(),
            changed.target_path().as_ref(),
        )
        .unwrap();
        let known = KnownState::new([
            KnownFileLink::from_resolved(&expected),
            KnownFileLink::from_resolved(&old_changed),
        ])
        .unwrap();

        let actual = workspace
            .inspector()
            .inspect(
                &desired([
                    expected.clone(),
                    unmanaged.clone(),
                    other.clone(),
                    changed.clone(),
                ]),
                &known,
            )
            .unwrap();

        assert_eq!(
            actual.get(expected.target_path()).unwrap().observation(),
            &TargetObservation::ExpectedLink {
                link_target: expected.link_target().clone(),
            }
        );
        assert_eq!(
            actual.get(unmanaged.target_path()).unwrap().observation(),
            &TargetObservation::MatchingUnmanagedLink {
                link_target: unmanaged.link_target().clone(),
            }
        );
        assert_eq!(
            actual.get(other.target_path()).unwrap().observation(),
            &TargetObservation::OtherLink {
                link_target: LinkTarget::new(
                    ResolvedPath::new(workspace.path("home/unfollowed-other-target")).unwrap(),
                ),
            }
        );
        assert_eq!(
            actual.get(changed.target_path()).unwrap().observation(),
            &TargetObservation::MatchingUnmanagedLink {
                link_target: changed.link_target().clone(),
            },
            "a link matching Desired but not Known must never be adopted"
        );
        assert_eq!(
            fs::read_link(other.target_path().as_ref()).unwrap(),
            PathBuf::from("../unfollowed-other-target"),
            "inspection must not rewrite or follow the final symbolic link"
        );
    }

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) {
        use std::os::windows::process::CommandExt;

        let output = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .output()
            .expect("the Windows command interpreter must be available");
        assert!(
            output.status.success(),
            "mklink /J must create a disposable junction: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[cfg(windows)]
    #[test]
    fn inspector_observes_file_links_and_rejects_junction_targets_and_parents() {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let workspace = TestWorkspace::new();
        workspace.create_dir("home/config");
        workspace.create_dir("outside");
        let expected = resource(
            &workspace,
            "workstation/expected",
            "expected-source",
            "config/expected",
        );
        let final_junction = resource(
            &workspace,
            "workstation/final-junction",
            "final-junction-source",
            "final-junction",
        );
        let parent_junction = resource(
            &workspace,
            "workstation/parent-junction",
            "parent-junction-source",
            "parent-junction/target",
        );
        symlink_file(
            expected.link_target().as_path().as_ref(),
            expected.target_path().as_ref(),
        )
        .expect("the Windows test runner must support file symbolic links");
        create_junction(
            &workspace.path("home/final-junction"),
            &workspace.path("outside"),
        );
        create_junction(
            &workspace.path("home/parent-junction"),
            &workspace.path("outside"),
        );
        let directory_link = resource(
            &workspace,
            "workstation/directory-link",
            "dir-source",
            "directory-link",
        );
        symlink_dir(
            workspace.path("outside"),
            directory_link.target_path().as_ref(),
        )
        .unwrap();
        let known = KnownState::new([KnownFileLink::from_resolved(&expected)]).unwrap();

        let actual = workspace
            .inspector()
            .inspect(
                &desired([
                    expected.clone(),
                    final_junction.clone(),
                    parent_junction.clone(),
                    directory_link.clone(),
                ]),
                &known,
            )
            .unwrap();

        assert_eq!(
            actual.get(expected.target_path()).unwrap().observation(),
            &TargetObservation::ExpectedLink {
                link_target: expected.link_target().clone(),
            }
        );
        assert_eq!(
            actual
                .get(final_junction.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::OtherEntry {
                kind: OtherEntryKind::ReparsePoint,
            }
        );
        assert_eq!(
            actual
                .get(parent_junction.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::UnsafePath {
                parent_safety: ParentSafety::ReparsePoint,
            }
        );
        assert_eq!(
            actual
                .get(directory_link.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::OtherEntry {
                kind: OtherEntryKind::ReparsePoint
            }
        );
        assert_eq!(
            fs::read_link(directory_link.target_path().as_ref()).unwrap(),
            workspace.path("outside")
        );
        assert!(
            !workspace.path("outside/target").exists(),
            "inspection must not follow a junction parent"
        );
    }

    #[test]
    fn inspector_classifies_missing_non_directory_and_outside_home_parents_as_unsafe() {
        let workspace = TestWorkspace::new();
        let missing_parent = resource(
            &workspace,
            "workstation/missing-parent",
            "missing-parent-source",
            "missing-parent/target",
        );
        let non_directory_parent = resource(
            &workspace,
            "workstation/non-directory-parent",
            "non-directory-source",
            "non-directory-parent/target",
        );
        workspace.write("home/non-directory-parent", "not a directory\n");
        let outside_home = resource_at(
            &workspace,
            "workstation/outside-home",
            "missing-parent-source",
            ResolvedPath::new(workspace.path("outside/target")).unwrap(),
        );

        let actual = workspace
            .inspector()
            .inspect(
                &desired([
                    missing_parent.clone(),
                    non_directory_parent.clone(),
                    outside_home.clone(),
                ]),
                &KnownState::empty(),
            )
            .unwrap();

        assert_eq!(
            actual
                .get(missing_parent.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Missing,
            }
        );
        assert_eq!(
            actual
                .get(non_directory_parent.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::UnsafePath {
                parent_safety: ParentSafety::NotDirectory,
            }
        );
        assert_eq!(
            actual
                .get(outside_home.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::UnsafePath {
                parent_safety: ParentSafety::OutsideHome,
            }
        );
        assert!(
            !workspace.path("home/missing-parent").exists(),
            "inspection must not create a missing parent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspector_rejects_a_symlinked_parent_without_observing_its_child() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.create_dir("outside");
        symlink(
            workspace.path("outside"),
            workspace.path("home/linked-parent"),
        )
        .unwrap();
        let resource = resource(
            &workspace,
            "workstation/linked-parent",
            "linked-parent-source",
            "linked-parent/target",
        );

        let actual = workspace
            .inspector()
            .inspect(&desired([resource.clone()]), &KnownState::empty())
            .unwrap();

        assert_eq!(
            actual.get(resource.target_path()).unwrap().observation(),
            &TargetObservation::UnsafePath {
                parent_safety: ParentSafety::Symlink,
            }
        );
        assert!(
            !workspace.path("outside/target").exists(),
            "inspection must not follow a symlinked parent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspector_classifies_unsupported_final_entries_without_mutation() {
        let workspace = TestWorkspace::new();
        let resource = resource(
            &workspace,
            "workstation/unsupported",
            "unsupported-source",
            "unsupported",
        );
        create_fifo(resource.target_path().as_ref());

        let actual = workspace
            .inspector()
            .inspect(&desired([resource.clone()]), &KnownState::empty())
            .unwrap();

        assert_eq!(
            actual.get(resource.target_path()).unwrap().observation(),
            &TargetObservation::OtherEntry {
                kind: OtherEntryKind::Unsupported,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspector_includes_known_only_stale_targets_and_uses_a_canonical_home_root() {
        use std::os::unix::fs::symlink;

        let workspace = TestWorkspace::new();
        workspace.create_dir("physical-home");
        symlink(
            workspace.path("physical-home"),
            workspace.path("declared-home"),
        )
        .unwrap();
        workspace.write("store/stale-source", "source contents\n");
        let stale = resource_at(
            &workspace,
            "workstation/stale",
            "stale-source",
            ResolvedPath::new(workspace.path("physical-home/stale")).unwrap(),
        );
        symlink(
            stale.link_target().as_path().as_ref(),
            stale.target_path().as_ref(),
        )
        .unwrap();
        let declared_target = resource_at(
            &workspace,
            "workstation/declared-home-target",
            "declared-home-source",
            ResolvedPath::new(workspace.path("declared-home/missing")).unwrap(),
        );
        workspace.write("store/declared-home-source", "source contents\n");
        let known = KnownState::new([KnownFileLink::from_resolved(&stale)]).unwrap();
        let inspector = FileLinkInspector::new(&workspace.path("declared-home")).unwrap();

        let actual = inspector
            .inspect(&desired([declared_target.clone()]), &known)
            .unwrap();

        assert_eq!(
            actual.get(stale.target_path()).unwrap().observation(),
            &TargetObservation::ExpectedLink {
                link_target: stale.link_target().clone(),
            }
        );
        assert_eq!(
            actual
                .get(declared_target.target_path())
                .unwrap()
                .observation(),
            &TargetObservation::Missing
        );
        assert!(
            !workspace.path("physical-home/missing").exists(),
            "inspection must not create a target below the canonical home"
        );
    }

    #[test]
    fn inspector_requires_an_existing_home_directory() {
        let workspace = TestWorkspace::new();
        workspace.write("not-a-directory", "contents\n");

        let error = FileLinkInspector::new(&workspace.path("not-a-directory")).unwrap_err();

        assert!(matches!(
            error,
            TargetInspectionError::HomeDirectoryNotDirectory { .. }
        ));
    }
}
