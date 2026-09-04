//! Windows no-follow metadata classification and file-link creation.

use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;

use super::NoFollowEntryKind;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

pub(super) fn classify_nofollow_entry(metadata: &fs::Metadata) -> NoFollowEntryKind {
    let file_type = metadata.file_type();
    let attributes = metadata.file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        if file_type.is_symlink() && attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            NoFollowEntryKind::FileSymbolicLink
        } else {
            // Metadata exposes the reparse attribute but not a stable tag.
            // Conservatively reject every non-file-link reparse point.
            NoFollowEntryKind::ReparsePoint
        }
    } else if file_type.is_file() {
        NoFollowEntryKind::RegularFile
    } else if file_type.is_dir() {
        NoFollowEntryKind::Directory
    } else {
        NoFollowEntryKind::Unsupported
    }
}

pub(super) fn create_file_symbolic_link_no_replace(
    _: &ResolvedPath,
    _: &ResolvedPath,
    _: &LinkTarget,
) -> std::io::Result<()> {
    // Keep the primitive fail-closed as well as the executor preflight. This prevents a future direct caller from reintroducing name-based traversal through a parent reparse point before no-follow traversal is available.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows file-link creation requires a no-follow parent traversal primitive",
    ))
}

pub(super) fn ensure_file_symbolic_link_creation_supported(_: &ResolvedPath) -> io::Result<()> {
    // `symlink_file` resolves parents by name. A junction or another reparse point can replace a checked parent before that call, so it cannot retain physical containment through the mutation. Keep Windows create fail-closed until this boundary has a no-follow parent traversal primitive.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows file-link creation requires a no-follow parent traversal primitive",
    ))
}
