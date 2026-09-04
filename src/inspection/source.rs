//! Physical containment and regular-file verification for local-store sources.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::paths::{ResolvedPath, ResolvedPathError, SourceRelativePath};
use crate::filesystem::is_link_or_reparse_point;

/// A canonical existing directory that is safe to use as a local-store root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalStoreRoot {
    path: ResolvedPath,
}

impl PhysicalStoreRoot {
    /// Returns the canonical directory path for containment comparisons.
    pub(crate) fn as_path(&self) -> &ResolvedPath {
        &self.path
    }
}

/// A source path that was proven to be a regular file below a physical store root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedSource {
    path: ResolvedPath,
    store_root: PhysicalStoreRoot,
    source_path: SourceRelativePath,
}

impl VerifiedSource {
    /// The verified absolute path used as a file-link source and link target.
    pub(crate) fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Repeats the no-follow store-containment and regular-file proof immediately before a resource mutation.
    pub(crate) fn reverify(&self) -> Result<Self, SourceVerificationError> {
        verify_regular_source(&self.store_root, &self.source_path)
    }
}

/// Resolves an existing local-store root to its physical directory.
pub(crate) fn resolve_store_root(
    store_root: &Path,
) -> Result<PhysicalStoreRoot, SourceVerificationError> {
    let physical_root =
        fs::canonicalize(store_root).map_err(|source| SourceVerificationError::StoreRootIo {
            path: store_root.to_path_buf(),
            source,
        })?;
    let metadata =
        fs::metadata(&physical_root).map_err(|source| SourceVerificationError::StoreRootIo {
            path: physical_root.clone(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(SourceVerificationError::StoreRootNotDirectory {
            path: physical_root,
        });
    }

    let path =
        ResolvedPath::new(physical_root).map_err(SourceVerificationError::InvalidResolvedPath)?;
    Ok(PhysicalStoreRoot { path })
}

/// Verifies all source components without following links beneath a physical store root.
pub(crate) fn verify_regular_source(
    store_root: &PhysicalStoreRoot,
    source_path: &SourceRelativePath,
) -> Result<VerifiedSource, SourceVerificationError> {
    let root_metadata = fs::symlink_metadata(store_root.as_path().as_ref()).map_err(|source| {
        SourceVerificationError::StoreRootIo {
            path: store_root.as_path().as_ref().to_path_buf(),
            source,
        }
    })?;
    if is_link_or_reparse_point(&root_metadata) {
        return Err(SourceVerificationError::StoreRootLinkOrReparsePoint {
            path: store_root.as_path().as_ref().to_path_buf(),
        });
    }
    if !root_metadata.is_dir() {
        return Err(SourceVerificationError::StoreRootNotDirectory {
            path: store_root.as_path().as_ref().to_path_buf(),
        });
    }

    let (final_component, parent_components) = source_path
        .components()
        .split_last()
        .expect("SourcePath always contains at least one validated component");
    let mut current = store_root.as_path().as_ref().to_path_buf();

    for component in parent_components {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|source| {
            SourceVerificationError::SourceComponentIo {
                path: current.clone(),
                source,
            }
        })?;
        let file_type = metadata.file_type();
        if is_link_or_reparse_point(&metadata) {
            return Err(SourceVerificationError::SourceParentLinkOrReparsePoint { path: current });
        }
        if !file_type.is_dir() {
            return Err(SourceVerificationError::SourceParentNotDirectory { path: current });
        }
    }

    current.push(final_component);
    let metadata = fs::symlink_metadata(&current).map_err(|source| {
        SourceVerificationError::SourceComponentIo {
            path: current.clone(),
            source,
        }
    })?;
    let file_type = metadata.file_type();
    if is_link_or_reparse_point(&metadata) {
        return Err(SourceVerificationError::SourceLinkOrReparsePoint { path: current });
    }
    if !file_type.is_file() {
        return Err(SourceVerificationError::SourceNotRegular { path: current });
    }

    let path = ResolvedPath::new(current).map_err(SourceVerificationError::InvalidResolvedPath)?;
    Ok(VerifiedSource {
        path,
        store_root: store_root.clone(),
        source_path: source_path.clone(),
    })
}

/// The reason a local-store source cannot be treated as verified input.
#[derive(Debug)]
pub(crate) enum SourceVerificationError {
    StoreRootIo { path: PathBuf, source: io::Error },
    StoreRootNotDirectory { path: PathBuf },
    StoreRootLinkOrReparsePoint { path: PathBuf },
    SourceComponentIo { path: PathBuf, source: io::Error },
    SourceParentLinkOrReparsePoint { path: PathBuf },
    SourceParentNotDirectory { path: PathBuf },
    SourceLinkOrReparsePoint { path: PathBuf },
    SourceNotRegular { path: PathBuf },
    InvalidResolvedPath(ResolvedPathError),
}

impl fmt::Display for SourceVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreRootIo { path, source } => {
                write!(
                    formatter,
                    "cannot access local store root {}: {source}",
                    path.display()
                )
            }
            Self::StoreRootNotDirectory { path } => {
                write!(
                    formatter,
                    "local store root is not a directory: {}",
                    path.display()
                )
            }
            Self::StoreRootLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "local store root must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::SourceComponentIo { path, source } => {
                write!(
                    formatter,
                    "cannot access source component {}: {source}",
                    path.display()
                )
            }
            Self::SourceParentLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "source parent must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::SourceParentNotDirectory { path } => {
                write!(
                    formatter,
                    "source parent is not a directory: {}",
                    path.display()
                )
            }
            Self::SourceLinkOrReparsePoint { path } => {
                write!(
                    formatter,
                    "source must not be a link or reparse point: {}",
                    path.display()
                )
            }
            Self::SourceNotRegular { path } => {
                write!(
                    formatter,
                    "source is not a regular file: {}",
                    path.display()
                )
            }
            Self::InvalidResolvedPath(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SourceVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StoreRootIo { source, .. } | Self::SourceComponentIo { source, .. } => {
                Some(source)
            }
            Self::InvalidResolvedPath(error) => Some(error),
            Self::StoreRootNotDirectory { .. }
            | Self::StoreRootLinkOrReparsePoint { .. }
            | Self::SourceParentLinkOrReparsePoint { .. }
            | Self::SourceParentNotDirectory { .. }
            | Self::SourceLinkOrReparsePoint { .. }
            | Self::SourceNotRegular { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(0);

    struct TestStore {
        root: PathBuf,
    }

    impl TestStore {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let unique_id = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "loadout-source-verification-test-{}-{timestamp}-{unique_id}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();

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

        fn physical_store_root(&self) -> PhysicalStoreRoot {
            resolve_store_root(&self.path("store")).unwrap()
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn source_path(raw_path: &str) -> SourceRelativePath {
        SourceRelativePath::parse(raw_path).unwrap()
    }

    #[test]
    fn verifier_returns_the_regular_file_below_the_physical_store_root() {
        let store = TestStore::new();
        store.write("store/git/config", "[user]\nname = Example\n");
        let root = store.physical_store_root();

        let verified = verify_regular_source(&root, &source_path("git/config")).unwrap();

        assert_eq!(
            verified.path().as_ref(),
            root.as_path().as_ref().join("git/config"),
            "a verified source is rooted at the physical store path"
        );
    }

    #[test]
    fn verifier_rejects_a_non_directory_parent_and_non_regular_final_entry() {
        let store = TestStore::new();
        store.write("store/not-a-directory", "contents\n");
        store.create_dir("store/a-directory");
        let root = store.physical_store_root();

        let parent_error =
            verify_regular_source(&root, &source_path("not-a-directory/config")).unwrap_err();
        assert!(matches!(
            parent_error,
            SourceVerificationError::SourceParentNotDirectory { .. }
        ));

        let final_error = verify_regular_source(&root, &source_path("a-directory")).unwrap_err();
        assert!(matches!(
            final_error,
            SourceVerificationError::SourceNotRegular { .. }
        ));
    }

    #[test]
    fn verifier_rejects_a_missing_final_source() {
        let store = TestStore::new();
        store.create_dir("store");

        let error =
            verify_regular_source(&store.physical_store_root(), &source_path("missing-file"))
                .unwrap_err();

        assert!(matches!(
            error,
            SourceVerificationError::SourceComponentIo { .. }
        ));
    }

    #[test]
    fn store_root_must_be_an_existing_directory() {
        let store = TestStore::new();
        store.write("not-a-directory", "contents\n");

        let error = resolve_store_root(&store.path("not-a-directory")).unwrap_err();

        assert!(matches!(
            error,
            SourceVerificationError::StoreRootNotDirectory { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn verifier_rejects_symlinked_source_parents_and_final_entries() {
        use std::os::unix::fs::symlink;

        let store = TestStore::new();
        store.write("outside/config", "[user]\n");
        store.create_dir("store");
        symlink(store.path("outside"), store.path("store/linked-parent")).unwrap();
        symlink(
            store.path("outside/config"),
            store.path("store/linked-file"),
        )
        .unwrap();
        let root = store.physical_store_root();

        let parent_error =
            verify_regular_source(&root, &source_path("linked-parent/config")).unwrap_err();
        assert!(matches!(
            parent_error,
            SourceVerificationError::SourceParentLinkOrReparsePoint { .. }
        ));

        let final_error = verify_regular_source(&root, &source_path("linked-file")).unwrap_err();
        assert!(matches!(
            final_error,
            SourceVerificationError::SourceLinkOrReparsePoint { .. }
        ));
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
    fn verifier_rejects_windows_junction_parents_and_file_links() {
        use std::os::windows::fs::symlink_file;

        let store = TestStore::new();
        store.write("outside/config", "[user]\n");
        store.create_dir("store");
        create_junction(&store.path("store/junction"), &store.path("outside"));
        symlink_file(
            store.path("outside/config"),
            store.path("store/linked-file"),
        )
        .expect("the Windows test runner must support file symbolic links");
        let root = store.physical_store_root();

        let junction_error =
            verify_regular_source(&root, &source_path("junction/config")).unwrap_err();
        assert!(matches!(
            junction_error,
            SourceVerificationError::SourceParentLinkOrReparsePoint { .. }
        ));

        let link_error = verify_regular_source(&root, &source_path("linked-file")).unwrap_err();
        assert!(matches!(
            link_error,
            SourceVerificationError::SourceLinkOrReparsePoint { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn verifier_uses_the_canonical_store_root_for_a_declared_store_symlink() {
        use std::os::unix::fs::symlink;

        let store = TestStore::new();
        store.write("physical-store/git/config", "[user]\n");
        symlink(store.path("physical-store"), store.path("declared-store")).unwrap();

        let root = resolve_store_root(&store.path("declared-store")).unwrap();
        let verified = verify_regular_source(&root, &source_path("git/config")).unwrap();

        assert_eq!(root.as_path().as_ref(), store.path("physical-store"));
        assert_eq!(
            verified.path().as_ref(),
            store.path("physical-store/git/config")
        );
    }
}
