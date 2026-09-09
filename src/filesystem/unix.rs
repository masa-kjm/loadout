//! Unix metadata classification and lifecycle capability gates.
//! Retained-context execution primitives live in `execution`.

use std::fs;
use std::io;

pub(super) mod execution;

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;

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

pub(super) fn create_file_symbolic_link_no_replace(
    canonical_home: &ResolvedPath,
    physical_target_path: &ResolvedPath,
    link_target: &LinkTarget,
    source_root: &ResolvedPath,
) -> io::Result<()> {
    execution::ExecutionTarget::open(canonical_home, physical_target_path)?
        .prepare_create(source_root, link_target)?
        .attempt()
}

pub(super) fn replace_file_symbolic_link_from_temporary(
    _: &ResolvedPath,
    _: &ResolvedPath,
    _: &ResolvedPath,
) -> io::Result<()> {
    // Keep replacement disabled at the direct boundary until the retained-context primitive is integrated with replacement and recovery.
    Err(expected_entry_replacement_unsupported())
}

#[allow(dead_code)]
pub(super) fn remove_expected_file_symbolic_link_entry(
    _: &ResolvedPath,
    _: &ResolvedPath,
    _: &LinkTarget,
) -> io::Result<()> {
    // Keep removal disabled until the retained-context primitive is integrated with removal and recovery.
    Err(expected_entry_removal_unsupported())
}

pub(super) fn ensure_file_symbolic_link_creation_supported(_: &ResolvedPath) -> io::Result<()> {
    // Unix exposes file symbolic links as a supported platform primitive.
    // Filesystem-specific errors remain mutable facts of the actual create.
    Ok(())
}

pub(super) fn ensure_file_symbolic_link_replacement_supported(_: &ResolvedPath) -> io::Result<()> {
    // The observational contract permits name replacement, but executor integration and native action evidence are not complete yet.
    Err(expected_entry_replacement_unsupported())
}

pub(super) fn ensure_file_symbolic_link_removal_supported(_: &ResolvedPath) -> io::Result<()> {
    // The executor retains and rechecks the parent handle immediately before the name-based removal. The external-concurrency contract deliberately does not claim final-entry identity between that recheck and unlinkat.
    Ok(())
}

fn expected_entry_replacement_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix file-link replacement is unavailable pending retained-context executor integration and native action evidence",
    )
}

#[allow(dead_code)]
fn expected_entry_removal_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix file-link removal is unavailable pending retained-context executor integration and native action evidence",
    )
}
