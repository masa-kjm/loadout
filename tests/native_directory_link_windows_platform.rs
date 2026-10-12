//! Native Windows/NTFS conformance evidence for the DirectoryLink lifecycle.

#![cfg(windows)]

use std::{
    fs::{self, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetVolumeInformationByHandleW,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::var_os("LOADOUT_NATIVE_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "loadout-native-directory-link-windows-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&root).unwrap();
        for directory in [
            "home/.config",
            "config/loadout",
            "portable/profiles",
            "store",
            "cwd",
            "state",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let fixture = Self { root };
        fixture.write(
            "portable/config.yaml",
            "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store\n",
        );
        fixture
    }

    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path.replace('/', "\\"))
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn profile(&self, resource: &str, source: &str, target: &str) {
        self.write(
            "portable/profiles/base.yaml",
            &format!(
                "schema_version: 3\nid: base\nresources:\n  {resource}:\n    type: file\n    properties:\n      kind: directory\n      operation: link\n      source:\n        store: files\n        path: {source}\n      target: ~/{target}\n"
            ),
        );
    }

    fn apply(&self) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
        command.current_dir(self.path("cwd"));
        command
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("APPDATA", self.path("config"))
            .env("LOCALAPPDATA", self.path("state"));
        command
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("state/loadout/state.json")).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn expect_success(output: Output) {
    assert_eq!(output.status.code(), Some(0), "{}", output_text(&output));
}

fn create_junction(target: &Path, source: &Path) {
    let command = format!(
        "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        target.display(),
        source.display()
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &command])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{command}: {}",
        output_text(&output)
    );
}

fn directory_link_replacement_guarantee_available(parent: &Path) -> bool {
    let parent = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(parent)
        .unwrap();
    let mut filesystem_name = [0_u16; 32];
    // SAFETY: the retained directory handle and output buffer remain valid for the synchronous query.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            parent.as_raw_handle() as _,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        )
    };
    result != 0
        && filesystem_name
            .iter()
            .copied()
            .take_while(|unit| *unit != 0)
            .eq("NTFS".encode_utf16())
}

#[test]
fn directory_link_lifecycle_rejects_junctions_and_preserves_old_effect_on_sharing_denial() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("store/old")).unwrap();
    fs::create_dir(fixture.path("store/new")).unwrap();
    fs::write(fixture.path("store/old/sentinel"), b"old referent").unwrap();
    fs::write(fixture.path("store/new/sentinel"), b"new referent").unwrap();

    let capability_probe = fixture.path("home/.config/capability-probe");
    if let Err(error) =
        std::os::windows::fs::symlink_dir(fixture.path("store/old"), &capability_probe)
    {
        assert!(fs::symlink_metadata(&capability_probe).is_err());
        fixture.profile("nvim", "old", ".config/nvim");
        let output = fixture.apply();
        assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
        assert!(
            output_text(&output).contains("Preflight"),
            "{}",
            output_text(&output)
        );
        assert!(fs::symlink_metadata(fixture.path("home/.config/nvim")).is_err());
        assert!(
            !fixture.path("state/loadout/state.json").exists(),
            "capability preflight rejection must not create state or an operation record"
        );
        eprintln!("directory symbolic-link creation unavailable: {error}");
        return;
    }
    fs::remove_dir(&capability_probe).unwrap();

    let junction = fixture.path("home/.config/junction");
    create_junction(&junction, &fixture.path("store/old"));
    fixture.profile("nvim", "old", ".config/junction");
    let output = fixture.apply();
    assert_ne!(output.status.code(), Some(0), "{}", output_text(&output));
    assert_eq!(fs::read_dir(&junction).unwrap().count(), 1);
    assert_eq!(
        fs::read(fixture.path("store/old/sentinel")).unwrap(),
        b"old referent"
    );
    assert!(!fixture.path("state/loadout/state.json").exists());
    fs::remove_dir(&junction).unwrap();

    fixture.profile("nvim", "old", ".config/nvim");
    expect_success(fixture.apply());
    let target = fixture.path("home/.config/nvim");
    let metadata = fs::symlink_metadata(&target).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(fs::read_link(&target).unwrap(), fixture.path("store/old"));

    fixture.profile("nvim", "new", ".config/nvim");
    let known_before = fs::read(fixture.path("state/loadout/state.json")).unwrap();
    if !directory_link_replacement_guarantee_available(&fixture.path("home/.config")) {
        let output = fixture.apply();
        assert_eq!(output.status.code(), Some(2), "{}", output_text(&output));
        assert!(output_text(&output).contains("Preflight"));
        assert_eq!(fs::read_link(&target).unwrap(), fixture.path("store/old"));
        assert_eq!(
            fs::read(fixture.path("state/loadout/state.json")).unwrap(),
            known_before
        );
        eprintln!("directory-link replacement guarantee unavailable");
        return;
    }

    let held_target = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&target)
        .unwrap();
    let output = fixture.apply();
    assert_ne!(output.status.code(), Some(0), "{}", output_text(&output));
    assert_eq!(fs::read_link(&target).unwrap(), fixture.path("store/old"));
    assert_eq!(
        fs::read(fixture.path("store/old/sentinel")).unwrap(),
        b"old referent"
    );
    assert_eq!(
        fixture.state()["resources"]["base/nvim"]["effect"]["link_target"],
        fixture.path("store/old").to_string_lossy().as_ref()
    );
    drop(held_target);

    expect_success(fixture.apply());
    assert_eq!(fs::read_link(&target).unwrap(), fixture.path("store/new"));
    assert_eq!(
        fs::read(fixture.path("store/old/sentinel")).unwrap(),
        b"old referent"
    );
    assert_eq!(
        fs::read(fixture.path("store/new/sentinel")).unwrap(),
        b"new referent"
    );

    fixture.write(
        "portable/profiles/base.yaml",
        "schema_version: 3\nid: base\nresources: {}\n",
    );
    expect_success(fixture.apply());
    assert!(fs::symlink_metadata(&target).is_err());
    assert_eq!(
        fs::read(fixture.path("store/new/sentinel")).unwrap(),
        b"new referent"
    );
    assert!(fixture.path("home/.config").is_dir());
}
