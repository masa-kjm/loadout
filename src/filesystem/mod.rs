//! Platform-specific, no-follow filesystem primitives.
//!
//! This module reports entry-kind facts only. Callers decide whether an entry is safe, owned, or eligible for a lifecycle action.

use std::fs;

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
