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
            "loadout-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = normal_existing_path(root);
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
        self.root.join(name).components().collect()
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
        self.write("portable/config.yaml", &format!("schema_version: 2\n{default}profile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store\n"));
    }
    fn profile(&self, id: &str, resource: &str, target: &str) {
        self.write(&format!("portable/profiles/{id}.yaml"), &format!("schema_version: 2\nid: {id}\nresources:\n  {resource}:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: {target}\n"));
    }
    fn command(&self) -> Command {
        self.command_with_home(&self.path("home"))
    }
    fn command_with_home(&self, home: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
        command.current_dir(self.path("cwd"));
        // Keep platform process-launch essentials while overriding every application location.
        command
            .env("HOME", home)
            .env("USERPROFILE", home)
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
            &json!({"schema_version":2,"resources":resources,"active_operation":active})
                .to_string(),
        );
    }
    fn known(&self, target: &str) -> Value {
        let source = self.path("store/source");
        let target = self.path(&format!("home/{target}"));
        let definition = json!({"format":"loadout.resolved-file-link.v1","kind":"file","operation":"link","source_path":source,"target_path":target,"type":"file"});
        let digest = Sha256::digest(serde_json_canonicalizer::to_vec(&definition).unwrap());
        json!({"definition_hash":format!("sha256:{digest:x}"),"effect":{"kind":"file_link","source_path":source,"target_path":target,"link_target":source}})
    }
    fn known_copy(&self, target: &str, contents: &[u8]) -> Value {
        let source = self.path("store/source");
        let target = self.path(&format!("home/{target}"));
        let definition = json!({"format":"loadout.resolved-file-copy.v1","kind":"file","operation":"copy","source_path":source,"target_path":target,"type":"file"});
        let definition_digest =
            Sha256::digest(serde_json_canonicalizer::to_vec(&definition).unwrap());
        let content_digest = Sha256::digest(contents);
        json!({"definition_hash":format!("sha256:{definition_digest:x}"),"effect":{"kind":"file_copy","source_path":source,"target_path":target,"content_fingerprint":format!("sha256:{content_digest:x}")}})
    }
}
#[cfg(target_os = "linux")]
struct DisposableDirectory {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl DisposableDirectory {
    fn new(parent: &Path, label: &str) -> Self {
        let path = parent.join(format!(
            "loadout-cli-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DisposableDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
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
    let environment = "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [../../portable/profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../../store\n";
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
        "schema_version: 2\nid: work\nincludes: [{id: missing}]\nresources: {}\n",
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
        "{\"schema_version\":1,\"resources\":{},\"active_operation\":null}",
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
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &["status profile: base"],
    );
    expect(
        f.run(&["resource", "list", "--known"]),
        0,
        &["Known resources: 0"],
    );
}

#[cfg(unix)]
#[test]
fn status_reports_a_symlinked_parent_as_unsafe_without_changing_any_fixture_entry() {
    use std::os::unix::fs::symlink;

    let f = Fixture::new();
    fs::create_dir(f.path("outside")).unwrap();
    symlink(f.path("outside"), f.path("home/unsafe")).unwrap();
    f.profile("base", "item", "~/unsafe/target");
    f.state(json!({"base/item":f.known("unsafe/target")}), Value::Null);

    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "desired-to-known: definitions_match",
            "known-to-actual: drifted",
            "unsafe_path",
        ],
    );
}

#[cfg(unix)]
#[test]
fn status_returns_partial_report_after_an_unavailable_desired_target_observation() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  available:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.available\n  unavailable:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.blocked/target\n",
    );
    let blocked = f.path("home/.blocked");
    fs::create_dir(&blocked).unwrap();
    let before = f.snapshot();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let output = f
        .command()
        .args(["status", "--config", "../portable/config.yaml"])
        .output()
        .unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        f.snapshot(),
        before,
        "status changed the unavailable fixture"
    );

    expect(
        output,
        1,
        &[
            "base/available",
            "missing",
            "base/unavailable",
            "desired-to-actual: desired_target_observation: unavailable",
        ],
    );
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
    f.write(
        "portable/config.yaml",
        "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    path: ../store\n",
    );
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
            "schema_version":2,"default_profile":"base",
            "profile_discovery":{"paths":[f.path("portable/profiles")]},
            "stores":{"files":{"type":"local","properties":{"path":f.path("store")}}}
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
    f.write("config/loadout/config.yaml", "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [../../portable/profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../../store\n");
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

#[test]
fn config_read_commands_report_selected_values_without_mutation() {
    let f = Fixture::new();
    f.write(
        "config/loadout/loadout.yaml",
        "schema_version: 1\nconfig_path: ../../portable/config.yaml\n",
    );
    let portable_path = f.path("portable/config.yaml");
    let output = f.run(&["config", "path"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        output.stdout,
        format!("{}\n", portable_path.display()).into_bytes()
    );
    assert!(output.stderr.is_empty());
    expect(
        f.run(&["config", "list"]),
        0,
        &[
            &format!("configuration: {}", portable_path.display()),
            "default_profile: base",
            &format!(
                "stores.files.properties.path: {}",
                f.path("store").display()
            ),
        ],
    );
    expect(
        f.run(&["config", "get", "default_profile"]),
        0,
        &["default_profile: base"],
    );
    expect(
        f.run(&[
            "config",
            "get",
            "--config",
            "../portable/config.yaml",
            "stores.files.properties.path",
        ]),
        0,
        &[
            &format!("configuration: {}", portable_path.display()),
            &format!(
                "stores.files.properties.path: {}",
                f.path("store").display()
            ),
        ],
    );
    expect(
        f.run(&["config", "get", "profile_discovery.paths"]),
        2,
        &["unsupported configuration field"],
    );
    expect(
        f.run(&[
            "config",
            "path",
            "--system",
            "--config",
            "../portable/config.yaml",
        ]),
        2,
        &["input error"],
    );
    f.write("config/loadout/loadout.yaml", "invalid");
    f.write("portable/config.yaml", "invalid");
    let system_path = f.path("config/loadout/loadout.yaml");
    let output = f.run(&["config", "path", "--system"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        output.stdout,
        format!("{}\n", system_path.display()).into_bytes()
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn inspection_commands_render_the_selected_declaration_desired_known_and_status_views() {
    let f = Fixture::new();
    f.profile("work", "work-item", "~/work-target");
    expect(
        f.run(&["profile", "list", "--config", "../portable/config.yaml"]),
        0,
        &["Profiles: 2", "base", "work"],
    );
    expect(
        f.run(&[
            "profile",
            "show",
            "--config",
            "../portable/config.yaml",
            "base",
        ]),
        0,
        &[
            "profile: base",
            "resource item",
            "store files",
            "target ~/target",
        ],
    );
    expect(
        f.run(&["resource", "list", "--config", "../portable/config.yaml"]),
        0,
        &[
            "Desired resources for base: 1",
            "base/item",
            "operation link",
        ],
    );
    expect(
        f.run(&[
            "resource",
            "show",
            "--config",
            "../portable/config.yaml",
            "base/item",
        ]),
        0,
        &["Desired resource for base", "base/item", "source", "target"],
    );
    f.state(json!({"base/item":f.known("target")}), Value::Null);
    expect(
        f.run(&["resource", "list", "--known"]),
        0,
        &["Known resources: 1", "base/item"],
    );
    expect(
        f.run(&["resource", "show", "--known", "base/item"]),
        0,
        &["base/item", "source", "target"],
    );
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "status profile: base",
            "desired-to-known: definitions_match",
            "drifted",
            "missing",
        ],
    );
    f.state(
        json!({"base/item":f.known("target")}),
        json!({
            "id":"interrupted-operation",
            "desired_hash":format!("sha256:{}", "a".repeat(64)),
            "actions": {
                "a1": {
                    "kind":"create_link",
                    "resource_id":"base/pending",
                    "target_path":f.path("home/pending"),
                    "precondition":{"target":"missing"},
                    "postcondition":{"target":"expected_link","link_target":f.path("store/source")},
                    "status":"pending"
                }
            }
        }),
    );
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &["active_operation: interrupted-operation", "a1", "pending"],
    );
    expect(
        f.run(&["resource", "show", "--known", "base/unknown"]),
        2,
        &["unknown resource ID"],
    );
}

#[test]
fn status_reports_active_operation_when_valid_state_precedes_an_invalid_declaration() {
    let f = Fixture::new();
    f.state(
        json!({}),
        json!({
            "id":"interrupted-operation",
            "desired_hash":format!("sha256:{}", "a".repeat(64)),
            "actions": {
                "a1": {
                    "kind":"create_link",
                    "resource_id":"base/pending",
                    "target_path":f.path("home/pending"),
                    "precondition":{"target":"missing"},
                    "postcondition":{"target":"expected_link","link_target":f.path("store/source")},
                    "status":"pending"
                }
            }
        }),
    );
    f.write(
        "portable/profiles/base.yaml",
        "not: a valid profile declaration\n",
    );

    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        2,
        &[
            "desired_unavailable",
            "active_operation: interrupted-operation",
            "a1",
            "pending",
        ],
    );
}

#[test]
fn copy_inspection_reports_typed_known_and_desired_only_facts_without_side_effects() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    f.write("home/.copy-target", "content\n");
    f.state(
        json!({"base/copied": f.known_copy(".copy-target", b"content\n")}),
        Value::Null,
    );

    expect(
        f.run(&["diff"]),
        0,
        &["Known resources: 1", "base/copied", "expected_copy"],
    );
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "definitions_match",
            "recorded_and_expected",
            "expected_copy",
        ],
    );

    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "definition_changed",
            "known-to-actual: expected_copy",
            "expected_copy",
        ],
    );

    let desired_only = Fixture::new();
    desired_only.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    desired_only.write("home/.copy-target", "content\n");
    expect(
        desired_only.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "desired_only",
            "desired_target_observation",
            "other_regular_file",
        ],
    );

    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    f.write("home/.copy-target", "drifted\n");
    expect(f.run(&["diff"]), 0, &["base/copied", "other_regular_file"]);
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &["definitions_match", "drifted", "other_regular_file"],
    );
}

#[cfg(target_os = "linux")]
#[test]
fn copy_capability_preflight_rejection_creates_no_operation_or_target() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    let home = DisposableDirectory::new(Path::new("/dev/shm"), "copy-preflight");
    assert!(fs::symlink_metadata(home.path.join(".copy-target")).is_err());
    assert!(!f.path("state/loadout/state.json").exists());
    let output = f
        .command_with_home(&home.path)
        .args(["apply", "--yes", "--config", "../portable/config.yaml"])
        .output()
        .unwrap();

    expect(
        output,
        2,
        &[
            "apply failed during Preflight",
            "file-copy publication capability is unsupported",
        ],
    );
    assert!(fs::symlink_metadata(home.path.join(".copy-target")).is_err());
    assert!(!f.path("state/loadout/state.json").exists());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn copy_capability_preflight_rejection_creates_no_operation_or_target() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );

    expect(
        f.command()
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        2,
        &[
            "apply failed during Preflight",
            "file-copy publication capability is unsupported",
        ],
    );
    assert!(fs::symlink_metadata(f.path("home/.copy-target")).is_err());
    assert!(!f.path("state/loadout/state.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn copy_recovery_closes_a_retained_create_before_presenting_a_fresh_plan() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    let fingerprint = format!("sha256:{:x}", Sha256::digest(b"content\n"));
    f.state(
        json!({}),
        json!({
            "id": "recover-copy-create",
            "desired_hash": format!("sha256:{}", "a".repeat(64)),
            "actions": {"a1": {
                "kind": "create_copy",
                "resource_id": "base/copied",
                "target_path": f.path("home/.copy-target"),
                "source_path": f.path("store/source"),
                "content_fingerprint": fingerprint,
                "temporary_path": f.path("home/.loadout-copy-a1"),
                "final_effect": {
                    "kind": "file_copy",
                    "source_path": f.path("store/source"),
                    "target_path": f.path("home/.copy-target"),
                    "content_fingerprint": fingerprint
                },
                "precondition": {"target": "missing"},
                "postcondition": {"target": "expected_copy", "content_fingerprint": fingerprint},
                "status": "running"
            }}
        }),
    );
    let before_target = fs::symlink_metadata(f.path("home/.copy-target")).is_err();
    let before_temporary = fs::symlink_metadata(f.path("home/.loadout-copy-a1")).is_err();

    expect(
        f.command()
            .args(["apply", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        2,
        &["create_copy", "confirmation unavailable"],
    );
    assert!(before_target && before_temporary);
    assert!(fs::symlink_metadata(f.path("home/.copy-target")).is_err());
    assert!(fs::symlink_metadata(f.path("home/.loadout-copy-a1")).is_err());
    let recovered =
        serde_json::from_slice::<Value>(&fs::read(f.path("state/loadout/state.json")).unwrap())
            .unwrap();
    assert!(recovered["active_operation"].is_null());
    assert_eq!(recovered["resources"], json!({}));

    expect(
        f.command()
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        0,
        &["create_copy", "apply completed: 1 committed actions"],
    );
    assert_eq!(fs::read(f.path("home/.copy-target")).unwrap(), b"content\n");
    let applied =
        serde_json::from_slice::<Value>(&fs::read(f.path("state/loadout/state.json")).unwrap())
            .unwrap();
    assert!(applied["active_operation"].is_null());
    assert_eq!(
        applied["resources"]["base/copied"]["effect"]["kind"],
        "file_copy"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn copy_lifecycle_commands_render_typed_actions_and_preserve_rejection_boundaries() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );

    expect(
        f.run(&["validate", "--config", "../portable/config.yaml"]),
        0,
        &["valid profile: base"],
    );
    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        0,
        &["executable", "create_copy", "base/copied", "target missing"],
    );
    expect(
        f.run(&[
            "apply",
            "--dry-run",
            "--yes",
            "--config",
            "../portable/config.yaml",
        ]),
        0,
        &["executable", "create_copy", "base/copied", "target missing"],
    );
    expect(
        f.command()
            .args(["apply", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        2,
        &["create_copy", "confirmation unavailable"],
    );
    assert!(fs::symlink_metadata(f.path("home/.copy-target")).is_err());
    assert!(!f.path("state/loadout/state.json").exists());

    expect(
        f.command()
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        0,
        &[
            "create_copy",
            "base/copied",
            "apply completed: 1 committed actions",
        ],
    );
    assert_eq!(fs::read(f.path("home/.copy-target")).unwrap(), b"content\n");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(f.path("state/loadout/state.json")).unwrap())
            .unwrap()["resources"]["base/copied"]["effect"]["kind"],
        "file_copy"
    );
    assert!(
        serde_json::from_slice::<Value>(&fs::read(f.path("state/loadout/state.json")).unwrap())
            .unwrap()["active_operation"]
            .is_null()
    );

    let conflict = Fixture::new();
    conflict.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/target\n",
    );
    conflict.write("home/target", "unmanaged");
    expect(
        conflict.run(&["plan", "--config", "../portable/config.yaml"]),
        2,
        &["blocked", "conflict", "base/copied", "OtherRegularFile"],
    );
}

#[test]
fn copy_schema_rejection_precedes_target_observation_or_lifecycle_mutation() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 1\nid: base\nresources: {}\n",
    );
    fs::remove_file(f.path("store/source")).unwrap();
    expect(
        f.command()
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        2,
        &["schema_version"],
    );
    assert!(fs::symlink_metadata(f.path("home/target")).is_err());
    assert!(!f.path("state/loadout/state.json").exists());

    let state_fixture = Fixture::new();
    state_fixture.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );
    state_fixture.write(
        "state/loadout/state.json",
        "{\"schema_version\":1,\"resources\":{},\"active_operation\":null}",
    );
    expect(
        state_fixture.run(&["plan", "--config", "../portable/config.yaml"]),
        1,
        &["state"],
    );
    assert!(fs::symlink_metadata(state_fixture.path("home/.copy-target")).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn copy_declarations_render_in_read_only_queries() {
    let f = Fixture::new();
    f.write(
        "portable/profiles/base.yaml",
        "schema_version: 2\nid: base\nresources:\n  copied:\n    type: file\n    properties:\n      kind: file\n      operation: copy\n      source:\n        store: files\n        path: source\n      target: ~/.copy-target\n",
    );

    expect(
        f.run(&["plan", "--config", "../portable/config.yaml"]),
        0,
        &["executable plan"],
    );
    expect(
        f.command()
            .args(["apply", "--yes", "--config", "../portable/config.yaml"])
            .output()
            .unwrap(),
        0,
        &["apply completed: 1 committed actions"],
    );
    assert_eq!(fs::read(f.path("home/.copy-target")).unwrap(), b"content\n");
    assert!(f.path("state/loadout/state.json").exists());
    expect(
        f.run(&["resource", "list", "--config", "../portable/config.yaml"]),
        0,
        &[
            "Desired resources for base: 1",
            "base/copied: file copy:",
            "operation copy",
        ],
    );

    f.state(
        json!({}),
        json!({
            "id":"copy-rendering-operation",
            "desired_hash":format!("sha256:{}", "a".repeat(64)),
            "actions": {
                "a1": {
                    "kind":"create_link",
                    "resource_id":"base/pending",
                    "target_path":f.path("home/pending"),
                    "precondition":{"target":"missing"},
                    "postcondition":{"target":"expected_link","link_target":f.path("store/source")},
                    "status":"pending",
                }
            }
        }),
    );
    expect(
        f.run(&["status", "--config", "../portable/config.yaml"]),
        0,
        &[
            "desired_only",
            "desired_target_observation",
            "other_regular_file",
            "active_operation: copy-rendering-operation",
            "a1",
            "pending",
        ],
    );
}

#[path = "support/apply.rs"]
mod apply;
