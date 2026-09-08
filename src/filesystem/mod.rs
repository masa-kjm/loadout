//! Platform-specific, no-follow filesystem primitives.
//!
//! This module reports entry-kind facts only. Callers decide whether an entry is safe, owned, or eligible for a lifecycle action.

use std::fs;
use std::io;

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;

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
) -> io::Result<()> {
    #[cfg(test)]
    crate::test_support::assert_mutation_allowed();
    platform::create_file_symbolic_link_no_replace(
        canonical_home,
        physical_target_path,
        link_target,
    )
}

/// Replaces the expected target with its recorded sibling only when the backend can retain both entry proofs through atomic replacement. The primitive must reject unsupported guarantees even if a caller bypasses capability preflight.
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

/// Removes one final file symbolic-link entry only when the platform can bind the removal to the expected link entry. The executor establishes the expected-link ownership precondition, and this primitive must retain that proof through the mutation boundary rather than deleting by a subsequently resolved name.
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

/// Rejects a file-link removal when the platform cannot bind the final expected entry to its deletion while retaining no-follow handling through the mutation boundary.
pub(crate) fn ensure_file_symbolic_link_removal_supported(
    target_parent: &ResolvedPath,
) -> io::Result<()> {
    platform::ensure_file_symbolic_link_removal_supported(target_parent)
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
    fn unsupported_destructive_primitives_protect_substituted_entries_even_without_preflight() {
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
            create_file_symbolic_link_no_replace(&f.root(), &target, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert!(fs::symlink_metadata(target.as_ref()).is_err());
        fs::write(target.as_ref(), "unmanaged").unwrap();
        assert_eq!(
            create_file_symbolic_link_no_replace(&f.root(), &target, &source)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(fs::read_to_string(target.as_ref()).unwrap(), "unmanaged");
    }
}
