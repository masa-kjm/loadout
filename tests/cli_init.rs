use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "loadout-cli-init-test-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir_all(root.join("runtime-config/loadout")).unwrap();
        fs::create_dir_all(root.join("runtime-state/loadout")).unwrap();
        fs::create_dir(root.join("native-assets")).unwrap();
        fs::write(root.join("native-assets/example"), "native asset").unwrap();
        fs::write(
            root.join("runtime-config/loadout/loadout.yaml"),
            "schema_version: 1\n",
        )
        .unwrap();
        fs::write(root.join("runtime-state/loadout/state.json"), "state").unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
        command
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("runtime-config"))
            .env("XDG_STATE_HOME", self.root.join("runtime-state"));
        command
    }
}

fn snapshot(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    snapshot_into(root, root, &mut entries);
    entries.sort();
    entries
}

fn snapshot_into(
    root: &std::path::Path,
    directory: &std::path::Path,
    entries: &mut Vec<(PathBuf, Vec<u8>)>,
) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            entries.push((relative, b"directory".to_vec()));
            snapshot_into(root, &path, entries);
        } else {
            entries.push((relative, fs::read(&path).unwrap()));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn init_creates_a_bundle_that_validate_can_select_explicitly() {
    let fixture = Fixture::new();
    let output = fixture.command().arg("init").output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        snapshot(&fixture.root.join(".loadout")),
        vec![
            (PathBuf::from("config.yaml"), b"schema_version: 1\ndefault_profile: base\n\nprofile_discovery:\n  paths:\n    - ./profiles\n\nstores:\n  native:\n    type: local\n    path: ..\n".to_vec()),
            (PathBuf::from("profiles"), b"directory".to_vec()),
            (PathBuf::from("profiles/base.yaml"), b"schema_version: 1\nid: base\nresources: {}\n".to_vec()),
        ]
    );

    let validation = fixture
        .command()
        .args(["validate", "--config", ".loadout/config.yaml"])
        .output()
        .unwrap();
    assert!(validation.status.success(), "{validation:?}");
}

#[test]
fn init_dry_run_and_existing_entry_rejection_leave_the_directory_unchanged() {
    let fixture = Fixture::new();
    let before_dry_run = snapshot(&fixture.root);
    let dry_run = fixture
        .command()
        .args(["init", "--dry-run"])
        .output()
        .unwrap();
    assert!(dry_run.status.success(), "{dry_run:?}");
    assert_eq!(snapshot(&fixture.root), before_dry_run);

    let existing = fixture.root.join(".loadout");
    fs::write(&existing, "user-owned").unwrap();
    let before_rejection = snapshot(&fixture.root);
    let rejected = fixture.command().arg("init").output().unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(snapshot(&fixture.root), before_rejection);
}
