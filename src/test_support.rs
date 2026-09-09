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

thread_local! {
    static FORBID_DECLARATION_READ: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct NoDeclarationReadGuard {
    previous: bool,
}
pub(crate) fn forbid_desired_dependencies() -> NoDeclarationReadGuard {
    NoDeclarationReadGuard {
        previous: FORBID_DECLARATION_READ.replace(true),
    }
}
impl Drop for NoDeclarationReadGuard {
    fn drop(&mut self) {
        FORBID_DECLARATION_READ.set(self.previous);
    }
}
pub(crate) fn assert_desired_dependencies_allowed() {
    assert!(
        !FORBID_DECLARATION_READ.get(),
        "forbidden configuration/profile/source/planner dependency"
    );
}

#[cfg(unix)]
pub(crate) use execution_hooks::*;

#[cfg(unix)]
mod execution_hooks {
    /// Semantic execution seams compiled only into tests. One-shot hooks are removed before invocation so callbacks may inspect the filesystem without reentrancy.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum ExecutionBoundary {
        BeforeFinalRecheck,
        AfterFinalRecheck,
        AfterMutationAttempt,
        AfterTemporaryCreation,
        BeforeRelocateRemovalRecheck,
        BeforePostObservation,
        BeforeCommit,
    }

    type ExecutionHook = Box<dyn FnOnce() -> std::io::Result<()>>;
    thread_local! {
        static EXECUTION_HOOK: std::cell::RefCell<Option<(ExecutionBoundary, ExecutionHook)>> = const { std::cell::RefCell::new(None) };
    }

    pub(crate) struct ExecutionHookGuard;
    pub(crate) fn on_execution_boundary(
        boundary: ExecutionBoundary,
        hook: impl FnOnce() -> std::io::Result<()> + 'static,
    ) -> ExecutionHookGuard {
        EXECUTION_HOOK.with_borrow_mut(|slot| {
            assert!(slot.is_none(), "execution hook already installed");
            *slot = Some((boundary, Box::new(hook)));
        });
        ExecutionHookGuard
    }
    impl Drop for ExecutionHookGuard {
        fn drop(&mut self) {
            EXECUTION_HOOK.with_borrow_mut(|slot| *slot = None);
        }
    }
    pub(crate) fn execution_boundary(boundary: ExecutionBoundary) -> std::io::Result<()> {
        let hook = EXECUTION_HOOK.with_borrow_mut(|slot| {
            if slot.as_ref().is_some_and(|(at, _)| *at == boundary) {
                slot.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = hook {
            hook()?;
        }
        Ok(())
    }
}
