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
            "loadout-cli-config-use-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for directory in ["home", "portable/profiles", "store", "cwd", "state"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("store/source"), "source").unwrap();
        fs::write(
            root.join("portable/config.yaml"),
            "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store\n",
        )
        .unwrap();
        fs::write(
            root.join("portable/profiles/base.yaml"),
            "schema_version: 1\nid: base\nresources: {}\n",
        )
        .unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
        command.current_dir(self.root.join("cwd"));
        command
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("runtime-config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("APPDATA", self.root.join("runtime-config"))
            .env("LOCALAPPDATA", self.root.join("state"));
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(path: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            if metadata.file_type().is_symlink() {
                output.insert(
                    path.to_owned(),
                    format!("link:{:?}", fs::read_link(path).unwrap()).into_bytes(),
                );
            } else if metadata.is_dir() {
                output.insert(path.to_owned(), b"directory".to_vec());
                for entry in fs::read_dir(path).unwrap() {
                    visit(&entry.unwrap().path(), output);
                }
            } else {
                output.insert(path.to_owned(), fs::read(path).unwrap());
            }
        }
        let mut snapshot = BTreeMap::new();
        visit(&self.root, &mut snapshot);
        snapshot
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
fn use_creates_runtime_selection_from_a_cwd_relative_validated_config() {
    let fixture = Fixture::new();
    let output = fixture.run(&["config", "use", "../portable/config.yaml", "--yes"]);

    assert!(output.status.success(), "{}", text(&output));
    let selected = normal_existing_path(fixture.root.join("portable/config.yaml"));
    assert_eq!(
        fs::read_to_string(fixture.root.join("runtime-config/loadout/loadout.yaml")).unwrap(),
        format!("schema_version: 1\nconfig_path: {}\n", selected.display())
    );
    assert!(!fixture.root.join("state/loadout").exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("store/source")).unwrap(),
        "source"
    );
}

#[test]
fn use_replaces_a_valid_runtime_configuration_without_editing_portable_config() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("runtime-config/loadout")).unwrap();
    fs::write(
        fixture.root.join("runtime-config/loadout/loadout.yaml"),
        "schema_version: 1\nconfig_path: /previous/config.yaml\n",
    )
    .unwrap();
    let portable_before = fs::read(fixture.root.join("portable/config.yaml")).unwrap();

    let output = fixture.run(&["config", "use", "../portable/config.yaml", "--yes"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(text(&output).contains("prior config_path: /previous/config.yaml"));
    assert_eq!(
        fs::read(fixture.root.join("portable/config.yaml")).unwrap(),
        portable_before
    );
}

#[test]
fn use_without_yes_in_noninteractive_mode_does_not_write() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();

    let output = fixture.run(&["config", "use", "../portable/config.yaml"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(text(&output).contains("requires --yes"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn invalid_portable_configuration_is_rejected_before_runtime_parent_creation() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("portable/config.yaml"), "invalid").unwrap();
    let before = fixture.snapshot();

    let output = fixture.run(&["config", "use", "../portable/config.yaml", "--yes"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn invalid_existing_runtime_configuration_is_rejected_without_mutation() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("runtime-config/loadout")).unwrap();
    fs::write(
        fixture.root.join("runtime-config/loadout/loadout.yaml"),
        "schema_version: 2\n",
    )
    .unwrap();
    let before = fixture.snapshot();

    let output = fixture.run(&["config", "use", "../portable/config.yaml", "--yes"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert_eq!(fixture.snapshot(), before);
}

#[cfg(unix)]
#[test]
fn use_rejects_a_symlinked_runtime_parent_without_touching_the_referent() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let external = fixture.root.join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("loadout.yaml"), "preserve me").unwrap();
    fs::create_dir(fixture.root.join("runtime-config")).unwrap();
    symlink(&external, fixture.root.join("runtime-config/loadout")).unwrap();
    let before = fixture.snapshot();

    let output = fixture.run(&["config", "use", "../portable/config.yaml", "--yes"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        fs::read_to_string(external.join("loadout.yaml")).unwrap(),
        "preserve me"
    );
}
