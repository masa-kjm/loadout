//! Apply acceptance shares only the disposable fixture with read-only acceptance.
use super::*;

const ARGS: &[&str] = &["apply", "--config", "../portable/config.yaml"];

fn state(f: &Fixture) -> Value {
    serde_json::from_slice(&fs::read(f.path("state/loadout/state.json")).unwrap()).unwrap()
}
fn no_new_operation(f: &Fixture) {
    assert!(fs::symlink_metadata(f.path("home/target")).is_err());
    if f.path("state/loadout/state.json").exists() {
        assert!(state(f)["active_operation"].is_null());
        assert_eq!(state(f)["resources"], json!({}));
    }
}

#[test]
fn dry_run_is_read_only_and_yes_has_no_effect() {
    let f = Fixture::new();
    for flags in [vec!["--dry-run"], vec!["--dry-run", "--yes"]] {
        let mut args = ARGS.to_vec();
        args.extend(flags);
        expect(
            f.run(&args),
            0,
            &[
                "executable",
                "create_link",
                "base/item",
                "target",
                "target missing",
            ],
        );
    }
    f.write("home/target", "unmanaged");
    expect(
        f.run(&[ARGS, &["--dry-run", "--yes"]].concat()),
        2,
        &["blocked", "base/item", "target", "conflict"],
    );
}

#[test]
fn apply_rejects_invalid_arguments_and_invalid_or_unreadable_input() {
    let f = Fixture::new();
    for args in [
        vec!["apply", "--all"],
        vec!["apply", "--yes", "--yes"],
        vec!["apply", "base", "extra"],
        vec!["apply", "--config"],
        vec!["apply", "--profile", "base"],
    ] {
        expect(
            f.command().args(args).output().unwrap(),
            2,
            &["input error"],
        );
        assert!(!f.path("state/loadout").exists());
    }
    f.write("portable/config.yaml", "schema_version: 99");
    expect(
        f.command().args(ARGS).arg("--yes").output().unwrap(),
        2,
        &["Resolution"],
    );
    no_new_operation(&f);
    fs::remove_file(f.path("portable/config.yaml")).unwrap();
    expect(
        f.command().args(ARGS).arg("--yes").output().unwrap(),
        1,
        &["cannot read configuration"],
    );
    no_new_operation(&f);
}

#[cfg(windows)]
#[test]
fn compiled_binary_creates_an_owned_file_link_and_commits_known_state() {
    let f = Fixture::new();

    expect(
        f.command().args(ARGS).arg("--yes").output().unwrap(),
        0,
        &[
            "create_link",
            "base/item",
            "apply completed: 1 committed actions",
        ],
    );
    assert_eq!(
        fs::read_link(f.path("home/target")).unwrap(),
        f.path("store/source")
    );
    assert!(state(&f)["resources"]["base/item"].is_object());
    assert!(state(&f)["active_operation"].is_null());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        process::{Child, Stdio},
        time::{Duration, Instant},
    };
    struct Session {
        child: Child,
        master: fs::File,
        transcript: String,
    }
    impl Session {
        fn start(f: &Fixture, stdin_terminal: bool, stderr_terminal: bool, extra: &[&str]) -> Self {
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
            // SAFETY: each successful openpty descriptor is owned exactly once.
            let master = unsafe { fs::File::from_raw_fd(master) };
            let slave = unsafe { fs::File::from_raw_fd(slave) };
            let mut command = f.command();
            command.args(ARGS).args(extra).stdout(Stdio::piped());
            command.stdin(if stdin_terminal {
                Stdio::from(slave.try_clone().unwrap())
            } else {
                Stdio::null()
            });
            command.stderr(if stderr_terminal {
                Stdio::from(slave)
            } else {
                Stdio::piped()
            });
            let child = command.spawn().unwrap();
            Self {
                child,
                master,
                transcript: String::new(),
            }
        }
        fn prompt(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.transcript.contains("Apply this plan?") {
                assert!(
                    Instant::now() < deadline,
                    "prompt timeout: {}",
                    self.transcript
                );
                let mut poll = libc::pollfd {
                    fd: self.master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd with a bounded timeout.
                assert!(unsafe { libc::poll(&mut poll, 1, 100) } >= 0);
                if poll.revents & libc::POLLIN != 0 {
                    let mut bytes = [0; 4096];
                    let count = self.master.read(&mut bytes).unwrap();
                    self.transcript
                        .push_str(&String::from_utf8_lossy(&bytes[..count]));
                }
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "exited before confirmation: {}",
                    self.transcript
                );
            }
        }
        fn answer(&mut self, answer: &str) {
            self.master.write_all(answer.as_bytes()).unwrap();
        }
        fn finish(&mut self) -> (i32, String) {
            let deadline = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "child timeout");
                std::thread::sleep(Duration::from_millis(10));
            };
            let mut output = self.transcript.clone();
            self.child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut output)
                .unwrap();
            if let Some(mut stderr) = self.child.stderr.take() {
                stderr.read_to_string(&mut output).unwrap();
            }
            loop {
                let mut poll = libc::pollfd {
                    fd: self.master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd, nonblocking poll.
                if unsafe { libc::poll(&mut poll, 1, 0) } <= 0
                    || poll.revents & (libc::POLLIN | libc::POLLHUP) == 0
                {
                    break;
                }
                let mut bytes = [0; 4096];
                match self.master.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => output.push_str(&String::from_utf8_lossy(&bytes[..count])),
                }
            }
            (status.code().unwrap(), output)
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn recovery_cleans_an_exact_recorded_temporary_after_dry_run_leaves_it_untouched() {
        let f = Fixture::new();
        f.write("store/new", "replacement");
        std::os::unix::fs::symlink(f.path("store/source"), f.path("home/target")).unwrap();
        std::os::unix::fs::symlink(f.path("store/new"), f.path("home/recorded-temporary")).unwrap();
        f.state(json!({"base/item":f.known("target")}), json!({
            "id":"recoverable-replacement", "desired_hash":format!("sha256:{}", "a".repeat(64)),
            "actions":{"a1":{"kind":"replace_link", "resource_id":"base/item", "target_path":f.path("home/target"),
                "temporary_path":f.path("home/recorded-temporary"),
                "precondition":{"target":"expected_link","link_target":f.path("store/source")},
                "postcondition":{"target":"expected_link","link_target":f.path("store/new")},"status":"running"}}
        }));
        for flags in [vec!["--dry-run"], vec!["--dry-run", "--yes"]] {
            expect(
                f.run(&[ARGS, &flags].concat()),
                0,
                &["noop", "already satisfied"],
            );
        }
        // Normal apply may perform the narrowly authorized recovery cleanup before evaluating its fresh plan; the old target remains the declared desired link.
        expect(
            f.command().args(ARGS).output().unwrap(),
            2,
            &["noop", "already satisfied", "confirmation unavailable"],
        );
        assert!(fs::symlink_metadata(f.path("home/recorded-temporary")).is_err());
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn compiled_binary_replaces_an_owned_link_without_delete_then_create() {
        let f = Fixture::new();
        f.write("store/new", "replacement");
        std::os::unix::fs::symlink(f.path("store/source"), f.path("home/target")).unwrap();
        f.state(json!({"base/item": f.known("target")}), Value::Null);
        f.write(
            "portable/profiles/base.yaml",
            "schema_version: 1\nid: base\nresources:\n  item:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: new\n      target: ~/target\n",
        );

        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &[
                "replace_link",
                "base/item",
                "apply completed: 1 committed actions",
            ],
        );
        assert_eq!(
            fs::read_link(f.path("home/target")).unwrap(),
            f.path("store/new")
        );
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn compiled_binary_replaces_an_owned_link_for_a_changed_source_handoff() {
        let f = Fixture::new();
        f.write("store/new", "replacement");
        std::os::unix::fs::symlink(f.path("store/source"), f.path("home/target")).unwrap();
        f.state(json!({"base/item": f.known("target")}), Value::Null);
        f.write(
            "portable/profiles/base.yaml",
            "schema_version: 1\nid: base\nresources:\n  renamed:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: new\n      target: ~/target\n",
        );

        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &[
                "replace_ownership",
                "base/item",
                "base/renamed",
                "apply completed: 1 committed actions",
            ],
        );
        assert_eq!(
            fs::read_link(f.path("home/target")).unwrap(),
            f.path("store/new")
        );
        assert!(state(&f)["resources"]["base/item"].is_null());
        assert!(state(&f)["resources"]["base/renamed"].is_object());
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn normal_decline_can_follow_recovery_without_starting_a_new_operation() {
        let f = Fixture::new();
        f.state(json!({}), json!({"id":"old-pending", "desired_hash":format!("sha256:{}", "a".repeat(64)),
            "actions":{"a1":{"kind":"create_link","resource_id":"base/item","target_path":f.path("home/target"),
                "precondition":{"target":"missing"},"postcondition":{"target":"expected_link","link_target":f.path("store/source")},"status":"pending"}}
        }));
        expect(
            f.command().args(ARGS).output().unwrap(),
            2,
            &["requires --yes", "create_link"],
        );
        no_new_operation(&f);
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn confirmation_requires_both_terminals_and_yes_bypasses_only_confirmation() {
        for (input, error) in [(true, false), (false, true), (false, false)] {
            let f = Fixture::new();
            let mut session = Session::start(&f, input, error, &[]);
            let (code, text) = session.finish();
            assert_eq!(code, 2, "{text}");
            assert!(text.contains("requires --yes"));
            assert!(!text.contains("Apply this plan?"));
            no_new_operation(&f);
        }
        let f = Fixture::new();
        let output = f.command().args(ARGS).arg("--yes").output().unwrap();
        expect(
            output,
            0,
            &["executable", "base/item", "apply completed: 1"],
        );
        assert_eq!(
            fs::read_link(f.path("home/target")).unwrap(),
            f.path("store/source")
        );
        assert!(state(&f)["active_operation"].is_null());
        assert!(state(&f)["resources"]["base/item"].is_object());
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["noop", "apply completed: 0"],
        );
        let g = Fixture::new();
        g.write("home/target", "unmanaged");
        expect(
            g.command().args(ARGS).arg("--yes").output().unwrap(),
            2,
            &["blocked", "base/item", "target"],
        );
        assert_eq!(
            fs::read_to_string(g.path("home/target")).unwrap(),
            "unmanaged"
        );
        assert!(!g.path("state/loadout/state.json").exists());
    }

    #[test]
    fn compiled_binary_removes_a_stale_owned_link_and_preserves_its_referent() {
        let f = Fixture::new();
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["apply completed: 1"],
        );
        f.write(
            "portable/profiles/base.yaml",
            "schema_version: 1\nid: base\nresources: {}\n",
        );

        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["remove_link", "apply completed: 1"],
        );
        assert!(!f.path("home/target").exists());
        assert_eq!(
            fs::read_to_string(f.path("store/source")).unwrap(),
            "content\n"
        );
        assert!(state(&f)["resources"].as_object().unwrap().is_empty());
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn compiled_binary_relocates_contiguously_and_commits_the_new_known_target() {
        let f = Fixture::new();
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["apply completed: 1"],
        );
        fs::create_dir(f.path("home/moved")).unwrap();
        f.profile("base", "item", "~/moved/target");

        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["relocate_link", "apply completed: 1"],
        );
        assert!(!f.path("home/target").exists());
        assert_eq!(
            fs::read_link(f.path("home/moved/target")).unwrap(),
            f.path("store/source")
        );
        assert_eq!(
            fs::read_to_string(f.path("store/source")).unwrap(),
            "content\n"
        );
        assert_eq!(
            state(&f)["resources"]["base/item"]["file_link"]["target_path"],
            json!(f.path("home/moved/target"))
        );
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn yes_and_dry_run_do_not_prompt_with_both_terminals() {
        for flags in [vec!["--yes"], vec!["--dry-run"], vec!["--dry-run", "--yes"]] {
            let f = Fixture::new();
            let before = f.snapshot();
            let mut session = Session::start(&f, true, true, &flags);
            let (code, text) = session.finish();
            assert_eq!(code, 0, "{text}");
            assert!(!text.contains("Apply this plan?"), "{text}");
            if flags.contains(&"--dry-run") {
                assert_eq!(f.snapshot(), before);
            } else {
                assert!(state(&f)["resources"]["base/item"].is_object());
            }
        }
    }

    #[test]
    fn interactive_prompt_precedes_operation_and_accepts_only_affirmative_response() {
        for (answer, expected) in [("YES\n", 0), ("n\n", 2), ("\n", 2)] {
            let f = Fixture::new();
            let mut session = Session::start(&f, true, true, &[]);
            session.prompt();
            no_new_operation(&f);
            assert!(!f.path("state/loadout/state.json").exists());
            if expected == 0 {
                expect(
                    f.command().args(ARGS).arg("--yes").output().unwrap(),
                    1,
                    &["LockAndState"],
                );
                expect(
                    f.run(&[ARGS, &["--dry-run"]].concat()),
                    0,
                    &["executable", "create_link"],
                );
                no_new_operation(&f);
            }
            session.answer(answer);
            let (code, text) = session.finish();
            assert_eq!(code, expected, "{text}");
            assert!(text.contains("create_link base/item"));
            if expected == 2 {
                no_new_operation(&f);
            } else {
                assert!(state(&f)["resources"]["base/item"].is_object());
            }
        }
    }

    #[test]
    fn operation_creation_commit_failure_is_runtime_failure_without_target_mutation() {
        let f = Fixture::new();
        let mut session = Session::start(&f, true, true, &[]);
        session.prompt();
        // Deterministic failure at state replacement, after successful preflight.
        fs::create_dir(f.path("state/loadout/state.json")).unwrap();
        session.answer("y\n");
        let (code, text) = session.finish();
        assert_eq!(code, 1, "{text}");
        for needle in [
            "OperationCreation",
            "previous state retained",
            "operation: absent",
            "committed actions: 0",
            "base/item",
        ] {
            assert!(text.contains(needle), "{text}");
        }
        assert!(fs::symlink_metadata(f.path("home/target")).is_err());
        assert!(f.path("state/loadout/state.json").is_dir());
    }

    #[test]
    fn blocked_replacement_never_prompts_or_creates_an_operation() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(f.path("store/source"), f.path("home/target")).unwrap();
        f.state(json!({"base/item":f.known("target")}), Value::Null);
        fs::remove_file(f.path("home/target")).unwrap();
        f.write("home/target", "unmanaged target");
        let before = state(&f);
        for extra in [vec![], vec!["--yes"]] {
            let output = f.command().args(ARGS).args(extra).output().unwrap();
            let code = output.status.code().unwrap();
            let text = text(&output);
            assert_eq!(code, 2, "{text}");
            assert!(text.contains("blocked"));
            assert!(text.contains("base/item"));
            assert!(!text.contains("Apply this plan?"));
            assert!(!text.contains("executable plan"));
            assert_eq!(state(&f), before);
        }
    }

    #[test]
    fn target_change_after_confirmation_reports_partial_progress_and_execution_uncertainty() {
        for first_succeeds in [false, true] {
            let f = Fixture::new();
            let profile = fs::read_to_string(f.path("portable/profiles/base.yaml")).unwrap();
            let item = profile.split("resources:\n").nth(1).unwrap();
            f.write(
                "portable/profiles/base.yaml",
                &format!(
                    "{profile}{}{}",
                    item.replace("item:", "second:")
                        .replace("~/target", "~/second"),
                    item.replace("item:", "third:")
                        .replace("~/target", "~/third")
                ),
            );
            let mut session = Session::start(&f, true, true, &[]);
            session.prompt();
            no_new_operation(&f);
            let changed = if first_succeeds {
                "home/second"
            } else {
                "home/target"
            };
            f.write(changed, "unmanaged raced entry");
            session.answer("y\n");
            let (code, text) = session.finish();
            assert_eq!(code, 1, "{text}");
            assert!(text.contains("Execution"), "{text}");
            assert!(text.contains("operation retained"), "{text}");
            assert!(text.contains("Uncertain"), "{text}");
            assert!(
                text.contains(if first_succeeds {
                    "committed actions: 1"
                } else {
                    "committed actions: 0"
                }),
                "{text}"
            );
            assert_eq!(
                fs::read_to_string(f.path(changed)).unwrap(),
                "unmanaged raced entry"
            );
            assert!(!f.path("home/third").exists());
            let persisted = state(&f);
            let statuses: Vec<_> = persisted["active_operation"]["actions"]
                .as_object()
                .unwrap()
                .values()
                .map(|a| a["status"].as_str().unwrap())
                .collect();
            assert!(statuses.contains(&"uncertain"));
            assert!(statuses.contains(&"skipped"));
            assert_eq!(
                persisted["resources"].as_object().unwrap().len(),
                usize::from(first_succeeds)
            );
            // An ensuing invocation is initial recovery uncertainty, even if declarations are broken.
            f.write("portable/config.yaml", "invalid");
            expect(
                f.command().args(ARGS).arg("--yes").output().unwrap(),
                2,
                &["Recovery", "operation retained", "Uncertain"],
            );
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        fs::OpenOptions,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{
        Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };

    fn hold_state_lock(f: &Fixture) -> fs::File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0x7)
            .open(f.path("state/loadout/state.lock"))
            .unwrap();
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: `file` remains open for the whole locked range and `overlapped` is valid for this synchronous call.
        assert_ne!(
            unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            },
            0
        );
        file
    }

    fn create_owned_link(f: &Fixture) {
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["create_link", "apply completed: 1 committed actions"],
        );
    }

    #[test]
    fn native_windows_create_reaches_confirmation_before_operation_progress() {
        let f = Fixture::new();
        expect(
            f.command().args(ARGS).output().unwrap(),
            2,
            &["create_link", "confirmation unavailable"],
        );
        no_new_operation(&f);
    }

    #[test]
    fn privilege_is_not_reported_as_a_permanent_preflight_capability_probe() {
        let f = Fixture::new();
        expect(
            f.command().args(ARGS).output().unwrap(),
            2,
            &["create_link", "confirmation unavailable"],
        );
        no_new_operation(&f);
    }

    #[test]
    fn independent_process_lock_contention_prevents_apply_before_target_observation() {
        let f = Fixture::new();
        f.write("state/loadout/state.lock", "");
        let before = f.snapshot();
        let held = hold_state_lock(&f);

        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            1,
            &["LockAndState", "lock"],
        );
        no_new_operation(&f);

        drop(held);
        assert_eq!(f.snapshot(), before);
        create_owned_link(&f);
    }

    #[test]
    fn native_windows_replacement_removal_and_relocation_commit_through_the_binary() {
        for (declaration, action, expected_target) in [
            ("source: new\ntarget: ~/target", "replace_link", "target"),
            (
                "source: new\ntarget: ~/target\nresource: renamed",
                "replace_ownership",
                "target",
            ),
            ("source: source\ntarget: ~/moved", "relocate_link", "moved"),
            ("", "remove_link", ""),
        ] {
            let f = Fixture::new();
            create_owned_link(&f);
            if declaration.is_empty() {
                f.write(
                    "portable/profiles/base.yaml",
                    "schema_version: 1\nid: base\nresources: {}\n",
                );
            } else {
                f.write("store/new", "new source");
                let (source, target, resource) = if action == "replace_ownership" {
                    ("new", "~/target", "renamed")
                } else if action == "replace_link" {
                    ("new", "~/target", "item")
                } else {
                    ("source", "~/moved", "item")
                };
                f.write(
                    "portable/profiles/base.yaml",
                    &format!("schema_version: 1\nid: base\nresources:\n  {resource}:\n    type: file\n    properties:\n      kind: file\n      operation: link\n      source:\n        store: files\n        path: {source}\n      target: {target}\n"),
                );
            }
            expect(
                f.command().args(ARGS).arg("--yes").output().unwrap(),
                0,
                &[action, "apply completed: 1 committed actions"],
            );
            assert!(state(&f)["active_operation"].is_null());
            if action == "remove_link" {
                assert!(fs::symlink_metadata(f.path("home/target")).is_err());
                assert!(state(&f)["resources"].as_object().unwrap().is_empty());
            } else {
                let source = if action == "relocate_link" {
                    "source"
                } else {
                    "new"
                };
                assert_eq!(
                    fs::read_link(f.path(&format!("home/{expected_target}"))).unwrap(),
                    f.path(&format!("store/{source}"))
                );
            }
        }
    }

    #[test]
    fn native_windows_normal_representation_supports_noop_and_same_source_handoff() {
        let f = Fixture::new();
        create_owned_link(&f);
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["noop", "base/item", "apply completed: 0"],
        );
        assert_eq!(
            fs::read_link(f.path("home/target")).unwrap(),
            f.path("store/source")
        );
        assert!(state(&f)["resources"]["base/item"].is_object());

        f.profile("base", "renamed", "~/target");
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &[
                "replace_ownership",
                "base/item",
                "base/renamed",
                "apply completed: 1 committed actions",
            ],
        );
        assert_eq!(
            fs::read_link(f.path("home/target")).unwrap(),
            f.path("store/source")
        );
        assert!(state(&f)["resources"]["base/item"].is_null());
        assert!(state(&f)["resources"]["base/renamed"].is_object());
        assert!(state(&f)["active_operation"].is_null());
    }

    #[test]
    fn native_windows_forget_and_remove_are_atomic_with_state_preflight() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        create_owned_link(&f);
        f.write(
            "portable/profiles/base.yaml",
            "schema_version: 1\nid: base\nresources: {}\n",
        );
        f.state(
            json!({"base/a":f.known("missing"), "base/z":f.known("target")}),
            Value::Null,
        );
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &[
                "forget_missing",
                "remove_link",
                "apply completed: 2 committed actions",
            ],
        );
        assert!(state(&f)["resources"].as_object().unwrap().is_empty());
        assert!(fs::symlink_metadata(f.path("home/target")).is_err());

        let f = Fixture::new();
        f.write(
            "portable/profiles/base.yaml",
            "schema_version: 1\nid: base\nresources: {}\n",
        );
        f.state(json!({"base/a":f.known("missing")}), Value::Null);
        {
            let before = state(&f);
            let _held = fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(f.path("state/loadout/state.json"))
                .unwrap();
            expect(
                f.command().args(ARGS).arg("--yes").output().unwrap(),
                2,
                &[
                    "Preflight",
                    "not writable",
                    "committed actions: 0",
                    "operation: absent",
                ],
            );
            assert_eq!(state(&f), before);
        }
        expect(
            f.command().args(ARGS).arg("--yes").output().unwrap(),
            0,
            &["forget_missing"],
        );
        assert_eq!(state(&f)["resources"], json!({}));
        assert!(state(&f)["active_operation"].is_null());
        assert!(fs::read_dir(f.path("home")).unwrap().next().is_none());
    }
}
