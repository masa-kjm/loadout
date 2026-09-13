//! Windows no-follow metadata classification and file-link creation.

#[allow(dead_code)]
// S6 wires this retained context into Windows executor actions after action-specific integration evidence.
pub(super) mod execution;

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};

use crate::domain::file_link::LinkTarget;
use crate::domain::paths::ResolvedPath;

use super::NoFollowEntryKind;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

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

#[allow(dead_code)] // S6 execution context consumes tag-based classification after native traversal evidence.
fn classify_reparse_tag(tag: u32, is_directory: bool) -> NoFollowEntryKind {
    match (tag, is_directory) {
        (IO_REPARSE_TAG_SYMLINK, false) => NoFollowEntryKind::FileSymbolicLink,
        (IO_REPARSE_TAG_MOUNT_POINT, _) | (IO_REPARSE_TAG_SYMLINK, true) => {
            NoFollowEntryKind::ReparsePoint
        }
        _ => NoFollowEntryKind::ReparsePoint,
    }
}

/// Reads the reparse tag from an already-opened no-follow final-entry handle without authorizing mutation.
#[allow(dead_code)]
fn reparse_tag_from_no_follow_handle(file: &fs::File) -> io::Result<u32> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    let mut buffer = [0_u8; 16 * 1024];
    let mut returned = 0_u32;
    // SAFETY: the handle belongs to `file` and the output buffer is writable for this synchronous call.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    if returned < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "reparse data lacks a tag",
        ));
    }
    Ok(u32::from_le_bytes(buffer[..4].try_into().unwrap()))
}

/// Opens one validated child of `parent` without following that child if it is a reparse point, and the returned handle owns the child for the next traversal or action-specific recheck.
#[allow(dead_code)] // S6 execution context consumes retained handles after native action evidence.
fn open_relative_no_follow(
    parent: &fs::File,
    component: &OsStr,
    expect_directory: bool,
) -> io::Result<fs::File> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN;
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;

    let file = open_relative_no_follow_with_disposition(
        parent,
        component,
        expect_directory,
        FILE_OPEN,
        FILE_GENERIC_READ,
    )?;
    if expect_directory {
        require_plain_directory(&file)?;
    }
    Ok(file)
}

/// Verifies that a retained traversal component is a normal directory rather than a reparse point.
fn require_plain_directory(file: &fs::File) -> io::Result<()> {
    let metadata = file.metadata()?;
    let attributes = metadata.file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory traversal component is a reparse point",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory traversal component is not a directory",
        ));
    }
    Ok(())
}

fn open_relative_no_follow_with_disposition(
    parent: &fs::File,
    component: &OsStr,
    expect_directory: bool,
    disposition: u32,
    desired_access: u32,
) -> io::Result<fs::File> {
    use std::mem::size_of;
    use std::ptr;

    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{HANDLE, RtlNtStatusToDosError, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut name = component.encode_wide().collect::<Vec<_>>();
    if !is_single_relative_component(&name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows retained-parent traversal requires one normal path component",
        ));
    }
    let byte_len = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let mut unicode = UNICODE_STRING {
        Length: byte_len as u16,
        MaximumLength: byte_len as u16,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &mut unicode,
        Attributes: 0,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let options = FILE_OPEN_REPARSE_POINT
        | if expect_directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    let mut handle: HANDLE = ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: all inputs are valid for this synchronous call, `parent` remains open, and a successful call returns a distinct owned file handle.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut status,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            disposition,
            options,
            ptr::null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: RtlNtStatusToDosError only converts the returned NTSTATUS.
        return Err(io::Error::from_raw_os_error(unsafe {
            RtlNtStatusToDosError(result) as i32
        }));
    }
    // SAFETY: NtCreateFile returned a new owned file handle on success.
    Ok(unsafe { fs::File::from_raw_handle(handle) })
}

fn is_single_relative_component(component: &[u16]) -> bool {
    !component.is_empty()
        && component != [b'.' as u16]
        && component != [b'.' as u16, b'.' as u16]
        && !component
            .iter()
            .any(|unit| *unit == 0 || matches!(*unit, 0x2F | 0x5C | 0x3A))
}

/// Creates one missing final non-directory entry relative to its retained parent.
fn create_relative_file_no_replace(parent: &fs::File, component: &OsStr) -> io::Result<fs::File> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_CREATE;
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE};

    open_relative_no_follow_with_disposition(
        parent,
        component,
        false,
        FILE_CREATE,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
    )
}

/// Opens a final non-directory entry relative to its retained parent with delete access.
/// The caller must verify the exact no-follow link predicate before consuming this handle.
fn open_relative_file_for_delete(parent: &fs::File, component: &OsStr) -> io::Result<fs::File> {
    use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN;
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_READ};

    open_relative_no_follow_with_disposition(
        parent,
        component,
        false,
        FILE_OPEN,
        FILE_GENERIC_READ | DELETE,
    )
}

/// Marks an already checked final entry for deletion when its owned handle closes.
fn mark_file_for_deletion_on_close(file: &fs::File) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the retained handle belongs to `file` and `disposition` remains valid for this synchronous call.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets an absolute file-symbolic-link reparse value on a newly created file handle.
fn set_file_symbolic_link_reparse_point(
    file: &fs::File,
    target: &std::path::Path,
) -> io::Result<()> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

    let buffer = file_symbolic_link_reparse_buffer(target)?;
    let mut returned = 0_u32;
    // SAFETY: the file handle remains open and `buffer` is immutable for this synchronous call.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_REPARSE_POINT,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
fn invalid_reparse_setup_error_for_prototype(file: &fs::File) -> io::Result<()> {
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

    let invalid_buffer = [0_u8; 8];
    let mut returned = 0_u32;
    // SAFETY: the file handle remains open and the deliberately invalid input buffer is valid for this synchronous call.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_REPARSE_POINT,
            invalid_buffer.as_ptr().cast(),
            invalid_buffer.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Err(io::Error::other(
            "an invalid reparse buffer unexpectedly succeeded",
        ));
    }
    Err(io::Error::last_os_error())
}

/// Renames an owned sibling entry to one normal final component beneath the retained parent.
fn rename_relative_from_handle(
    file: &fs::File,
    parent: &fs::File,
    component: &OsStr,
    replace_if_exists: bool,
) -> io::Result<()> {
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformationEx, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let name = component.encode_wide().collect::<Vec<_>>();
    if !is_single_relative_component(&name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows retained-parent rename requires one normal path component",
        ));
    }
    let name_length = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .filter(|length| *length <= u32::MAX as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let root_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, RootDirectory);
    let length_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileNameLength);
    let name_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    let mut buffer = vec![0_u8; name_offset + name_length];
    buffer[0] = u8::from(replace_if_exists);
    buffer[root_offset..root_offset + std::mem::size_of::<usize>()]
        .copy_from_slice(&(parent.as_raw_handle() as usize).to_ne_bytes());
    buffer[length_offset..length_offset + std::mem::size_of::<u32>()]
        .copy_from_slice(&(name_length as u32).to_ne_bytes());
    for (index, unit) in name.iter().enumerate() {
        let offset = name_offset + index * std::mem::size_of::<u16>();
        buffer[offset..offset + std::mem::size_of::<u16>()].copy_from_slice(&unit.to_ne_bytes());
    }
    let mut status = IO_STATUS_BLOCK::default();
    // SAFETY: the buffer follows the binding's field offsets, and the retained handles and buffer remain valid for this synchronous call.
    let result = unsafe {
        NtSetInformationFile(
            file.as_raw_handle() as _,
            &mut status,
            buffer.as_ptr().cast(),
            buffer.len() as u32,
            FileRenameInformationEx,
        )
    };
    if result < 0 {
        // SAFETY: RtlNtStatusToDosError only converts the returned NTSTATUS.
        return Err(io::Error::from_raw_os_error(unsafe {
            RtlNtStatusToDosError(result) as i32
        }));
    }
    Ok(())
}

fn file_symbolic_link_reparse_buffer(target: &std::path::Path) -> io::Result<Vec<u8>> {
    let print_name = target.as_os_str().encode_wide().collect::<Vec<_>>();
    if !target.is_absolute() || print_name.is_empty() || print_name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symbolic link requires a non-empty absolute target without NUL",
        ));
    }
    let mut substitute_name = if print_name.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        r"\??\UNC\".encode_utf16().collect::<Vec<_>>()
    } else {
        r"\??\".encode_utf16().collect::<Vec<_>>()
    };
    substitute_name.extend_from_slice(if print_name.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        &print_name[2..]
    } else {
        &print_name
    });
    let substitute_length = utf16_byte_length(&substitute_name)?;
    let print_length = utf16_byte_length(&print_name)?;
    let path_length = substitute_length.checked_add(print_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "symbolic-link target is too long",
        )
    })?;
    let reparse_data_length = 12_u16.checked_add(path_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "symbolic-link target is too long",
        )
    })?;
    let mut buffer = Vec::with_capacity(8 + reparse_data_length as usize);
    buffer.extend_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
    buffer.extend_from_slice(&reparse_data_length.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    buffer.extend_from_slice(&substitute_length.to_le_bytes());
    buffer.extend_from_slice(&substitute_length.to_le_bytes());
    buffer.extend_from_slice(&print_length.to_le_bytes());
    buffer.extend_from_slice(&0_u32.to_le_bytes());
    for unit in substitute_name.into_iter().chain(print_name) {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(buffer)
}

fn utf16_byte_length(value: &[u16]) -> io::Result<u16> {
    value
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .filter(|length| *length <= u16::MAX as usize)
        .map(|length| length as u16)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbolic-link target is too long",
            )
        })
}

#[cfg(test)]
mod reparse_tag_tests {
    use super::*;

    #[test]
    fn reparse_tags_accept_only_file_symbolic_links() {
        assert_eq!(
            classify_reparse_tag(IO_REPARSE_TAG_SYMLINK, false),
            NoFollowEntryKind::FileSymbolicLink
        );
        for (tag, directory) in [
            (IO_REPARSE_TAG_SYMLINK, true),
            (IO_REPARSE_TAG_MOUNT_POINT, false),
            (IO_REPARSE_TAG_MOUNT_POINT, true),
            (0xDEAD_BEEF, false),
        ] {
            assert_eq!(
                classify_reparse_tag(tag, directory),
                NoFollowEntryKind::ReparsePoint
            );
        }
    }
}

#[allow(dead_code)] // S6 keeps the direct primitive fail-closed while executor uses retained tokens.
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

#[allow(dead_code)] // S6 keeps the direct primitive fail-closed while executor uses retained tokens.
pub(super) fn remove_expected_file_symbolic_link_entry(
    _: &ResolvedPath,
    _: &ResolvedPath,
    _: &LinkTarget,
) -> std::io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows file-link removal requires a no-follow parent traversal primitive",
    ))
}

#[allow(dead_code)] // S6 keeps the direct primitive fail-closed while executor uses retained tokens.
pub(super) fn replace_file_symbolic_link_from_temporary(
    _: &ResolvedPath,
    _: &ResolvedPath,
    _: &ResolvedPath,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows file-link replacement requires a no-follow parent traversal primitive",
    ))
}

pub(super) fn ensure_file_symbolic_link_creation_supported(_: &ResolvedPath) -> io::Result<()> {
    // Execution uses the retained-parent primitive rather than a path-based symbolic-link call.
    Ok(())
}

pub(super) fn ensure_file_symbolic_link_replacement_supported(_: &ResolvedPath) -> io::Result<()> {
    // Execution uses an owned temporary handle and a retained parent for the rename.
    Ok(())
}

pub(super) fn ensure_file_symbolic_link_removal_supported(_: &ResolvedPath) -> io::Result<()> {
    // Execution uses the checked final-entry handle and never a path-based deletion.
    Ok(())
}

#[cfg(test)]
mod retained_parent_tests {
    use std::ffi::OsStr;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::fs::symlink_file;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    use super::*;

    #[test]
    fn nt_create_file_opens_a_final_component_relative_to_a_retained_parent_handle() {
        let root = fixture_root("relative");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("parent")).unwrap();
        fs::write(root.join("parent/entry"), "fixture").unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let parent_handle =
            open_relative_no_follow(&root_handle, OsStr::new("parent"), true).unwrap();
        let handle = open_relative_no_follow(&parent_handle, OsStr::new("entry"), false).unwrap();
        assert!(
            open_relative_no_follow(&parent_handle, OsStr::new("missing"), false).is_err(),
            "FILE_OPEN must not create an entry"
        );
        assert!(!root.join("parent/missing").exists());
        drop(handle);
        drop(parent_handle);
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_traversal_rejects_non_components_before_an_nt_call() {
        let root = fixture_root("component");
        fs::create_dir(&root).unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        for component in ["", ".", "..", "child/name", "child\\name", "C:child"] {
            let error =
                open_relative_no_follow(&root_handle, OsStr::new(component), false).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        drop(root_handle);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn retained_parent_handle_can_become_detached_from_the_declared_parent_path() {
        let root = fixture_root("detached-parent");
        let declared_parent = root.join("parent");
        let relocated_parent = root.join("relocated");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&declared_parent).unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let parent_handle =
            open_relative_no_follow(&root_handle, OsStr::new("parent"), true).unwrap();
        fs::rename(&declared_parent, &relocated_parent).unwrap();
        fs::create_dir(&declared_parent).unwrap();
        let detached_entry =
            create_relative_file_no_replace(&parent_handle, OsStr::new("entry")).unwrap();
        drop(detached_entry);
        assert!(
            relocated_parent.join("entry").is_file(),
            "the retained handle remains bound to the moved directory object"
        );
        assert!(
            !declared_parent.join("entry").exists(),
            "a handle-local result does not establish the declared-path postcondition"
        );
        drop(parent_handle);
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_follow_handle_reports_a_file_symbolic_link_tag() {
        let root = fixture_root("reparse-tag");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source"), "fixture").unwrap();
        if let Err(error) = symlink_file(root.join("source"), root.join("link")) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("file symbolic-link creation is unavailable for this test token");
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("could not create file symbolic-link fixture: {error}");
        }
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let link_handle = open_relative_no_follow(&root_handle, OsStr::new("link"), false).unwrap();
        assert_eq!(
            reparse_tag_from_no_follow_handle(&link_handle).unwrap(),
            IO_REPARSE_TAG_SYMLINK
        );
        drop(link_handle);
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_create_sets_a_file_symbolic_link_without_replacing_an_entry() {
        let root = fixture_root("create");
        let source = root.join("source");
        let link = root.join("link");
        fs::create_dir(&root).unwrap();
        fs::write(&source, "fixture").unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let link_handle =
            create_relative_file_no_replace(&root_handle, OsStr::new("link")).unwrap();
        assert!(
            create_relative_file_no_replace(&root_handle, OsStr::new("link")).is_err(),
            "FILE_CREATE must not replace an existing entry"
        );
        if let Err(error) = set_file_symbolic_link_reparse_point(&link_handle, &source) {
            drop(link_handle);
            drop(root_handle);
            if error.raw_os_error() == Some(1314) {
                eprintln!(
                    "setting a symbolic-link reparse point is unavailable for this test token"
                );
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("could not set file symbolic-link reparse point: {error}");
        }
        assert_eq!(
            reparse_tag_from_no_follow_handle(&link_handle).unwrap(),
            IO_REPARSE_TAG_SYMLINK
        );
        drop(link_handle);
        drop(root_handle);
        assert_eq!(fs::read_link(&link).unwrap(), source);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_create_cleans_up_after_reparse_setup_failure() {
        let root = fixture_root("create-cleanup");
        let link = root.join("link");
        fs::create_dir(&root).unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let link_handle =
            create_relative_file_no_replace(&root_handle, OsStr::new("link")).unwrap();
        let setup_error = invalid_reparse_setup_error_for_prototype(&link_handle).unwrap_err();
        assert!(
            setup_error.raw_os_error().is_some(),
            "the deliberately invalid reparse buffer must be rejected by Windows"
        );
        mark_file_for_deletion_on_close(&link_handle).unwrap();
        drop(link_handle);
        assert!(
            matches!(fs::symlink_metadata(&link), Err(error) if error.kind() == io::ErrorKind::NotFound),
            "the retained create handle must clean up its own failed entry"
        );
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_rename_replaces_only_when_requested_and_preserves_the_old_link_on_failure() {
        let root = fixture_root("rename");
        let old_source = root.join("old-source");
        let new_source = root.join("new-source");
        let target = root.join("target");
        let temporary = root.join("temporary");
        fs::create_dir(&root).unwrap();
        fs::write(&old_source, "old").unwrap();
        fs::write(&new_source, "new").unwrap();
        if let Err(error) = symlink_file(&old_source, &target) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("file symbolic-link creation is unavailable for this test token");
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("could not create old file symbolic-link fixture: {error}");
        }
        symlink_file(&new_source, &temporary).unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let temporary_handle =
            open_relative_file_for_delete(&root_handle, OsStr::new("temporary")).unwrap();
        assert!(
            rename_relative_from_handle(
                &temporary_handle,
                &root_handle,
                OsStr::new("target"),
                false,
            )
            .is_err(),
            "a no-replace rename must reject an existing target"
        );
        assert_eq!(fs::read_link(&target).unwrap(), old_source);
        assert_eq!(fs::read_link(&temporary).unwrap(), new_source);
        rename_relative_from_handle(&temporary_handle, &root_handle, OsStr::new("target"), true)
            .unwrap();
        drop(temporary_handle);
        assert_eq!(fs::read_link(&target).unwrap(), new_source);
        assert!(
            matches!(fs::symlink_metadata(&temporary), Err(error) if error.kind() == io::ErrorKind::NotFound),
            "a successful same-parent rename must remove the temporary name"
        );
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_delete_removes_only_the_opened_file_symbolic_link() {
        let root = fixture_root("delete");
        let source = root.join("source");
        let link = root.join("link");
        fs::create_dir(&root).unwrap();
        fs::write(&source, "fixture").unwrap();
        if let Err(error) = symlink_file(&source, &link) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("file symbolic-link creation is unavailable for this test token");
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("could not create file symbolic-link fixture: {error}");
        }
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let link_handle = open_relative_file_for_delete(&root_handle, OsStr::new("link")).unwrap();
        assert_eq!(
            reparse_tag_from_no_follow_handle(&link_handle).unwrap(),
            IO_REPARSE_TAG_SYMLINK
        );
        mark_file_for_deletion_on_close(&link_handle).unwrap();
        drop(link_handle);
        assert!(
            matches!(fs::symlink_metadata(&link), Err(error) if error.kind() == io::ErrorKind::NotFound),
            "closing a delete-marked file-link handle must leave the declared entry missing"
        );
        assert!(
            source.is_file(),
            "deleting the link must not delete its source"
        );
        drop(root_handle);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_parent_delete_open_is_rejected_by_a_target_sharing_denial() {
        let root = fixture_root("delete-sharing-denial");
        let source = root.join("source");
        let link = root.join("link");
        fs::create_dir(&root).unwrap();
        fs::write(&source, "fixture").unwrap();
        if let Err(error) = symlink_file(&source, &link) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("file symbolic-link creation is unavailable for this test token");
                fs::remove_dir_all(root).unwrap();
                return;
            }
            panic!("could not create file symbolic-link fixture: {error}");
        }
        let sharing_guard = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&link)
            .unwrap();
        let root_handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let error = open_relative_file_for_delete(&root_handle, OsStr::new("link")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(32));
        assert_eq!(fs::read_link(&link).unwrap(), source);
        assert!(source.is_file());
        drop(root_handle);
        drop(sharing_guard);
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("loadout-s6-{label}-{}-{nanos}", std::process::id()))
    }
}
