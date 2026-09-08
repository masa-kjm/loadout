//! v0.2 binary acceptance in disposable machine, configuration, source and state trees.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "loadout-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let fixture = Self { root };
        for dir in [
            "home",
            "config/loadout",
            "portable/profiles",
            "store",
            "cwd",
            "state",
        ] {
            fs::create_dir_all(fixture.path(dir)).unwrap();
        }
        fixture.write("store/source", "content\n");
        fixture.environment(Some("base"));
        fixture.profile("base", "item", "~/target");
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    fn write(&self, name: &str, text: &str) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn environment(&self, default: Option<&str>) {
        let default = default
            .map(|id| format!("default_profile: {id}\n"))
            .unwrap_or_default();
        self.write("portable/config.yaml", &format!("schema_version: 1\n{default}profile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    path: ../store\n"));
    }
    fn profile(&self, id: &str, resource: &str, target: &str) {
        self.write(&format!("portable/profiles/{id}.yaml"), &format!("schema_version: 1\nid: {id}\nresources:\n  {resource}:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: {target}\n"));
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
        command.current_dir(self.path("cwd"));
        // Keep platform process-launch essentials while overriding every application location.
        command
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("APPDATA", self.path("config"))
            .env("LOCALAPPDATA", self.path("state"));
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        let before = self.snapshot();
        let output = self.command().args(args).output().unwrap();
        assert_eq!(
            self.snapshot(),
            before,
            "read-only invocation changed a fixture: {args:?}"
        );
        output
    }
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(path: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let meta = fs::symlink_metadata(path).unwrap();
            if meta.file_type().is_symlink() {
                output.insert(
                    path.to_owned(),
                    format!("link:{:?}", fs::read_link(path).unwrap()).into_bytes(),
                );
            } else if meta.is_dir() {
                output.insert(path.to_owned(), b"directory".to_vec());
                for entry in fs::read_dir(path).unwrap() {
                    visit(&entry.unwrap().path(), output);
                }
            } else {
                output.insert(path.to_owned(), fs::read(path).unwrap());
            }
        }
        let mut output = BTreeMap::new();
        visit(&self.root, &mut output);
        output
    }
    fn state(&self, resources: Value, active: Value) {
        self.write(
            "state/loadout/state.json",
            &json!({"schema_version":1,"resources":resources,"active_operation":active})
                .to_string(),
        );
    }
    fn known(&self, target: &str) -> Value {
        let source = self.path("store/source");
        let target = self.path(&format!("home/{target}"));
        let definition = json!({"format":"loadout.resolved-file-link.v1","kind":"file","operation":"link","source_path":source,"target_path":target,"type":"file"});
        let digest = Sha256::digest(serde_json_canonicalizer::to_vec(&definition).unwrap());
        json!({"definition_hash":format!("sha256:{digest:x}"),"file_link":{"source_path":source,"target_path":target,"link_target":source}})
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
fn expect(output: Output, code: i32, needles: &[&str]) {
    assert_eq!(output.status.code(), Some(code), "{}", text(&output));
    let text = text(&output);
    for needle in needles {
        assert!(text.contains(needle), "missing {needle:?} in {text}");
    }
}

#[test]
fn explicit_configuration_precedes_invalid_runtime_and_positional_root_precedes_default() {
    let f = Fixture::new();
    f.write("config/loadout/loadout.yaml", "invalid");
    f.profile("work", "work-item", "~/work-target");
    expect(
        f.run(&["validate", "--config", "../portable/config.yaml"]),
        0,
        &["base"],
    );
    expect(
        f.run(&["plan", "work", "--config", "../portable/config.yaml"]),
        0,
        &[
            "executable",
            "create_link",
            "work/work-item",
            "work-target",
            "target missing",
        ],
    );
    assert!(!f.path("state/loadout").exists());
    f.profile("work", "work-item", "~/missing/target");
    expect(
        f.run(&["validate", "work", "--config", "../portable/config.yaml"]),
        0,
        &["work"],
    );
}

#[test]
fn runtime_selection_and_default_config_fallback_resolve_their_own_bases() {
    let f = Fixture::new();
    f.write(
        "config/loadout/loadout.yaml",
        "schema_version: 1\nconfig_path: ../../portable/config.yaml\n",
    );
    expect(f.run(&["plan"]), 0, &["base/item", "create_link"]);
    f.write(
        "config/loadout/loadout.yaml",
        "schema_version: 1\nconfig_path: ~/../portable/config.yaml\n",
    );
    expect(f.run(&["validate"]), 0, &["base"]);
    // Default config files have their own relative store/discovery bases.
    let environment = "schema_version: 1\ndefault_profile: base\nprofile_discovery:\n  paths: [../../portable/profiles]\nstores:\n  files:\n    type: local\n    path: ../../store\n";
    f.write("config/loadout/config.yaml", environment);
    f.write("config/loadout/loadout.yaml", "schema_version: 1\n");
    expect(f.run(&["validate"]), 0, &["base"]);
    fs::remove_file(f.path("config/loadout/loadout.yaml")).unwrap();
    expect(f.run(&["plan"]), 0, &["executable"]);
    let absolute = f.path("portable/config.yaml");
    expect(
        f.run(&["validate", "--config", absolute.to_str().unwrap()]),
        0,
        &["base"],
    );
}

#[test]
fn defaults_all_roots_and_argument_rejections() {
    let f = Fixture::new();
    f.environment(None);
    for command in ["validate", "plan"] {
        expect(
            f.run(&[command, "--config", "../portable/config.yaml"]),
            2,
            &["profile"],
        );
        expect(
            f.run(&[command, "--config", "../portable/config.yaml", "base"]),
            0,
            &["base"],
        );
    }
    f.profile("work", "item", "~/another");
    expect(
        f.run(&["validate", "--config", "../portable/config.yaml", "--all"]),
        0,
        &["base", "work"],
    );
    f.write(
        "portable/profiles/work.yaml",
        "schema_version: 1\nid: work\nincludes: [{id: missing}]\nresources: {}\n",
    );
    expect(
        f.run(&["validate", "--config", "../portable/config.yaml", "--all"]),
        2,
        &["valid profile: base", "invalid profile work", "missing"],
    );
    for args in [
        vec!["validate", "--all", "base"],
        vec!["plan", "base", "work"],
        vec!["validate", "base", "work"],
        vec!["plan", "--all"],
        vec!["validate", "--profile", "base"],
        vec!["plan", "--profile", "base"],
        vec!["diff", "--config", "bad"],
        vec!["diff", "base"],
        vec!["diff", "--all"],
        vec!["validate", "--config"],
        vec!["plan", "--yes"],
        vec!["plan", "--dry-run"],
    ] {
        expect(f.run(&args), 2, &["input error"]);
    }
}

#[test]
fn plan_conflict_and_runtime_state_error_have_distinct_exit_status() {
    let f = Fixture::new();
    f.write("home/target", "unmanaged");
    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        2,
        &["blocked", "conflict", "base/item", "target", "other_entry"],
    );
    for state in [
        "invalid",
        "{\"schema_version\":2,\"resources\":{},\"active_operation\":null}",
    ] {
        f.write("state/loadout/state.json", state);
        for command in ["plan", "diff"] {
            let args = if command == "plan" {
                vec![command, "--config", "../portable/config.yaml"]
            } else {
                vec![command]
            };
            let result = f.run(&args);
            assert!(!text(&result).contains("conflict"));
            expect(result, 1, &["state"]);
        }
        expect(
            f.run(&["validate", "--config", "../portable/config.yaml"]),
            0,
            &["base"],
        );
    }
}

#[test]
fn diff_needs_no_configuration_even_with_unfinished_operation() {
    let f = Fixture::new();
    f.write("config/loadout/loadout.yaml", "invalid");
    fs::remove_file(f.path("portable/config.yaml")).unwrap();
    fs::remove_file(f.path("store/source")).unwrap();
    expect(f.run(&["diff"]), 0, &["Known resources: 0"]);
    let mut actions = serde_json::Map::new();
    for (index, status) in ["pending", "running", "uncertain"].iter().enumerate() {
        actions.insert(format!("a{index}"), json!({"kind":"create_link","resource_id":format!("base/r{index}"),"target_path":f.path(&format!("home/t{index}")),"precondition":{"target":"missing"},"postcondition":{"target":"expected_link","link_target":f.path("store/source")},"status":status}));
    }
    f.state(json!({}), json!({"id":"operation-fixture","desired_hash":format!("sha256:{}","a".repeat(64)),"actions":actions}));
    expect(
        f.run(&["diff"]),
        0,
        &[
            "operation-fixture",
            "pending",
            "running",
            "uncertain",
            "base/r0",
            "base/r1",
            "base/r2",
        ],
    );
}

#[cfg(unix)]
#[test]
fn diff_reports_every_observation_and_noop_plan_succeeds_without_mutation() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    symlink(f.path("store/source"), f.path("home/target")).unwrap();
    f.state(json!({"base/item":f.known("target")}), Value::Null);
    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        0,
        &["executable", "noop", "already satisfied"],
    );
    f.profile("base", "renamed", "~/target");
    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        0,
        &[
            "replace_ownership",
            "base/item",
            "base/renamed",
            "managed identity handoff",
        ],
    );
    f.profile("base", "item", "~/moved");
    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        0,
        &["relocate_link", "target", "moved", "target changed"],
    );
    f.write("home/file", "unmanaged");
    symlink(f.path("wrong"), f.path("home/wrong")).unwrap();
    f.state(json!({"base/expected":f.known("target"),"base/missing":f.known("missing"),"base/file":f.known("file"),"base/wrong":f.known("wrong"),"base/unsafe":f.known("absent/target")}), Value::Null);
    fs::remove_file(f.path("store/source")).unwrap();
    expect(
        f.run(&["diff"]),
        0,
        &[
            "base/expected",
            "expected_link",
            "base/missing",
            "missing",
            "base/file",
            "other_entry",
            "base/wrong",
            "other_link",
            "base/unsafe",
            "unsafe_path",
        ],
    );
}

#[cfg(unix)]
#[test]
fn read_only_processes_do_not_acquire_the_exclusive_state_lock() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    f.write("state/loadout/state.lock", "");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.path("state/loadout/state.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    expect(f.run(&["diff"]), 0, &["Known resources: 0"]);
    for command in ["validate", "plan"] {
        expect(
            f.run(&[command, "--config", "../portable/config.yaml"]),
            0,
            &["base"],
        );
    }
}

#[test]
fn diff_does_not_depend_on_runtime_configuration_locations() {
    let f = Fixture::new();
    let before = f.snapshot();
    let output = f
        .command()
        .env("XDG_CONFIG_HOME", "invalid-relative-path")
        .env_remove("APPDATA")
        .arg("diff")
        .output()
        .unwrap();
    expect(output, 0, &["Known resources: 0"]);
    assert_eq!(f.snapshot(), before);
}

#[cfg(unix)]
#[test]
fn unset_xdg_locations_use_only_the_isolated_home_defaults() {
    let f = Fixture::new();
    f.write(
        "home/.config/loadout/loadout.yaml",
        "schema_version: 1\nconfig_path: ~/../portable/config.yaml\n",
    );
    let before = f.snapshot();
    for name in ["validate", "plan", "diff"] {
        let output = f
            .command()
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .arg(name)
            .output()
            .unwrap();
        expect(output, 0, &[]);
        assert_eq!(f.snapshot(), before);
    }
    assert!(!f.path("home/.local/state").exists());
}

#[test]
fn selected_configuration_errors_are_reported_without_creating_state() {
    let f = Fixture::new();
    f.write("config/loadout/loadout.yaml", "schema_version: 99\n");
    expect(
        f.run(&["validate"]),
        2,
        &["runtime configuration", "loadout.yaml"],
    );
    f.write("portable/config.yaml", "schema_version: 99\n");
    expect(
        f.run(&["validate", "--config", "../portable/config.yaml"]),
        2,
        &["environment configuration"],
    );
    expect(
        f.run(&["plan", "--config", "../missing.yaml"]),
        1,
        &["configuration", "missing.yaml"],
    );
    assert!(!f.path("state/loadout").exists());
}

#[test]
fn native_absolute_store_and_discovery_paths_and_home_relative_selection_work() {
    let f = Fixture::new();
    f.write(
        "portable/config.yaml",
        &serde_yaml::to_string(&json!({
            "schema_version":1,"default_profile":"base",
            "profile_discovery":{"paths":[f.path("portable/profiles")]},
            "stores":{"files":{"type":"local","path":f.path("store")}}
        }))
        .unwrap(),
    );
    expect(
        f.run(&["validate", "--config", "~/../portable/config.yaml"]),
        0,
        &["base"],
    );
    expect(
        f.run(&[
            "plan",
            "--config",
            f.path("portable/config.yaml").to_str().unwrap(),
        ]),
        0,
        &["executable", "base/item"],
    );
}

#[cfg(unix)]
#[test]
fn dangling_runtime_selection_is_an_error_and_does_not_fall_back() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write("config/loadout/config.yaml", "schema_version: 1\ndefault_profile: base\nprofile_discovery:\n  paths: [../../portable/profiles]\nstores:\n  files:\n    type: local\n    path: ../../store\n");
    symlink(
        f.path("missing-runtime"),
        f.path("config/loadout/loadout.yaml"),
    )
    .unwrap();
    expect(f.run(&["validate"]), 1, &["loadout.yaml"]);
    expect(
        f.run(&["validate", "--config", "../portable/config.yaml"]),
        0,
        &["base"],
    );
}

#[path = "support/apply.rs"]
mod apply;
