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
                        std::ptr::null(),
                        std::ptr::null(),
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
                if unsafe { libc::poll(&mut poll, 1, 0) } <= 0 || poll.revents & libc::POLLIN == 0 {
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
    fn dry_run_leaves_recoverable_replacement_and_temporary_link_untouched() {
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
        // This backend cannot authorize temporary-link removal: normal recovery records uncertainty.
        expect(
            f.command().args(ARGS).output().unwrap(),
            2,
            &["Recovery", "operation retained", "Uncertain"],
        );
        assert_eq!(
            fs::read_link(f.path("home/recorded-temporary")).unwrap(),
            f.path("store/new")
        );
        assert_eq!(
            state(&f)["active_operation"]["actions"]["a1"]["status"],
            "uncertain"
        );
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
    fn failed_preflight_never_prompts_or_creates_an_operation() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(f.path("store/source"), f.path("home/target")).unwrap();
        f.state(json!({"base/item":f.known("target")}), Value::Null);
        f.write("store/changed", "new source");
        let profile = fs::read_to_string(f.path("portable/profiles/base.yaml"))
            .unwrap()
            .replace("path: source", "path: changed");
        f.write("portable/profiles/base.yaml", &profile);
        let before = state(&f);
        for extra in [vec![], vec!["--yes"]] {
            let mut session = Session::start(&f, true, true, &extra);
            let (code, text) = session.finish();
            assert_eq!(code, 2, "{text}");
            assert!(text.contains("Preflight"));
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
