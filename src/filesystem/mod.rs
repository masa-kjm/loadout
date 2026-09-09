//! Platform-specific, no-follow filesystem primitives.
//!
//! This module reports physical entry and path-association facts. Callers decide ownership and lifecycle eligibility; execution primitives enforce the supplied physical predicates.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::{ResolvedPath, ResolvedPathError};

#[cfg(unix)]
pub(crate) use unix::execution::ExecutionTarget;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(unix, windows)))]
compile_error!("Loadout v0.2 supports only Unix and Windows filesystems");

/// The no-follow kind of one filesystem entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NoFollowEntryKind {
    FileSymbolicLink,
    RegularFile,
    Directory,
    #[cfg_attr(unix, allow(dead_code))]
    ReparsePoint,
    Unsupported,
}

impl NoFollowEntryKind {
    /// Whether source or discovery traversal must reject this entry.
    pub(crate) fn is_link_or_reparse_point(self) -> bool {
        matches!(self, Self::FileSymbolicLink | Self::ReparsePoint)
    }
}

/// Classifies metadata obtained with a no-follow observation.
pub(crate) fn classify_nofollow_entry(metadata: &fs::Metadata) -> NoFollowEntryKind {
    platform::classify_nofollow_entry(metadata)
}

/// Whether metadata identifies an entry that discovery and source verification
/// must not traverse.
pub(crate) fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    classify_nofollow_entry(metadata).is_link_or_reparse_point()
}

/// Creates one file symbolic-link entry without replacing an existing final target. The executor owns all lifecycle and ownership decisions.
pub(crate) fn create_file_symbolic_link_no_replace(
    canonical_home: &ResolvedPath,
    physical_target_path: &ResolvedPath,
    link_target: &LinkTarget,
    _source_root: &ResolvedPath,
) -> io::Result<()> {
    #[cfg(test)]
    crate::test_support::assert_mutation_allowed();
    platform::create_file_symbolic_link_no_replace(
        canonical_home,
        physical_target_path,
        link_target,
        #[cfg(unix)]
        _source_root,
    )
}

/// Replaces a rechecked target with its recorded sibling under the observational concurrency contract. Currently disabled pending action integration and native evidence; direct calls also reject.
pub(crate) fn replace_file_symbolic_link_from_temporary(
    canonical_home: &ResolvedPath,
    physical_target_path: &ResolvedPath,
    physical_temporary_path: &ResolvedPath,
) -> io::Result<()> {
    #[cfg(test)]
    crate::test_support::assert_mutation_allowed();
    platform::replace_file_symbolic_link_from_temporary(
        canonical_home,
        physical_target_path,
        physical_temporary_path,
    )
}

/// Removes a freshly rechecked expected link by name under the observational concurrency contract. Currently disabled pending retained-context action integration and native evidence; no atomic entry-identity guarantee is claimed.
#[cfg_attr(unix, allow(dead_code))]
pub(crate) fn remove_expected_file_symbolic_link_entry(
    canonical_home: &ResolvedPath,
    physical_target_path: &ResolvedPath,
    expected_link_target: &LinkTarget,
) -> io::Result<()> {
    #[cfg(test)]
    crate::test_support::assert_mutation_allowed();
    platform::remove_expected_file_symbolic_link_entry(
        canonical_home,
        physical_target_path,
        expected_link_target,
    )
}

/// Rejects a file-link create when the platform cannot prove that it can create the required symbolic-link representation without weakening no-follow safety.
///
/// This deliberately does not attempt a permission probe. Permission and sharing can change after preflight and must still be classified from the post-mutation observation if an actual create attempt is denied.
pub(crate) fn ensure_file_symbolic_link_creation_supported(
    target_parent: &ResolvedPath,
) -> io::Result<()> {
    platform::ensure_file_symbolic_link_creation_supported(target_parent)
}

/// Rejects replacement when the platform cannot atomically replace one name with a sibling while preserving the old name on failure.
pub(crate) fn ensure_file_symbolic_link_replacement_supported(
    target_parent: &ResolvedPath,
) -> io::Result<()> {
    platform::ensure_file_symbolic_link_replacement_supported(target_parent)
}

/// Rejects removal until the executor integrates retained-parent rechecks, no-follow removal and recorded-path observations with native action evidence.
pub(crate) fn ensure_file_symbolic_link_removal_supported(
    target_parent: &ResolvedPath,
) -> io::Result<()> {
    platform::ensure_file_symbolic_link_removal_supported(target_parent)
}

pub(crate) fn normalize_observed_absolute_path(
    path: &Path,
) -> Result<ResolvedPath, ResolvedPathError> {
    if !path.is_absolute() {
        return ResolvedPath::new(path.to_path_buf());
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(component) => normalized.push(component),
        }
    }

    ResolvedPath::new(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loadout-primitive-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("source"), "source referent").unwrap();
            Self(root)
        }
        fn path(&self, name: &str) -> ResolvedPath {
            ResolvedPath::new(self.0.join(name)).unwrap()
        }
        fn root(&self) -> ResolvedPath {
            ResolvedPath::new(self.0.clone()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unavailable_direct_destructive_primitives_protect_substituted_entries() {
        let f = Fixture::new();
        let root = f.root();
        let target = f.path("target");
        let temporary = f.path("temporary");
        let expected = LinkTarget::new(f.path("source"));
        // On Unix, explicitly substitute a regular file after observing the old link.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(expected.as_path().as_ref(), target.as_ref()).unwrap();
            assert_eq!(
                classify_nofollow_entry(&fs::symlink_metadata(target.as_ref()).unwrap()),
                NoFollowEntryKind::FileSymbolicLink
            );
            fs::remove_file(target.as_ref()).unwrap();
        }
        fs::write(target.as_ref(), "substituted unmanaged target").unwrap();
        fs::write(temporary.as_ref(), "substituted unmanaged temporary").unwrap();
        assert_eq!(
            ensure_file_symbolic_link_replacement_supported(&root)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        #[cfg(unix)]
        ensure_file_symbolic_link_removal_supported(&root).unwrap();
        #[cfg(windows)]
        assert_eq!(
            ensure_file_symbolic_link_removal_supported(&root)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            replace_file_symbolic_link_from_temporary(&root, &target, &temporary)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        for path in [&target, &temporary] {
            assert_eq!(
                remove_expected_file_symbolic_link_entry(&root, path, &expected)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported
            );
        }
        assert_eq!(
            fs::read_to_string(target.as_ref()).unwrap(),
            "substituted unmanaged target"
        );
        assert_eq!(
            fs::read_to_string(temporary.as_ref()).unwrap(),
            "substituted unmanaged temporary"
        );
        assert_eq!(
            fs::read_to_string(expected.as_path().as_ref()).unwrap(),
            "source referent"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_create_boundary_rejects_without_touching_missing_or_existing_targets() {
        let f = Fixture::new();
        let target = f.path("target");
        let source = LinkTarget::new(f.path("source"));
        assert_eq!(
            ensure_file_symbolic_link_creation_supported(&f.root())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            create_file_symbolic_link_no_replace(&f.root(), &target, &source, &f.root())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(fs::symlink_metadata(target.as_ref()).is_err());
        fs::write(target.as_ref(), "unmanaged").unwrap();
        assert_eq!(
            create_file_symbolic_link_no_replace(&f.root(), &target, &source, &f.root())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read_to_string(target.as_ref()).unwrap(), "unmanaged");
    }
}
