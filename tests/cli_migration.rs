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
            "schema_version: 4\nid: base\nresources: {}\n",
        )
        .unwrap();
        fs::write(
            root.join("state/loadout/state.json"),
            "{\"schema_version\":4,\"resources\":{},\"active_operation\":null}",
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

    fn profile(&self, name: &str, generation: u32) {
        fs::write(
            self.root
                .join("portable/profiles")
                .join(format!("{name}.yaml")),
            format!("schema_version: {generation}\nid: {name}\nresources: {{}}\n"),
        )
        .unwrap();
    }

    fn state(&self, generation: u32) {
        let value = match generation {
            3 | 4 => format!(
                "{{\"schema_version\":{generation},\"resources\":{{}},\"active_operation\":null}}"
            ),
            5 => "{\"schema_version\":5,\"resources\":{},\"applied_lifecycle_topology\":{\"current\":[],\"historical\":[]},\"active_operation\":null}".to_owned(),
            _ => panic!("unsupported fixture generation"),
        };
        fs::write(self.root.join("state/loadout/state.json"), value).unwrap();
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

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn presentation_lists_full_write_set_and_noninteractive_decline_changes_nothing() {
    let fixture = Fixture::new();
    fixture.state(3);
    fixture.profile("base", 3);
    fixture.profile("middle", 4);
    fixture.profile("current", 5);
    let before = fixture.snapshot();
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml"]);
    let report = text(&output);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(report.contains("migration: state and profiles"), "{report}");
    assert!(report.contains("documents: 4"), "{report}");
    assert!(
        report.contains("route: 3 -> 4 -> 5 -> 6; write: yes"),
        "{report}"
    );
    assert!(
        report.contains("route: 4 -> 5 -> 6; write: yes"),
        "{report}"
    );
    assert!(report.contains("route: 5 -> 6; write: yes"), "{report}");
    assert!(report.contains("write set: 4 document(s)"), "{report}");
    assert!(
        report.contains("normal commands after migration: available"),
        "{report}"
    );
    assert!(
        report.contains("resource IDs, effects, and Known ownership do not change"),
        "{report}"
    );
    assert!(report.contains("requires --yes"), "{report}");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn one_confirmed_command_migrates_state_and_profiles_to_default_generation() {
    let fixture = Fixture::new();
    fixture.state(3);
    fixture.profile("base", 3);
    fixture.profile("middle", 4);
    let target_before = fs::read(fixture.root.join("home/managed-target")).unwrap();
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml", "--yes"]);
    let report = text(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report.contains("migration completed: schema 6; published 3 document(s)"),
        "{report}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/loadout/state.json")).unwrap())
            .unwrap();
    assert_eq!(state["schema_version"], 6);
    for name in ["base", "middle"] {
        assert!(
            fs::read_to_string(
                fixture
                    .root
                    .join("portable/profiles")
                    .join(format!("{name}.yaml"))
            )
            .unwrap()
            .contains("schema_version: 6")
        );
    }
    assert_eq!(
        fs::read(fixture.root.join("home/managed-target")).unwrap(),
        target_before
    );
    let before = fixture.snapshot();
    let no_change = fixture.run(&["migrate", "--config", "../portable/config.yaml", "--yes"]);
    assert_eq!(no_change.status.code(), Some(0), "{}", text(&no_change));
    assert!(text(&no_change).contains("published 0 document(s)"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn target_four_warns_then_target_five_completes() {
    let fixture = Fixture::new();
    fixture.state(3);
    fixture.profile("base", 3);
    let output = fixture.run(&[
        "migrate",
        "--config",
        "../portable/config.yaml",
        "--to-schema",
        "4",
        "--yes",
    ]);
    let report = text(&output);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert!(
        report
            .matches("schema-6-only readers cannot read older documents")
            .count()
            >= 2,
        "{report}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/loadout/state.json")).unwrap())
            .unwrap();
    assert_eq!(state["schema_version"], 4);
    let validate = fixture.run(&["validate", "--config", "../portable/config.yaml"]);
    assert_ne!(validate.status.code(), Some(0));
    let next = fixture.run(&[
        "migrate",
        "--config",
        "../portable/config.yaml",
        "--to-schema",
        "5",
        "--yes",
    ]);
    assert_eq!(next.status.code(), Some(0), "{}", text(&next));
}

#[test]
fn absent_state_stays_absent_and_mixed_profiles_resume() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("state/loadout/state.json")).unwrap();
    fixture.profile("base", 3);
    fixture.profile("middle", 4);
    fixture.profile("current", 5);
    let current = fs::read(fixture.root.join("portable/profiles/current.yaml")).unwrap();
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml", "--yes"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fixture.root.join("state/loadout/state.json").exists());
    assert!(!fixture.root.join("state/loadout/state.lock").exists());
    assert_ne!(
        fs::read(fixture.root.join("portable/profiles/current.yaml")).unwrap(),
        current
    );
    for name in ["base", "middle"] {
        assert!(
            fs::read_to_string(
                fixture
                    .root
                    .join("portable/profiles")
                    .join(format!("{name}.yaml"))
            )
            .unwrap()
            .contains("schema_version: 6")
        );
    }
}

#[test]
fn old_subcommands_and_unsupported_targets_are_input_errors_without_writes() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    for args in [
        vec![
            "migrate",
            "state",
            "--config",
            "../portable/config.yaml",
            "--yes",
        ],
        vec![
            "migrate",
            "profiles",
            "--config",
            "../portable/config.yaml",
            "--yes",
        ],
        vec![
            "migrate",
            "--config",
            "../portable/config.yaml",
            "--to-schema",
            "3",
            "--yes",
        ],
        vec![
            "migrate",
            "--config",
            "../portable/config.yaml",
            "--to-schema",
            "7",
            "--yes",
        ],
    ] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(2), "{}", text(&output));
        assert!(text(&output).contains("loadout migrate") || text(&output).contains("--to-schema"));
        assert_eq!(fixture.snapshot(), before);
    }
}

#[test]
fn target_four_rejects_newer_document_before_writing() {
    let fixture = Fixture::new();
    fixture.profile("new", 5);
    let before = fixture.snapshot();
    let output = fixture.run(&[
        "migrate",
        "--config",
        "../portable/config.yaml",
        "--to-schema",
        "4",
        "--yes",
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn schema_five_state_with_recorded_topology_rejects_before_publication() {
    let fixture = Fixture::new();
    fixture.state(5);
    fixture.profile("base", 5);
    let state_path = fixture.root.join("state/loadout/state.json");
    let source = fs::read_to_string(&state_path).unwrap();
    fs::write(
        &state_path,
        source.replace("\"current\":[]", "\"current\":[{}]"),
    )
    .unwrap();
    let before = fixture.snapshot();
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("applied_lifecycle_topology.current"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn presentation_identifies_broadened_order_through_empty_phase_before_confirmation() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("state/loadout/state.json")).unwrap();
    fs::create_dir(fixture.root.join("store")).unwrap();
    fs::write(
        fixture.root.join("portable/config.yaml"),
        format!(
            "schema_version: 2\ndefault_profile: root\nprofile_discovery:\n  paths: [profiles]\nstores:\n  dotfiles:\n    type: local\n    properties: {{ path: {} }}\n",
            fixture.root.join("store").display()
        ),
    )
    .unwrap();
    let resource = |name: &str, stage: &str| {
        format!(
            "  {name}:\n    type: file\n    stage: {stage}\n    properties:\n      kind: file\n      operation: link\n      source: {{ store: dotfiles, path: {name} }}\n      target: {}\n",
            fixture.root.join("home").join(name).display()
        )
    };
    for (profile, first, second) in [("first", "a", "b"), ("second", "c", "d")] {
        fs::write(
            fixture.root.join("portable/profiles").join(format!("{profile}.yaml")),
            format!(
                "schema_version: 5\nid: {profile}\nparameters: {{ slot: string }}\nstages: {{ a: {{ after: [] }}, b: {{ after: [a] }}, c: {{ after: [b] }} }}\nresources:\n{}{}",
                resource(first, "a"),
                resource(second, "c")
            ),
        )
        .unwrap();
    }
    fs::write(
        fixture.root.join("portable/profiles/base.yaml"),
        "schema_version: 5\nid: root\nincludes: [{ id: first, with: { slot: a } }, { id: second, with: { slot: b } }]\nresources: {}\n",
    )
    .unwrap();
    let before = fixture.snapshot();
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml"]);
    let report = text(&output);
    assert_eq!(output.status.code(), Some(2), "{report}");
    assert!(report.contains("broadened ordering: root root"), "{report}");
    assert!(report.contains("phase a -> b -> c"), "{report}");
    assert!(report.contains("root/first/a -> root/second/d"), "{report}");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn dual_use_stage_profile_rejects_before_any_document_publication() {
    let fixture = Fixture::new();
    fixture.state(5);
    fs::create_dir(fixture.root.join("store")).unwrap();
    fs::write(fixture.root.join("store/item"), "item").unwrap();
    fs::write(
        fixture.root.join("portable/config.yaml"),
        format!(
            "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  dotfiles:\n    type: local\n    properties: {{ path: {} }}\n",
            fixture.root.join("store").display()
        ),
    )
    .unwrap();
    fs::write(
        fixture.root.join("portable/profiles/base.yaml"),
        "schema_version: 5\nid: base\nincludes: [{ id: child }]\nresources: {}\n",
    )
    .unwrap();
    fs::write(
        fixture.root.join("portable/profiles/child.yaml"),
        format!(
            "schema_version: 5\nid: child\nstages: {{ early: {{ after: [] }} }}\nresources:\n  item:\n    type: file\n    stage: early\n    properties: {{ kind: file, operation: link, source: {{ store: dotfiles, path: item }}, target: {} }}\n",
            fixture.root.join("home/item").display()
        ),
    )
    .unwrap();
    let before = fixture.snapshot();
    let legacy = fixture.run(&[
        "migrate",
        "--config",
        "../portable/config.yaml",
        "--to-schema",
        "5",
        "--yes",
    ]);
    assert_eq!(legacy.status.code(), Some(0), "{}", text(&legacy));
    assert_eq!(fixture.snapshot(), before);
    let output = fixture.run(&["migrate", "--config", "../portable/config.yaml", "--yes"]);
    let report = text(&output);
    assert_eq!(output.status.code(), Some(1), "{report}");
    for expected in [
        "profile child",
        "stages [early]",
        "root base include path base/child",
        "Create a dedicated root profile",
    ] {
        assert!(report.contains(expected), "{report}");
    }
    assert_eq!(fixture.snapshot(), before);
}
