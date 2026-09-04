//! Windows no-follow metadata classification.

use std::fs;
use std::os::windows::fs::MetadataExt;

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
