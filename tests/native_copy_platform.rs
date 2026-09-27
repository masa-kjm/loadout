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
