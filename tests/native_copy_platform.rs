//! Native API probes retained as CI evidence for file-copy capability decisions.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::var_os("LOADOUT_NATIVE_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = root.join(format!(
            "loadout-native-copy-platform-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn retained_parent_no_replace_rejects_an_existing_final_name() {
    use std::fs::{self, File};

    use rustix::fs::{RenameFlags, renameat_with};

    let fixture = Fixture::new();
    let parent = File::open(fixture.path()).unwrap();
    fs::write(fixture.path().join("source"), b"source bytes").unwrap();
    fs::write(fixture.path().join("target"), b"existing target bytes").unwrap();

    let error =
        renameat_with(&parent, "source", &parent, "target", RenameFlags::NOREPLACE).unwrap_err();

    assert_eq!(error, rustix::io::Errno::EXIST);
    assert_eq!(
        fs::read(fixture.path().join("source")).unwrap(),
        b"source bytes"
    );
    assert_eq!(
        fs::read(fixture.path().join("target")).unwrap(),
        b"existing target bytes"
    );
}

// This partial primitive spike covers only success and collision aftermath.
// It must not be used to select a candidate or enable Windows copy execution before the remaining Phase 2 matrix is proven natively.
#[cfg(windows)]
mod retained_parent_copy_candidates {
    use std::{
        ffi::OsStr,
        fs::{self, File, OpenOptions},
        io::{self, Write},
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::Path,
        ptr,
    };

    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
                FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation,
                NtCreateFile, NtSetInformationFile,
            },
        },
        Win32::{
            Foundation::{ERROR_ALREADY_EXISTS, HANDLE, RtlNtStatusToDosError, UNICODE_STRING},
            Storage::FileSystem::{
                DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    use super::Fixture;

    fn retained_parent(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .expect("open disposable fixture parent")
    }

    fn create_relative_no_replace(parent: &File, component: &str) -> io::Result<File> {
        let mut name = OsStr::new(component).encode_wide().collect::<Vec<_>>();
        let bytes = name
            .len()
            .checked_mul(size_of::<u16>())
            .filter(|length| *length <= u16::MAX as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "component is too long"))?;
        let mut unicode = UNICODE_STRING {
            Length: bytes as u16,
            MaximumLength: bytes as u16,
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
        let mut handle: HANDLE = ptr::null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the retained parent, UTF-16 component, and synchronous output pointers remain valid for the call.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE
                    | DELETE,
                &attributes,
                &mut status,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                ptr::null(),
                0,
            )
        };
        if result < 0 {
            // SAFETY: this converts only the returned NTSTATUS value.
            return Err(io::Error::from_raw_os_error(unsafe {
                RtlNtStatusToDosError(result) as i32
            }));
        }
        // SAFETY: a successful NtCreateFile returns one owned handle.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn rename_relative_no_replace(file: &File, parent: &File, component: &str) -> io::Result<()> {
        let name = OsStr::new(component).encode_wide().collect::<Vec<_>>();
        let name_length = name
            .len()
            .checked_mul(size_of::<u16>())
            .filter(|length| *length <= u32::MAX as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "component is too long"))?;
        let root_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, RootDirectory);
        let length_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileNameLength);
        let name_offset = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
        let mut buffer = vec![0_u8; name_offset + name_length];
        buffer[root_offset..root_offset + size_of::<usize>()]
            .copy_from_slice(&(parent.as_raw_handle() as usize).to_ne_bytes());
        buffer[length_offset..length_offset + size_of::<u32>()]
            .copy_from_slice(&(name_length as u32).to_ne_bytes());
        for (index, unit) in name.iter().enumerate() {
            let offset = name_offset + index * size_of::<u16>();
            buffer[offset..offset + size_of::<u16>()].copy_from_slice(&unit.to_ne_bytes());
        }
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the FILE_RENAME_INFORMATION-compatible buffer and retained handles remain valid for this synchronous call.
        let result = unsafe {
            NtSetInformationFile(
                file.as_raw_handle() as HANDLE,
                &mut status,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                FileRenameInformation,
            )
        };
        if result < 0 {
            // SAFETY: this converts only the returned NTSTATUS value.
            return Err(io::Error::from_raw_os_error(unsafe {
                RtlNtStatusToDosError(result) as i32
            }));
        }
        Ok(())
    }

    #[test]
    fn direct_exclusive_create_preserves_collision_and_publishes_exact_bytes() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());
        fs::write(fixture.path().join("existing"), b"external bytes").unwrap();

        let collision = create_relative_no_replace(&parent, "existing").unwrap_err();
        assert!(collision.raw_os_error().is_some());
        assert_eq!(
            fs::read(fixture.path().join("existing")).unwrap(),
            b"external bytes"
        );

        let mut created = create_relative_no_replace(&parent, "created").unwrap();
        created.write_all(b"exact candidate bytes").unwrap();
        created.sync_all().unwrap();
        drop(created);
        assert_eq!(
            fs::read(fixture.path().join("created")).unwrap(),
            b"exact candidate bytes"
        );

        let mut partial = create_relative_no_replace(&parent, "partial").unwrap();
        partial.write_all(b"incomplete candidate bytes").unwrap();
        drop(partial);
        assert_eq!(
            fs::read(fixture.path().join("partial")).unwrap(),
            b"incomplete candidate bytes"
        );
    }

    #[test]
    fn temporary_publication_uses_handle_relative_no_replace_and_preserves_collision() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());
        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"staged candidate bytes").unwrap();
        temporary.sync_all().unwrap();
        fs::write(fixture.path().join("target"), b"external bytes").unwrap();

        let collision = rename_relative_no_replace(&temporary, &parent, "target").unwrap_err();
        assert_eq!(collision.raw_os_error(), Some(ERROR_ALREADY_EXISTS as i32));
        assert_eq!(
            fs::read(fixture.path().join("target")).unwrap(),
            b"external bytes"
        );
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"staged candidate bytes"
        );
        drop(temporary);

        let mut temporary = create_relative_no_replace(&parent, "publish").unwrap();
        temporary.write_all(b"published candidate bytes").unwrap();
        temporary.sync_all().unwrap();
        rename_relative_no_replace(&temporary, &parent, "published").unwrap();
        drop(temporary);
        assert_eq!(
            fs::read(fixture.path().join("published")).unwrap(),
            b"published candidate bytes"
        );
        assert!(!fixture.path().join("publish").exists());
    }
}

#[cfg(windows)]
#[test]
fn move_file_ex_without_replace_rejects_an_existing_final_name() {
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, GetLastError},
        Storage::FileSystem::MoveFileExW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    fs::write(&source, b"source bytes").unwrap();
    fs::write(&target, b"existing target bytes").unwrap();
    let source_wide = wide(&source);
    let target_wide = wide(&target);

    assert_eq!(
        unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), 0) },
        0
    );
    let error = unsafe { GetLastError() };
    assert!(matches!(error, ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS));
    assert_eq!(fs::read(&source).unwrap(), b"source bytes");
    assert_eq!(fs::read(&target).unwrap(), b"existing target bytes");
}
