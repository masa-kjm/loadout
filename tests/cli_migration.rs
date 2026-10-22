use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
fn normal_existing_path(path: PathBuf) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap();
    let raw = canonical.to_str().unwrap();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else {
        PathBuf::from(raw.strip_prefix(r"\\?\").unwrap_or(raw))
    }
}

#[cfg(not(windows))]
fn normal_existing_path(path: PathBuf) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "loadout-cli-migration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = normal_existing_path(root);
        for directory in ["cwd", "home", "portable/profiles", "state/loadout"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(
            root.join("portable/config.yaml"),
            "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores: {}\n",
        )
        .unwrap();
        fs::write(
            root.join("portable/profiles/base.yaml"),
            "schema_version: 3\nid: base\nresources: {}\n",
        )
        .unwrap();
        fs::write(
            root.join("state/loadout/state.json"),
            "{\"schema_version\":3,\"resources\":{},\"active_operation\":null}",
        )
        .unwrap();
        fs::write(root.join("home/managed-target"), "unchanged").unwrap();
        Self { root }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_loadout"))
            .current_dir(self.root.join("cwd"))
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("runtime-config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("APPDATA", self.root.join("runtime-config"))
            .env("LOCALAPPDATA", self.root.join("state"))
            .args(arguments)
            .output()
            .unwrap()
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut snapshot = BTreeMap::new();
        snapshot_into(&self.root, &self.root, &mut snapshot);
        snapshot
    }
}

fn snapshot_into(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        snapshot.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            b"directory".to_vec(),
        );
        for entry in fs::read_dir(path).unwrap() {
            snapshot_into(root, &entry.unwrap().path(), snapshot);
        }
    } else {
        snapshot.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            fs::read(path).unwrap(),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn state_migration_without_yes_presents_the_boundary_and_leaves_everything_unchanged() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();

    let output = fixture.run(&["migrate", "state", "--config", "../portable/config.yaml"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.status.code(), Some(2), "{text}");
    assert!(text.contains("migration: state"), "{text}");
    assert!(text.contains("affected documents: 1"), "{text}");
    let document = format!(
        "document: {} (schema 3 -> 4)",
        fixture
            .root
            .join("state")
            .join("loadout")
            .join("state.json")
            .display()
    );
    assert!(text.contains(&document), "expected {document:?} in {text}");
    assert!(text.contains("schema versions: 3 -> 4"), "{text}");
    assert!(text.contains("resource IDs, effects, and Known ownership do not change"));
    assert!(text.contains("requires --yes"), "{text}");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn profile_migration_confirmation_lists_each_exact_schema_three_document() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("portable/profiles/extra.yaml"),
        "schema_version: 3\nid: extra\nresources: {}\n",
    )
    .unwrap();
    let before = fixture.snapshot();

    let output = fixture.run(&["migrate", "profiles", "--config", "../portable/config.yaml"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.status.code(), Some(2), "{text}");
    assert!(text.contains("migration: profiles"), "{text}");
    assert!(text.contains("affected documents: 2"), "{text}");
    for profile in ["base.yaml", "extra.yaml"] {
        let document = format!(
            "document: {} (schema 3 -> 4)",
            fixture
                .root
                .join("portable")
                .join("profiles")
                .join(profile)
                .display()
        );
        assert!(text.contains(&document), "expected {document:?} in {text}");
    }
    assert!(text.contains("required next step:"), "{text}");
    assert!(text.contains("resource IDs, effects, and Known ownership do not change"));
    assert!(text.contains("requires --yes"), "{text}");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn profiles_before_the_state_barrier_fail_without_publishing_a_profile() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();

    let output = fixture.run(&[
        "migrate",
        "profiles",
        "--config",
        "../portable/config.yaml",
        "--yes",
    ]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("migration: profiles"), "{text}");
    assert!(text.contains("migration failed:"), "{text}");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn confirmed_state_then_profile_migration_only_changes_schema_versions() {
    let fixture = Fixture::new();
    let target_before = fs::read(fixture.root.join("home/managed-target")).unwrap();

    let state = fixture.run(&[
        "migrate",
        "state",
        "--config",
        "../portable/config.yaml",
        "--yes",
    ]);
    let state_text = format!(
        "{}{}",
        String::from_utf8_lossy(&state.stdout),
        String::from_utf8_lossy(&state.stderr)
    );
    assert_eq!(state.status.code(), Some(0), "{state_text}");
    assert!(
        state_text.contains("migration completed: state"),
        "{state_text}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(fixture.root.join("state/loadout/state.json")).unwrap(),
        )
        .unwrap(),
        serde_json::json!({"schema_version": 4, "resources": {}, "active_operation": null})
    );

    let profiles = fixture.run(&[
        "migrate",
        "profiles",
        "--config",
        "../portable/config.yaml",
        "--yes",
    ]);
    let profiles_text = format!(
        "{}{}",
        String::from_utf8_lossy(&profiles.stdout),
        String::from_utf8_lossy(&profiles.stderr)
    );
    assert_eq!(profiles.status.code(), Some(0), "{profiles_text}");
    assert!(
        profiles_text.contains("migration completed: profiles"),
        "{profiles_text}"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("portable/profiles/base.yaml")).unwrap(),
        "schema_version: 4\nid: base\nresources: {}\n"
    );
    assert_eq!(
        fs::read(fixture.root.join("home/managed-target")).unwrap(),
        target_before
    );
}

#[test]
fn confirmed_profile_migration_resumes_remaining_schema_three_documents_without_rewriting_schema_four()
 {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("portable/profiles/extra.yaml"),
        "schema_version: 3\nid: extra\nresources: {}\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("portable/profiles/base.yaml"),
        "# already published\nschema_version: 4\nid: base\nresources: {}\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("state/loadout/state.json"),
        "{\"schema_version\":4,\"resources\":{},\"active_operation\":null}",
    )
    .unwrap();
    let published_before = fs::read(fixture.root.join("portable/profiles/base.yaml")).unwrap();

    let output = fixture.run(&[
        "migrate",
        "profiles",
        "--config",
        "../portable/config.yaml",
        "--yes",
    ]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.status.code(), Some(0), "{text}");
    assert_eq!(
        fs::read(fixture.root.join("portable/profiles/base.yaml")).unwrap(),
        published_before
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("portable/profiles/extra.yaml")).unwrap(),
        "schema_version: 4\nid: extra\nresources: {}\n"
    );
}
