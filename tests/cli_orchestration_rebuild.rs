//! Compiled-binary proof of the complete recorded-order rebuild procedure.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
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
            "loadout-orchestration-rebuild-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = normal_existing_path(root);
        let fixture = Self { root };
        for directory in [
            "home",
            "config",
            "portable/profiles",
            "store",
            "state",
            "cwd",
        ] {
            fs::create_dir_all(fixture.path(directory)).unwrap();
        }
        fixture.write("store/source", "source\n");
        fixture.write(
            "portable/config.yaml",
            "schema_version: 2\ndefault_profile: base\nprofile_discovery:\n  paths: [profiles]\nstores:\n  files:\n    type: local\n    properties:\n      path: ../store\n",
        );
        fixture
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn profile(&self, first_after: &str, second_after: &str, members: bool) {
        let resources = if members {
            "  first-item:\n    type: file\n    stage: first\n    properties: { kind: file, operation: link, source: { store: files, path: source }, target: ~/first }\n  second-item:\n    type: file\n    stage: second\n    properties: { kind: file, operation: link, source: { store: files, path: source }, target: ~/second }\n"
        } else {
            "  {}\n"
        };
        self.write(
            "portable/profiles/base.yaml",
            &format!(
                "schema_version: 6\nid: base\nstages:\n  first: {{ after: [{first_after}] }}\n  second: {{ after: [{second_after}] }}\nresources:\n{resources}"
            ),
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_loadout"))
            .current_dir(self.path("cwd"))
            .env("HOME", self.path("home"))
            .env("USERPROFILE", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("APPDATA", self.path("config"))
            .env("LOCALAPPDATA", self.path("state"))
            .args(args)
            .output()
            .unwrap()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("state/loadout/state.json")).unwrap()).unwrap()
    }

    fn inject_running_create_with_unproved_publication(&self) {
        let mut state = self.state();
        let fingerprint = format!("sha256:{:x}", Sha256::digest(b"source\n"));
        let target = self.path("home/interrupted-copy");
        let source = self.path("store/source");
        state["active_operation"] = json!({
            "id": "interrupted-create",
            "desired_hash": format!("sha256:{}", "a".repeat(64)),
            "actions": {
                "a1": {
                    "kind": "create_copy",
                    "resource_id": "base/interrupted",
                    "target_path": target,
                    "source_path": source,
                    "content_fingerprint": fingerprint,
                    "temporary_path": self.path("home/interrupted-temporary"),
                    "final_effect": {
                        "kind": "file_copy",
                        "source_path": source,
                        "target_path": target,
                        "content_fingerprint": fingerprint
                    },
                    "precondition": { "target": "missing" },
                    "postcondition": { "target": "expected_copy", "content_fingerprint": fingerprint },
                    "temporary_staged": false,
                    "publication_attempted": false,
                    "status": "running"
                }
            },
            "schedule": ["a1"],
            "skip_reasons": {},
            "graph": {
                "actions": [{
                    "id": "a1",
                    "resource_id": "base/interrupted",
                    "role": "current_create",
                    "topology_authority": { "current": [], "historical": [] },
                    "topology_delta": {
                        "add_current_direct": [], "remove_current_direct": [],
                        "add_historical_direct": [], "remove_historical_direct": [],
                        "add_current_barriers": [], "remove_current_barriers": [],
                        "add_historical_barriers": [], "remove_historical_barriers": [],
                        "advance_current_barriers": [], "advance_historical_barriers": [],
                        "replace_current_barriers": [], "replace_historical_barriers": []
                    }
                }],
                "direct_edges": [],
                "barriers": [],
                "schedule": ["a1"],
                "stale_topology_snapshot": { "current": [], "historical": [] }
            }
        });
        fs::write(self.path("state/loadout/state.json"), state.to_string()).unwrap();
        self.write("home/interrupted-copy", "source\n");
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

fn expect(output: Output, code: i32, fragments: &[&str]) -> String {
    let text = output_text(&output);
    assert_eq!(output.status.code(), Some(code), "{text}");
    for fragment in fragments {
        assert!(text.contains(fragment), "missing {fragment:?} in {text}");
    }
    text
}

fn targets_exist(fixture: &Fixture) -> bool {
    ["home/first", "home/second"]
        .iter()
        .all(|name| fs::symlink_metadata(fixture.path(name)).is_ok())
}

fn target_is_absent(fixture: &Fixture, name: &str) -> bool {
    fs::symlink_metadata(fixture.path(name)).is_err()
}

fn assert_no_topology(state: &Value) {
    assert_eq!(
        state["applied_lifecycle_topology"]["current"],
        serde_json::json!([])
    );
    assert_eq!(
        state["applied_lifecycle_topology"]["historical"],
        serde_json::json!([])
    );
}

const CONFIG: &[&str] = &["--config", "../portable/config.yaml"];

#[cfg(unix)]
mod interactive {
    use super::*;
    use std::{
        io::{Read, Write},
        os::fd::FromRawFd,
        os::unix::fs::PermissionsExt,
        process::{Child, Stdio},
        sync::{Arc, Mutex},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    struct Session {
        child: Child,
        master: fs::File,
        transcript: Arc<Mutex<String>>,
        reader: Option<JoinHandle<()>>,
    }

    impl Session {
        fn start(fixture: &Fixture) -> Self {
            let (mut master, mut slave) = (-1, -1);
            // SAFETY: openpty initializes both descriptors; no optional configuration pointers.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            // SAFETY: each successful descriptor is owned by exactly one File.
            let master = unsafe { fs::File::from_raw_fd(master) };
            let mut reader_master = master.try_clone().unwrap();
            let transcript = Arc::new(Mutex::new(String::new()));
            let observed = Arc::clone(&transcript);
            let reader = thread::spawn(move || {
                let mut buffer = [0; 4096];
                loop {
                    match reader_master.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => observed
                            .lock()
                            .unwrap()
                            .push_str(&String::from_utf8_lossy(&buffer[..count])),
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => panic!("read terminal output: {error}"),
                    }
                }
            });
            // SAFETY: the slave descriptor is transferred to stdin/stderr below.
            let slave = unsafe { fs::File::from_raw_fd(slave) };
            let mut command = Command::new(env!("CARGO_BIN_EXE_loadout"));
            command
                .current_dir(fixture.path("cwd"))
                .env("HOME", fixture.path("home"))
                .env("USERPROFILE", fixture.path("home"))
                .env("XDG_CONFIG_HOME", fixture.path("config"))
                .env("XDG_STATE_HOME", fixture.path("state"))
                .env("APPDATA", fixture.path("config"))
                .env("LOCALAPPDATA", fixture.path("state"))
                .args(["apply", "--config", "../portable/config.yaml"])
                .stdout(Stdio::piped())
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave));
            let child = command.spawn().unwrap();
            Self {
                child,
                master,
                transcript,
                reader: Some(reader),
            }
        }

        fn prompt(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if self.transcript.lock().unwrap().contains("Apply this plan?") {
                    return;
                }
                assert!(Instant::now() < deadline, "prompt timeout");
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "exited before prompt"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }

        fn answer_and_finish(&mut self, answer: &str) -> (i32, String) {
            self.master.write_all(answer.as_bytes()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "child timeout");
                thread::sleep(Duration::from_millis(10));
            };
            self.reader.take().unwrap().join().unwrap();
            let mut output = self.transcript.lock().unwrap().clone();
            self.child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut output)
                .unwrap();
            (status.code().unwrap(), output)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    #[test]
    fn partial_stale_removal_retains_old_facts_until_retry_completes() {
        let fixture = Fixture::new();
        fixture.profile("", "first", true);
        expect(
            fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
            0,
            &["apply completed: 2 committed actions"],
        );
        fixture.profile("second", "", false);

        let mut session = Session::start(&fixture);
        session.prompt();
        fs::rename(
            fixture.path("home/first"),
            fixture.path("home/displaced-first"),
        )
        .unwrap();
        fixture.write("home/first", "unmanaged after preflight\n");
        let (code, transcript) = session.answer_and_finish("y\n");
        assert_eq!(code, 1, "{transcript}");
        assert!(transcript.contains("committed actions: 1"), "{transcript}");
        assert!(target_is_absent(&fixture, "home/second"));
        assert_eq!(
            fs::read(fixture.path("home/first")).unwrap(),
            b"unmanaged after preflight\n"
        );
        let partial = fixture.state();
        assert_eq!(partial["resources"].as_object().unwrap().len(), 1);
        assert!(partial["resources"]["base/first-item"].is_object());
        assert!(
            !partial["applied_lifecycle_topology"]["current"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        fs::remove_file(fixture.path("home/first")).unwrap();
        fs::rename(
            fixture.path("home/displaced-first"),
            fixture.path("home/first"),
        )
        .unwrap();
        expect(
            fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
            0,
            &["apply completed"],
        );
        assert!(target_is_absent(&fixture, "home/first"));
        assert!(fixture.state()["resources"].as_object().unwrap().is_empty());
        assert_no_topology(&fixture.state());
        fixture.profile("second", "", true);
        expect(
            fixture.run(&[&["plan"], CONFIG].concat()),
            0,
            &["executable"],
        );
    }

    #[test]
    fn uncertain_later_stale_removal_preserves_partial_rebuild_until_recovery() {
        // Root can bypass the directory permission check used to make target observation unavailable.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipping permission-based uncertain stale removal as root");
            return;
        }
        let fixture = Fixture::new();
        fs::create_dir(fixture.path("home/nested")).unwrap();
        fixture.profile("", "first", true);
        let profile = fs::read_to_string(fixture.path("portable/profiles/base.yaml")).unwrap();
        fixture.write(
            "portable/profiles/base.yaml",
            &profile.replace("target: ~/first", "target: ~/nested/first"),
        );
        expect(
            fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
            0,
            &["apply completed: 2 committed actions"],
        );
        fixture.profile("second", "", false);

        let mut session = Session::start(&fixture);
        session.prompt();
        fs::set_permissions(
            fixture.path("home/nested"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let (code, transcript) = session.answer_and_finish("y\n");
        assert_ne!(code, 0, "{transcript}");
        assert!(transcript.contains("committed actions: 1"), "{transcript}");
        assert!(target_is_absent(&fixture, "home/second"));
        let partial = fixture.state();
        assert_eq!(partial["resources"].as_object().unwrap().len(), 1);
        assert!(
            !partial["applied_lifecycle_topology"]["current"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let later_action = partial["active_operation"]["actions"]
            .as_object()
            .unwrap()
            .values()
            .find(|action| action["resource_id"] == "base/first-item")
            .unwrap();
        assert_eq!(later_action["status"], "uncertain", "{transcript}");

        fs::set_permissions(
            fixture.path("home/nested"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        expect(
            fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
            0,
            &["prior operation recovery", "apply completed"],
        );
        assert!(target_is_absent(&fixture, "home/nested/first"));
        assert!(fixture.state()["resources"].as_object().unwrap().is_empty());
        assert_no_topology(&fixture.state());
    }
}

#[test]
fn recorded_order_rebuild_removes_every_member_and_old_fact_before_reapplying() {
    let fixture = Fixture::new();
    fixture.profile("", "first", true);
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        0,
        &["apply completed: 2 committed actions"],
    );
    assert!(targets_exist(&fixture));
    assert_eq!(fixture.state()["resources"].as_object().unwrap().len(), 2);
    assert!(
        !fixture.state()["applied_lifecycle_topology"]["current"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    fixture.profile("second", "", true);
    let state_before = fs::read(fixture.path("state/loadout/state.json")).unwrap();
    let conflict = expect(
        fixture.run(&[&["plan"], CONFIG].concat()),
        2,
        &[
            "recorded order base/first-item -> base/second-item",
            "proposed order base/second-item -> base/first-item",
            "rebuild set: base/first-item, base/second-item",
            "remove the entire rebuild set from Desired",
        ],
    );
    assert!(conflict.contains("stage/base/"));
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--dry-run"]].concat()),
        2,
        &["rebuild set: base/first-item, base/second-item"],
    );
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        2,
        &["rebuild set: base/first-item, base/second-item"],
    );
    assert_eq!(
        fs::read(fixture.path("state/loadout/state.json")).unwrap(),
        state_before
    );
    assert!(targets_exist(&fixture));

    fixture.profile("second", "", false);
    fs::rename(
        fixture.path("home/second"),
        fixture.path("home/displaced-second"),
    )
    .unwrap();
    fixture.write("home/second", "unmanaged\n");
    let before_rejected_removal = fs::read(fixture.path("state/loadout/state.json")).unwrap();
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        2,
        &["blocked", "base/second-item", "conflict"],
    );
    assert_eq!(
        fs::read(fixture.path("state/loadout/state.json")).unwrap(),
        before_rejected_removal
    );
    assert_eq!(
        fs::read(fixture.path("home/second")).unwrap(),
        b"unmanaged\n"
    );
    assert!(fs::symlink_metadata(fixture.path("home/first")).is_ok());
    fs::remove_file(fixture.path("home/second")).unwrap();
    fs::rename(
        fixture.path("home/displaced-second"),
        fixture.path("home/second"),
    )
    .unwrap();

    fixture.inject_running_create_with_unproved_publication();
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        2,
        &["apply failed during Recovery", "must be recovered"],
    );
    assert_eq!(
        fixture.state()["active_operation"]["actions"]["a1"]["status"],
        "uncertain"
    );
    assert_eq!(fixture.state()["resources"].as_object().unwrap().len(), 2);
    assert!(targets_exist(&fixture));
    fs::remove_file(fixture.path("home/interrupted-copy")).unwrap();

    let removal_plan = expect(
        fixture.run(&[&["plan"], CONFIG].concat()),
        0,
        &["remove_link", "base/first-item", "base/second-item"],
    );
    let removal_actions = removal_plan
        .lines()
        .filter(|line| line.starts_with("remove_link "))
        .collect::<Vec<_>>();
    assert_eq!(removal_actions.len(), 2, "{removal_plan}");
    assert!(
        removal_actions[0].contains("base/second-item"),
        "{removal_plan}"
    );
    assert!(
        removal_actions[1].contains("base/first-item"),
        "{removal_plan}"
    );
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        0,
        &["apply completed: 2 committed actions"],
    );
    assert!(target_is_absent(&fixture, "home/first"));
    assert!(target_is_absent(&fixture, "home/second"));
    assert!(fixture.state()["resources"].as_object().unwrap().is_empty());
    assert_no_topology(&fixture.state());
    assert!(fixture.state()["active_operation"].is_null());

    fixture.profile("second", "", true);
    expect(
        fixture.run(&[&["plan"], CONFIG].concat()),
        0,
        &[
            "executable",
            "create_link",
            "base/first-item",
            "base/second-item",
        ],
    );
    expect(
        fixture.run(&[&["apply"], CONFIG, &["--yes"]].concat()),
        0,
        &["apply completed: 2 committed actions"],
    );
    assert!(targets_exist(&fixture));
    assert_eq!(fixture.state()["resources"].as_object().unwrap().len(), 2);
    assert!(fixture.state()["active_operation"].is_null());
}
