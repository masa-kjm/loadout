//! Unix no-follow metadata classification.

use std::fs;

use super::NoFollowEntryKind;

pub(super) fn classify_nofollow_entry(metadata: &fs::Metadata) -> NoFollowEntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        NoFollowEntryKind::FileSymbolicLink
    } else if file_type.is_file() {
        NoFollowEntryKind::RegularFile
    } else if file_type.is_dir() {
        NoFollowEntryKind::Directory
    } else {
        NoFollowEntryKind::Unsupported
    }
}
