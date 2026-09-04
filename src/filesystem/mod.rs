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
    platform::create_file_symbolic_link_no_replace(
        canonical_home,
        physical_target_path,
        link_target,
    )
}

/// Removes one final file symbolic-link entry only when the platform can bind the removal to the expected link entry. The executor establishes the expected-link ownership precondition, and this primitive must retain that proof through the mutation boundary rather than deleting by a subsequently resolved name.
pub(crate) fn remove_expected_file_symbolic_link_entry(
    canonical_home: &ResolvedPath,
    physical_target_path: &ResolvedPath,
    expected_link_target: &LinkTarget,
) -> io::Result<()> {
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

/// Rejects a file-link removal when the platform cannot bind the final expected entry to its deletion while retaining no-follow handling through the mutation boundary.
pub(crate) fn ensure_file_symbolic_link_removal_supported(
    target_parent: &ResolvedPath,
) -> io::Result<()> {
    platform::ensure_file_symbolic_link_removal_supported(target_parent)
}
