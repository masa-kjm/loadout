//! Native Unix filesystem evidence for the DirectoryLink primitive contract.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    fs,
    os::unix::{fs::symlink, io::AsFd},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};

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
            "loadout-native-directory-link-platform-{}-{}",
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

#[test]
fn directory_link_create_and_no_follow_inspection_preserve_the_referent_and_reject_a_symlinked_parent()
 {
    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    let parent = fixture.path().join("parent");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sentinel"), b"directory referent").unwrap();
    fs::create_dir(&parent).unwrap();
    let target = parent.join("target");
    symlink(&source, &target).unwrap();

    let metadata = fs::symlink_metadata(&target).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(fs::read_link(&target).unwrap(), source);
    assert_eq!(
        fs::read(source.join("sentinel")).unwrap(),
        b"directory referent"
    );

    let unsafe_parent = fixture.path().join("unsafe-parent");
    symlink(&parent, &unsafe_parent).unwrap();
    let root = fs::File::open(fixture.path()).unwrap();
    assert!(
        openat(
            root.as_fd(),
            "unsafe-parent",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .is_err()
    );
    assert!(fs::symlink_metadata(unsafe_parent.join("new-target")).is_err());
    assert_eq!(
        fs::read(source.join("sentinel")).unwrap(),
        b"directory referent"
    );
}

#[test]
fn atomic_replacement_switches_only_the_directory_link_entry() {
    let fixture = Fixture::new();
    let old_source = fixture.path().join("old-source");
    let new_source = fixture.path().join("new-source");
    let parent_path = fixture.path().join("parent");
    fs::create_dir(&old_source).unwrap();
    fs::create_dir(&new_source).unwrap();
    fs::write(old_source.join("sentinel"), b"old referent").unwrap();
    fs::write(new_source.join("sentinel"), b"new referent").unwrap();
    fs::create_dir(&parent_path).unwrap();
    symlink(&old_source, parent_path.join("target")).unwrap();
    symlink(&new_source, parent_path.join(".target.loadout-tmp")).unwrap();

    let parent = fs::File::open(&parent_path).unwrap();
    renameat(
        parent.as_fd(),
        ".target.loadout-tmp",
        parent.as_fd(),
        "target",
    )
    .unwrap();

    assert_eq!(
        fs::read_link(parent_path.join("target")).unwrap(),
        new_source
    );
    assert!(fs::symlink_metadata(parent_path.join(".target.loadout-tmp")).is_err());
    assert_eq!(
        fs::read(old_source.join("sentinel")).unwrap(),
        b"old referent"
    );
    assert_eq!(
        fs::read(new_source.join("sentinel")).unwrap(),
        b"new referent"
    );
}

#[test]
fn removal_unlinks_only_the_directory_link_entry() {
    let fixture = Fixture::new();
    let source = fixture.path().join("source");
    let parent_path = fixture.path().join("parent");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("sentinel"), b"directory referent").unwrap();
    fs::create_dir(&parent_path).unwrap();
    symlink(&source, parent_path.join("target")).unwrap();

    let parent = fs::File::open(&parent_path).unwrap();
    unlinkat(parent.as_fd(), "target", AtFlags::empty()).unwrap();

    assert!(fs::symlink_metadata(parent_path.join("target")).is_err());
    assert_eq!(
        fs::read(source.join("sentinel")).unwrap(),
        b"directory referent"
    );
    assert!(parent_path.is_dir());
}
