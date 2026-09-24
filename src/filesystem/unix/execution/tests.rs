use super::*;
use crate::test_support::{ExecutionBoundary as Boundary, on_execution_boundary};
use sha2::{Digest, Sha256};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loadout-execution-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let f = Self(fs::canonicalize(path).unwrap());
        fs::create_dir_all(f.0.join("home/parent")).unwrap();
        fs::create_dir(f.0.join("store")).unwrap();
        fs::write(f.0.join("store/old"), "old contents").unwrap();
        fs::write(f.0.join("store/new"), "new contents").unwrap();
        f
    }
    fn path(&self, p: &str) -> ResolvedPath {
        ResolvedPath::new(self.0.join(p)).unwrap()
    }
    fn target(&self) -> ExecutionTarget {
        ExecutionTarget::open(&self.path("home"), &self.path("home/parent/target")).unwrap()
    }
    fn old(&self) -> LinkTarget {
        LinkTarget::new(self.path("store/old"))
    }
    fn new_link(&self) -> LinkTarget {
        LinkTarget::new(self.path("store/new"))
    }
    fn link(&self, at: &str, value: &LinkTarget) {
        symlink(value.as_path().as_ref(), self.path(at).as_ref()).unwrap();
    }
    fn assert_sources(&self) {
        assert_eq!(
            fs::read_to_string(self.path("store/old").as_ref()).unwrap(),
            "old contents"
        );
        assert_eq!(
            fs::read_to_string(self.path("store/new").as_ref()).unwrap(),
            "new contents"
        );
        assert!(self.0.join("home/parent").is_dir());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fingerprint(contents: &[u8]) -> ContentFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    ContentFingerprint::parse(format!("sha256:{:x}", hasher.finalize())).unwrap()
}

#[test]
fn copy_temporary_is_flushed_verified_and_published_without_replacing_a_target() {
    let f = Fixture::new();
    let target = f.target();
    let temporary_path = f.path("home/parent/.loadout-copy-a1");
    let temporary = ExecutionTarget::open(&f.path("home"), &temporary_path).unwrap();
    let expected = fingerprint(b"new contents");

    temporary
        .copy_source_to_temporary(&f.path("store"), &f.path("store/new"), &expected)
        .unwrap();
    target
        .publish_copy_no_replace(&temporary_path, &expected)
        .unwrap();

    assert_eq!(
        fs::read(f.path("home/parent/target").as_ref()).unwrap(),
        b"new contents"
    );
    assert!(fs::symlink_metadata(temporary_path.as_ref()).is_err());
    f.assert_sources();
}

#[test]
fn copy_no_replace_preserves_an_entry_that_appears_after_the_final_recheck() {
    let f = Fixture::new();
    let target = f.target();
    let temporary_path = f.path("home/parent/.loadout-copy-a1");
    let temporary = ExecutionTarget::open(&f.path("home"), &temporary_path).unwrap();
    let expected = fingerprint(b"new contents");
    temporary
        .copy_source_to_temporary(&f.path("store"), &f.path("store/new"), &expected)
        .unwrap();
    let final_path = f.path("home/parent/target");
    let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
        fs::write(final_path.as_ref(), "unmanaged")
    });

    assert_eq!(
        target
            .publish_copy_no_replace(&temporary_path, &expected)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        fs::read_to_string(f.path("home/parent/target").as_ref()).unwrap(),
        "unmanaged"
    );
    assert_eq!(fs::read(temporary_path.as_ref()).unwrap(), b"new contents");
    f.assert_sources();
}

#[test]
fn copy_replace_and_removal_require_the_expected_owned_fingerprint() {
    let f = Fixture::new();
    let target = f.target();
    let temporary_path = f.path("home/parent/.loadout-copy-a1");
    let temporary = ExecutionTarget::open(&f.path("home"), &temporary_path).unwrap();
    let old = fingerprint(b"old copy bytes");
    let new = fingerprint(b"new contents");
    fs::write(f.path("home/parent/target").as_ref(), b"old copy bytes").unwrap();
    temporary
        .copy_source_to_temporary(&f.path("store"), &f.path("store/new"), &new)
        .unwrap();

    target
        .replace_copy_from_temporary(&temporary_path, &old, &new)
        .unwrap();
    assert_eq!(
        target.observe_copy(Some(&new)).unwrap(),
        CopyTargetObservation::ExpectedCopy {
            content_fingerprint: new.clone(),
        }
    );
    assert!(fs::symlink_metadata(temporary_path.as_ref()).is_err());
    assert!(target.remove_expected_copy(&old).is_err());
    assert_eq!(
        fs::read(f.path("home/parent/target").as_ref()).unwrap(),
        b"new contents"
    );
    target.remove_expected_copy(&new).unwrap();
    assert_eq!(
        target.observe_copy(Some(&new)).unwrap(),
        CopyTargetObservation::Missing
    );
    f.assert_sources();
}

#[test]
fn link_to_copy_handoff_requires_the_expected_link_and_verified_temporary() {
    let f = Fixture::new();
    let target = f.target();
    let temporary_path = f.path("home/parent/.loadout-copy-a1");
    let temporary = ExecutionTarget::open(&f.path("home"), &temporary_path).unwrap();
    let expected = fingerprint(b"new contents");
    f.link("home/parent/target", &f.old());
    temporary
        .copy_source_to_temporary(&f.path("store"), &f.path("store/new"), &expected)
        .unwrap();

    target
        .replace_link_with_copy_temporary(&temporary_path, &f.old(), &expected)
        .unwrap();

    assert_eq!(
        target.observe_copy(Some(&expected)).unwrap(),
        CopyTargetObservation::ExpectedCopy {
            content_fingerprint: expected,
        }
    );
    assert!(fs::symlink_metadata(temporary_path.as_ref()).is_err());
    f.assert_sources();
}

#[test]
fn create_remove_and_replace_preserve_sources_and_parent() {
    let f = Fixture::new();
    let context = f.target();
    let old = f.old();
    context
        .prepare_create(&f.path("store"), &old)
        .unwrap()
        .attempt()
        .unwrap();
    assert_eq!(
        context.observe(&old).unwrap(),
        TargetObservation::ExpectedLink {
            link_target: old.clone()
        }
    );
    let new = f.new_link();
    let temp = f.path("home/parent/temporary");
    let temporary = ExecutionTarget::open(&f.path("home"), &temp).unwrap();
    temporary
        .prepare_create(&f.path("store"), &new)
        .unwrap()
        .attempt()
        .unwrap();
    context
        .prepare_replace(&temp, &old, &new, &f.path("store"))
        .unwrap()
        .attempt()
        .unwrap();
    assert_eq!(
        context.observe(&new).unwrap(),
        TargetObservation::ExpectedLink {
            link_target: new.clone()
        }
    );
    assert_eq!(temporary.observe(&new).unwrap(), TargetObservation::Missing);
    context.prepare_remove(&new).unwrap().attempt().unwrap();
    assert_eq!(context.observe(&new).unwrap(), TargetObservation::Missing);
    f.assert_sources();
}

#[test]
fn removal_does_not_follow_a_dangling_link_and_normalizes_relative_values() {
    let f = Fixture::new();
    symlink("../../store/./old", f.path("home/parent/target").as_ref()).unwrap();
    fs::remove_file(f.path("store/old").as_ref()).unwrap();
    f.target()
        .prepare_remove(&f.old())
        .unwrap()
        .attempt()
        .unwrap();
    assert_eq!(
        f.target().observe(&f.old()).unwrap(),
        TargetObservation::Missing
    );
    assert!(f.0.join("home/parent").is_dir());
}

#[test]
fn changed_entries_are_rejected_before_mutation() {
    for kind in ["regular", "wrong", "directory", "missing"] {
        let f = Fixture::new();
        let path = f.path("home/parent/target");
        match kind {
            "regular" => fs::write(path.as_ref(), "unmanaged").unwrap(),
            "wrong" => f.link("home/parent/target", &f.new_link()),
            "directory" => fs::create_dir(path.as_ref()).unwrap(),
            _ => (),
        }
        let ctx = f.target();
        let before = ctx.observe(&f.old()).unwrap();
        assert!(ctx.prepare_remove(&f.old()).is_err());
        assert_eq!(ctx.observe(&f.old()).unwrap(), before);
        f.assert_sources();
    }
}

#[test]
fn create_never_replaces_an_entry_appearing_after_final_recheck() {
    let f = Fixture::new();
    let path = f.path("home/parent/target");
    let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
        fs::write(path.as_ref(), "unmanaged")
    });
    let ctx = f.target();
    let old = f.old();
    assert_eq!(
        ctx.prepare_create(&f.path("store"), &old)
            .unwrap()
            .attempt()
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        fs::read_to_string(f.path("home/parent/target").as_ref()).unwrap(),
        "unmanaged"
    );
    f.assert_sources();
}

#[test]
fn substitution_before_recheck_is_rejected_but_after_recheck_can_be_removed() {
    for boundary in [Boundary::BeforeFinalRecheck, Boundary::AfterFinalRecheck] {
        let f = Fixture::new();
        f.link("home/parent/target", &f.old());
        let ctx = f.target();
        let path = f.path("home/parent/target");
        let _hook = on_execution_boundary(boundary, move || {
            fs::remove_file(path.as_ref())?;
            fs::write(path.as_ref(), "substituted unmanaged file")
        });
        match ctx.prepare_remove(&f.old()) {
            Err(_) => {
                assert_eq!(boundary, Boundary::BeforeFinalRecheck);
                assert_eq!(
                    fs::read_to_string(f.path("home/parent/target").as_ref()).unwrap(),
                    "substituted unmanaged file"
                );
            }
            Ok(checked) => {
                assert_eq!(boundary, Boundary::AfterFinalRecheck);
                checked.attempt().unwrap();
                // Production observations cannot distinguish this from ordinary
                // removal. Classification/Known integration belongs to S3.
                assert_eq!(ctx.observe(&f.old()).unwrap(), TargetObservation::Missing);
            }
        }
        f.assert_sources();
    }
}

#[test]
fn renamed_or_replaced_parent_never_produces_handle_local_success() {
    for boundary in [
        Boundary::BeforeFinalRecheck,
        Boundary::AfterFinalRecheck,
        Boundary::BeforePostObservation,
    ] {
        for replace in [false, true] {
            let f = Fixture::new();
            f.link("home/parent/target", &f.old());
            let ctx = f.target();
            let root = f.0.clone();
            let _hook = on_execution_boundary(boundary, move || {
                fs::rename(root.join("home/parent"), root.join("detached"))?;
                if replace {
                    fs::create_dir(root.join("home/parent"))?;
                }
                Ok(())
            });
            if boundary == Boundary::BeforeFinalRecheck {
                assert!(ctx.prepare_remove(&f.old()).is_err());
                assert!(
                    fs::symlink_metadata(f.0.join("detached/target"))
                        .unwrap()
                        .is_symlink()
                );
            } else {
                ctx.prepare_remove(&f.old()).unwrap().attempt().unwrap();
                assert!(ctx.observe(&f.old()).is_err());
                assert!(!f.0.join("detached/target").exists());
            }
            assert!(f.0.join("store/old").is_file());
        }
    }
}

#[test]
fn changed_ancestor_is_detected_even_when_original_parent_is_moved_back() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("home/parent/nested")).unwrap();
    let target =
        ExecutionTarget::open(&f.path("home"), &f.path("home/parent/nested/target")).unwrap();
    fs::rename(f.0.join("home/parent"), f.0.join("old-parent")).unwrap();
    fs::create_dir(f.0.join("home/parent")).unwrap();
    fs::rename(
        f.0.join("old-parent/nested"),
        f.0.join("home/parent/nested"),
    )
    .unwrap();
    assert!(target.prepare_create(&f.path("store"), &f.old()).is_err());
    assert!(target.observe(&f.old()).is_err());
    assert!(!f.0.join("home/parent/nested/target").exists());
}

#[test]
fn unsafe_parent_root_and_outside_paths_are_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("outside")).unwrap();
    symlink(f.0.join("outside"), f.0.join("home/linked")).unwrap();
    fs::write(f.0.join("home/file"), "not a parent").unwrap();
    for path in [
        "home/linked/target",
        "home/file/target",
        "home/missing/target",
        "outside/target",
        "home",
    ] {
        assert!(
            ExecutionTarget::open(&f.path("home"), &f.path(path)).is_err(),
            "{path}"
        );
    }
    let ctx = f.target();
    fs::rename(f.0.join("home"), f.0.join("old-home")).unwrap();
    symlink(f.0.join("old-home"), f.0.join("home")).unwrap();
    assert!(ctx.prepare_remove(&f.old()).is_err());
    assert!(ctx.observe(&f.old()).is_err());
    assert_eq!(fs::read_dir(f.0.join("outside")).unwrap().count(), 0);
}

#[test]
fn replacement_checks_both_entries_and_requires_a_distinct_recorded_sibling() {
    for altered in ["target", "temporary"] {
        let f = Fixture::new();
        f.link("home/parent/target", &f.old());
        f.link("home/parent/temporary", &f.new_link());
        let ctx = f.target();
        let temp = f.path("home/parent/temporary");
        let path = f.path(&format!("home/parent/{altered}"));
        fs::remove_file(path.as_ref()).unwrap();
        fs::write(path.as_ref(), "unmanaged").unwrap();
        assert!(
            ctx.prepare_replace(&temp, &f.old(), &f.new_link(), &f.path("store"))
                .is_err()
        );
        assert_eq!(fs::read_to_string(path.as_ref()).unwrap(), "unmanaged");
        assert!(fs::symlink_metadata(temp.as_ref()).is_ok());
        f.assert_sources();
    }
    let f = Fixture::new();
    f.link("home/parent/target", &f.old());
    for temporary in ["home/parent/target", "home/other-temp", "outside/temp"] {
        assert!(
            f.target()
                .prepare_replace(
                    &f.path(temporary),
                    &f.old(),
                    &f.new_link(),
                    &f.path("store")
                )
                .is_err()
        );
    }
}

#[test]
fn failed_rename_preserves_old_target_and_post_effect_error_preserves_observation() {
    let f = Fixture::new();
    f.link("home/parent/target", &f.old());
    f.link("home/parent/temporary", &f.new_link());
    let ctx = f.target();
    let temp = f.path("home/parent/temporary");
    let path = temp.clone();
    let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
        fs::remove_file(path.as_ref())
    });
    let checked = ctx
        .prepare_replace(&temp, &f.old(), &f.new_link(), &f.path("store"))
        .unwrap();
    assert_eq!(
        checked.attempt().unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        ctx.observe(&f.old()).unwrap(),
        TargetObservation::ExpectedLink {
            link_target: f.old()
        }
    );
    f.assert_sources();
    drop(_hook);
    let _hook = on_execution_boundary(Boundary::AfterMutationAttempt, || {
        Err(io::Error::other("error after effect"))
    });
    assert!(ctx.prepare_remove(&f.old()).unwrap().attempt().is_err());
    assert_eq!(ctx.observe(&f.old()).unwrap(), TargetObservation::Missing);
}

#[test]
fn source_changes_observable_before_recheck_reject_both_create_and_replacement() {
    for replace in [false, true] {
        let f = Fixture::new();
        if replace {
            f.link("home/parent/target", &f.old());
            f.link("home/parent/temporary", &f.new_link());
        }
        let ctx = f.target();
        let path = f.path("store/new");
        let old = f.old();
        let _hook = on_execution_boundary(Boundary::BeforeFinalRecheck, move || {
            fs::remove_file(path.as_ref())?;
            symlink(old.as_path().as_ref(), path.as_ref())
        });
        if replace {
            assert!(
                ctx.prepare_replace(
                    &f.path("home/parent/temporary"),
                    &f.old(),
                    &f.new_link(),
                    &f.path("store")
                )
                .is_err()
            );
            assert_eq!(
                ctx.observe(&f.old()).unwrap(),
                TargetObservation::ExpectedLink {
                    link_target: f.old()
                }
            );
        } else {
            assert!(ctx.prepare_create(&f.path("store"), &f.new_link()).is_err());
            assert_eq!(ctx.observe(&f.old()).unwrap(), TargetObservation::Missing);
        }
    }
}

#[test]
fn source_change_after_recheck_is_not_excluded_and_observation_failure_is_not_success() {
    let f = Fixture::new();
    let ctx = f.target();
    let old = f.old();
    let path = f.path("store/old");
    let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
        fs::remove_file(path.as_ref())
    });
    ctx.prepare_create(&f.path("store"), &old)
        .unwrap()
        .attempt()
        .unwrap();
    assert_eq!(
        ctx.observe(&old).unwrap(),
        TargetObservation::ExpectedLink {
            link_target: old.clone()
        }
    );
    drop(_hook);
    let _hook = on_execution_boundary(Boundary::BeforePostObservation, || {
        Err(io::Error::other("observation unavailable"))
    });
    assert!(ctx.observe(&old).is_err());
}

#[test]
fn interrupted_temporary_creation_can_clean_only_the_exact_rechecked_link() {
    for changed in [false, true] {
        let f = Fixture::new();
        f.link("home/parent/target", &f.old());
        fs::write(f.0.join("home/parent/unrelated"), "preserve sibling").unwrap();
        let temp_path = f.path("home/parent/temporary");
        let temp = ExecutionTarget::open(&f.path("home"), &temp_path).unwrap();
        let _hook = on_execution_boundary(Boundary::AfterTemporaryCreation, || {
            Err(io::Error::other("interrupted after temporary creation"))
        });
        let new = f.new_link();
        assert!(
            temp.prepare_create(&f.path("store"), &new)
                .unwrap()
                .attempt_temporary()
                .is_err()
        );
        assert_eq!(
            temp.observe(&new).unwrap(),
            TargetObservation::ExpectedLink {
                link_target: new.clone()
            }
        );
        if changed {
            fs::remove_file(temp_path.as_ref()).unwrap();
            fs::write(temp_path.as_ref(), "unexpected temporary").unwrap();
            assert!(temp.prepare_remove(&new).is_err());
            assert_eq!(
                fs::read_to_string(temp_path.as_ref()).unwrap(),
                "unexpected temporary"
            );
        } else {
            temp.prepare_remove(&new).unwrap().attempt().unwrap();
            assert_eq!(temp.observe(&new).unwrap(), TargetObservation::Missing);
        }
        assert_eq!(
            f.target().observe(&f.old()).unwrap(),
            TargetObservation::ExpectedLink {
                link_target: f.old()
            }
        );
        assert_eq!(
            fs::read_to_string(f.0.join("home/parent/unrelated")).unwrap(),
            "preserve sibling"
        );
        f.assert_sources();
    }
}

#[test]
fn source_parent_substitution_is_rejected_without_target_mutation() {
    let f = Fixture::new();
    fs::create_dir(f.0.join("store/nested")).unwrap();
    fs::write(f.0.join("store/nested/source"), "source").unwrap();
    let source = LinkTarget::new(f.path("store/nested/source"));
    let root = f.0.clone();
    let _hook = on_execution_boundary(Boundary::BeforeFinalRecheck, move || {
        fs::rename(root.join("store/nested"), root.join("outside-source"))?;
        symlink(root.join("outside-source"), root.join("store/nested"))
    });
    assert!(
        f.target()
            .prepare_create(&f.path("store"), &source)
            .is_err()
    );
    assert_eq!(
        f.target().observe(&source).unwrap(),
        TargetObservation::Missing
    );
    assert_eq!(
        fs::read_to_string(f.0.join("outside-source/source")).unwrap(),
        "source"
    );
}

#[test]
fn replacement_after_recheck_substitution_has_only_observational_guarantees() {
    for which in ["target", "temporary"] {
        let f = Fixture::new();
        f.link("home/parent/target", &f.old());
        f.link("home/parent/temporary", &f.new_link());
        let ctx = f.target();
        let temp = f.path("home/parent/temporary");
        let changed = f.path(&format!("home/parent/{which}"));
        let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
            fs::remove_file(changed.as_ref())?;
            fs::write(changed.as_ref(), "unmanaged substitution")
        });
        ctx.prepare_replace(&temp, &f.old(), &f.new_link(), &f.path("store"))
            .unwrap()
            .attempt()
            .unwrap();
        let after = ctx.observe(&f.new_link()).unwrap();
        if which == "target" {
            assert_eq!(
                after,
                TargetObservation::ExpectedLink {
                    link_target: f.new_link()
                }
            );
        } else {
            assert_eq!(
                after,
                TargetObservation::OtherEntry {
                    kind: OtherEntryKind::RegularFile
                }
            );
            assert_eq!(
                fs::read_to_string(f.path("home/parent/target").as_ref()).unwrap(),
                "unmanaged substitution"
            );
        }
        assert!(
            matches!(fs::symlink_metadata(temp.as_ref()), Err(e) if e.kind() == io::ErrorKind::NotFound)
        );
        f.assert_sources();
    }
}

#[test]
fn changed_root_object_and_directory_substitution_are_rejected() {
    let f = Fixture::new();
    let ctx = f.target();
    fs::rename(f.0.join("home"), f.0.join("old-home")).unwrap();
    fs::create_dir_all(f.0.join("home/parent")).unwrap();
    assert!(ctx.prepare_create(&f.path("store"), &f.old()).is_err());
    assert!(ctx.observe(&f.old()).is_err());
    assert!(!f.0.join("home/parent/target").exists());

    let f = Fixture::new();
    f.link("home/parent/target", &f.old());
    let path = f.path("home/parent/target");
    let _hook = on_execution_boundary(Boundary::AfterFinalRecheck, move || {
        fs::remove_file(path.as_ref())?;
        fs::create_dir(path.as_ref())
    });
    assert!(
        f.target()
            .prepare_remove(&f.old())
            .unwrap()
            .attempt()
            .is_err()
    );
    assert!(f.0.join("home/parent/target").is_dir());
    f.assert_sources();
}

#[test]
fn changed_declared_home_alias_rejects_before_attempt_or_postobservation() {
    for boundary in [
        Boundary::BeforeFinalRecheck,
        Boundary::BeforePostObservation,
    ] {
        let f = Fixture::new();
        symlink(f.path("home").as_ref(), f.path("alias").as_ref()).unwrap();
        fs::create_dir(f.path("outside-home").as_ref()).unwrap();
        let ctx = ExecutionTarget::open_with_declared_root(
            &f.path("home"),
            &f.path("alias"),
            &f.path("home/parent/target"),
        )
        .unwrap();
        let root = f.0.clone();
        let _hook = on_execution_boundary(boundary, move || {
            fs::remove_file(root.join("alias"))?;
            symlink(root.join("outside-home"), root.join("alias"))
        });
        let old = f.old();
        if boundary == Boundary::BeforeFinalRecheck {
            assert!(ctx.prepare_create(&f.path("store"), &old).is_err());
            assert!(!f.0.join("home/parent/target").exists());
        } else {
            ctx.prepare_create(&f.path("store"), &old)
                .unwrap()
                .attempt()
                .unwrap();
            assert!(ctx.observe(&old).is_err());
            assert!(
                fs::symlink_metadata(f.0.join("home/parent/target"))
                    .unwrap()
                    .is_symlink()
            );
        }
        assert_eq!(fs::read_dir(f.0.join("outside-home")).unwrap().count(), 0);
    }
}
