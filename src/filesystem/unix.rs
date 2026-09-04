//! Unix no-follow metadata classification and file-link creation.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

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
) -> io::Result<()> {
    let relative_target = physical_target_path
        .as_ref()
        .strip_prefix(canonical_home.as_ref())
        .map_err(|_| invalid_input("target is not below the canonical home root"))?;
    let components = relative_target
        .components()
        .map(|component| match component {
            Component::Normal(component) => Ok(component),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => Err(invalid_input(
                "target has an invalid canonical-home-relative component",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (final_component, parent_components) = components
        .split_last()
        .ok_or_else(|| invalid_input("target must not equal the canonical home root"))?;

    let mut parent = open_directory(canonical_home.as_ref())?;
    for component in parent_components {
        parent = open_directory_at(parent.as_raw_fd(), component)?;
    }

    let source = c_string(link_target.as_path().as_ref())?;
    let entry = c_string(Path::new(final_component))?;
    let result = unsafe { libc::symlinkat(source.as_ptr(), parent.as_raw_fd(), entry.as_ptr()) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn ensure_file_symbolic_link_creation_supported(_: &ResolvedPath) -> io::Result<()> {
    // Unix exposes file symbolic links as a supported platform primitive.
    // Filesystem-specific errors remain mutable facts of the actual create.
    Ok(())
}

fn open_directory(path: &Path) -> io::Result<fs::File> {
    let path = c_string(path)?;
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    file_from_descriptor(descriptor)
}

fn open_directory_at(
    parent_descriptor: std::os::fd::RawFd,
    component: &std::ffi::OsStr,
) -> io::Result<fs::File> {
    let component = c_string(Path::new(component))?;
    let descriptor = unsafe {
        libc::openat(
            parent_descriptor,
            component.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    file_from_descriptor(descriptor)
}

fn file_from_descriptor(descriptor: libc::c_int) -> io::Result<fs::File> {
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { fs::File::from_raw_fd(descriptor) })
    }
}

fn c_string(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| invalid_input("path contains an interior NUL byte"))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
