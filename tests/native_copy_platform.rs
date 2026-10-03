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

#[cfg(target_os = "linux")]
#[test]
fn retained_parent_no_replace_publishes_exact_bytes_to_a_missing_final_name() {
    use std::fs::{self, File};

    use rustix::fs::{RenameFlags, renameat_with};

    let fixture = Fixture::new();
    let parent = File::open(fixture.path()).unwrap();
    fs::write(fixture.path().join("source"), b"exact source bytes").unwrap();

    renameat_with(&parent, "source", &parent, "target", RenameFlags::NOREPLACE).unwrap();

    assert!(fs::symlink_metadata(fixture.path().join("source")).is_err());
    assert_eq!(
        fs::read(fixture.path().join("target")).unwrap(),
        b"exact source bytes"
    );
}

// This partial primitive spike covers selected native primitive outcomes only.
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
        path::{Path, PathBuf},
        process::Command,
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
            Foundation::{
                ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_LOCK_VIOLATION, HANDLE,
                RtlNtStatusToDosError, UNICODE_STRING,
            },
            Storage::FileSystem::{
                DELETE, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
                LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
            },
            System::IO::{IO_STATUS_BLOCK, OVERLAPPED},
        },
    };

    use super::Fixture;

    struct DenyAddFileAcl {
        path: PathBuf,
        principal: String,
        installed: bool,
    }

    impl DenyAddFileAcl {
        fn install(path: &Path) -> Self {
            let principal_output = Command::new("whoami.exe")
                .output()
                .expect("resolve the current Windows principal");
            assert!(principal_output.status.success());
            let principal = String::from_utf8(principal_output.stdout)
                .expect("current Windows principal is UTF-8")
                .trim()
                .to_owned();
            assert!(!principal.is_empty());

            let status = Command::new("icacls.exe")
                .arg(path)
                .arg("/deny")
                .arg(format!("{principal}:(WD)"))
                .arg("/c")
                .status()
                .expect("install disposable fixture ACL");
            assert!(status.success());

            Self {
                path: path.to_owned(),
                principal,
                installed: true,
            }
        }

        fn remove(&mut self) {
            if !self.installed {
                return;
            }
            let status = Command::new("icacls.exe")
                .arg(&self.path)
                .arg("/remove:d")
                .arg(&self.principal)
                .arg("/c")
                .status()
                .expect("restore disposable fixture ACL");
            self.installed = false;
            assert!(status.success());
        }
    }

    impl Drop for DenyAddFileAcl {
        fn drop(&mut self) {
            if self.installed {
                let _ = Command::new("icacls.exe")
                    .arg(&self.path)
                    .arg("/remove:d")
                    .arg(&self.principal)
                    .arg("/c")
                    .status();
            }
        }
    }

    fn lock_byte(path: &Path, offset: u32) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .expect("open disposable fixture file for byte-range lock");
        let mut overlapped = OVERLAPPED::default();
        // This initializes the synchronous byte-range lock offset.
        overlapped.Anonymous.Anonymous.Offset = offset;
        // SAFETY: the lock is synchronous and the file remains open until the caller releases it.
        assert_ne!(
            unsafe {
                LockFileEx(
                    file.as_raw_handle() as HANDLE,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            },
            0
        );
        file
    }

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
    fn direct_and_staged_create_preserve_partial_artifacts_after_locked_write_failure() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());

        let direct_prefix = b"direct prefix";
        let mut direct = create_relative_no_replace(&parent, "direct-final").unwrap();
        direct.write_all(direct_prefix).unwrap();
        direct.sync_all().unwrap();
        let direct_lock = lock_byte(
            &fixture.path().join("direct-final"),
            direct_prefix.len() as u32,
        );
        let direct_error = direct.write_all(b" remainder").unwrap_err();
        assert_eq!(
            direct_error.raw_os_error(),
            Some(ERROR_LOCK_VIOLATION as i32)
        );
        drop(direct_lock);
        assert_eq!(
            fs::read(fixture.path().join("direct-final")).unwrap(),
            direct_prefix
        );
        drop(direct);

        let staged_prefix = b"staged prefix";
        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(staged_prefix).unwrap();
        temporary.sync_all().unwrap();
        let temporary_lock = lock_byte(
            &fixture.path().join("temporary"),
            staged_prefix.len() as u32,
        );
        let staged_error = temporary.write_all(b" remainder").unwrap_err();
        assert_eq!(
            staged_error.raw_os_error(),
            Some(ERROR_LOCK_VIOLATION as i32)
        );
        drop(temporary_lock);
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            staged_prefix
        );
        assert!(!fixture.path().join("target").exists());
        drop(temporary);
    }

    #[test]
    fn candidates_observe_locked_read_failure_during_verification() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());

        let mut direct = create_relative_no_replace(&parent, "direct-final").unwrap();
        direct.write_all(b"planned direct bytes").unwrap();
        direct.sync_all().unwrap();
        let direct_lock = lock_byte(&fixture.path().join("direct-final"), 0);
        let direct_read = fs::read(fixture.path().join("direct-final")).unwrap_err();
        assert_eq!(
            direct_read.raw_os_error(),
            Some(ERROR_LOCK_VIOLATION as i32)
        );
        drop(direct_lock);
        assert_eq!(
            fs::read(fixture.path().join("direct-final")).unwrap(),
            b"planned direct bytes"
        );
        drop(direct);

        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"planned staged bytes").unwrap();
        temporary.sync_all().unwrap();
        let temporary_lock = lock_byte(&fixture.path().join("temporary"), 0);
        let temporary_read = fs::read(fixture.path().join("temporary")).unwrap_err();
        assert_eq!(
            temporary_read.raw_os_error(),
            Some(ERROR_LOCK_VIOLATION as i32)
        );
        assert!(!fixture.path().join("target").exists());
        drop(temporary_lock);
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"planned staged bytes"
        );
        drop(temporary);
    }

    #[test]
    fn candidates_observe_missing_names_after_another_operation_removes_them() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());

        let mut direct = create_relative_no_replace(&parent, "direct-final").unwrap();
        direct.write_all(b"planned direct bytes").unwrap();
        direct.sync_all().unwrap();
        fs::remove_file(fixture.path().join("direct-final")).unwrap();
        let direct_observation = fs::read(fixture.path().join("direct-final")).unwrap_err();
        assert_eq!(direct_observation.kind(), io::ErrorKind::NotFound);
        drop(direct);

        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"planned staged bytes").unwrap();
        temporary.sync_all().unwrap();
        fs::remove_file(fixture.path().join("temporary")).unwrap();
        let publication = rename_relative_no_replace(&temporary, &parent, "target").unwrap_err();
        assert!(publication.raw_os_error().is_some());
        assert!(!fixture.path().join("temporary").exists());
        assert!(!fixture.path().join("target").exists());
        drop(temporary);
    }

    #[test]
    fn candidates_observe_byte_changes_through_another_handle_before_verification_or_publication() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());

        let mut direct = create_relative_no_replace(&parent, "direct-final").unwrap();
        direct.write_all(b"planned direct bytes").unwrap();
        direct.sync_all().unwrap();
        let mut external_direct = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(fixture.path().join("direct-final"))
            .unwrap();
        external_direct.set_len(0).unwrap();
        external_direct.write_all(b"external direct bytes").unwrap();
        external_direct.sync_all().unwrap();
        drop(external_direct);
        assert_ne!(
            fs::read(fixture.path().join("direct-final")).unwrap(),
            b"planned direct bytes"
        );
        assert_eq!(
            fs::read(fixture.path().join("direct-final")).unwrap(),
            b"external direct bytes"
        );
        drop(direct);

        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"planned staged bytes").unwrap();
        temporary.sync_all().unwrap();
        let mut external_temporary = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(fixture.path().join("temporary"))
            .unwrap();
        external_temporary.set_len(0).unwrap();
        external_temporary
            .write_all(b"external staged bytes")
            .unwrap();
        external_temporary.sync_all().unwrap();
        drop(external_temporary);
        assert_ne!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"planned staged bytes"
        );
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"external staged bytes"
        );
        assert!(!fixture.path().join("target").exists());
        drop(temporary);
    }

    #[test]
    fn candidates_preserve_missing_names_when_parent_acl_denies_add_file() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());
        let mut acl = DenyAddFileAcl::install(fixture.path());

        let direct = create_relative_no_replace(&parent, "direct-final").unwrap_err();
        assert_eq!(direct.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert!(!fixture.path().join("direct-final").exists());

        let staged = create_relative_no_replace(&parent, "temporary").unwrap_err();
        assert_eq!(staged.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert!(!fixture.path().join("temporary").exists());

        acl.remove();
    }

    #[test]
    fn temporary_publication_preserves_temporary_when_parent_acl_denies_add_file() {
        let fixture = Fixture::new();
        let parent = retained_parent(fixture.path());
        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"staged candidate bytes").unwrap();
        temporary.sync_all().unwrap();
        let mut acl = DenyAddFileAcl::install(fixture.path());

        let publication = rename_relative_no_replace(&temporary, &parent, "target").unwrap_err();
        assert_eq!(publication.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"staged candidate bytes"
        );
        assert!(!fixture.path().join("target").exists());

        acl.remove();
        drop(temporary);
    }

    #[test]
    fn candidates_reject_a_file_reparse_target_without_changing_it() {
        use std::os::windows::fs::symlink_file;

        let fixture = Fixture::new();
        let target = fixture.path().join("target");
        let referent = fixture.path().join("referent");
        fs::write(&referent, b"referent bytes").unwrap();
        symlink_file(&referent, &target)
            .expect("native fixture requires file symbolic-link support");
        let parent = retained_parent(fixture.path());

        let direct = create_relative_no_replace(&parent, "target").unwrap_err();
        assert!(direct.raw_os_error().is_some());
        assert_eq!(fs::read_link(&target).unwrap(), referent);
        assert_eq!(fs::read(&referent).unwrap(), b"referent bytes");

        let mut temporary = create_relative_no_replace(&parent, "temporary").unwrap();
        temporary.write_all(b"staged candidate bytes").unwrap();
        temporary.sync_all().unwrap();
        let staged = rename_relative_no_replace(&temporary, &parent, "target").unwrap_err();
        assert!(staged.raw_os_error().is_some());
        assert_eq!(fs::read_link(&target).unwrap(), referent);
        assert_eq!(fs::read(&referent).unwrap(), b"referent bytes");
        assert_eq!(
            fs::read(fixture.path().join("temporary")).unwrap(),
            b"staged candidate bytes"
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
