//! Thread-local assertions for forbidden boundary calls in read-only tests.

use std::cell::Cell;

thread_local! {
    static FORBID_MUTATION: Cell<bool> = const { Cell::new(false) };
    static FORBID_TARGET_INSPECTION: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct ReadOnlyGuard {
    previous: bool,
}

pub(crate) fn forbid_mutation() -> ReadOnlyGuard {
    ReadOnlyGuard {
        previous: FORBID_MUTATION.replace(true),
    }
}

impl Drop for ReadOnlyGuard {
    fn drop(&mut self) {
        FORBID_MUTATION.set(self.previous);
    }
}

pub(crate) fn assert_mutation_allowed() {
    assert!(
        !FORBID_MUTATION.get(),
        "forbidden lock, preflight, persistence, or filesystem mutation boundary"
    );
}

pub(crate) struct NoTargetInspectionGuard {
    previous: bool,
}

pub(crate) fn forbid_target_inspection() -> NoTargetInspectionGuard {
    NoTargetInspectionGuard {
        previous: FORBID_TARGET_INSPECTION.replace(true),
    }
}

impl Drop for NoTargetInspectionGuard {
    fn drop(&mut self) {
        FORBID_TARGET_INSPECTION.set(self.previous);
    }
}

pub(crate) fn assert_target_inspection_allowed() {
    assert!(
        !FORBID_TARGET_INSPECTION.get(),
        "forbidden managed-target inspection boundary"
    );
}
